pub mod attention;
mod execution;
mod naming;
pub mod notifications;
pub mod subagents;
use crate::{
    auth::Auth,
    code_mode::{Calls, CodeMode, Event as CodeEvent},
    provider::{Provider, is_tool_call},
    shell::Shell,
    store::{Entry, EntryKind, Filing, FilingKind, Session, SessionSummary, Settings, Store},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, RwLock, RwLockReadGuard, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// Runtime activity for presentation and attention decisions. Not a persistence
/// or execution guard: terminal persistence can still be in progress.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConversationStatus {
    #[default]
    Ready,
    Connecting,
    Thinking,
    Writing,
    RunningTool(String),
    Retrying(crate::provider::RetryStatus),
    CompactingContext,
    RecoveringContext,
    Stopping,
    Stopped,
    Completed,
    NeedsAttention,
}
impl std::fmt::Display for ConversationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ready => "Ready",
            Self::Connecting => "Connecting…",
            Self::Thinking => "Thinking…",
            Self::Writing => "Writing…",
            Self::RunningTool(name) => return write!(f, "Running {name}…"),
            Self::Retrying(status) => return status.fmt(f),
            Self::CompactingContext => "Compacting context…",
            Self::RecoveringContext => "Recovering context capacity…",
            Self::Stopping => "Stopping…",
            Self::Stopped => "Stopped",
            Self::Completed => "Completed",
            Self::NeedsAttention => "Needs attention",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilingAction {
    File(FilingKind),
    StopCommandsAndFile(FilingKind),
    Undo,
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilingOutcome {
    Applied,
    RunningCommands(FilingKind),
}

pub struct Conversation {
    pub id: String,
    notifications: Arc<notifications::Notifications>,
    pub questionnaires: Option<Arc<crate::questionnaire::Questionnaires>>,
    revision: AtomicU64,
    changes: tokio::sync::watch::Sender<()>,
    pub state: Mutex<Session>,
    pub running: AtomicBool,
    manual_compaction: AtomicBool,
    finishing: AtomicBool,
    naming: AtomicBool,
    naming_cancel: CancellationToken,
    status: Mutex<ConversationStatus>,
    pub turn_usage: Mutex<TurnUsage>,
    cancel: Mutex<CancellationToken>,
    deleted: AtomicBool,
    code: CodeMode,
    live_calls: Arc<Calls>,
    last_settings: Mutex<Settings>,
    persisted: Mutex<Option<Session>>,
    pending_activity: Mutex<Vec<crate::store::activity::Sample>>,
    submissions: AtomicU64,
    turn_generation: AtomicU64,
    loaded: AtomicBool,
    restart_after_stop: AtomicBool,
    store: Store,
}
impl Conversation {
    pub fn status(&self) -> ConversationStatus {
        self.status.lock().expect("status lock").clone()
    }
    fn set_status(&self, status: ConversationStatus) {
        *self.status.lock().expect("status lock") = status;
    }
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
        let (snapshot, samples) = {
            let state = self.state.lock().expect("session lock");
            let samples =
                std::mem::take(&mut *self.pending_activity.lock().expect("activity lock"));
            (state.clone(), samples)
        };
        if samples.is_empty() && saved.as_ref() == Some(&snapshot) {
            return Ok(());
        }
        if let Err(error) = self.store.save_session_transaction(
            saved.as_ref(),
            &snapshot,
            crate::store::Checkpoint::Save(&samples),
        ) {
            // A normal save or flush retries the measurements as well as the transcript.
            self.pending_activity
                .lock()
                .expect("activity lock")
                .extend(samples);
            return Err(error);
        }
        *saved = Some(snapshot);
        Ok(())
    }
    pub fn save(&self) -> Result<()> {
        self.save_locked(&mut self.persisted.lock().expect("persistence lock"))
    }
    async fn save_async(self: &Arc<Self>) -> Result<()> {
        let conversation = self.clone();
        #[cfg(test)]
        let queued = std::time::Instant::now();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let wait = queued.elapsed();
            #[cfg(test)]
            if let Some(trace) = &conversation.store.save_trace {
                trace
                    .send((
                        conversation.id.clone(),
                        crate::test_support::SaveTiming::WorkerQueue(wait),
                    ))
                    .expect("live benchmark receiver");
            }
            conversation.save()
        })
        .await?
    }
    /// Scheduling hint only. In particular, do not acquire shell/process locks
    /// on the UI thread to decide whether to enqueue housekeeping.
    pub fn idle_body(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
            && !self.naming.load(Ordering::Acquire)
            && !self.is_turn_active()
    }
    /// The main turn includes terminal persistence, not just model/tool execution.
    pub fn is_turn_active(&self) -> bool {
        self.is_running() || self.finishing.load(Ordering::Acquire)
    }
    pub fn is_executing(&self) -> bool {
        self.is_turn_active()
            || self.code.is_executing()
            || self
                .questionnaires
                .as_ref()
                .is_some_and(|q| q.has_pending())
    }
    /// None for historical entries; otherwise whether Stop is still available.
    pub fn tool_stop_state(&self, entry: &str) -> Option<crate::code_mode::ToolStop> {
        self.live_calls.state(entry)
    }
    pub fn stop_tool(&self, entry: &str) {
        self.live_calls.stop(entry);
    }
    pub fn cancel(&self) {
        let _state = self.state.lock().expect("session lock");
        self.restart_after_stop.store(false, Ordering::Release);
        self.cancel.lock().expect("cancel lock").cancel();
        self.code.stop_all();
        self.notifications.clear(&self.id);
        self.set_status(if self.is_running() {
            ConversationStatus::Stopping
        } else {
            ConversationStatus::Stopped
        });
        self.changed();
    }
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire) || self.is_manually_compacting()
    }
    pub fn is_manually_compacting(&self) -> bool {
        self.manual_compaction.load(Ordering::Acquire)
    }
}
/// Usage and streaming time from completed model responses in the current turn.
/// Keeping these together prevents TPS from changing during tools or idle time.
#[derive(Clone, Copy, Default)]
pub struct TurnUsage {
    pub output: u64,
    pub streaming_time: Duration,
}
impl TurnUsage {
    fn record_response(&mut self, response: &crate::provider::Response) {
        self.output += response.usage.output;
        self.streaming_time += response.streaming_time;
    }
}

#[derive(Default)]
struct FamilyRegistry {
    gates: HashMap<String, Arc<Mutex<subagents::Coordinator>>>,
    // After Stop All, only an explicit start may reopen a family, including
    // one that wasn't resident when shutdown began.
    stopped_by_default: bool,
}

pub struct Harness {
    home: Option<std::path::PathBuf>,
    notifications: Arc<notifications::Notifications>,
    changes: tokio::sync::watch::Sender<()>,
    pub store: Store,
    pub auth: Arc<Auth>,
    provider: Provider,
    image_client: reqwest::Client,
    web_client: reqwest::Client,
    runtime: Option<tokio::runtime::Runtime>,
    conversations: Mutex<HashMap<String, Arc<Conversation>>>,
    summaries: Mutex<HashMap<String, SessionSummary>>,
    catalog_revision: AtomicU64,
    // Lock order: library (shared except global barriers), family lifecycle,
    // per-ID hydration, conversation persistence, then the short-lived state lock.
    library: RwLock<()>,
    families: Mutex<FamilyRegistry>,
    loading: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    run_index: Mutex<subagents::RunIndex>,
    /// Recoverable problems opening individual records, for the startup UI.
    pub startup_warnings: Vec<String>,
    weak: std::sync::Weak<Self>,
}
impl Harness {
    pub fn new(store: Store, sessions: Vec<SessionSummary>) -> Result<Arc<Self>> {
        let auth = Auth::new(store.clone())?;
        let provider = Provider::new(auth)?;
        Self::with_environment(store, sessions, provider, std::env::home_dir())
    }
    #[cfg(test)]
    fn with_provider(
        store: Store,
        sessions: Vec<SessionSummary>,
        provider: Provider,
    ) -> Result<Arc<Self>> {
        Self::with_environment(store, sessions, provider, None)
    }
    fn with_environment(
        store: Store,
        sessions: Vec<SessionSummary>,
        provider: Provider,
        home: Option<std::path::PathBuf>,
    ) -> Result<Arc<Self>> {
        let auth = provider.auth.clone();
        let (run_index, startup_warnings) = subagents::RunIndex::load(&store)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let image_client = crate::image_tools::ImageTools::client()?;
        let web_client = crate::web_search::WebSearch::client()?;
        Ok(Arc::new_cyclic(|weak| Self {
            notifications: Arc::new(notifications::Notifications::default()),
            home,
            image_client,
            web_client,
            weak: weak.clone(),
            library: RwLock::default(),
            families: Mutex::default(),
            loading: Mutex::default(),
            run_index: Mutex::new(run_index),
            startup_warnings,
            changes: tokio::sync::watch::channel(()).0,
            store,
            auth,
            provider,
            runtime: Some(runtime),
            conversations: Mutex::new(HashMap::new()),
            catalog_revision: AtomicU64::new(0),
            summaries: Mutex::new(sessions.into_iter().map(|s| (s.id.clone(), s)).collect()),
        }))
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
            let harness = self.weak.clone();
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
                        CodeEvent::ExecutionStatus { id, status } => {
                            let entry = session.entries.by_id_mut(&id).expect("execution entry");
                            let EntryKind::Tool {
                                command_status,
                                name,
                                complete,
                                ..
                            } = &mut entry.kind
                            else {
                                unreachable!("execution status belongs to a tool entry");
                            };
                            *command_status = Some(status);
                            // The invocation checkpoints its current cell status with its
                            // result. After a yield, exits need their own checkpoint.
                            if name == "exec" && !*complete {
                                return Ok(());
                            }
                        }
                        CodeEvent::Tool(event) => {
                            if let Some(result) = event.output {
                                // A successful ask_user result is accepted human input,
                                // not ordinary tool activity. Save its age with the answer.
                                if event.name == "ask_user" && !event.failed {
                                    session.updated = crate::store::now();
                                    if let Some(harness) = harness.upgrade() {
                                        harness.catalog_revision.fetch_add(1, Ordering::Release);
                                    }
                                }
                                if let Some(entry) = session.entries.by_id_mut(&event.id)
                                    && let EntryKind::Tool {
                                        output,
                                        complete,
                                        failed,
                                        media,
                                        web,
                                        ..
                                    } = &mut entry.kind
                                {
                                    *media = event.media;
                                    *web = event.web;
                                    *output = result;
                                    *complete = true;
                                    *failed = event.failed;
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
                                        failed: false,
                                        command_status: None,
                                        cell_id: None,
                                        parent: Some(event.parent),
                                        media: vec![],
                                        web: None,
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
            let questionnaires = session.child.is_none().then(|| {
                let target = weak.clone();
                let front = Mutex::new(None);
                Arc::new(crate::questionnaire::Questionnaires::new(move || {
                    if let Some(c) = target.upgrade() {
                        let mut front = front.lock().expect("questionnaire notification");
                        let pending = c
                            .questionnaires
                            .as_ref()
                            .expect("main questionnaires")
                            .pending();
                        let next = pending.first().map(|q| q.id.clone());
                        if *front != next {
                            let state = c.state.lock().expect("session lock");
                            if let Some(id) = &next {
                                if !c.deleted.load(Ordering::Acquire)
                                    && !c.cancel.lock().expect("cancel lock").is_cancelled()
                                {
                                    c.notifications
                                        .publish(&state, notifications::Kind::Question(id.clone()));
                                }
                            } else {
                                c.notifications.clear_question(&c.id);
                            }
                            *front = next;
                        }
                        c.changed();
                    }
                }))
            });
            let history_target = weak.clone();
            let images = Arc::new(crate::image_tools::ImageTools::new(
                self.store.clone(),
                self.auth.clone(),
                session.project.clone(),
                &id,
                session.child.is_none(),
                Arc::new(move || {
                    let conversation = history_target.upgrade().context("Session closed")?;
                    if conversation.deleted.load(Ordering::Acquire) {
                        bail!("Session deleted");
                    }
                    Ok(conversation.snapshot())
                }),
                self.image_client.clone(),
            ));
            let web = {
                let target = weak.clone();
                Arc::new(crate::web_search::WebSearch::new(
                    self.auth.clone(),
                    id.clone(),
                    Arc::new(move || {
                        let conversation = target.upgrade().context("Session closed")?;
                        if conversation.deleted.load(Ordering::Acquire) {
                            bail!("Session deleted");
                        }
                        Ok(conversation
                            .state
                            .lock()
                            .expect("session lock")
                            .model
                            .clone())
                    }),
                    self.web_client.clone(),
                ))
            };
            Conversation {
                id: id.clone(),
                notifications: self.notifications.clone(),
                revision: AtomicU64::new(0),
                changes: self.changes.clone(),
                last_settings: Mutex::new(Settings::default()),
                live_calls: Arc::new(Calls::new(self.changes.clone())),
                code: CodeMode::with_tools(
                    Arc::new(Shell::with_runtime(
                        session.project.clone(),
                        self.runtime
                            .as_ref()
                            .expect("live runtime")
                            .handle()
                            .clone(),
                    )),
                    observer,
                    session.child.is_none().then(|| {
                        let harness = self.weak.clone();
                        let owner = id.clone();
                        Arc::new(
                            move |name: &str,
                                  args: &str,
                                  request: &str,
                                  cancel: &CancellationToken| {
                                harness
                                    .upgrade()
                                    .context("Harness closed")?
                                    .subagent_tool(&owner, name, args, request, cancel)
                            },
                        ) as crate::code_mode::Delegation
                    }),
                    questionnaires.clone(),
                )
                .with_images(images)
                .with_web(web),
                questionnaires,
                state: Mutex::new(session),
                running: AtomicBool::new(false),
                manual_compaction: AtomicBool::new(false),
                finishing: AtomicBool::new(false),
                naming: AtomicBool::new(false),
                naming_cancel: CancellationToken::new(),
                status: Mutex::new(ConversationStatus::Ready),
                turn_usage: Mutex::new(TurnUsage::default()),
                cancel: Mutex::new(CancellationToken::new()),
                deleted: AtomicBool::new(false),
                persisted: Mutex::new(saved),
                pending_activity: Mutex::default(),
                submissions: AtomicU64::new(0),
                turn_generation: AtomicU64::new(0),
                loaded: AtomicBool::new(true),
                restart_after_stop: AtomicBool::new(false),
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
    /// Children share admission with their parent (including stop and deletion).
    /// Only consult in-memory metadata here; the registry lock never spans I/O.
    pub fn family_id(&self, id: &str) -> String {
        if let Some(s) = self.summaries.lock().expect("summary lock").get(id) {
            return s
                .child
                .as_ref()
                .map_or_else(|| id.to_owned(), |child| child.parent_id.clone());
        }
        // Recovery can skip a damaged child summary but retain its assignment.
        self.run_index().parent_of(id).unwrap_or(id).to_owned()
    }
    fn family(&self, id: &str) -> Arc<Mutex<subagents::Coordinator>> {
        let root = self.family_id(id);
        let mut registry = self.families.lock().expect("family registry");
        let stopped = registry.stopped_by_default;
        registry
            .gates
            .entry(root)
            .or_insert_with(|| Arc::new(Mutex::new(subagents::Coordinator::new(stopped))))
            .clone()
    }
    fn admit(&self, id: &str) -> (RwLockReadGuard<'_, ()>, Arc<Mutex<subagents::Coordinator>>) {
        let library = self.library.read().expect("library lock");
        (library, self.family(id))
    }

    // Key by session ID, not summary metadata: even a child whose summary was
    // damaged must not republish a cold WAL snapshot across family deletion.
    // Weak slots keep the gate stable for all waiters without retaining every
    // cold session (or invalid lookup) for the lifetime of the library.
    fn load_gate(&self, id: &str) -> Arc<Mutex<()>> {
        let mut gates = self.loading.lock().expect("load registry");
        if let Some(gate) = gates.get(id).and_then(Weak::upgrade) {
            return gate;
        }
        // Reap on growth, not once per ID while deleting a large project.
        if gates.len() == gates.capacity() {
            gates.retain(|_, gate| gate.strong_count() != 0);
        }
        let gate = Arc::new(Mutex::new(()));
        gates.insert(id.to_owned(), Arc::downgrade(&gate));
        gate
    }
    pub fn load(&self, id: &str) -> Result<Arc<Conversation>> {
        let gate = self.load_gate(id);
        let _loading = gate.lock().expect("loading lock");
        self.load_locked(id)
    }
    fn load_locked(&self, id: &str) -> Result<Arc<Conversation>> {
        if let Some(c) = self.get(id) {
            if !c.loaded.load(Ordering::Acquire) {
                let mut saved = c.persisted.lock().expect("persistence lock");
                let session = self.store.load_session(id)?;
                *saved = Some(session.clone());
                *c.state.lock().expect("session lock") = session;
                c.loaded.store(true, Ordering::Release);
                c.changed();
            }
            return Ok(c);
        }
        let session = self.store.load_session(id)?;
        let session = if let Some(repaired) = recover_after_restart(&session) {
            // Publish only after every repair is durable. On failure the next load
            // starts from the unchanged on-disk checkpoint, not a cached half-repair.
            self.store.save_changes(Some(&session), &repaired)
                .context("Could not save session recovery. Check disk space and permissions, then reopen the session to retry")?;
            repaired
        } else {
            session
        };
        Ok(self.make_conversation(session, true))
    }
    pub fn catalog_revision(&self) -> u64 {
        self.catalog_revision.load(Ordering::Acquire)
    }
    pub fn summaries(&self) -> Vec<SessionSummary> {
        let updates: Vec<_> = self
            .all()
            .into_iter()
            .map(|c| {
                let summary = SessionSummary::from(&*c.state.lock().expect("session lock"));
                (c, summary)
            })
            .collect();
        let mut summaries = self.summaries.lock().expect("summary lock");
        for (conversation, summary) in updates {
            // Deletion marks the conversation before removing its cached summary.
            // Check under this lock so a refresh collected before deletion cannot
            // reinsert a stale sidebar row after the delete commits.
            if !conversation.deleted.load(Ordering::Acquire) {
                summaries.insert(summary.id.clone(), summary);
            }
        }
        summaries.values().cloned().collect()
    }
    #[cfg(test)]
    pub fn evict_inactive(&self, selected: Option<&str>) -> Result<()> {
        for c in self.all() {
            if Some(c.id.as_str()) != selected {
                self.evict_idle(&c.id)?;
            }
        }
        Ok(())
    }
    pub fn evict_idle(&self, id: &str) -> Result<()> {
        let (_library, family) = self.admit(id);
        let _coordinator = family.lock().expect("family lifecycle");
        let body = self.load_gate(id);
        let _loading = body.lock().expect("loading lock");
        let Some(c) = self.get(id) else { return Ok(()) };
        if !c.idle_body()
            || c.code.is_executing()
            || c.questionnaires.as_ref().is_some_and(|q| q.has_pending())
            || self.active_children(id) > 0
        {
            return Ok(());
        }
        // The coordinator/loading guards exclude new starts and eviction.
        // Skip running/finalizing work before waiting on its storage lock,
        // otherwise a slow save would delay Stop and new turns.
        let mut saved = c.persisted.lock().expect("persistence lock");
        c.save_locked(&mut saved)?;
        let mut state = c.state.lock().expect("session lock");
        self.summaries
            .lock()
            .expect("summary lock")
            .insert(c.id.clone(), SessionSummary::from(&*state));
        // Keep runtime IDs, unread results and JS store values, not the full transcript.
        state.points = Default::default();
        state.entries = Default::default();
        state.input = Default::default();
        c.loaded.store(false, Ordering::Release);
        drop(state);
        // Free the last saved history outside the lock used by UI summary reads.
        *saved = None;
        if !c.code.has_work() {
            self.conversations
                .lock()
                .expect("conversations lock")
                .remove(&c.id);
        }
        Ok(())
    }
    pub fn flush(&self) -> Result<()> {
        let _library = self.library.write().expect("library lock");
        let results = self.flush_results();
        for c in self.all() {
            c.save()?;
        }
        results
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
    pub fn reconcile_model(&self, id: &str, settings: &Settings) -> Result<bool> {
        let body = self.load_gate(id);
        let loading = body.lock().expect("loading lock");
        let conversation = self.load_locked(id)?;
        if crate::models::model(&conversation.state.lock().expect("session lock").model).is_some() {
            return Ok(false);
        }
        let mut saved = conversation.persisted.lock().expect("persistence lock");
        drop(loading);
        let mut session = conversation.snapshot();
        if !session.reconcile_model(settings) {
            return Ok(false);
        }
        self.store.save_changes(saved.as_ref(), &session)?;
        conversation
            .state
            .lock()
            .expect("session lock")
            .select_model(&session.model, settings);
        *saved = Some(session);
        conversation.changed();
        Ok(true)
    }
    pub fn edit(&self, id: &str, edit: impl FnOnce(&mut Session)) -> Result<()> {
        let body = self.load_gate(id);
        let loading = body.lock().expect("loading lock");
        let conversation = self.load_locked(id)?;
        let mut saved = conversation.persisted.lock().expect("persistence lock");
        drop(loading);
        let metadata_changed = {
            let mut state = conversation.state.lock().expect("session lock");
            let before = SessionSummary::from(&*state);
            edit(&mut state);
            if state.title != before.title {
                state.title_manually_set = true;
            }
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
    /// Serialize filing with submissions, turn starts, child starts and eviction.
    pub fn file_session(&self, id: &str, action: FilingAction) -> Result<FilingOutcome> {
        let (_library, family) = self.admit(id);
        let mut coordinator = family.lock().expect("family lifecycle");
        let conversation = self.load(id)?;
        let state = conversation.snapshot();
        if state.child.is_some() {
            bail!("Only main sessions can be filed away");
        }
        let destination = match action {
            FilingAction::Undo => None,
            FilingAction::File(kind) | FilingAction::StopCommandsAndFile(kind) => {
                let children = self.subagent_runs(id);
                if conversation.is_turn_active()
                    || conversation.code.cells_executing()
                    || conversation
                        .questionnaires
                        .as_ref()
                        .is_some_and(|q| q.has_pending())
                    || children.iter().any(|r| {
                        r.status == subagents::RunStatus::Running
                            || self.get(&r.session_id).is_some_and(|c| c.is_executing())
                            || (r.status.needs_attention() && !r.attention_acknowledged)
                    })
                    || state.interrupted
                    || state.needs_attention
                    || state.unchecked_completion
                    || conversation.status() == ConversationStatus::NeedsAttention
                {
                    bail!(
                        "This session is no longer idle. Open it and finish or review its work before filing it away."
                    );
                }
                let shell = conversation.code.shell();
                if shell.has_running_commands() {
                    if action == FilingAction::File(kind) {
                        return Ok(FilingOutcome::RunningCommands(kind));
                    }
                    // Signal under the start guard, then release it before waiting for
                    // exit/pipe EOF. Recheck all guards before filing.
                    let stopped = shell.stop_running_commands();
                    drop(coordinator);
                    drop(_library);
                    shell.runtime().block_on(stopped).context(match kind {
                        FilingKind::Deferred => {
                            "Session not deferred. Check its activity and retry Defer."
                        }
                        FilingKind::Settled => {
                            "Session not settled. Check its activity and retry Settle."
                        }
                    })?;
                    return self.file_session(id, FilingAction::File(kind));
                }
                // Finish outstanding durable delivery before filing. Stopped turns may
                // retain queued input/notices; filing preserves them without resuming.
                if self.run_index().runs(id).any(|r| !r.notified) {
                    self.retry_results(&mut coordinator, id)
                        .and_then(|()| self.deliver_notices(&mut coordinator, id))
                        .context("Could not finish saving child results before filing the session. Keep Orbit open, check disk space and permissions, then retry")?;
                }
                Some(kind)
            }
        };
        self.edit(id, |s| {
            if s.filing.map(|f| f.kind) != destination {
                s.filing = destination.map(|kind| Filing { kind, at: crate::store::now() });
            }
        }).context("Could not save the filing change. It is retained in memory for the next save attempt; check available disk space and permissions, then retry")?;
        Ok(FilingOutcome::Applied)
    }
    /// Forking publishes a new, idle main conversation only after the complete
    /// independently restorable session has committed.
    pub fn fork(&self, source: &str, entry: &str, settings: &Settings) -> Result<String> {
        let (_library, family) = self.admit(source);
        let _coordinator = family.lock().expect("family lifecycle");
        let session = self.load(source)?.snapshot().fork(entry, settings)?;
        let id = session.id.clone();
        let body = self.load_gate(&id);
        let _publishing = body.lock().expect("loading lock");
        self.store.save_session(&session)?;
        self.make_conversation(session, true);
        Ok(id)
    }
    pub fn send_new(
        self: &Arc<Self>,
        mut session: Session,
        submission: &str,
        mut message: crate::store::UserMessage,
        settings: Settings,
    ) -> Result<()> {
        if message.is_empty() {
            bail!("Type a message or attach an image first");
        }
        crate::send_trace::phase("lifecycle lock");
        let (library, family) = self.admit(&session.id);
        let mut coordinator = family.lock().expect("family lifecycle");
        let body = self.load_gate(&session.id);
        let publishing = body.lock().expect("loading lock");
        if self.store.contains_session(&session.id)? {
            drop(publishing);
            drop(coordinator);
            drop(library);
            return self.send(&session.id, submission, message, settings);
        }
        crate::send_trace::phase("prepare submission");
        message.files = crate::file_tags::capture(
            &session.project,
            &self.store.root().join("files"),
            &message.text,
        )?;
        message.skills =
            crate::skills::capture(&session.project, self.home.as_deref(), &message.text)?;
        let naming_message = session.needs_title().then(|| message.clone());
        session.checkpoint();
        session.accept_submission(submission.to_owned(), message, false);
        self.store.commit_new_session(&session)?;
        crate::send_trace::phase("publish acceptance");
        let conversation = self.make_conversation(session, true);
        drop(publishing);
        if let Some(message) = naming_message {
            self.name_session(&conversation, message);
        }
        coordinator.parent_stopped = false;
        self.start_inner(conversation, settings)
    }
    pub fn send(
        self: &Arc<Self>,
        id: &str,
        submission: &str,
        mut message: crate::store::UserMessage,
        settings: Settings,
    ) -> Result<()> {
        if message.is_empty() {
            bail!("Type a message or attach an image first");
        }
        crate::send_trace::phase("lifecycle lock");
        let (_library, family) = self.admit(id);
        let mut coordinator = family.lock().expect("family lifecycle");
        crate::send_trace::phase("session load");
        let conversation = self.load(id)?;
        crate::send_trace::phase("prepare submission");
        if conversation.snapshot().child.is_some() {
            bail!("Continue child sessions through spawn_subagent in their parent");
        }
        let snapshot = conversation.snapshot();
        if snapshot.entries.by_id(submission).is_none() {
            message.skills =
                crate::skills::capture(&snapshot.project, self.home.as_deref(), &message.text)?;
            message.files = crate::file_tags::capture_reusing(
                &snapshot.draft_files,
                &snapshot.project,
                &self.store.root().join("files"),
                &message.text,
            )?;
        }
        anyhow::ensure!(
            !conversation.deleted.load(Ordering::Acquire),
            "Session deleted before file attachments could be accepted"
        );
        anyhow::ensure!(
            !conversation.is_manually_compacting(),
            "Wait for compaction to finish before sending a message"
        );
        crate::send_trace::phase("pending child notices");
        self.retry_results(&mut coordinator, id)?;
        self.deliver_notices(&mut coordinator, id)?;
        let needs_start = {
            crate::send_trace::phase("persistence lock");
            let mut saved = conversation.persisted.lock().expect("persistence lock");
            crate::send_trace::phase("prepare checkpoint");
            let idle_boundary = !conversation.is_running();
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
                // Capture intent before starting a new turn: ordinary sends also
                // briefly occupy the pending queue while the agent is running.
                let steering = conversation.is_running()
                    && !conversation
                        .cancel
                        .lock()
                        .expect("cancel lock")
                        .is_cancelled();
                let naming_message = candidate.needs_title().then(|| message.clone());
                if idle_boundary && candidate.in_progress_response.is_none() {
                    candidate.checkpoint();
                }
                let points = candidate.points.clone();
                let accepted_at = crate::store::now();
                candidate.accept_submission_at(
                    submission.to_owned(),
                    message.clone(),
                    steering,
                    accepted_at,
                );
                self.store.save_changes(saved.as_ref(), &candidate)?;
                crate::send_trace::phase("publish acceptance");
                *saved = Some(candidate);
                let mut state = conversation.state.lock().expect("session lock");
                state.points = points;
                state.accept_submission_at(submission.to_owned(), message, steering, accepted_at);
                if let Some(message) = naming_message {
                    self.name_session(&conversation, message);
                }
                conversation.submissions.fetch_add(1, Ordering::Release);
                self.catalog_revision.fetch_add(1, Ordering::Release);
                conversation.changed();
                true
            }
        };
        if needs_start {
            coordinator.parent_stopped = false;
            self.start_inner(conversation, settings)?;
        }
        Ok(())
    }
    pub fn start(self: &Arc<Self>, id: &str, settings: Settings) -> Result<()> {
        crate::send_trace::phase("lifecycle lock");
        let (_library, family) = self.admit(id);
        let mut coordinator = family.lock().expect("family lifecycle");
        let conversation = self.load(id)?;
        anyhow::ensure!(
            conversation
                .state
                .lock()
                .expect("session lock")
                .child
                .is_none(),
            "Start child assignments through their parent"
        );
        self.start_inner(conversation, settings)?;
        coordinator.parent_stopped = false;
        Ok(())
    }
    fn start_inner(
        self: &Arc<Self>,
        conversation: Arc<Conversation>,
        settings: Settings,
    ) -> Result<()> {
        crate::send_trace::phase("turn start");
        anyhow::ensure!(
            !conversation.is_manually_compacting(),
            "Wait for compaction to finish before starting a turn"
        );
        self.start_turn(conversation, settings);
        Ok(())
    }
    fn start_turn(self: &Arc<Self>, conversation: Arc<Conversation>, settings: Settings) {
        *conversation.last_settings.lock().expect("settings lock") = settings.clone();
        let cancel = {
            let mut state = conversation.state.lock().expect("session lock");
            if conversation.is_running() {
                // A stopping tool may itself need family admission to settle.
                // Record a restart instead of waiting here with admission held.
                if conversation
                    .cancel
                    .lock()
                    .expect("cancel lock")
                    .is_cancelled()
                {
                    conversation
                        .restart_after_stop
                        .store(true, Ordering::Release);
                }
                return;
            }
            conversation
                .restart_after_stop
                .store(false, Ordering::Release);
            let cancel = CancellationToken::new();
            *conversation.cancel.lock().expect("cancel lock") = cancel.clone();
            *conversation.turn_usage.lock().expect("turn usage lock") = TurnUsage::default();
            conversation.set_status(ConversationStatus::Connecting);
            // cancel() takes the same state lock: it cannot cancel an obsolete
            // token between publishing a running turn and installing its token.
            conversation.turn_generation.fetch_add(1, Ordering::Release);
            state.unchecked_completion = false;
            // All starts, including automatic child-completion wakeups, return
            // the conversation to the main list before publishing active work.
            state.filing = None;
            conversation.running.store(true, Ordering::Release);
            conversation.notifications.clear_terminal(&conversation.id);
            cancel
        };
        conversation.changed();
        self.catalog_revision.fetch_add(1, Ordering::Release);
        let is_child = conversation.snapshot().child.is_some();
        let generation = conversation.turn_generation.load(Ordering::Acquire);
        let harness = self.clone();
        self.runtime.as_ref().expect("live runtime").spawn(async move {
            let mut recovery = execution::Recovery::default();
            loop {
                let submission = conversation.submissions.load(Ordering::Acquire);
                let result = harness.run(&conversation, &settings, &cancel, &mut recovery).await;
                if result.is_err() || cancel.is_cancelled() {
                    harness.provider.discard_continuation(&conversation.id);
                    recovery.stopped(&conversation, cancel.is_cancelled());
                }
                {
                    let mut state = conversation.state.lock().expect("session lock");
                    if result.is_ok()
                        && (!state.pending.is_empty() || !state.pending_tool_outputs.is_empty() || !state.subagent_notices.is_empty())
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
                let save_result = conversation.save_async().await;
                if let Err(error) = &save_result {
                    let mut state = conversation.state.lock().expect("session lock");
                    state.needs_attention = !cancel.is_cancelled();
                    state.entries.push(Entry::new(
                        EntryKind::Notice,
                        format!("Could not save session: {error:#}. Keep Orbit open, free disk space or fix permissions, then retry saving."),
                    ));
                }
                conversation.set_status(if cancel.is_cancelled() {
                    ConversationStatus::Stopped
                } else if result.is_ok() && save_result.is_ok() {
                    ConversationStatus::Completed
                } else {
                    ConversationStatus::NeedsAttention
                });
                {
                    // Submission/start uses the same state lock. No accepted work can
                    // slip between this final check and the transition to idle.
                    let state = conversation.state.lock().expect("session lock");
                    let submitted = conversation.submissions.load(Ordering::Acquire) != submission;
                    let pending = !state.pending.is_empty() || !state.pending_tool_outputs.is_empty() || !state.subagent_notices.is_empty();
                    if !cancel.is_cancelled() && pending && (submitted || result.is_ok()) {
                        continue;
                    }
                    conversation.finishing.store(true, Ordering::Release);
                    conversation.running.store(false, Ordering::Release);
                }
                if !is_child {
                    let (_library, family) = harness.admit(&conversation.id);
                    let _coordinator = family.lock().expect("family lifecycle");
                    let mut state = conversation.state.lock().expect("session lock");
                    if conversation.turn_generation.load(Ordering::Acquire) == generation {
                        if !cancel.is_cancelled() {
                            if result.is_err() || save_result.is_err() {
                                conversation.notifications.publish(&state, notifications::Kind::Failed);
                            } else if !conversation.questionnaires.as_ref().is_some_and(|q| q.has_pending())
                                && harness.active_children(&conversation.id) == 0
                            {
                                // This sidebar hint uses ordinary session saves (eviction,
                                // edits, shutdown), not a separate completion transaction.
                                state.unchecked_completion = true;
                                state.updated = crate::store::now();
                                conversation.notifications.publish(&state, notifications::Kind::Finished);
                            }
                        }
                        // A newer turn may already be finishing. An old task must
                        // not clear its eviction/deletion guard after releasing this lock.
                        conversation.finishing.store(false, Ordering::Release);
                        drop(state);
                        if conversation.restart_after_stop.swap(false, Ordering::AcqRel) {
                            let settings = conversation.last_settings.lock().expect("settings lock").clone();
                            harness.start_turn(conversation.clone(), settings);
                        }
                    }
                }
                if is_child {
                let finish_harness = harness.clone();
                let child = conversation.clone();
                let cancelled = cancel.is_cancelled();
                if let Err(error) = tokio::task::spawn_blocking(move || finish_harness.finish_assignment(&child, result, cancelled)).await.expect("assignment completion worker") {
                    conversation.state.lock().expect("session lock").entries.push(Entry::new(EntryKind::Notice, format!("Could not save/deliver subagent completion: {error:#}. Result retained in memory; fix storage and retry saving before quitting.")));
                    conversation.set_status(ConversationStatus::NeedsAttention);
                }
                conversation.finishing.store(false, Ordering::Release);
                }
                harness.catalog_revision.fetch_add(1, Ordering::Release);
                conversation.changed();
                break;
            }
        });
    }
    /// Compact the current context without accepting a submission or starting an agent turn.
    pub fn compact(self: &Arc<Self>, id: &str, settings: Settings) -> Result<()> {
        let (_library, family) = self.admit(id);
        let _coordinator = family.lock().expect("family lifecycle");
        let conversation = self.load(id)?;
        anyhow::ensure!(
            !conversation.is_executing() && self.active_children(id) == 0,
            "Wait for session activity to finish before compacting"
        );
        let cancel = CancellationToken::new();
        {
            let state = conversation.state.lock().expect("session lock");
            anyhow::ensure!(state.child.is_none(), "Child sessions are read-only");
            anyhow::ensure!(
                !state.input.is_empty(),
                "There is no conversation context to compact yet"
            );
            anyhow::ensure!(
                state.pending.is_empty() && state.subagent_notices.is_empty(),
                "Resume the pending work before compacting"
            );
            *conversation.cancel.lock().expect("cancel lock") = cancel.clone();
            conversation.turn_generation.fetch_add(1, Ordering::Release);
            conversation
                .manual_compaction
                .store(true, Ordering::Release);
            conversation.set_status(ConversationStatus::CompactingContext);
        }
        self.catalog_revision.fetch_add(1, Ordering::Release);
        conversation.changed();
        let harness = self.clone();
        let runtime = self.runtime.as_ref().expect("live runtime");
        runtime.spawn(async move {
            let result = async {
                let instructions = harness.prepare_context(&conversation, &cancel).await?;
                let snapshot = conversation.snapshot();
                harness.compact_context(
                    &conversation,
                    &snapshot,
                    &instructions,
                    settings.context_limit,
                    &cancel,
                ).await
            }.await;
            let mut status = ConversationStatus::Ready;
            if let Err(error) = result {
                status = if cancel.is_cancelled() {
                    ConversationStatus::Stopped
                } else {
                    ConversationStatus::NeedsAttention
                };
                {
                    let mut state = conversation.state.lock().expect("session lock");
                    state.needs_attention |= !cancel.is_cancelled();
                    state.entries.push(Entry::new(
                        EntryKind::Notice,
                        format!("Could not finish manual compaction: {error:#}. The transcript is preserved."),
                    ));
                }
                if let Err(error) = conversation.save_async().await {
                    conversation.state.lock().expect("session lock").entries.push(Entry::new(
                        EntryKind::Notice,
                        format!("Could not save session: {error:#}. Keep Orbit open, fix storage, and retry saving."),
                    ));
                    status = ConversationStatus::NeedsAttention;
                }
            }
            {
                let _state = conversation.state.lock().expect("session lock");
                conversation.set_status(status);
                conversation.manual_compaction.store(false, Ordering::Release);
            }
            harness.catalog_revision.fetch_add(1, Ordering::Release);
            conversation.changed();
        });
        Ok(())
    }

    async fn prepare_context(
        &self,
        conversation: &Arc<Conversation>,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let credentials = self.auth.token(None).await?;
        let account = credentials.account_id.or(credentials.email);
        {
            let mut s = conversation.state.lock().expect("session lock");
            if s.context_account != account && s.context_account.is_some() {
                s.consume_pending();
                s.input = plaintext_context(&s).into();
                s.effort_baseline = None;
                s.usage.input = 0;
                s.entries.push(Entry::new(EntryKind::Notice, "ChatGPT account changed. Rebuilt context from the preserved transcript; encrypted provider context was not sent to the new account."));
            }
            s.context_account = account;
            conversation.changed();
        }
        conversation.save_async().await?;
        let (cwd, profile) = {
            let session = conversation.state.lock().expect("session lock");
            (
                session.project.clone(),
                session.child.as_ref().map(|child| child.profile),
            )
        };
        let project_guidance = crate::project_guidance::read(&cwd)?;
        let skill_dir = cwd.clone();
        let home = self.home.clone();
        let catalog = tokio::select! {
            _ = cancel.cancelled() => bail!("Turn cancelled"),
            result = tokio::task::spawn_blocking(move || crate::skills::discover_with_home(&skill_dir, home.as_deref())) => {
                result.context("Skill discovery worker")??.developer_text()
            }
        };
        {
            let mut s = conversation.state.lock().expect("session lock");
            s.prepare_effort();
            crate::project_guidance::Fragment::Skills.apply(&mut s.input, catalog.as_deref());
            crate::project_guidance::Fragment::Project
                .apply(&mut s.input, project_guidance.as_deref());
        }
        let mut instructions = format!(
            "You are a Codex, an agent based on GPT. You and the user share a workspace: {}. Return a concise explanation of changes and tests. Use subagents when necessary.\n",
            cwd.display()
        );
        instructions.push_str(profile.map_or(
            crate::questionnaire::INSTRUCTIONS,
            subagents::Profile::instructions,
        ));
        Ok(instructions)
    }

    async fn run(
        &self,
        conversation: &Arc<Conversation>,
        settings: &Settings,
        cancel: &CancellationToken,
        recovery: &mut execution::Recovery,
    ) -> Result<String> {
        {
            let mut s = conversation.state.lock().expect("session lock");
            repair_calls(&mut s);
            s.interrupted = true;
            s.needs_attention = false;
            conversation.changed();
        }
        conversation.save_async().await?;
        let instructions = self.prepare_context(conversation, cancel).await?;
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
            // Serialized provider context includes opaque encrypted payloads, whose byte
            // size does not represent model tokens. Use the provider's reported usage.
            if snapshot.usage.input
                > settings.context_limit * u64::from(settings.compact_percent) / 100
                && compacted_size != Some(snapshot.input.len())
            {
                conversation.set_status(ConversationStatus::CompactingContext);
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
            conversation.set_status(ConversationStatus::Thinking);
            conversation.changed();
            let result = self
                .respond_and_execute(conversation, &snapshot, &instructions, cancel, recovery)
                .await;
            let response = match result {
                Ok(response) => {
                    recovery.resumed(conversation);
                    response
                }
                Err(error)
                    if error.is::<crate::provider::StreamInterrupted>()
                        && !cancel.is_cancelled() =>
                {
                    {
                        let mut session = conversation.state.lock().expect("session lock");
                        commit_progress(&mut session);
                        conversation.changed();
                    }
                    recovery.retry(conversation, cancel).await?;
                    continue;
                }
                Err(error)
                    if error.is::<crate::provider::ContextLimit>()
                        && conversation
                            .state
                            .lock()
                            .expect("session lock")
                            .in_progress_response
                            .as_ref()
                            .is_some_and(|response| response.items.is_empty())
                        && compacted_size != Some(snapshot.input.len()) =>
                {
                    conversation
                        .state
                        .lock()
                        .expect("session lock")
                        .in_progress_response = None;
                    conversation.set_status(ConversationStatus::RecoveringContext);
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
            let final_text = subagents::final_response(&response.output);
            let has_calls = response.output.iter().any(is_tool_call);
            {
                let mut s = conversation.state.lock().expect("session lock");
                s.input.extend(response.output);
                let progress = s.in_progress_response.take().expect("active response");
                s.input
                    .extend(progress.results.into_iter().map(|(_, value)| value));
                conversation.changed();
            }
            conversation.save_async().await?;
            // A steering message accepted during streaming/tool execution is consumed at this boundary.
            let state = conversation.state.lock().expect("session lock");
            if !has_calls
                && state.pending.is_empty()
                && state.pending_tool_outputs.is_empty()
                && state.subagent_notices.is_empty()
            {
                return Ok(final_text);
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
        let manual = conversation.is_manually_compacting();
        let input = self
            .provider
            .compact(snapshot, instructions, limit, cancel, &|response| {
                let mut session = conversation.state.lock().expect("session lock");
                session.usage.total_input += response.usage.input;
                session.usage.total_cached += response.usage.cached;
                session.usage.total_output += response.usage.output;
                if !manual {
                    conversation
                        .turn_usage
                        .lock()
                        .expect("turn usage lock")
                        .record_response(response);
                }
                conversation.changed();
            })
            .await?;
        let size = {
            let mut session = conversation.state.lock().expect("session lock");
            anyhow::ensure!(!cancel.is_cancelled(), "Compaction cancelled");
            session.input = input.input.into();
            if let Some(update) = snapshot.latest_effort_update() {
                session.input.push(update.clone());
            }
            // Both native compaction and fallback summaries start a new context window.
            for fragment in [
                crate::project_guidance::Fragment::Skills,
                crate::project_guidance::Fragment::Project,
            ] {
                if let Some(item) = fragment.latest(&snapshot.input) {
                    session.input.push(item.clone());
                }
            }
            // A new submission may trigger compaction before any model response.
            // A summary of a skill mention is not its selected instructions.
            let tail = snapshot
                .input
                .iter()
                .rev()
                .take_while(|item| item["role"] == "user")
                .count();
            let pending = snapshot
                .input
                .range(snapshot.input.len() - tail)
                .skip_while(|item| {
                    !item["content"]
                        .as_array()
                        .is_some_and(|parts| parts.iter().any(|p| p["type"] == "orbit_skill"))
                });
            session.input.extend(pending.cloned());
            if let Some(summary) = input.summary {
                session.summary = summary;
                session.summarized_entries = input.summarized_entries;
            }
            session.usage.input = 0;
            session.entries.push(Entry::new(
                EntryKind::Notice,
                if manual {
                    "Context compacted. The complete transcript is preserved."
                } else {
                    "Context compacted automatically. The complete transcript is preserved."
                },
            ));
            session.input.len()
        };
        conversation.save_async().await?;
        conversation.changed();
        Ok(size)
    }
    pub fn stop_all(&self) {
        let _library = self.library.write().expect("library lock");
        // A failed shutdown can be resumed per parent. An explicit send to B
        // must not allow a late child completion to restart stopped parent A.
        let families: Vec<_> = {
            let mut registry = self.families.lock().expect("family registry");
            registry.stopped_by_default = true;
            registry.gates.values().cloned().collect()
        };
        for family in families {
            family.lock().expect("family lifecycle").parent_stopped = true;
        }
        for conversation in self.all() {
            conversation.cancel();
        }
    }
}
/// Only cold loads need restart recovery. An evicted runtime stub is still live.
fn recover_after_restart(before: &Session) -> Option<Session> {
    use crate::shell::CommandStatus;
    let mut repaired = before.clone();
    let mut commands = false;
    let mut abandoned = false;
    for (index, entry) in before.entries.iter().enumerate() {
        let EntryKind::Tool {
            name,
            complete,
            command_status,
            ..
        } = &entry.kind
        else {
            continue;
        };
        let running = *command_status == Some(CommandStatus::Running);
        let unfinished = !complete && matches!(name.as_str(), "ask_user" | "web__run");
        if !running && !unfinished {
            continue;
        }
        let EntryKind::Tool {
            command_status,
            complete,
            failed,
            output,
            ..
        } = &mut repaired.entries[index].kind
        else {
            unreachable!("same entry as the restart checkpoint")
        };
        if running {
            *command_status = Some(CommandStatus::Interrupted);
            commands = true;
        }
        if unfinished {
            *complete = true;
            *failed = true;
            *output = if name == "web__run" {
                "Web request interrupted by application restart. Remote work may have completed and consumed usage. It was not automatically replayed."
            } else {
                "Questionnaire cancelled by application restart. Unsubmitted answers were not saved; ask again if still needed."
            }.into();
            abandoned = true;
        }
    }
    if commands {
        repaired.entries.push(Entry::new(EntryKind::Notice,
            "Execution tracking was interrupted by the restart. Inspect the workspace before retrying; tools may have performed side effects."));
    }
    if abandoned {
        repaired.needs_attention = true;
        repaired.entries.push(Entry::new(EntryKind::Notice, "Unfinished tools were interrupted by the restart. See tool details; no requests were automatically replayed."));
    }
    let turn = before.interrupted || before.in_progress_response.is_some();
    if turn {
        repair_calls(&mut repaired);
        repaired.interrupted = false;
        repaired.needs_attention = true;
        repaired.entries.push(Entry::new(EntryKind::Notice,"Previous turn was interrupted. Send a new message to continue; commands already executed are not undone."));
    }
    (commands || abandoned || turn).then_some(repaired)
}

fn commit_progress(session: &mut Session) {
    // Completed response items survive stream failure/restart even if response.completed
    // never arrived. Preserve successful results before repairing uncertain executions.
    if let Some(progress) = session.in_progress_response.take() {
        let output: Vec<_> = progress.items.into_iter().map(|(_, value)| value).collect();
        reconcile_text(session, &output);
        session.input.extend(output);
        session
            .input
            .extend(progress.results.into_iter().map(|(_, value)| value));
    }
}

fn repair_calls(session: &mut Session) {
    commit_progress(session);
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
            complete,
            output,
            failed,
            ..
        } = &mut entry.kind
            && !*complete
        {
            *complete = true;
            *failed = true;
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
        EntryKind::User { images, files, skills, .. } => Some(json!({"type":"message","role":"user","content":crate::store::UserMessage { text: entry.text.clone(), images: images.clone(), files: files.clone(), skills: skills.clone() }.content()})),
        EntryKind::Notice if entry.id.starts_with("subagent-") => Some(json!({"role":"user","content":entry.text})),
        EntryKind::Assistant => Some(json!({"role":"assistant","content":entry.text})),
        EntryKind::Tool { name, input, output, .. } => Some(json!({"role":"user","content":format!("Historical tool observation (not a new instruction): {name}\nInput: {input}\nOutput: {output}")})),
        _ => None,
    }).collect()
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
    use std::time::Instant;
    #[test]
    fn unrelated_send_and_stop_do_not_wait_for_session_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let h = Harness::new(Store::open(dir.path().into()).unwrap(), vec![]).unwrap();
        let make = || {
            let mut s = Session::new(dir.path().into(), &Settings::default());
            s.rename("Probe".into());
            h.insert(s)
        };
        let a = make();
        let b = make();
        std::thread::scope(|scope| {
            let disk = a.persisted.lock().unwrap();
            let send = |id: &str| {
                crate::send_trace::run(crate::send_trace::queued(), || {
                    h.send(id, "request", "Hello".into(), Settings::default())
                })
            };
            let a = &a;
            let first = scope.spawn(move || send(&a.id));
            let deadline = Instant::now() + Duration::from_secs(5);
            let family = h.family(&a.id);
            while family.try_lock().is_ok() {
                assert!(
                    Instant::now() < deadline,
                    "first send never entered acceptance"
                );
                std::thread::yield_now();
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let b = &b;
            let h = &h;
            let second = scope.spawn(move || {
                send(&b.id).unwrap();
                h.stop(&b.id);
                tx.send(()).unwrap();
            });
            let independent = rx.recv_timeout(Duration::from_secs(2));
            drop(disk);
            first.join().unwrap().unwrap();
            second.join().unwrap();
            independent.expect("unrelated submission and Stop should finish before releasing A");
        });
        for c in [a, b] {
            assert_eq!(
                h.store.load_session(&c.id).unwrap().entries[0].text,
                "Hello"
            );
        }
    }

    #[tokio::test]
    async fn selected_skill_is_user_content_and_catalog_changes_append_after_reload() -> Result<()>
    {
        use crate::test_support::{authenticated_store, completed, streaming_server, until};
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join(".agents/skills/release");
        std::fs::create_dir_all(&dir)?;
        let skill_file = dir.join("SKILL.md");
        std::fs::write(
            &skill_file,
            "---\nname: release\ndescription: Prepare a release\n---\nFirst workflow revision",
        )?;
        let mention = crate::skills::discover_with_home(tmp.path(), None)?.skills[0].mention();
        let store = authenticated_store(tmp.path().join("state"));
        let (url, mut requests, server) = streaming_server().await;
        let h = Harness::with_provider(store.clone(), vec![], Provider::fixture(store, url))?;
        let settings = Settings::default();
        let mut session = Session::new(tmp.path().into(), &settings);
        session.rename("Release fixture".into());
        let id = session.id.clone();
        h.send_new(
            session,
            "first",
            format!("Use {mention}").into(),
            settings.clone(),
        )?;
        let req = tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await?
            .unwrap();
        let prefix = req.body["input"].as_array().unwrap().clone();
        assert_eq!(prefix[0]["role"], "developer");
        assert!(!prefix[0].to_string().contains("Prepare a release"));
        assert_eq!(prefix[1]["role"], "developer");
        assert!(
            prefix[1]["content"]
                .as_str()
                .unwrap()
                .contains("Prepare a release")
        );
        assert!(!prefix[1].to_string().contains("First workflow revision"));
        assert_eq!(prefix[2]["role"], "user");
        assert!(
            prefix[2]["content"][1]["text"]
                .as_str()
                .unwrap()
                .contains("First workflow revision")
        );
        req.reply.send(completed(json!([]))).unwrap();
        until(|| !h.get(&id).unwrap().is_turn_active()).await;
        h.evict_inactive(None)?;
        std::fs::write(
            &skill_file,
            "---\nname: release\ndescription: Updated release checklist\n---\nNew workflow body",
        )?;
        h.send(&id, "second", "Continue".into(), settings)?;
        let req = tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await?
            .unwrap();
        let input = req.body["input"].as_array().unwrap();
        assert!(input.starts_with(&prefix));
        let suffix = &input[prefix.len()..];
        assert_eq!(suffix[0]["role"], "developer");
        assert!(
            suffix[0]["content"]
                .as_str()
                .unwrap()
                .contains("Updated release checklist")
        );
        assert_eq!(suffix[1]["content"], "Continue");
        assert!(!req.body.to_string().contains("New workflow body"));
        req.reply.send(completed(json!([]))).unwrap();
        until(|| !h.get(&id).unwrap().is_turn_active()).await;
        h.edit(&id, |s| s.usage.input = Settings::default().context_limit)?;
        h.send(
            &id,
            "third",
            format!("Now use {mention}").into(),
            Settings::default(),
        )?;
        let req = tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await?
            .unwrap();
        assert_eq!(
            req.body["input"].as_array().unwrap().last().unwrap()["type"],
            "compaction_trigger"
        );
        req.reply
            .send(completed(
                json!([{"type":"compaction","encrypted_content":"opaque"}]),
            ))
            .unwrap();
        let req = tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await?
            .unwrap();
        let fresh = req.body["input"].as_array().unwrap();
        assert!(fresh.iter().any(|item| item["role"] == "developer"
            && item.to_string().contains("Updated release checklist")));
        let latest = fresh.last().unwrap();
        assert_eq!(latest["role"], "user");
        assert_eq!(latest["content"][1]["type"], "input_text");
        assert!(
            latest["content"][1]["text"]
                .as_str()
                .unwrap()
                .contains("New workflow body")
        );
        req.reply.send(completed(json!([]))).unwrap();
        until(|| !h.get(&id).unwrap().is_turn_active()).await;
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn project_guidance_changes_append_and_compaction_keeps_only_the_current_version() {
        use crate::test_support::{authenticated_store, completed, streaming_server, until};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        let store = authenticated_store(dir.path().join("state"));
        let (url, mut requests, server) = streaming_server().await;
        let h =
            Harness::with_provider(store.clone(), vec![], Provider::fixture(store, url)).unwrap();
        let settings = Settings::default();
        let mut session = Session::new(dir.path().into(), &settings);
        session.rename("Guidance fixture".into());
        let id = session.id.clone();
        h.insert(session);

        let mut previous = Vec::new();
        for (step, (file, expected_update)) in [
            (Some("Use sapphire."), Some("Use sapphire.")),
            (Some("Use sapphire."), None),
            (Some("Use amber."), Some("Use amber.")),
            (None, Some("no longer apply")),
            (None, None),
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(text) = file {
                std::fs::write(&path, text).unwrap();
            } else if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
            if step == 2 {
                h.edit(&id, |s| s.usage.input = settings.context_limit)
                    .unwrap();
            }
            let submission = format!("turn-{step}");
            h.send(
                &id,
                &submission,
                submission.clone().into(),
                settings.clone(),
            )
            .unwrap();
            let request = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                .await
                .unwrap()
                .unwrap();
            let input = request.body["input"].as_array().unwrap();
            assert_eq!(input[0]["role"], "developer");
            assert!(!input[0].to_string().contains("Use sapphire."));
            assert!(input.starts_with(&previous));
            let start = previous.len().max(1);
            let mut suffix = &input[start..];
            if let Some(update) = expected_update {
                assert_eq!(suffix[0]["role"], "user");
                assert!(suffix[0]["content"].as_str().unwrap().contains(update));
                suffix = &suffix[1..];
            }
            assert_eq!(suffix[0]["content"], submission);
            if step == 2 {
                assert_eq!(suffix.len(), 2);
                assert_eq!(suffix[1]["type"], "compaction_trigger");
                let compact = json!({"type":"compaction", "encrypted_content":"opaque"});
                request.reply.send(completed(json!([compact]))).unwrap();
                let next = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                    .await
                    .unwrap()
                    .unwrap();
                previous = next.body["input"].as_array().unwrap().clone();
                assert_eq!(previous.len(), 3);
                assert_eq!(previous[0], input[0]);
                assert_eq!(previous[1], compact);
                assert_eq!(previous[2], input[start]);
                next.reply.send(completed(json!([]))).unwrap();
            } else {
                assert_eq!(suffix.len(), 1);
                previous = input.clone();
                request.reply.send(completed(json!([]))).unwrap();
            }
            until(|| !h.get(&id).unwrap().is_turn_active()).await;
            // Exercise the durable context rather than an in-memory "already sent" flag.
            h.evict_inactive(None).unwrap();
        }
        server.abort();
    }

    #[tokio::test]
    async fn effort_changes_chain_and_survive_replay_and_compaction() -> Result<()> {
        use crate::test_support::{authenticated_store, completed, streaming_server, until};
        let dir = tempfile::tempdir()?;
        let store = authenticated_store(dir.path().join("state"));
        let (url, mut requests, server) = streaming_server().await;
        let h = Harness::with_provider(store.clone(), vec![], Provider::fixture(store, url))?;
        let settings = Settings::default();
        let mut s = Session::new(dir.path().into(), &settings);
        s.rename("Effort fixture".into());
        s.model = crate::models::LUNA.id.into();
        s.effort = "low".into();
        let id = h.insert(s).id.clone();
        let update = json!({"type":"configuration_update","reasoning":{"effort":"none"}});
        let compact = json!({"type":"compaction","encrypted_content":"opaque"});
        for turn in 0..4 {
            let msg = format!("turn {turn}");
            if turn == 1 {
                h.edit(&id, |s| s.effort = "none".into())?;
            } else if turn == 2 {
                h.evict_inactive(None)?;
                h.edit(&id, |s| s.usage.input = settings.context_limit)?;
            } else if turn == 3 {
                h.edit(&id, |s| {
                    s.model = crate::models::SOL.id.into();
                    s.effort = "high".into();
                })?;
            }
            h.send(&id, &msg, msg.clone().into(), settings.clone())?;
            let req = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                .await?
                .unwrap();
            let input = req.body["input"].as_array().unwrap();
            assert_eq!(
                req.body["reasoning"]["effort"],
                if turn == 3 { "high" } else { "low" }
            );
            match turn {
                0 => {
                    assert_eq!(input.len(), 2);
                    assert_eq!(input[1]["content"], msg);
                    req.reply.send(json!({"type":"response.completed","response":{"status":"completed","id":"first","output":[]}}).to_string())?;
                }
                1 => {
                    assert_eq!(req.body["previous_response_id"], "first");
                    assert_eq!(
                        input,
                        &[
                            update.clone(),
                            json!({"type":"message","role":"user","content":msg})
                        ]
                    );
                    req.reply.send(completed(json!([])))?;
                }
                2 => {
                    assert!(req.body.get("previous_response_id").is_none());
                    assert_eq!(input.len(), 6);
                    assert_eq!(input[1]["content"], "turn 0");
                    assert_eq!(input[2], update);
                    assert_eq!(input[3]["content"], "turn 1");
                    assert_eq!(input[4]["content"], msg);
                    assert_eq!(input[5]["type"], "compaction_trigger");
                    req.reply.send(completed(json!([compact])))?;
                    let next = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                        .await?
                        .unwrap();
                    assert_eq!(next.body["reasoning"]["effort"], "low");
                    assert_eq!(next.body["input"], json!([input[0], compact, update]));
                    next.reply.send(completed(json!([])))?;
                }
                _ => {
                    // Luna's "none" cannot be sent to Sol, but its conversation is retained.
                    assert_eq!(
                        req.body["input"],
                        json!([input[0],compact,{"type":"message","role":"user","content":msg}])
                    );
                    req.reply.send(completed(json!([])))?;
                }
            }
            until(|| !h.get(&id).unwrap().is_turn_active()).await;
        }
        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn filing_persists_retries_and_only_accepted_messages_restore() {
        for kind in [FilingKind::Settled, FilingKind::Deferred] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path().into()).unwrap();
            let h = Harness::new(store.clone(), vec![]).unwrap();
            let mut session = Session::new(dir.path().into(), &Settings::default());
            session.updated = 77;
            session.draft = "Unsent draft λ".into();
            let c = h.insert(session);
            c.save().unwrap();
            let revision = h.catalog_revision();
            assert_eq!(
                h.file_session(&c.id, FilingAction::File(kind)).unwrap(),
                FilingOutcome::Applied
            );
            let filed = c.snapshot().filing.unwrap();
            assert_eq!(filed.kind, kind);
            assert!(h.catalog_revision() > revision);
            assert_eq!(store.load().unwrap().1[0].filing, Some(filed));
            assert_eq!(store.load_session(&c.id).unwrap().updated, 77);
            h.edit(&c.id, |s| s.filing = Some(Filing { kind, at: 123 }))
                .unwrap();
            assert_eq!(
                h.file_session(&c.id, FilingAction::File(kind)).unwrap(),
                FilingOutcome::Applied
            );
            h.evict_inactive(None).unwrap();
            let c = h.load(&c.id).unwrap();
            assert_eq!(c.snapshot().filing, Some(Filing { kind, at: 123 }));
            assert_eq!(c.snapshot().draft, "Unsent draft λ");

            assert_eq!(
                h.file_session(&c.id, FilingAction::Undo).unwrap(),
                FilingOutcome::Applied
            );
            assert_eq!(store.load().unwrap().1[0].filing, None);
            assert_eq!(c.snapshot().updated, 77);
            assert_eq!(
                h.file_session(&c.id, FilingAction::File(kind)).unwrap(),
                FilingOutcome::Applied
            );
            assert!(c.snapshot().filing.unwrap().at >= filed.at);

            let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
            db.execute_batch("CREATE TRIGGER fail_settlement BEFORE UPDATE ON sessions BEGIN SELECT RAISE(FAIL,'disk fixture'); END;").unwrap();
            assert!(h.file_session(&c.id, FilingAction::Undo).is_err());
            assert!(
                c.snapshot().filing.is_none(),
                "retain the edit for save retry"
            );
            assert!(store.load_session(&c.id).unwrap().filing.is_some());
            db.execute_batch("DROP TRIGGER fail_settlement;").unwrap();
            h.flush().unwrap();
            assert!(store.load_session(&c.id).unwrap().filing.is_none());
            assert_eq!(
                h.file_session(&c.id, FilingAction::File(kind)).unwrap(),
                FilingOutcome::Applied
            );

            assert!(
                h.send(&c.id, "empty", "".into(), Settings::default())
                    .is_err()
            );
            assert!(c.snapshot().filing.is_some());
            db.execute_batch("CREATE TRIGGER fail_send BEFORE UPDATE ON sessions BEGIN SELECT RAISE(FAIL,'disk fixture'); END;").unwrap();
            assert!(
                h.send(&c.id, "retry", "Continue".into(), Settings::default())
                    .is_err()
            );
            assert!(
                c.snapshot().filing.is_some(),
                "failed acceptance must not unsettle"
            );
            db.execute_batch("DROP TRIGGER fail_send;").unwrap();
            // No credentials: acceptance is local, then the provider fails offline.
            h.send(&c.id, "retry", "Continue".into(), Settings::default())
                .unwrap();
            assert!(c.snapshot().filing.is_none());
            assert!(store.load_session(&c.id).unwrap().filing.is_none());
            let mut changes = h.subscribe();
            while c.is_executing() {
                tokio::time::timeout(Duration::from_secs(3), changes.changed())
                    .await
                    .unwrap()
                    .unwrap();
            }
        }
    }

    #[tokio::test]
    async fn settlement_rejects_unread_attention_work_questions_and_children() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let h = Harness::new(store, vec![]).unwrap();
        let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
        for flag in 0..5 {
            h.edit(&c.id, |s| {
                s.unchecked_completion = flag == 0;
                s.needs_attention = flag == 1;
                s.interrupted = flag == 2;
            })
            .unwrap();
            c.running.store(flag == 3, Ordering::Release);
            c.finishing.store(flag == 4, Ordering::Release);
            assert!(
                h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
                    .is_err(),
                "blocked state {flag}"
            );
            assert!(c.snapshot().filing.is_none());
        }
        c.finishing.store(false, Ordering::Release);
        let q = c.questionnaires.as_ref().unwrap().clone();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let mut changes = h.subscribe();
        let pending = tokio::spawn(async move {
            q.ask(
                "settle-question",
                r#"{"questions":[{"id":"q","kind":"text","question":"Continue?"}]}"#,
                &token,
            )
            .await
        });
        while !c.questionnaires.as_ref().unwrap().has_pending() {
            tokio::time::timeout(Duration::from_secs(3), changes.changed())
                .await
                .unwrap()
                .unwrap();
        }
        assert!(
            h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
                .is_err()
        );
        cancel.cancel();
        assert!(pending.await.unwrap().is_err());

        let mut child = Session::new(dir.path().into(), &Settings::default());
        child.child = Some(subagents::Child {
            parent_id: c.id.clone(),
            profile: subagents::Profile::Explore,
        });
        let child = h.insert(child);
        let mut run = subagents::RunInfo {
            configuration: None,
            run_id: "child-run".into(),
            session_id: child.id.clone(),
            parent_id: c.id.clone(),
            request_id: "request".into(),
            profile: subagents::Profile::Explore,
            title: "Inspect".into(),
            status: subagents::RunStatus::Running,
            assignment: 1,
            started: 1,
            finished: None,
            notified: true,
            attention_acknowledged: false,
        };
        h.run_index().record(run.clone());
        assert!(
            h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
                .is_err()
        );
        run.status = subagents::RunStatus::Failed;
        h.run_index().record(run.clone());
        assert!(
            h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
                .is_err()
        );
        run.attention_acknowledged = true;
        h.run_index().record(run);
        child.finishing.store(true, Ordering::Release);
        assert!(
            h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
                .is_err(),
            "terminal assignment still finalizing"
        );
        child.finishing.store(false, Ordering::Release);
        assert_eq!(
            h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
                .unwrap(),
            FilingOutcome::Applied
        );
        assert!(c.snapshot().filing.is_some());
        assert!(
            h.file_session(&child.id, FilingAction::File(FilingKind::Settled))
                .is_err()
        );
    }

    #[test]
    fn filing_confirms_only_lingering_commands_in_the_clicked_session() {
        for kind in [FilingKind::Settled, FilingKind::Deferred] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path().into()).unwrap();
            let h = Harness::new(store.clone(), vec![]).unwrap();
            let start_session = || {
                let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
                let output = c.code.exec(
                r#"text(await tools.exec_command({cmd:"sleep 60 & wait", login:false, yield_time_ms:0}));"#,
                "outer", &CancellationToken::new(),
            ).unwrap();
                assert!(output.contains("Process running with session ID"));
                c
            };
            let clicked = start_session();
            let other = start_session();
            assert_eq!(
                h.file_session(&clicked.id, FilingAction::File(kind))
                    .unwrap(),
                FilingOutcome::RunningCommands(kind)
            );
            assert!(clicked.snapshot().filing.is_none());
            assert!(clicked.code.shell().has_running_commands());

            // Confirmation cannot override activity that appeared with the dialog open.
            h.edit(&clicked.id, |s| s.unchecked_completion = true)
                .unwrap();
            assert!(
                h.file_session(&clicked.id, FilingAction::StopCommandsAndFile(kind))
                    .is_err()
            );
            assert!(clicked.code.shell().has_running_commands());
            h.edit(&clicked.id, |s| s.unchecked_completion = false)
                .unwrap();
            assert_eq!(
                h.file_session(&clicked.id, FilingAction::StopCommandsAndFile(kind))
                    .unwrap(),
                FilingOutcome::Applied
            );
            assert_eq!(
                store
                    .load_session(&clicked.id)
                    .unwrap()
                    .filing
                    .unwrap()
                    .kind,
                kind
            );
            assert!(!clicked.code.shell().has_running_commands());
            assert!(other.code.shell().has_running_commands());
            assert!(other.snapshot().filing.is_none());
        }
    }

    #[test]
    fn command_status_tracks_yielded_processes_without_polling() {
        use crate::shell::CommandStatus;
        for (tty, exit, stop) in [(false, 0, false), (true, 7, false), (false, 0, true)] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path().into()).unwrap();
            let h = Harness::new(store.clone(), vec![]).unwrap();
            let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
            let args = json!({
                "cmd": format!("while [ ! -f release ]; do sleep 0.01; done; printf finished; exit {exit}"),
                "login": false, "tty": tty, "yield_time_ms": 0
            });
            let result = c
                .code
                .exec(
                    &format!("text(await tools.exec_command({args}));"),
                    "outer",
                    &CancellationToken::new(),
                )
                .unwrap();
            assert!(
                result.contains("Process running with session ID"),
                "{result}"
            );
            let snapshot = c.snapshot();
            let entry = snapshot
                .entries
                .iter()
                .find(|e| matches!(&e.kind, EntryKind::Tool { name, .. } if name == "exec_command"))
                .unwrap();
            let entry_id = entry.id.clone();
            let EntryKind::Tool {
                complete,
                failed,
                output,
                command_status,
                ..
            } = &entry.kind
            else {
                unreachable!()
            };
            assert!(
                *complete && !failed,
                "invocation returned successfully while the process lives"
            );
            assert_eq!(*command_status, Some(CommandStatus::Running));
            let original_output = output.clone();
            h.evict_inactive(None).unwrap();
            assert!(
                c.loaded.load(Ordering::Acquire),
                "live commands must stay observable across session switches"
            );
            if stop {
                c.cancel();
            } else {
                std::fs::write(dir.path().join("release"), "").unwrap();
            }
            let expected = if stop || exit != 0 {
                CommandStatus::Failed
            } else {
                CommandStatus::Succeeded
            };
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let saved = store.load_session(&c.id).unwrap();
                if matches!(&saved.entries.by_id(&entry_id).unwrap().kind,
                    EntryKind::Tool { command_status: Some(status), .. } if *status == expected)
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "process status was not persisted automatically"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                matches!(&c.snapshot().entries.by_id(&entry_id).unwrap().kind,
                EntryKind::Tool { complete: true, failed: false, output, .. } if output == &original_output),
                "exit status must not rewrite the original tool result"
            );
        }
    }

    #[test]
    fn restart_marks_only_live_command_tracking_interrupted() {
        use crate::shell::CommandStatus;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        for (id, status) in [
            ("running", CommandStatus::Running),
            ("success", CommandStatus::Succeeded),
            ("failed", CommandStatus::Failed),
        ] {
            session.entries.push(Entry {
                id: id.into(),
                text: String::new(),
                kind: EntryKind::Tool {
                    name: "exec_command".into(),
                    input: "{}".into(),
                    output: "original result".into(),
                    complete: true,
                    failed: false,
                    command_status: Some(status),
                    cell_id: None,
                    parent: Some("outer".into()),
                    media: vec![],
                    web: None,
                },
            });
        }
        store.save_session(&session).unwrap();
        let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        h.load(&session.id).unwrap();
        let saved = store.load_session(&session.id).unwrap();
        for (id, expected) in [
            ("running", CommandStatus::Interrupted),
            ("success", CommandStatus::Succeeded),
            ("failed", CommandStatus::Failed),
        ] {
            assert!(matches!(&saved.entries.by_id(id).unwrap().kind,
                EntryKind::Tool { command_status: Some(status), output, .. } if *status == expected && output == "original result"));
        }
    }

    #[test]
    fn failed_restart_repair_retries_all_changes_before_publishing() {
        use crate::{shell::CommandStatus, store::InProgressResponse, test_support::sqlite};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let mut checkpoint = Session::new(dir.path().into(), &Settings::default());
        checkpoint.interrupted = true;
        checkpoint.in_progress_response = Some(InProgressResponse {
            items: [(
                0usize,
                json!({"type":"custom_tool_call","call_id":"outer","name":"exec","input":"work"}),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        });
        for (name, status) in [
            ("exec_command", Some(CommandStatus::Running)),
            ("web__run", None),
        ] {
            checkpoint.entries.push(Entry {
                id: name.into(),
                text: String::new(),
                kind: EntryKind::Tool {
                    name: name.into(),
                    input: "{}".into(),
                    output: String::new(),
                    complete: status.is_some(),
                    command_status: status,
                    failed: false,
                    cell_id: None,
                    parent: Some("outer".into()),
                    media: vec![],
                    web: None,
                },
            });
        }
        store.save_session(&checkpoint).unwrap();
        let id = &checkpoint.id;
        let harness = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        // Fail first at the original early write, then at the context write
        // after the entry and metadata changes. Neither may publish partial state.
        for table in ["sessions", "context"] {
            sqlite::fail_writes(&db, table, "fixture recovery failure");
            assert!(harness.load(id).is_err());
            assert!(harness.get(id).is_none());
            let unchanged = store.load_session(id).unwrap();
            assert!(unchanged.interrupted && unchanged.in_progress_response.is_some());
            assert!(unchanged.entries.iter().eq(checkpoint.entries.iter()));
            sqlite::allow_writes(&db, table);
        }
        let recovered = harness.load(id).unwrap().snapshot();
        assert!(recovered.needs_attention && !recovered.interrupted);
        assert!(recovered.in_progress_response.is_none());
        assert!(matches!(
            recovered.entries[0].kind,
            EntryKind::Tool {
                command_status: Some(CommandStatus::Interrupted),
                ..
            }
        ));
        assert!(
            matches!(&recovered.entries[1].kind, EntryKind::Tool { complete: true, failed: true, output, .. } if output.contains("not automatically replayed"))
        );
        assert_eq!(recovered.input.len(), 2);
        assert_eq!(recovered.input[1]["type"], "custom_tool_call_output");
        assert_eq!(recovered.input[1]["call_id"], "outer");
        drop(harness);
        let restarted = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        let reopened = restarted.load(id).unwrap().snapshot();
        assert!(
            reopened.entries.iter().eq(recovered.entries.iter()),
            "no duplicate recovery notices"
        );
        assert!(reopened.input.iter().eq(recovered.input.iter()));
    }

    #[test]
    fn restart_finishes_abandoned_web_calls_without_replay_or_metadata_loss() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        let metadata = crate::web_search::Metadata {
            mode: crate::web_search::Mode::Live,
            searched_at: 123,
            sources: vec![],
            details: "UI-only metadata".into(),
        };
        session.entries.push(Entry {
            id: "finished".into(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: "web__run".into(),
                input: "{}".into(),
                output: "completed text".into(),
                complete: true,
                failed: false,
                command_status: None,
                cell_id: None,
                parent: Some("outer".into()),
                media: vec![],
                web: Some(metadata.clone()),
            },
        });
        session.entries.push(Entry {
            id: "abandoned".into(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: "web__run".into(),
                input: "{}".into(),
                output: String::new(),
                complete: false,
                failed: false,
                command_status: None,
                cell_id: None,
                parent: Some("outer".into()),
                media: vec![],
                web: None,
            },
        });
        store.save_session(&session).unwrap();
        let harness = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        let conversation = harness.load(&id).unwrap();
        assert!(!conversation.is_executing());
        let saved = store.load_session(&id).unwrap();
        assert!(
            matches!(&saved.entries.by_id("finished").unwrap().kind,EntryKind::Tool{output,web:Some(actual),..} if output=="completed text" && actual==&metadata)
        );
        assert!(
            matches!(&saved.entries.by_id("abandoned").unwrap().kind,EntryKind::Tool{output,complete:true,failed:true,..} if output.contains("not automatically replayed"))
        );
        assert!(
            !serde_json::to_string(&plaintext_context(&saved))
                .unwrap()
                .contains("UI-only metadata")
        );
    }

    #[tokio::test]
    async fn questionnaire_blocks_model_continuation_until_all_answers_or_stop() {
        use crate::test_support::{completed, server_with};

        for (tool, stop) in [
            ("exec", false),
            ("wait", false),
            ("exec", true),
            ("wait", true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let store = crate::test_support::authenticated_store(dir.path().into());
            let source = r#"// @exec: {"yield_time_ms": 100}
text('Output before questionnaire');
await Promise.all(['first', 'second'].map(async id => {
    text(await tools.ask_user({questions:[{id, question:id + '?', kind:'text'}]}));
}));"#;
            let initial_cell = Arc::new(Mutex::new(None::<String>));
            let cell = initial_cell.clone();
            let (url, mut requests, server) = server_with((0..3).map(move |step| {
                let cell = cell.clone();
                move |request: &str| {
                    let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                    let output = match step {
                        0 if tool == "exec" => json!([{"type":"custom_tool_call","call_id":"ask","name":"exec","input":source}]),
                        0 => json!([{"type":"function_call","call_id":"ask","name":"wait","arguments":json!({"cell_id":cell.lock().unwrap().as_ref().unwrap(),"yield_time_ms":1}).to_string()}]),
                        1 => {
                            let result = body["input"].as_array().unwrap().iter().find(|v| v["call_id"] == "ask" && v.get("output").is_some()).unwrap();
                            let id = result["output"].as_str().unwrap().lines().next().unwrap().strip_prefix("Script running with cell ID ").unwrap();
                            json!([{"type":"function_call","call_id":"collect","name":"wait","arguments":json!({"cell_id":id}).to_string()}])
                        }
                        _ => {
                            let result = body["input"].as_array().unwrap().iter().find(|v| v["call_id"] == "collect" && v.get("output").is_some()).unwrap();
                            let output = result["output"].as_str().unwrap();
                            assert!(output.starts_with("Script completed"), "{output}");
                            assert!(output.contains("First answer λ") && output.contains("Second answer 界"), "{output}");
                            json!([{"type":"message","id":"done","phase":"final_answer","content":[{"type":"output_text","text":"Answers received"}]}])
                        }
                    };
                    (200, completed(output))
                }
            })).await;
            let h = Harness::with_provider(store.clone(), vec![], Provider::fixture(store, url))
                .unwrap();
            let mut session = Session::new(dir.path().into(), &Settings::default());
            session.rename("Questionnaire fixture".into());
            let c = h.insert(session);
            let questions = c.questionnaires.as_ref().unwrap();
            let mut changes = h.subscribe();
            if tool == "wait" {
                let worker = c.clone();
                let id = tokio::task::spawn_blocking(move || {
                    worker
                        .code
                        .exec_output(source, "initial", &CancellationToken::new())
                        .unwrap()
                        .cell_id
                        .unwrap()
                })
                .await
                .unwrap();
                *initial_cell.lock().unwrap() = Some(id);
            }
            h.send(
                &c.id,
                "user",
                "Ask before proceeding".into(),
                Settings::default(),
            )
            .unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while questions.pending().len() != 2 {
                    changes.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            // Both questions can be visible before the first exec/wait drains
            // its output. Answer only after that yield: the final collect below
            // deliberately expects *both* answers in the unread output.
            crate::test_support::until(|| {
                matches!(
                    c.snapshot().entries.by_id("ask").map(|entry| &entry.kind),
                    Some(EntryKind::Tool { complete: true, .. })
                )
            })
            .await;
            // The first request is the model asking. No second request may be made
            // even after the exec/wait yield deadline has elapsed.
            tokio::time::timeout(Duration::from_secs(3), requests.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(250), requests.recv())
                    .await
                    .is_err()
            );
            assert!(c.is_running());
            assert!(!c.snapshot().unchecked_completion);
            let pending = questions.pending();
            questions
                .submit(&pending[0].id, vec!["First answer λ".into()])
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(100), requests.recv())
                    .await
                    .is_err()
            );
            assert!(
                c.is_running(),
                "one answered question must not release the queue"
            );
            if stop {
                h.stop(&c.id);
            } else {
                questions
                    .submit(&pending[1].id, vec!["Second answer 界".into()])
                    .unwrap();
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                while c.is_executing() {
                    changes.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            assert!(!questions.has_pending());
            if stop {
                assert_eq!(c.status(), ConversationStatus::Stopped);
                assert!(!c.snapshot().unchecked_completion);
                if tool == "exec" {
                    assert!(
                        matches!(
                            &c.snapshot().entries.by_id("ask").unwrap().kind,
                            EntryKind::Tool { output, complete: true, .. }
                                if output.contains("Output before questionnaire")
                        ),
                        "Stop must preserve output collected before the questionnaire"
                    );
                }
                assert!(
                    requests.try_recv().is_err(),
                    "Stop must not resume the model"
                );
                server.abort();
            } else {
                server.await.unwrap();
                assert_eq!(c.status(), ConversationStatus::Completed);
                assert!(
                    c.snapshot()
                        .entries
                        .iter()
                        .any(|e| e.text == "Answers received")
                );
                assert_eq!(
                    c.snapshot()
                        .entries
                        .iter()
                        .filter(|e| matches!(e.kind, EntryKind::User { .. }))
                        .count(),
                    1
                );
            }
        }
    }

    #[test]
    fn questionnaire_persistence_failures_and_restart_do_not_lose_or_reopen_answers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        store.save_session(&session).unwrap();
        let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        let c = h.load(&id).unwrap();
        let q = c.questionnaires.as_ref().unwrap();
        let source = r#"// @exec: {"yield_time_ms": 1}
text(await tools.ask_user({questions:[{id:'choice',question:'Target?',kind:'text'}]}));"#;
        let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        crate::test_support::sqlite::fail_writes(&db, "sessions", "fixture disk failure");
        let result = c
            .code
            .exec(source, "failed-start", &CancellationToken::new())
            .unwrap();
        let cell = result
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ");
        let result = cell.map_or(result.clone(), |id| {
            c.code
                .wait(
                    &json!({"cell_id":id}).to_string(),
                    &CancellationToken::new(),
                )
                .unwrap()
        });
        assert!(result.contains("fixture disk failure"), "{result}");
        assert!(
            !q.has_pending(),
            "do not display a request before tool-start persistence succeeds"
        );
        crate::test_support::sqlite::allow_writes(&db, "sessions");
        let running = c
            .code
            .exec(source, "working-start", &CancellationToken::new())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !q.has_pending() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let pending = q.pending().pop().unwrap();
        h.edit(&id, |s| s.updated = 77).unwrap();
        let revision = h.catalog_revision();
        let answered_after = crate::store::now();
        assert!(q.submit(&pending.id, vec![]).is_err());
        assert_eq!(c.snapshot().updated, 77);
        h.evict_inactive(None).unwrap();
        assert!(h.get(&id).is_some());
        let stored = store.load_session(&id).unwrap();
        assert!(matches!(
            &stored.entries.by_id(&pending.id).unwrap().kind,
            EntryKind::Tool {
                complete: false,
                ..
            }
        ));
        crate::test_support::sqlite::fail_writes(&db, "sessions", "fixture result failure");
        q.submit(&pending.id, vec!["Retain this answer λ".into()])
            .unwrap();
        let cell = running
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        let output = c
            .code
            .wait(
                &json!({"cell_id":cell}).to_string(),
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(output.contains("fixture result failure"), "{output}");
        let snapshot = c.snapshot();
        assert!((answered_after..=crate::store::now()).contains(&snapshot.updated));
        assert!(h.catalog_revision() > revision);
        assert!(
            matches!(&snapshot.entries.by_id(&pending.id).unwrap().kind, EntryKind::Tool { output, complete:true, .. } if output.contains("Retain this answer λ"))
        );
        crate::test_support::sqlite::allow_writes(&db, "sessions");
        h.flush().unwrap();
        assert_eq!(store.load_session(&id).unwrap().updated, snapshot.updated);
        assert!(
            matches!(&store.load_session(&id).unwrap().entries.by_id(&pending.id).unwrap().kind, EntryKind::Tool { output, complete:true, .. } if output.contains("Retain this answer λ"))
        );
        // Simulate the persisted state of an abrupt exit after the model finished.
        let mut abandoned = Session::new(dir.path().into(), &Settings::default());
        abandoned.entries.push(Entry {
            id: "abandoned-ask".into(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: "ask_user".into(),
                input: "{}".into(),
                output: String::new(),
                complete: false,
                failed: false,
                command_status: None,
                cell_id: None,
                parent: Some("yielded-exec".into()),
                media: vec![],
                web: None,
            },
        });
        let abandoned_id = abandoned.id.clone();
        store.save_session(&abandoned).unwrap();
        let recovered = h.load(&abandoned_id).unwrap();
        assert!(!recovered.questionnaires.as_ref().unwrap().has_pending());
        assert!(
            matches!(&recovered.snapshot().entries.by_id("abandoned-ask").unwrap().kind, EntryKind::Tool { output, complete:true, .. } if output.contains("restart"))
        );
        assert!(store.load_session(&abandoned_id).unwrap().needs_attention);
    }
    #[test]
    fn turn_usage_sums_only_response_streaming_time_and_resets_on_start() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        let harness = Harness::new(store, vec![]).unwrap();
        harness.insert(session);
        let conversation = harness.get(&id).unwrap();
        {
            let mut usage = conversation.turn_usage.lock().unwrap();
            for (output, seconds) in [(17, 2), (43, 3)] {
                usage.record_response(&crate::provider::Response {
                    output: vec![],
                    usage: crate::store::Usage {
                        output,
                        ..Default::default()
                    },
                    first_output_latency: None,
                    streaming_time: Duration::from_secs(seconds),
                });
            }
            assert_eq!(usage.output, 60);
            assert_eq!(usage.streaming_time, Duration::from_secs(5));
        }
        harness.start(&id, Settings::default()).unwrap();
        let usage = conversation.turn_usage.lock().unwrap();
        assert_eq!(usage.output, 0);
        assert_eq!(usage.streaming_time, Duration::ZERO);
    }

    #[test]
    fn file_submissions_are_durable_idempotent_and_keep_failed_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let store = Store::open(dir.path().join("state")).unwrap();
        let mut session = Session::new(project.clone(), &Settings::default());
        let text = "@@ -1 +1 @@\nRead @missing @\"bad\\q\" and @source.rs";
        session.draft = text.into();
        std::fs::write(project.join("source.rs"), "captured λ").unwrap();
        // A real attachment still needs durable storage before accepting the draft.
        let files_dir = store.root().join("files");
        std::fs::write(&files_dir, "not a directory").unwrap();
        let id = session.id.clone();
        store.save_session(&session).unwrap();
        let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        assert!(
            h.send(
                &id,
                "failed",
                session.draft.clone().into(),
                Settings::default()
            )
            .is_err()
        );
        assert_eq!(h.get(&id).unwrap().snapshot().draft, text);
        assert!(store.load_session(&id).unwrap().entries.is_empty());
        std::fs::remove_file(files_dir).unwrap();
        h.send(&id, "files", text.into(), Settings::default())
            .unwrap();
        let c = h.get(&id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while c.is_running() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let captured = c.snapshot().pending[0].clone();
        assert_eq!(captured.files.len(), 1);
        assert_eq!(captured.files[0].inline.as_deref(), Some("captured λ"));
        std::fs::remove_file(project.join("source.rs")).unwrap();
        h.edit(&id, |s| s.consume_pending()).unwrap();
        h.send(&id, "files", text.into(), Settings::default())
            .unwrap();
        let saved = store.load_session(&id).unwrap();
        assert_eq!(saved.entries.iter().filter(|e| e.id == "files").count(), 1);
        assert_eq!(saved.entries.by_id("files").unwrap().text, text);
        let mut replay = serde_json::json!(plaintext_context(&saved));
        crate::media::project(&mut replay, false).unwrap();
        assert!(replay.to_string().contains("captured λ"));
        // First-send retries use the accepted session, not the original unsaved draft.
        let mut fresh = Session::new(project.clone(), &Settings::default());
        fresh.title = "Fixture".into();
        std::fs::write(project.join("fresh"), "first version").unwrap();
        h.send_new(fresh.clone(), "first", "@fresh".into(), Settings::default())
            .unwrap();
        let new = h.get(&fresh.id).unwrap();
        while new.is_running() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        h.edit(&fresh.id, |s| s.consume_pending()).unwrap();
        std::fs::remove_file(project.join("fresh")).unwrap();
        h.send_new(fresh.clone(), "first", "@fresh".into(), Settings::default())
            .unwrap();
        assert_eq!(
            store
                .load_session(&fresh.id)
                .unwrap()
                .entries
                .iter()
                .filter(|e| e.id == "first")
                .count(),
            1
        );
    }
    #[test]
    fn failed_acceptance_can_be_edited_and_restarted_without_queuing_old_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        store.save_session(&session).unwrap();
        let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        crate::test_support::sqlite::fail_writes(&db, "sessions", "fixture failure");
        assert!(
            h.send(&id, "old", "old text".into(), Settings::default())
                .is_err()
        );
        crate::test_support::sqlite::allow_writes(&db, "sessions");
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
        assert_eq!(c.snapshot().pending, vec!["corrected text".into()]);
        assert!(
            matches!(
                c.snapshot().entries.by_id("new").unwrap().kind,
                EntryKind::User {
                    steering: false,
                    ..
                }
            ),
            "a normal send stays unlabelled even while pending"
        );
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
        session.updated = 77;
        session
            .entries
            .push(Entry::new(EntryKind::Assistant, "original"));
        store.save_session(&session).unwrap();
        let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        let c = h.load(&id).unwrap();
        assert_eq!(c.snapshot().entries.len(), 2);
        let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        crate::test_support::sqlite::fail_writes(&db, "sessions", "fixture disk failure");
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
        assert_eq!(c.snapshot().updated, 77);
        assert!(
            c.snapshot().pending.is_empty(),
            "failed acceptance must not be published"
        );
        crate::test_support::sqlite::allow_writes(&db, "sessions");
        let sent_after = crate::store::now();
        h.send(
            &id,
            "submission-1",
            "one request".into(),
            Settings::default(),
        )
        .unwrap();
        assert!((sent_after..=crate::store::now()).contains(&c.snapshot().updated));
        assert_eq!(
            store.load_session(&id).unwrap().updated,
            c.snapshot().updated
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while c.is_running() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            c.snapshot()
                .entries
                .iter()
                .filter(|e| matches!(e.kind, EntryKind::User { .. }))
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
        session.entries.push(Entry::new(
            EntryKind::User {
                skills: vec![],
                files: vec![],
                images: vec![],
                sent_at: None,
                steering: false,
            },
            "preserved history",
        ));
        let c = h.insert(session);
        let result=c.code.exec("// @exec: {\"yield_time_ms\": 1}\nstore('key','retained');while (!load('release')) await new Promise(r=>setTimeout(r,1));text('unread result');","parent",&CancellationToken::new()).unwrap();
        let cell = result
            .lines()
            .find_map(|l| l.strip_prefix("Script running with cell ID "))
            .unwrap()
            .to_owned();
        c.code
            .exec(
                "store('release',true);",
                "release",
                &CancellationToken::new(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while c.code.is_executing() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        h.evict_inactive(None).unwrap();
        assert!(!c.loaded.load(Ordering::Acquire));
        assert!(c.snapshot().entries.is_empty());
        let loaded = h.load(&id).unwrap();
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
        assert_eq!(conversation.status(), ConversationStatus::NeedsAttention);
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
            harness
                .store
                .load_session(&id)
                .unwrap()
                .pending_tool_outputs[0]["output"],
            "after foreground turn"
        );
    }
    #[test]
    fn deleting_a_session_stops_its_yielded_tool_without_recreating_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state")).unwrap();
        let harness = Harness::new(store.clone(), vec![]).unwrap();
        let c = harness.insert(Session::new(dir.path().into(), &Settings::default()));
        // The marker is written by the nested command, not by the JS caller:
        // deletion must race with real work rather than an unstarted cell.
        c.code.exec(r#"// @exec: {"yield_time_ms": 1}
            text(await tools.exec_command({cmd:"touch started; exec sleep 60",login:false,yield_time_ms:60000}));"#,
            "background", &CancellationToken::new()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.path().join("started").exists() {
            assert!(Instant::now() < deadline, "command did not start");
            std::thread::sleep(Duration::from_millis(2));
        }
        harness.remove(&c.id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while c.is_executing() {
            assert!(Instant::now() < deadline, "deleted session still executing");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(store.load().unwrap().1.is_empty());
    }

    #[test]
    fn image_snapshots_persist_and_cleanup_failure_never_resurrects_deleted_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state")).unwrap();
        let session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        store.save_session(&session).unwrap();
        let h = Harness::new(store.clone(), vec![]).unwrap();
        let c = h.insert(session);
        let source = dir.path().join("image.png");
        std::fs::write(&source, crate::media::tests::png()).unwrap();
        let result = c
            .code
            .exec_output(
                "image(await tools.view_image({path:'image.png'}));",
                "view",
                &CancellationToken::new(),
            )
            .unwrap();
        let media = result.media().remove(0);
        let saved = store.load_session(&id).unwrap();
        assert!(saved.entries.iter().any(|e|matches!(&e.kind,EntryKind::Tool {media:m,complete:true,..} if m==&vec![media.clone()])));
        std::fs::remove_file(source).unwrap();
        assert_eq!(
            std::fs::read(&media.path).unwrap(),
            crate::media::tests::png()
        );
        // Force cleanup failure after the SQL transaction commits.
        let media_dir = store.root().join("media").join(&id);
        std::fs::remove_dir_all(&media_dir).unwrap();
        std::fs::write(&media_dir, b"not a directory").unwrap();
        let error = h.remove(&id).unwrap_err().to_string();
        assert!(error.contains("was deleted"), "{error}");
        assert!(h.get(&id).is_none());
        c.save().unwrap();
        assert!(store.load_session(&id).is_err());
        assert!(store.load().unwrap().1.is_empty());
        assert!(
            c.code
                .exec_output(
                    "image(await tools.view_image({path:'image.png'}));",
                    "late",
                    &CancellationToken::new()
                )
                .unwrap()
                .summary()
                .contains("Session deleted")
        );
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
            assert_eq!(snapshot.pending, vec!["Do not lose this request".into()]);
            assert_eq!(
                snapshot
                    .entries
                    .iter()
                    .filter(|e| matches!(e.kind, EntryKind::User { .. }))
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
            vec!["Do not lose this request".into()]
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

#[cfg(test)]
mod web_acceptance;
