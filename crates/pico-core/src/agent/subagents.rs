//! Built-in delegation. The coordinator serializes acceptance, stop, delivery and
//! deletion; it is never held across an await or a wait for another agent.
use super::*;
use crate::store::{id, now};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn model(self) -> &'static str {
        match self {
            Self::Review => "gpt-6.1-sol",
            Self::Explore => "gpt-6-luna",
        }
    }
    pub fn effort(self) -> &'static str {
        match self {
            Self::Review => "medium",
            Self::Explore => "high",
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
    pub title: String,
    pub status: RunStatus,
    pub assignment: usize,
    pub started: u64,
    pub finished: Option<u64>,
    pub notified: bool,
    #[serde(default)]
    pub attention_acknowledged: bool,
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
#[derive(Default)]
pub(super) struct Coordinator {
    pub(super) runs: HashMap<String, RunInfo>,
    pub stopped: HashSet<String>,
    pub closing: bool,
    // Retain terminal text on storage failure. Flush retries without rerunning work.
    unsaved: HashMap<String, (RunInfo, Option<String>, Option<String>)>,
}
impl Coordinator {
    pub fn load(store: &Store) -> Result<Self> {
        let mut coordinator = Self::default();
        let db = store.db.lock().expect("database lock");
        let mut query = db.prepare(
            "SELECT id,parent,session,request,metadata,result IS NOT NULL FROM subagent_runs",
        )?;
        let rows = query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })?;
        for row in rows {
            let (id, parent, session, request, metadata, has_result) = row?;
            let mut run: RunInfo = serde_json::from_str(&metadata)?;
            if run.run_id != id
                || run.parent_id != parent
                || run.session_id != session
                || run.request_id != request
                || (run.status == RunStatus::Completed && !has_result)
            {
                bail!(
                    "Invalid subagent assignment {id}; database preserved. Restore it from a backup or repair it before retrying."
                );
            }
            if run.status == RunStatus::Running {
                run.status = RunStatus::Interrupted;
                run.finished = Some(now());
                db.execute("UPDATE subagent_runs SET metadata=?2,error=?3 WHERE id=?1", params![run.run_id, serde_json::to_string(&run)?, "Pico exited before this assignment finished. Commands are not replayed; continue through the parent after inspecting the workspace."])?;
            }
            coordinator.runs.insert(run.run_id.clone(), run);
        }
        Ok(coordinator)
    }
}

impl Harness {
    pub fn subagent_runs(&self, parent: &str) -> Vec<RunInfo> {
        let coordinator = self.subagents.lock().expect("subagent lock");
        let mut latest: HashMap<&str, &RunInfo> = HashMap::new();
        for run in coordinator.runs.values().filter(|r| r.parent_id == parent) {
            let entry = latest.entry(&run.session_id).or_insert(run);
            if run.assignment > entry.assignment {
                *entry = run;
            }
        }
        let mut runs: Vec<_> = latest.into_values().cloned().collect();
        runs.sort_by(|a, b| {
            b.started
                .cmp(&a.started)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        runs
    }
    pub fn active_children(&self, parent: &str) -> usize {
        self.subagents
            .lock()
            .expect("subagent lock")
            .runs
            .values()
            .filter(|r| r.parent_id == parent && r.status == RunStatus::Running)
            .count()
    }
    pub fn children_need_attention(&self, parent: &str) -> bool {
        self.subagent_runs(parent)
            .iter()
            .any(|r| r.status.needs_attention() && !r.attention_acknowledged)
    }
    pub fn subagent_run(&self, parent: &str, run_id: &str) -> Option<RunInfo> {
        self.subagents
            .lock()
            .expect("subagent lock")
            .runs
            .get(run_id)
            .filter(|run| run.parent_id == parent)
            .cloned()
    }
    pub fn subagent_for_request(&self, parent: &str, request: &str) -> Option<RunInfo> {
        self.subagents
            .lock()
            .expect("subagent lock")
            .runs
            .values()
            .find(|r| r.parent_id == parent && r.request_id == request)
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
        if let Some(run) = coordinator
            .runs
            .values()
            .find(|r| r.parent_id == parent && r.request_id == request)
        {
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
        if coordinator
            .runs
            .values()
            .filter(|r| r.parent_id == parent && r.status == RunStatus::Running)
            .count()
            >= 6
        {
            bail!(
                "Six subagents are already running. Wait for one to finish before spawning another."
            );
        }
        let settings = owner.last_settings.lock().expect("settings lock").clone();
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
                    .runs
                    .values()
                    .any(|r| r.session_id == child.id && r.status == RunStatus::Running)
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
            session.model = args.profile.model().into();
            session.effort = args.profile.effort().into();
            session.fast = false;
            session
        };
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
            title,
            status: RunStatus::Running,
            assignment: coordinator
                .runs
                .values()
                .filter(|r| r.session_id == session.id)
                .count()
                + 1,
            started: now(),
            finished: None,
            notified: false,
            attention_acknowledged: false,
        };
        session.accept_submission(run.run_id.clone(), args.task.into());
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
        coordinator.runs.insert(run.run_id.clone(), run.clone());
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
                    args.run_ids.iter().map(|id| coordinator.runs.get(id).filter(|r| r.parent_id == parent).cloned().with_context(|| format!("Unknown run_id {id} for this parent"))).collect::<Result<Vec<_>>>()?
                };
                if runs.iter().any(|r| r.status != RunStatus::Running) || tokio::time::Instant::now() >= deadline {
                    let mut results = vec![];
                    for run in runs {
                        let mut result = json!({"run_id":run.run_id,"session_id":run.session_id,"status":run.status});
                        if run.status != RunStatus::Running {
                            let coordinator = self.subagents.lock().expect("subagent lock");
                            let (text,error) = if let Some((_,text,error)) = coordinator.unsaved.get(&run.run_id) { (text.clone(),error.clone()) } else {
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
        let Some(mut run) = coordinator
            .runs
            .values()
            .find(|r| r.session_id == child.id && r.status == RunStatus::Running)
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
        coordinator.runs.insert(run.run_id.clone(), run.clone());
        coordinator
            .unsaved
            .insert(run.run_id.clone(), (run.clone(), text, error));
        self.retry_results(&mut coordinator)?;
        self.deliver_notices(&mut coordinator, &run.parent_id)?;
        if !coordinator.closing && !coordinator.stopped.contains(&run.parent_id) {
            let parent = self.load(&run.parent_id)?;
            let settings = parent.last_settings.lock().expect("settings lock").clone();
            self.start_inner(&run.parent_id, settings)?;
        }
        Ok(())
    }
    pub(super) fn retry_results(&self, coordinator: &mut Coordinator) -> Result<()> {
        let db = self.store.db.lock().expect("database lock");
        let ids: Vec<_> = coordinator.unsaved.keys().cloned().collect();
        for id in ids {
            let (run, text, error) = &coordinator.unsaved[&id];
            db.execute(
                "UPDATE subagent_runs SET metadata=?2,result=?3,error=?4 WHERE id=?1",
                params![id, serde_json::to_string(run)?, text, error],
            )?;
            coordinator.unsaved.remove(&id);
        }
        Ok(())
    }
    pub(super) fn deliver_notices(
        &self,
        coordinator: &mut Coordinator,
        parent: &str,
    ) -> Result<()> {
        let runs: Vec<_> = coordinator
            .runs
            .values()
            .filter(|r| r.parent_id == parent && r.status != RunStatus::Running && !r.notified)
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
            coordinator.runs.insert(run.run_id.clone(), run);
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
        let children: HashSet<_> = coordinator
            .runs
            .values()
            .filter(|r| r.parent_id == session)
            .map(|r| r.session_id.as_str())
            .collect();
        for child in children {
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
        let mut coordinator = self.subagents.lock().expect("subagent lock");
        if self.load(session)?.snapshot().child.is_some() {
            bail!("Delete children together with their parent");
        }
        let children: HashSet<_> = coordinator
            .runs
            .values()
            .filter(|r| r.parent_id == session)
            .map(|r| r.session_id.clone())
            .collect();
        if coordinator
            .runs
            .values()
            .any(|r| r.parent_id == session && r.status == RunStatus::Running)
            || children
                .iter()
                .chain(std::iter::once(&session.to_owned()))
                .any(|id| {
                    self.get(id)
                        .is_some_and(|c| c.is_running() || c.finishing.load(Ordering::Acquire))
                })
        {
            bail!("Stop the parent and all its children before deleting this session");
        }
        let _loading = self.loading.lock().expect("loading lock");
        let family: Vec<_> = children
            .iter()
            .chain(std::iter::once(&session.to_owned()))
            .filter_map(|id| self.get(id))
            .collect();
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
            for id in children.iter().chain(std::iter::once(&session.to_owned())) {
                tx.execute("DELETE FROM sessions WHERE id=?1", [id])?;
            }
            tx.commit()?;
        }
        for lease in &mut media_leases {
            **lease = false;
        }
        drop(media_leases);
        let mut cleanup_errors = Vec::new();
        for id in children.iter().chain(std::iter::once(&session.to_owned())) {
            if let Some(c) = self.get(id) {
                c.deleted.store(true, Ordering::Release);
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
        coordinator.runs.retain(|_, r| r.parent_id != session);
        coordinator
            .unsaved
            .retain(|_, (r, _, _)| r.parent_id != session);
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
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::mpsc,
    };
    struct Request {
        body: Value,
        reply: mpsc::UnboundedSender<String>,
    }
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
            let store = Store::open(dir.path().join("state")).unwrap();
            store
                .write(
                    "auth.json",
                    &crate::auth::Credentials {
                        access_token: "fixture".into(),
                        refresh_token: "fixture".into(),
                        expires_at: now() + 3600,
                        account_id: Some("fixture".into()),
                        email: None,
                    },
                )
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let (tx, requests) = mpsc::unbounded_channel();
            let server = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let mut bytes = vec![];
                        let end = loop {
                            let mut buffer = [0; 4096];
                            let n = socket.read(&mut buffer).await.unwrap();
                            if n == 0 {
                                return;
                            }
                            bytes.extend_from_slice(&buffer[..n]);
                            if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                                break end + 4;
                            }
                        };
                        let header = std::str::from_utf8(&bytes[..end]).unwrap();
                        let length: usize = header
                            .lines()
                            .find_map(|s| {
                                s.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .unwrap()
                            .parse()
                            .unwrap();
                        while bytes.len() < end + length {
                            let mut buffer = [0; 4096];
                            let n = socket.read(&mut buffer).await.unwrap();
                            if n == 0 {
                                return;
                            }
                            bytes.extend_from_slice(&buffer[..n]);
                        }
                        let (reply, mut rx) = mpsc::unbounded_channel();
                        if tx
                            .send(Request {
                                body: serde_json::from_slice(&bytes[end..end + length]).unwrap(),
                                reply,
                            })
                            .is_err()
                        {
                            return;
                        }
                        if socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
                            .await
                            .is_err()
                        {
                            return;
                        }
                        while let Some(body) = rx.recv().await {
                            let chunk = format!("{:x}\r\n{body}\r\n", body.len());
                            if socket.write_all(chunk.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        let _ = socket.write_all(b"0\r\n\r\n").await;
                    });
                }
            });
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
        assert_eq!(tools[0]["name"], "exec");
        assert_eq!(tools[0]["type"], "custom");
        assert_eq!(tools[0]["format"]["definition"], crate::code_mode::GRAMMAR);
        assert_eq!(tools[1]["name"], "wait");
        let description = tools[0]["description"].as_str().unwrap();
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
        assert_eq!(runtime["names"], json!(expected));
        assert_eq!(description.matches("### `").count(), expected.len());
        for tool in runtime["definitions"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(description.contains(&format!(
                "### `{name}`\n{}",
                tool["description"].as_str().unwrap()
            )));
            assert!(description.contains(&format!("declare const tools: {{ {name}(args:")));
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
            assert!(
                !instructions.contains(name),
                "tool guidance leaked into instructions: {name}"
            );
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
    async fn until(mut condition: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn child_activity_tracks_streamed_reasoning_and_message_text() {
        let mut f = Fixture::new().await;
        let run = f
            .spawn(Profile::Explore, None, "streaming activity")
            .unwrap();
        let request = f.request().await;
        let child = f.h.get(&run.session_id).unwrap();
        assert_eq!(child.status.lock().unwrap().clone(), "Thinking…");

        for (event, entry_id, text, expected) in [
            (
                "response.reasoning_summary_text.delta",
                "reasoning-1",
                "Inspecting",
                "Thinking…",
            ),
            (
                "response.output_text.delta",
                "commentary",
                "Here is my update",
                "Writing…",
            ),
            (
                "response.reasoning_summary_text.delta",
                "reasoning-2",
                "Checking again",
                "Thinking…",
            ),
            (
                "response.output_text.delta",
                "answer",
                "Finished",
                "Writing…",
            ),
        ] {
            request
                .reply
                .send(format!(
                    "data: {}\n\n",
                    json!({"type": event, "item_id": entry_id, "delta": text})
                ))
                .unwrap();
            until(|| child.snapshot().entries.by_id(entry_id).is_some()).await;
            assert_eq!(child.status.lock().unwrap().clone(), expected);
            assert!(child.is_running());
        }
        request.reply.send(answer("Finished")).unwrap();
        until(|| !child.is_running()).await;
        assert_eq!(child.status.lock().unwrap().clone(), "Completed");
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
        q.submit("pending-ask", vec!["Answer".into()]).unwrap();
        ask.await.unwrap().unwrap();
        assert!(!f.h.notification_is_current(&events[0]));
        assert!(
            f.h.take_notifications().is_empty(),
            "answering must not replay a past completion"
        );
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
            &["exec_command", "write_stdin", "view_image"],
        )
        .await;
        assert_eq!(explore_request.body["model"], Profile::Explore.model());
        assert_eq!(
            explore_request.body["reasoning"]["effort"],
            Profile::Explore.effort()
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
        assert_eq!(review_request.body["model"], Profile::Review.model());
        assert_eq!(
            review_request.body["reasoning"]["effort"],
            Profile::Review.effort()
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
        let result = f
            .wait(vec![explore.run_id.clone(), review.run_id.clone()], 100)
            .await;
        assert_eq!(
            result["runs"][0]["final_response"],
            "Final sapphire finding"
        );
        assert_eq!(result["runs"][1]["status"], "running");
        let resumed = f
            .spawn(
                Profile::Explore,
                Some(explore.session_id.clone()),
                "follow-up",
            )
            .unwrap();
        assert_ne!(resumed.run_id, explore.run_id);
        let continuation = f.request().await;
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
    async fn acceptance_is_atomic_and_completion_survives_failed_storage_without_replay() {
        let mut f = Fixture::new().await;
        let db = rusqlite::Connection::open(f.h.store.root().join("sessions.sqlite3")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_run BEFORE INSERT ON subagent_runs BEGIN SELECT RAISE(FAIL,'disk fixture'); END;").unwrap();
        assert!(f.spawn(Profile::Explore, None, "retry").is_err());
        assert_eq!(f.h.store.load().unwrap().1.len(), 1);
        assert!(f.h.subagent_runs(&f.parent).is_empty());
        assert!(f.requests.try_recv().is_err());
        db.execute_batch("DROP TRIGGER fail_run;").unwrap();
        let run = f.spawn(Profile::Explore, None, "retry").unwrap();
        let child = f.request().await;
        assert_eq!(
            f.spawn(Profile::Explore, None, "retry").unwrap().run_id,
            run.run_id
        );
        assert_eq!(f.h.active_children(&f.parent), 1);
        db.execute_batch("CREATE TRIGGER fail_terminal BEFORE UPDATE ON subagent_runs BEGIN SELECT RAISE(FAIL,'disk fixture'); END;").unwrap();
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
        db.execute_batch("DROP TRIGGER fail_terminal;").unwrap();
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
                .filter(|e| e.id == format!("subagent-{}", run.run_id))
                .count(),
            1
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
    async fn attention_acknowledges_existing_child_failures_without_losing_results_or_new_alerts() {
        let mut f = Fixture::new().await;
        let first = f.spawn(Profile::Explore, None, "first failure").unwrap();
        let first_request = f.request().await;
        let second = f.spawn(Profile::Review, None, "later failure").unwrap();
        let second_request = f.request().await;
        assert!(f.h.attention(&first.session_id).is_none());
        let db = rusqlite::Connection::open(f.h.store.root().join("sessions.sqlite3")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_terminal BEFORE UPDATE ON subagent_runs BEGIN SELECT RAISE(FAIL,'disk fixture'); END;").unwrap();
        let failure = |message| {
            format!(
                "data: {}\n\n",
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
        assert!(f.h.children_need_attention(&f.parent));
        db.execute_batch("DROP TRIGGER fail_terminal;").unwrap();

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
        assert!(f.h.children_need_attention(&f.parent));
        f.h.acknowledge_attention(f.h.attention(&f.parent).unwrap())
            .unwrap();
        assert!(!f.h.children_need_attention(&f.parent));
        f.h.flush().unwrap();
        assert_eq!(f.wait(vec![first.run_id.clone()], 0).await, original);
        let reloaded = Coordinator::load(&f.h.store).unwrap();
        for id in [&first.run_id, &second.run_id] {
            assert!(reloaded.runs[id].attention_acknowledged);
            assert_eq!(reloaded.runs[id].status, RunStatus::Failed);
            assert!(reloaded.runs[id].notified);
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
        first.reply.send(format!("data: {}\n\ndata: {}\n\n",
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
        parent.reply.send(answer("Reached boundary")).unwrap();
        let next = f.request().await;
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
        let child = f.h.load(&review.session_id).unwrap();
        let tools = tokio::task::spawn_blocking(move || {
            child.code.exec(
                "text(ALL_TOOLS.map(t=>t.name));text(typeof tools.spawn_subagent);",
                "inspect-tools",
                &CancellationToken::new(),
            )
        })
        .await
        .unwrap()
        .unwrap();
        assert!(tools.contains("undefined"));
        assert!(!tools.contains("spawn_subagent"));
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
        let (endpoint, mut requests, server) = crate::test_support::server(vec![(
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
