//! Fresh QuickJS cells implementing Codex's public code-mode contract.
use crate::{
    image_tools::ImageTools,
    media::{Output, Part},
    output::{OutputBuffer, character_budget},
    shell::{CommandStatus, Shell},
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
mod catalog;
mod live;
pub(crate) use live::Calls;
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ToolStop {
    Available,
    Stopping,
}
use catalog::Capabilities;

pub fn tools(child: bool) -> Value {
    let definitions = catalog::definitions(Capabilities::conversation(child));
    let description = catalog::description(DESCRIPTION, &definitions);
    json!([
        {"type":"custom","name":"exec","description":description,"format":{"type":"grammar","syntax":"lark","definition":GRAMMAR}},
        {"type":"function","name":"wait","description":"Retrieve new output from a running exec cell or terminate it.","strict":false,"parameters":{"type":"object","properties":{"cell_id":{"type":"string","description":"Identifier of the running exec cell."},"yield_time_ms":{"type":"number","description":"Wait before yielding more output. Defaults to 10000 ms."},"max_tokens":{"type":"number","description":"Output token budget for this wait call. Defaults to 10000 tokens."},"terminate":{"type":"boolean","description":"True stops the running exec cell; false or omitted waits for output."}},"required":["cell_id"],"additionalProperties":false}}
    ])
}
const DESCRIPTION: &str = r#"Run JavaScript tool calls in a fresh async QuickJS module; no Node, filesystem, network or console APIs.
Only emitted output is returned. Await tool calls; unawaited work ends with the cell. Independent calls can use Promise.all.
Optional first line: // @exec: {"yield_time_ms": 10000, "max_output_tokens": 10000}
Shell max_output_tokens limits what exec_command or write_stdin returns to JavaScript; @exec max_output_tokens separately limits what the cell emits inline. Raise both for larger inline shell output.
Oversized text is saved in full under /tmp with its path and byte size. Read or search that file with exec_command; wait does not replay it.
For new output, use wait with a running cell_id or write_stdin with a running shell session_id.
Globals:
- text(value): emit text or JSON.
- image(value), audio(value): emit a base64 data URL, URL object or MCP media block.
- generatedImage(result): emit an image tool result and its output hint.
- content(blockOrIterable): emit MCP text/image/audio/resource/resource_link blocks.
- notify(value): emit an immediate notification.
- yield_control(): yield current output without ending the cell.
- store(key, value), load(key): share serializable values across cells until app exit.
- setTimeout(callback, delayMs), clearTimeout(id): schedule/cancel callbacks; timers alone do not keep cells alive.
- exit(): end the cell.
- ALL_TOOLS: available nested tool definitions.
Use media helpers, not text, for image/audio results. Reply images use ![description](path)."#;

#[derive(Clone, Debug)]
pub struct ToolEvent {
    pub id: String,
    pub parent: String,
    pub name: String,
    pub input: String,
    pub output: Option<String>,
    pub failed: bool,
    pub media: Vec<crate::media::Media>,
    pub web: Option<crate::web_search::Metadata>,
}
#[derive(Clone, Debug)]
pub enum Event {
    Tool(ToolEvent),
    ExecutionStatus {
        id: String,
        status: crate::shell::CommandStatus,
    },
    Notification {
        parent: String,
        text: String,
    },
}
pub type Delegation =
    Arc<dyn Fn(&str, &str, &str, &CancellationToken) -> Result<String> + Send + Sync>;
pub type Observer = Arc<dyn Fn(Event) -> Result<()> + Send + Sync>;
#[derive(Default)]
struct State {
    output: OutputBuffer,
    parts: Vec<BufferedPart>,
    done: bool,
    error: Option<String>,
    yielded: bool,
}
enum BufferedPart {
    Text(OutputBuffer),
    Media(crate::media::Media),
}
struct Cell {
    calls: Option<Arc<Calls>>,
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
    services: Services,
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
            services: Services {
                shell,
                delegation,
                questionnaires,
                images: None,
                web: None,
            },
            cells: Mutex::new(HashMap::new()),
            stored: Arc::new(Mutex::new(Stored::default())),
            observer,
            workers: Arc::new(AtomicUsize::new(0)),
            active_cells: Arc::new(AtomicUsize::new(0)),
        }
    }
    pub fn with_images(mut self, images: Arc<ImageTools>) -> Self {
        self.services.images = Some(images);
        self
    }
    pub(crate) fn with_web(mut self, web: Arc<crate::web_search::WebSearch>) -> Self {
        self.services.web = Some(web);
        self
    }
    pub(crate) fn media_lifecycle(&self) -> Option<std::sync::MutexGuard<'_, bool>> {
        self.services.images.as_ref().map(|i| i.media.lifecycle())
    }
    #[cfg(test)]
    pub fn exec(&self, source: &str, parent: &str, cancel: &CancellationToken) -> Result<String> {
        self.exec_output(source, parent, cancel)
            .map(|o| o.summary())
    }
    pub fn exec_output(
        &self,
        source: &str,
        parent: &str,
        cancel: &CancellationToken,
    ) -> Result<Output> {
        self.exec_cell(source, parent, cancel, None)
    }
    /// The harness has already checkpointed the transcript row for this call.
    pub(crate) fn invoke(
        &self,
        name: &str,
        input: &str,
        id: &str,
        cancel: &CancellationToken,
        calls: &Arc<Calls>,
    ) -> Result<Output> {
        match name {
            "exec" => self.exec_cell(input, id, cancel, Some(calls)),
            "wait" => {
                let stop = CancellationToken::new();
                let _live = calls.register(id.to_owned(), &stop);
                self.wait_cell(input, cancel, &stop)
            }
            _ => bail!("Unknown tool {name}; only exec and wait are top-level tools"),
        }
    }
    fn exec_cell(
        &self,
        source: &str,
        parent: &str,
        cancel: &CancellationToken,
        calls: Option<&Arc<Calls>>,
    ) -> Result<Output> {
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
            calls: calls.cloned(),
            state: Arc::new((Mutex::new(State::default()), tokio::sync::Notify::new())),
            cancel: cancel.child_token(),
        });
        cells.insert(cell_id.clone(), cell.clone());
        drop(cells);
        let live = if let Some(calls) = calls {
            if let Err(error) = (self.observer)(Event::ExecutionStatus {
                id: parent.to_owned(),
                status: CommandStatus::Running,
            }) {
                self.cells.lock().expect("cell lock").remove(&cell_id);
                return Err(error);
            }
            Some(calls.register(parent.to_owned(), &cell.cancel))
        } else {
            None
        };
        let services = self.services.clone();
        let stored = self.stored.clone();
        let observer = self.observer.clone();
        let parent = parent.to_owned();
        let worker = cell.clone();
        let workers = self.workers.clone();
        self.active_cells.fetch_add(1, Ordering::AcqRel);
        let active = WorkerPermit(self.active_cells.clone());
        thread::spawn(move || {
            let _active = active;
            let result = run_cell(
                code,
                services,
                stored,
                observer.clone(),
                parent.clone(),
                workers,
                &worker,
            );
            let status = if result.is_ok() {
                CommandStatus::Succeeded
            } else if worker.cancel.is_cancelled() {
                CommandStatus::Interrupted
            } else {
                CommandStatus::Failed
            };
            let tracked = live.is_some();
            drop(live);
            worker.cancel.cancel();
            let mut state = worker.state.0.lock().expect("cell state");
            if let Err(error) = result {
                state.error = Some(format!("{error:#}"));
            }
            state.done = true;
            worker.state.1.notify_one();
            drop(state);
            if tracked && let Err(error) = observer(Event::ExecutionStatus { id: parent, status }) {
                eprintln!("Could not record code cell exit: {error:#}");
            }
        });
        self.observe(
            &cell_id,
            cell,
            pragma.yield_time_ms.unwrap_or(10_000),
            pragma.max_output_tokens.unwrap_or(10_000),
            cancel,
            &CancellationToken::new(),
        )
    }
    #[cfg(test)]
    pub fn wait(&self, args: &str, cancel: &CancellationToken) -> Result<String> {
        self.wait_output(args, cancel).map(|o| o.summary())
    }
    pub fn wait_output(&self, args: &str, cancel: &CancellationToken) -> Result<Output> {
        self.wait_cell(args, cancel, &CancellationToken::new())
    }
    fn wait_cell(
        &self,
        args: &str,
        cancel: &CancellationToken,
        stop: &CancellationToken,
    ) -> Result<Output> {
        let args: Wait = serde_json::from_str(args)?;
        let cell = self
            .cells
            .lock()
            .expect("cell lock")
            .get(&args.cell_id)
            .cloned()
            .context("Unknown or already closed cell")?;
        if args.terminate.unwrap_or(false) {
            cell.cancel.cancel();
        }
        self.observe(
            &args.cell_id,
            cell,
            args.yield_time_ms.unwrap_or(10_000),
            args.max_tokens.unwrap_or(10_000),
            cancel,
            stop,
        )
    }
    fn observe(
        &self,
        id: &str,
        cell: Arc<Cell>,
        ms: u64,
        tokens: usize,
        cancel: &CancellationToken,
        stop_wait: &CancellationToken,
    ) -> Result<Output> {
        let interrupted = self.services.shell.runtime().block_on(async {
            let deadline = tokio::time::Instant::now() + Duration::from_millis(ms.min(300_000));
            let mut interrupted = false;
            loop {
                let notified = cell.state.1.notified();
                {
                    let state = cell.state.0.lock().expect("cell state");
                    if state.done || (!interrupted && state.yielded) {
                        break;
                    }
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled(), if !interrupted => {
                        cell.cancel.cancel();
                        interrupted = true;
                        // Wait for the cell to finish emitting before draining its output.
                        // Returning an error here would discard buffered text and media.
                    }
                    _ = stop_wait.cancelled(), if !interrupted => break,
                    _ = tokio::time::sleep_until(deadline), if !interrupted => break,
                    _ = notified => {}
                }
            }
            interrupted
        });
        let mut state = cell.state.0.lock().expect("cell state");
        if stop_wait.is_cancelled() && !interrupted {
            bail!("Wait stopped. Use wait with cell_id {id} to collect remaining output.");
        }
        let result = (|| {
            state.yielded = false;
            let mut budget = character_budget(tokens);
            let mut pending = Vec::new();
            for part in &mut state.parts {
                pending.push(match part {
                    BufferedPart::Text(buffer) => {
                        let text = buffer.prepare_read(budget).with_context(|| {
                            format!("Could not return cell output; retry wait with cell_id {id}")
                        })?;
                        budget = budget.saturating_sub(text.chars().count());
                        Part::InputText { text }
                    }
                    BufferedPart::Media(media) => Part::OrbitMedia {
                        media: media.clone(),
                    },
                });
            }
            let output = state.output.prepare_read(budget).with_context(|| {
                format!("Could not return cell output; retry wait with cell_id {id}")
            })?;
            if !output.is_empty() {
                pending.push(Part::InputText { text: output });
            }
            state.parts.clear();
            state.output = OutputBuffer::default();
            let status = if state.done {
                self.cells.lock().expect("cell lock").remove(id);
                match &state.error {
                    Some(e) => format!(
                        "Script failed: {}",
                        crate::output::preserve_text(e, character_budget(tokens))
                    ),
                    None if interrupted => "Script cancelled".into(),
                    None => "Script completed".into(),
                }
            } else {
                format!("Script running with cell ID {id}")
            };
            let mut parts = vec![Part::InputText {
                text: format!("{status}\nOutput:"),
            }];
            parts.extend(pending);
            Ok(Output {
                parts,
                failed: interrupted || state.error.is_some(),
                cell_id: Some(id.to_owned()),
            })
        })();
        drop(state);
        if result.is_err() {
            // Stop may have removed the ID while this observation was active. Failed
            // output preservation must still leave its unread buffers available to wait.
            self.cells
                .lock()
                .expect("cell lock")
                .insert(id.to_owned(), cell);
        }
        result
    }

    pub(crate) fn shell(&self) -> &Shell {
        &self.services.shell
    }
    pub(crate) fn cells_executing(&self) -> bool {
        self.active_cells.load(Ordering::Acquire) > 0 || self.workers.load(Ordering::Acquire) > 0
    }
    pub fn is_executing(&self) -> bool {
        self.cells_executing() || self.shell().is_executing()
    }
    pub fn has_work(&self) -> bool {
        !self.cells.lock().expect("cell lock").is_empty()
            || self.services.shell.has_work()
            || !self.stored.lock().expect("store lock").values.is_empty()
    }
    pub fn stop_all(&self) {
        for (_, cell) in self.cells.lock().expect("cell lock").drain() {
            cell.cancel.cancel();
        }
        self.services.shell.stop_all();
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
#[derive(Clone)]
struct Services {
    shell: Arc<Shell>,
    delegation: Option<Delegation>,
    questionnaires: Option<Arc<crate::questionnaire::Questionnaires>>,
    images: Option<Arc<ImageTools>>,
    web: Option<Arc<crate::web_search::WebSearch>>,
}
fn run_cell(
    code: String,
    Services {
        shell,
        delegation,
        questionnaires,
        images,
        web,
    }: Services,
    stored: Arc<Mutex<Stored>>,
    observer: Observer,
    parent: String,
    workers: Arc<AtomicUsize>,
    cell: &Cell,
) -> Result<()> {
    let executor = shell.runtime().clone();
    let runtime = Runtime::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    runtime.set_memory_limit(256 * 1024 * 1024);
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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, String, bool, bool)>();
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
        let media_state=cell.state.clone();
        let media_tools=images.clone();
        ctx.globals().set("__media", Function::new(ctx.clone(), move |url:String,audio:bool|->rquickjs::Result<()> {
            let result=(||->Result<()> {
                if media_state.0.lock().expect("cell state").parts.iter().filter(|p|matches!(p,BufferedPart::Media(_))).count()>=64 {bail!("64 media items already emitted; yield or complete this cell before emitting more");}
                let tools=media_tools.as_ref().context("Media storage is unavailable in this runtime")?;
                let media=tools.media.from_url(&url,audio)?;
                if audio && crate::media::short_wav(&media.path)? {
                    let mut state=media_state.0.lock().expect("cell state");
                    if !state.output.is_empty() {state.output.append(b"\n",4*1024*1024);}
                    state.output.append(b"Audio output omitted because the clip is shorter than 25 ms; use a longer clip.",4*1024*1024);
                    return Ok(());
                }
                if audio && !tools.audio_supported()? {bail!("This chat model does not support audio input. Audio was preserved at {}; use text or select a model that supports audio.",media.path.display());}
                let mut state=media_state.0.lock().expect("cell state");
                if !state.output.is_empty() {let buffer=std::mem::take(&mut state.output);state.parts.push(BufferedPart::Text(buffer));}
                let note=crate::image_tools::animation_notice(&media);
                state.parts.push(BufferedPart::Media(media.clone()));
                if !note.is_empty() {state.output.append(note.as_bytes(),4*1024*1024);}
                media_state.1.notify_one();
                Ok(())
            })();
            result.map_err(|e|rquickjs::Error::new_from_js_message("media","output",format!("{e:#}")))
        })).map_err(js_error)?;
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
        let definitions = catalog::definitions(Capabilities {
            delegation: delegation.is_some(),
            web: web.is_some(),
            questionnaires: questionnaires.is_some(),
            images: images.is_some(),
            image_generation: images.as_ref().is_some_and(|i| i.generation),
        });
        ctx.globals().set("__tools", serde_json::to_string(&definitions)?).map_err(js_error)?;
        let cancellation = cell.cancel.clone();
        let calls = cell.calls.clone();
        ctx.globals()
            .set(
                "__call",
                Function::new(
                    ctx.clone(),
                    move |name: String, args: String| -> rquickjs::Result<String> {
                        if !definitions.iter().any(|tool| tool.name == name) {
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
                        let images = images.clone();
                        let web = web.clone();
                        let tx = tx.clone();
                        let observer = observer.clone();
                        // A yielded command and its panel outlive the starting cell.
                        let token = if name == "exec_command" {
                            CancellationToken::new()
                        } else {
                            cancellation.child_token()
                        };
                        let mut live = calls.as_ref().map(|calls| calls.register(request.clone(), &token));
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
                                    failed: false,
                                    media: vec![],
                                    web: None,
                                }));
                                let mut attachments=Vec::new();
                                let mut web_metadata=None;
                                let mut display=None;
                                let mut command_failed=false;
                                let structured=matches!(name.as_str(),"view_image"|"image_gen__imagegen");
                                let result = started.and_then(|()| {
                                    if token.is_cancelled() || cancellation.is_cancelled() { bail!("Tool call cancelled before dispatch"); }
                                    match name.as_str() {
                                    "web__run" => shell.runtime().block_on(web.as_ref().expect("validated web tool").call(&args,&token)).map(|result| {web_metadata=Some(result.metadata);result.text}).inspect_err(|error| {if let Some(failure)=error.downcast_ref::<crate::web_search::Failure>() {web_metadata=Some(failure.metadata.clone());}}),
                                    "view_image"|"image_gen__imagegen" => shell.runtime().block_on(images.as_ref().expect("validated image tool").call(&name,&args,&request,&token)).map(|result| {attachments.push(result.media);display=Some(result.text);result.value.to_string()}),
                                    "exec_command" => serde_json::from_str(&args)
                                        .map_err(anyhow::Error::from)
                                        .and_then(|a| {
                                            let observer = observer.clone();
                                            let id = request.clone();
                                            let command = live.take();
                                            shell.exec_observed(a, &cancellation, Some((&token, Arc::new(move |status| {
                                                // Keep the command stoppable after its invocation yields.
                                                let _keep_alive = &command;
                                                observer(Event::ExecutionStatus { id: id.clone(), status })
                                            }))))
                                        }).map(|result| {command_failed=result.failed;result.text}),
                                    "write_stdin" => serde_json::from_str(&args)
                                        .map_err(anyhow::Error::from)
                                        .and_then(|a| shell.write(a, &token)).map(|result| {command_failed=result.failed;result.text}),
                                    "ask_user" => shell.runtime().block_on(questionnaires.as_ref().expect("validated questionnaire tool").ask(&request, &args, &token)),
                                    _ => delegation.as_ref().expect("validated delegation tool")(&name, &args, &request, &token),
                                }});
                                drop(live);
                                let failed = result.is_err();
                                let output = result.unwrap_or_else(|e| format!("{e:#}"));
                                let name_for_output = name.clone();
                                let saved = observer(Event::Tool(ToolEvent {
                                    id: request.clone(),
                                    parent,
                                    name,
                                    input: args,
                                    output: Some(display.clone().unwrap_or_else(||output.clone())),
                                    failed: failed || command_failed,
                                    media: attachments,
                                    web: web_metadata,
                                }));
                                let (output, failed) = match saved { Ok(()) => (output,failed), Err(e) => (format!("{}\nCould not save tool result: {e:#}. Side effects may have occurred; do not blindly repeat the command.",display.unwrap_or_else(||output.clone())),true) };
                                let output = if name_for_output == "ask_user" { crate::output::preserve_text(&output, 40_000) } else { output };
                                let _ = tx.send((request, output, failed, structured));
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
            if let Some((id,output,failed,structured))=completion {
                ctx.globals().get::<_,Function>("__deliver").map_err(js_error)?.call::<_,()>((id,output,failed,structured)).map_err(|e|exception(&ctx,e))?;
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
    #[test]
    fn transcript_outcomes_distinguish_native_command_and_script_failures() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let mode = CodeMode::new(
            Arc::new(Shell::new(std::env::temp_dir())),
            Arc::new(move |event| {
                observed.lock().unwrap().push(event);
                Ok(())
            }),
        );
        let cancel = CancellationToken::new();
        let result = mode.exec_output("try { await tools.exec_command({bad:true}); } catch(e) {} text(await tools.exec_command({cmd:'exit 7',login:false}));", "outer", &cancel).unwrap();
        assert!(
            !result.failed,
            "caught native failure and nonzero shell exit do not change JS semantics"
        );
        let observed = events.lock().unwrap();
        let finished: Vec<_> = observed
            .iter()
            .filter_map(|e| match e {
                Event::Tool(e) if e.output.is_some() => Some(e),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 2);
        assert!(finished.iter().all(|e| e.failed));
        drop(observed);
        assert!(
            mode.exec_output("throw new Error('before any tool');", "pure", &cancel)
                .unwrap()
                .failed
        );
        let result = mode
            .exec_output(
                "// @exec: {\"yield_time_ms\":1}\nawait new Promise(() => {});",
                "yield",
                &cancel,
            )
            .unwrap();
        assert!(!result.failed);
        assert!(result.summary().starts_with("Script running"));
        mode.stop_all();
        let yielded = mode.exec_output("text(await tools.exec_command({cmd:'printf ready',login:false})); await yield_control(); while (!load('resume-tools')) await new Promise(r => setTimeout(r, 1));", "yielded-tools", &cancel).unwrap();
        let cell = yielded.cell_id.as_ref().unwrap();
        assert!(yielded.summary().starts_with("Script running"));
        mode.exec("store('resume-tools', true);", "resume", &cancel)
            .unwrap();
        let waited = mode
            .wait_output(&json!({"cell_id":cell}).to_string(), &cancel)
            .unwrap();
        assert_eq!(waited.cell_id.as_ref(), Some(cell));
        assert!(!waited.failed);
        assert!(waited.summary().starts_with("Script completed"));
    }

    fn mode() -> CodeMode {
        CodeMode::new(
            Arc::new(Shell::new(std::env::temp_dir())),
            Arc::new(|_| Ok(())),
        )
    }
    #[test]
    fn media_helpers_preserve_order_yield_and_reject_bad_content_without_losing_prior_items() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("state")).unwrap();
        let session =
            crate::store::Session::new(dir.path().into(), &crate::store::Settings::default());
        let session_id = session.id.clone();
        let tools = Arc::new(ImageTools::new(
            store.clone(),
            crate::auth::Auth::new(store).unwrap(),
            dir.path().into(),
            &session_id,
            false,
            Arc::new(move || Ok(session.clone())),
            ImageTools::client().unwrap(),
        ));
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let mode = CodeMode::new(
            Arc::new(Shell::new(dir.path().into())),
            Arc::new(move |event| {
                captured.lock().unwrap().push(event);
                Ok(())
            }),
        )
        .with_images(tools.clone());
        std::fs::write(dir.path().join("source.png"), crate::media::tests::png()).unwrap();
        let cancel = CancellationToken::new();
        let source = r#"text('before'); const result=await tools.view_image({path:'source.png'}); store('viewed',result); image(result); text('after'); text(typeof tools.image_gen__imagegen);"#;
        let out = mode.exec_output(source, "parent", &cancel).unwrap();
        assert_eq!(out.media().len(), 1);
        let image_index = out
            .parts
            .iter()
            .position(|part| matches!(part, Part::OrbitMedia { .. }))
            .unwrap();
        let text = |parts: &[Part]| {
            parts
                .iter()
                .filter_map(|part| match part {
                    Part::InputText { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(text(&out.parts[..image_index]).ends_with("before"));
        assert_eq!(text(&out.parts[image_index + 1..]), "after\nundefined");
        let events = events.lock().unwrap();
        let finished = events
            .iter()
            .find_map(|e| match e {
                Event::Tool(t) if t.output.is_some() => Some(t),
                _ => None,
            })
            .unwrap();
        assert_eq!(finished.media.len(), 1);
        assert!(!finished.output.as_ref().unwrap().contains("base64"));
        drop(events);
        let out = mode
            .exec_output(
                "content([{type:'text',text:'retained'}, {type:'image',data:'not-base64'}]);",
                "bad",
                &cancel,
            )
            .unwrap();
        assert!(out.summary().contains("Script failed"));
        assert!(out.summary().contains("retained"));
        let out = mode
            .exec_output(
                "generatedImage({...load('viewed'),output_hint:'saved hint'});",
                "generated",
                &cancel,
            )
            .unwrap();
        assert_eq!(out.media().len(), 1);
        assert!(out.summary().ends_with("saved hint"));
        let out=mode.exec_output("// @exec: {\"yield_time_ms\": 1, \"max_output_tokens\": 0}\nimage(load('viewed'));text('first text');await yield_control();while (!load('resume-media')) await new Promise(r=>setTimeout(r,1));image(load('viewed'));text('second text');","yield",&cancel).unwrap();
        mode.exec("store('resume-media', true);", "resume", &cancel)
            .unwrap();
        let mut all = out.media().len();
        let summary = out.summary();
        let id = summary
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        loop {
            let next = mode
                .wait_output(&json!({"cell_id":id,"max_tokens":0}).to_string(), &cancel)
                .unwrap();
            all += next.media().len();
            if next.summary().starts_with("Script completed") {
                break;
            }
        }
        assert_eq!(all, 2);
        let out = mode
            .exec_output("image('https://example.com/x.png');", "remote", &cancel)
            .unwrap();
        assert!(out.summary().contains("Script failed"));
        let out = mode
            .exec_output("audio('data:audio/wav;base64,AQID');", "audio", &cancel)
            .unwrap();
        assert!(out.summary().contains("does not support audio"));
        assert!(out.media().is_empty());
        assert!(out.summary().contains("preserved at"));
        let out = mode
            .exec_output("__call('image_gen__imagegen','{}');", "forbidden", &cancel)
            .unwrap();
        assert!(out.summary().contains("Unknown nested tool"));
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
    fn stopped_wait_does_not_reopen_a_cell_removed_by_session_stop() {
        let mode = mode();
        let cancel = CancellationToken::new();
        let id = mode
            .exec_output(
                "await yield_control(); await new Promise(() => {});",
                "outer",
                &cancel,
            )
            .unwrap()
            .cell_id
            .unwrap();
        // This wait already resolved its ID when the panel and session stops arrived.
        let waiting = mode.cells.lock().unwrap()[&id].clone();
        let stop = CancellationToken::new();
        stop.cancel();
        mode.stop_all();
        let err = mode
            .observe(&id, waiting, 0, 100, &cancel, &stop)
            .unwrap_err();
        assert!(err.to_string().contains("Wait stopped"));
        let next = mode.wait(&json!({"cell_id":id}).to_string(), &cancel);
        assert!(next.unwrap_err().to_string().contains("already closed"));
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
    fn cell_output_preserves_large_unicode_writes_in_order() {
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
    fn pre_cancelled_calls_do_not_run_or_exhaust_cell_slots() {
        let mode = mode();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        // Cross the open-cell limit: rejecting a call must not leak its slot.
        for _ in 0..40 {
            assert!(mode.exec("store('ran', true);", "p", &cancelled).is_err());
        }
        let result = mode
            .exec(
                "text(load('ran') === true ? 'ran' : 'skipped');",
                "p",
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(result.contains("skipped"));
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
        // Explicitly hand off the first batch; a five-millisecond deadline can
        // elapse before the JS worker even starts on a busy test runner.
        let out = mode.exec("text('first'); await yield_control(); while (!load('resume')) await new Promise(r=>setTimeout(r,1)); text('last');", "p", &cancel).unwrap();
        assert!(out.contains("first"));
        assert!(!out.contains("last"));
        let id = out
            .lines()
            .next()
            .unwrap()
            .strip_prefix("Script running with cell ID ")
            .unwrap();
        mode.exec("store('resume', true);", "resume", &cancel)
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
