//! Fresh QuickJS cells implementing Codex's public code-mode contract.
use crate::{
    shell::{Shell, truncate},
    store::id,
};
use anyhow::{Context as _, Result, bail};
use rquickjs::{Context, Function, Module, Runtime, Value as JsValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
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
pub type Observer = Arc<dyn Fn(Event) + Send + Sync>;
#[derive(Default)]
struct State {
    output: Vec<String>,
    done: bool,
    error: Option<String>,
    yielded: bool,
}
struct Cell {
    state: Arc<(Mutex<State>, Condvar)>,
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
pub struct CodeMode {
    shell: Arc<Shell>,
    cells: Mutex<HashMap<String, Arc<Cell>>>,
    stored: Arc<Mutex<HashMap<String, String>>>,
    observer: Observer,
    workers: Arc<AtomicUsize>,
}
impl CodeMode {
    pub fn new(shell: Arc<Shell>, observer: Observer) -> Self {
        Self {
            shell,
            cells: Mutex::new(HashMap::new()),
            stored: Arc::new(Mutex::new(HashMap::new())),
            observer,
            workers: Arc::new(AtomicUsize::new(0)),
        }
    }
    pub fn exec(&self, source: &str, parent: &str, cancel: &CancellationToken) -> Result<String> {
        if cancel.is_cancelled() {
            bail!("Execution cancelled");
        }
        let (pragma, code) = parse(source)?;
        if self.cells.lock().expect("cell lock").len() >= 32 {
            bail!("Too many open cells. Wait for or terminate existing cells.");
        }
        let cell_id = id();
        let cell = Arc::new(Cell {
            state: Arc::new((Mutex::new(State::default()), Condvar::new())),
            cancel: cancel.child_token(),
        });
        self.cells
            .lock()
            .expect("cell lock")
            .insert(cell_id.clone(), cell.clone());
        let shell = self.shell.clone();
        let stored = self.stored.clone();
        let observer = self.observer.clone();
        let parent = parent.to_owned();
        let worker = cell.clone();
        let workers = self.workers.clone();
        thread::spawn(move || {
            let result = run_cell(code, shell, stored, observer, parent, workers, &worker);
            worker.cancel.cancel();
            let mut state = worker.state.0.lock().expect("cell state");
            if let Err(error) = result {
                state.error = Some(format!("{error:#}"));
            }
            state.done = true;
            worker.state.1.notify_all();
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
        let deadline = Instant::now() + Duration::from_millis(ms.min(300_000));
        let mut state = cell.state.0.lock().expect("cell state");
        while !state.done && !state.yielded && Instant::now() < deadline {
            if cancel.is_cancelled() {
                cell.cancel.cancel();
                self.cells.lock().expect("cell lock").remove(id);
                bail!("Execution cancelled");
            }
            state = cell
                .state
                .1
                .wait_timeout(state, Duration::from_millis(10))
                .expect("cell wait")
                .0;
        }
        state.yielded = false;
        let output = truncate(&std::mem::take(&mut state.output).join("\n"), tokens);
        let status = if state.done {
            self.cells.lock().expect("cell lock").remove(id);
            match &state.error {
                Some(e) => format!("Script failed: {e}"),
                None => "Script completed".into(),
            }
        } else {
            format!("Script running with cell ID {id}")
        };
        Ok(format!("{status}\nOutput:\n{output}"))
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
    shell: Arc<Shell>,
    stored: Arc<Mutex<HashMap<String, String>>>,
    observer: Observer,
    parent: String,
    workers: Arc<AtomicUsize>,
    cell: &Cell,
) -> Result<()> {
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
        let (tx, rx) = mpsc::channel::<(String, String, bool)>();
        let notify_observer = observer.clone();
        let notify_parent = parent.clone();
        ctx.globals()
            .set(
                "__notify",
                Function::new(ctx.clone(), move |text: String| {
                    notify_observer(Event::Notification {
                        parent: notify_parent.clone(),
                        text,
                    });
                }),
            )
            .map_err(js_error)?;
        let state = cell.state.clone();
        ctx.globals()
            .set(
                "__text",
                Function::new(ctx.clone(), move |value: String, immediate: bool| {
                    let mut s = state.0.lock().expect("cell state");
                    if s.output.iter().map(String::len).sum::<usize>() < 4 * 1024 * 1024 {
                        s.output.push(value);
                    }
                    if immediate {
                        s.yielded = true;
                    }
                    state.1.notify_all();
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
                        let existing = stored.get(&key).map_or(0, String::len);
                        let size = stored
                            .iter()
                            .map(|(key, value)| key.len() + value.len())
                            .sum::<usize>();
                        if size - existing + key.len() + value.len() > 16 * 1024 * 1024 {
                            return Err(rquickjs::Error::new_from_js_message(
                                "value",
                                "store",
                                "Session store exceeds 16 MiB; store smaller values",
                            ));
                        }
                        stored.insert(key, value);
                        Ok(())
                    },
                ),
            )
            .map_err(js_error)?;
        ctx.globals()
            .set(
                "__load",
                Function::new(ctx.clone(), move |key: String| -> Option<String> {
                    stored.lock().expect("store lock").get(&key).cloned()
                }),
            )
            .map_err(js_error)?;
        let cancellation = cell.cancel.clone();
        ctx.globals()
            .set(
                "__call",
                Function::new(
                    ctx.clone(),
                    move |name: String, args: String| -> rquickjs::Result<String> {
                        if !matches!(name.as_str(), "exec_command" | "write_stdin") {
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
                        let tx = tx.clone();
                        let observer = observer.clone();
                        let cancellation = cancellation.clone();
                        let parent = parent.clone();
                        thread::Builder::new()
                            .spawn(move || {
                                let _permit = permit;
                                observer(Event::Tool(ToolEvent {
                                    id: request.clone(),
                                    parent: parent.clone(),
                                    name: name.clone(),
                                    input: args.clone(),
                                    output: None,
                                }));
                                let result = match name.as_str() {
                                    "exec_command" => serde_json::from_str(&args)
                                        .map_err(anyhow::Error::from)
                                        .and_then(|a| shell.exec(a, &cancellation)),
                                    "write_stdin" => serde_json::from_str(&args)
                                        .map_err(anyhow::Error::from)
                                        .and_then(|a| shell.write(a, &cancellation)),
                                    _ => unreachable!("fixed tool names"),
                                };
                                let failed = result.is_err();
                                let output = result.unwrap_or_else(|e| format!("{e:#}"));
                                observer(Event::Tool(ToolEvent {
                                    id: request.clone(),
                                    parent,
                                    name,
                                    input: args,
                                    output: Some(output.clone()),
                                }));
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
            for (id, output, failed) in rx.try_iter() {
                let deliver: Function = ctx.globals().get("__deliver").map_err(js_error)?;
                deliver
                    .call::<_, ()>((id, output, failed))
                    .map_err(|e| exception(&ctx, e))?;
            }
            ctx.eval::<(), _>("__tick()")
                .map_err(|e| exception(&ctx, e))?;
            thread::sleep(Duration::from_millis(2));
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
        CodeMode::new(Arc::new(Shell::new(std::env::temp_dir())), Arc::new(|_| {}))
    }
    #[test]
    fn notifications_are_separate_from_direct_cell_output() {
        let events = Arc::new(Mutex::new(vec![]));
        let recorded = events.clone();
        let mode = CodeMode::new(
            Arc::new(Shell::new(std::env::temp_dir())),
            Arc::new(move |event| recorded.lock().unwrap().push(event)),
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
