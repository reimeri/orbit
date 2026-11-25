//! Built-in delegation. The coordinator serializes acceptance, stop, delivery and
//! deletion; it is never held across an await or a wait for another agent.
use super::*;
use crate::store::{SubagentSettings, id, now};
use rusqlite::params;
use serde::{Deserialize, Serialize};

enum Deletion<'a> {
    Session(&'a str),
    Project(&'a std::path::Path),
}

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
            effort: effort.into(),
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
const WAIT_OUTPUT_TOKENS: usize = 10_000;

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
                args.task
                    .lines()
                    .next()
                    .expect("nonempty task")
                    .chars()
                    .take(80)
                    .collect()
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
                    return Ok(crate::output::preserve_text(&serde_json::to_string(&json!({"runs":results}))?, crate::output::character_budget(WAIT_OUTPUT_TOKENS)));
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
    pub fn stop(&self, session: &str) {
        let (_library, family) = self.admit(session);
        let mut coordinator = family.lock().expect("family lifecycle");
        if self.family_id(session) == session {
            coordinator.parent_stopped = true;
        }
        if let Some(c) = self.get(session) {
            c.cancel();
        }
        let children: Vec<_> = self
            .run_index()
            .families
            .get(session)
            .into_iter()
            .flat_map(|f| f.latest.keys().cloned())
            .collect();
        for child in children {
            if let Some(c) = self.get(&child) {
                if c.is_running() {
                    c.cancel();
                } else {
                    c.code.stop_all();
                }
            }
        }
        self.catalog_revision.fetch_add(1, Ordering::Release);
        self.changes.send_replace(());
    }
    pub fn remove(&self, session: &str) -> Result<()> {
        self.remove_sessions(Deletion::Session(session))
    }

    /// Delete every conversation in a project as one database transaction.
    /// The library barrier excludes creation of new families during discovery.
    pub fn remove_project(&self, project: &std::path::Path) -> Result<()> {
        self.remove_sessions(Deletion::Project(project))
    }

    fn remove_sessions(&self, target: Deletion<'_>) -> Result<()> {
        // A project spans an open-ended set of families; close admission while
        // discovering and deleting it. A single-family deletion is local.
        let project_deletion = matches!(target, Deletion::Project(_));
        let global = project_deletion.then(|| self.library.write().expect("library lock"));
        let local = (!project_deletion).then(|| self.library.read().expect("library lock"));
        let parents: HashSet<String> = match target {
            Deletion::Session(session) => {
                if self.load(session)?.snapshot().child.is_some() {
                    bail!("Delete children together with their parent");
                }
                [session.to_owned()].into()
            }
            Deletion::Project(project) => self
                .summaries()
                .into_iter()
                .filter(|s| s.project == project && s.child.is_none())
                .map(|s| s.id)
                .collect(),
        };
        let gates: Vec<_> = parents.iter().map(|id| self.family(id)).collect();
        let mut reservations: Vec<_> = gates
            .iter()
            .map(|f| f.lock().expect("family lifecycle"))
            .collect();
        let mut ids = parents.clone();
        let index = self.run_index();
        ids.extend(
            parents
                .iter()
                .filter_map(|id| index.families.get(id))
                .flat_map(|f| f.latest.keys().cloned()),
        );
        drop(index);
        // A child's only assignment may have been skipped during startup recovery.
        ids.extend(
            self.summaries
                .lock()
                .expect("summary lock")
                .values()
                .filter(|s| {
                    s.child
                        .as_ref()
                        .is_some_and(|child| parents.contains(&child.parent_id))
                })
                .map(|s| s.id.clone()),
        );
        if parents.iter().any(|id| self.active_children(id) > 0)
            || ids.iter().any(|id| {
                self.get(id).is_some_and(|c| {
                    if project_deletion {
                        c.is_executing()
                    } else {
                        c.is_turn_active()
                    }
                })
            })
        {
            bail!("Stop all running sessions and their subagents before deleting. Then retry.");
        }
        let mut order: Vec<_> = ids.iter().collect();
        order.sort_unstable();
        let bodies: Vec<_> = order.iter().map(|id| self.load_gate(id)).collect();
        let loading: Vec<_> = bodies
            .iter()
            .map(|gate| gate.lock().expect("loading lock"))
            .collect();
        let family: Vec<_> = ids.iter().filter_map(|id| self.get(id)).collect();
        // Exclude late native callbacks: none can recreate a just-deleted row.
        let _saved: Vec<_> = family
            .iter()
            .map(|c| c.persisted.lock().expect("persistence lock"))
            .collect();
        // Serialize publication with logical deletion, not merely directory cleanup.
        let mut media_leases: Vec<_> = family
            .iter()
            .filter_map(|c| c.code.media_lifecycle())
            .collect();
        // Delete the whole family atomically before evicting runtime state.
        let unused_media = self.store.delete_sessions(&ids)?;
        for lease in &mut media_leases {
            **lease = false;
        }
        drop(media_leases);
        for id in &ids {
            if let Some(c) = self.get(id) {
                c.deleted.store(true, Ordering::Release);
                c.naming_cancel.cancel();
                c.cancel();
            }
            self.notifications.clear(id);
            self.conversations
                .lock()
                .expect("conversations lock")
                .remove(id);
            self.summaries.lock().expect("summary lock").remove(id);
        }
        for parent in &parents {
            self.run_index().families.remove(parent);
        }
        drop(_saved);
        drop(loading);
        for state in &mut reservations {
            state.unsaved.clear();
            state.parent_stopped = true;
        }
        drop(reservations);
        drop(gates);
        // Never replace a family gate underneath an already-admitted waiter.
        let mut registry = self.families.lock().expect("family registry");
        for parent in parents {
            if registry
                .gates
                .get(&parent)
                .is_some_and(|gate| Arc::strong_count(gate) == 1)
            {
                registry.gates.remove(&parent);
            }
        }
        drop(registry);
        drop(local);
        drop(global);
        let mut cleanup_errors = Vec::new();
        for owner in unused_media {
            if let Err(e) = crate::media::remove(&self.store.root().join("media").join(&owner)) {
                cleanup_errors.push(format!("{owner}: {e:#}"));
            }
        }
        self.catalog_revision.fetch_add(1, Ordering::Release);
        self.changes.send_replace(());
        if !cleanup_errors.is_empty() {
            bail!(
                "Session family was deleted and cannot be recreated by late callbacks, but private media cleanup needs attention under {}: {}",
                self.store.root().join("media").display(),
                cleanup_errors.join("; ")
            );
        }
        Ok(())
    }
}

pub(super) fn final_response(output: &[Value]) -> String {
    output
        .iter()
        .filter(|v| v["type"] == "message" && v["phase"] != "commentary")
        .flat_map(|v| v["content"].as_array().into_iter().flatten())
        .filter_map(|v| v["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{StreamingRequest as Request, until};
    use tokio::sync::mpsc;
    struct Fixture {
        _dir: tempfile::TempDir,
        h: Arc<Harness>,
        parent: String,
        requests: mpsc::UnboundedReceiver<Request>,
        server: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.h.stop_all();
            self.server.abort();
        }
    }
    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("AGENTS.md"),
                "Project rule: preserve the sapphire invariant.",
            )
            .unwrap();
            let store = crate::test_support::authenticated_store(dir.path().join("state"));
            let (endpoint, requests, server) = crate::test_support::streaming_server().await;
            let provider = Provider::fixture(store.clone(), endpoint);
            let h = Harness::with_provider(store.clone(), vec![], provider).unwrap();
            let mut session = Session::new(dir.path().into(), &Settings::default());
            session.accept("PRIVATE PARENT CONTEXT".into());
            session.consume_pending();
            let parent = session.id.clone();
            store.save_session(&session).unwrap();
            h.insert(session);
            Self {
                _dir: dir,
                h,
                parent,
                requests,
                server,
            }
        }
        async fn request(&mut self) -> Request {
            tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
                .await
                .unwrap()
                .unwrap()
        }
        fn spawn(
            &self,
            profile: Profile,
            session: Option<String>,
            request: &str,
        ) -> Result<RunInfo> {
            self.h.spawn_subagent(
                &self.parent,
                Spawn {
                    profile,
                    task: format!("Inspect {request}"),
                    title: Some(request.into()),
                    session_id: session,
                },
                request,
                &CancellationToken::new(),
            )
        }
        async fn wait_raw(&self, ids: Vec<String>, timeout: u64) -> String {
            let h = self.h.clone();
            let parent = self.parent.clone();
            tokio::task::spawn_blocking(move || {
                h.wait_subagent(
                    &parent,
                    Wait {
                        run_ids: ids,
                        timeout_ms: Some(timeout),
                    },
                    &CancellationToken::new(),
                )
            })
            .await
            .unwrap()
            .unwrap()
        }
        async fn wait(&self, ids: Vec<String>, timeout: u64) -> Value {
            serde_json::from_str(&self.wait_raw(ids, timeout).await).unwrap()
        }
    }
    async fn assert_tool_catalog(f: &Fixture, session: &str, request: &Value, expected: &[&str]) {
        let tools = request["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        let exec = tools.iter().find(|tool| tool["name"] == "exec").unwrap();
        let wait = tools.iter().find(|tool| tool["name"] == "wait").unwrap();
        assert_eq!(exec["type"], "custom");
        assert_eq!(exec["format"]["definition"], crate::code_mode::GRAMMAR);
        assert_eq!(wait["type"], "function");
        let description = exec["description"].as_str().unwrap();
        assert!(request.get("instructions").is_none());
        assert_eq!(request["input"][0]["role"], "developer");
        let instructions = request["input"][0]["content"][0]["text"].as_str().unwrap();
        let c = f.h.get(session).unwrap();
        let output = tokio::task::spawn_blocking(move || {
            c.code.exec(
                "text({names:Object.keys(tools), definitions:ALL_TOOLS});",
                "inspect-catalog",
                &CancellationToken::new(),
            )
        })
        .await
        .unwrap()
        .unwrap();
        let runtime: Value =
            serde_json::from_str(output.strip_prefix("Script completed\nOutput:\n").unwrap())
                .unwrap();
        let names = runtime["names"].as_array().unwrap();
        let actual: std::collections::BTreeSet<_> =
            names.iter().map(|name| name.as_str().unwrap()).collect();
        assert_eq!(names.len(), actual.len(), "tool names must be unique");
        assert_eq!(actual, expected.iter().copied().collect());
        let definitions = runtime["definitions"].as_array().unwrap();
        let documented: std::collections::BTreeSet<_> = definitions
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            definitions.len(),
            documented.len(),
            "definitions must be unique"
        );
        assert_eq!(
            documented, actual,
            "ALL_TOOLS must describe every callable tool"
        );
        for tool in definitions {
            let name = tool["name"].as_str().unwrap();
            assert!(description.contains(name));
            assert!(description.contains(tool["description"].as_str().unwrap()));
            assert!(tool["input_schema"].is_object());
            assert!(tool["output_schema"].is_object());
        }
        for name in [
            "exec_command",
            "write_stdin",
            "web__run",
            "view_image",
            "image_gen__imagegen",
            "ask_user",
            "spawn_subagent",
            "wait_subagent",
            "list_subagents",
        ] {
            if name == "ask_user" {
                assert_eq!(
                    instructions.contains("tools.ask_user"),
                    expected.contains(&name),
                    "questionnaire guidance must match tool availability"
                );
            } else {
                assert!(
                    !instructions.contains(name),
                    "tool guidance leaked into instructions: {name}"
                );
            }
            if !expected.contains(&name) {
                assert!(
                    !description.contains(name),
                    "unavailable tool advertised: {name}"
                );
            }
        }
    }
    fn answer(text: &str) -> String {
        crate::test_support::completed(
            json!([{"type":"message","id":id(),"phase":"final_answer","content":[{"type":"output_text","text":text}]}]),
        )
    }

    #[tokio::test]
    async fn child_activity_tracks_streamed_reasoning_and_message_text() {
        let mut f = Fixture::new().await;
        let run = f
            .spawn(Profile::Explore, None, "streaming activity")
            .unwrap();
        let request = f.request().await;
        let child = f.h.get(&run.session_id).unwrap();
        assert_eq!(child.status(), ConversationStatus::Thinking);

        for (event, entry_id, text, expected) in [
            (
                "response.reasoning_summary_text.delta",
                "reasoning-1",
                "Inspecting",
                ConversationStatus::Thinking,
            ),
            (
                "response.output_text.delta",
                "commentary",
                "Here is my update",
                ConversationStatus::Writing,
            ),
            (
                "response.reasoning_summary_text.delta",
                "reasoning-2",
                "Checking again",
                ConversationStatus::Thinking,
            ),
            (
                "response.output_text.delta",
                "answer",
                "Finished",
                ConversationStatus::Writing,
            ),
        ] {
            request
                .reply
                .send(format!(
                    "{}\n\n",
                    json!({"type": event, "item_id": entry_id, "summary_index": 0, "delta": text})
                ))
                .unwrap();
            until(|| child.snapshot().entries.by_id(entry_id).is_some()).await;
            assert_eq!(child.status(), expected);
            assert!(child.is_running());
        }
        request.reply.send(answer("Finished")).unwrap();
        until(|| !child.is_running()).await;
        assert_eq!(child.status(), ConversationStatus::Completed);
    }

    #[tokio::test]
    async fn pending_question_prevents_completion_notification_across_a_new_main_turn() {
        let mut f = Fixture::new().await;
        let c = f.h.get(&f.parent).unwrap();
        let q = c.questionnaires.as_ref().unwrap().clone();
        let questions = q.clone();
        let ask = tokio::spawn(async move {
            questions.ask("pending-ask", r#"{"questions":[{"id":"answer","question":"Private question","kind":"text"}]}"#, &CancellationToken::new()).await
        });
        until(|| q.has_pending()).await;
        f.h.start(&f.parent, Settings::default()).unwrap();
        f.request()
            .await
            .reply
            .send(answer("Main response ended, but input is still needed"))
            .unwrap();
        until(|| !c.is_running() && !c.finishing.load(Ordering::Acquire)).await;
        let events = f.h.take_notifications();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].kind,
            super::super::notifications::Kind::Question("pending-ask".into())
        );
        assert!(f.h.notification_is_current(&events[0]));
        assert!(!c.snapshot().unchecked_completion);
        assert!(
            !f.h.store
                .load_session(&f.parent)
                .unwrap()
                .unchecked_completion
        );
        q.submit("pending-ask", vec!["Answer".into()]).unwrap();
        ask.await.unwrap().unwrap();
        assert!(!f.h.notification_is_current(&events[0]));
        assert!(
            f.h.take_notifications().is_empty(),
            "answering must not replay a past completion"
        );
    }

    #[tokio::test]
    async fn both_child_profiles_have_web_search_and_workspace_skill_metadata() {
        let mut f = Fixture::new().await;
        let dir = f._dir.path().join(".agents/skills/audit");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: audit\ndescription: Inspect test coverage\n---\nBody for selected skill only.").unwrap();
        let mut pending = vec![];
        for profile in [Profile::Explore, Profile::Review] {
            let child = f.spawn(profile, None, profile.label()).unwrap();
            let request = f.request().await;
            assert_tool_catalog(
                &f,
                &child.session_id,
                &request.body,
                &["exec_command", "write_stdin", "web__run", "view_image"],
            )
            .await;
            let input = request.body["input"].as_array().unwrap();
            assert!(!input[0].to_string().contains("Inspect test coverage"));
            assert!(input[1..].iter().any(|item| item["role"] == "developer"
                && item.to_string().contains("Inspect test coverage")));
            assert!(
                !request
                    .body
                    .to_string()
                    .contains("Body for selected skill only.")
            );
            pending.push(request);
        }
    }

    #[tokio::test]
    async fn parallel_children_isolate_context_wake_parent_and_resume_with_stable_results() {
        let mut f = Fixture::new().await;
        // Exercise the real QuickJS bridge. A completed spawning cell must not cancel its child.
        let c = f.h.get(&f.parent).unwrap();
        let script = "text(await tools.spawn_subagent({type:'explore',task:'Trace the sapphire invariant',title:'Explore invariant'}));";
        let out = tokio::task::spawn_blocking(move || {
            c.code.exec(script, "outer", &CancellationToken::new())
        })
        .await
        .unwrap()
        .unwrap();
        assert!(out.contains("run_id"));
        let explore = f.h.subagent_runs(&f.parent).pop().unwrap();
        let explore_request = f.request().await;
        assert_tool_catalog(
            &f,
            &explore.session_id,
            &explore_request.body,
            &["exec_command", "write_stdin", "web__run", "view_image"],
        )
        .await;
        assert_eq!(
            explore_request.body["model"],
            Profile::Explore.defaults().model
        );
        assert_eq!(
            explore_request.body["reasoning"]["effort"],
            Profile::Explore.defaults().effort
        );
        assert!(
            !explore_request
                .body
                .to_string()
                .contains("PRIVATE PARENT CONTEXT")
        );
        assert!(
            explore_request.body["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["role"] == "user"
                    && item["content"]
                        .as_str()
                        .is_some_and(|s| s.contains("sapphire invariant")))
        );
        assert!(
            explore_request.body["input"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("READ-ONLY")
        );
        assert!(
            !explore_request.body["input"][0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("tools.spawn_subagent")
        );
        let review = f.spawn(Profile::Review, None, "review").unwrap();
        let review_request = f.request().await;
        assert_eq!(
            review_request.body["model"],
            Profile::Review.defaults().model
        );
        assert_eq!(
            review_request.body["reasoning"]["effort"],
            Profile::Review.defaults().effort
        );
        assert_eq!(f.h.active_children(&f.parent), 2);
        assert_eq!(
            f.wait(vec![explore.run_id.clone(), review.run_id.clone()], 0)
                .await["runs"][0]["status"],
            "running"
        );
        assert!(
            f.spawn(Profile::Explore, Some(explore.session_id.clone()), "busy")
                .unwrap_err()
                .to_string()
                .contains("busy")
        );
        f.h.send(
            &f.parent,
            "parallel-parent",
            "Continue main work".into(),
            Settings::default(),
        )
        .unwrap();
        let main = f.request().await;
        assert_tool_catalog(
            &f,
            &f.parent,
            &main.body,
            &[
                "exec_command",
                "write_stdin",
                "web__run",
                "view_image",
                "image_gen__imagegen",
                "ask_user",
                "spawn_subagent",
                "wait_subagent",
                "list_subagents",
            ],
        )
        .await;
        assert_eq!(main.body["prompt_cache_key"], f.parent);
        main.reply
            .send(answer("Main work finished while children run"))
            .unwrap();
        until(|| !f.h.get(&f.parent).unwrap().is_executing()).await;
        assert!(
            f.h.take_notifications().is_empty(),
            "main is waiting for children, not finished"
        );
        assert!(!f.h.get(&f.parent).unwrap().snapshot().unchecked_completion);
        explore_request.reply.send(crate::test_support::completed(json!([
            {"type":"message","id":"progress","phase":"commentary","content":[{"type":"output_text","text":"intermediate secret"}]},
            {"type":"message","id":"final","phase":"final_answer","content":[{"type":"output_text","text":"Final sapphire finding"}]}
        ]))).unwrap();
        let wake = f.request().await;
        assert_eq!(
            f.h.store.activity_summary().unwrap().tokens[0].output,
            22,
            "one main response and one child response; waking the parent does not recount the child"
        );
        assert_eq!(wake.body["prompt_cache_key"], f.parent);
        assert!(wake.body["input"].to_string().contains(&explore.run_id));
        assert!(
            !wake.body["input"]
                .to_string()
                .contains("Final sapphire finding")
        );
        wake.reply.send(crate::test_support::completed(json!([{"type":"custom_tool_call","call_id":"retrieve","name":"exec","input":format!("text(await tools.wait_subagent({{run_ids:[{:?}]}}));",explore.run_id)}]))).unwrap();
        let acknowledged = f.request().await;
        assert!(
            acknowledged.body["input"]
                .to_string()
                .contains("Final sapphire finding")
        );
        assert!(
            !acknowledged.body["input"]
                .to_string()
                .contains("intermediate secret")
        );
        acknowledged.reply.send(answer("Acknowledged")).unwrap();
        until(|| !f.h.get(&f.parent).unwrap().is_executing()).await;
        assert!(
            f.h.take_notifications().is_empty(),
            "child completion and intermediate parent wake must not notify"
        );
        assert!(!f.h.get(&f.parent).unwrap().snapshot().unchecked_completion);
        assert!(
            !f.h.get(&explore.session_id)
                .unwrap()
                .snapshot()
                .unchecked_completion
        );
        let result = f
            .wait(vec![explore.run_id.clone(), review.run_id.clone()], 100)
            .await;
        assert_eq!(
            result["runs"][0]["final_response"],
            "Final sapphire finding"
        );
        assert_eq!(result["runs"][1]["status"], "running");
        let mut catalog = f.h.store.load_catalog().unwrap();
        let follow_up = SubagentSettings {
            model: crate::models::ASTRA.id.into(),
            effort: "low".into(),
            fast: true,
        };
        catalog
            .settings
            .subagents
            .insert(Profile::Explore, follow_up.clone());
        f.h.store.save_catalog(&catalog).unwrap();
        let resumed = f
            .spawn(
                Profile::Explore,
                Some(explore.session_id.clone()),
                "follow-up",
            )
            .unwrap();
        assert_ne!(resumed.run_id, explore.run_id);
        let continuation = f.request().await;
        assert_eq!(resumed.configuration.as_ref(), Some(&follow_up));
        assert_eq!(continuation.body["model"], follow_up.model);
        assert_eq!(continuation.body["reasoning"]["effort"], follow_up.effort);
        assert_eq!(continuation.body["service_tier"], "priority");
        assert!(
            continuation.body["input"]
                .to_string()
                .contains("Final sapphire finding")
        );
        assert!(
            continuation.body["input"]
                .to_string()
                .contains("Inspect follow-up")
        );
        assert!(
            !continuation.body["input"]
                .to_string()
                .contains("PRIVATE PARENT CONTEXT")
        );
        assert_eq!(
            f.wait(vec![explore.run_id.clone()], 0).await["runs"][0]["final_response"],
            "Final sapphire finding"
        );
        assert!(
            f.spawn(
                Profile::Review,
                Some(explore.session_id.clone()),
                "wrong-profile"
            )
            .is_err()
        );
        let nested = f.h.spawn_subagent(
            &review.session_id,
            Spawn {
                profile: Profile::Explore,
                task: "nested".into(),
                title: None,
                session_id: None,
            },
            "nested",
            &CancellationToken::new(),
        );
        assert!(nested.unwrap_err().to_string().contains("cannot delegate"));
        f.h.stop(&f.parent);
        until(|| f.h.active_children(&f.parent) == 0).await;
        assert_eq!(
            f.wait(vec![review.run_id.clone()], 0).await["runs"][0]["status"],
            "cancelled"
        );
        assert!(
            f.wait(vec![review.run_id], 0).await["runs"][0]
                .get("final_response")
                .is_none()
        );
        until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
        assert!(
            f.requests.try_recv().is_err(),
            "explicit Stop must not restart parent"
        );
        drop((continuation, review_request));
    }

    #[tokio::test]
    async fn concurrent_capacity_is_per_parent_and_single_child_stop_wakes_only_its_parent() {
        let mut f = Fixture::new().await;
        let mut tasks = vec![];
        for n in 0..9 {
            let h = f.h.clone();
            let parent = f.parent.clone();
            tasks.push(tokio::task::spawn_blocking(move || {
                h.spawn_subagent(
                    &parent,
                    Spawn {
                        profile: Profile::Explore,
                        task: format!("Task {n}"),
                        title: None,
                        session_id: None,
                    },
                    &format!("request-{n}"),
                    &CancellationToken::new(),
                )
            }));
        }
        let mut accepted = vec![];
        let mut rejected = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(run) => accepted.push(run),
                Err(e) => {
                    assert!(e.to_string().contains("Six subagents"));
                    rejected += 1;
                }
            }
        }
        assert_eq!(accepted.len(), 6);
        assert_eq!(rejected, 3);
        let mut held = vec![];
        for _ in 0..6 {
            held.push(f.request().await);
        }
        let other = Session::new(f._dir.path().into(), &Settings::default());
        let other_id = other.id.clone();
        f.h.store.save_session(&other).unwrap();
        f.h.insert(other);
        let seventh =
            f.h.spawn_subagent(
                &other_id,
                Spawn {
                    profile: Profile::Review,
                    task: "Independent seventh child".into(),
                    title: None,
                    session_id: None,
                },
                "seventh",
                &CancellationToken::new(),
            )
            .unwrap();
        let seventh_request = f.request().await;
        assert!(
            f.h.spawn_subagent(
                &other_id,
                Spawn {
                    profile: Profile::Explore,
                    task: "Wrong owner".into(),
                    title: None,
                    session_id: Some(accepted[0].session_id.clone())
                },
                "foreign",
                &CancellationToken::new()
            )
            .is_err()
        );
        f.h.stop(&accepted[0].session_id);
        let wake = f.request().await;
        assert_eq!(wake.body["prompt_cache_key"], f.parent);
        assert!(wake.body["input"].to_string().contains("Cancelled"));
        wake.reply
            .send(answer("Child cancellation acknowledged"))
            .unwrap();
        assert_eq!(f.h.active_children(&other_id), 1);
        assert_eq!(f.h.active_children(&f.parent), 5);
        assert!(f.h.remove(&f.parent).is_err());
        f.h.stop(&f.parent);
        until(|| {
            f.h.active_children(&f.parent) == 0 && !f.h.get(&f.parent).unwrap().is_executing()
        })
        .await;
        assert_eq!(f.h.active_children(&other_id), 1);
        seventh_request
            .reply
            .send(answer("Independent result"))
            .unwrap();
        let other_wake = f.request().await;
        assert_eq!(other_wake.body["prompt_cache_key"], other_id);
        other_wake
            .reply
            .send(answer("Other parent acknowledged"))
            .unwrap();
        until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
        f.h.remove(&f.parent).unwrap();
        assert!(f.h.store.load_session(&f.parent).is_err());
        for run in accepted {
            assert!(f.h.store.load_session(&run.session_id).is_err());
        }
        assert!(f.h.store.load_session(&seventh.session_id).is_ok());
        assert!(f.requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn project_deletion_is_atomic_includes_children_and_preserves_other_projects() {
        let mut f = Fixture::new().await;
        let project = f.h.load(&f.parent).unwrap().snapshot().project;
        let second = Session::new(project.clone(), &Settings::default());
        let second_id = second.id.clone();
        f.h.store.save_session(&second).unwrap();
        f.h.insert(second);
        let other = Session::new(project.join("other-project"), &Settings::default());
        let other_id = other.id.clone();
        f.h.store.save_session(&other).unwrap();
        f.h.insert(other);

        let child = f
            .spawn(Profile::Explore, None, "delete project fixture")
            .unwrap();
        let _request = f.request().await;
        assert!(
            f.h.remove_project(&project)
                .unwrap_err()
                .to_string()
                .contains("Stop")
        );
        for id in [&f.parent, &second_id, &child.session_id, &other_id] {
            assert!(f.h.store.load_session(id).is_ok());
        }
        f.h.stop(&f.parent);
        until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
        let background = f.h.get(&second_id).unwrap();
        let execution = background.clone();
        tokio::task::spawn_blocking(move || {
            execution.code.exec(
                r#"// @exec: {"yield_time_ms": 1}
await new Promise(r => setTimeout(r, 60000));"#,
                "background",
                &CancellationToken::new(),
            )
        })
        .await
        .unwrap()
        .unwrap();
        assert!(!background.is_running());
        assert!(background.is_executing());
        assert!(f.h.remove_project(&project).is_err());
        assert!(
            background.is_executing(),
            "refusal must not cancel outstanding work"
        );
        f.h.stop(&second_id);
        until(|| !background.is_executing()).await;
        // A yielded shell can outlive both its JavaScript cell and the model turn.
        let execution = background.clone();
        let output = tokio::task::spawn_blocking(move || execution.code.exec(
            r#"text(await tools.exec_command({cmd:"while [ ! -f release-shell ]; do sleep 0.01; done; printf done", login:false, yield_time_ms:1}));"#,
            "background-shell", &CancellationToken::new()
        )).await.unwrap().unwrap();
        assert!(output.contains("Script completed"), "{output}");
        assert!(
            output.contains("Process running with session ID"),
            "{output}"
        );
        assert!(!background.is_running());
        assert!(background.is_executing());
        assert!(f.h.remove_project(&project).is_err());
        assert!(background.is_executing());
        std::fs::write(project.join("release-shell"), "").unwrap();
        until(|| !background.is_executing()).await;
        assert!(
            background.code.has_work(),
            "completed, unread shell output is retained but must not block deletion"
        );
        let db = rusqlite::Connection::open(f.h.store.root().join("sessions.sqlite3")).unwrap();
        // Fail after a delete has already occurred inside the transaction.
        db.execute_batch(
            "CREATE TRIGGER reject_project_delete AFTER DELETE ON sessions
            WHEN (SELECT count(*) FROM sessions) < 3
            BEGIN SELECT RAISE(FAIL, 'fixture deletion failure'); END;",
        )
        .unwrap();
        assert!(f.h.remove_project(&project).is_err());
        for id in [&f.parent, &second_id, &child.session_id, &other_id] {
            assert!(f.h.get(id).is_some());
            assert!(f.h.store.load_session(id).is_ok());
        }
        db.execute_batch("DROP TRIGGER reject_project_delete")
            .unwrap();
        f.h.remove_project(&project).unwrap();
        for id in [&f.parent, &second_id, &child.session_id] {
            assert!(f.h.get(id).is_none());
            assert!(f.h.store.load_session(id).is_err());
        }
        assert!(f.h.subagent_runs(&f.parent).is_empty());
        let (_, summaries, _) = f.h.store.load().unwrap();
        assert_eq!(
            summaries.iter().map(|s| &s.id).collect::<Vec<_>>(),
            vec![&other_id]
        );
        f.h.remove_project(&project).unwrap();
        assert!(f.h.store.load_session(&other_id).is_ok());
    }

    #[tokio::test]
    async fn acceptance_is_atomic_and_completion_survives_failed_storage_without_replay() {
        let mut f = Fixture::new().await;
        let db = rusqlite::Connection::open(f.h.store.root().join("sessions.sqlite3")).unwrap();
        crate::test_support::sqlite::fail_writes(&db, "subagent_runs", "disk fixture");
        assert!(f.spawn(Profile::Explore, None, "retry").is_err());
        assert_eq!(f.h.store.load().unwrap().1.len(), 1);
        assert!(f.h.subagent_runs(&f.parent).is_empty());
        assert!(f.requests.try_recv().is_err());
        crate::test_support::sqlite::allow_writes(&db, "subagent_runs");
        let run = f.spawn(Profile::Explore, None, "retry").unwrap();
        let child = f.request().await;
        assert_eq!(
            f.spawn(Profile::Explore, None, "retry").unwrap().run_id,
            run.run_id
        );
        assert_eq!(f.h.active_children(&f.parent), 1);
        crate::test_support::sqlite::fail_writes(&db, "subagent_runs", "disk fixture");
        child
            .reply
            .send(answer("Retain this full final answer λ"))
            .unwrap();
        until(|| {
            f.h.active_children(&f.parent) == 0 && !f.h.get(&run.session_id).unwrap().is_executing()
        })
        .await;
        let result = f.wait(vec![run.run_id.clone()], 0).await;
        assert_eq!(
            result["runs"][0]["final_response"],
            "Retain this full final answer λ"
        );
        assert!(f.h.flush().is_err());
        crate::test_support::sqlite::allow_writes(&db, "subagent_runs");
        f.h.flush().unwrap();
        let stored: String = db
            .query_row(
                "SELECT result FROM subagent_runs WHERE id=?1",
                [&run.run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, "Retain this full final answer λ");
        f.h.flush().unwrap();
        let parent = f.h.load(&f.parent).unwrap().snapshot();
        assert_eq!(parent.subagent_notices.len(), 1);
        assert_eq!(
            parent
                .entries
                .iter()
                .filter(|e| matches!(e.kind, EntryKind::Notice) && e.text.contains(&run.run_id))
                .count(),
            1
        );
        let notices: Vec<_> = parent
            .entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Notice) && e.text.contains(&run.run_id))
            .cloned()
            .collect();
        let reloaded = f.h.store.load_session(&f.parent).unwrap();
        assert_eq!(
            reloaded
                .entries
                .iter()
                .filter(|e| matches!(e.kind, EntryKind::Notice) && e.text.contains(&run.run_id))
                .cloned()
                .collect::<Vec<_>>(),
            notices
        );
        assert!(
            f.requests.try_recv().is_err(),
            "saving must not replay child or wake idle parent"
        );
    }

    #[tokio::test]
    async fn wait_preserves_oversized_result_without_a_budget_argument() {
        let mut f = Fixture::new().await;
        let run = f.spawn(Profile::Explore, None, "large result").unwrap();
        let response = format!(
            "begin:{}:end",
            "λ".repeat(crate::output::character_budget(WAIT_OUTPUT_TOKENS))
        );
        f.request().await.reply.send(answer(&response)).unwrap();
        let notice = f.wait_raw(vec![run.run_id.clone()], 5000).await;
        let saved = crate::test_support::saved_output(&notice);
        assert_eq!(
            serde_json::from_slice::<Value>(&saved).unwrap(),
            json!({
                "runs": [{"run_id": run.run_id, "session_id": run.session_id,
                          "status": "completed", "final_response": response}]
            })
        );
    }

    #[tokio::test]
    async fn failed_result_does_not_block_another_parents_send_or_completion() {
        let mut f = Fixture::new().await;
        let blocked = f.spawn(Profile::Review, None, "blocked").unwrap();
        let db = rusqlite::Connection::open(f.h.store.root().join("sessions.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER reject_one_result BEFORE UPDATE ON subagent_runs
            WHEN OLD.request='blocked' BEGIN SELECT RAISE(FAIL,'result write failed'); END;",
        )
        .unwrap();
        f.request().await.reply.send(answer("retained")).unwrap();
        until(|| !f.h.get(&blocked.session_id).unwrap().is_executing()).await;

        // The owner still cannot advance past its uncommitted child result.
        assert!(
            f.h.send(&f.parent, "retry", "continue".into(), Settings::default())
                .is_err()
        );
        let mut other = Session::new(f._dir.path().into(), &Settings::default());
        other.rename("Other parent".into());
        let other = f.h.insert(other);
        other.save().unwrap();
        f.h.send(
            &other.id,
            "independent",
            "continue".into(),
            Settings::default(),
        )
        .unwrap();
        let turn = f.request().await;
        assert_eq!(turn.body["prompt_cache_key"], other.id);
        turn.reply.send(answer("accepted")).unwrap();
        until(|| !other.is_executing()).await;
        assert!(
            f.h.store
                .load_session(&other.id)
                .unwrap()
                .entries
                .by_id("independent")
                .is_some()
        );

        let healthy =
            f.h.spawn_subagent(
                &other.id,
                Spawn {
                    profile: Profile::Review,
                    session_id: None,
                    task: "Check the other project".into(),
                    title: None,
                },
                "healthy",
                &CancellationToken::new(),
            )
            .unwrap();
        f.request()
            .await
            .reply
            .send(answer("independent result"))
            .unwrap();
        let wake = f.request().await;
        assert_eq!(wake.body["prompt_cache_key"], other.id);
        assert!(wake.body["input"].to_string().contains(&healthy.run_id));
        wake.reply.send(answer("acknowledged")).unwrap();
        until(|| f.h.all().iter().all(|c| !c.is_executing())).await;

        assert!(
            f.h.flush().is_err(),
            "global flush must report the blocked family"
        );
        let result = |id: &str| {
            db.query_row("SELECT result FROM subagent_runs WHERE id=?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap()
        };
        assert_eq!(
            result(&healthy.run_id).as_deref(),
            Some("independent result")
        );
        assert_eq!(result(&blocked.run_id), None);
        db.execute_batch("DROP TRIGGER reject_one_result").unwrap();
        f.h.flush().unwrap();
        assert_eq!(result(&blocked.run_id).as_deref(), Some("retained"));
        assert!(
            f.requests.try_recv().is_err(),
            "flushing must not rerun the child"
        );
    }

    #[test]
    fn older_assignments_do_not_change_latest_child_status() {
        let mut coordinator = RunIndex::default();
        let mut old = RunInfo {
            configuration: None,
            run_id: "old".into(),
            session_id: "child".into(),
            parent_id: "parent".into(),
            request_id: "first".into(),
            profile: Profile::Review,
            title: "first".into(),
            status: RunStatus::Failed,
            assignment: 1,
            started: 0,
            finished: Some(1),
            notified: false,
            attention_acknowledged: false,
        };
        let latest = RunInfo {
            run_id: "latest".into(),
            assignment: 2,
            status: RunStatus::Running,
            finished: None,
            ..old.clone()
        };
        coordinator.record(latest.clone());
        coordinator.record(old.clone()); // Startup query is unordered.
        assert_eq!(
            coordinator.status("parent"),
            ChildStatus {
                running: 1,
                needs_attention: false
            }
        );
        let sibling = RunInfo {
            run_id: "sibling".into(),
            session_id: "second child".into(),
            ..old.clone()
        };
        coordinator.record(sibling);
        old.attention_acknowledged = true;
        coordinator.record(old); // A late acknowledgement for a historical run.
        coordinator.record(RunInfo {
            status: RunStatus::Completed,
            ..latest
        });
        assert_eq!(
            coordinator.status("parent"),
            ChildStatus {
                running: 0,
                needs_attention: true
            }
        );
        assert_eq!(coordinator.latest_runs("parent").len(), 2);
        assert!(
            coordinator
                .run("parent", "old")
                .unwrap()
                .attention_acknowledged
        );
    }

    #[tokio::test]
    async fn sidebar_status_reads_do_not_wait_for_cold_session_filing() {
        let mut f = Fixture::new().await;
        let run = f
            .spawn(Profile::Review, None, "review the open session")
            .unwrap();
        let _pending_request = f.request().await;
        let h = &f.h;
        let parent = &f.parent;
        let cold = Session::new(f._dir.path().into(), &Settings::default());
        h.store.save_session(&cold).unwrap();

        // Stall the actual cold load rather than assuming a large transcript is slow.
        std::thread::scope(|scope| {
            let body = h.load_gate(&cold.id);
            let blocked_load = body.lock().unwrap();
            let family = h.family(&cold.id);
            let filing =
                scope.spawn(|| h.file_session(&cold.id, FilingAction::File(FilingKind::Settled)));
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while family.try_lock().is_ok() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "filing did not enter the cold load"
                );
                std::thread::yield_now();
            }

            let (read, done) = std::sync::mpsc::channel();
            scope.spawn(move || {
                assert_eq!(h.active_children(parent), 1);
                assert_eq!(h.child_statuses()[parent].running, 1);
                assert_eq!(h.subagent_runs(parent)[0].run_id, run.run_id);
                assert_eq!(
                    h.resident_subagent(parent, RunKey::Id(&run.run_id))
                        .unwrap()
                        .status,
                    RunStatus::Running
                );
                assert!(h.attention(parent).is_none());
                read.send(()).unwrap();
            });
            let responsive = done.recv_timeout(Duration::from_secs(5));
            drop(blocked_load);
            assert_eq!(filing.join().unwrap().unwrap(), FilingOutcome::Applied);
            responsive.expect("UI reads blocked on storage for a different session");
        });
        assert_eq!(
            h.store.load_session(&cold.id).unwrap().filing.unwrap().kind,
            FilingKind::Settled
        );
    }

    #[tokio::test]
    async fn attention_acknowledges_existing_child_failures_without_losing_results_or_new_alerts() {
        let mut f = Fixture::new().await;
        let first = f.spawn(Profile::Explore, None, "first failure").unwrap();
        let first_request = f.request().await;
        let second = f.spawn(Profile::Review, None, "later failure").unwrap();
        let second_request = f.request().await;
        assert!(f.h.attention(&first.session_id).is_none());
        let db = rusqlite::Connection::open(f.h.store.root().join("sessions.sqlite3")).unwrap();
        crate::test_support::sqlite::fail_writes(&db, "subagent_runs", "disk fixture");
        let failure = |message| {
            format!(
                "{}\n\n",
                json!({"type":"response.failed","response":{"status":"failed","error":{"message":message}}})
            )
        };
        first_request
            .reply
            .send(failure("First flagged request"))
            .unwrap();
        until(|| !f.h.get(&first.session_id).unwrap().is_executing()).await;
        let original = f.wait(vec![first.run_id.clone()], 0).await;
        assert_eq!(original["runs"][0]["status"], "failed");
        assert!(
            original["runs"][0]["error"]
                .as_str()
                .unwrap()
                .contains("First flagged request")
        );
        assert!(
            f.h.acknowledge_attention(f.h.attention(&f.parent).unwrap())
                .is_err()
        );
        assert!(f.h.child_statuses()[&f.parent].needs_attention);
        crate::test_support::sqlite::allow_writes(&db, "subagent_runs");

        let seen = f.h.attention(&f.parent).unwrap();
        second_request.reply.send(failure("Newer failure")).unwrap();
        let wake = f.request().await;
        wake.reply.send(answer("Observed both failures")).unwrap();
        until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
        f.h.acknowledge_attention(seen).unwrap();
        assert!(
            f.h.subagent_run(&f.parent, RunKey::Id(&first.run_id))
                .unwrap()
                .unwrap()
                .attention_acknowledged
        );
        assert!(
            !f.h.subagent_run(&f.parent, RunKey::Id(&second.run_id))
                .unwrap()
                .unwrap()
                .attention_acknowledged
        );
        assert!(f.h.child_statuses()[&f.parent].needs_attention);
        f.h.acknowledge_attention(f.h.attention(&f.parent).unwrap())
            .unwrap();
        assert!(!f.h.child_statuses()[&f.parent].needs_attention);
        f.h.flush().unwrap();
        assert_eq!(f.wait(vec![first.run_id.clone()], 0).await, original);
        let (reloaded, _) = RunIndex::load(&f.h.store).unwrap();
        for id in [&first.run_id, &second.run_id] {
            assert!(reloaded.run(&f.parent, id).unwrap().attention_acknowledged);
            assert_eq!(
                reloaded.run(&f.parent, id).unwrap().status,
                RunStatus::Failed
            );
            assert!(reloaded.run(&f.parent, id).unwrap().notified);
        }
    }

    #[tokio::test]
    async fn simultaneous_completion_and_failure_merge_with_user_steering_without_partial_results()
    {
        let mut f = Fixture::new().await;
        let explore = f.spawn(Profile::Explore, None, "explore").unwrap();
        let first = f.request().await;
        let review = f.spawn(Profile::Review, None, "review").unwrap();
        let second = f.request().await;
        f.h.send(
            &f.parent,
            "parent-work",
            "Work in parallel".into(),
            Settings::default(),
        )
        .unwrap();
        let parent = f.request().await;
        first.reply.send(format!("{}\n\n{}\n\n",
            json!({"type":"response.output_text.delta","item_id":"partial","delta":"Never return this partial answer"}),
            json!({"type":"response.failed","response":{"status":"failed","error":{"message":"Fixture failure"}}})
        )).unwrap();
        second.reply.send(answer("")).unwrap();
        until(|| f.h.active_children(&f.parent) == 0).await;
        f.h.send(
            &f.parent,
            "steering",
            "User steering takes precedence".into(),
            Settings::default(),
        )
        .unwrap();
        assert!(
            f.requests.try_recv().is_err(),
            "completion must not start overlapping parent requests"
        );
        let queued = f.h.load(&f.parent).unwrap().snapshot();
        assert!(matches!(
            queued.entries.by_id("parent-work").unwrap().kind,
            EntryKind::User {
                steering: false,
                ..
            }
        ));
        assert!(matches!(
            queued.entries.by_id("steering").unwrap().kind,
            EntryKind::User { steering: true, .. }
        ));
        parent.reply.send(answer("Reached boundary")).unwrap();
        let next = f.request().await;
        assert!(matches!(
            f.h.store
                .load_session(&f.parent)
                .unwrap()
                .entries
                .by_id("steering")
                .unwrap()
                .kind,
            EntryKind::User {
                steering: false,
                ..
            }
        ));
        let input = next.body["input"].to_string();
        assert!(input.contains(&explore.run_id) && input.contains(&review.run_id));
        assert!(input.contains("User steering takes precedence"));
        next.reply
            .send(answer("Acknowledged both results and steering"))
            .unwrap();
        until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
        assert!(f.requests.try_recv().is_err());
        let results = f.wait(vec![explore.run_id, review.run_id], 0).await;
        assert_eq!(results["runs"][0]["status"], "failed");
        assert!(results["runs"][0].get("final_response").is_none());
        assert!(
            !results
                .to_string()
                .contains("Never return this partial answer")
        );
        assert_eq!(results["runs"][1]["final_response"], "");
    }

    #[test]
    fn restart_keeps_history_cold_but_addressable_and_acknowledgeable() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let parent = Session::new(dir.path().into(), &Settings::default());
        let mut child = Session::new(dir.path().into(), &Settings::default());
        child.child = Some(Child {
            parent_id: parent.id.clone(),
            profile: Profile::Review,
        });
        for session in [&parent, &child] {
            store.save_session(session).unwrap();
        }
        // Insert in reverse order: neither rowid nor timestamps identify the latest.
        // The newest row lacks its required result; the previous readable one wins.
        for n in (1..=32).rev() {
            let status = match n {
                2 => RunStatus::Running, // recovery is required even though superseded
                3 | 4 => RunStatus::Failed,
                _ => RunStatus::Completed,
            };
            let run = RunInfo {
                run_id: format!("run-{n}"),
                session_id: child.id.clone(),
                parent_id: parent.id.clone(),
                request_id: format!("request-{n}"),
                profile: Profile::Review,
                configuration: None,
                title: format!("Task {n}"),
                status,
                assignment: n,
                started: 0,
                finished: (n != 2).then_some(1),
                notified: n != 2,
                attention_acknowledged: false,
            };
            store
                .db
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO subagent_runs
                (id,parent,session,request,metadata,result) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        run.run_id,
                        parent.id,
                        child.id,
                        run.request_id,
                        serde_json::to_string(&run).unwrap(),
                        (status == RunStatus::Completed && n != 32).then(|| format!("Finding {n}"))
                    ],
                )
                .unwrap();
        }
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE subagent_runs SET request='damaged' WHERE id='run-4'",
                [],
            )
            .unwrap();
        let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        assert!(h.startup_warnings.iter().any(|w| w.contains("run-32")));
        assert_eq!(h.subagent_runs(&parent.id)[0].run_id, "run-31");
        assert_eq!(h.child_statuses()[&parent.id], ChildStatus::default());
        let lookup = |key| h.subagent_run(&parent.id, key).unwrap().unwrap();
        assert_eq!(lookup(RunKey::Id("run-2")).status, RunStatus::Interrupted);
        assert!(
            h.resident_subagent(&parent.id, RunKey::Id("run-3"))
                .is_none()
        );
        let results = h
            .wait_subagent(
                &parent.id,
                Wait {
                    run_ids: vec!["run-1".into(), "run-2".into()],
                    timeout_ms: Some(0),
                },
                &CancellationToken::new(),
            )
            .unwrap();
        let results: Value = serde_json::from_str(&results).unwrap();
        assert_eq!(results["runs"][0]["final_response"], "Finding 1");
        assert_eq!(results["runs"][1]["status"], "interrupted");
        let replay = h
            .subagent_tool(
                &parent.id,
                "spawn_subagent",
                r#"{"type":"review","task":"Do not run twice"}"#,
                "request-1",
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<RunInfo>(&replay).unwrap().run_id,
            "run-1"
        );
        for key in [RunKey::Id("run-1"), RunKey::Request("request-1")] {
            assert!(h.subagent_run(&child.id, key).unwrap().is_none());
            assert!(h.resident_subagent(&parent.id, key).is_none());
        }
        let seen = h.attention(&child.id).unwrap();
        // A failure from a subsequent assignment of the *same* child commits
        // while that acknowledgement is queued on the UI storage worker.
        let later = RunInfo {
            run_id: "later".into(),
            request_id: "later".into(),
            assignment: 33,
            status: RunStatus::Failed,
            ..lookup(RunKey::Id("run-31"))
        };
        store
            .save_session_transaction(
                Some(&child),
                &child,
                crate::store::Checkpoint::StartRun(&later),
            )
            .unwrap();
        h.run_index().record(later);
        let error = h.acknowledge_attention(seen).unwrap_err();
        assert!(error.to_string().contains("identity mismatch"));
        assert!(lookup(RunKey::Id("run-3")).attention_acknowledged);
        assert!(!lookup(RunKey::Id("later")).attention_acknowledged);
        assert!(h.attention(&parent.id).is_some());
        assert!(h.child_statuses()[&parent.id].needs_attention);
        h.flush().unwrap();
        h.flush().unwrap();
        let notices = store.load_session(&parent.id).unwrap().subagent_notices;
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("run-2"));
        assert!(lookup(RunKey::Id("run-2")).notified);
        assert!(
            h.resident_subagent(&parent.id, RunKey::Id("run-2"))
                .is_none()
        );
    }

    #[test]
    fn restart_skips_only_malformed_assignments_and_preserves_their_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let parent = Session::new(dir.path().into(), &Settings::default());
        let unrelated = Session::new(dir.path().into(), &Settings::default());
        for session in [&parent, &unrelated] {
            store.save_session(session).unwrap();
        }
        let mut children = vec![];
        for request in ["bad-json", "identity-mismatch", "missing-result", "valid"] {
            let mut child = Session::new(dir.path().into(), &Settings::default());
            child.child = Some(Child {
                parent_id: parent.id.clone(),
                profile: Profile::Review,
            });
            let run = RunInfo {
                configuration: None,
                run_id: id(),
                parent_id: parent.id.clone(),
                session_id: child.id.clone(),
                request_id: request.into(),
                title: request.into(),
                profile: Profile::Review,
                status: if request == "missing-result" {
                    RunStatus::Completed
                } else {
                    RunStatus::Running
                },
                assignment: 1,
                started: now(),
                finished: None,
                notified: false,
                attention_acknowledged: false,
            };
            store
                .save_session_transaction(None, &child, crate::store::Checkpoint::StartRun(&run))
                .unwrap();
            children.push((child, run));
        }
        let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        db.execute_batch(
            "UPDATE subagent_runs SET metadata='{' WHERE request='bad-json';
            UPDATE subagent_runs SET request='other' WHERE request='identity-mismatch';",
        )
        .unwrap();
        let stored_rows = || {
            db.prepare(
                "SELECT json_array(id,parent,session,request,metadata,result,error)
                        FROM subagent_runs WHERE request!='valid' ORDER BY id",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
        };
        let before = stored_rows();
        let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        assert_eq!(h.load(&unrelated.id).unwrap().id, unrelated.id);
        assert_eq!(h.load(&parent.id).unwrap().id, parent.id);
        assert_eq!(h.startup_warnings.len(), 3);
        for (_, run) in &children[..3] {
            assert!(h.startup_warnings.iter().any(|w| w.contains(&run.run_id)));
            assert!(h.subagent_run(&parent.id, RunKey::Id(&run.run_id)).is_err());
        }
        let runs = h.subagent_runs(&parent.id);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, children[3].1.run_id);
        assert_eq!(runs[0].status, RunStatus::Interrupted);
        assert_eq!(stored_rows(), before);
        // Even a child with no readable assignments still belongs to the family.
        h.remove(&parent.id).unwrap();
        for (child, _) in children {
            assert!(!store.contains_session(&child.id).unwrap());
        }
        assert!(store.contains_session(&unrelated.id).unwrap());
    }

    #[tokio::test]
    async fn restart_retains_results_and_interrupts_unfinished_work_without_waking_parent() {
        let mut f = Fixture::new().await;
        let run = f.spawn(Profile::Review, None, "unfinished").unwrap();
        let request = f.request().await;
        let parent_before_stop = f.h.store.load_session(&f.parent).unwrap();
        f.h.stop_all();
        until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
        f.h.store.save_session(&parent_before_stop).unwrap();
        // Leave the durable record exactly as an interrupted process would have
        // left it, with the original accepted task and no terminal result.
        f.h.store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE subagent_runs SET metadata=?2,result=NULL,error=NULL WHERE id=?1",
                params![run.run_id, serde_json::to_string(&run).unwrap()],
            )
            .unwrap();
        let (endpoint, mut requests, server) = crate::test_support::ws_server(vec![(
            200,
            answer("Recovered parent acknowledges interruption"),
        )])
        .await;
        let recovered = Harness::with_provider(
            f.h.store.clone(),
            f.h.store.load().unwrap().1,
            Provider::fixture(f.h.store.clone(), endpoint),
        )
        .unwrap();
        let info = recovered.subagent_runs(&f.parent).pop().unwrap();
        assert_eq!(info.status, RunStatus::Interrupted);
        assert!(recovered.all().is_empty());
        let h = recovered.clone();
        let parent = f.parent.clone();
        let run_id = run.run_id;
        let out = tokio::task::spawn_blocking(move || {
            h.wait_subagent(
                &parent,
                Wait {
                    run_ids: vec![run_id],
                    timeout_ms: Some(0),
                },
                &CancellationToken::new(),
            )
        })
        .await
        .unwrap()
        .unwrap();
        assert!(!out.contains("final_response"));
        assert!(out.contains("interrupted"));
        assert!(
            recovered
                .load(&f.parent)
                .unwrap()
                .snapshot()
                .subagent_notices
                .is_empty()
        );
        recovered
            .send(
                &f.parent,
                "resume-parent",
                "What happened?".into(),
                Settings::default(),
            )
            .unwrap();
        until(|| !recovered.get(&f.parent).unwrap().is_executing()).await;
        let parent = recovered.load(&f.parent).unwrap().snapshot();
        assert!(
            parent
                .entries
                .iter()
                .any(|e| e.text.contains("Interrupted"))
        );
        assert_eq!(recovered.active_children(&f.parent), 0);
        server.await.unwrap();
        let request_body = requests.recv().await.unwrap();
        assert!(request_body.contains("What happened?"));
        recovered.stop_all();
        drop(request);
    }
}
