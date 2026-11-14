use crate::{
    auth::Auth,
    code_mode::{CodeMode, Event as CodeEvent},
    provider::{Delta, Provider},
    shell::Shell,
    store::{Entry, EntryKind, Session, SessionSummary, Settings, Store},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub struct Conversation {
    pub id: String,
    revision: Arc<AtomicU64>,
    changes: tokio::sync::watch::Sender<()>,
    pub state: Arc<Mutex<Session>>,
    pub running: AtomicBool,
    pub status: Mutex<String>,
    pub started: Mutex<Option<Instant>>,
    pub elapsed: Mutex<Duration>,
    pub output_baseline: AtomicU64,
    cancel: Mutex<CancellationToken>,
    deleted: Arc<AtomicBool>,
    code: CodeMode,
    persisted: Mutex<Option<Session>>,
    submissions: AtomicU64,
    loaded: AtomicBool,
    stopped: Condvar,
    store: Store,
}
impl Conversation {
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::Release);
        self.changes.send_replace(());
    }

    pub fn snapshot(&self) -> Session {
        self.state.lock().expect("session lock").clone()
    }
    // Lock ordering: persistence -> state. Disk I/O never holds the state lock.
    fn save_locked(&self, saved: &mut Option<Session>) -> Result<()> {
        if self.deleted.load(Ordering::Acquire) || !self.loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        let snapshot = self.snapshot();
        self.store.save_changes(saved.as_ref(), &snapshot)?;
        *saved = Some(snapshot);
        Ok(())
    }
    pub fn save(&self) -> Result<()> {
        self.save_locked(&mut self.persisted.lock().expect("persistence lock"))
    }
    async fn save_async(self: &Arc<Self>) -> Result<()> {
        let conversation = self.clone();
        tokio::task::spawn_blocking(move || conversation.save()).await?
    }
    pub fn is_executing(&self) -> bool {
        self.is_running() || self.code.is_executing()
    }
    pub fn cancel(&self) {
        let _state = self.state.lock().expect("session lock");
        self.cancel.lock().expect("cancel lock").cancel();
        self.code.stop_all();
        *self.status.lock().expect("status lock") = "Stopping…".into();
        self.changed();
    }
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
    pub fn duration(&self) -> Duration {
        self.started
            .lock()
            .expect("timer lock")
            .map(|t| t.elapsed())
            .unwrap_or(*self.elapsed.lock().expect("elapsed lock"))
    }
}
pub struct Harness {
    changes: tokio::sync::watch::Sender<()>,
    pub store: Store,
    pub auth: Arc<Auth>,
    provider: Arc<Provider>,
    runtime: Option<tokio::runtime::Runtime>,
    conversations: Mutex<HashMap<String, Arc<Conversation>>>,
    summaries: Mutex<HashMap<String, SessionSummary>>,
    catalog_revision: AtomicU64,
}
impl Harness {
    pub fn new(store: Store, sessions: Vec<SessionSummary>) -> Result<Arc<Self>> {
        let auth = Auth::new(store.clone())?;
        let provider = Arc::new(Provider::new(auth.clone())?);
        let harness = Arc::new(Self {
            changes: tokio::sync::watch::channel(()).0,
            store,
            auth,
            provider,
            runtime: Some(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?,
            ),
            conversations: Mutex::new(HashMap::new()),
            catalog_revision: AtomicU64::new(0),
            summaries: Mutex::new(sessions.into_iter().map(|s| (s.id.clone(), s)).collect()),
        });
        Ok(harness)
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.changes.subscribe()
    }
    pub fn insert(&self, session: Session) -> Arc<Conversation> {
        self.make_conversation(session, false)
    }
    fn make_conversation(&self, session: Session, persisted: bool) -> Arc<Conversation> {
        let summary = SessionSummary::from(&session);
        let id = session.id.clone();
        let saved = persisted.then(|| session.clone());
        let conversation = Arc::new_cyclic(|weak: &std::sync::Weak<Conversation>| {
            let target = weak.clone();
            let observer = Arc::new(move |event: CodeEvent| -> Result<()> {
                let Some(conversation) = target.upgrade() else {
                    return Ok(());
                };
                let mut saved = conversation.persisted.lock().expect("persistence lock");
                if conversation.deleted.load(Ordering::Acquire) {
                    bail!("Session deleted");
                }
                {
                    let mut session = conversation.state.lock().expect("session lock");
                    match event {
                        CodeEvent::Notification { parent, text } => {
                            session.pending_tool_outputs.push(json!({"type":"custom_tool_call_output","call_id":parent,"output":text}));
                            session.entries.push(Entry::new(
                                EntryKind::Notice,
                                format!("Cell notification: {text}"),
                            ));
                        }
                        CodeEvent::Tool(event) => {
                            if let Some(result) = event.output {
                                if let Some(entry) = session.entries.by_id_mut(&event.id)
                                    && let EntryKind::Tool {
                                        output, complete, ..
                                    } = &mut entry.kind
                                {
                                    *output = result;
                                    *complete = true;
                                }
                            } else {
                                session.entries.push(Entry {
                                    id: event.id,
                                    text: String::new(),
                                    kind: EntryKind::Tool {
                                        name: event.name,
                                        input: event.input,
                                        output: String::new(),
                                        complete: false,
                                        parent: Some(event.parent),
                                    },
                                });
                            }
                        }
                    }
                }
                let result = conversation.save_locked(&mut saved);
                if let Err(error) = &result {
                    conversation.state.lock().expect("session lock").entries.push(Entry::new(EntryKind::Notice,format!("Could not save tool state: {error:#}. Fix storage and retry saving before quitting.")));
                }
                conversation.changed();
                result
            });
            Conversation {
                id: id.clone(),
                revision: Arc::new(AtomicU64::new(0)),
                changes: self.changes.clone(),
                code: CodeMode::new(
                    Arc::new(Shell::with_runtime(
                        session.project.clone(),
                        self.runtime
                            .as_ref()
                            .expect("live runtime")
                            .handle()
                            .clone(),
                    )),
                    observer,
                ),
                state: Arc::new(Mutex::new(session)),
                running: AtomicBool::new(false),
                status: Mutex::new("Ready".into()),
                started: Mutex::new(None),
                elapsed: Mutex::new(Duration::ZERO),
                output_baseline: AtomicU64::new(0),
                cancel: Mutex::new(CancellationToken::new()),
                deleted: Arc::new(AtomicBool::new(false)),
                persisted: Mutex::new(saved),
                submissions: AtomicU64::new(0),
                loaded: AtomicBool::new(true),
                stopped: Condvar::new(),
                store: self.store.clone(),
            }
        });
        self.summaries
            .lock()
            .expect("summary lock")
            .insert(id.clone(), summary);
        self.conversations
            .lock()
            .expect("conversations lock")
            .insert(id, conversation.clone());
        self.catalog_revision.fetch_add(1, Ordering::Release);
        self.changes.send_replace(());
        conversation
    }
    // Loading and eviction are called from the serialized UI worker, never view/update.
    pub fn load(&self, id: &str) -> Result<Arc<Conversation>> {
        if let Some(c) = self.get(id) {
            let mut saved = c.persisted.lock().expect("persistence lock");
            if !c.loaded.load(Ordering::Acquire) {
                let session = self.store.load_session(id)?;
                *saved = Some(session.clone());
                *c.state.lock().expect("session lock") = session;
                c.loaded.store(true, Ordering::Release);
                c.changed();
            }
            drop(saved);
            return Ok(c);
        }
        let session = self.store.load_session(id)?;
        let interrupted = session.interrupted;
        let conversation = self.make_conversation(session, true);
        if interrupted {
            conversation.state.lock().expect("session lock").entries.push(Entry::new(EntryKind::Notice,"Previous turn was interrupted. Send a new message to continue; commands already executed are not undone."));
        }
        Ok(conversation)
    }
    pub fn catalog_revision(&self) -> u64 {
        self.catalog_revision.load(Ordering::Acquire)
    }
    pub fn summaries(&self) -> Vec<SessionSummary> {
        let updates: Vec<_> = self
            .all()
            .into_iter()
            .map(|c| SessionSummary::from(&*c.state.lock().expect("session lock")))
            .collect();
        let mut summaries = self.summaries.lock().expect("summary lock");
        for summary in updates {
            summaries.insert(summary.id.clone(), summary);
        }
        summaries.values().cloned().collect()
    }
    pub fn evict_inactive(&self, selected: Option<&str>) -> Result<()> {
        for c in self.all() {
            if Some(c.id.as_str()) == selected {
                continue;
            }
            let mut saved = c.persisted.lock().expect("persistence lock");
            if c.is_running() || c.code.is_executing() || !c.loaded.load(Ordering::Acquire) {
                continue;
            }
            c.save_locked(&mut saved)?;
            let mut state = c.state.lock().expect("session lock");
            self.summaries
                .lock()
                .expect("summary lock")
                .insert(c.id.clone(), SessionSummary::from(&*state));
            // Keep runtime IDs, unread results and JS store values, not the full transcript.
            state.entries = Default::default();
            state.input = Default::default();
            *saved = None;
            c.loaded.store(false, Ordering::Release);
            if !c.code.has_work() {
                self.conversations
                    .lock()
                    .expect("conversations lock")
                    .remove(&c.id);
            }
        }
        Ok(())
    }
    pub fn flush(&self) -> Result<()> {
        for c in self.all() {
            c.save()?;
        }
        Ok(())
    }
    pub fn get(&self, id: &str) -> Option<Arc<Conversation>> {
        self.conversations
            .lock()
            .expect("conversations lock")
            .get(id)
            .cloned()
    }
    pub fn all(&self) -> Vec<Arc<Conversation>> {
        self.conversations
            .lock()
            .expect("conversations lock")
            .values()
            .cloned()
            .collect()
    }
    pub fn edit(&self, id: &str, edit: impl FnOnce(&mut Session)) -> Result<()> {
        let conversation = self.load(id)?;
        let mut saved = conversation.persisted.lock().expect("persistence lock");
        let metadata_changed = {
            let mut state = conversation.state.lock().expect("session lock");
            let before = SessionSummary::from(&*state);
            edit(&mut state);
            before != SessionSummary::from(&*state)
        };
        // Failed writes retain unsaved changes in memory for retry, never discard user work.
        let result = conversation.save_locked(&mut saved);
        if metadata_changed {
            self.catalog_revision.fetch_add(1, Ordering::Release);
        }
        conversation.changed();
        result
    }
    pub fn remove(&self, id: &str) -> Result<()> {
        let conversation = self.get(id);
        if let Some(c) = &conversation {
            if c.is_running() {
                bail!("Stop the turn before deleting this session");
            }
            let _saved = c.persisted.lock().expect("persistence lock");
            self.store.delete_session(id)?;
            c.deleted.store(true, Ordering::Release);
        } else {
            self.store.delete_session(id)?;
        }
        if let Some(c) = conversation {
            c.cancel();
        }
        self.conversations
            .lock()
            .expect("conversations lock")
            .remove(id);
        self.summaries.lock().expect("summary lock").remove(id);
        self.catalog_revision.fetch_add(1, Ordering::Release);
        self.changes.send_replace(());
        Ok(())
    }
    pub fn send_new(
        self: &Arc<Self>,
        mut session: Session,
        submission: &str,
        message: String,
        settings: Settings,
    ) -> Result<()> {
        if message.trim().is_empty() {
            bail!("Type a message first");
        }
        session.accept_submission(submission.to_owned(), message);
        self.store.commit_new_session(&session)?;
        let id = session.id.clone();
        self.make_conversation(session, true);
        self.start(&id, settings)
    }
    pub fn send(
        self: &Arc<Self>,
        id: &str,
        submission: &str,
        message: String,
        settings: Settings,
    ) -> Result<()> {
        if message.trim().is_empty() {
            bail!("Type a message first");
        }
        let conversation = self.load(id)?;
        let needs_start = {
            let mut saved = conversation.persisted.lock().expect("persistence lock");
            let mut candidate = conversation.snapshot();
            if candidate.entries.by_id(submission).is_some() {
                candidate
                    .pending_submissions
                    .iter()
                    .any(|id| id == submission)
            } else {
                // Publish acceptance only after its transaction commits. Streaming can
                // continue while writing: merge this new message into the current state,
                // not by replacing it with the pre-write snapshot.
                candidate.accept_submission(submission.to_owned(), message.clone());
                self.store.save_changes(saved.as_ref(), &candidate)?;
                *saved = Some(candidate);
                let mut state = conversation.state.lock().expect("session lock");
                state.accept_submission(submission.to_owned(), message);
                conversation.submissions.fetch_add(1, Ordering::Release);
                self.catalog_revision.fetch_add(1, Ordering::Release);
                conversation.changed();
                true
            }
        };
        if needs_start {
            self.start(id, settings)?;
        }
        Ok(())
    }
    pub fn start(self: &Arc<Self>, id: &str, settings: Settings) -> Result<()> {
        let conversation = self.load(id)?;
        let cancel = {
            let mut state = conversation.state.lock().expect("session lock");
            while conversation.is_running()
                && conversation
                    .cancel
                    .lock()
                    .expect("cancel lock")
                    .is_cancelled()
            {
                state = conversation.stopped.wait(state).expect("stopping turn");
            }
            if conversation.is_running() {
                return Ok(());
            }
            let cancel = CancellationToken::new();
            *conversation.cancel.lock().expect("cancel lock") = cancel.clone();
            conversation
                .output_baseline
                .store(state.usage.total_output, Ordering::Relaxed);
            *conversation.started.lock().expect("timer lock") = Some(Instant::now());
            *conversation.status.lock().expect("status lock") = "Connecting…".into();
            // cancel() takes the same state lock: it cannot cancel an obsolete
            // token between publishing a running turn and installing its token.
            conversation.running.store(true, Ordering::Release);
            cancel
        };
        conversation.changed();
        self.catalog_revision.fetch_add(1, Ordering::Release);
        let harness = self.clone();
        self.runtime.as_ref().expect("live runtime").spawn(async move {
            loop {
                let submission = conversation.submissions.load(Ordering::Acquire);
                let result = harness.run(&conversation, settings.clone(), &cancel).await;
                {
                    let mut state = conversation.state.lock().expect("session lock");
                    if result.is_ok()
                        && (!state.pending.is_empty() || !state.pending_tool_outputs.is_empty())
                        && !cancel.is_cancelled()
                    {
                        continue;
                    }
                    if let Err(error) = &result {
                        let notice = if cancel.is_cancelled() {
                            "Turn stopped. Completed commands are not undone. Send a new message to continue.".into()
                        } else {
                            format!("{error:#}")
                        };
                        state.entries.push(Entry::new(EntryKind::Notice, notice));
                        repair_calls(&mut state);
                    }
                    state.interrupted = false;
                    state.needs_attention = result.is_err() && !cancel.is_cancelled();
                }
                if let Err(error) = conversation.save_async().await {
                    conversation.state.lock().expect("session lock").entries.push(Entry::new(
                        EntryKind::Notice,
                        format!("Could not save session: {error:#}. Keep Pico open, free disk space or fix permissions, then retry saving."),
                    ));
                }
                *conversation.status.lock().expect("status lock") = if cancel.is_cancelled() {
                    "Stopped"
                } else if result.is_ok() {
                    "Completed"
                } else {
                    "Needs attention"
                }.into();
                {
                    // Submission/start uses the same state lock. No accepted work can
                    // slip between this final check and the transition to idle.
                    let state = conversation.state.lock().expect("session lock");
                    let submitted = conversation.submissions.load(Ordering::Acquire) != submission;
                    let pending = !state.pending.is_empty() || !state.pending_tool_outputs.is_empty();
                    if !cancel.is_cancelled() && (submitted || result.is_ok() && pending) {
                        continue;
                    }
                    let elapsed = conversation.started.lock().expect("timer lock")
                        .take().expect("active timer").elapsed();
                    *conversation.elapsed.lock().expect("elapsed lock") = elapsed;
                    conversation.running.store(false, Ordering::Release);
                    conversation.stopped.notify_all();
                }
                harness.catalog_revision.fetch_add(1, Ordering::Release);
                conversation.changed();
                break;
            }
        });
        Ok(())
    }
    async fn run(
        &self,
        conversation: &Arc<Conversation>,
        settings: Settings,
        cancel: &CancellationToken,
    ) -> Result<()> {
        {
            let mut s = conversation.state.lock().expect("session lock");
            repair_calls(&mut s);
            s.interrupted = true;
            s.needs_attention = false;
            conversation.changed();
        }
        conversation.save_async().await?;
        let credentials = self.auth.token(None).await?;
        let account = credentials.account_id.or(credentials.email);
        {
            let mut s = conversation.state.lock().expect("session lock");
            if s.context_account != account && s.context_account.is_some() {
                s.consume_pending();
                s.input = plaintext_context(&s).into();
                s.usage.input = 0;
                s.entries.push(Entry::new(EntryKind::Notice, "ChatGPT account changed. Rebuilt context from the preserved transcript; encrypted provider context was not sent to the new account."));
            }
            s.context_account = account;
            conversation.changed();
        }
        conversation.save_async().await?;
        let cwd = conversation.snapshot().project;
        let instructions = instructions(&cwd)?;
        let mut compacted_size = None;
        loop {
            if cancel.is_cancelled() {
                bail!("Turn cancelled");
            }
            let snapshot = {
                let mut s = conversation.state.lock().expect("session lock");
                s.consume_pending();
                conversation.changed();
                s.clone()
            };
            conversation.save_async().await?;
            let estimated =
                (serde_json::to_vec(&snapshot.input)?.len() + instructions.len() + 8_000) as u64
                    / 3;
            let context = snapshot.usage.input.max(estimated);
            if context > settings.context_limit * u64::from(settings.compact_percent) / 100
                && compacted_size != Some(snapshot.input.len())
            {
                *conversation.status.lock().expect("status lock") = "Compacting context…".into();
                conversation.changed();
                compacted_size = Some(
                    self.compact_context(
                        conversation,
                        &snapshot,
                        &instructions,
                        settings.context_limit,
                        cancel,
                    )
                    .await?,
                );
                continue;
            }
            *conversation.status.lock().expect("status lock") = "Thinking…".into();
            conversation.changed();
            let target = conversation.clone();
            let result = self
                .provider
                .respond(
                    &snapshot,
                    &instructions,
                    cancel,
                    &move |delta| match delta {
                        Delta::Status(status) => {
                            *target.status.lock().expect("status lock") = status;
                            target.changed();
                        }
                        Delta::Text {
                            id,
                            text,
                            reasoning,
                        } => {
                            let mut s = target.state.lock().expect("session lock");
                            if let Some(entry) = s.entries.by_id_mut(&id) {
                                entry.text.push_str(&text);
                            } else {
                                s.entries.push(Entry {
                                    id,
                                    text,
                                    kind: if reasoning {
                                        EntryKind::Reasoning
                                    } else {
                                        EntryKind::Assistant
                                    },
                                });
                            }
                            target.changed();
                        }
                    },
                )
                .await;
            let response = match result {
                Ok(response) => response,
                Err(error)
                    if error.is::<crate::provider::ContextLimit>()
                        && compacted_size != Some(snapshot.input.len()) =>
                {
                    *conversation.status.lock().expect("status lock") =
                        "Recovering context capacity…".into();
                    conversation.changed();
                    compacted_size = Some(
                        self.compact_context(
                            conversation,
                            &snapshot,
                            &instructions,
                            (settings.context_limit / 2).max(8000),
                            cancel,
                        )
                        .await?,
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            let calls: Vec<_> = response
                .output
                .iter()
                .filter(|v| {
                    matches!(
                        v["type"].as_str(),
                        Some("function_call" | "custom_tool_call")
                    )
                })
                .cloned()
                .collect();
            {
                let mut s = conversation.state.lock().expect("session lock");
                reconcile_text(&mut s, &response.output);
                let total = s.usage.total_output + response.usage.output;
                s.usage = response.usage;
                s.usage.total_output = total;
                s.input.extend(response.output);
                conversation.changed();
            }
            conversation.save_async().await?;
            for call in &calls {
                if cancel.is_cancelled() {
                    bail!("Turn cancelled");
                }
                let name = call["name"]
                    .as_str()
                    .context("Missing tool name")?
                    .to_owned();
                let call_id = call["call_id"]
                    .as_str()
                    .context("Missing tool call ID")?
                    .to_owned();
                let input = if call["type"] == "custom_tool_call" {
                    call["input"].as_str()
                } else {
                    call["arguments"].as_str()
                }
                .context("Missing tool input")?
                .to_owned();
                *conversation.status.lock().expect("status lock") = format!("Running {name}…");
                {
                    let mut s = conversation.state.lock().expect("session lock");
                    s.entries.push(Entry {
                        id: call_id.clone(),
                        text: String::new(),
                        kind: EntryKind::Tool {
                            name: name.clone(),
                            input: input.clone(),
                            output: String::new(),
                            complete: false,
                            parent: None,
                        },
                    });
                    conversation.changed();
                }
                conversation.save_async().await?;
                let conv = conversation.clone();
                let token = cancel.clone();
                let parent = call_id.clone();
                let result = tokio::task::spawn_blocking(move || match name.as_str() {
                    "exec" => conv.code.exec(&input, &parent, &token),
                    "wait" => conv.code.wait(&input, &token),
                    _ => bail!("Unknown tool {name}; only exec and wait are top-level tools"),
                })
                .await?;
                let output = result.unwrap_or_else(|e| format!("Tool error: {e:#}"));
                {
                    let mut s = conversation.state.lock().expect("session lock");
                    if let Some(entry) = s.entries.by_id_mut(&call_id)
                        && let EntryKind::Tool {
                            output: result,
                            complete,
                            ..
                        } = &mut entry.kind
                    {
                        *result = output.clone();
                        *complete = true;
                    }
                    s.input.push(json!({"type":if call["type"] == "custom_tool_call" { "custom_tool_call_output" } else { "function_call_output" },"call_id":call_id,"output":output}));
                    conversation.changed();
                }
                conversation.save_async().await?;
            }
            // A steering message accepted during streaming/tool execution is consumed at this boundary.
            let state = conversation.state.lock().expect("session lock");
            if calls.is_empty() && state.pending.is_empty() && state.pending_tool_outputs.is_empty()
            {
                return Ok(());
            }
        }
    }
    async fn compact_context(
        &self,
        conversation: &Arc<Conversation>,
        snapshot: &Session,
        instructions: &str,
        limit: u64,
        cancel: &CancellationToken,
    ) -> Result<usize> {
        let input = self
            .provider
            .compact(snapshot, instructions, limit, cancel)
            .await?;
        let size = {
            let mut session = conversation.state.lock().expect("session lock");
            session.input = input.input.into();
            if let Some(summary) = input.summary {
                session.summary = summary;
                session.summarized_entries = input.summarized_entries;
            }
            session.usage.input = 0;
            session.entries.push(Entry::new(
                EntryKind::Notice,
                "Context compacted automatically. The complete transcript is preserved.",
            ));
            session.input.len()
        };
        conversation.save_async().await?;
        conversation.changed();
        Ok(size)
    }
    pub fn stop_all(&self) {
        for conversation in self.all() {
            conversation.cancel();
        }
    }
}
fn repair_calls(session: &mut Session) {
    let completed: HashSet<_> = session
        .input
        .iter()
        .filter(|v| {
            matches!(
                v["type"].as_str(),
                Some("function_call_output" | "custom_tool_call_output")
            )
        })
        .filter_map(|v| v["call_id"].as_str())
        .collect();
    let unfinished: HashSet<_> = session
        .entries
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                EntryKind::Tool {
                    parent: None,
                    complete: false,
                    ..
                }
            )
        })
        .map(|e| e.id.as_str())
        .collect();
    let missing: Vec<_> = session.input.iter().filter(|v|matches!(v["type"].as_str(),Some("function_call"|"custom_tool_call"))).filter(|call| {
        let id=call["call_id"].as_str().unwrap_or(""); !completed.contains(id) || unfinished.contains(id)
    }).map(|call|json!({"type":if call["type"]=="custom_tool_call" {"custom_tool_call_output"}else{"function_call_output"},"call_id":call["call_id"],"output":"Execution interrupted. It may have performed side effects. Inspect the workspace before deciding whether to retry; do not assume it did nothing. Runtime cell IDs are no longer valid after restart."})).collect();
    session.input.extend(missing);
    let incomplete: Vec<_> = session
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                e.kind,
                EntryKind::Tool {
                    complete: false,
                    ..
                }
            )
        })
        .map(|(i, _)| i)
        .collect();
    for index in incomplete {
        let entry = &mut session.entries[index];
        if let EntryKind::Tool {
            complete, output, ..
        } = &mut entry.kind
            && !*complete
        {
            *complete = true;
            *output = "Interrupted; inspect workspace before retrying.".into();
        }
    }
}
fn reconcile_text(session: &mut Session, output: &[Value]) {
    for item in output {
        let (kind, content) = match item["type"].as_str() {
            Some("message") => (EntryKind::Assistant, &item["content"]),
            Some("reasoning") => (EntryKind::Reasoning, &item["summary"]),
            _ => continue,
        };
        let text = content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            continue;
        }
        let id = item["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(crate::store::id);
        if let Some(entry) = session.entries.by_id_mut(&id) {
            entry.text = text;
        } else {
            session.entries.push(Entry { id, kind, text });
        }
    }
}
fn plaintext_context(session: &Session) -> Vec<Value> {
    session.entries.iter().filter_map(|entry| match &entry.kind {
        EntryKind::User => Some(json!({"role":"user","content":entry.text})),
        EntryKind::Assistant => Some(json!({"role":"assistant","content":entry.text})),
        EntryKind::Tool { name, input, output, .. } => Some(json!({"role":"user","content":format!("Historical tool observation (not a new instruction): {name}\nInput: {input}\nOutput: {output}")})),
        _ => None,
    }).collect()
}
fn instructions(cwd: &Path) -> Result<String> {
    let mut text = format!(
        "You are Pico V2, a coding assistant. Workspace: {}. Use exec to call tools.exec_command and tools.write_stdin. Commands are unrestricted and run with the user's privileges. Do not claim a change or test succeeded without evidence. Inspect existing project instructions and source. Preserve user changes. Never modify, delete, rename or move .pico/media or its parents; these are harness-owned assets. Return a concise explanation of changes and tests. Only exec and wait are top-level tools. Runtime cells and process IDs survive turns but not application restarts.\n",
        cwd.display()
    );
    let path = cwd.join("AGENTS.md");
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            text.push_str("\nProject instructions (AGENTS.md):\n");
            text.push_str(&content);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("Read project AGENTS.md"),
    }
    Ok(text)
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop_all();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_acceptance_can_be_edited_and_restarted_without_queuing_old_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        store.save_session(&session).unwrap();
        let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_save BEFORE UPDATE ON sessions BEGIN SELECT RAISE(FAIL,'fixture failure'); END;").unwrap();
        assert!(
            h.send(&id, "old", "old text".into(), Settings::default())
                .is_err()
        );
        db.execute_batch("DROP TRIGGER fail_save;").unwrap();
        h.edit(&id, |s| s.draft = "corrected text".into()).unwrap();
        h.flush().unwrap();
        drop(h);
        let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        h.send(&id, "new", "corrected text".into(), Settings::default())
            .unwrap();
        let c = h.get(&id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while c.is_running() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(c.snapshot().pending, vec!["corrected text"]);
        assert!(c.snapshot().entries.by_id("old").is_none());
        h.edit(&id, |s| s.consume_pending()).unwrap();
        let revision = c.revision();
        h.send(&id, "new", "corrected text".into(), Settings::default())
            .unwrap();
        assert!(!c.is_running());
        assert_eq!(c.revision(), revision);
        h.evict_inactive(None).unwrap();
        assert!(
            h.summaries()
                .iter()
                .find(|s| s.id == id)
                .unwrap()
                .needs_attention
        );
    }
    #[test]
    fn failed_submission_retry_is_idempotent_and_interrupted_load_keeps_positions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        session.interrupted = true;
        session
            .entries
            .push(Entry::new(EntryKind::Assistant, "original"));
        store.save_session(&session).unwrap();
        let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        let c = h.load(&id).unwrap();
        assert_eq!(c.snapshot().entries.len(), 2);
        let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_save BEFORE UPDATE ON sessions BEGIN SELECT RAISE(FAIL,'fixture disk failure'); END;").unwrap();
        assert!(
            h.send(
                &id,
                "submission-1",
                "one request".into(),
                Settings::default()
            )
            .is_err()
        );
        assert!(!c.is_running());
        assert!(
            c.snapshot().pending.is_empty(),
            "failed acceptance must not be published"
        );
        db.execute_batch("DROP TRIGGER fail_save;").unwrap();
        h.send(
            &id,
            "submission-1",
            "one request".into(),
            Settings::default(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while c.is_running() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            c.snapshot()
                .entries
                .iter()
                .filter(|e| matches!(e.kind, EntryKind::User))
                .count(),
            1
        );
        h.evict_inactive(None).unwrap();
        let c = h.load(&id).unwrap();
        assert_eq!(c.snapshot().entries[0].text, "original");
        assert!(c.snapshot().entries[1].text.contains("Previous turn"));
        h.edit(&id, |s| s.entries[2].text = "edited request".into())
            .unwrap();
        let restored = store.load_session(&id).unwrap();
        assert_eq!(restored.entries[2].text, "edited request");
        assert_eq!(restored.entries[0].text, "original");
    }
    #[test]
    fn evicting_transcript_keeps_completed_cell_results_and_shared_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let h = Harness::new(store.clone(), vec![]).unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        session
            .entries
            .push(Entry::new(EntryKind::User, "preserved history"));
        let c = h.insert(session);
        let result=c.code.exec("// @exec: {\"yield_time_ms\": 1}\nstore('key','retained');await new Promise(r=>setTimeout(r,30));text('unread result');","parent",&CancellationToken::new()).unwrap();
        let cell = result
            .lines()
            .find_map(|l| l.strip_prefix("Script running with cell ID "))
            .unwrap()
            .to_owned();
        let deadline = Instant::now() + Duration::from_secs(3);
        while c.code.is_executing() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        h.evict_inactive(None).unwrap();
        assert!(!c.loaded.load(Ordering::Acquire));
        assert!(c.snapshot().entries.is_empty());
        let loaded = h.load(&id).unwrap();
        assert!(Arc::ptr_eq(&c, &loaded));
        assert_eq!(loaded.snapshot().entries[0].text, "preserved history");
        assert!(
            loaded
                .code
                .wait(
                    &json!({"cell_id":cell}).to_string(),
                    &CancellationToken::new()
                )
                .unwrap()
                .contains("unread result")
        );
        assert!(
            loaded
                .code
                .exec("text(load('key'));", "next", &CancellationToken::new())
                .unwrap()
                .contains("retained")
        );
    }
    #[tokio::test]
    async fn revisions_notify_completion_and_background_changes_without_polling() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        let harness = Harness::new(store, vec![]).unwrap();
        harness.insert(session);
        let conversation = harness.get(&id).unwrap();
        let mut changes = harness.subscribe();
        let initial = conversation.revision();
        harness
            .send(
                &id,
                "offline-submission",
                "Offline request".into(),
                Settings::default(),
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                changes.changed().await.unwrap();
                if !conversation.is_running() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(conversation.revision() > initial);
        assert_eq!(*conversation.status.lock().unwrap(), "Needs attention");
        assert!(
            conversation
                .snapshot()
                .entries
                .iter()
                .any(|e| e.text.contains("Sign in"))
        );
        changes.borrow_and_update();
        let settled = conversation.revision();
        let worker = conversation.clone();
        tokio::task::spawn_blocking(move || {
            worker.code.exec(
                "notify('after foreground turn');",
                "parent",
                &CancellationToken::new(),
            )
        })
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), changes.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(!conversation.is_running());
        assert!(conversation.revision() > settled);
        assert_eq!(
            conversation.snapshot().pending_tool_outputs[0]["output"],
            "after foreground turn"
        );
    }
    #[test]
    fn background_notifications_are_durable_and_deleted_sessions_stay_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        store.save_session(&session).unwrap();
        let harness = Harness::new(store.clone(), vec![]).unwrap();
        harness.insert(session);
        let conversation = harness.get(&id).unwrap();
        conversation
            .code
            .exec(
                "notify('durable progress');",
                "old-call",
                &CancellationToken::new(),
            )
            .unwrap();
        let (_, saved, _) = store.load().unwrap();
        assert_eq!(
            store
                .load_session(&saved[0].id)
                .unwrap()
                .pending_tool_outputs[0]["output"],
            "durable progress"
        );
        conversation.code.exec("// @exec: {\"yield_time_ms\": 5}\ntext(await tools.exec_command({cmd:'sleep 1; printf done',login:false,yield_time_ms:2000}));", "parent", &CancellationToken::new()).unwrap();
        harness.remove(&id).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        assert!(!store.root().join(format!("sessions/{id}.json")).exists());
        assert!(store.load().unwrap().1.is_empty());
    }
    #[test]
    fn missing_login_preserves_a_message_across_retry_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        let harness = Harness::new(store.clone(), vec![]).unwrap();
        harness.insert(session);
        harness
            .send(
                &id,
                "preserved-submission",
                "Do not lose this request".into(),
                Settings::default(),
            )
            .unwrap();
        for attempt in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(3);
            while harness.get(&id).unwrap().is_running() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            let snapshot = harness.get(&id).unwrap().snapshot();
            assert_eq!(snapshot.pending, vec!["Do not lose this request"]);
            assert_eq!(
                snapshot
                    .entries
                    .iter()
                    .filter(|e| matches!(e.kind, EntryKind::User))
                    .count(),
                1
            );
            assert!(snapshot.entries.iter().any(|e| e.text.contains("Sign in")));
            if attempt == 0 {
                harness.start(&id, Settings::default()).unwrap();
            }
        }
        let (_, sessions, _) = store.load().unwrap();
        assert_eq!(
            store.load_session(&sessions[0].id).unwrap().pending,
            vec!["Do not lose this request"]
        );
    }
    #[test]
    fn repair_does_not_repeat_successful_calls_and_closes_orphans() {
        let mut s = Session::new(std::env::temp_dir(), &Settings::default());
        s.input = vec![
            json!({"type":"custom_tool_call","call_id":"done"}),
            json!({"type":"custom_tool_call_output","call_id":"done","output":"ok"}),
            json!({"type":"function_call","call_id":"interrupted"}),
        ]
        .into();
        repair_calls(&mut s);
        assert_eq!(s.input.len(), 4);
        assert_eq!(s.input[3]["call_id"], "interrupted");
        assert!(
            s.input[3]["output"]
                .as_str()
                .unwrap()
                .contains("side effects")
        );
        repair_calls(&mut s);
        assert_eq!(s.input.len(), 4);
    }
}
