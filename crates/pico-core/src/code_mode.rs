//! Fresh QuickJS cells implementing Codex's public code-mode contract.
use crate::{
    output::{OutputBuffer, character_budget},
    shell::Shell,
    store::id,
};
use anyhow::{Context as _, Result, bail};
use rquickjs::{Context, Function, Module, Runtime, Value as JsValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const GRAMMAR: &str = "\nstart: pragma_source | plain_source\npragma_source: PRAGMA_LINE NEWLINE SOURCE\nplain_source: SOURCE\n\nPRAGMA_LINE: /[ \\t]*\\/\\/ @exec:[^\\r\\n]*/\nNEWLINE: /\\r?\\n/\nSOURCE: /[\\s\\S]+/\n";
pub fn tools() -> Value {
    json!([
        {"type":"custom","name":"exec","description":DESCRIPTION,"format":{"type":"grammar","syntax":"lark","definition":GRAMMAR}},
        {"type":"function","name":"wait","description":"Waits on a yielded exec cell and returns only new output or completion. Use only after exec returns Script running with cell ID. terminate:true stops that cell. yield_time_ms and max_tokens default to 10000.","strict":false,"parameters":{"type":"object","properties":{"cell_id":{"type":"string","description":"Identifier of the running exec cell."},"yield_time_ms":{"type":"number","description":"Wait before yielding more output. Defaults to 10000 ms."},"max_tokens":{"type":"number","description":"Output token budget for this wait call. Defaults to 10000 tokens."},"terminate":{"type":"boolean","description":"True stops the running exec cell; false or omitted waits for output."}},"required":["cell_id"],"additionalProperties":false}}
    ])
}
const DESCRIPTION: &str = r#"Run raw JavaScript to orchestrate tool calls in a fresh embedded isolate as an async module. No Node, filesystem, network or console. Variables do not survive cells; store/load do. Only explicitly emitted text reaches the model. Unawaited promises are discarded when the module finishes.
Optional first line: // @exec: {"yield_time_ms": 10000, "max_output_tokens": 10000}
Output budgets limit inline text only. Oversized output is saved completely under /tmp; results give its path and exact byte size instead of a preview. Use shell commands to read/search that file. Files remain after the cell/command exits.
exec yields early with a cell ID if still running. Use wait to retrieve subsequent output or terminate it.
Globals: text(value) emits text (JSON.stringify for nonstrings); exit() ends successfully; store(key,value) and load(key) share serializable state across cells in this conversation; notify(value) immediately injects an extra custom_tool_call_output for this exec call; yield_control() yields accumulated output immediately; setTimeout(callback, delayMs), clearTimeout(id); ALL_TOOLS lists nested tool metadata. Pending timers alone do not keep a cell alive.
Nested tools (return strings):
 tools.exec_command({cmd: string, workdir?: string, shell?: string, login?: boolean, tty?: boolean, yield_time_ms?: number, max_output_tokens?: number, timeout_ms?: number}): Run a command in the project (or workdir). Default login=true, tty=false, yield_time_ms=10000. Returns exit code or a running session_id. tty=true keeps interactive stdin open; tty=false closes stdin. Commands are unrestricted, not sandboxed.
 tools.write_stdin({session_id: number, chars?: string, yield_time_ms?: number, max_output_tokens?: number}): Send input to a PTY or poll using empty chars. Default yield_time_ms=250. Ctrl-C is '\u0003'. Returns only new output and exit/session status.
Example: text(await tools.exec_command({cmd:"pwd; ls -la", max_output_tokens:2000}));
Use Promise.all for independent parallel tool calls. Tools are only available through exec, not as top-level calls."#;

#[derive(Clone, Debug)]
pub struct ToolEvent {
    pub id: String,
    pub parent: String,
    pub name: String,
    pub input: String,
    pub output: Option<String>,
}
#[derive(Clone, Debug)]
pub enum Event {
    Tool(ToolEvent),
    Notification { parent: String, text: String },
}
pub type Delegation =
    Arc<dyn Fn(&str, &str, &str, &CancellationToken) -> Result<String> + Send + Sync>;
pub type Observer = Arc<dyn Fn(Event) -> Result<()> + Send + Sync>;
#[derive(Default)]
struct State {
    output: OutputBuffer,
    done: bool,
    error: Option<String>,
    yielded: bool,
}
struct Cell {
    state: Arc<(Mutex<State>, tokio::sync::Notify)>,
    cancel: CancellationToken,
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Pragma {
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wait {
    cell_id: String,
    yield_time_ms: Option<u64>,
    max_tokens: Option<usize>,
    terminate: Option<bool>,
}
#[derive(Default)]
struct Stored {
    values: HashMap<String, String>,
    bytes: usize,
}
pub struct CodeMode {
    questionnaires: Option<Arc<crate::questionnaire::Questionnaires>>,
    delegation: Option<Delegation>,
    shell: Arc<Shell>,
    cells: Mutex<HashMap<String, Arc<Cell>>>,
    stored: Arc<Mutex<Stored>>,
    observer: Observer,
    workers: Arc<AtomicUsize>,
    active_cells: Arc<AtomicUsize>,
}
impl CodeMode {
    pub fn new(shell: Arc<Shell>, observer: Observer) -> Self {
        Self::with_tools(shell, observer, None, None)
    }
    pub fn with_tools(
        shell: Arc<Shell>,
        observer: Observer,
        delegation: Option<Delegation>,
        questionnaires: Option<Arc<crate::questionnaire::Questionnaires>>,
    ) -> Self {
        Self {
            questionnaires,
            delegation,
            shell,
            cells: Mutex::new(HashMap::new()),
            stored: Arc::new(Mutex::new(Stored::default())),
            observer,
            workers: Arc::new(AtomicUsize::new(0)),
            active_cells: Arc::new(AtomicUsize::new(0)),
        }
    }
    pub fn exec(&self, source: &str, parent: &str, cancel: &CancellationToken) -> Result<String> {
        if cancel.is_cancelled() {
            bail!("Execution cancelled");
        }
        let (pragma, code) = parse(source)?;
        let mut cells = self.cells.lock().expect("cell lock");
        if cancel.is_cancelled() {
            bail!("Execution cancelled");
        }
        if cells.len() >= 32 {
            bail!("Too many open cells. Wait for or terminate existing cells.");
        }
        let cell_id = id();
        let cell = Arc::new(Cell {
            state: Arc::new((Mutex::new(State::default()), tokio::sync::Notify::new())),
            cancel: cancel.child_token(),
        });
        cells.insert(cell_id.clone(), cell.clone());
        drop(cells);
        let shell = (
            self.shell.clone(),
            self.delegation.clone(),
            self.questionnaires.clone(),
        );
        let stored = self.stored.clone();
        let observer = self.observer.clone();
        let parent = parent.to_owned();
        let worker = cell.clone();
        let workers = self.workers.clone();
        self.active_cells.fetch_add(1, Ordering::AcqRel);
        let active = WorkerPermit(self.active_cells.clone());
        thread::spawn(move || {
            let _active = active;
            let result = run_cell(code, shell, stored, observer, parent, workers, &worker);
            worker.cancel.cancel();
            let mut state = worker.state.0.lock().expect("cell state");
            if let Err(error) = result {
                state.error = Some(format!("{error:#}"));
            }
            state.done = true;
            worker.state.1.notify_one();
        });
        self.observe(
            &cell_id,
            pragma.yield_time_ms.unwrap_or(10_000),
            pragma.max_output_tokens.unwrap_or(10_000),
            cancel,
        )
    }
    pub fn wait(&self, args: &str, cancel: &CancellationToken) -> Result<String> {
        let args: Wait = serde_json::from_str(args)?;
        if args.terminate.unwrap_or(false) {
            let cells = self.cells.lock().expect("cell lock");
            cells
                .get(&args.cell_id)
                .context("Unknown or already closed cell")?
                .cancel
                .cancel();
        }
        self.observe(
            &args.cell_id,
            args.yield_time_ms.unwrap_or(10_000),
            args.max_tokens.unwrap_or(10_000),
            cancel,
        )
    }
    fn observe(
        &self,
        id: &str,
        ms: u64,
        tokens: usize,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let cell = self
            .cells
            .lock()
            .expect("cell lock")
            .get(id)
            .cloned()
            .context("Unknown or already closed cell")?;
        self.shell.runtime().block_on(async {
            let deadline=tokio::time::Instant::now()+Duration::from_millis(ms.min(300_000));
            loop {
                let notified=cell.state.1.notified();
                { let state=cell.state.0.lock().expect("cell state");if state.done || state.yielded {break;} }
                tokio::select! {
                    biased;
                    _=cancel.cancelled()=>{cell.cancel.cancel();self.cells.lock().expect("cell lock").remove(id);bail!("Execution cancelled");}
                    _=tokio::time::sleep_until(deadline)=>break,
                    _=notified=>{}
                }
            }
            Ok::<_,anyhow::Error>(())
        })?;
        let mut state = cell.state.0.lock().expect("cell state");
        state.yielded = false;
        let output = state
            .output
            .read(character_budget(tokens))
            .with_context(|| {
                format!("Could not return cell output. Retry wait with cell_id {id}")
            })?;
        let status = if state.done {
            self.cells.lock().expect("cell lock").remove(id);
            match &state.error {
                Some(e) => format!(
                    "Script failed: {}",
                    crate::output::preserve_text(e, character_budget(tokens))
                ),
                None => "Script completed".into(),
            }
        } else {
            format!("Script running with cell ID {id}")
        };
        Ok(format!("{status}\nOutput:\n{output}"))
    }
    pub fn is_executing(&self) -> bool {
        self.active_cells.load(Ordering::Acquire) > 0 || self.workers.load(Ordering::Acquire) > 0
    }
    pub fn has_work(&self) -> bool {
        !self.cells.lock().expect("cell lock").is_empty()
            || self.shell.has_work()
            || !self.stored.lock().expect("store lock").values.is_empty()
    }
    pub fn stop_all(&self) {
        for (_, cell) in self.cells.lock().expect("cell lock").drain() {
            cell.cancel.cancel();
        }
        self.shell.stop_all();
    }
}
impl Drop for CodeMode {
    fn drop(&mut self) {
        self.stop_all();
    }
}
fn parse(source: &str) -> Result<(Pragma, String)> {
    if source.trim().is_empty() {
        bail!("exec expects non-empty raw JavaScript, not JSON or Markdown fences");
    }
    let (first, rest) = source.split_once('\n').unwrap_or((source, ""));
    if let Some(pragma) = first.trim_start().strip_prefix("// @exec:") {
        if rest.trim().is_empty() {
            bail!("Execution pragma must be followed by JavaScript");
        }
        Ok((serde_json::from_str(pragma)?, rest.into()))
    } else {
        Ok((Pragma::default(), source.into()))
    }
}
fn run_cell(
    code: String,
    (shell, delegation, questionnaires): (
        Arc<Shell>,
        Option<Delegation>,
        Option<Arc<crate::questionnaire::Questionnaires>>,
    ),
    stored: Arc<Mutex<Stored>>,
    observer: Observer,
    parent: String,
    workers: Arc<AtomicUsize>,
    cell: &Cell,
) -> Result<()> {
    let executor = shell.runtime().clone();
    let runtime = Runtime::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    runtime.set_memory_limit(64 * 1024 * 1024);
    runtime.set_max_stack_size(1024 * 1024);
    let cancel = cell.cancel.clone();
    let exited = Arc::new(AtomicBool::new(false));
    let interrupt_exit = exited.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        cancel.is_cancelled() || interrupt_exit.load(Ordering::Relaxed)
    })));
    let context = Context::full(&runtime).map_err(|e| anyhow::anyhow!("{e}"))?;
    context.with(|ctx| -> Result<()> {
        let exit = exited.clone();
        ctx.globals()
            .set(
                "__exit",
                Function::new(ctx.clone(), move || {
                    exit.store(true, Ordering::Relaxed);
                }),
            )
            .map_err(js_error)?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, String, bool)>();
        let notify_observer = observer.clone();
        let notify_parent = parent.clone();
        ctx.globals()
            .set(
                "__notify",
                Function::new(ctx.clone(), move |text: String| -> rquickjs::Result<()> {
                    notify_observer(Event::Notification {
                        parent: notify_parent.clone(),
                        text,
                    }).map_err(|e| rquickjs::Error::new_from_js_message("notify","storage",e.to_string()))
                }),
            )
            .map_err(js_error)?;
        let state = cell.state.clone();
        ctx.globals()
            .set(
                "__text",
                Function::new(ctx.clone(), move |value: String, immediate: bool| {
                    let mut s = state.0.lock().expect("cell state");
                    const LIMIT:usize=4*1024*1024;
                    if !value.is_empty() {
                        if !s.output.is_empty() {s.output.append(b"\n", LIMIT);}
                        s.output.append(value.as_bytes(), LIMIT);
                    }
                    if immediate {
                        s.yielded = true;
                    }
                    state.1.notify_one();
                }),
            )
            .map_err(js_error)?;
        let save = stored.clone();
        ctx.globals()
            .set(
                "__store",
                Function::new(
                    ctx.clone(),
                    move |key: String, value: String| -> rquickjs::Result<()> {
                        let mut stored = save.lock().expect("store lock");
                        let existing = stored.values.get(&key).map_or(0, |value| key.len()+value.len());
                        let size=stored.bytes-existing+key.len()+value.len();
                        if size > 16 * 1024 * 1024 {
                            return Err(rquickjs::Error::new_from_js_message(
                                "value",
                                "store",
                                "Session store exceeds 16 MiB; store smaller values",
                            ));
                        }
                        stored.bytes=size;
                        stored.values.insert(key, value);
                        Ok(())
                    },
                ),
            )
            .map_err(js_error)?;
        ctx.globals()
            .set(
                "__load",
                Function::new(ctx.clone(), move |key: String| -> Option<String> {
                    stored.lock().expect("store lock").values.get(&key).cloned()
                }),
            )
            .map_err(js_error)?;
        ctx.globals().set("__delegation", delegation.is_some()).map_err(js_error)?;
        ctx.globals().set("__questionnaires", questionnaires.is_some()).map_err(js_error)?;
        let cancellation = cell.cancel.clone();
        ctx.globals()
            .set(
                "__call",
                Function::new(
                    ctx.clone(),
                    move |name: String, args: String| -> rquickjs::Result<String> {
                        if !matches!(name.as_str(), "exec_command" | "write_stdin") && !(questionnaires.is_some() && name == "ask_user") && !(delegation.is_some() && matches!(name.as_str(), "spawn_subagent" | "wait_subagent" | "list_subagents")) {
                            return Err(rquickjs::Error::new_from_js_message(
                                "name",
                                "tool",
                                "Unknown nested tool",
                            ));
                        }
                        if workers.fetch_add(1, Ordering::AcqRel) >= 64 {
                            workers.fetch_sub(1, Ordering::AcqRel);
                            return Err(rquickjs::Error::new_from_js_message(
                                "call",
                                "tool",
                                "64 nested calls already active; await them before starting more",
                            ));
                        }
                        let permit = WorkerPermit(workers.clone());
                        let id = id();
                        let request = id.clone();
                        let shell = shell.clone();
                        let delegation = delegation.clone();
                        let questionnaires = questionnaires.clone();
                        let tx = tx.clone();
                        let observer = observer.clone();
                        let cancellation = cancellation.clone();
                        let parent = parent.clone();
                        thread::Builder::new()
                            .spawn(move || {
                                let _permit = permit;
                                let started = observer(Event::Tool(ToolEvent {
                                    id: request.clone(),
                                    parent: parent.clone(),
                                    name: name.clone(),
                                    input: args.clone(),
                                    output: None,
                                }));
                                let result = started.and_then(|()| match name.as_str() {
                                    "exec_command" => serde_json::from_str(&args)
                                        .map_err(anyhow::Error::from)
                                        .and_then(|a| shell.exec(a, &cancellation)),
                                    "write_stdin" => serde_json::from_str(&args)
                                        .map_err(anyhow::Error::from)
                                        .and_then(|a| shell.write(a, &cancellation)),
                                    "ask_user" => shell.runtime().block_on(questionnaires.as_ref().expect("validated questionnaire tool").ask(&request, &args, &cancellation)),
                                    _ => delegation.as_ref().expect("validated delegation tool")(&name, &args, &request, &cancellation),
                                });
                                let failed = result.is_err();
                                let output = result.unwrap_or_else(|e| format!("{e:#}"));
                                let name_for_output = name.clone();
                                let saved = observer(Event::Tool(ToolEvent {
                                    id: request.clone(),
                                    parent,
                                    name,
                                    input: args,
                                    output: Some(output.clone()),
                                }));
                                let (output, failed) = match saved { Ok(()) => (output,failed), Err(e) => (format!("{output}\nCould not save tool result: {e:#}. Side effects may have occurred; do not blindly repeat the command."),true) };
                                let output = if name_for_output == "ask_user" { crate::output::preserve_text(&output, 40_000) } else { output };
                                let _ = tx.send((request, output, failed));
                            })
                            .map_err(|e| {
                                rquickjs::Error::new_from_js_message("call", "tool", e.to_string())
                            })?;
                        Ok(id)
                    },
                ),
            )
            .map_err(js_error)?;
        ctx.eval::<(), _>(include_str!("code_mode.js"))
            .map_err(|e| exception(&ctx, e))?;
        let promise =
            Module::evaluate(ctx.clone(), "cell", code).map_err(|e| exception(&ctx, e))?;
        loop {
            if cell.cancel.is_cancelled() {
                bail!("Cell terminated");
            }
            while promise.state() == rquickjs::promise::PromiseState::Pending
                && ctx.execute_pending_job()
            {
                if exited.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if cell.cancel.is_cancelled() {
                    bail!("Cell terminated");
                }
            }
            if exited.load(Ordering::Relaxed) {
                return Ok(());
            }
            if let Some(result) = promise.result::<JsValue>() {
                return result.map(|_| ()).map_err(|e| exception(&ctx, e));
            }
            let delay: Option<f64>=ctx.globals().get::<_,Function>("__nextTimer").map_err(js_error)?.call(()).map_err(js_error)?;
            let completion=executor.block_on(async {
                tokio::select! {
                    biased;
                    _=cell.cancel.cancelled()=>None,
                    result=rx.recv()=>result,
                    _=async {match delay {Some(ms)=>tokio::time::sleep(Duration::from_secs_f64((ms/1000.).clamp(0.,86400.))).await,None=>std::future::pending().await}}=>None,
                }
            });
            if let Some((id,output,failed))=completion {
                ctx.globals().get::<_,Function>("__deliver").map_err(js_error)?.call::<_,()>((id,output,failed)).map_err(|e|exception(&ctx,e))?;
            }
            ctx.globals().get::<_,Function>("__tick").map_err(js_error)?.call::<_,()>(()).map_err(|e|exception(&ctx,e))?;
        }
    })
}
struct WorkerPermit(Arc<AtomicUsize>);
impl Drop for WorkerPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn js_error(error: rquickjs::Error) -> anyhow::Error {
    anyhow::anyhow!("JavaScript runtime: {error}")
}
fn exception(ctx: &rquickjs::Ctx<'_>, error: rquickjs::Error) -> anyhow::Error {
    if error.is_exception() {
        let value = ctx.catch();
        if let Some(ex) = value.as_exception() {
            return anyhow::anyhow!(
                "{}",
                ex.message()
                    .unwrap_or_else(|| "JavaScript exception".into())
            );
        }
        return anyhow::anyhow!("JavaScript exception: {value:?}");
    }
    js_error(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mode() -> CodeMode {
        CodeMode::new(
            Arc::new(Shell::new(std::env::temp_dir())),
            Arc::new(|_| Ok(())),
        )
    }
    #[test]
    fn questionnaires_yield_preserve_full_results_and_follow_cell_lifetime() {
        let (changed, changes) = std::sync::mpsc::channel();
        let q = Arc::new(crate::questionnaire::Questionnaires::new(move || {
            let _ = changed.send(());
        }));
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer = events.clone();
        let mode = CodeMode::with_tools(
            Arc::new(Shell::new(std::env::temp_dir())),
            Arc::new(move |event| {
                observer.lock().unwrap().push(event);
                Ok(())
            }),
            None,
            Some(q.clone()),
        );
        let cancel = CancellationToken::new();
        let source = r#"// @exec: {"yield_time_ms": 1}
text(await tools.ask_user({questions:[{id:"target",question:"Target?",kind:"text"}]}));"#;
        let running = mode.exec(source, "parent", &cancel).unwrap();
        assert!(
            running.starts_with("Script running with cell ID "),
            "{running}"
        );
        changes.recv_timeout(Duration::from_secs(2)).unwrap();
        let request = q.pending().pop().unwrap();
        let text = "Important λ界\n".repeat(20_000);
        q.submit(&request.id, vec![text.clone()]).unwrap();
        let cell_id = running
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        let result = mode
            .wait(&json!({"cell_id":cell_id}).to_string(), &cancel)
            .unwrap();
        assert!(result.starts_with("Script completed"), "{result}");
        let preserved: Value =
            serde_json::from_slice(&crate::test_support::saved_output(&result)).unwrap();
        assert_eq!(
            preserved,
            json!({"answers":[{"id":"target","answer":text}]})
        );
        assert!(!q.has_pending());
        let persisted = events.lock().unwrap();
        assert!(persisted.iter().any(|e| matches!(e, Event::Tool(t) if t.name == "ask_user" && t.output.as_ref().is_some_and(|v| serde_json::from_str::<Value>(v).unwrap() == preserved))));
        drop(persisted);
        assert!(
            mode.wait(&json!({"cell_id":cell_id}).to_string(), &cancel)
                .is_err()
        );

        // A detached promise cannot keep a completed cell's questionnaire alive.
        let result = mode
            .exec(
                "tools.ask_user({questions:[{id:'q',question:'?',kind:'text'}]});",
                "detached",
                &cancel,
            )
            .unwrap();
        assert!(result.starts_with("Script completed"));
        assert!(!q.has_pending());
        let running = mode.exec(source, "cancelled", &cancel).unwrap();
        let cell_id = running
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        mode.wait(
            &json!({"cell_id":cell_id,"terminate":true}).to_string(),
            &cancel,
        )
        .unwrap();
        assert!(!q.has_pending());

        // Capability omission is enforced in JS and the native dispatcher.
        let child = self::mode();
        for source in [
            "text(ALL_TOOLS.some(t=>t.name==='ask_user'));",
            "text(typeof tools.ask_user);",
        ] {
            let result = child.exec(source, "child", &cancel).unwrap();
            assert!(
                result.ends_with("false") || result.ends_with("undefined"),
                "{result}"
            );
        }
        let result = child
            .exec("__call('ask_user','{}');", "child", &cancel)
            .unwrap();
        assert!(result.contains("Unknown nested tool"), "{result}");
    }
    #[test]
    fn cancelled_cell_remains_active_until_native_callback_exits() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let waiter = release.clone();
        let mode = CodeMode::new(
            Arc::new(Shell::new(std::env::temp_dir())),
            Arc::new(move |_| {
                entered_tx.send(()).unwrap();
                let (lock, cv) = &*waiter;
                let mut done = lock.lock().unwrap();
                while !*done {
                    done = cv.wait(done).unwrap();
                }
                Ok(())
            }),
        );
        let result = mode
            .exec(
                "// @exec: {\"yield_time_ms\": 1}\nnotify('blocked');",
                "parent",
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(result.contains("Script running"));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        mode.stop_all();
        assert!(
            mode.is_executing(),
            "draining IDs must not hide live callbacks"
        );
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while mode.is_executing() {
            assert!(std::time::Instant::now() < deadline);
            thread::yield_now();
        }
    }
    #[test]
    fn timer_callbacks_yield_to_resolved_module_and_storage_failure_blocks_commands() {
        let mode = mode();
        let result=mode.exec("function again(){setTimeout(again,0);} await new Promise(resolve=>setTimeout(()=>{resolve();again();},0));text('finished');","timer",&CancellationToken::new()).unwrap();
        assert!(result.contains("Script completed"));
        assert!(result.contains("finished"));
        let dir = tempfile::tempdir().unwrap();
        let blocked = CodeMode::new(
            Arc::new(Shell::new(dir.path().into())),
            Arc::new(|_| bail!("fixture persistence failure")),
        );
        let result = blocked
            .exec(
                "text(await tools.exec_command({cmd:'touch marker',login:false}));",
                "blocked",
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(result.contains("fixture persistence failure"));
        assert!(!dir.path().join("marker").exists());
    }
    #[test]
    fn cell_output_spills_all_unicode_and_releases_capacity_after_reading() {
        let mode = mode();
        let result = mode
            .exec(
                "text('λ'.repeat(2097151));text('界界');for(let i=0;i<10000;i++)text('');",
                "output",
                &CancellationToken::new(),
            )
            .unwrap();
        let expected = format!("{}\n界界", "λ".repeat(2097151));
        assert_eq!(
            crate::test_support::saved_output(&result),
            expected.as_bytes()
        );
        assert!(result.contains("Script completed"));
        let result = mode
            .exec("text('small output');", "next", &CancellationToken::new())
            .unwrap();
        assert!(result.contains("small output"));
        assert!(!result.contains("limit reached"));
    }
    #[test]
    fn outer_exec_and_wait_budgets_spill_instead_of_hiding_text() {
        let mode = mode();
        let cancel = CancellationToken::new();
        let result = mode
            .exec(
                "// @exec: {\"max_output_tokens\": 0}\ntext('important middle λ');",
                "zero",
                &cancel,
            )
            .unwrap();
        assert_eq!(
            crate::test_support::saved_output(&result),
            "important middle λ".as_bytes()
        );

        // Hold the cell in notify until exec has returned its running ID.
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let waiter = release.clone();
        let waiting = CodeMode::new(
            Arc::new(Shell::new(std::env::temp_dir())),
            Arc::new(move |_| {
                entered_tx.send(()).unwrap();
                let mut ready = waiter.0.lock().unwrap();
                while !*ready {
                    ready = waiter.1.wait(ready).unwrap();
                }
                Ok(())
            }),
        );
        let initial = waiting
            .exec(
                "// @exec: {\"yield_time_ms\": 0}\nnotify('ready');text('start λ middle 界 end');",
                "wait",
                &cancel,
            )
            .unwrap();
        let id = initial
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        let result = waiting
            .wait(
                &json!({"cell_id": id, "max_tokens": 1}).to_string(),
                &cancel,
            )
            .unwrap();
        assert!(result.contains("Script completed"));
        drop(waiting);
        assert_eq!(
            crate::test_support::saved_output(&result),
            "start λ middle 界 end".as_bytes()
        );
    }

    #[test]
    fn oversized_exception_preserves_complete_message_with_zero_budget() {
        let result = mode().exec(
            "// @exec: {\"max_output_tokens\": 0}\nthrow new Error('start ' + 'λ'.repeat(50000) + ' crucial end');",
            "error", &CancellationToken::new(),
        ).unwrap();
        assert!(result.starts_with("Script failed:"));
        assert_eq!(
            crate::test_support::saved_output(&result),
            format!("start {} crucial end", "λ".repeat(50000)).as_bytes()
        );
    }

    #[test]
    fn notifications_are_separate_from_direct_cell_output() {
        let events = Arc::new(Mutex::new(vec![]));
        let recorded = events.clone();
        let mode = CodeMode::new(
            Arc::new(Shell::new(std::env::temp_dir())),
            Arc::new(move |event| {
                recorded.lock().unwrap().push(event);
                Ok(())
            }),
        );
        let out = mode
            .exec(
                "notify('progress'); text('final');",
                "origin",
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(out.contains("final"));
        assert!(!out.contains("progress"));
        assert!(
            matches!(&events.lock().unwrap()[0],Event::Notification{parent,text} if parent=="origin" && text=="progress")
        );
    }
    #[test]
    fn completed_module_discards_unawaited_microtasks_and_exit_is_successful() {
        let mode = mode();
        let cancel = CancellationToken::new();
        let out = mode.exec("// @exec: {\"yield_time_ms\": 100}\n(async()=>{while(true) await Promise.resolve()})(); text('finished');", "p", &cancel).unwrap();
        assert!(out.starts_with("Script completed"), "{out}");
        let out = mode
            .exec("text('before'); exit(); text('unreachable');", "p", &cancel)
            .unwrap();
        assert!(out.starts_with("Script completed"), "{out}");
        assert!(!out.contains("unreachable"));
    }
    #[test]
    fn cancellation_does_not_exhaust_future_cell_slots() {
        let mode = mode();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        for _ in 0..40 {
            assert!(mode.exec("text('no')", "p", &cancelled).is_err());
        }
        let out = mode
            .exec("text('recovered')", "p", &CancellationToken::new())
            .unwrap();
        assert!(out.contains("recovered"));
    }
    #[test]
    fn fresh_cells_store_values_and_await_parallel_tools() {
        let mode = mode();
        let cancel = CancellationToken::new();
        let out = mode.exec("const x=7; store('saved', {n:x}); text(await Promise.all([tools.exec_command({cmd:'printf left',login:false}), tools.exec_command({cmd:'printf right',login:false})]));", "parent", &cancel).unwrap();
        assert!(out.contains("left"));
        assert!(out.contains("right"));
        let out = mode
            .exec("text(typeof x); text(load('saved'));", "p", &cancel)
            .unwrap();
        assert!(out.contains("undefined"));
        assert!(out.contains("\"n\":7"));
    }
    #[test]
    fn yielding_and_waiting_consume_output_once() {
        let mode = mode();
        let cancel = CancellationToken::new();
        let out = mode.exec("// @exec: {\"yield_time_ms\": 5}\ntext('first'); await new Promise(r=>setTimeout(r,100)); text('last');", "p", &cancel).unwrap();
        let id = out
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        let last = mode
            .wait(
                &json!({"cell_id":id,"yield_time_ms":2000}).to_string(),
                &cancel,
            )
            .unwrap();
        assert!(last.contains("completed"));
        assert!(last.contains("last"));
        assert!(!last.contains("first"));
        assert!(
            mode.wait(&json!({"cell_id":id}).to_string(), &cancel)
                .is_err()
        );
    }
    #[test]
    fn infinite_loop_can_be_terminated() {
        let mode = mode();
        let cancel = CancellationToken::new();
        let out = mode
            .exec(
                "// @exec: {\"yield_time_ms\": 5}\nwhile(true) {}",
                "p",
                &cancel,
            )
            .unwrap();
        let id = out
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        let out = mode
            .wait(
                &json!({"cell_id":id,"terminate":true,"yield_time_ms":2000}).to_string(),
                &cancel,
            )
            .unwrap();
        assert!(out.contains("failed"));
    }
}
