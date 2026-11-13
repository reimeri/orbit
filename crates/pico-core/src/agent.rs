use crate::{
    auth::Auth,
    code_mode::{CodeMode, Event as CodeEvent},
    provider::{Delta, Provider},
    shell::Shell,
    store::{Entry, EntryKind, Session, Settings, Store},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
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
    pub fn cancel(&self) {
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
}
impl Harness {
    pub fn new(store: Store, sessions: Vec<Session>) -> Result<Arc<Self>> {
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
        });
        for session in sessions {
            harness.insert(session);
        }
        Ok(harness)
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.changes.subscribe()
    }
    pub fn insert(&self, session: Session) -> Arc<Conversation> {
        let state = Arc::new(Mutex::new(session.clone()));
        let revision = Arc::new(AtomicU64::new(0));
        let observer_revision = revision.clone();
        let changes = self.changes.clone();
        let observer_state = state.clone();
        let observer_store = self.store.clone();
        let deleted = Arc::new(AtomicBool::new(false));
        let observer_deleted = deleted.clone();
        // Background cells may emit after the foreground turn settles. Persist them under
        // the same state lock, while preventing late callbacks from recreating deleted files.
        let observer = Arc::new(move |event: CodeEvent| {
            let mut session = observer_state.lock().expect("session lock");
            if observer_deleted.load(Ordering::Relaxed) {
                return;
            }
            match event {
                CodeEvent::Notification { parent, text } => {
                    session.pending_tool_outputs.push(
                        json!({"type":"custom_tool_call_output","call_id":parent,"output":text}),
                    );
                    session.entries.push(Entry::new(
                        EntryKind::Notice,
                        format!("Cell notification: {text}"),
                    ));
                }
                CodeEvent::Tool(event) => {
                    if let Some(result) = event.output {
                        if let Some(entry) = session.entries.iter_mut().find(|e| e.id == event.id)
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
            if let Err(error) = observer_store.save_session(&session) {
                session.entries.push(Entry::new(EntryKind::Notice, format!("Could not save background tool state: {error:#}. Fix storage and save this session before quitting.")));
            }
            observer_revision.fetch_add(1, Ordering::Release);
            changes.send_replace(());
        });
        // Tool observers and the loop share the same authoritative state.
        let code = CodeMode::new(Arc::new(Shell::new(session.project.clone())), observer);
        let conversation = Arc::new(Conversation {
            id: session.id.clone(),
            revision,
            changes: self.changes.clone(),
            state,
            running: AtomicBool::new(false),
            status: Mutex::new("Ready".into()),
            started: Mutex::new(None),
            elapsed: Mutex::new(Duration::ZERO),
            output_baseline: AtomicU64::new(0),
            cancel: Mutex::new(CancellationToken::new()),
            deleted,
            code,
        });
        self.conversations
            .lock()
            .expect("conversations lock")
            .insert(conversation.snapshot().id, conversation.clone());
        self.changes.send_replace(());
        conversation
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
        let conversation = self.get(id).context("Session no longer exists")?;
        let mut state = conversation.state.lock().expect("session lock");
        let before = state.clone();
        edit(&mut state);
        if let Err(error) = self.store.save_session(&state) {
            *state = before;
            return Err(error);
        }
        conversation.changed();
        Ok(())
    }
    pub fn remove(&self, id: &str) -> Result<()> {
        let conversation = self.get(id).context("Session no longer exists")?;
        if conversation.is_running() {
            bail!("Stop the turn before deleting this session");
        }
        {
            let _state = conversation.state.lock().expect("session lock");
            self.store.delete_session(id)?;
            conversation.deleted.store(true, Ordering::Relaxed);
        }
        conversation.code.stop_all();
        self.conversations
            .lock()
            .expect("conversations lock")
            .remove(id);
        self.changes.send_replace(());
        Ok(())
    }
    pub fn send(self: &Arc<Self>, id: &str, message: String, settings: Settings) -> Result<()> {
        if message.trim().is_empty() {
            bail!("Type a message first");
        }
        self.edit(id, |s| s.accept(message))?;
        self.start(id, settings)
    }
    pub fn start(self: &Arc<Self>, id: &str, settings: Settings) -> Result<()> {
        let conversation = self.get(id).context("Session no longer exists")?;
        {
            let _state = conversation.state.lock().expect("session lock");
            if conversation.running.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
        }
        conversation.output_baseline.store(
            conversation.snapshot().usage.total_output,
            Ordering::Relaxed,
        );
        let cancel = CancellationToken::new();
        *conversation.cancel.lock().expect("cancel lock") = cancel.clone();
        *conversation.started.lock().expect("timer lock") = Some(Instant::now());
        *conversation.status.lock().expect("status lock") = "Connecting…".into();
        conversation.changed();
        let harness = self.clone();
        self.runtime.as_ref().expect("live runtime").spawn(async move {
            loop {
            let result = harness.run(&conversation, settings.clone(), &cancel).await;
            let mut state = conversation.state.lock().expect("session lock");
            if result.is_ok() && (!state.pending.is_empty() || !state.pending_tool_outputs.is_empty()) && !cancel.is_cancelled() { continue; }
            if let Err(error) = &result { state.entries.push(Entry::new(EntryKind::Notice, if cancel.is_cancelled() { "Turn stopped. Completed commands are not undone. Send a new message to continue.".into() } else { format!("{error:#}") })); repair_calls(&mut state); }
            state.interrupted = false;
            if let Err(error) = harness.store.save_session(&state) { state.entries.push(Entry::new(EntryKind::Notice, format!("Could not save session: {error:#}. Keep Pico open, free disk space or fix permissions, then retry saving."))); }
            *conversation.status.lock().expect("status lock") = if cancel.is_cancelled() { "Stopped" } else if result.is_ok() { "Completed" } else { "Needs attention" }.into();
            let elapsed = conversation.started.lock().expect("timer lock").take().expect("active timer").elapsed();
            *conversation.elapsed.lock().expect("elapsed lock") = elapsed;
            conversation.running.store(false, Ordering::Release);
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
            self.store.save_session(&s)?;
            conversation.changed();
        }
        let credentials = self.auth.token(None).await?;
        let account = credentials.account_id.or(credentials.email);
        {
            let mut s = conversation.state.lock().expect("session lock");
            if s.context_account != account && s.context_account.is_some() {
                s.consume_pending();
                s.input = plaintext_context(&s);
                s.usage.input = 0;
                s.entries.push(Entry::new(EntryKind::Notice, "ChatGPT account changed. Rebuilt context from the preserved transcript; encrypted provider context was not sent to the new account."));
            }
            s.context_account = account;
            self.store.save_session(&s)?;
            conversation.changed();
        }
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
                self.store.save_session(&s)?;
                conversation.changed();
                s.clone()
            };
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
                            if let Some(entry) = s.entries.iter_mut().find(|e| e.id == id) {
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
                self.store.save_session(&s)?;
                conversation.changed();
            }
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
                    self.store.save_session(&s)?;
                    conversation.changed();
                }
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
                let mut s = conversation.state.lock().expect("session lock");
                if let Some(entry) = s.entries.iter_mut().find(|e| e.id == call_id)
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
                self.store.save_session(&s)?;
                conversation.changed();
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
        conversation: &Conversation,
        snapshot: &Session,
        instructions: &str,
        limit: u64,
        cancel: &CancellationToken,
    ) -> Result<usize> {
        let input = self
            .provider
            .compact(snapshot, instructions, limit, cancel)
            .await?;
        let mut session = conversation.state.lock().expect("session lock");
        session.input = input;
        session.usage.input = 0;
        session.entries.push(Entry::new(
            EntryKind::Notice,
            "Context compacted automatically. The complete transcript is preserved.",
        ));
        self.store.save_session(&session)?;
        conversation.changed();
        Ok(session.input.len())
    }
    pub fn stop_all(&self) {
        for conversation in self.all() {
            conversation.cancel();
        }
    }
}
fn repair_calls(session: &mut Session) {
    let missing: Vec<_> = session.input.iter().filter(|v| matches!(v["type"].as_str(), Some("function_call" | "custom_tool_call"))).filter(|call| !session.input.iter().any(|v| matches!(v["type"].as_str(), Some("function_call_output" | "custom_tool_call_output")) && v["call_id"] == call["call_id"]) || session.entries.iter().any(|entry| entry.id == call["call_id"].as_str().unwrap_or("") && matches!(entry.kind, EntryKind::Tool { parent: None, complete: false, .. }))).map(|call| json!({"type":if call["type"] == "custom_tool_call" { "custom_tool_call_output" } else { "function_call_output" },"call_id":call["call_id"],"output":"Execution interrupted. It may have performed side effects. Inspect the workspace before deciding whether to retry; do not assume it did nothing. Runtime cell IDs are no longer valid after restart."})).collect();
    session.input.extend(missing);
    for entry in &mut session.entries {
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
        if let Some(entry) = session.entries.iter_mut().find(|e| e.id == id) {
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
    #[tokio::test]
    async fn revisions_notify_completion_and_background_changes_without_polling() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let session = Session::new(dir.path().into(), &Settings::default());
        let id = session.id.clone();
        let harness = Harness::new(store, vec![session]).unwrap();
        let conversation = harness.get(&id).unwrap();
        let mut changes = harness.subscribe();
        let initial = conversation.revision();
        harness
            .send(&id, "Offline request".into(), Settings::default())
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
        conversation
            .code
            .exec(
                "notify('after foreground turn');",
                "parent",
                &CancellationToken::new(),
            )
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
        let harness = Harness::new(store.clone(), vec![session]).unwrap();
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
            saved[0].pending_tool_outputs[0]["output"],
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
        let harness = Harness::new(store.clone(), vec![session]).unwrap();
        harness
            .send(&id, "Do not lose this request".into(), Settings::default())
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
        assert_eq!(sessions[0].pending, vec!["Do not lose this request"]);
    }
    #[test]
    fn repair_does_not_repeat_successful_calls_and_closes_orphans() {
        let mut s = Session::new(std::env::temp_dir(), &Settings::default());
        s.input = vec![
            json!({"type":"custom_tool_call","call_id":"done"}),
            json!({"type":"custom_tool_call_output","call_id":"done","output":"ok"}),
            json!({"type":"function_call","call_id":"interrupted"}),
        ];
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
