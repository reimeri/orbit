//! Codex-style command sessions, scoped to one conversation. No approval/sandbox layer.
use crate::output::{OutputBuffer, character_budget};
use anyhow::{Context, Result, bail};
use async_io::Async;
use filedescriptor::FileDescriptor;
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI32, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const BUFFER_LIMIT: usize = 1024 * 1024;
fn exec_yield() -> u64 {
    10_000
}
fn stdin_yield() -> u64 {
    250
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecArgs {
    pub cmd: String,
    pub workdir: Option<PathBuf>,
    pub shell: Option<String>,
    pub login: Option<bool>,
    #[serde(default)]
    pub tty: bool,
    #[serde(default = "exec_yield")]
    pub yield_time_ms: u64,
    pub max_output_tokens: Option<usize>,
    pub timeout_ms: Option<u64>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StdinArgs {
    pub session_id: i32,
    #[serde(default)]
    pub chars: String,
    #[serde(default = "stdin_yield")]
    pub yield_time_ms: u64,
    pub max_output_tokens: Option<usize>,
}
/// Process lifetime, independent of whether an exec_command invocation has returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandStatus {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}
pub type CommandObserver = Arc<dyn Fn(CommandStatus) -> Result<()> + Send + Sync>;

pub struct CommandOutput {
    pub text: String,
    pub failed: bool,
}

#[derive(Default)]
struct Output {
    buffer: OutputBuffer,
    exit: Option<(String, bool)>,
    stopped: bool,
    readers: usize,
}
enum Read<'a> {
    Start(&'a CancellationToken),
    Poll,
    AfterWrite,
}
struct Process {
    output: Arc<(Mutex<Output>, tokio::sync::Notify)>,
    stdin: Option<tokio::sync::Mutex<Arc<Async<FileDescriptor>>>>,
    runtime: tokio::runtime::Handle,
    pid: u32,
    started: Instant,
}
impl Process {
    fn is_running(&self) -> bool {
        let state = self.output.0.lock().expect("output lock");
        state.exit.is_none() || state.readers > 0
    }
    async fn wait_for_exit(&self) {
        loop {
            let notified = self.output.1.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.is_running() {
                return;
            }
            notified.await;
        }
    }
    fn stop(&self) -> bool {
        let mut state = self.output.0.lock().expect("output lock");
        if state.stopped || (state.exit.is_some() && state.readers == 0) {
            return false;
        }
        state.stopped = true;
        let _ = killpg(Pid::from_raw(self.pid as i32), Signal::SIGKILL);
        true
    }
    fn read(
        &self,
        id: i32,
        wait: u64,
        tokens: usize,
        cancel: &CancellationToken,
        read: Read<'_>,
    ) -> Result<(CommandOutput, bool)> {
        self.runtime.block_on(async {
            let deadline = tokio::time::sleep(Duration::from_millis(wait.min(300_000)));
            if let Read::Start(panel_stop) = read {
                tokio::select! {
                    biased;
                    _ = self.wait_for_exit() => return,
                    _ = cancel.cancelled() => { self.stop(); }
                    // The process observer owns this signal, even after yield.
                    _ = panel_stop.cancelled() => {}
                    _ = deadline => return,
                }
                // An escaped descendant can keep a pipe open. Return the session ID
                // rather than blocking the invocation or losing the remaining bytes.
                let _ = tokio::time::timeout(Duration::from_secs(3), self.wait_for_exit()).await;
            } else {
                tokio::select! {
                    _ = cancel.cancelled() => {}
                    _ = self.wait_for_exit() => {}
                    _ = deadline => {}
                }
            }
        });
        let mut output = self.output.0.lock().expect("process output lock");
        if !matches!(read, Read::Start(_)) && cancel.is_cancelled() {
            bail!(
                "Wait stopped. Use write_stdin with session_id {id} to collect remaining output."
            );
        }
        let complete = output.exit.is_some() && output.readers == 0;
        let exit = output.exit.clone();
        let stopped = output.stopped;
        let body = output
            .buffer
            .read(character_budget(tokens))
            .with_context(|| {
                format!("Could not return command output. Retry write_stdin with session_id {id}")
            })?;
        drop(output);
        let status = if complete {
            format!(
                "Process exited with code {}",
                exit.as_ref().expect("complete process").0
            )
        } else {
            format!("Process running with session ID {id}")
        };
        let hint = if stopped && !complete {
            "\nStop requested. Poll this session for remaining output."
        } else if stopped {
            "\nCommand stopped. Side effects already performed are not undone."
        } else if matches!(read, Read::Poll) && !complete && body.is_empty() {
            "\nNo new output. Do independent work before polling again. If you only need the exit result, set yield_time_ms to 300000 on both write_stdin and the enclosing @exec cell; the wait returns early on exit. Use shorter polls for intermediate output."
        } else {
            ""
        };
        Ok((
            CommandOutput {
                text: format!(
                    "Wall time: {:.3} seconds\n{status}{hint}\nOutput:\n{body}",
                    self.started.elapsed().as_secs_f64()
                ),
                failed: complete && (stopped || exit.as_ref().is_some_and(|(_, failed)| *failed)),
            },
            complete,
        ))
    }
}
type SharedOutput = Arc<(Mutex<Output>, tokio::sync::Notify)>;
fn append(output: &SharedOutput, bytes: &[u8]) {
    let mut state = output.0.lock().expect("output lock");
    state.buffer.append(bytes, BUFFER_LIMIT);
    // Readers do not need to wake consumers until exit: reads yield on their deadline.
}
fn finish_reader(output: &SharedOutput, error: Option<std::io::Error>) {
    if let Some(e) = error
        && e.raw_os_error() != Some(5)
    {
        append(output, format!("\n[Output read failed: {e}]").as_bytes());
    }
    output.0.lock().expect("output lock").readers -= 1;
    output.1.notify_waiters();
}
async fn pipe_reader(mut source: impl tokio::io::AsyncRead + Unpin, output: SharedOutput) {
    use tokio::io::AsyncReadExt;
    let mut buffer = [0; 8192];
    let error = loop {
        match source.read(&mut buffer).await {
            Ok(0) => break None,
            Ok(n) => append(&output, &buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => break Some(e),
        }
    };
    finish_reader(&output, error);
}
async fn pty_reader(source: Arc<Async<FileDescriptor>>, output: SharedOutput) {
    let mut buffer = [0; 8192];
    let error = loop {
        // A continuously readable PTY must still yield to other Tokio tasks.
        tokio::task::consume_budget().await;
        match source
            .read_with(|fd| Ok(rustix::io::read(fd, &mut buffer[..])?))
            .await
        {
            Ok(0) => break None,
            Ok(n) => append(&output, &buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => break Some(e),
        }
    };
    finish_reader(&output, error);
}
fn shared_runtime() -> tokio::runtime::Handle {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("execution runtime")
        })
        .handle()
        .clone()
}

// Keep the conversation resident until its final process status has been saved,
// even if a concurrent poll already removed the process from the session map.
struct PendingStatus(Arc<AtomicUsize>);
impl Drop for PendingStatus {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

pub struct Shell {
    pending_status: Arc<AtomicUsize>,
    cwd: PathBuf,
    processes: Mutex<HashMap<i32, Arc<Process>>>,
    next: AtomicI32,
    runtime: tokio::runtime::Handle,
}
impl Shell {
    pub fn new(cwd: PathBuf) -> Self {
        Self::with_runtime(cwd, shared_runtime())
    }
    pub fn with_runtime(cwd: PathBuf, runtime: tokio::runtime::Handle) -> Self {
        Self {
            pending_status: Arc::new(AtomicUsize::new(0)),
            runtime,
            cwd,
            processes: Mutex::new(HashMap::new()),
            next: AtomicI32::new(rand::random_range(1_000..i32::MAX / 2)),
        }
    }
    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }
    pub fn exec(&self, args: ExecArgs, cancel: &CancellationToken) -> Result<CommandOutput> {
        self.exec_observed(args, cancel, None)
    }
    pub(crate) fn exec_observed(
        &self,
        args: ExecArgs,
        cancel: &CancellationToken,
        tracking: Option<(&CancellationToken, CommandObserver)>,
    ) -> Result<CommandOutput> {
        let stop = tracking.as_ref().map_or(cancel, |(stop, _)| *stop);
        if cancel.is_cancelled() || stop.is_cancelled() {
            bail!("Execution cancelled");
        }
        let mut processes = self.processes.lock().expect("process lock");
        if cancel.is_cancelled() || stop.is_cancelled() {
            bail!("Execution cancelled");
        }
        if processes.len() >= 64 {
            bail!(
                "64 command sessions are open. Poll or stop existing sessions before starting more."
            );
        }
        let cwd = args
            .workdir
            .map(|p| self.cwd.join(p))
            .unwrap_or_else(|| self.cwd.clone());
        let shell = args
            .shell
            .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()));
        let flag = if args.login.unwrap_or(true) {
            "-lc"
        } else {
            "-c"
        };
        let output = Arc::new((
            Mutex::new(Output {
                readers: if args.tty { 1 } else { 2 },
                ..Output::default()
            }),
            tokio::sync::Notify::new(),
        ));
        let process = if args.tty {
            let pair = native_pty_system().openpty(PtySize {
                rows: 24,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            let mut cmd = CommandBuilder::new(shell);
            cmd.args([flag, &args.cmd]);
            cmd.cwd(cwd);
            cmd.env("TERM", "xterm-256color");
            // Register one owned duplicate of the master, shared by the reader and writer.
            let master_fd = pair
                .master
                .as_raw_fd()
                .context("PTY has no Unix descriptor")?;
            let pty = Arc::new(Async::new(FileDescriptor::dup(&master_fd)?)?);
            let mut child = pair.slave.spawn_command(cmd).context("Start PTY command")?;
            let pid = match child.process_id() {
                Some(pid) => pid,
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("Missing PTY process id");
                }
            };
            self.runtime.spawn(pty_reader(pty.clone(), output.clone()));
            drop(pair.slave);
            let state = output.clone();
            thread::spawn(move || {
                let status = child
                    .wait()
                    .map(|s| (s.exit_code().to_string(), !s.success()))
                    .unwrap_or_else(|e| (format!("unknown ({e})"), true));
                state.0.lock().expect("output lock").exit = Some(status);
                state.1.notify_waiters();
                drop(pair.master);
            });
            Arc::new(Process {
                output,
                stdin: Some(tokio::sync::Mutex::new(pty)),
                runtime: self.runtime.clone(),
                pid,
                started: Instant::now(),
            })
        } else {
            let _runtime = self.runtime.enter();
            let mut child = tokio::process::Command::new(shell)
                .args([flag, &args.cmd])
                .current_dir(cwd)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Start shell command")?;
            let pid = child.id().expect("spawned child");
            self.runtime.spawn(pipe_reader(
                child.stdout.take().expect("piped stdout"),
                output.clone(),
            ));
            self.runtime.spawn(pipe_reader(
                child.stderr.take().expect("piped stderr"),
                output.clone(),
            ));
            let state = output.clone();
            self.runtime.spawn(async move {
                let status = child
                    .wait()
                    .await
                    .map(|s| {
                        (
                            s.code()
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "signal".into()),
                            !s.success(),
                        )
                    })
                    .unwrap_or_else(|e| (format!("unknown ({e})"), true));
                state.0.lock().expect("output lock").exit = Some(status);
                state.1.notify_waiters();
            });
            Arc::new(Process {
                output,
                stdin: None,
                runtime: self.runtime.clone(),
                pid,
                started: Instant::now(),
            })
        };
        if let Some(timeout) = args.timeout_ms {
            let process = process.clone();
            self.runtime.spawn(async move {
                if tokio::time::timeout(
                    Duration::from_millis(timeout.min(86_400_000)),
                    process.wait_for_exit(),
                )
                .await
                .is_err()
                {
                    process.stop();
                }
            });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        processes.insert(id, process.clone());
        drop(processes);
        if let Some((_, observer)) = tracking {
            self.pending_status.fetch_add(1, Ordering::AcqRel);
            let pending = PendingStatus(self.pending_status.clone());
            if let Err(error) = observer(CommandStatus::Running) {
                process.stop();
                self.processes.lock().expect("process lock").remove(&id);
                return Err(error);
            }
            let watched = process.clone();
            let stop = stop.clone();
            self.runtime.spawn(async move {
                let _pending = pending;
                let interrupted = tokio::select! {
                    biased;
                    _ = watched.wait_for_exit() => false,
                    _ = stop.cancelled() => {
                        let interrupted = watched.stop();
                        watched.wait_for_exit().await;
                        interrupted
                    }
                };
                let status = {
                    let state = watched.output.0.lock().expect("output lock");
                    if interrupted {
                        CommandStatus::Interrupted
                    } else if state.stopped || state.exit.as_ref().expect("exited command").1 {
                        CommandStatus::Failed
                    } else {
                        CommandStatus::Succeeded
                    }
                };
                if let Err(error) = observer(status) {
                    eprintln!("Could not record command exit: {error:#}");
                }
            });
        }
        let result = process.read(
            id,
            args.yield_time_ms,
            args.max_output_tokens.unwrap_or(10_000),
            cancel,
            Read::Start(stop),
        );
        // This is the handoff from the starting cell to the yielded process.
        // Honour cancellation even if preserving the output failed.
        let cancelled = cancel.is_cancelled();
        if cancelled {
            process.stop();
        }
        let (output, complete) = result?;
        if complete {
            self.processes.lock().expect("process lock").remove(&id);
        }
        if cancelled && (!complete || output.failed) {
            bail!("Command cancelled. {}", output.text);
        }
        Ok(output)
    }
    pub fn write(&self, args: StdinArgs, cancel: &CancellationToken) -> Result<CommandOutput> {
        let process = self
            .processes
            .lock()
            .expect("process lock")
            .get(&args.session_id)
            .cloned()
            .context("Unknown or already closed command session; start a new command")?;
        if !args.chars.is_empty() {
            let stdin = process.stdin.as_ref().context(
                "stdin is closed for this session; rerun exec_command with tty=true for interactive input",
            )?;
            self.runtime.block_on(async {
                let stdin = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => bail!("Input cancelled before writing"),
                    lock = stdin.lock() => lock,
                };
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                let mut remaining = args.chars.as_bytes();
                while !remaining.is_empty() {
                    let written = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => bail!("Input cancelled; some bytes may already have been sent. Poll session {} before sending more.", args.session_id),
                        _ = tokio::time::sleep_until(deadline) => bail!("PTY input stalled; {} bytes were not sent. Poll the session before sending more.", remaining.len()),
                        result = stdin.write_with(|fd| Ok(rustix::io::write(fd, remaining)?)) => result,
                    };
                    match written {
                        Ok(0) => bail!("PTY stdin closed"),
                        Ok(n) => remaining = &remaining[n..],
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e.into()),
                    }
                }
                Ok(())
            })?;
        }
        let (output, complete) = process.read(
            args.session_id,
            args.yield_time_ms,
            args.max_output_tokens.unwrap_or(10_000),
            cancel,
            if args.chars.is_empty() {
                Read::Poll
            } else {
                Read::AfterWrite
            },
        )?;
        if complete {
            self.processes
                .lock()
                .expect("process lock")
                .remove(&args.session_id);
        }
        Ok(output)
    }
    pub fn is_executing(&self) -> bool {
        self.pending_status.load(Ordering::Acquire) > 0 || self.has_running_commands()
    }
    pub(crate) fn has_running_commands(&self) -> bool {
        self.processes
            .lock()
            .expect("process lock")
            .values()
            .any(|p| p.is_running())
    }
    /// Signal only live groups. Keep the session handles and their unread output.
    pub(crate) fn stop_running_commands(&self) -> impl Future<Output = Result<()>> + use<> {
        let running: Vec<_> = self
            .processes
            .lock()
            .expect("process lock")
            .values()
            .filter(|p| p.is_running())
            .cloned()
            .collect();
        for process in &running {
            process.stop();
        }
        async move {
            tokio::time::timeout(Duration::from_secs(3), async {
                for process in running {
                    process.wait_for_exit().await;
                }
            })
            .await
            .context("Commands have not finished stopping.")
        }
    }
    pub fn has_work(&self) -> bool {
        !self.processes.lock().expect("process lock").is_empty()
    }
    pub fn stop_all(&self) {
        for (_, process) in self.processes.lock().expect("process lock").drain() {
            process.stop();
        }
    }
}
impl Drop for Shell {
    fn drop(&mut self) {
        self.stop_all();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn running_id(output: &str) -> i32 {
        output
            .lines()
            .find_map(|line| line.strip_prefix("Process running with session ID "))
            .expect("running command")
            .parse()
            .unwrap()
    }
    #[test]
    fn rejected_running_checkpoint_does_not_leave_a_handle_or_publish_exit() {
        let shell = Shell::new(std::env::temp_dir());
        let (tx, rx) = std::sync::mpsc::channel();
        let args =
            serde_json::from_value(serde_json::json!({"cmd":"exit 0","login":false})).unwrap();
        let result = shell.exec_observed(
            args,
            &CancellationToken::new(),
            Some((
                &CancellationToken::new(),
                Arc::new(move |status| {
                    tx.send(status).unwrap();
                    anyhow::bail!("cannot publish")
                }),
            )),
        );
        assert!(result.is_err());
        assert!(!shell.has_work());
        assert_eq!(rx.into_iter().collect::<Vec<_>>(), [CommandStatus::Running]);
    }
    #[test]
    fn command_status_includes_children_retaining_output_after_shell_exit() {
        let dir = tempfile::tempdir().unwrap();
        let shell = Shell::new(dir.path().into());
        let (events, received) = std::sync::mpsc::channel();
        let result = shell
            .exec_observed(
                serde_json::from_value(serde_json::json!({
                    "cmd": "while [ ! -f release ]; do sleep 0.01; done &",
                    "login": false, "yield_time_ms": 0
                }))
                .unwrap(),
                &CancellationToken::new(),
                Some((
                    &CancellationToken::new(),
                    Arc::new(move |status| {
                        events.send(status).unwrap();
                        Ok(())
                    }),
                )),
            )
            .unwrap();
        let id = running_id(&result.text);
        let process = shell.processes.lock().unwrap()[&id].clone();
        let deadline = Instant::now() + Duration::from_secs(5);
        // Establish that the shell itself exited successfully, but its child still
        // owns the output pipes, before stopping the session.
        loop {
            let output = process.output.0.lock().unwrap();
            if let Some((_, failed)) = output.exit {
                assert!(!failed);
                assert!(output.readers > 0);
                break;
            }
            drop(output);
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)).unwrap(),
            CommandStatus::Running
        );
        assert!(matches!(
            received.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        shell.stop_all();
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)).unwrap(),
            CommandStatus::Failed
        );
    }

    #[test]
    fn command_status_publication_keeps_execution_live_after_result_is_consumed() {
        let shell = Shell::new(std::env::temp_dir());
        let (events, received) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let gate = Mutex::new(gate);
        let output = shell
            .exec_observed(
                serde_json::from_value(serde_json::json!({"cmd": "exit 7", "login": false}))
                    .unwrap(),
                &CancellationToken::new(),
                Some((
                    &CancellationToken::new(),
                    Arc::new(move |status| {
                        events.send(status).unwrap();
                        if status != CommandStatus::Running {
                            gate.lock()
                                .unwrap()
                                .recv_timeout(Duration::from_secs(5))
                                .unwrap();
                        }
                        Ok(())
                    }),
                )),
            )
            .unwrap();
        assert!(output.failed);
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)).unwrap(),
            CommandStatus::Running
        );
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)).unwrap(),
            CommandStatus::Failed
        );
        assert!(
            !shell.has_work(),
            "the completed result was already consumed"
        );
        assert!(
            shell.is_executing(),
            "do not evict a transcript before the exit event is saved"
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while shell.is_executing() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn command_and_poll_preserve_oversized_output_after_session_closes() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        // Below the memory threshold, but above the default response budget.
        let text = format!("start\n{}\nend", "λ中".repeat(25_000));
        std::fs::write(dir.path().join("text"), &text).unwrap();
        let shell = Shell::new(dir.path().into());
        let first = shell
            .exec(
                serde_json::from_value(serde_json::json!({
                    "cmd": "cat text", "login": false
                }))
                .unwrap(),
                &cancel,
            )
            .unwrap();
        assert!(first.text.contains("Process exited with code 0"));
        assert_eq!(
            crate::test_support::saved_output(&first.text),
            text.as_bytes()
        );

        // Hold output until write_stdin, then exceed the old ring buffer with
        // asymmetric binary data, exercising both stdout and stderr capture paths.
        let bytes: Vec<_> = (0..1024 * 1024 + 17777)
            .map(|i| ((i * 17 + i / 29) % 251) as u8)
            .collect();
        std::fs::write(dir.path().join("binary"), &bytes).unwrap();
        let initial = shell
            .exec(
                serde_json::from_value(serde_json::json!({
                    "cmd": "while [ ! -f release ]; do sleep 0.01; done; cat binary >&2; exit 7",
                    "login": false, "yield_time_ms": 0
                }))
                .unwrap(),
                &cancel,
            )
            .unwrap();
        let session_id = running_id(&initial.text);
        std::fs::write(dir.path().join("release"), "").unwrap();
        let result = shell
            .write(
                StdinArgs {
                    session_id,
                    chars: String::new(),
                    yield_time_ms: 3000,
                    max_output_tokens: None,
                },
                &cancel,
            )
            .unwrap();
        assert!(result.text.contains("Process exited with code 7"));
        assert!(!shell.has_work());
        drop(shell);
        assert_eq!(crate::test_support::saved_output(&result.text), bytes);
        assert_eq!(
            crate::test_support::saved_output(&first.text),
            text.as_bytes()
        );
    }

    #[test]
    fn stopping_before_first_yield_preserves_output_for_the_invocation() {
        for stop_cell in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let shell = Shell::new(dir.path().into());
            let cell = CancellationToken::new();
            let panel = CancellationToken::new();
            let args = serde_json::from_value(serde_json::json!({
                "cmd": "printf 'before stop'; touch ready; exec sleep 30",
                "login": false, "yield_time_ms": 300000,
            }))
            .unwrap();
            let result = thread::scope(|scope| {
                let job = scope.spawn(|| {
                    shell.exec_observed(args, &cell, Some((&panel, Arc::new(|_| Ok(())))))
                });
                let deadline = Instant::now() + Duration::from_secs(5);
                while !dir.path().join("ready").exists() {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(1));
                }
                if stop_cell {
                    cell.cancel()
                } else {
                    panel.cancel()
                }
                job.join().unwrap()
            });
            if stop_cell {
                assert!(result.err().unwrap().to_string().contains("before stop"));
            } else {
                let output = result.unwrap();
                assert!(output.failed && output.text.contains("before stop"));
            }
            assert!(
                !shell.has_work(),
                "the initial read collected the stopped process"
            );
        }
    }

    #[test]
    fn cancelled_commands_release_slots_without_stop_all() {
        let shell = Arc::new(Shell::new(std::env::temp_dir()));
        for _ in 0..66 {
            let cancel = CancellationToken::new();
            let worker_cancel = cancel.clone();
            let worker = shell.clone();
            let job = thread::spawn(move || {
                worker.exec(
                    serde_json::from_value(serde_json::json!({"cmd":"sleep 30","login":false}))
                        .unwrap(),
                    &worker_cancel,
                )
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            while !shell.has_running_commands() {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            cancel.cancel();
            assert!(job.join().unwrap().is_err());
        }
        // More than a full set of cancelled commands must still leave room to run.
        assert!(
            shell
                .exec(
                    serde_json::from_value(
                        serde_json::json!({"cmd":"printf usable","login":false})
                    )
                    .unwrap(),
                    &CancellationToken::new()
                )
                .unwrap()
                .text
                .contains("usable")
        );
    }
    #[test]
    fn timeout_terminates_descendants_after_shell_leader_exits() {
        let shell = Shell::new(std::env::temp_dir());
        let out = shell.exec(serde_json::from_value(serde_json::json!({"cmd":"sleep 10 &", "login":false,"timeout_ms":30,"yield_time_ms":1000})).unwrap(), &CancellationToken::new()).unwrap();
        assert!(out.text.contains("Process exited"), "{}", out.text);
    }
    // Pin the devpts inode, not an open terminal. Its link disappears when the last
    // master closes, even if a concurrent test immediately reuses the PTY number.
    fn pty_slave_inode(shell: &Shell, session_id: i32) -> std::os::fd::OwnedFd {
        use rustix::fs::{Mode, OFlags, open};
        let pid = shell.processes.lock().unwrap()[&session_id].pid;
        open(
            format!("/proc/{pid}/fd/0"),
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap()
    }

    fn assert_pty_released(slave: std::os::fd::OwnedFd) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while rustix::fs::fstat(&slave).unwrap().st_nlink != 0 {
            assert!(Instant::now() < deadline, "PTY master was not released");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn blocked_pty_input_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let shell = Arc::new(Shell::new(dir.path().into()));
        let cancel = CancellationToken::new();
        let first = shell.exec(serde_json::from_value(serde_json::json!({"cmd":"stty -echo -icanon; dd bs=1 count=1 of=/dev/null 2>/dev/null && touch received; exec sleep 20", "tty":true,"login":false,"yield_time_ms":30})).unwrap(), &cancel).unwrap();
        let session_id = running_id(&first.text);
        let slave = pty_slave_inode(&shell, session_id);
        let worker = shell.clone();
        let token = cancel.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let result = worker.write(
                StdinArgs {
                    session_id,
                    chars: "x".repeat(2 * 1024 * 1024),
                    yield_time_ms: 10,
                    max_output_tokens: None,
                },
                &token,
            );
            tx.send(result).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.path().join("received").exists() {
            assert!(Instant::now() < deadline, "PTY received no input");
            thread::sleep(Duration::from_millis(2));
        }
        cancel.cancel();
        let error = rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .err()
            .unwrap();
        assert!(error.to_string().contains("cancelled"), "{error}");
        let poll = shell.write(
            StdinArgs {
                session_id,
                chars: String::new(),
                yield_time_ms: 0,
                max_output_tokens: None,
            },
            &CancellationToken::new(),
        );
        assert_eq!(
            running_id(&poll.unwrap().text),
            session_id,
            "stopping a writer does not own the process"
        );
        shell.stop_all();
        assert_pty_released(slave);
    }
    #[test]
    fn yielded_output_is_consumed_once_and_stderr_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let shell = Shell::new(dir.path().into());
        let cancel = CancellationToken::new();
        let first = shell.exec(serde_json::from_value(serde_json::json!({"cmd":"printf before; while [ ! -f release ]; do sleep 0.01; done; printf after >&2; exit 7","login":false,"yield_time_ms":20})).unwrap(), &cancel).unwrap();
        let session_id = running_id(&first.text);
        fn output(text: &str) -> &str {
            text.split_once("\nOutput:\n").unwrap().1
        }
        let mut before = output(&first.text).to_owned();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while before.len() < "before".len() {
            assert!(
                std::time::Instant::now() < deadline,
                "first marker never arrived"
            );
            let next = shell
                .write(
                    StdinArgs {
                        session_id,
                        chars: String::new(),
                        yield_time_ms: 20,
                        max_output_tokens: None,
                    },
                    &cancel,
                )
                .unwrap();
            assert!(!next.failed);
            before.push_str(output(&next.text));
        }
        assert_eq!(before, "before");
        let empty = shell
            .write(
                StdinArgs {
                    session_id,
                    chars: String::new(),
                    yield_time_ms: 0,
                    max_output_tokens: None,
                },
                &cancel,
            )
            .unwrap();
        assert!(empty.text.contains("write_stdin") && empty.text.contains("@exec"));
        assert_eq!(output(&empty.text), "");
        assert!(!first.text.contains("@exec"));

        std::fs::write(dir.path().join("release"), "").unwrap();
        let second = shell
            .write(
                StdinArgs {
                    session_id,
                    chars: String::new(),
                    yield_time_ms: 2000,
                    max_output_tokens: None,
                },
                &cancel,
            )
            .unwrap();
        assert!(second.failed);
        assert!(!first.failed);
        assert!(second.text.contains("code 7"));
        assert!(!second.text.contains("@exec"));
        assert_eq!(
            output(&second.text),
            "after",
            "the first marker must not replay"
        );
    }
    #[test]
    fn pty_accepts_input() {
        let dir = tempfile::tempdir().unwrap();
        let shell = Shell::new(dir.path().into());
        let cancel = CancellationToken::new();
        let first = shell.exec(serde_json::from_value(serde_json::json!({"cmd":"read value; printf 'received:%s' \"$value\"", "tty":true,"login":false,"yield_time_ms":10})).unwrap(), &cancel).unwrap();
        let session_id = running_id(&first.text);
        let slave = pty_slave_inode(&shell, session_id);
        let out = shell
            .write(
                StdinArgs {
                    session_id,
                    chars: "asymmetric\n".into(),
                    yield_time_ms: 2000,
                    max_output_tokens: None,
                },
                &cancel,
            )
            .unwrap();
        assert!(out.text.contains("received:asymmetric"));
        assert!(!out.failed);
        assert!(out.text.contains("code 0"));
        assert_pty_released(slave);
    }
}
