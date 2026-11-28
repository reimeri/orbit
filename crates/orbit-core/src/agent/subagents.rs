//! Built-in delegation. The coordinator serializes acceptance, stop, delivery and
//! deletion; it is never held across an await or a wait for another agent.
use super::*;
use crate::store::{SubagentSettings, id, now};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Review,
    Explore,
}
impl Profile {
    pub fn label(self) -> &'static str {
        match self {
            Self::Review => "Review",
            Self::Explore => "Explore",
        }
    }
    pub const ALL: [Self; 2] = [Self::Review, Self::Explore];

    pub fn defaults(self) -> SubagentSettings {
        let (model, effort) = match self {
            Self::Review => (crate::models::SOL, "medium"),
            Self::Explore => (crate::models::LUNA, "high"),
        };
        SubagentSettings {
            model: model.id.into(),
            effort: model.preferred_effort(effort).into(),
            fast: false,
        }
    }
    pub(super) fn instructions(self) -> &'static str {
        match self {
            Self::Review => {
                "\nYou are the Review subagent. Review changes for concrete bugs, regressions and missing validation. Report actionable findings with file/line references and severity; distinguish evidence from uncertainty. You are READ-ONLY: do not edit files or mutate repository/application state. Shell access is not a sandbox. You cannot delegate. Return a self-contained final review, explicitly saying when there are no findings.\n"
            }
            Self::Explore => {
                "\nYou are the Explore subagent. Quickly inspect the project and answer the assigned questions with precise paths, relevant symbols and evidence. You are READ-ONLY: do not edit files or mutate repository/application state. Shell access is not a sandbox. You cannot delegate. Return a concise, self-contained final answer useful to the parent.\n"
            }
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    pub parent_id: String,
    pub profile: Profile,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}
impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::Interrupted => "Interrupted",
        }
    }
    pub fn needs_attention(self) -> bool {
        matches!(self, Self::Failed | Self::Interrupted)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunInfo {
    pub run_id: String,
    pub session_id: String,
    pub parent_id: String,
    pub request_id: String,
    pub profile: Profile,
    /// Snapshot at acceptance; independent of later edits to this profile.
    pub configuration: Option<SubagentSettings>,
    pub title: String,
    pub status: RunStatus,
    pub assignment: usize,
    pub started: u64,
    pub finished: Option<u64>,
    pub notified: bool,
    #[serde(default)]
    pub attention_acknowledged: bool,
}
// Use the same expressions in the indexes and their readers. Invalid JSON must
// not prevent opening a database or preserving a damaged row for repair.
pub(crate) const ORDINAL: &str =
    "CASE WHEN json_valid(metadata) THEN metadata ->> '$.assignment' END";
pub(crate) const PENDING: &str = "CASE WHEN json_valid(metadata) THEN
    metadata ->> '$.status' = 'running' OR metadata ->> '$.notified' = 0 ELSE 1 END";
pub(crate) const UNACKNOWLEDGED: &str = "CASE WHEN json_valid(metadata) THEN
    metadata ->> '$.status' IN ('failed','interrupted')
    AND coalesce(metadata ->> '$.attention_acknowledged', 0) = 0 END";
pub(super) const RUN_COLUMNS: &str =
    "rowid,id,parent,session,request,metadata,result IS NOT NULL AS has_result";

#[derive(Clone, Copy)]
pub enum RunKey<'a> {
    Id(&'a str),
    Request(&'a str),
}

impl RunInfo {
    fn stored(store: &Store, parent: &str, key: RunKey<'_>) -> Result<Option<Self>> {
        let (column, value) = match key {
            RunKey::Id(id) => ("id", id),
            RunKey::Request(request) => ("request", request),
        };
        let db = store.read_connection()?;
        let mut query = db.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM subagent_runs WHERE parent=?1 AND {column}=?2"
        ))?;
        query.query([parent, value])?.next()?.map(RunInfo::from_row).transpose()
            .with_context(|| format!("Could not read subagent assignment with {column} {value}; database row preserved. Repair it or restore a backup"))
    }

    pub(super) fn from_row(row: &rusqlite::Row<'_>) -> Result<Self> {
        let run: Self = serde_json::from_str(row.get_ref("metadata")?.as_str()?)?;
        anyhow::ensure!(
            run.run_id == row.get_ref("id")?.as_str()?
                && run.parent_id == row.get_ref("parent")?.as_str()?
                && run.session_id == row.get_ref("session")?.as_str()?
                && run.request_id == row.get_ref("request")?.as_str()?,
            "assignment identity mismatch"
        );
        anyhow::ensure!(
            run.status != RunStatus::Completed || row.get("has_result")?,
            "completed assignment has no stored result"
        );
        Ok(run)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spawn {
    #[serde(rename = "type")]
    profile: Profile,
    task: String,
    title: Option<String>,
    session_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wait {
    run_ids: Vec<String>,
    timeout_ms: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {}
/// Sidebar/rail state uses only the latest assignment of each child.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ChildStatus {
    pub running: usize,
    pub needs_attention: bool,
}

#[derive(Default)]
struct Family {
    runs: HashMap<String, RunInfo>,
    // Child session -> latest assignment ID. Historical results remain addressable.
    latest: HashMap<String, String>,
    running: usize,
    attention: usize,
    // An inclusive acknowledgement cutoff per child with outstanding failures.
    unacknowledged: HashMap<String, usize>,
}

impl Family {
    fn record(&mut self, run: RunInfo) {
        if run.status.needs_attention() && !run.attention_acknowledged {
            let through = self
                .unacknowledged
                .entry(run.session_id.clone())
                .or_default();
            *through = (*through).max(run.assignment);
        }
        let previous = self.latest.get(&run.session_id).map(|id| &self.runs[id]);
        if previous.is_none_or(|old| old.assignment <= run.assignment) {
            let counts = |r: &RunInfo| {
                (
                    usize::from(r.status == RunStatus::Running),
                    usize::from(r.status.needs_attention() && !r.attention_acknowledged),
                )
            };
            let (old_running, old_attention) = previous.map_or((0, 0), counts);
            let (running, attention) = counts(&run);
            self.running = self.running - old_running + running;
            self.attention = self.attention - old_attention + attention;
            // A notified result is already durable; unsaved results are never notified.
            if let Some(id) = self
                .latest
                .insert(run.session_id.clone(), run.run_id.clone())
                && id != run.run_id
                && self.runs[&id].notified
            {
                self.runs.remove(&id);
            }
        } else if run.notified {
            self.runs.remove(&run.run_id);
            return;
        }
        self.runs.insert(run.run_id.clone(), run);
    }
    fn status(&self) -> ChildStatus {
        ChildStatus {
            running: self.running,
            needs_attention: self.attention != 0,
        }
    }
}

type AssignmentResult = (Option<String>, Option<String>);

pub(super) struct Coordinator {
    // Retain complete terminal text on storage failure, without rerunning work.
    unsaved: HashMap<String, AssignmentResult>,
    pub parent_stopped: bool,
}
impl Coordinator {
    pub(super) fn new(parent_stopped: bool) -> Self {
        Self {
            parent_stopped,
            unsaved: HashMap::new(),
        }
    }
}
/// Resident assignment metadata. Its lock never spans I/O or lifecycle operations:
/// rendering the open conversation must not wait for a different session to load.
#[derive(Default)]
pub(super) struct RunIndex {
    families: HashMap<String, Family>,
}
impl RunIndex {
    pub(super) fn child_ids(&self, parent: &str) -> impl Iterator<Item = &String> {
        self.families
            .get(parent)
            .into_iter()
            .flat_map(|family| family.latest.keys())
    }
    pub(super) fn forget_family(&mut self, parent: &str) {
        self.families.remove(parent);
    }

    pub(super) fn record(&mut self, run: RunInfo) {
        self.families
            .entry(run.parent_id.clone())
            .or_default()
            .record(run);
    }
    pub(super) fn run(&self, parent: &str, id: &str) -> Option<&RunInfo> {
        self.families.get(parent)?.runs.get(id)
    }
    pub(super) fn runs(&self, parent: &str) -> impl Iterator<Item = &RunInfo> {
        self.families
            .get(parent)
            .into_iter()
            .flat_map(|f| f.runs.values())
    }
    fn latest_run(&self, parent: &str, child: &str) -> Option<&RunInfo> {
        let family = self.families.get(parent)?;
        family.latest.get(child).map(|id| &family.runs[id])
    }
    pub(super) fn status(&self, parent: &str) -> ChildStatus {
        self.families
            .get(parent)
            .map(Family::status)
            .unwrap_or_default()
    }
    pub(super) fn parent_of(&self, session: &str) -> Option<&str> {
        self.families.iter().find_map(|(parent, family)| {
            family
                .latest
                .contains_key(session)
                .then_some(parent.as_str())
        })
    }

    pub(super) fn latest_runs(&self, parent: &str) -> Vec<RunInfo> {
        let Some(family) = self.families.get(parent) else {
            return vec![];
        };
        let mut runs: Vec<_> = family
            .latest
            .values()
            .map(|id| family.runs[id].clone())
            .collect();
        runs.sort_by(|a, b| {
            b.started
                .cmp(&a.started)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        runs
    }
    fn resident(&self, parent: &str, key: RunKey<'_>) -> Option<&RunInfo> {
        match key {
            RunKey::Id(id) => self.run(parent, id),
            RunKey::Request(request) => self.runs(parent).find(|run| run.request_id == request),
        }
    }

    pub(super) fn attention_ranges(&self, parent: &str, session: &str) -> Vec<(String, usize)> {
        self.families
            .get(parent)
            .into_iter()
            .flat_map(|f| &f.unacknowledged)
            .filter(|(child, _)| session == parent || *child == session)
            .map(|(child, through)| (child.clone(), *through))
            .collect()
    }

    pub(super) fn acknowledged(&mut self, parent: &str, ranges: &[(String, usize)]) {
        if let Some(family) = self.families.get_mut(parent) {
            for (child, through) in ranges {
                if family
                    .unacknowledged
                    .get(child)
                    .is_some_and(|high| high <= through)
                {
                    family.unacknowledged.remove(child);
                }
            }
        }
    }

    pub fn load(store: &Store) -> Result<(Self, Vec<String>)> {
        let mut index = Self::default();
        let mut warnings = Vec::new();
        let db = store.db.lock().expect("database lock");
        let mut skipped = HashSet::new();
        let mut decode = |row: &rusqlite::Row<'_>| -> Result<Option<RunInfo>> {
            match RunInfo::from_row(row) {
                Ok(run) => Ok(Some(run)),
                Err(error) => {
                    let rowid = row.get::<_, i64>("rowid")?;
                    if skipped.insert(rowid) {
                        let id = row
                            .get::<_, String>("id")
                            .unwrap_or_else(|_| format!("at row {rowid}"));
                        warnings.push(format!("Skipped subagent assignment {id}: {error}. Other assignments and conversations remain available. The database row was preserved; repair it or restore a backup and restart to recover this assignment."));
                    }
                    Ok(None)
                }
            }
        };
        // Walk the ordinal index only until the newest readable assignment.
        let mut children = db.prepare(&format!(
            "SELECT id, EXISTS(
            SELECT 1 FROM subagent_runs WHERE session=sessions.id AND {UNACKNOWLEDGED})
            FROM sessions WHERE EXISTS(SELECT 1 FROM subagent_runs WHERE session=sessions.id)"
        ))?;
        let mut latest = db.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM subagent_runs WHERE session=?1 ORDER BY {ORDINAL} DESC"
        ))?;
        let mut children = children.query([])?;
        while let Some(child) = children.next()? {
            let mut rows = latest.query([child.get_ref(0)?.as_str()?])?;
            while let Some(row) = rows.next()? {
                if let Some(run) = decode(row)? {
                    let family = index.families.entry(run.parent_id.clone()).or_default();
                    if child.get::<_, bool>(1)? {
                        family
                            .unacknowledged
                            .insert(run.session_id.clone(), run.assignment);
                    }
                    family.record(run);
                    break;
                }
            }
        }
        let mut pending = db.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM subagent_runs WHERE {PENDING}"
        ))?;
        let mut rows = pending.query([])?;
        let mut interrupted = Vec::new();
        while let Some(row) = rows.next()? {
            if let Some(mut run) = decode(row)? {
                if run.status == RunStatus::Running {
                    run.status = RunStatus::Interrupted;
                    run.finished = Some(now());
                    interrupted.push((run.run_id.clone(), serde_json::to_string(&run)?));
                }
                index.record(run);
            }
        }
        // Updating indexed metadata while traversing the pending index can skip rows.
        for (id, metadata) in interrupted {
            db.execute("UPDATE subagent_runs SET metadata=?2,error=?3 WHERE id=?1", params![id, metadata, "Orbit exited before this assignment finished. Commands are not replayed; continue through the parent after inspecting the workspace."])?;
        }
        Ok((index, warnings))
    }
}

impl Harness {
    pub(super) fn run_index(&self) -> std::sync::MutexGuard<'_, RunIndex> {
        self.run_index.lock().expect("run index lock")
    }
    pub fn subagent_runs(&self, parent: &str) -> Vec<RunInfo> {
        self.run_index().latest_runs(parent)
    }
    pub fn child_statuses(&self) -> HashMap<String, ChildStatus> {
        self.run_index()
            .families
            .iter()
            .map(|(id, family)| (id.clone(), family.status()))
            .collect()
    }
    pub fn active_children(&self, parent: &str) -> usize {
        self.run_index().status(parent).running
    }
    /// Memory-only path for rendering current activity.
    pub fn resident_subagent(&self, parent: &str, key: RunKey<'_>) -> Option<RunInfo> {
        self.run_index().resident(parent, key).cloned()
    }
    pub fn subagent_run(&self, parent: &str, key: RunKey<'_>) -> Result<Option<RunInfo>> {
        match self.resident_subagent(parent, key) {
            Some(run) => Ok(Some(run)),
            None => RunInfo::stored(&self.store, parent, key),
        }
    }
    pub(super) fn subagent_tool(
        self: &Arc<Self>,
        parent: &str,
        name: &str,
        args: &str,
        request: &str,
        cancel: &CancellationToken,
    ) -> Result<String> {
        match name {
            "spawn_subagent" => {
                let args = serde_json::from_str(args)?;
                serde_json::to_string(&self.spawn_subagent(parent, args, request, cancel)?)
                    .map_err(Into::into)
            }
            "wait_subagent" => self.wait_subagent(parent, serde_json::from_str(args)?, cancel),
            "list_subagents" => {
                let _: List = serde_json::from_str(args)?;
                Ok(serde_json::to_string(&self.subagent_runs(parent))?)
            }
            _ => bail!("Unknown subagent tool {name}"),
        }
    }
    fn spawn_subagent(
        self: &Arc<Self>,
        parent: &str,
        args: Spawn,
        request: &str,
        cancel: &CancellationToken,
    ) -> Result<RunInfo> {
        if args.task.trim().is_empty() {
            bail!("Provide a complete, nonempty task briefing");
        }
        let (_library, family) = self.admit(parent);
        let coordinator = family.lock().expect("family lifecycle");
        let key = RunKey::Request(request);
        if let Some(run) = self.resident_subagent(parent, key) {
            return Ok(run);
        }
        if let Some(run) = RunInfo::stored(&self.store, parent, key)? {
            return Ok(run);
        }
        if cancel.is_cancelled() || coordinator.parent_stopped {
            bail!("Parent stopped; no child was started");
        }
        let owner = self.load(parent)?;
        let snapshot = owner.snapshot();
        if snapshot.child.is_some() {
            bail!("Subagents cannot delegate");
        }
        if self.active_children(parent) >= 6 {
            bail!(
                "Six subagents are already running. Wait for one to finish before spawning another."
            );
        }
        // Read the committed preferences for this assignment, even within a long-running parent turn.
        let settings = self.store.load_catalog()?.settings;
        let existing = args
            .session_id
            .as_deref()
            .map(|session| self.load(session))
            .transpose()?;
        let mut session = if let Some(child) = &existing {
            let snapshot = child.snapshot();
            if snapshot.child.as_ref()
                != Some(&Child {
                    parent_id: parent.into(),
                    profile: args.profile,
                })
            {
                bail!("Resume only your own child with its original profile");
            }
            if child.is_executing()
                || self
                    .run_index()
                    .latest_run(parent, &child.id)
                    .is_some_and(|r| r.status == RunStatus::Running)
            {
                bail!("Child is busy. Wait for its current assignment before continuing it.");
            }
            snapshot
        } else {
            let mut session = Session::new(snapshot.project, &settings);
            session.child = Some(Child {
                parent_id: parent.into(),
                profile: args.profile,
            });
            session
        };
        let configuration = &settings.subagents[&args.profile];
        session.model.clone_from(&configuration.model);
        session.effort.clone_from(&configuration.effort);
        session.fast = configuration.fast;
        let title = args
            .title
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                crate::store::title_excerpt(&args.task, 80).expect("validated nonblank task")
            });
        session.title = title.clone();
        // An interrupted/offline earlier task is history, not a second pending assignment.
        session.consume_pending();
        let run = RunInfo {
            run_id: id(),
            session_id: session.id.clone(),
            parent_id: parent.into(),
            request_id: request.into(),
            profile: args.profile,
            configuration: Some(configuration.clone()),
            title,
            status: RunStatus::Running,
            // Skipped, unreadable assignments still occupy their original positions.
            assignment: if existing.is_some() {
                self.store
                    .read_connection()?
                    .query_row(
                        "SELECT COUNT(*) + 1 FROM subagent_runs WHERE session=?1",
                        [&session.id],
                        |row| row.get::<_, i64>(0),
                    )?
                    .try_into()?
            } else {
                1
            },
            started: now(),
            finished: None,
            notified: false,
            attention_acknowledged: false,
        };
        session.accept_submission(run.run_id.clone(), args.task.into(), false);
        if let Some(child) = &existing {
            let mut saved = child.persisted.lock().expect("persistence lock");
            self.store.save_session_transaction(
                saved.as_ref(),
                &session,
                crate::store::Checkpoint::StartRun(&run),
            )?;
            *child.state.lock().expect("session lock") = session.clone();
            *saved = Some(session);
            child.changed();
        } else {
            let body = self.load_gate(&session.id);
            let _publishing = body.lock().expect("loading lock");
            self.store.save_session_transaction(
                None,
                &session,
                crate::store::Checkpoint::StartRun(&run),
            )?;
            self.make_conversation(session, true);
        }
        self.run_index().record(run.clone());
        // start_inner only schedules work; its worker cannot finish under this lock.
        self.start_inner(self.load(&run.session_id)?, settings)?;
        self.catalog_revision.fetch_add(1, Ordering::Release);
        self.changes.send_replace(());
        Ok(run)
    }
    fn wait_subagent(
        &self,
        parent: &str,
        args: Wait,
        cancel: &CancellationToken,
    ) -> Result<String> {
        if args.run_ids.is_empty() {
            bail!("Provide at least one run_id");
        }
        let mut changes = self.subscribe();
        self.runtime.as_ref().expect("live runtime").handle().block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_millis(args.timeout_ms.unwrap_or(10_000).min(300_000));
            loop {
                changes.borrow_and_update();
                let runs = args.run_ids.iter().map(|id| self.subagent_run(parent, RunKey::Id(id))?.with_context(|| format!("Unknown run_id {id} for this parent"))).collect::<Result<Vec<_>>>()?;
                if runs.iter().any(|r| r.status != RunStatus::Running) || tokio::time::Instant::now() >= deadline {
                    let mut results = vec![];
                    for run in runs {
                        let mut result = json!({"run_id":run.run_id,"session_id":run.session_id,"status":run.status});
                        if run.status != RunStatus::Running {
                            let unsaved = self.family(parent).lock().expect("family lifecycle").unsaved.get(&run.run_id).cloned();
                            let (text,error) = if let Some(result) = unsaved { result } else {
                                self.store.read_connection()?.query_row("SELECT result,error FROM subagent_runs WHERE id=?1",[&run.run_id],|r| Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?)))?
                            };
                            if run.status == RunStatus::Completed { result["final_response"] = json!(text.context("Completed assignment result is missing; database preserved. Restore it from a backup or repair it before retrying")?); }
                            else { result["error"] = json!(error); }
                        }
                        results.push(result);
                    }
                    return Ok(crate::output::preserve_default(&serde_json::to_string(&json!({"runs":results}))?));
                }
                tokio::select! { _ = cancel.cancelled() => bail!("Wait cancelled; child assignments are unchanged"), _ = tokio::time::sleep_until(deadline) => {}, result = changes.changed() => { result.context("Harness closed")?; } }
            }
        })
    }
    pub(super) fn finish_assignment(
        self: &Arc<Self>,
        child: &Arc<Conversation>,
        result: Result<String>,
        cancelled: bool,
    ) -> Result<()> {
        let parent = child
            .state
            .lock()
            .expect("session lock")
            .child
            .as_ref()
            .expect("child assignment")
            .parent_id
            .clone();
        let (_library, family) = self.admit(&parent);
        let mut coordinator = family.lock().expect("family lifecycle");
        // Release the finishing reservation before another assignment can
        // acquire this family lock, including when delivery fails.
        let _finishing = scopeguard::guard(&child.finishing, |flag| {
            flag.store(false, Ordering::Release)
        });
        let Some(mut run) = self
            .run_index()
            .latest_run(&parent, &child.id)
            .filter(|r| r.status == RunStatus::Running)
            .cloned()
        else {
            return Ok(());
        };
        let (text, error) = if cancelled {
            run.status = RunStatus::Cancelled;
            (
                None,
                Some("Assignment stopped. Completed commands are not undone.".into()),
            )
        } else {
            match result {
                Ok(text) => {
                    run.status = RunStatus::Completed;
                    (Some(text), None)
                }
                Err(e) => {
                    run.status = RunStatus::Failed;
                    (None, Some(format!("{e:#}")))
                }
            }
        };
        run.finished = Some(now());
        self.run_index().record(run.clone());
        coordinator
            .unsaved
            .insert(run.run_id.clone(), (text, error));
        self.retry_results(&mut coordinator, &parent)?;
        self.deliver_notices(&mut coordinator, &run.parent_id)?;
        if !coordinator.parent_stopped {
            let parent = self.load(&run.parent_id)?;
            let settings = parent.last_settings.lock().expect("settings lock").clone();
            self.start_inner(parent, settings)?;
        }
        Ok(())
    }
    pub(super) fn retry_results(&self, coordinator: &mut Coordinator, parent: &str) -> Result<()> {
        if coordinator.unsaved.is_empty() {
            return Ok(());
        }
        let mut db = self.store.db.lock().expect("database lock");
        let tx = db.transaction()?;
        for (id, (text, error)) in &coordinator.unsaved {
            let metadata = serde_json::to_string(
                self.run_index()
                    .run(parent, id)
                    .expect("unsaved assignment"),
            )?;
            tx.execute(
                "UPDATE subagent_runs SET metadata=?2,result=?3,error=?4 WHERE id=?1",
                params![id, metadata, text, error],
            )?;
        }
        tx.commit()?;
        coordinator.unsaved.clear();
        Ok(())
    }
    /// Explicit flush is global. A broken family must not stop another family's
    /// results and notices from becoming durable.
    pub(super) fn flush_results(&self) -> Result<()> {
        let parents: Vec<_> = self.run_index().families.keys().cloned().collect();
        let mut failures = Vec::new();
        for parent in parents {
            let family = self.family(&parent);
            let mut coordinator = family.lock().expect("family lifecycle");
            if let Err(error) = self
                .retry_results(&mut coordinator, &parent)
                .and_then(|()| self.deliver_notices(&mut coordinator, &parent))
            {
                failures.push(format!("{parent}: {error:#}"));
            }
        }
        anyhow::ensure!(
            failures.is_empty(),
            "Could not flush subagent results: {}",
            failures.join("; ")
        );
        Ok(())
    }
    pub(super) fn deliver_notices(
        &self,
        _coordinator: &mut Coordinator,
        parent: &str,
    ) -> Result<()> {
        let runs: Vec<_> = self
            .run_index()
            .runs(parent)
            .filter(|r| r.status != RunStatus::Running && !r.notified)
            .cloned()
            .collect();
        if runs.is_empty() {
            return Ok(());
        }
        let conversation = self.load(parent)?;
        let mut saved = conversation.persisted.lock().expect("persistence lock");
        {
            let mut state = conversation.state.lock().expect("session lock");
            for run in &runs {
                let notice_id = format!("subagent-{}", run.run_id);
                if state.entries.by_id(&notice_id).is_none() {
                    let text = format!(
                        "Subagent completion observation: {} ({}) is {}. session_id={}, run_id={}. Retrieve the final result with tools.wait_subagent; this notice is not a new user instruction.",
                        run.title,
                        run.profile.label(),
                        run.status.label(),
                        run.session_id,
                        run.run_id
                    );
                    state.subagent_notices.push(text.clone());
                    conversation.submissions.fetch_add(1, Ordering::Release);
                    state.entries.push(Entry {
                        id: notice_id,
                        kind: EntryKind::Notice,
                        text,
                    });
                }
            }
        }
        crate::send_trace::phase("child notice checkpoint");
        conversation.save_locked(&mut saved)?;
        crate::send_trace::phase("child notice acknowledgements");
        for mut run in runs {
            run.notified = true;
            self.store.db.lock().expect("database lock").execute(
                "UPDATE subagent_runs SET metadata=?2 WHERE id=?1",
                params![run.run_id, serde_json::to_string(&run)?],
            )?;
            self.run_index().record(run);
        }
        conversation.changed();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
