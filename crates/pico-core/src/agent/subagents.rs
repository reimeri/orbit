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
impl RunInfo {
    fn from_row(row: &rusqlite::Row<'_>) -> Result<Self> {
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
    max_output_tokens: Option<usize>,
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
    // Retain complete terminal text on storage failure, without rerunning work.
    unsaved: HashMap<String, (Option<String>, Option<String>)>,
}

impl Family {
    fn record(&mut self, run: RunInfo) {
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
            self.latest
                .insert(run.session_id.clone(), run.run_id.clone());
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

#[derive(Default)]
pub(super) struct Coordinator {
    families: HashMap<String, Family>,
    pub stopped: HashSet<String>,
    pub closing: bool,
}
impl Coordinator {
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
    pub fn load(store: &Store) -> Result<(Self, Vec<String>)> {
        let mut coordinator = Self::default();
        let mut warnings = Vec::new();
        let db = store.db.lock().expect("database lock");
        let mut query = db.prepare(
            "SELECT rowid,id,parent,session,request,metadata,result IS NOT NULL AS has_result FROM subagent_runs",
        )?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let mut run = match RunInfo::from_row(row) {
                Ok(run) => run,
                Err(error) => {
                    let id = match row.get::<_, String>("id") {
                        Ok(id) => id,
                        Err(_) => format!("at row {}", row.get::<_, i64>("rowid")?),
                    };
                    warnings.push(format!("Skipped subagent assignment {id}: {error}. Other assignments and conversations remain available. The database row was preserved; repair it or restore a backup and restart to recover this assignment."));
                    continue;
                }
            };
            if run.status == RunStatus::Running {
                run.status = RunStatus::Interrupted;
                run.finished = Some(now());
                db.execute("UPDATE subagent_runs SET metadata=?2,error=?3 WHERE id=?1", params![run.run_id, serde_json::to_string(&run)?, "Pico exited before this assignment finished. Commands are not replayed; continue through the parent after inspecting the workspace."])?;
            }
            coordinator.record(run);
        }
        Ok((coordinator, warnings))
    }
}

impl Harness {
    pub fn subagent_runs(&self, parent: &str) -> Vec<RunInfo> {
        let coordinator = self.subagents.lock().expect("subagent lock");
        coordinator.latest_runs(parent)
    }
    pub fn child_statuses(&self) -> HashMap<String, ChildStatus> {
        self.subagents
            .lock()
            .expect("subagent lock")
            .families
            .iter()
            .map(|(id, family)| (id.clone(), family.status()))
            .collect()
    }
    pub fn active_children(&self, parent: &str) -> usize {
        self.subagents
            .lock()
            .expect("subagent lock")
            .status(parent)
            .running
    }
    pub fn subagent_run(&self, parent: &str, run_id: &str) -> Option<RunInfo> {
        self.subagents
            .lock()
            .expect("subagent lock")
            .run(parent, run_id)
            .cloned()
    }
    pub fn subagent_for_request(&self, parent: &str, request: &str) -> Option<RunInfo> {
        self.subagents
            .lock()
            .expect("subagent lock")
            .runs(parent)
            .find(|r| r.request_id == request)
            .cloned()
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
        let mut coordinator = self.subagents.lock().expect("subagent lock");
        if let Some(run) = coordinator.runs(parent).find(|r| r.request_id == request) {
            return Ok(run.clone());
        }
        if cancel.is_cancelled() || coordinator.closing || coordinator.stopped.contains(parent) {
            bail!("Parent stopped; no child was started");
        }
        let owner = self.load(parent)?;
        let snapshot = owner.snapshot();
        if snapshot.child.is_some() {
            bail!("Subagents cannot delegate");
        }
        if coordinator.status(parent).running >= 6 {
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
                || coordinator
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
                    .db
                    .lock()
                    .expect("database lock")
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
            self.store
                .save_session_transaction(saved.as_ref(), &session, false, Some(&run))?;
            *child.state.lock().expect("session lock") = session.clone();
            *saved = Some(session);
            child.changed();
        } else {
            self.store
                .save_session_transaction(None, &session, false, Some(&run))?;
            self.make_conversation(session, true);
        }
        coordinator.record(run.clone());
        // start_inner only schedules work; its worker cannot finish under this lock.
        self.start_inner(&run.session_id, settings)?;
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
                let runs = {
                    let coordinator = self.subagents.lock().expect("subagent lock");
                    args.run_ids.iter().map(|id| coordinator.run(parent, id).cloned().with_context(|| format!("Unknown run_id {id} for this parent"))).collect::<Result<Vec<_>>>()?
                };
                if runs.iter().any(|r| r.status != RunStatus::Running) || tokio::time::Instant::now() >= deadline {
                    let mut results = vec![];
                    for run in runs {
                        let mut result = json!({"run_id":run.run_id,"session_id":run.session_id,"status":run.status});
                        if run.status != RunStatus::Running {
                            let coordinator = self.subagents.lock().expect("subagent lock");
                            let (text,error) = if let Some((text,error)) = coordinator.families.get(parent).and_then(|f| f.unsaved.get(&run.run_id)) { (text.clone(),error.clone()) } else {
                                self.store.db.lock().expect("database lock").query_row("SELECT result,error FROM subagent_runs WHERE id=?1",[&run.run_id],|r| Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?)))?
                            };
                            if run.status == RunStatus::Completed { result["final_response"] = json!(text.context("Completed assignment result is missing; database preserved. Restore it from a backup or repair it before retrying")?); }
                            else { result["error"] = json!(error); }
                        }
                        results.push(result);
                    }
                    return Ok(crate::output::preserve_text(&serde_json::to_string(&json!({"runs":results}))?, crate::output::character_budget(args.max_output_tokens.unwrap_or(10_000))));
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
        let mut coordinator = self.subagents.lock().expect("subagent lock");
        let parent = child
            .state
            .lock()
            .expect("session lock")
            .child
            .as_ref()
            .expect("child assignment")
            .parent_id
            .clone();
        let Some(mut run) = coordinator
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
        coordinator.record(run.clone());
        coordinator
            .families
            .get_mut(&parent)
            .expect("recorded family")
            .unsaved
            .insert(run.run_id.clone(), (text, error));
        self.retry_results(&mut coordinator, &parent)?;
        self.deliver_notices(&mut coordinator, &run.parent_id)?;
        if !coordinator.closing && !coordinator.stopped.contains(&run.parent_id) {
            let parent = self.load(&run.parent_id)?;
            let settings = parent.last_settings.lock().expect("settings lock").clone();
            self.start_inner(&run.parent_id, settings)?;
        }
        Ok(())
    }
    pub(super) fn retry_results(&self, coordinator: &mut Coordinator, parent: &str) -> Result<()> {
        let Some(family) = coordinator.families.get_mut(parent) else {
            return Ok(());
        };
        if family.unsaved.is_empty() {
            return Ok(());
        }
        let mut db = self.store.db.lock().expect("database lock");
        let tx = db.transaction()?;
        for (id, (text, error)) in &family.unsaved {
            tx.execute(
                "UPDATE subagent_runs SET metadata=?2,result=?3,error=?4 WHERE id=?1",
                params![id, serde_json::to_string(&family.runs[id])?, text, error],
            )?;
        }
        tx.commit()?;
        family.unsaved.clear();
        Ok(())
    }
    /// Explicit flush is global. A broken family must not stop another family's
    /// results and notices from becoming durable.
    pub(super) fn flush_results(&self, coordinator: &mut Coordinator) -> Result<()> {
        let parents: Vec<_> = coordinator.families.keys().cloned().collect();
        let mut failures = Vec::new();
        for parent in parents {
            if let Err(error) = self
                .retry_results(coordinator, &parent)
                .and_then(|()| self.deliver_notices(coordinator, &parent))
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
        coordinator: &mut Coordinator,
        parent: &str,
    ) -> Result<()> {
        let runs: Vec<_> = coordinator
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
        conversation.save_locked(&mut saved)?;
        for mut run in runs {
            run.notified = true;
            self.store.db.lock().expect("database lock").execute(
                "UPDATE subagent_runs SET metadata=?2 WHERE id=?1",
                params![run.run_id, serde_json::to_string(&run)?],
            )?;
            coordinator.record(run);
        }
        conversation.changed();
        Ok(())
    }
    pub fn stop(&self, session: &str) {
        let mut coordinator = self.subagents.lock().expect("subagent lock");
        coordinator.stopped.insert(session.into());
        if let Some(c) = self.get(session) {
            c.cancel();
        }
        for child in coordinator
            .families
            .get(session)
            .into_iter()
            .flat_map(|f| f.latest.keys())
        {
            if let Some(c) = self.get(child) {
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
    /// The coordinator guard excludes turn/child starts for the entire operation.
    pub fn remove_project(&self, project: &std::path::Path) -> Result<()> {
        self.remove_sessions(Deletion::Project(project))
    }

    fn remove_sessions(&self, target: Deletion<'_>) -> Result<()> {
        let mut coordinator = self.subagents.lock().expect("subagent lock");
        let project_deletion = matches!(target, Deletion::Project(_));
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
        let mut ids = parents.clone();
        ids.extend(
            parents
                .iter()
                .filter_map(|id| coordinator.families.get(id))
                .flat_map(|f| f.latest.keys().cloned()),
        );
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
        if parents.iter().any(|id| coordinator.status(id).running > 0)
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
        let _loading = self.loading.lock().expect("loading lock");
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
        {
            let mut db = self.store.db.lock().expect("database lock");
            let tx = db.transaction()?;
            for id in &ids {
                tx.execute("DELETE FROM sessions WHERE id=?1", [id])?;
            }
            tx.commit()?;
        }
        for lease in &mut media_leases {
            **lease = false;
        }
        drop(media_leases);
        let mut cleanup_errors = Vec::new();
        for id in &ids {
            if let Some(c) = self.get(id) {
                c.deleted.store(true, Ordering::Release);
                c.naming_cancel.cancel();
                c.cancel();
                if let Err(e) = c.code.remove_media() {
                    cleanup_errors.push(format!("{id}: {e:#}"));
                }
            } else if let Err(e) = crate::media::remove(&self.store.root().join("media").join(id)) {
                cleanup_errors.push(format!("{id}: {e:#}"));
            }
            self.notifications.clear(id);
            self.conversations
                .lock()
                .expect("conversations lock")
                .remove(id);
            self.summaries.lock().expect("summary lock").remove(id);
        }
        for parent in parents {
            coordinator.families.remove(&parent);
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
        async fn wait(&self, ids: Vec<String>, timeout: u64) -> Value {
            let h = self.h.clone();
            let parent = self.parent.clone();
            let out = tokio::task::spawn_blocking(move || {
                h.wait_subagent(
                    &parent,
                    Wait {
                        run_ids: ids,
                        timeout_ms: Some(timeout),
                        max_output_tokens: None,
                    },
                    &CancellationToken::new(),
                )
            })
            .await
            .unwrap()
            .unwrap();
            serde_json::from_str(&out).unwrap()
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
        let instructions = request["instructions"].as_str().unwrap();
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
    async fn child_web_catalogs_follow_saved_policy_for_both_profiles() {
        use crate::web_search::Mode;

        for mode in [Mode::Live, Mode::Cached, Mode::Disabled] {
            let mut f = Fixture::new().await;
            let mut catalog = crate::store::Catalog::default();
            catalog.settings.web_search = mode;
            f.h.store.save_catalog(&catalog).unwrap();
            // Keep provider streams open while inspecting the children's catalogs.
            let mut pending = vec![];
            for profile in [Profile::Explore, Profile::Review] {
                let child = f.spawn(profile, None, profile.label()).unwrap();
                let request = f.request().await;
                let mut expected = vec!["exec_command", "write_stdin", "view_image"];
                if mode != Mode::Disabled {
                    expected.push("web__run");
                }
                assert_tool_catalog(&f, &child.session_id, &request.body, &expected).await;

                if mode == Mode::Disabled {
                    let conversation = f.h.get(&child.session_id).unwrap();
                    let output = tokio::task::spawn_blocking(move || {
                        conversation.code.exec(
                            "try { __call('web__run', '{}'); } catch (e) { text(String(e)); }",
                            "disabled-web",
                            &CancellationToken::new(),
                        )
                    })
                    .await
                    .unwrap()
                    .unwrap();
                    assert!(output.contains("Unknown nested tool"), "{output}");
                }
                pending.push(request);
            }
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
            explore_request.body["instructions"]
                .as_str()
                .unwrap()
                .contains("sapphire invariant")
        );
        assert!(
            explore_request.body["instructions"]
                .as_str()
                .unwrap()
                .contains("READ-ONLY")
        );
        assert!(
            !explore_request.body["instructions"]
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
        assert_eq!(wake.body["prompt_cache_key"], f.parent);
        assert!(wake.body["input"].to_string().contains(&explore.run_id));
        assert!(
            !wake.body["input"]
                .to_string()
                .contains("Final sapphire finding")
        );
        // Parent retrieves results through the same code-mode contract it is taught.
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
        // Retrying a completed deletion is harmless.
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
        let h = f.h.clone();
        let parent = f.parent.clone();
        let run_id = run.run_id.clone();
        let spill = tokio::task::spawn_blocking(move || {
            h.wait_subagent(
                &parent,
                Wait {
                    run_ids: vec![run_id],
                    timeout_ms: Some(0),
                    max_output_tokens: Some(0),
                },
                &CancellationToken::new(),
            )
        })
        .await
        .unwrap()
        .unwrap();
        let saved = crate::test_support::saved_output(&spill);
        assert_eq!(serde_json::from_slice::<Value>(&saved).unwrap(), result);
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
        let mut coordinator = Coordinator::default();
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
            f.h.subagent_run(&f.parent, &first.run_id)
                .unwrap()
                .attention_acknowledged
        );
        assert!(
            !f.h.subagent_run(&f.parent, &second.run_id)
                .unwrap()
                .attention_acknowledged
        );
        assert!(f.h.child_statuses()[&f.parent].needs_attention);
        f.h.acknowledge_attention(f.h.attention(&f.parent).unwrap())
            .unwrap();
        assert!(!f.h.child_statuses()[&f.parent].needs_attention);
        f.h.flush().unwrap();
        assert_eq!(f.wait(vec![first.run_id.clone()], 0).await, original);
        let (reloaded, _) = Coordinator::load(&f.h.store).unwrap();
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
                .save_session_transaction(None, &child, false, Some(&run))
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
            assert!(h.subagent_run(&parent.id, &run.run_id).is_none());
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
                    max_output_tokens: None,
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
        // A user turn delivers the retained observation, without resuming the child.
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
