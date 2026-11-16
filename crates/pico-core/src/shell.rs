//! Codex-style command sessions, scoped to one conversation. No approval/sandbox layer.
use crate::output::{OutputBuffer, character_budget};
use anyhow::{Context, Result, bail};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::Deserialize;
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI32, Ordering},
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
pub struct CommandOutput {
    pub text: String,
    pub failed: bool,
}

#[derive(Default)]
struct Output {
    buffer: OutputBuffer,
    exit: Option<(String, bool)>,
    readers: usize,
}
struct Process {
    output: Arc<(Mutex<Output>, tokio::sync::Notify)>,
    stdin: Mutex<Option<tokio::io::unix::AsyncFd<filedescriptor::FileDescriptor>>>,
    runtime: tokio::runtime::Handle,
    pid: u32,
    started: Instant,
}
impl Process {
    fn stop(&self) {
        let state = self.output.0.lock().expect("output lock");
        if state.exit.is_some() && state.readers == 0 {
            return;
        }
        let _ = killpg(Pid::from_raw(self.pid as i32), Signal::SIGKILL);
    }
    fn read(
        &self,
        id: i32,
        wait: u64,
        tokens: usize,
        cancel: &CancellationToken,
    ) -> Result<(CommandOutput, bool)> {
        self.runtime.block_on(async {
            let deadline=tokio::time::Instant::now()+Duration::from_millis(wait.min(300_000));
            loop {
                let notified=self.output.1.notified();tokio::pin!(notified);notified.as_mut().enable();
                {let output=self.output.0.lock().expect("output lock");if output.exit.is_some()&&output.readers==0 {break;}}
                tokio::select! {
                    biased;
                    _=cancel.cancelled()=>{self.stop();bail!("Command cancelled. Side effects already performed are not undone.");}
                    _=tokio::time::sleep_until(deadline)=>break,
                    _=notified=>{}
                }
            }
            Ok::<_,anyhow::Error>(())
        })?;
        let mut output = self.output.0.lock().expect("process output lock");
        let complete = output.exit.is_some() && output.readers == 0;
        let exit = output.exit.clone();
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
        Ok((
            CommandOutput {
                text: format!(
                    "Wall time: {:.3} seconds\n{status}\nOutput:\n{body}",
                    self.started.elapsed().as_secs_f64()
                ),
                failed: complete && exit.as_ref().is_some_and(|(_, failed)| *failed),
            },
            complete,
        ))
    }
}
// The pinned Tokio only provides deprecated safe constructors. This private
// wrapper owns a FileDescriptor whose number cannot change during registration.
#[allow(deprecated)]
fn register_pty(
    fd: filedescriptor::FileDescriptor,
) -> std::io::Result<tokio::io::unix::AsyncFd<filedescriptor::FileDescriptor>> {
    tokio::io::unix::AsyncFd::new(fd)
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
async fn pty_reader(
    mut source: tokio::io::unix::AsyncFd<filedescriptor::FileDescriptor>,
    output: SharedOutput,
) {
    let mut buffer = [0; 8192];
    let error = loop {
        let mut ready = match source.readable_mut().await {
            Ok(ready) => ready,
            Err(e) => break Some(e),
        };
        match ready.try_io(|fd| fd.get_mut().read(&mut buffer)) {
            Ok(Ok(0)) => break None,
            Ok(Ok(n)) => append(&output, &buffer[..n]),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Ok(Err(e)) => break Some(e),
            Err(_) => {}
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

pub struct Shell {
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
        if cancel.is_cancelled() {
            bail!("Execution cancelled");
        }
        let mut processes = self.processes.lock().expect("process lock");
        if cancel.is_cancelled() {
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
            struct Descriptor(i32);
            impl std::os::fd::AsRawFd for Descriptor {
                fn as_raw_fd(&self) -> i32 {
                    self.0
                }
            }
            let mut stdin = filedescriptor::FileDescriptor::dup(&Descriptor(
                pair.master
                    .as_raw_fd()
                    .context("PTY has no Unix descriptor")?,
            ))?;
            stdin.set_non_blocking(true)?;
            let reader = filedescriptor::FileDescriptor::dup(&stdin)?;
            let (stdin, reader) = {
                let _runtime = self.runtime.enter();
                (register_pty(stdin)?, register_pty(reader)?)
            };
            let mut child = pair.slave.spawn_command(cmd).context("Start PTY command")?;
            let pid = match child.process_id() {
                Some(pid) => pid,
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("Missing PTY process id");
                }
            };
            self.runtime.spawn(pty_reader(reader, output.clone()));
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
                stdin: Mutex::new(Some(stdin)),
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
                stdin: Mutex::new(None),
                runtime: self.runtime.clone(),
                pid,
                started: Instant::now(),
            })
        };
        if let Some(timeout) = args.timeout_ms {
            let process = process.clone();
            self.runtime.spawn(async move {
                let deadline=tokio::time::Instant::now()+Duration::from_millis(timeout.min(86_400_000));
                loop {
                    let notified=process.output.1.notified();tokio::pin!(notified);notified.as_mut().enable();
                    {let state=process.output.0.lock().expect("output lock");if state.exit.is_some()&&state.readers==0 {return;}}
                    tokio::select! {_=tokio::time::sleep_until(deadline)=>{process.stop();return;},_=notified=>{}}
                }
            });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        processes.insert(id, process.clone());
        drop(processes);
        let result = process.read(
            id,
            args.yield_time_ms,
            args.max_output_tokens.unwrap_or(10_000),
            cancel,
        );
        let cancelled = cancel.is_cancelled();
        if cancelled {
            process.stop();
        }
        if result.as_ref().is_ok_and(|(_, complete)| *complete) || cancelled {
            self.processes.lock().expect("process lock").remove(&id);
        }
        result.map(|(text, _)| text)
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
            let mut stdin = process.stdin.lock().expect("stdin lock");
            let stdin = stdin.as_mut().context("stdin is closed for this session; rerun exec_command with tty=true for interactive input")?;
            self.runtime.block_on(async {
                let deadline=tokio::time::Instant::now()+Duration::from_secs(10);
                let mut remaining=args.chars.as_bytes();
                while !remaining.is_empty() {
                    let mut ready=tokio::select! {
                        biased;
                        _=cancel.cancelled()=>{process.stop();self.processes.lock().expect("process lock").remove(&args.session_id);bail!("Input cancelled; some bytes may already have been sent");}
                        _=tokio::time::sleep_until(deadline)=>bail!("PTY input stalled; {} bytes were not sent. Poll the session before sending more.",remaining.len()),
                        ready=stdin.writable_mut()=>ready?,
                    };
                    match ready.try_io(|fd|fd.get_mut().write(remaining)) {
                        Ok(Ok(0))=>bail!("PTY stdin closed"),Ok(Ok(n))=>remaining=&remaining[n..],
                        Ok(Err(e)) if e.kind()==std::io::ErrorKind::Interrupted=>{},
                        Ok(Err(e))=>return Err(e.into()),Err(_)=>{}
                    }
                }
                Ok(())
            })?;
        }
        let result = process.read(
            args.session_id,
            args.yield_time_ms,
            args.max_output_tokens.unwrap_or(10_000),
            cancel,
        );
        let cancelled = cancel.is_cancelled();
        if cancelled {
            process.stop();
        }
        if result.as_ref().is_ok_and(|(_, complete)| *complete) || cancelled {
            self.processes
                .lock()
                .expect("process lock")
                .remove(&args.session_id);
        }
        result.map(|(text, _)| text)
    }
    pub fn is_executing(&self) -> bool {
        self.processes
            .lock()
            .expect("process lock")
            .values()
            .any(|process| {
                let output = process.output.0.lock().expect("output lock");
                output.exit.is_none() || output.readers > 0
            })
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
            while shell.processes.lock().unwrap().is_empty() {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            cancel.cancel();
            assert!(job.join().unwrap().is_err());
            assert!(shell.processes.lock().unwrap().is_empty());
        }
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
    #[test]
    fn blocked_pty_input_can_be_cancelled() {
        let shell = Arc::new(Shell::new(std::env::temp_dir()));
        let cancel = CancellationToken::new();
        let first = shell.exec(serde_json::from_value(serde_json::json!({"cmd":"stty -echo -icanon; printf ready; sleep 20", "tty":true,"login":false,"yield_time_ms":30})).unwrap(), &cancel).unwrap();
        let session_id = running_id(&first.text);
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
        thread::sleep(Duration::from_millis(30));
        cancel.cancel();
        assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap().is_err());
        shell.stop_all();
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
    }
}
