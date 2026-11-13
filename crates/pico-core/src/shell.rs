//! Codex-style command sessions, scoped to one conversation. No approval/sandbox layer.
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
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
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
#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    dropped: usize,
    exit: Option<String>,
    readers: usize,
}
struct Process {
    output: Arc<(Mutex<Output>, Condvar)>,
    stdin: Mutex<Option<Box<dyn Write + Send>>>,
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
    ) -> Result<(String, bool)> {
        let deadline = Instant::now() + Duration::from_millis(wait.min(300_000));
        let (lock, signal) = &*self.output;
        let mut output = lock.lock().expect("process output lock");
        loop {
            if cancel.is_cancelled() {
                drop(output);
                self.stop();
                bail!("Command cancelled. Side effects already performed are not undone.");
            }
            if output.exit.is_some() && output.readers == 0 || Instant::now() >= deadline {
                break;
            }
            output = signal
                .wait_timeout(output, Duration::from_millis(20))
                .expect("process output wait")
                .0;
        }
        let complete = output.exit.is_some() && output.readers == 0;
        let mut body = String::from_utf8_lossy(&std::mem::take(&mut output.bytes)).into_owned();
        let dropped = std::mem::take(&mut output.dropped);
        if dropped > 0 {
            body.insert_str(0, &format!("[Dropped {dropped} earlier output bytes]\n"));
        }
        let body = truncate(&body, tokens);
        let status = if complete {
            format!(
                "Process exited with code {}",
                output.exit.as_ref().expect("complete process")
            )
        } else {
            format!("Process running with session ID {id}")
        };
        Ok((
            format!(
                "Wall time: {:.3} seconds\n{status}\nOutput:\n{body}",
                self.started.elapsed().as_secs_f64()
            ),
            complete,
        ))
    }
}
fn reader(mut source: impl Read + Send + 'static, output: Arc<(Mutex<Output>, Condvar)>) {
    thread::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            match source.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let mut state = output.0.lock().expect("output lock");
                    state.bytes.extend_from_slice(&buffer[..n]);
                    if state.bytes.len() > BUFFER_LIMIT {
                        let excess = state.bytes.len() - BUFFER_LIMIT;
                        state.bytes.drain(..excess);
                        state.dropped += excess;
                    }
                    output.1.notify_all();
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                // PTYs return EIO when their slave closes.
                Err(e) => {
                    if e.raw_os_error() != Some(5) {
                        let mut state = output.0.lock().expect("output lock");
                        state
                            .bytes
                            .extend_from_slice(format!("\n[Output read failed: {e}]").as_bytes());
                    }
                    break;
                }
            }
        }
        output.0.lock().expect("output lock").readers -= 1;
        output.1.notify_all();
    });
}

pub struct Shell {
    cwd: PathBuf,
    processes: Mutex<HashMap<i32, Arc<Process>>>,
    next: AtomicI32,
}
impl Shell {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            processes: Mutex::new(HashMap::new()),
            next: AtomicI32::new(rand::random_range(1_000..i32::MAX / 2)),
        }
    }
    pub fn exec(&self, args: ExecArgs, cancel: &CancellationToken) -> Result<String> {
        if cancel.is_cancelled() {
            bail!("Execution cancelled");
        }
        let mut processes = self.processes.lock().expect("process lock");
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
            Condvar::new(),
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
            let mut child = pair.slave.spawn_command(cmd).context("Start PTY command")?;
            let pid = child.process_id().context("Missing PTY process id")?;
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
            reader(pair.master.try_clone_reader()?, output.clone());
            drop(pair.slave);
            let state = output.clone();
            thread::spawn(move || {
                let status = child
                    .wait()
                    .map(|s| s.exit_code().to_string())
                    .unwrap_or_else(|e| format!("unknown ({e})"));
                state.0.lock().expect("output lock").exit = Some(status);
                state.1.notify_all();
                drop(pair.master);
            });
            Arc::new(Process {
                output,
                stdin: Mutex::new(Some(Box::new(stdin))),
                pid,
                started: Instant::now(),
            })
        } else {
            let mut child = Command::new(shell)
                .args([flag, &args.cmd])
                .current_dir(cwd)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Start shell command")?;
            let pid = child.id();
            reader(child.stdout.take().expect("piped stdout"), output.clone());
            reader(child.stderr.take().expect("piped stderr"), output.clone());
            let state = output.clone();
            thread::spawn(move || {
                let status = child
                    .wait()
                    .map(|s| {
                        s.code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "signal".into())
                    })
                    .unwrap_or_else(|e| format!("unknown ({e})"));
                state.0.lock().expect("output lock").exit = Some(status);
                state.1.notify_all();
            });
            Arc::new(Process {
                output,
                stdin: Mutex::new(None),
                pid,
                started: Instant::now(),
            })
        };
        if let Some(timeout) = args.timeout_ms {
            let process = process.clone();
            thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_millis(timeout.min(86_400_000));
                let mut state = process.output.0.lock().expect("output lock");
                while !(state.exit.is_some() && state.readers == 0) {
                    if Instant::now() >= deadline {
                        drop(state);
                        process.stop();
                        return;
                    }
                    state = process
                        .output
                        .1
                        .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                        .expect("timeout wait")
                        .0;
                }
            });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        processes.insert(id, process.clone());
        drop(processes);
        let (result, complete) = process.read(
            id,
            args.yield_time_ms,
            args.max_output_tokens.unwrap_or(10_000),
            cancel,
        )?;
        if complete {
            self.processes.lock().expect("process lock").remove(&id);
        }
        Ok(result)
    }
    pub fn write(&self, args: StdinArgs, cancel: &CancellationToken) -> Result<String> {
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
            let mut remaining = args.chars.as_bytes();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !remaining.is_empty() {
                if cancel.is_cancelled() {
                    process.stop();
                    bail!("Input cancelled; some bytes may already have been sent");
                }
                if Instant::now() >= deadline {
                    bail!(
                        "PTY input stalled; {} bytes were not sent. Poll the session before sending more.",
                        remaining.len()
                    );
                }
                match stdin.write(remaining) {
                    Ok(0) => bail!("PTY stdin closed"),
                    Ok(n) => remaining = &remaining[n..],
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        let (result, complete) = process.read(
            args.session_id,
            args.yield_time_ms,
            args.max_output_tokens.unwrap_or(10_000),
            cancel,
        )?;
        if complete {
            self.processes
                .lock()
                .expect("process lock")
                .remove(&args.session_id);
        }
        Ok(result)
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
pub fn truncate(text: &str, tokens: usize) -> String {
    if tokens == 0 {
        return String::new();
    }
    let limit = tokens.clamp(1, 100_000) * 4;
    if text.chars().count() <= limit {
        text.into()
    } else {
        let first: String = text.chars().take(limit / 2).collect();
        let last: String = text
            .chars()
            .rev()
            .take(limit / 2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{first}\n… [output truncated] …\n{last}")
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
    fn timeout_terminates_descendants_after_shell_leader_exits() {
        let shell = Shell::new(std::env::temp_dir());
        let out = shell.exec(serde_json::from_value(serde_json::json!({"cmd":"sleep 10 &", "login":false,"timeout_ms":30,"yield_time_ms":1000})).unwrap(), &CancellationToken::new()).unwrap();
        assert!(out.contains("Process exited"), "{out}");
    }
    #[test]
    fn blocked_pty_input_can_be_cancelled() {
        let shell = Arc::new(Shell::new(std::env::temp_dir()));
        let cancel = CancellationToken::new();
        let first = shell.exec(serde_json::from_value(serde_json::json!({"cmd":"stty -echo -icanon; printf ready; sleep 20", "tty":true,"login":false,"yield_time_ms":30})).unwrap(), &cancel).unwrap();
        let session_id = running_id(&first);
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
        let first = shell.exec(serde_json::from_value(serde_json::json!({"cmd":"printf before; sleep 0.15; printf after >&2; exit 7","login":false,"yield_time_ms":20})).unwrap(), &cancel).unwrap();
        let session_id = running_id(&first);
        assert!(first.contains("before"));
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
        assert!(second.contains("code 7"));
        assert!(second.contains("after"));
        assert!(!second.contains("before"));
    }
    #[test]
    fn pty_accepts_input() {
        let dir = tempfile::tempdir().unwrap();
        let shell = Shell::new(dir.path().into());
        let cancel = CancellationToken::new();
        let first = shell.exec(serde_json::from_value(serde_json::json!({"cmd":"read value; printf 'received:%s' \"$value\"", "tty":true,"login":false,"yield_time_ms":10})).unwrap(), &cancel).unwrap();
        let session_id = running_id(&first);
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
        assert!(out.contains("received:asymmetric"));
        assert!(out.contains("code 0"));
    }
}
