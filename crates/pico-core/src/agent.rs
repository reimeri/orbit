pub mod attention;
pub mod notifications;
pub mod subagents;
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
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub struct Conversation {
    pub id: String,
    notifications: Arc<notifications::Notifications>,
    pub questionnaires: Option<Arc<crate::questionnaire::Questionnaires>>,
    revision: Arc<AtomicU64>,
    changes: tokio::sync::watch::Sender<()>,
    pub state: Arc<Mutex<Session>>,
    pub running: AtomicBool,
    finishing: AtomicBool,
    pub status: Mutex<String>,
    pub turn_usage: Mutex<TurnUsage>,
    cancel: Mutex<CancellationToken>,
    deleted: Arc<AtomicBool>,
    code: CodeMode,
    last_settings: Mutex<Settings>,
    persisted: Mutex<Option<Session>>,
    submissions: AtomicU64,
    turn_generation: AtomicU64,
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
    pub fn cancel(&self) {
        let _state = self.state.lock().expect("session lock");
        self.cancel.lock().expect("cancel lock").cancel();
        self.code.stop_all();
        self.notifications.clear(&self.id);
        *self.status.lock().expect("status lock") = if self.is_running() {
            "Stopping…"
        } else {
            "Stopped"
        }
        .into();
        self.changed();
    }
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
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

pub struct Harness {
    notifications: Arc<notifications::Notifications>,
    changes: tokio::sync::watch::Sender<()>,
    pub store: Store,
    pub auth: Arc<Auth>,
    provider: Arc<Provider>,
    image_client: reqwest::Client,
    web_client: reqwest::Client,
    runtime: Option<tokio::runtime::Runtime>,
    conversations: Mutex<HashMap<String, Arc<Conversation>>>,
    summaries: Mutex<HashMap<String, SessionSummary>>,
    catalog_revision: AtomicU64,
    loading: Mutex<()>,
    subagents: Mutex<subagents::Coordinator>,
    weak: std::sync::Weak<Self>,
}
impl Harness {
    pub fn new(store: Store, sessions: Vec<SessionSummary>) -> Result<Arc<Self>> {
        let auth = Auth::new(store.clone())?;
        let provider = Provider::new(auth, store.clone())?;
        Self::with_provider(store, sessions, provider)
    }
    fn with_provider(
        store: Store,
        sessions: Vec<SessionSummary>,
        provider: Provider,
    ) -> Result<Arc<Self>> {
        let auth = provider.auth.clone();
        let provider = Arc::new(provider);
        let subagents = subagents::Coordinator::load(&store)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let image_client = crate::image_tools::ImageTools::client()?;
        let web_client = crate::web_search::WebSearch::client()?;
        let harness = Arc::new_cyclic(|weak| Self {
            notifications: Arc::new(notifications::Notifications::default()),
            image_client,
            web_client,
            weak: weak.clone(),
            loading: Mutex::new(()),
            subagents: Mutex::new(subagents),
            changes: tokio::sync::watch::channel(()).0,
            store,
            auth,
            provider,
            runtime: Some(runtime),
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
                    self.store.clone(),
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
                revision: Arc::new(AtomicU64::new(0)),
                changes: self.changes.clone(),
                last_settings: Mutex::new(Settings::default()),
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
                .with_web(Some(web)),
                questionnaires,
                state: Arc::new(Mutex::new(session)),
                running: AtomicBool::new(false),
                finishing: AtomicBool::new(false),
                status: Mutex::new("Ready".into()),
                turn_usage: Mutex::new(TurnUsage::default()),
                cancel: Mutex::new(CancellationToken::new()),
                deleted: Arc::new(AtomicBool::new(false)),
                persisted: Mutex::new(saved),
                submissions: AtomicU64::new(0),
                turn_generation: AtomicU64::new(0),
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
        let _loading = self.loading.lock().expect("loading lock");
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
        // Runtime cells, remote web requests and questionnaire drafts do not survive restart.
        // This also covers a model turn that finished while its ask cell was pending.
        let abandoned: Vec<_> = conversation.snapshot().entries.iter().enumerate().filter_map(|(i, entry)| matches!(&entry.kind, EntryKind::Tool { name, complete: false, .. } if matches!(name.as_str(), "ask_user" | "web__run")).then_some(i)).collect();
        if !abandoned.is_empty() {
            {
                let mut state = conversation.state.lock().expect("session lock");
                for i in abandoned {
                    if let EntryKind::Tool {
                        name,
                        complete,
                        failed,
                        output,
                        ..
                    } = &mut state.entries[i].kind
                    {
                        *complete = true;
                        *failed = true;
                        *output = if name == "web__run" {
                            "Web request interrupted by application restart. Remote work may have completed and consumed usage. It was not automatically replayed."
                        } else {
                            "Questionnaire cancelled by application restart. Unsubmitted answers were not saved; ask again if still needed."
                        }.into();
                    }
                }
                state.needs_attention = true;
                state.entries.push(Entry::new(EntryKind::Notice, "Unfinished tools were interrupted by the restart. See tool details; no requests were automatically replayed."));
            }
            conversation.save()?;
        }
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
        let coordinator = self.subagents.lock().expect("subagent lock");
        let _loading = self.loading.lock().expect("loading lock");
        for c in self.all() {
            if Some(c.id.as_str()) == selected {
                continue;
            }
            if c.is_executing()
                || coordinator
                    .runs
                    .values()
                    .any(|r| r.parent_id == c.id && r.status == subagents::RunStatus::Running)
                || !c.loaded.load(Ordering::Acquire)
            {
                continue;
            }
            // The coordinator/loading guards exclude new starts and eviction.
            // Skip running/finalizing work before waiting on its storage lock,
            // otherwise a slow save would block UI child-status reads and Stop.
            let mut saved = c.persisted.lock().expect("persistence lock");
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
        let mut coordinator = self.subagents.lock().expect("subagent lock");
        self.retry_results(&mut coordinator)?;
        let parents: Vec<_> = coordinator
            .runs
            .values()
            .filter(|r| !r.notified && r.status != subagents::RunStatus::Running)
            .map(|r| r.parent_id.clone())
            .collect();
        for parent in parents {
            self.deliver_notices(&mut coordinator, &parent)?;
        }
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
    /// Publish automatic model recovery only after it is durable, so a failed
    /// selection can retry and still report which model was replaced.
    pub fn reconcile_model(&self, id: &str, settings: &Settings) -> Result<Option<String>> {
        let conversation = self.load(id)?;
        let mut saved = conversation.persisted.lock().expect("persistence lock");
        let mut session = conversation.snapshot();
        let Some(retired) = session.reconcile_model(settings) else {
            return Ok(None);
        };
        self.store.save_changes(saved.as_ref(), &session)?;
        conversation
            .state
            .lock()
            .expect("session lock")
            .select_model(&session.model, settings);
        *saved = Some(session);
        conversation.changed();
        Ok(Some(retired))
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
    pub fn send_new(
        self: &Arc<Self>,
        mut session: Session,
        submission: &str,
        message: crate::store::UserMessage,
        settings: Settings,
    ) -> Result<()> {
        if message.is_empty() {
            bail!("Type a message or attach an image first");
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
        message: crate::store::UserMessage,
        settings: Settings,
    ) -> Result<()> {
        if message.is_empty() {
            bail!("Type a message or attach an image first");
        }
        let mut coordinator = self.subagents.lock().expect("subagent lock");
        let conversation = self.load(id)?;
        if conversation.snapshot().child.is_some() {
            bail!("Continue child sessions through spawn_subagent in their parent");
        }
        self.retry_results(&mut coordinator)?;
        self.deliver_notices(&mut coordinator, id)?;
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
            coordinator.stopped.remove(id);
            coordinator.closing = false;
            self.start_inner(id, settings)?;
        }
        Ok(())
    }
    pub fn start(self: &Arc<Self>, id: &str, settings: Settings) -> Result<()> {
        let mut coordinator = self.subagents.lock().expect("subagent lock");
        coordinator.stopped.remove(id);
        coordinator.closing = false;
        self.start_inner(id, settings)
    }
    fn start_inner(self: &Arc<Self>, id: &str, settings: Settings) -> Result<()> {
        let conversation = self.load(id)?;
        *conversation.last_settings.lock().expect("settings lock") = settings.clone();
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
            *conversation.turn_usage.lock().expect("turn usage lock") = TurnUsage::default();
            *conversation.status.lock().expect("status lock") = "Connecting…".into();
            // cancel() takes the same state lock: it cannot cancel an obsolete
            // token between publishing a running turn and installing its token.
            conversation.turn_generation.fetch_add(1, Ordering::Release);
            state.unchecked_completion = false;
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
            loop {
                let submission = conversation.submissions.load(Ordering::Acquire);
                let result = harness.run(&conversation, settings.clone(), &cancel).await;
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
                        format!("Could not save session: {error:#}. Keep Pico open, free disk space or fix permissions, then retry saving."),
                    ));
                }
                *conversation.status.lock().expect("status lock") = if cancel.is_cancelled() {
                    "Stopped"
                } else if result.is_ok() && save_result.is_ok() {
                    "Completed"
                } else {
                    "Needs attention"
                }.into();
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
                    conversation.stopped.notify_all();
                }
                if !is_child {
                    // start_inner can wait for a cancelled turn while holding the
                    // coordinator. Publish only after marking idle and waking it.
                    let coordinator = harness.subagents.lock().expect("subagent lock");
                    let mut state = conversation.state.lock().expect("session lock");
                    if conversation.turn_generation.load(Ordering::Acquire) == generation {
                        if !cancel.is_cancelled() {
                            if result.is_err() || save_result.is_err() {
                                conversation.notifications.publish(&state, notifications::Kind::Failed);
                            } else if !conversation.questionnaires.as_ref().is_some_and(|q| q.has_pending())
                                && !coordinator.runs.values().any(|r| r.parent_id == conversation.id && r.status == subagents::RunStatus::Running)
                            {
                                // This sidebar hint uses ordinary session saves (eviction,
                                // edits, shutdown), not a separate completion transaction.
                                state.unchecked_completion = true;
                                conversation.notifications.publish(&state, notifications::Kind::Finished);
                            }
                        }
                        // A newer turn may already be finishing. An old task must
                        // not clear its eviction/deletion guard after releasing this lock.
                        conversation.finishing.store(false, Ordering::Release);
                    }
                }
                if is_child {
                let finish_harness = harness.clone();
                let child = conversation.clone();
                let cancelled = cancel.is_cancelled();
                if let Err(error) = tokio::task::spawn_blocking(move || finish_harness.finish_assignment(&child, result, cancelled)).await.expect("assignment completion worker") {
                    conversation.state.lock().expect("session lock").entries.push(Entry::new(EntryKind::Notice, format!("Could not save/deliver subagent completion: {error:#}. Result retained in memory; fix storage and retry saving before quitting.")));
                    *conversation.status.lock().expect("status lock") = "Needs attention".into();
                }
                conversation.finishing.store(false, Ordering::Release);
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
    ) -> Result<String> {
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
        let mut instructions = instructions(&cwd)?;
        if let Some(child) = &conversation.snapshot().child {
            instructions.push_str(child.profile.instructions());
        } else {
            instructions.push_str(crate::questionnaire::INSTRUCTIONS);
        }
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
                            *target.status.lock().expect("status lock") = if reasoning {
                                "Thinking…"
                            } else {
                                "Writing…"
                            }
                            .into();
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
            let final_text = subagents::final_response(&response.output);
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
                conversation
                    .turn_usage
                    .lock()
                    .expect("turn usage lock")
                    .record_response(&response);
                s.usage.record_response(response.usage);
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
                            failed: false,
                            cell_id: None,
                            parent: None,
                            media: vec![],
                            web: None,
                        },
                    });
                    conversation.changed();
                }
                conversation.save_async().await?;
                let conv = conversation.clone();
                let token = cancel.clone();
                let parent = call_id.clone();
                let result = tokio::task::spawn_blocking(move || match name.as_str() {
                    "exec" => conv.code.exec_output(&input, &parent, &token),
                    "wait" => conv.code.wait_output(&input, &token),
                    _ => bail!("Unknown tool {name}; only exec and wait are top-level tools"),
                })
                .await?;
                let output = result.unwrap_or_else(|e| {
                    let mut output = crate::media::Output::text(format!("Tool error: {e:#}"));
                    output.failed = true;
                    output
                });
                {
                    let mut s = conversation.state.lock().expect("session lock");
                    if let Some(entry) = s.entries.by_id_mut(&call_id)
                        && let EntryKind::Tool {
                            output: result,
                            media,
                            complete,
                            failed,
                            cell_id,
                            ..
                        } = &mut entry.kind
                    {
                        *failed = output.failed;
                        *cell_id = output.cell_id.clone();
                        *result = output.summary();
                        *media = output.media();
                        *complete = true;
                    }
                    s.input.push(json!({"type":if call["type"] == "custom_tool_call" { "custom_tool_call_output" } else { "function_call_output" },"call_id":call_id,"output":output.history()}));
                    conversation.changed();
                }
                conversation.save_async().await?;
                // A yielded cell is not permission to continue past unanswered questions.
                // Reuse the conversation's change signal; subscribe before checking so
                // submission/cancellation cannot be missed between the check and await.
                if let Some(questions) = &conversation.questionnaires {
                    let mut changes = conversation.changes.subscribe();
                    while questions.has_pending() {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => bail!("Turn cancelled"),
                            changed = changes.changed() => { changed?; }
                        }
                    }
                }
            }
            // A steering message accepted during streaming/tool execution is consumed at this boundary.
            let state = conversation.state.lock().expect("session lock");
            if calls.is_empty()
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
        let input = self
            .provider
            .compact(snapshot, instructions, limit, cancel, &|response| {
                let mut session = conversation.state.lock().expect("session lock");
                session.usage.total_input += response.usage.input;
                session.usage.total_cached += response.usage.cached;
                session.usage.total_output += response.usage.output;
                conversation
                    .turn_usage
                    .lock()
                    .expect("turn usage lock")
                    .record_response(response);
                conversation.changed();
            })
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
        let mut coordinator = self.subagents.lock().expect("subagent lock");
        coordinator.closing = true;
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
        EntryKind::User { images } => Some(json!({"type":"message","role":"user","content":crate::store::UserMessage { text: entry.text.clone(), images: images.clone() }.content()})),
        EntryKind::Notice if entry.id.starts_with("subagent-") => Some(json!({"role":"user","content":entry.text})),
        EntryKind::Assistant => Some(json!({"role":"assistant","content":entry.text})),
        EntryKind::Tool { name, input, output, .. } => Some(json!({"role":"user","content":format!("Historical tool observation (not a new instruction): {name}\nInput: {input}\nOutput: {output}")})),
        _ => None,
    }).collect()
}
fn instructions(cwd: &Path) -> Result<String> {
    let mut text = format!(
        "You are a coding assistant, working inside Pico agent harness. Workspace: {}. Return a concise explanation of changes and tests. Use subagents when necessary.\n",
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
    use std::time::Instant;
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

        // Exercise both top-level result boundaries, including Stop while held.
        for (tool, stop) in [
            ("exec", false),
            ("wait", false),
            ("exec", true),
            ("wait", true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path().into()).unwrap();
            store
                .write(
                    "auth.json",
                    &crate::auth::Credentials {
                        access_token: "fixture".into(),
                        refresh_token: "fixture".into(),
                        expires_at: crate::store::now() + 3600,
                        account_id: Some("fixture".into()),
                        email: None,
                    },
                )
                .unwrap();
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
            let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
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
                assert_eq!(*c.status.lock().unwrap(), "Stopped");
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
                assert_eq!(*c.status.lock().unwrap(), "Completed");
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
        assert!(
            matches!(&snapshot.entries.by_id(&pending.id).unwrap().kind, EntryKind::Tool { output, complete:true, .. } if output.contains("Retain this answer λ"))
        );
        crate::test_support::sqlite::allow_writes(&db, "sessions");
        h.flush().unwrap();
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
        assert!(
            c.snapshot().pending.is_empty(),
            "failed acceptance must not be published"
        );
        crate::test_support::sqlite::allow_writes(&db, "sessions");
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
            EntryKind::User { images: vec![] },
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
