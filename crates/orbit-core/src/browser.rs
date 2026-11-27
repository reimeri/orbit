//! Session-scoped off-screen browser. The sandboxed CEF helper owns web content;
//! the model never sees a host handle, profile path, or another session's ID.
use crate::CancellationToken;
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};
use tempfile::NamedTempFile;
use tokio::sync::{oneshot, watch};

pub const MAX_SIDE: u32 = 4096;
#[derive(Default, Clone, Debug)]
pub struct Status {
    pub surface_id: Option<u64>,
    pub viewport: Option<(u32, u32)>,
    pub handoff_pending: bool,
    pub selection: Arc<str>,
    pub url: String,
    pub title: String,
    pub back: bool,
    pub forward: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub isolated: bool,
    pub user_control: bool,
}
#[derive(Clone, Debug)]
pub struct Frame {
    pub key: (u64, u32),
    pub width: u32,
    pub height: u32,
    pub bgra: Arc<Vec<u8>>,
    pub view: (u32, u32),
    pub scale: f32,
}
struct Surface {
    id: u64,
    file: NamedTempFile,
}
#[derive(Default)]
struct Tab {
    surface: Option<Arc<Surface>>,
    status: Status,
    epoch: u32,
}
type Reply = oneshot::Sender<Result<Value>>;
struct Pending {
    id: String,
    reply: Reply,
    cancel: CancellationToken,
}
struct Job {
    msg: Value,
    project: Option<PathBuf>,
}
struct Host {
    surfaces: tempfile::TempDir,
    id: u64,
    tx: mpsc::Sender<Job>,
    exit: mpsc::Receiver<Result<()>>,
}
#[derive(Default)]
struct State {
    host: Option<Host>,
    serial: u64,
    tabs: HashMap<String, Tab>,
    pending: HashMap<u64, Pending>,
}
impl State {
    fn serial(&mut self) -> u64 {
        self.serial += 1;
        self.serial
    }
    fn send(&self, msg: Value) -> Result<()> {
        self.host
            .as_ref()
            .context("Browser is not running")?
            .tx
            .send(Job { msg, project: None })
            .map_err(|_| anyhow!("Browser host stopped; reopen the tab"))
    }
    fn revoke(&mut self, id: &str) {
        self.pending.retain(|_, p| p.id != id);
        if let Some(t) = self.tabs.get_mut(id) {
            t.epoch += 1;
        }
    }
}

pub struct Browser {
    store: crate::store::Store,
    state: Mutex<State>,
    changes: watch::Sender<()>,
}
impl Browser {
    pub fn new(store: crate::store::Store) -> Result<Arc<Self>> {
        let mut state = State::default();
        {
            let db = store.db.lock().unwrap();
            let mut query =
                db.prepare("SELECT session, isolated, user_control FROM browser_policy")?;
            for row in
                query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)))?
            {
                let (id, isolated, user_control) = row?;
                state.tabs.insert(
                    id,
                    Tab {
                        status: Status {
                            isolated,
                            user_control,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                );
            }
        }
        Ok(Arc::new(Self {
            store,
            state: Mutex::new(state),
            changes: watch::channel(()).0,
        }))
    }
    fn save_policy(&self, id: &str, isolated: bool, user: bool) -> Result<()> {
        self.store.db.lock().unwrap().execute("INSERT INTO browser_policy(session,isolated,user_control) VALUES (?1,?2,?3) ON CONFLICT(session) DO UPDATE SET isolated=excluded.isolated,user_control=excluded.user_control",rusqlite::params![id,isolated,user])?;
        Ok(())
    }
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }
    fn changed(&self) {
        self.changes.send_replace(());
    }
    pub fn status(&self, id: &str) -> Status {
        self.state
            .lock()
            .unwrap()
            .tabs
            .get(id)
            .map(|t| t.status.clone())
            .unwrap_or_default()
    }

    fn start(self: &Arc<Self>, state: &mut State) -> Result<()> {
        let executable = std::env::var_os("ORBIT_BROWSER_HOST").context(
            "CEF helper is unavailable. Use the installed Orbit package or re-enter nix develop.",
        )?;
        let store = self.store.clone();
        let root = store.root().join("browser");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        let root = root.canonicalize()?;
        let surfaces = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()?;
        root.to_str().context("CEF requires a UTF-8 state path")?;
        let mut child = Command::new(executable)
            .env("ORBIT_BROWSER_ROOT", &root)
            .env_remove("DISPLAY")
            .args([
                "--ozone-platform=headless",
                "--disable-background-networking",
                // Alloy has no Chrome tab for Chromium 151's reader-mode
                // soft-navigation observer (CEF #4234).
                "--disable-features=ImmersiveReadAnything",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("Starting sandboxed CEF")?;
        let mut input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel::<Job>();
        let (done, exit) = mpsc::channel();
        let host_id = state.serial();
        let weak = Arc::downgrade(self);
        let reader = weak.clone();
        thread::spawn(move || {
            let mut error = None;
            for line in BufReader::new(output).lines() {
                let Some(browser) = reader.upgrade() else {
                    break;
                };
                match line
                    .map_err(anyhow::Error::from)
                    .and_then(|s| Ok(serde_json::from_str(&s)?))
                {
                    Ok(msg) => browser.event(host_id, msg),
                    Err(e) => {
                        error = Some(format!("Browser connection failed: {e}"));
                        break;
                    }
                }
            }
            if let Some(browser) = reader.upgrade() {
                let mut s = browser.state.lock().unwrap();
                if s.host.as_ref().is_some_and(|h| h.id == host_id) {
                    s.host = None;
                    for tab in s.tabs.values_mut() {
                        if tab.surface.take().is_some() {
                            tab.status.loading = false;
                            tab.status.handoff_pending = false;
                            tab.status.surface_id = None;
                            tab.status.error =
                                Some(error.clone().unwrap_or_else(|| {
                                    "Browser host exited. Reopen to retry.".into()
                                }));
                        }
                    }
                    s.pending.clear();
                    browser.changed();
                }
            }
            let result = child
                .wait()
                .context("Waiting for CEF shutdown")
                .and_then(|status| {
                    ensure!(
                        status.success(),
                        "Browser host exited with {status}; site data may not have finished saving"
                    );
                    Ok(())
                });
            let _ = done.send(result);
        });
        thread::spawn(move || {
            for mut job in rx {
                if let Some(project) = job.project {
                    match store
                        .load_catalog()
                        .and_then(|c| profile_key(c.project_origin(&project)))
                    {
                        Ok(key) => job.msg["profile"] = json!(root.join(key)),
                        Err(e) => {
                            if let Some(b) = weak.upgrade() {
                                b.event(host_id,json!({"tab":job.msg["tab"],"event":"open_failed","error":e.to_string()}));
                            }
                            continue;
                        }
                    }
                }
                if let Some(req) = job.msg.get("req").and_then(Value::as_u64)
                    && !weak.upgrade().is_some_and(|b| {
                        b.state
                            .lock()
                            .unwrap()
                            .pending
                            .get(&req)
                            .is_some_and(|p| !p.cancel.is_cancelled() && !p.reply.is_closed())
                    })
                {
                    continue;
                }
                if serde_json::to_writer(&mut input, &job.msg).is_err()
                    || input.write_all(b"\n").is_err()
                {
                    break;
                }
            }
        });
        state.host = Some(Host {
            surfaces,
            id: host_id,
            tx,
            exit,
        });
        Ok(())
    }
    fn ensure(self: &Arc<Self>, s: &mut State, id: &str, project: &Path) -> Result<u64> {
        if let Some(surface) = s.tabs.get(id).and_then(|t| t.surface.as_ref()) {
            return Ok(surface.id);
        }
        if s.host.is_none() {
            self.start(s)?
        }
        let file = NamedTempFile::new_in(s.host.as_ref().unwrap().surfaces.path())?;
        file.as_file()
            .set_len(64 + u64::from(MAX_SIDE).pow(2) * 4)?;
        let key = s.serial();
        let t = s.tabs.entry(id.to_owned()).or_default();
        t.status = Status {
            isolated: t.status.isolated,
            user_control: t.status.user_control,
            loading: true,
            ..Status::default()
        };
        t.status.surface_id = Some(key);
        file.as_file().write_all_at(&t.epoch.to_ne_bytes(), 12)?;
        let epoch = t.epoch;
        let isolated = t.status.isolated;
        let deny = t.status.user_control;
        s.host
            .as_ref()
            .unwrap()
            .tx
            .send(Job {
                project: (!isolated).then(|| project.to_owned()),
                msg: json!({"op":"open","tab":key,"file":file.path().to_str().context("CEF requires a UTF-8 temporary path")?,"profile":""}),
            })
            .map_err(|_| anyhow!("Browser host stopped"))?;
        s.tabs.get_mut(id).unwrap().surface = Some(Arc::new(Surface { id: key, file }));
        if deny {
            s.send(json!({"op":"control","tab":key,"allow":false,"epoch":epoch}))?
        }
        Ok(key)
    }
    pub fn show(self: &Arc<Self>, id: &str, project: &Path) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let key = self.ensure(&mut s, id, project)?;
        s.send(json!({"op":"visible","tab":key,"show":true}))
    }
    pub fn hide(&self, id: &str) {
        let _ = self.user(id, json!({"op":"visible","show":false}));
    }
    pub fn user(&self, id: &str, mut msg: Value) -> Result<()> {
        let s = self.state.lock().unwrap();
        msg["tab"] = json!(
            s.tabs
                .get(id)
                .and_then(|t| t.surface.as_ref())
                .context("Open the browser first")?
                .id
        );
        s.send(msg)
    }
    pub fn control(&self, id: &str, user: bool) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.revoke(id);
        let t = s.tabs.entry(id.into()).or_default();
        let isolated = t.status.isolated;
        if !user {
            self.save_policy(id, isolated, false)?;
        }
        t.status.user_control = user;
        if let Some(ref f) = t.surface {
            let key = f.id;
            let epoch = t.epoch;
            f.file.as_file().write_all_at(&epoch.to_ne_bytes(), 12)?;
            t.status.handoff_pending = true;
            s.send(json!({"tab":key,"op":"control","allow":!user,"epoch":epoch}))?
        }
        self.changed();
        if user {
            self.save_policy(id, isolated, true)
                .context("Agent blocked now, but could not save takeover for restart")?;
        }
        Ok(())
    }
    fn close_locked(s: &mut State, id: &str) {
        s.revoke(id);
        if let Some(t) = s.tabs.get_mut(id)
            && let Some(f) = t.surface.take()
        {
            t.status = Status {
                isolated: t.status.isolated,
                user_control: t.status.user_control,
                ..Default::default()
            };
            // Retiring the generation also invalidates input already in CEF's queue.
            let _ = f.file.as_file().write_all_at(&t.epoch.to_ne_bytes(), 12);
            let _ = s.send(json!({"tab":f.id,"op":"close"}));
        }
    }
    pub fn close(&self, id: &str) {
        Self::close_locked(&mut self.state.lock().unwrap(), id);
        self.changed();
    }
    pub fn forget(&self, id: &str) {
        let mut s = self.state.lock().unwrap();
        Self::close_locked(&mut s, id);
        s.tabs.remove(id);
        self.changed();
    }
    pub fn set_isolated(&self, id: &str, isolated: bool) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        self.save_policy(
            id,
            isolated,
            s.tabs.get(id).is_some_and(|t| t.status.user_control),
        )?;
        Self::close_locked(&mut s, id);
        s.tabs.entry(id.into()).or_default().status.isolated = isolated;
        self.changed();
        Ok(())
    }
    pub fn shutdown(&self) -> Result<()> {
        let host = {
            let mut s = self.state.lock().unwrap();
            for t in s.tabs.values_mut() {
                t.surface = None;
                t.status = Status {
                    user_control: t.status.user_control,
                    isolated: t.status.isolated,
                    ..Default::default()
                };
                t.epoch += 1;
            }
            s.pending.clear();
            s.host.take()
        };
        if let Some(Host { tx, exit, .. }) = host {
            drop(tx);
            exit.recv_timeout(Duration::from_secs(20))
                .context("Waiting for the browser to finish saving site data")??;
        }
        Ok(())
    }
    fn event(&self, host: u64, v: Value) {
        let mut s = self.state.lock().unwrap();
        if !s.host.as_ref().is_some_and(|h| h.id == host) {
            return;
        }
        if let Some(req) = v["req"].as_u64().filter(|r| *r != 0) {
            if let Some(p) = s.pending.remove(&req) {
                let _ = p.reply.send(if let Some(e) = v["error"].as_str() {
                    Err(anyhow!("{e}"))
                } else {
                    Ok(v)
                });
            }
            return;
        }
        let key = v["tab"].as_u64();
        let Some(t) = s
            .tabs
            .values_mut()
            .find(|t| t.surface.as_ref().is_some_and(|f| Some(f.id) == key))
        else {
            return;
        };
        match v["event"].as_str() {
            Some("state") => {
                let a = &mut t.status;
                a.url = v["url"].as_str().unwrap().into();
                a.title = v["title"].as_str().unwrap().into();
                a.back = v["back"].as_bool().unwrap();
                a.forward = v["forward"].as_bool().unwrap();
                a.loading = v["loading"].as_bool().unwrap();
                if a.loading {
                    a.error = None;
                }
            }
            Some("viewport") => {
                t.status.viewport =
                    serde_json::from_value(v["fixed"].clone()).expect("native viewport");
            }
            Some("control") => {
                if v["epoch"].as_u64() == Some(u64::from(t.epoch)) {
                    t.status.handoff_pending = false;
                }
            }
            Some("selection") => t.status.selection = Arc::from(v["text"].as_str().unwrap()),
            Some("open_failed") => {
                t.surface = None;
                t.status.surface_id = None;
                t.status.handoff_pending = false;
                t.status.loading = false;
                t.status.error = Some(v["error"].as_str().unwrap().into());
            }
            Some("error") => t.status.error = Some(v["error"].as_str().unwrap().into()),
            Some("closed") => {
                t.surface = None;
                t.status.surface_id = None;
                t.status.handoff_pending = false;
                t.status.loading = false;
            }
            Some("paint") => {}
            _ => return,
        }
        self.changed()
    }
    pub fn frame(&self, id: &str, last: Option<(u64, u32)>) -> Result<Option<Frame>> {
        let Some(f) = self
            .state
            .lock()
            .unwrap()
            .tabs
            .get(id)
            .and_then(|t| t.surface.clone())
        else {
            return Ok(None);
        };
        let mut header = [0u8; 28];
        let file = f.file.as_file();
        file.read_exact_at(&mut header, 0)?;
        let seq = u32::from_ne_bytes(header[0..4].try_into()?);
        let key = (f.id, seq);
        if seq == 0 || seq % 2 == 1 || Some(key) == last {
            return Ok(None);
        }
        let w = u32::from_ne_bytes(header[4..8].try_into()?);
        let h = u32::from_ne_bytes(header[8..12].try_into()?);
        ensure!(
            (1..=MAX_SIDE).contains(&w) && (1..=MAX_SIDE).contains(&h),
            "Invalid CEF frame dimensions"
        );
        let mut pixels = vec![0; (w * h * 4) as usize];
        file.read_exact_at(&mut pixels, 64)?;
        let mut end = [0; 4];
        file.read_exact_at(&mut end, 0)?;
        if u32::from_ne_bytes(end) != seq {
            return Ok(None);
        }
        Ok(Some(Frame {
            key,
            width: w,
            height: h,
            bgra: Arc::new(pixels),
            view: (
                u32::from_ne_bytes(header[16..20].try_into()?),
                u32::from_ne_bytes(header[20..24].try_into()?),
            ),
            scale: f32::from_ne_bytes(header[24..28].try_into()?),
        }))
    }
}

fn profile_key(project: &Path) -> Result<String> {
    let checkout = project
        .canonicalize()
        .context("Cannot resolve browser project directory")?;
    // Only a checkout actually registered in Git's list may share the primary
    // identity. Preserve the relative path of projects rooted below a checkout.
    let out = Command::new("git")
        .current_dir(&checkout)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()?;
    let mut hash = Sha256::new();
    let registered = out
        .stdout
        .split(|b| *b == 0)
        .filter_map(|field| field.strip_prefix(b"worktree "))
        .map(|path| Path::new(std::ffi::OsStr::from_bytes(path)))
        .filter(|path| checkout.starts_with(path))
        .max_by_key(|path| path.components().count());
    if out.status.success()
        && let Some(path) = registered
    {
        let common = Command::new("git")
            .current_dir(&checkout)
            .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .output()?;
        ensure!(
            common.status.success(),
            "Cannot resolve Git repository for browser profile"
        );
        let dir = Path::new(std::ffi::OsStr::from_bytes(
            common.stdout.strip_suffix(b"\n").unwrap_or(&common.stdout),
        ))
        .canonicalize()?;
        hash.update(b"git\0");
        hash.update(dir.as_os_str().as_bytes());
        hash.update(b"\0");
        hash.update(checkout.strip_prefix(path)?.as_os_str().as_bytes());
    } else {
        hash.update(b"path\0");
        hash.update(checkout.as_os_str().as_bytes());
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn address(input: &str) -> Result<String> {
    let input = input.trim();
    ensure!(!input.is_empty(), "Enter an HTTP(S) address");
    let addr = if input.contains("://") {
        input.to_owned()
    } else if input == "localhost"
        || input.starts_with("localhost/")
        || input.starts_with("localhost:")
        || input.starts_with("127.")
        || input.starts_with("[::1]")
    {
        format!("http://{input}")
    } else {
        format!("https://{input}")
    };
    let u = url::Url::parse(&addr)?;
    ensure!(
        matches!(u.scheme(), "http" | "https")
            && u.host().is_some()
            && u.username().is_empty()
            && u.password().is_none(),
        "Only HTTP(S) addresses without embedded credentials are supported"
    );
    Ok(u.to_string())
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum AgentAction {
    Navigate {
        url: String,
    },
    Screenshot,
    Back,
    Forward,
    Reload,
    Stop,
    Click {
        x: Option<i32>,
        y: Option<i32>,
        r#ref: Option<String>,
    },
    Fill {
        r#ref: String,
        text: String,
    },
    Inspect,
    Viewport {
        width: Option<u32>,
        height: Option<u32>,
        mode: Option<String>,
    },
    Wait {
        role: Option<String>,
        name: Option<String>,
        state: Option<WaitState>,
        url: Option<String>,
        timeout_ms: Option<u64>,
    },
    Type {
        text: String,
    },
    Scroll {
        x: i32,
        y: i32,
        dy: i32,
    },
    Key {
        key: String,
    },
}
#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum WaitState {
    Appeared,
    Disappeared,
    Enabled,
}

#[derive(Clone)]
pub(crate) struct AgentTab {
    pub browser: Arc<Browser>,
    pub id: String,
    pub project: PathBuf,
}
impl AgentTab {
    pub async fn call(
        &self,
        args: &str,
        cancel: &CancellationToken,
    ) -> Result<(Value, Option<Vec<u8>>)> {
        let a: AgentAction = serde_json::from_str(args)?;
        let capture = matches!(a, AgentAction::Screenshot)
            .then(|| {
                tempfile::Builder::new()
                    .permissions(fs::Permissions::from_mode(0o700))
                    .tempdir_in(self.browser.store.root())
            })
            .transpose()?;
        let mut timeout = Duration::from_secs(20);
        let mut msg = match a {
            AgentAction::Back => json!({"op":"back"}),
            AgentAction::Forward => json!({"op":"forward"}),
            AgentAction::Reload => json!({"op":"reload"}),
            AgentAction::Stop => json!({"op":"stop"}),
            AgentAction::Navigate { url } => json!({"op":"navigate","url":address(&url)?}),
            AgentAction::Screenshot => {
                json!({"op":"screenshot","file":capture.as_ref().unwrap().path().join("page.png")})
            }
            AgentAction::Click { x, y, r#ref } => match (x, y, r#ref) {
                (Some(x), Some(y), None) => json!({"op":"click","x":x,"y":y}),
                (None, None, Some(r)) => json!({"op":"activate","ref":r}),
                _ => bail!("Click requires either x and y or ref"),
            },
            AgentAction::Fill { r#ref, text } => json!({"op":"fill","ref":r#ref,"text":text}),
            AgentAction::Inspect => {
                json!({"op":"inspect","snapshot":uuid::Uuid::new_v4().simple().to_string()})
            }
            AgentAction::Viewport {
                width,
                height,
                mode,
            } => match (width, height, mode.as_deref()) {
                (Some(w), Some(h), None)
                    if (1..=MAX_SIDE).contains(&w) && (1..=MAX_SIDE).contains(&h) =>
                {
                    json!({"op":"viewport","fixed":[w,h]})
                }
                (None, None, Some("fit")) => json!({"op":"viewport","fixed":null}),
                _ => bail!("Specify width and height in 1..={MAX_SIDE}, or mode: fit"),
            },
            AgentAction::Wait {
                role,
                name,
                state,
                url,
                timeout_ms,
            } => {
                let ms = timeout_ms.unwrap_or(5_000);
                ensure!(
                    (1..=60_000).contains(&ms),
                    "timeout_ms must be in 1..=60000"
                );
                timeout = Duration::from_millis(ms) + Duration::from_secs(20);
                match (role, name, state, url) {
                    (None, None, None, Some(url)) => {
                        json!({"op":"wait","url":if url=="about:blank"{url}else{address(&url)?},"timeout_ms":ms})
                    }
                    (Some(role), Some(name), state, None) if !role.is_empty() => {
                        json!({"op":"wait","role":role,"name":name,"state":state.unwrap_or(WaitState::Appeared),"timeout_ms":ms})
                    }
                    _ => bail!("Wait requires either an exact URL or an accessible role and name"),
                }
            }
            AgentAction::Scroll { x, y, dy } => json!({"op":"scroll","x":x,"y":y,"dy":dy}),
            AgentAction::Type { text } => json!({"op":"text","text":text}),
            AgentAction::Key { key } => {
                json!({"op":"key","code":match key.as_str(){"Enter"=>13,"Tab"=>9,"Escape"=>27,"Backspace"=>8,"Delete"=>46,"ArrowDown"=>40,"ArrowUp"=>38,"ArrowLeft"=>37,"ArrowRight"=>39,_=>bail!("Unknown browser key")}})
            }
        };
        let (id, key, epoch, rx) = {
            let mut s = self.browser.state.lock().unwrap();
            ensure!(!cancel.is_cancelled(), "Browser action cancelled");
            ensure!(
                !s.tabs.get(&self.id).is_some_and(|t| t.status.user_control),
                "User has browser control; wait for handback"
            );
            let key = self.browser.ensure(&mut s, &self.id, &self.project)?;
            let epoch = s.tabs[&self.id].epoch;
            let id = s.serial();
            let (tx, rx) = oneshot::channel();
            msg["lease"] = json!(epoch);
            msg["req"] = json!(id);
            msg["tab"] = json!(key);
            s.send(msg)?;
            s.pending.insert(
                id,
                Pending {
                    id: self.id.clone(),
                    reply: tx,
                    cancel: cancel.clone(),
                },
            );
            (id, key, epoch, rx)
        };
        let result = tokio::select! {biased;_=cancel.cancelled()=>Err(anyhow!("Browser action cancelled")), v=tokio::time::timeout(timeout,rx)=>match v {Ok(Ok(reply))=>reply,Ok(Err(_))=>Err(anyhow!("Browser control revoked or tab closed")),Err(_)=>Err(anyhow!("Browser timed out; check the page before retrying"))}};
        let mut s = self.browser.state.lock().unwrap();
        s.pending.remove(&id);
        if result.is_err() {
            let _ = s.send(json!({"op":"cancel","tab":key,"ticket":id}));
        }
        let mut value = result?;
        ensure!(
            s.tabs.get(&self.id).is_some_and(|t| t.epoch == epoch
                && !t.status.user_control
                && t.surface.as_ref().is_some_and(|f| f.id == key)),
            "Browser control was revoked"
        );
        value.as_object_mut().unwrap().remove("req");
        Ok((
            value,
            capture
                .map(|file| fs::read(file.path().join("page.png")))
                .transpose()?,
        ))
    }
}

/// Keep large semantic results out of QuickJS's bounded heap without losing nodes.
pub(crate) fn externalize_snapshot(value: &mut Value) {
    let bytes = serde_json::to_vec(value).expect("JSON snapshot");
    if bytes.len() <= 40_000 {
        return;
    }
    let save = || -> Result<PathBuf> {
        let mut file = tempfile::Builder::new()
            .prefix("orbit-browser-ax-")
            .suffix(".json")
            .tempfile_in("/tmp")?;
        file.write_all(&bytes)?;
        Ok(file.keep()?.1)
    };
    match save() {
        Ok(path) => *value = json!({"url":value["url"],"snapshot_file":path,"bytes":bytes.len()}),
        Err(e) => {
            value["warning"] = json!(format!(
                "Could not preserve snapshot in /tmp: {e}; returning complete snapshot inline"
            ))
        }
    }
}

#[cfg(test)]
mod tests;
