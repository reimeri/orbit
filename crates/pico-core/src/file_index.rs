//! Background, path-only workspace discovery. No UI/storage-thread filesystem work.
use anyhow::Result;
use notify::{RecursiveMode, Watcher};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering as Cmp,
    collections::HashSet,
    fs::{self, File},
    hash::{DefaultHasher, Hash, Hasher},
    io::{BufRead, BufReader, Read},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
        process::CommandExt,
    },
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

pub const RESULTS: usize = 24;
// Each service retains at most a current, building and in-flight snapshot. The app
// keeps at most two services. Budget includes arena, entries and both sort indexes.
const SNAPSHOT_BYTES: usize = 512 * 1024 * 1024;
const RECONCILE: Duration = Duration::from_secs(30);
const DEBOUNCE: Duration = Duration::from_millis(250);
const CACHE_MAGIC: &[u8] = b"pico-evo-files-2\0";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchPhase {
    /// No results for this search are ready to accept yet.
    Pending,
    /// Current results are usable while more filenames are loaded or searched.
    Partial,
    #[default]
    Complete,
}

#[derive(Clone, Debug, Default)]
pub struct Response {
    pub request: u64,
    pub generation: u64,
    /// Encoded, complete mention strings; never truncated filesystem paths.
    pub paths: Vec<String>,
    pub phase: SearchPhase,
    pub status: String,
}
#[derive(Clone)]
struct Request {
    id: u64,
    query: Vec<u8>,
    ignored: bool,
}
#[derive(Clone, Copy)]
struct Entry {
    start: u32,
    end: u32,
    basename: u32,
}
struct Snapshot {
    arena: Vec<u8>,
    entries: Vec<Entry>,
    paths: Vec<u32>,
    names: Vec<u32>,
}
fn fold_cmp(a: &[u8], b: &[u8]) -> Cmp {
    a.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(b.iter().map(u8::to_ascii_lowercase))
}
fn starts(a: &[u8], b: &[u8]) -> bool {
    a.len() >= b.len() && a[..b.len()].eq_ignore_ascii_case(b)
}
fn internal(path: &Path) -> bool {
    path.components().any(
        |c| matches!(c, Component::Normal(n) if n == ".git" || n == ".pico" || n == ".pico-evo"),
    )
}
fn safe(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}
impl Snapshot {
    fn new() -> Self {
        Self {
            arena: vec![],
            entries: vec![],
            paths: vec![],
            names: vec![],
        }
    }
    fn append(&mut self, path: &[u8]) -> Result<()> {
        let p = Path::new(std::ffi::OsStr::from_bytes(path));
        anyhow::ensure!(safe(p), "Enumerator returned an unsafe relative path");
        if internal(p) {
            return Ok(());
        }
        let bytes = self.arena.len()
            + path.len()
            + 1
            + (self.entries.len() + 1)
                * (std::mem::size_of::<Entry>() + 3 * std::mem::size_of::<u32>());
        anyhow::ensure!(
            bytes <= SNAPSHOT_BYTES,
            "Workspace file index exceeds its memory budget; narrow the workspace or ignore generated trees. No partial index was published"
        );
        let arena_capacity = if self.arena.len() + path.len() + 1 > self.arena.capacity() {
            (self.arena.capacity() * 2).max(self.arena.len() + path.len() + 1)
        } else {
            self.arena.capacity()
        };
        let entry_capacity = if self.entries.len() == self.entries.capacity() {
            (self.entries.capacity() * 2).max(1)
        } else {
            self.entries.capacity()
        };
        anyhow::ensure!(
            arena_capacity
                + entry_capacity * std::mem::size_of::<Entry>()
                + (self.entries.len() + 1) * 3 * std::mem::size_of::<u32>()
                <= SNAPSHOT_BYTES,
            "Workspace index allocation budget exceeded; narrow the workspace or ignore generated trees"
        );
        self.arena.reserve_exact(arena_capacity - self.arena.len());
        self.entries
            .reserve_exact(entry_capacity - self.entries.len());
        let start = self.arena.len();
        self.arena.extend_from_slice(path);
        self.arena.push(0);
        self.entries.push(Entry {
            start: start as u32,
            end: (start + path.len()) as u32,
            basename: (start + path.iter().rposition(|b| *b == b'/').map_or(0, |i| i + 1)) as u32,
        });
        Ok(())
    }
    fn path(&self, id: u32) -> &[u8] {
        let e = self.entries[id as usize];
        &self.arena[e.start as usize..e.end as usize]
    }
    fn name(&self, id: u32) -> &[u8] {
        let e = self.entries[id as usize];
        &self.arena[e.basename as usize..e.end as usize]
    }
    fn finish(mut self, cancelled: &impl Fn() -> bool) -> Result<Self> {
        self.arena.shrink_to_fit();
        self.entries.shrink_to_fit();
        let mut paths: Vec<_> = (0..self.entries.len() as u32).collect();
        sort_ids(
            &mut paths,
            |a, b| {
                fold_cmp(self.path(*a), self.path(*b))
                    .then_with(|| self.path(*a).cmp(self.path(*b)))
            },
            cancelled,
        )?;
        paths.dedup_by(|a, b| self.path(*a) == self.path(*b));
        let mut names = paths.clone();
        sort_ids(
            &mut names,
            |a, b| {
                fold_cmp(self.name(*a), self.name(*b))
                    .then_with(|| fold_cmp(self.path(*a), self.path(*b)))
                    .then_with(|| self.path(*a).cmp(self.path(*b)))
            },
            cancelled,
        )?;
        self.paths = paths;
        self.names = names;
        Ok(self)
    }
    fn search(
        &self,
        query: &[u8],
        cancelled: impl Fn() -> bool,
        mut progress: impl FnMut(&[String]),
    ) -> Option<Vec<String>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for rank in 0..4 {
            // Prefix hits are usable immediately; substring backfill may scan the corpus.
            if rank == 3 && !out.is_empty() && !cancelled() {
                progress(&out);
            }
            let names = rank < 2;
            let index = if names { &self.names } else { &self.paths };
            let key = |id| if names { self.name(id) } else { self.path(id) };
            let start = if rank < 3 && !query.is_empty() {
                index.partition_point(|id| fold_cmp(key(*id), query).is_lt())
            } else {
                0
            };
            for (step, id) in index[start..].iter().enumerate() {
                if step % 128 == 0 && cancelled() {
                    return None;
                }
                let path = self.path(*id);
                let matched = if query.is_empty() {
                    rank == 3
                } else {
                    match rank {
                        0 => key(*id).eq_ignore_ascii_case(query),
                        1 | 2 => starts(key(*id), query),
                        _ => memchr::memchr2_iter(
                            query[0].to_ascii_lowercase(),
                            query[0].to_ascii_uppercase(),
                            path,
                        )
                        .take_while(|at| path.len() - at >= query.len())
                        .any(|at| path[at..at + query.len()].eq_ignore_ascii_case(query)),
                    }
                };
                if rank < 3 && !matched && !query.is_empty() {
                    break;
                }
                if matched && seen.insert(*id) {
                    out.push(crate::file_tags::encode(Path::new(
                        std::ffi::OsStr::from_bytes(path),
                    )));
                    if out.len() == RESULTS {
                        return Some(out);
                    }
                }
                if query.is_empty() && rank < 3 {
                    break;
                }
            }
        }
        Some(out)
    }
}
/// Sorting is cancellable too: a workspace switch must not retain a million-path
/// build until both whole-corpus sorts finish. Only one bounded scratch array lives.
fn sort_ids(
    ids: &mut [u32],
    compare: impl Fn(&u32, &u32) -> Cmp,
    cancelled: &impl Fn() -> bool,
) -> Result<()> {
    const CHUNK: usize = 1024;
    for chunk in ids.chunks_mut(CHUNK) {
        anyhow::ensure!(!cancelled(), "File index sorting cancelled");
        chunk.sort_unstable_by(&compare);
    }
    if ids.len() <= CHUNK {
        return Ok(());
    }
    let mut scratch = vec![0; ids.len()];
    let mut width = CHUNK;
    while width < ids.len() {
        for start in (0..ids.len()).step_by(width * 2) {
            let middle = (start + width).min(ids.len());
            let end = (start + width * 2).min(ids.len());
            let (mut left, mut right) = (start, middle);
            for (out, target) in scratch.iter_mut().enumerate().take(end).skip(start) {
                if out % CHUNK == 0 {
                    anyhow::ensure!(!cancelled(), "File index sorting cancelled");
                }
                if right == end || (left < middle && !compare(&ids[left], &ids[right]).is_gt()) {
                    *target = ids[left];
                    left += 1;
                } else {
                    *target = ids[right];
                    right += 1;
                }
            }
        }
        ids.copy_from_slice(&scratch);
        width *= 2;
    }
    Ok(())
}
struct State {
    snapshot: Option<Arc<Snapshot>>,
    generation: u64,
    revision: u64,
    request: Option<Request>,
    status: String,
    refreshing: bool,
    dirty: Option<Instant>,
    force: bool,
    explicit_scope: Option<PathBuf>,
    // Invalidates scoped snapshots after watch setup or reconciliation.
    scope_revision: u64,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    stop: AtomicBool,
    serial: AtomicU64,
    responses: tokio::sync::watch::Sender<Response>,
}
pub struct Service {
    shared: Arc<Shared>,
    pub root: PathBuf,
}
impl Service {
    pub fn open(root: PathBuf, cache: PathBuf) -> Result<Arc<Self>> {
        Self::start(root, cache, RECONCILE)
    }
    fn start(root: PathBuf, cache: PathBuf, reconcile: Duration) -> Result<Arc<Self>> {
        let (responses, _) = tokio::sync::watch::channel(Response::default());
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                snapshot: None,
                generation: 0,
                revision: 0,
                request: None,
                status: String::new(),
                refreshing: true,
                dirty: None,
                force: false,
                explicit_scope: None,
                scope_revision: 0,
            }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            serial: AtomicU64::new(0),
            responses,
        });
        let service = Arc::new(Self {
            shared: shared.clone(),
            root: root.clone(),
        });
        let worker = shared.clone();
        let search_root = root.clone();
        std::thread::Builder::new()
            .name("pico-file-search".into())
            .spawn(move || search_worker(worker, search_root))?;
        std::thread::Builder::new()
            .name("pico-file-discovery".into())
            .spawn(move || discovery_worker(shared, root, cache, reconcile))?;
        Ok(service)
    }
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Response> {
        self.shared.responses.subscribe()
    }
    pub fn query(&self, request: u64, query: Vec<u8>, ignored: bool) {
        self.shared.serial.store(request, Ordering::Relaxed);
        let mut state = self.shared.state.lock().expect("file index");
        let scope = if ignored {
            query
                .iter()
                .rposition(|b| *b == b'/')
                .map(|slash| PathBuf::from(std::ffi::OsString::from_vec(query[..slash].to_vec())))
                .filter(|path| safe(path) && !internal(path))
        } else {
            None
        };
        state.explicit_scope = scope;
        state.request = Some(Request {
            id: request,
            query,
            ignored,
        });
        state.revision += 1;
        self.shared.wake.notify_all();
    }
    pub fn cancel_query(&self) {
        self.shared.serial.fetch_add(1, Ordering::Relaxed);
        let mut state = self.shared.state.lock().expect("file index");
        state.request = None;
        state.explicit_scope = None;
        state.revision += 1;
        self.shared.wake.notify_all();
    }
    pub fn refresh(&self) {
        let mut state = self.shared.state.lock().expect("file index");
        state.force = true;
        self.shared.wake.notify_all();
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        // Pair the stop predicate with the wait mutex: otherwise shutdown can
        // notify between a worker's predicate check and its actual wait.
        let _state = self.shared.state.lock().expect("file index");
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
    }
}
fn stopped(shared: &Shared) -> bool {
    shared.stop.load(Ordering::Relaxed)
}
fn search_worker(shared: Arc<Shared>, root: PathBuf) {
    let mut done = u64::MAX;
    let mut scope_cache: Option<(Vec<u8>, u64, Snapshot)> = None;
    loop {
        let (request, snapshot, generation, scope_revision, revision, status, refreshing) = {
            let mut state = shared.state.lock().expect("file index");
            while !stopped(&shared) && (state.request.is_none() || state.revision == done) {
                state = shared.wake.wait(state).expect("file index");
            }
            if stopped(&shared) {
                return;
            }
            (
                state.request.clone().expect("request"),
                state.snapshot.clone(),
                state.generation,
                state.scope_revision,
                state.revision,
                state.status.clone(),
                state.refreshing || state.force,
            )
        };
        let cancelled = || stopped(&shared) || shared.serial.load(Ordering::Relaxed) != request.id;
        let mut response = Response {
            request: request.id,
            generation,
            paths: vec![],
            phase: if refreshing {
                SearchPhase::Pending
            } else {
                SearchPhase::Complete
            },
            status,
        };
        let mut source = snapshot.as_deref();
        if request.ignored
            && let Some(slash) = request.query.iter().rposition(|b| *b == b'/')
        {
            let scope = &request.query[..slash];
            if scope_cache
                .as_ref()
                .is_none_or(|(old, g, _)| old != scope || *g != scope_revision)
            {
                let dir = PathBuf::from(std::ffi::OsString::from_vec(scope.to_vec()));
                let result = (|| {
                    anyhow::ensure!(
                        safe(&dir) && !internal(&dir),
                        "Ignored scope must be a workspace directory"
                    );
                    // No directory symlinks, including ancestors, for explicit traversal.
                    let root_fd = File::open(&root)?;
                    let _scope = rustix::fs::openat2(
                        &root_fd,
                        &dir,
                        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
                        rustix::fs::Mode::empty(),
                        rustix::fs::ResolveFlags::BENEATH | rustix::fs::ResolveFlags::NO_SYMLINKS,
                    )?;
                    walk_scope(File::from(_scope), dir, &cancelled)
                })();
                match result {
                    Ok(index) => scope_cache = Some((scope.to_vec(), scope_revision, index)),
                    Err(error) => {
                        response.status = format!("Could not search ignored directory: {error:#}");
                        source = None;
                        response.phase = SearchPhase::Complete;
                    }
                }
            }
            if let Some((old, g, index)) = &scope_cache
                && old == scope
                && *g == scope_revision
            {
                source = Some(index);
                response.status = "Including ignored files in this directory".into();
            }
        } else {
            scope_cache = None;
        }
        if let Some(snapshot) = source {
            shared.responses.send_replace(Response {
                phase: SearchPhase::Pending,
                ..response.clone()
            });
            let Some(paths) = snapshot.search(&request.query, cancelled, |paths| {
                shared.responses.send_replace(Response {
                    paths: paths.to_vec(),
                    phase: SearchPhase::Partial,
                    ..response.clone()
                });
            }) else {
                continue;
            };
            response.paths = paths;
            response.phase = if refreshing {
                SearchPhase::Partial
            } else {
                SearchPhase::Complete
            };
        }
        if !cancelled() {
            shared.responses.send_replace(response);
            done = revision;
        }
    }
}
// Resolve exclusions once per background operation, never once per filename or
// on the UI thread. Do not exclude arbitrary projects merely named pico-evo.
fn discovery_exclusions(cache: &Path) -> Vec<PathBuf> {
    let mut paths = vec![cache.to_path_buf()];
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let home = std::env::var_os("HOME");
    if let Some(config) = crate::config_dir(
        xdg.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
    ) {
        paths.push(config);
    }
    for path in paths.clone() {
        if let Ok(resolved) = fs::canonicalize(&path)
            && resolved != path
        {
            paths.push(resolved);
        }
    }
    paths
}
fn discovery_walk(root: &Path, cache: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .add_custom_ignore_filename(".rgignore")
        .filter_entry({
            let excluded = discovery_exclusions(cache);
            move |entry| {
                !excluded.iter().any(|path| entry.path().starts_with(path))
                    && !internal(entry.path())
            }
        })
        .build()
}
fn walk(root: &Path, cache: &Path, cancelled: &impl Fn() -> bool) -> Result<Snapshot> {
    let mut snapshot = Snapshot::new();
    for entry in discovery_walk(root, cache) {
        anyhow::ensure!(!cancelled(), "File discovery cancelled");
        let entry = entry?;
        if entry.file_type().is_some_and(|kind| kind.is_file()) {
            snapshot.append(entry.path().strip_prefix(root)?.as_os_str().as_bytes())?;
        }
    }
    snapshot.finish(cancelled)
}
// Enumerate explicit ignored directories through pinned descriptors, never re-open
// the checked scope by a mutable pathname. Directory symlinks are not followed.
fn visit_scope(
    directory: File,
    relative: PathBuf,
    cancelled: &impl Fn() -> bool,
    mut visit: impl FnMut(&Path, bool) -> Result<bool>,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    if !visit(&relative, true)? {
        return Ok(());
    }
    let entries = fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
    let mut stack = vec![(directory, relative, entries)];
    while let Some((directory, relative, entries)) = stack.last_mut() {
        anyhow::ensure!(!cancelled(), "Scoped file discovery cancelled");
        let Some(entry) = entries.next() else {
            stack.pop();
            continue;
        };
        let entry = entry?;
        let name = entry.file_name();
        let path = relative.join(&name);
        if internal(&path) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let fd = rustix::fs::openat2(
                &*directory,
                name.as_os_str(),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
                rustix::fs::ResolveFlags::BENEATH | rustix::fs::ResolveFlags::NO_SYMLINKS,
            )?;
            let child = File::from(fd);
            if !visit(&path, true)? {
                return Ok(());
            }
            let entries = fs::read_dir(format!("/proc/self/fd/{}", child.as_raw_fd()))?;
            stack.push((child, path, entries));
        } else if kind.is_file() && !visit(&path, false)? {
            return Ok(());
        }
    }
    Ok(())
}
fn walk_scope(
    directory: File,
    relative: PathBuf,
    cancelled: &impl Fn() -> bool,
) -> Result<Snapshot> {
    let mut index = Snapshot::new();
    visit_scope(directory, relative, cancelled, |path, directory| {
        if !directory {
            index.append(path.as_os_str().as_bytes())?;
        }
        Ok(true)
    })?;
    index.finish(cancelled)
}
fn discover(root: &Path, shared: &Shared, cache: &Path) -> Result<(Snapshot, String)> {
    let child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--deduplicate",
            "-z",
            "--",
            ".",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    if let Ok(child) = child {
        enumerate_git(child, root, shared, cache).and_then(|index| match index {
            Some(index) => Ok((index, String::new())),
            None => walk(root, cache, &|| stopped(shared))
                .map(|index| (index, "Ignore-aware filesystem discovery".into())),
        })
    } else {
        walk(root, cache, &|| stopped(shared)).map(|index| {
            (
                index,
                "Git unavailable; using ignore-aware filesystem discovery".into(),
            )
        })
    }
}
// Pipe reads and child reaping share a cancellable process-group supervisor.
fn enumerate_git(
    child: std::process::Child,
    root: &Path,
    shared: &Shared,
    cache: &Path,
) -> Result<Option<Snapshot>> {
    let excluded = discovery_exclusions(cache);
    let (status, snapshot) = read_git(child, shared, |output| {
        let mut snapshot = Snapshot::new();
        let mut reader = BufReader::new(output);
        let mut path = Vec::new();
        loop {
            path.clear();
            let n = (&mut reader)
                .take((SNAPSHOT_BYTES + 1) as u64)
                .read_until(0, &mut path)?;
            if n == 0 {
                break;
            }
            anyhow::ensure!(!stopped(shared), "File discovery cancelled");
            anyhow::ensure!(path.pop() == Some(0), "Invalid git filename stream");
            let relative = Path::new(std::ffi::OsStr::from_bytes(&path));
            if excluded
                .iter()
                .any(|path| root.join(relative).starts_with(path))
            {
                continue;
            }
            // Deleted tracked paths and submodule directories are not files.
            match fs::symlink_metadata(root.join(relative)) {
                Ok(metadata) if metadata.is_file() => snapshot.append(&path)?,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        snapshot.finish(&|| stopped(shared))
    })?;
    Ok(status.success().then_some(snapshot))
}

// Keep metadata-only Git probes subject to the same shutdown and timeout rules
// as filename discovery, including when a descendant holds the pipe open.
fn read_git<T>(
    mut child: std::process::Child,
    shared: &Shared,
    read: impl FnOnce(std::process::ChildStdout) -> Result<T>,
) -> Result<(std::process::ExitStatus, T)> {
    let output = child.stdout.take().expect("piped git output");
    // A separate guard kills/reaps Git even if its pipe stalls during shutdown.
    let process_group = nix::unistd::Pid::from_raw(child.id() as i32);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let guard = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(120);
            while !stop.load(Ordering::Relaxed) {
                if stopped(shared) || Instant::now() >= deadline {
                    let _ =
                        nix::sys::signal::killpg(process_group, nix::sys::signal::Signal::SIGKILL);
                    break;
                }
                std::thread::park_timeout(Duration::from_millis(50));
            }
        });
        let result = read(output);
        if result.is_err() {
            let _ = nix::sys::signal::killpg(process_group, nix::sys::signal::Signal::SIGKILL);
        }
        let status = loop {
            let status = child.try_wait();
            match status {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => break Err(error),
            }
        };
        stop.store(true, Ordering::Relaxed);
        guard.thread().unpark();
        Ok((status?, result?))
    })
}
fn cache_path(root: &Path, cache: &Path) -> PathBuf {
    cache.join(format!(
        "{:x}.index",
        Sha256::digest(root.as_os_str().as_bytes())
    ))
}
fn identity(root: &Path) -> Result<Vec<u8>> {
    let metadata = fs::metadata(root)?;
    let mut header = CACHE_MAGIC.to_vec();
    header.extend(metadata.dev().to_le_bytes());
    header.extend(metadata.ino().to_le_bytes());
    header.extend(root.as_os_str().as_bytes());
    header.push(0);
    Ok(header)
}
fn load_cache(root: &Path, cache: &Path, cancelled: &impl Fn() -> bool) -> Result<Snapshot> {
    let mut file = File::open(cache_path(root, cache))?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.uid() == rustix::process::geteuid().as_raw() && metadata.mode() & 0o077 == 0,
        "Filename cache is not private to this user"
    );
    let expected = identity(root)?;
    let mut header = vec![0; expected.len()];
    file.read_exact(&mut header)?;
    anyhow::ensure!(header == expected, "Workspace cache identity changed");
    let mut snapshot = Snapshot::new();
    let mut reader = BufReader::new(file);
    let mut path = Vec::new();
    loop {
        anyhow::ensure!(!cancelled(), "Cache loading cancelled");
        path.clear();
        let n = (&mut reader)
            .take((SNAPSHOT_BYTES + 1) as u64)
            .read_until(0, &mut path)?;
        if n == 0 {
            break;
        }
        anyhow::ensure!(path.pop() == Some(0), "Invalid filename cache");
        snapshot.append(&path)?;
    }
    snapshot.finish(cancelled)
}
fn save_cache(root: &Path, cache: &Path, snapshot: &Snapshot) -> Result<()> {
    fs::create_dir_all(cache)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(cache, fs::Permissions::from_mode(0o700))?;
    let mut bytes = identity(root)?;
    bytes.extend(&snapshot.arena);
    crate::store::atomic_write(&cache_path(root, cache), &bytes)
}
const MAX_WATCHES: usize = 8192;
const WATCH_BYTES: usize = 8 * 1024 * 1024;
fn add_watch_path(paths: &mut HashSet<PathBuf>, bytes: &mut usize, path: PathBuf) -> bool {
    if paths.contains(&path) {
        return true;
    }
    let size = path.as_os_str().len() + std::mem::size_of::<PathBuf>() * 2;
    if paths.len() == MAX_WATCHES || *bytes + size > WATCH_BYTES {
        return false;
    }
    *bytes += size;
    paths.insert(path);
    true
}
fn watch_paths(
    root: &Path,
    snapshot: &Snapshot,
    shared: &Shared,
    cache: &Path,
) -> (HashSet<PathBuf>, bool) {
    let mut paths = HashSet::new();
    let mut bytes = 0;
    let mut add = |path| add_watch_path(&mut paths, &mut bytes, path);
    add(root.to_path_buf());
    let git = root.join(".git");
    if git.is_dir() {
        add(git.clone());
        add(git.join("info"));
    } else if let Ok(text) = fs::read_to_string(git)
        && let Some(dir) = text.trim().strip_prefix("gitdir: ")
    {
        let dir = root.join(dir);
        add(dir.clone());
        if let Ok(common) = fs::read_to_string(dir.join("commondir")) {
            let common = dir.join(common.trim());
            add(common.clone());
            add(common.join("info"));
        }
    }
    // Parent ignore files and conventional global ignore/config locations can alter discovery.
    for parent in root.ancestors().skip(1) {
        for name in [".gitignore", ".ignore", ".rgignore"] {
            let path = parent.join(name);
            if path.is_file() {
                add(path);
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        for path in [
            PathBuf::from(&home).join(".gitconfig"),
            PathBuf::from(&home).join(".config/git/ignore"),
        ] {
            if path.is_file() {
                add(path);
            }
        }
    }
    // Parents of tracked-but-ignored files must be watched too.
    for id in &snapshot.paths {
        if stopped(shared) {
            return (paths, false);
        }
        let path = Path::new(std::ffi::OsStr::from_bytes(snapshot.path(*id)));
        for parent in path.ancestors().skip(1) {
            if !add(root.join(parent)) {
                return (paths, false);
            }
        }
    }
    // Directory discovery includes empty directories, without watching ignored build trees.
    // Filenames are already indexed; visiting entries here also bounds cancellation latency.
    for entry in discovery_walk(root, cache) {
        if stopped(shared) {
            return (paths, false);
        }
        match entry {
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_dir()) => {
                if !add(entry.into_path()) {
                    return (paths, false);
                }
            }
            Ok(_) => {}
            Err(_) => return (paths, false),
        }
    }
    (paths, true)
}
// Reuse the normal directory set when the active ignored scope changes. This
// traversal must not rediscover, sort or serialize the main filename corpus.
fn scope_watch_paths(
    root: &Path,
    scope: Option<&Path>,
    shared: &Shared,
    base: &HashSet<PathBuf>,
) -> (HashSet<PathBuf>, bool) {
    let mut paths = base.clone();
    let mut bytes = paths
        .iter()
        .map(|path| path.as_os_str().len() + std::mem::size_of::<PathBuf>() * 2)
        .sum();
    if let Some(scope) = scope {
        let mut complete = true;
        let result = (|| -> Result<()> {
            let root_fd = File::open(root)?;
            let scope_fd = rustix::fs::openat2(
                &root_fd,
                scope,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
                rustix::fs::ResolveFlags::BENEATH | rustix::fs::ResolveFlags::NO_SYMLINKS,
            )?;
            visit_scope(
                File::from(scope_fd),
                scope.to_path_buf(),
                &|| stopped(shared),
                |path, directory| {
                    if directory && !add_watch_path(&mut paths, &mut bytes, root.join(path)) {
                        complete = false;
                        return Ok(false);
                    }
                    Ok(true)
                },
            )
        })();
        if result.is_err() || !complete {
            return (paths, false);
        }
    }

    (paths, true)
}
// Unlike workspace directory entries, absent external ignore files can't be
// covered by our directory watches. Sample their metadata before discovery,
// including missing paths so creation/deletion and inode replacement invalidate.
fn policy_stamp(root: &Path, shared: &Shared) -> Result<u64> {
    let mut hash = DefaultHasher::new();
    let mut stat = |path: &Path| -> std::io::Result<()> {
        let stamp = match fs::metadata(path) {
            Ok(m) => Some((m.dev(), m.ino(), m.ctime(), m.ctime_nsec())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        (path, stamp).hash(&mut hash);
        Ok(())
    };
    stat(root)?;
    // These can appear/change outside the directory watches, including when
    // the selected workspace is a subdirectory of a repository.
    let mut repository = std::env::var_os("GIT_DIR").is_some();
    for ancestor in root.ancestors() {
        for name in [".ignore", ".rgignore", ".gitignore"] {
            stat(&ancestor.join(name))?;
        }
        repository |= ancestor.join(".git").try_exists()?;
    }
    if let Some(path) = ignore::gitignore::gitconfig_excludes_path() {
        stat(&path)?;
    }
    if repository {
        // Let Git resolve linked worktrees, subdirectory workspaces and config
        // includes. These queries inspect policy only; neither lists filenames.
        let probe = |args: &[&str]| -> Result<_> {
            let child = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()?;
            read_git(child, shared, |output| {
                let mut bytes = Vec::new();
                output.take(32 * 1024 + 1).read_to_end(&mut bytes)?;
                anyhow::ensure!(
                    bytes.len() <= 32 * 1024,
                    "Git policy paths exceeded probe budget"
                );
                Ok(bytes)
            })
        };
        let locations = probe(&[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-path",
            "index",
            "--git-path",
            "config",
            "--git-path",
            "config.worktree",
            "--git-path",
            "info/exclude",
        ]);
        let (status, bytes) = match locations {
            Ok(output) => output,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                // Discovery also uses the ignore-aware walker without Git.
                return Ok(hash.finish());
            }
            Err(e) => return Err(e),
        };
        anyhow::ensure!(status.success(), "Could not locate Git policy files");
        let paths: Vec<_> = bytes
            .strip_suffix(b"\n")
            .unwrap_or(&bytes)
            .split(|&b| b == b'\n')
            .map(|b| Path::new(std::ffi::OsStr::from_bytes(b)))
            .collect();
        // rev-parse has no NUL delimiter: ambiguous paths take the full fallback.
        let [worktree, index, config, worktree_config, exclude] = paths[..] else {
            anyhow::bail!("Ambiguous Git policy paths");
        };
        for path in [index, config, worktree_config, exclude] {
            stat(path)?;
        }
        let (status, bytes) = probe(&[
            "config",
            "--includes",
            "--null",
            "--path",
            "--get",
            "core.excludesFile",
        ])?;
        if status.success() {
            let path = bytes
                .strip_suffix(&[0])
                .ok_or_else(|| anyhow::anyhow!("Invalid Git exclude path"))?;
            stat(&worktree.join(std::ffi::OsStr::from_bytes(path)))?;
        } else {
            anyhow::ensure!(
                status.code() == Some(1),
                "Could not read Git ignore configuration"
            );
        }
    }
    Ok(hash.finish())
}

/// Directory ctime covers entry creation/removal/rename, not ordinary file
/// writes. Check ignore files too: in-place policy edits can change membership.
/// Compare to the *start* of discovery to cover the gap before watch setup.
fn watches_unchanged(watched: &HashSet<PathBuf>, started: SystemTime, shared: &Shared) -> bool {
    let Ok(since) = started.duration_since(SystemTime::UNIX_EPOCH) else {
        return false;
    };
    let before_scan = |m: &fs::Metadata| {
        (m.ctime(), m.ctime_nsec()) < (since.as_secs() as i64, i64::from(since.subsec_nanos()))
    };
    watched.iter().all(|path| {
        if stopped(shared) {
            return false;
        }
        let Ok(meta) = fs::metadata(path) else {
            return false;
        };
        before_scan(&meta)
            && (!meta.is_dir()
                || [".gitignore", ".ignore", ".rgignore"].iter().all(|name| {
                    match fs::metadata(path.join(name)) {
                        Ok(m) => before_scan(&m),
                        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
                    }
                }))
    })
}

fn discovery_worker(shared: Arc<Shared>, root: PathBuf, cache: PathBuf, reconcile: Duration) {
    let weak = Arc::downgrade(&shared);
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let relevant = match event {
            Ok(event) => {
                !matches!(event.kind, notify::EventKind::Access(_))
                    && (!matches!(
                        event.kind,
                        notify::EventKind::Modify(
                            notify::event::ModifyKind::Data(_)
                                | notify::event::ModifyKind::Metadata(_)
                        )
                    ) || event.paths.iter().any(|p| {
                        p.file_name().is_some_and(|n| {
                            [
                                ".gitignore",
                                ".ignore",
                                ".rgignore",
                                "index",
                                "exclude",
                                "config",
                                "commondir",
                            ]
                            .iter()
                            .any(|s| n == *s)
                        })
                    }))
            }
            Err(_) => true,
        };
        if relevant && let Some(shared) = weak.upgrade() {
            let mut state = shared.state.lock().expect("file index");
            state.dirty = Some(Instant::now());
            shared.wake.notify_all();
        }
    });
    let mut watch_error = watcher
        .as_ref()
        .err()
        .map(|e| format!("Filesystem watching unavailable ({e}); reconciling periodically."));
    let mut watcher = watcher.ok();
    let mut watched = HashSet::new();
    if let Ok(index) = load_cache(&root, &cache, &|| stopped(&shared)) {
        let mut state = shared.state.lock().expect("file index");
        state.snapshot = Some(Arc::new(index));
        state.generation += 1;
        state.revision += 1;
        state.status = "Cached filenames".into();
        shared.wake.notify_all();
    }
    let mut base_paths = HashSet::new();
    let mut base_complete = true;
    let mut status = String::new();
    let mut rebuild = true;
    let mut next = Instant::now();
    let mut baseline = None;
    loop {
        if stopped(&shared) {
            return;
        }
        let mut replacement = None;
        if rebuild {
            {
                let mut state = shared.state.lock().expect("file index");
                state.refreshing = true;
                state.force = false;
                state.dirty = None;
            }
            let started = SystemTime::now();
            let policy = policy_stamp(&root, &shared).ok();
            baseline = None;
            match discover(&root, &shared, &cache) {
                Ok((index, mut discovered_status)) => {
                    if stopped(&shared) {
                        return;
                    }
                    if watcher.is_some() {
                        (base_paths, base_complete) = watch_paths(&root, &index, &shared, &cache);
                    }
                    if let Err(error) = save_cache(&root, &cache, &index) {
                        discovered_status.push_str(&format!(
                            " Cache unavailable: {error:#}; live search remains available."
                        ));
                    } else {
                        baseline = policy.map(|stamp| (started, stamp));
                    }
                    status = discovered_status;
                    replacement = Some(index);
                }
                Err(error) => {
                    status = format!(
                        "File refresh failed: {error:#}. Retaining previous results; retrying automatically."
                    );
                }
            }
            next = Instant::now() + reconcile;
        }
        let scope = shared
            .state
            .lock()
            .expect("file index")
            .explicit_scope
            .clone();
        if let Some(watcher) = &mut watcher {
            let (desired, complete) =
                scope_watch_paths(&root, scope.as_deref(), &shared, &base_paths);
            watch_error = update_watches(watcher, &mut watched, desired, rebuild);
            if !complete || !base_complete {
                watch_error = Some("Directory-watch budget/availability exceeded; unwatched directories reconcile periodically.".into());
            }
        }
        let mut state = shared.state.lock().expect("file index");
        if let Some(index) = replacement {
            state.snapshot = Some(Arc::new(index));
            state.generation += 1;
        }
        // A scoped search may have run before its watches were installed. Rerun
        // it to cover that blind interval, without replacing the main snapshot.
        state.scope_revision += 1;
        state.revision += 1;
        state.refreshing = false;
        state.status = status.clone();
        if let Some(error) = &watch_error {
            state.status.push_str(error);
        }
        shared.wake.notify_all();
        loop {
            if stopped(&shared) {
                return;
            }
            let now = Instant::now();
            rebuild = state.force
                || state
                    .dirty
                    .is_some_and(|last| now.duration_since(last) >= DEBOUNCE || now >= next);
            if !rebuild && now >= next {
                drop(state);
                // Failed discovery, missing watches, or an inconclusive probe
                // take the full recovery path. Never do filesystem I/O under the lock.
                rebuild = watch_error.is_some()
                    || !baseline.is_some_and(|(started, original)| {
                        watches_unchanged(&watched, started, &shared)
                            && policy_stamp(&root, &shared).is_ok_and(|current| current == original)
                    });
                state = shared.state.lock().expect("file index");
                next = Instant::now() + reconcile;
                if !rebuild {
                    continue; // Recheck events/force/scope that arrived during the probe.
                }
            }
            if rebuild || state.explicit_scope != scope {
                break;
            }
            let deadline = state.dirty.map_or(next, |last| (last + DEBOUNCE).min(next));
            state = shared
                .wake
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("file index")
                .0;
        }
    }
}
// Reconciliation reinstalls watches to recover replaced inodes; scope-only
// updates keep shared/base watches and remove just the obsolete extension.
fn update_watches(
    watcher: &mut impl Watcher,
    watched: &mut HashSet<PathBuf>,
    desired: HashSet<PathBuf>,
    reinstall: bool,
) -> Option<String> {
    for path in watched.difference(&desired) {
        let _ = watcher.unwatch(path);
    }
    watched.retain(|path| desired.contains(path));
    let mut error = None;
    for path in desired {
        if watched.contains(&path) {
            if !reinstall {
                continue;
            }
            watched.remove(&path);
            let _ = watcher.unwatch(&path);
        }
        match watcher.watch(&path, RecursiveMode::NonRecursive) {
            Ok(()) => {
                watched.insert(path);
            }
            Err(e) if path.exists() => {
                error = Some(format!(
                    "Some directory watches failed ({e}); retrying during periodic reconciliation."
                ));
            }
            Err(_) => {}
        }
    }
    error
}
/// Disk caches are hints; absence of a cache directory never prevents live search.
pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".cache"))
        })
        .unwrap_or_else(|| {
            PathBuf::from(format!(
                "/tmp/pico-evo-cache-{}",
                rustix::process::geteuid().as_raw()
            ))
        })
        .join("pico-evo/files")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranking_long_paths_bytes_and_cancellation() {
        let long = format!("{}/main.rs", "deep/".repeat(100));
        let mut index = Snapshot::new();
        for path in [
            b"z/main.rs".as_slice(),
            b"a/main.rs.old",
            b"main.rs/final",
            b"q/remain.rs",
            b"a/MAIN.rs",
            long.as_bytes(),
            b"raw\xff",
        ] {
            index.append(path).unwrap();
        }
        let index = index.finish(&|| false).unwrap();
        let hits = index.search(b"main.rs", || false, |_| {}).unwrap();
        assert_eq!(hits[0], "@a/MAIN.rs");
        assert!(hits.iter().any(|s| s.len() > 288));
        assert_eq!(
            &hits[hits.len() - 3..],
            &["@a/main.rs.old", "@main.rs/final", "@q/remain.rs"]
        );
        assert_eq!(
            index.search(b"absent", || false, |_| {}).unwrap(),
            Vec::<String>::new()
        );
        assert!(index.search(b"x", || true, |_| {}).is_none());
        assert_eq!(
            crate::file_tags::mentions(&index.search(b"raw", || false, |_| {}).unwrap()[0])[0]
                .as_os_str()
                .as_bytes(),
            b"raw\xff"
        );
    }
    #[test]
    fn prefix_results_are_published_before_cancellable_substring_backfill() {
        let mut snapshot = Snapshot::new();
        for name in ["src/target.rs", "elsewhere/target.rs.more", "other.rs"] {
            snapshot.append(name.as_bytes()).unwrap();
        }
        let snapshot = snapshot.finish(&|| false).unwrap();
        let cancelled = std::cell::Cell::new(false);
        let mut prefix = Vec::new();
        let result = snapshot.search(
            b"target.rs",
            || cancelled.get(),
            |paths| {
                prefix = paths.to_vec();
                cancelled.set(true);
            },
        );
        assert_eq!(prefix, vec!["@src/target.rs", "@elsewhere/target.rs.more"]);
        assert!(
            result.is_none(),
            "the published prefix must not wait for the full scan"
        );
    }
    #[test]
    fn ignore_aware_discovery_cache_and_explicit_scope() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".gitignore"), "generated/\n").unwrap();
        fs::create_dir(root.path().join("generated")).unwrap();
        fs::write(root.path().join("generated/hidden"), "x").unwrap();
        fs::write(root.path().join("visible"), "x").unwrap();
        let index = walk(root.path(), cache.path(), &|| false).unwrap();
        assert!(
            index
                .search(b"hidden", || false, |_| {})
                .unwrap()
                .is_empty()
        );
        let scoped = walk_scope(
            File::open(root.path().join("generated")).unwrap(),
            PathBuf::from("generated"),
            &|| false,
        )
        .unwrap();
        assert_eq!(
            scoped.search(b"hidden", || false, |_| {}).unwrap(),
            vec!["@generated/hidden"]
        );
        save_cache(root.path(), cache.path(), &index).unwrap();
        let loaded = load_cache(root.path(), cache.path(), &|| false).unwrap();
        assert_eq!(
            loaded.search(b"vis", || false, |_| {}),
            Some(vec!["@visible".into()])
        );
        fs::write(cache_path(root.path(), cache.path()), "broken").unwrap();
        assert!(load_cache(root.path(), cache.path(), &|| false).is_err());
    }
    #[test]
    fn scoped_walk_pins_the_checked_directory_and_cache_is_not_discovered() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("scope")).unwrap();
        fs::write(root.path().join("scope/inside"), "x").unwrap();
        fs::write(external.path().join("secret"), "x").unwrap();
        fs::create_dir(external.path().join("external-child")).unwrap();
        let pinned = File::open(root.path().join("scope")).unwrap();
        fs::rename(root.path().join("scope"), root.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(external.path(), root.path().join("scope")).unwrap();
        let index = walk_scope(pinned, PathBuf::from("scope"), &|| false).unwrap();
        assert_eq!(
            index.search(b"", || false, |_| {}).unwrap(),
            vec!["@scope/inside"]
        );
        let watch_cache = tempfile::tempdir().unwrap();
        let service = Service::open(root.path().into(), watch_cache.path().into()).unwrap();
        let (base, _) = watch_paths(root.path(), &index, &service.shared, watch_cache.path());
        let (watched, complete) = scope_watch_paths(
            root.path(),
            Some(Path::new("scope")),
            &service.shared,
            &base,
        );
        assert!(!complete);
        // Paths from a stale index can include the old scope parent, but collection
        // must not descend through its replacement symlink to external children.
        assert!(!watched.contains(&root.path().join("scope/external-child")));
        let cache = root.path().join("file-cache");
        fs::create_dir(&cache).unwrap();
        fs::write(cache.join("private"), "paths").unwrap();
        let index = walk(root.path(), &cache, &|| false).unwrap();
        assert!(
            index
                .search(b"private", || false, |_| {})
                .unwrap()
                .is_empty()
        );
        let mut ids: Vec<u32> = (0..5003).rev().collect();
        sort_ids(&mut ids, u32::cmp, &|| false).unwrap();
        assert_eq!(ids, (0..5003).collect::<Vec<_>>());
        assert!(sort_ids(&mut ids, u32::cmp, &|| true).is_err());
    }
    #[tokio::test]
    async fn latest_request_refresh_and_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        fs::write(root.path().join("first"), "body").unwrap();
        fs::create_dir(root.path().join("empty")).unwrap();
        let service = Service::open(root.path().into(), cache.path().into()).unwrap();
        let mut rx = service.subscribe();
        service.query(1, b"wrong".to_vec(), false);
        service.query(2, b"first".to_vec(), false);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                rx.changed().await.unwrap();
                let r = rx.borrow_and_update().clone();
                if r.request == 2 && r.phase != SearchPhase::Pending && !r.paths.is_empty() {
                    assert_eq!(r.paths, vec!["@first"]);
                    break;
                }
            }
            fs::write(root.path().join("empty/second"), "body").unwrap();
            service.query(3, b"second".to_vec(), false);
            // No explicit refresh: an existing empty directory must be watched.
            loop {
                rx.changed().await.unwrap();
                let r = rx.borrow_and_update().clone();
                if r.request == 3
                    && r.phase != SearchPhase::Pending
                    && r.paths == vec!["@empty/second"]
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        drop(service);
        tokio::time::timeout(Duration::from_secs(10), async {
            while rx.changed().await.is_ok() {}
        })
        .await
        .unwrap();
    }
    async fn until(
        rx: &mut tokio::sync::watch::Receiver<Response>,
        check: impl Fn(&Response) -> bool,
    ) -> Response {
        tokio::time::timeout(Duration::from_secs(10), async {
            rx.wait_for(check).await.unwrap().clone()
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn clean_reconciliation_reuses_index_but_unwatched_ignore_change_rebuilds() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(root.path())
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        let excludes = external.path().join("excludes");
        fs::write(&excludes, "other-name\n").unwrap();
        git(&["config", "core.excludesFile", excludes.to_str().unwrap()]);
        fs::write(root.path().join("visible"), "before").unwrap();
        let interval = Duration::from_millis(100);
        let service = Service::start(root.path().into(), cache.path().into(), interval).unwrap();
        let mut rx = service.subscribe();
        service.query(1, b"visible".to_vec(), false);
        let first = until(&mut rx, |r| {
            r.phase != SearchPhase::Pending && r.paths == ["@visible"]
        })
        .await;
        let cached = || {
            fs::metadata(cache_path(root.path(), cache.path()))
                .unwrap()
                .ino()
        };
        let inode = cached();
        fs::write(root.path().join("visible"), "ordinary content edit").unwrap();
        tokio::time::sleep(interval * 4).await;
        service.query(2, b"visible".to_vec(), false);
        let idle = until(&mut rx, |r| {
            r.request == 2 && r.phase != SearchPhase::Pending
        })
        .await;
        assert_eq!(idle.generation, first.generation);
        assert_eq!(
            cached(),
            inode,
            "idle checks must not replace the cache file"
        );

        // No event in any watched directory: the periodic safety check must
        // still notice an external policy edit and rerun the active query.
        fs::write(excludes, "visible\n").unwrap();
        until(&mut rx, |r| {
            r.generation > idle.generation && r.phase != SearchPhase::Pending && r.paths.is_empty()
        })
        .await;
    }

    #[tokio::test]
    async fn ignored_scope_changes_keep_main_index_and_refresh_live() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(".gitignore"),
            "generated-a/\ngenerated-b/\n",
        )
        .unwrap();
        for dir in ["generated-a", "generated-b"] {
            fs::create_dir_all(root.path().join(dir).join("nested")).unwrap();
            fs::write(root.path().join(dir).join("nested/first"), "x").unwrap();
        }
        fs::write(root.path().join("visible"), "x").unwrap();
        let service = Service::open(root.path().into(), cache.path().into()).unwrap();
        let mut rx = service.subscribe();
        service.query(1, b"visible".to_vec(), false);
        let initial = until(&mut rx, |r| {
            r.phase != SearchPhase::Pending && r.paths == ["@visible"]
        })
        .await;
        for (request, scope) in [(2, "generated-a"), (3, "generated-b")] {
            let previous = service.shared.state.lock().unwrap().scope_revision;
            service.query(request, format!("{scope}/").into_bytes(), true);
            until(&mut rx, |r| {
                r.request == request
                    && r.phase != SearchPhase::Pending
                    && r.paths == [format!("@{scope}/nested/first")]
                    && service.shared.state.lock().unwrap().scope_revision > previous
            })
            .await;
            assert_eq!(
                service.shared.state.lock().unwrap().generation,
                initial.generation,
                "scope changes must not replace the main filename index"
            );
        }
        // Watch setup has completed. A nested ignored-directory event must update
        // the current query without a manual refresh or a new query request.
        fs::write(root.path().join("generated-b/nested/second"), "x").unwrap();
        let refreshed = until(&mut rx, |r| {
            r.request == 3
                && r.phase != SearchPhase::Pending
                && r.paths == ["@generated-b/nested/first", "@generated-b/nested/second"]
        })
        .await;
        let previous = service.shared.state.lock().unwrap().scope_revision;
        service.cancel_query();
        // There is no query to publish a response to while cancelled. Wait on the
        // worker's existing completion signal rather than sleeping for a debounce.
        {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut state = service.shared.state.lock().unwrap();
            assert!(state.explicit_scope.is_none());
            while state.scope_revision == previous {
                let now = Instant::now();
                assert!(now < deadline, "cancelled scope watches must be released");
                state = service
                    .shared
                    .wake
                    .wait_timeout(state, deadline - now)
                    .unwrap()
                    .0;
            }
            assert_eq!(state.generation, refreshed.generation);
        }
        // Reopening the same scope must not reuse its pre-cancellation cache.
        fs::write(root.path().join("generated-b/nested/third"), "x").unwrap();
        service.query(4, b"generated-b/".to_vec(), true);
        until(&mut rx, |r| {
            r.request == 4
                && r.phase != SearchPhase::Pending
                && r.paths
                    == [
                        "@generated-b/nested/first",
                        "@generated-b/nested/second",
                        "@generated-b/nested/third",
                    ]
        })
        .await;
    }
    #[test]
    fn cancelling_scope_watches_preserves_shared_base_directories() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        for dir in ["generated/shared", "generated/scoped-only"] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        let service = Service::open(root.path().into(), cache.path().into()).unwrap();
        // Model a tracked file inside an otherwise ignored tree: its parent watch
        // belongs to the base corpus as well as to the explicit scope.
        let base = HashSet::from([
            root.path().to_path_buf(),
            root.path().join("generated/shared"),
        ]);
        let (desired, complete) = scope_watch_paths(
            root.path(),
            Some(Path::new("generated")),
            &service.shared,
            &base,
        );
        assert!(complete);
        let mut watcher = notify::recommended_watcher(|_| {}).unwrap();
        let mut watched = HashSet::new();
        assert!(update_watches(&mut watcher, &mut watched, desired, false).is_none());
        assert!(watched.contains(&root.path().join("generated/scoped-only")));
        service.query(1, b"generated/".to_vec(), true);
        service.cancel_query();
        let scope = service.shared.state.lock().unwrap().explicit_scope.clone();
        let (desired, complete) =
            scope_watch_paths(root.path(), scope.as_deref(), &service.shared, &base);
        assert!(complete);
        assert!(update_watches(&mut watcher, &mut watched, desired, false).is_none());
        assert_eq!(watched, base);
        // Check the actual watcher too, not just our bookkeeping.
        assert!(
            watcher
                .unwatch(&root.path().join("generated/scoped-only"))
                .is_err()
        );
        assert!(
            watcher
                .unwatch(&root.path().join("generated/shared"))
                .is_ok()
        );
    }
    #[tokio::test]
    async fn failed_refresh_keeps_results_and_root_replacement_recovers_watching() {
        let parent = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("original"), "x").unwrap();
        let service = Service::start(
            root.clone(),
            cache.path().into(),
            Duration::from_millis(200),
        )
        .unwrap();
        let mut rx = service.subscribe();
        service.query(1, vec![], false);
        until(&mut rx, |r| {
            r.paths == vec!["@original"] && r.phase != SearchPhase::Pending
        })
        .await;
        fs::rename(&root, parent.path().join("old")).unwrap();
        service.refresh();
        let failed = until(&mut rx, |r| {
            r.status.contains("refresh failed") && r.phase != SearchPhase::Pending
        })
        .await;
        assert_eq!(failed.paths, vec!["@original"]);
        fs::create_dir(&root).unwrap();
        fs::write(root.join("replacement"), "x").unwrap();
        // Failed discovery must retry without a watch on the new inode.
        until(&mut rx, |r| {
            r.paths == vec!["@replacement"] && r.phase != SearchPhase::Pending
        })
        .await;
        // No manual refresh: this must be a watch on the replacement inode.
        fs::write(root.join("added"), "x").unwrap();
        until(&mut rx, |r| {
            r.paths == vec!["@added", "@replacement"] && r.phase != SearchPhase::Pending
        })
        .await;
    }
    #[test]
    fn git_shutdown_kills_closed_stdout_and_inherited_pipe_children() {
        for script in [
            "exec 1>&-; echo ready >&2; sleep 10",
            "sleep 10 & echo ready >&2; exit 0",
        ] {
            let root = tempfile::tempdir().unwrap();
            let cache = tempfile::tempdir().unwrap();
            let service = Service::open(root.path().into(), cache.path().into()).unwrap();
            let mut child = Command::new("sh")
                .args(["-c", script])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .spawn()
                .unwrap();
            let mut ready = String::new();
            BufReader::new(child.stderr.take().unwrap())
                .read_line(&mut ready)
                .unwrap();
            assert_eq!(ready, "ready\n");
            let shared = service.shared.clone();
            let root = root.path().to_path_buf();
            let cache = cache.path().to_path_buf();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = enumerate_git(child, &root, &shared, &cache);
                let _ = tx.send(result.is_ok());
            });
            // Let the supervisor enter its blocking pipe/reap path before shutdown.
            std::thread::sleep(Duration::from_millis(100));
            drop(service);
            rx.recv_timeout(Duration::from_secs(5))
                .expect("Git cancellation must not wait for descendants or a mutex held by wait()");
        }
    }
    #[test]
    #[ignore = "opt-in deep/Unicode million-path capacity and latency benchmark"]
    fn deep_paths_benchmark() {
        let count = 1_000_000;
        let start = Instant::now();
        let mut index = Snapshot::new();
        for i in 0..count {
            let component = if i % 7 == 0 {
                "international/日本語/λ-module"
            } else {
                "services/application/components"
            };
            index.append(format!("packages/team-{}/projects/{component}/src/implementation/feature-{}/nested/production/component-{i:07}.rs",i%97,i%1231).as_bytes()).unwrap();
        }
        let index = index.finish(&|| false).unwrap();
        assert_eq!(index.paths.len(), count);
        println!(
            "deep_paths={count} build_ms={} arena_bytes={} retained_bytes={}",
            start.elapsed().as_millis(),
            index.arena.len(),
            index.arena.len()
                + index.entries.len() * std::mem::size_of::<Entry>()
                + (index.paths.len() + index.names.len()) * std::mem::size_of::<u32>()
        );
        for query in ["component-0999999.rs", "not-in-this-workspace"] {
            let start = Instant::now();
            let mut first = None;
            let hits = index
                .search(
                    query.as_bytes(),
                    || false,
                    |_| {
                        first.get_or_insert_with(|| start.elapsed().as_secs_f64() * 1000.);
                    },
                )
                .unwrap();
            println!(
                "query={query} complete_ms={:.3} first_batch_ms={:.3} hits={}",
                start.elapsed().as_secs_f64() * 1000.,
                first.unwrap_or_else(|| start.elapsed().as_secs_f64() * 1000.),
                hits.len()
            );
            if query == "component-0999999.rs" {
                assert_eq!(hits.len(), 1);
                assert!(hits[0].ends_with("component-0999999.rs"));
            } else {
                assert!(hits.is_empty());
            }
        }
    }
    #[tokio::test]
    #[ignore = "opt-in cold discovery and warm cache benchmark"]
    async fn workspace_benchmark() {
        let fixture = tempfile::tempdir().unwrap();
        let root = if let Some(root) = std::env::var_os("PICO_FILE_BENCH_WORKSPACE") {
            fs::canonicalize(root).unwrap()
        } else {
            for dir in 0..100 {
                let path = fixture.path().join(format!("dir{dir}"));
                fs::create_dir(&path).unwrap();
                for file in 0..100 {
                    fs::write(path.join(format!("file{file}.rs")), "fixture").unwrap();
                }
            }
            fixture.path().to_path_buf()
        };
        for sample in 1..=3 {
            let cache = tempfile::tempdir().unwrap();
            for phase in ["cold", "cached"] {
                let start = Instant::now();
                let service = Service::open(root.clone(), cache.path().into()).unwrap();
                let mut rx = service.subscribe();
                service.query(1, vec![], false);
                let response = tokio::time::timeout(Duration::from_secs(120), async {
                    rx.wait_for(|r| {
                        r.request == 1 && r.phase != SearchPhase::Pending && !r.paths.is_empty()
                    })
                    .await
                    .unwrap()
                    .clone()
                })
                .await
                .unwrap();
                let elapsed = start.elapsed().as_secs_f64() * 1000.;
                let files = service
                    .shared
                    .state
                    .lock()
                    .unwrap()
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .paths
                    .len();
                println!(
                    "sample={sample} phase={phase} files={files} first_results_ms={elapsed:.3} status={}",
                    response.status
                );
                drop(service);
                tokio::time::timeout(Duration::from_secs(5), async {
                    while rx.changed().await.is_ok() {}
                })
                .await
                .unwrap();
            }
        }
    }
    #[test]
    #[ignore = "opt-in filename search benchmark"]
    fn benchmark() {
        for sample in 1..=3 {
            for count in [10_000, 100_000, 1_000_000] {
                let start = Instant::now();
                let mut index = Snapshot::new();
                for i in 0..count {
                    index
                        .append(format!("d{}/file{i:07}.rs", i % 137).as_bytes())
                        .unwrap();
                }
                let index = index.finish(&|| false).unwrap();
                println!(
                    "sample={sample} paths={count} build_ms={} bytes={}",
                    start.elapsed().as_millis(),
                    index.arena.len()
                        + index.entries.len() * std::mem::size_of::<Entry>()
                        + (index.paths.len() + index.names.len()) * std::mem::size_of::<u32>()
                );
                for query in ["file0001234", "d13/", "1234", "no-match"] {
                    let mut times = Vec::new();
                    let mut first_batches = Vec::new();
                    let mut found = 0;
                    for _ in 0..7 {
                        let start = Instant::now();
                        let mut first = None;
                        let hits = index
                            .search(
                                query.as_bytes(),
                                || false,
                                |_| {
                                    first.get_or_insert_with(|| {
                                        start.elapsed().as_secs_f64() * 1000.
                                    });
                                },
                            )
                            .unwrap();
                        first_batches
                            .push(first.unwrap_or_else(|| start.elapsed().as_secs_f64() * 1000.));
                        times.push(start.elapsed().as_secs_f64() * 1000.);
                        found = hits.len();
                    }
                    times.sort_by(f64::total_cmp);
                    first_batches.sort_by(f64::total_cmp);
                    println!(
                        "sample={sample} paths={count} query={query} median_ms={:.3} first_batch_ms={:.3} hits={found}",
                        times[3], first_batches[3]
                    );
                }
            }
        }
    }
}
