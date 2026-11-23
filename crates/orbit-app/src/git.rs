pub mod changes;
use std::{
    ffi::OsStr,
    fmt,
    io::{BufRead, Read},
    num::NonZeroUsize,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub enum Info {
    Changes {
        branch: String,
        added: u64,
        removed: u64,
    },
    Unavailable(String),
}
impl fmt::Display for Info {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changes {
                branch,
                added,
                removed,
            } => {
                f.write_str(branch)?;
                if *added != 0 || *removed != 0 {
                    write!(f, "  +{added} −{removed}")?;
                }
                Ok(())
            }
            Self::Unavailable(message) => f.write_str(message),
        }
    }
}

// Counts are properties of a file version, not of a branch or a path. Device
// and inode distinguish replacements; ctime also catches rewrites with a
// restored mtime. No paths or file contents are retained.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct FileVersion {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl From<&std::fs::Metadata> for FileVersion {
    fn from(m: &std::fs::Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        }
    }
}

struct CachedCount {
    lines: u64,
    last_used: Instant,
}

pub struct LineCounts(Mutex<lru::LruCache<FileVersion, CachedCount>>);

impl Default for LineCounts {
    fn default() -> Self {
        Self(Mutex::new(lru::LruCache::new(
            NonZeroUsize::new(8192).unwrap(),
        )))
    }
}

#[cfg(test)]
pub async fn info(path: PathBuf, counts: Arc<LineCounts>) -> Option<Info> {
    capture(path, counts, false).await.0
}

type Capture = (Option<Info>, Option<Arc<changes::Files>>);

pub async fn capture(path: PathBuf, counts: Arc<LineCounts>, detailed: bool) -> Capture {
    let deadline = Instant::now() + Duration::from_secs(15);
    match tokio::time::timeout_at(
        deadline.into(),
        read_info(&path, deadline, counts, detailed),
    )
    .await
    {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(error)) => (Some(Info::Unavailable(error)), None),
        Err(_) => (
            Some(Info::Unavailable(
                "Git status timed out; will retry automatically".into(),
            )),
            None,
        ),
    }
}

pub(crate) fn local_command(dir: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    // Launching Orbit from a hook or another checkout must not redirect
    // mutations to that repository (or its index/object store).
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_SHALLOW_FILE",
        "GIT_GRAFT_FILE",
        "GIT_PREFIX",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_REPLACE_REF_BASE",
    ] {
        cmd.env_remove(key);
    }
    cmd.kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .current_dir(dir)
        .args(["--no-optional-locks", "-c", "core.hooksPath=/dev/null"])
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    cmd
}
async fn git(path: &Path, args: &[&str]) -> Result<Output, String> {
    local_command(path).args(args).output().await.map_err(|e| {
        format!("Could not run Git: {e}. Check that Git is installed; will retry automatically.")
    })
}
/// Sidebar rows need only refs, never a diff or an untracked-file scan.
pub async fn sidebar_branch(path: &Path) -> Option<String> {
    let head = git(path, &["symbolic-ref", "--quiet", "HEAD"]).await.ok()?;
    let branch = head
        .stdout
        .strip_suffix(b"\n")?
        .strip_prefix(b"refs/heads/")?;
    let origin = git(
        path,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )
    .await
    .ok()?;
    let is_default = if let Some(default) = origin
        .stdout
        .strip_suffix(b"\n")
        .and_then(|r| r.strip_prefix(b"refs/remotes/origin/"))
    {
        branch == default
    } else {
        match branch {
            b"main" => true,
            b"master" => !git(
                path,
                &["show-ref", "--verify", "--quiet", "refs/heads/main"],
            )
            .await
            .ok()?
            .status
            .success(),
            _ => false,
        }
    };
    (!is_default).then(|| String::from_utf8_lossy(branch).into_owned())
}

fn successful(output: Output) -> Result<Vec<u8>, String> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "Could not read Git changes: {}. Will retry automatically.",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
async fn read_info(
    path: &Path,
    deadline: Instant,
    counts: Arc<LineCounts>,
    detailed: bool,
) -> Result<Capture, String> {
    let root = git(path, &["rev-parse", "--show-toplevel"]).await?;
    // Not being a repository is normal for a workspace. Keep other failures
    // (permissions, unsafe ownership, missing Git, etc.) visible and retryable.
    if !root.status.success()
        && root
            .stderr
            .starts_with(b"fatal: not a git repository (or any")
    {
        return Ok((None, None));
    }
    let root = successful(root)?;
    let root = Path::new(OsStr::from_bytes(root.strip_suffix(b"\n").unwrap_or(&root)));
    let head = git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]).await?;
    let branch = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    let branch = if branch.status.success() {
        String::from_utf8_lossy(&branch.stdout)
            .trim_end()
            .to_owned()
    } else {
        let head = successful(git(root, &["rev-parse", "--short", "HEAD"]).await?)?;
        format!("{} (detached)", String::from_utf8_lossy(&head).trim_end())
    };
    // An unborn branch is diffed against an empty tree. hash-object without -w
    // computes its ID without changing the repository or assuming SHA-1.
    let base = if head.status.success() {
        head.stdout
    } else {
        successful(git(root, &["hash-object", "-t", "tree", "--stdin"]).await?)?
    };
    let base = String::from_utf8(base)
        .map_err(|e| e.to_string())?
        .trim_end()
        .to_owned();
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--numstat",
        "-z",
    ];
    if detailed {
        args.push("--raw");
    }
    args.extend([base.as_str(), "--"]);
    let diff = successful(git(root, &args).await?)?;
    let untracked =
        successful(git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).await?)?;
    let root = root.to_owned();
    let (added, removed, files) = tokio::task::spawn_blocking(move || {
        let mut files = detailed.then(|| changes::Files::new(root.clone(), base));
        let stats = if let Some(files) = files.as_mut() {
            files.read_status(&diff)?
        } else {
            &diff
        };
        let (mut added, removed) = numstat(stats, files.as_mut())?;
        added += untracked_lines(&root, &untracked, deadline, &counts, files.as_mut())?;
        Ok::<_, String>((added, removed, files.map(Arc::new)))
    })
    .await
    .map_err(|e| format!("Could not read changed files: {e}; will retry automatically"))??;
    Ok((
        Some(Info::Changes {
            branch,
            added,
            removed,
        }),
        files,
    ))
}

fn numstat(bytes: &[u8], mut files: Option<&mut changes::Files>) -> Result<(u64, u64), String> {
    let mut added = 0;
    let mut removed = 0;
    for record in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let mut fields = record.splitn(3, |b| *b == b'\t');
        let mut count = || -> Result<u64, String> {
            let value = fields.next().ok_or("Missing Git diff count")?;
            if value == b"-" {
                return Ok(0); // Git reports binary files without line counts.
            }
            std::str::from_utf8(value)
                .ok()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| "Invalid Git diff count; will retry automatically".into())
        };
        let (a, d) = (count()?, count()?);
        added += a;
        removed += d;
        if let Some(files) = files.as_mut() {
            let name = fields.next().ok_or("Missing Git diff path")?;
            files.set_counts(Path::new(OsStr::from_bytes(name)), a, d)?;
        }
    }
    Ok((added, removed))
}

fn untracked_lines(
    root: &Path,
    paths: &[u8],
    deadline: Instant,
    counts: &LineCounts,
    mut files: Option<&mut changes::Files>,
) -> Result<u64, String> {
    let root_fd = std::fs::File::open(root).map_err(|e| e.to_string())?;
    let scan_start = Instant::now();
    let mut total = 0;
    for name in paths.split(|b| *b == 0).filter(|name| !name.is_empty()) {
        if Instant::now() >= deadline {
            return Err("Git status timed out; will retry automatically".into());
        }
        let path = root.join(OsStr::from_bytes(name));
        let relative = Path::new(OsStr::from_bytes(name));
        let count = || -> std::io::Result<(u64, FileVersion)> {
            let pinned = pin_untracked(&root_fd, relative)?;
            let metadata = pinned.metadata()?;
            let version = FileVersion::from(&metadata);
            if metadata.file_type().is_symlink() {
                return Ok((1, version)); // Git stores the link target as a single line.
            }
            if !metadata.is_file() {
                return Ok((0, version));
            }
            if let Some(hit) = counts.0.lock().expect("line counts").get_mut(&version) {
                hit.last_used = Instant::now();
                return Ok((hit.lines, version));
            }
            let file = read_pinned(&pinned)?;
            let lines = count_lines(&file, deadline)?;
            // Never memoize a partial scan or a file replaced/edited while we
            // read it. Check the open descriptor, not a new lookup by pathname.
            if FileVersion::from(&file.metadata()?) == version {
                let mut cache = counts.0.lock().expect("line counts");
                // Once even the oldest resident has been used during this scan,
                // the working set fills the cache. Count overflow without
                // displacing the entries the next poll is about to need.
                if cache.len() < cache.cap().get()
                    || cache
                        .peek_lru()
                        .is_some_and(|(_, oldest)| oldest.last_used < scan_start)
                {
                    cache.put(
                        version,
                        CachedCount {
                            lines,
                            last_used: Instant::now(),
                        },
                    );
                }
            }
            Ok((lines, version))
        };
        match count() {
            Ok((lines, stamp)) => {
                total += lines;
                if let Some(files) = files.as_mut() {
                    files.add_untracked(relative, lines, stamp);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // Removed during refresh.
            Err(e) => {
                return Err(format!(
                    "Could not count {}: {e}; will retry automatically",
                    path.display()
                ));
            }
        }
    }
    Ok(total)
}

// Pin the directory entry itself. In particular, a symlink replacement of an
// ancestor must not redirect a refresh or a diff to a file outside this checkout.
fn pin_untracked(root: &std::fs::File, path: &Path) -> std::io::Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags, ResolveFlags, openat2};
    Ok(openat2(
        root,
        path,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    )?
    .into())
}
fn read_pinned(file: &std::fs::File) -> std::io::Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    std::fs::File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

fn count_lines(file: &std::fs::File, deadline: Instant) -> std::io::Result<u64> {
    let mut reader = std::io::BufReader::new(file);
    // Match Git's binary-content heuristic (NUL in the first 8000 bytes).
    let first = reader.fill_buf()?;
    if first[..first.len().min(8000)].contains(&0) {
        return Ok(0);
    }
    let mut lines = 0;
    let mut last = None;
    let mut buffer = [0; 8192];
    loop {
        if Instant::now() >= deadline {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        lines += buffer[..n].iter().filter(|b| **b == b'\n').count() as u64;
        last = Some(buffer[n - 1]);
    }
    Ok(lines + u64::from(last.is_some_and(|b| b != b'\n')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn sidebar_branch_falls_back_to_main_then_master_without_origin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        assert_eq!(sidebar_branch(path).await, None);
        command(path, &["init", "--template=", "-b", "master"]);
        command(path, &["commit", "--allow-empty", "-m", "base"]);
        assert_eq!(sidebar_branch(path).await, None);
        command(path, &["branch", "main"]);
        assert_eq!(sidebar_branch(path).await.as_deref(), Some("master"));
        command(path, &["switch", "main"]);
        assert_eq!(sidebar_branch(path).await, None);
        command(path, &["checkout", "--detach"]);
        assert_eq!(sidebar_branch(path).await, None);
    }

    #[tokio::test]
    async fn diff_counts_net_lines_and_untracked_text_from_the_repository_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        command(root, &["init", "--template=", "--initial-branch=main"]);
        std::fs::write(root.join("tracked"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(root.join("deleted"), "a\nb\n").unwrap();
        std::fs::write(root.join("odd\tname\n.txt"), "old\n").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        command(root, &["add", "."]);
        command(root, &["commit", "-m", "initial"]);
        std::fs::write(root.join("tracked"), "ONE\ntwo\nthree\n").unwrap();
        command(root, &["add", "tracked"]);
        std::fs::write(root.join("tracked"), "one\ntwo\nthree\nfour\n").unwrap();
        std::fs::remove_file(root.join("deleted")).unwrap();
        std::fs::write(root.join("odd\tname\n.txt"), "new\nextra\n").unwrap();
        std::fs::write(root.join("staged"), "new\nfile\n").unwrap();
        command(root, &["add", "staged"]);
        std::fs::create_dir(root.join("subdir")).unwrap();
        std::fs::write(root.join("subdir/new\tfile\n.txt"), "first\nlast").unwrap();
        std::fs::write(root.join("binary"), b"\0a\nb\n").unwrap();
        std::fs::write(root.join("empty"), b"").unwrap();
        std::fs::write(root.join("ignored"), "not\ncounted\n").unwrap();
        let result = info(root.join("subdir"), Arc::default()).await;
        match result {
            Some(Info::Changes {
                branch,
                added,
                removed,
            }) => {
                assert_eq!(branch, "main");
                assert_eq!(added, 7); // net tracked +1, replacement +2, staged +2, untracked +2
                assert_eq!(removed, 3); // deleted file -2, replacement -1
            }
            other => panic!("Expected Git changes, got {other:?}"),
        }
        command(root, &["checkout", "--detach", "-f", "HEAD"]);
        assert!(
            matches!(info(root.to_owned(), Arc::default()).await, Some(Info::Changes { branch, .. }) if branch.ends_with(" (detached)"))
        );
    }

    #[tokio::test]
    async fn unchanged_untracked_files_are_not_read_and_edits_update_the_total() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        command(root, &["init", "--template=", "--initial-branch=main"]);
        let text = root.join("untracked.txt");
        let binary = root.join("blob");
        std::fs::write(&text, "a\nb\nc").unwrap();
        std::fs::write(&binary, b"abc\0").unwrap();
        let modified = text.metadata().unwrap().modified().unwrap();

        // Observe real reads, including the binary sniff, rather than cache internals.
        let mut watcher = inotify::Inotify::init().unwrap();
        let text_watch = watcher
            .watches()
            .add(&text, inotify::WatchMask::ACCESS)
            .unwrap();
        let binary_watch = watcher
            .watches()
            .add(&binary, inotify::WatchMask::ACCESS)
            .unwrap();
        let mut read_files = || {
            let mut buffer = [0; 4096];
            match watcher.read_events(&mut buffer) {
                Ok(events) => events
                    .map(|e| e.wd)
                    .collect::<std::collections::HashSet<_>>(),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Default::default(),
                Err(e) => panic!("Could not observe fixture reads: {e}"),
            }
        };
        let counts = Arc::default();
        let refresh = || info(root.to_owned(), Arc::clone(&counts));
        assert!(matches!(
            refresh().await,
            Some(Info::Changes { added: 3, .. })
        ));
        assert_eq!(
            read_files(),
            [text_watch.clone(), binary_watch.clone()].into()
        );
        assert!(matches!(
            refresh().await,
            Some(Info::Changes { added: 3, .. })
        ));
        assert!(
            read_files().is_empty(),
            "a warm refresh should read neither file"
        );

        // Same inode, size and mtime, different number of lines.
        std::fs::write(&text, "abcde").unwrap();
        std::fs::File::open(&text)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert!(matches!(
            refresh().await,
            Some(Info::Changes { added: 1, .. })
        ));
        assert_eq!(read_files(), [text_watch].into());

        // An atomic same-size replacement changes binary classification; removing
        // the other untracked file must remove its contribution, even if cached.
        let replacement = root.join("replacement");
        std::fs::write(&replacement, "x\ny\n").unwrap();
        std::fs::rename(&replacement, &binary).unwrap();
        std::fs::remove_file(text).unwrap();
        assert!(matches!(
            refresh().await,
            Some(Info::Changes { added: 2, .. })
        ));
    }

    #[test]
    fn repeated_overflow_scans_read_only_excess_files_and_reuse_vacated_slots() {
        use inotify::{Inotify, WatchMask};
        use std::collections::HashSet;

        let dir = tempfile::tempdir().unwrap();
        for (name, text) in [("a", "1"), ("b", "1\n2"), ("c", "1\n2\n3"), ("d", "four\n")] {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        let counts = LineCounts(Mutex::new(lru::LruCache::new(
            NonZeroUsize::new(3).unwrap(),
        )));
        let mut watcher = Inotify::init().unwrap();
        watcher
            .watches()
            .add(dir.path(), WatchMask::ACCESS)
            .unwrap();
        let mut scan = |paths: &[u8], expected| {
            let total = untracked_lines(
                dir.path(),
                paths,
                Instant::now() + Duration::from_secs(15),
                &counts,
                None,
            )
            .unwrap();
            assert_eq!(total, expected);
            let mut buf = [0; 4096];
            match watcher.read_events(&mut buf) {
                Ok(events) => events.filter_map(|e| e.name).collect::<HashSet<_>>().len(),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => 0,
                Err(e) => panic!("Could not observe fixture reads: {e}"),
            }
        };
        assert_eq!(scan(b"a\0b\0c\0", 6), 3);
        for _ in 0..3 {
            assert_eq!(
                scan(b"a\0b\0c\0d\0", 7),
                1,
                "one overflow file, not a full reread"
            );
        }
        // Drop one member. The now-fitting working set should become entirely warm.
        scan(b"b\0c\0d\0", 6);
        assert_eq!(scan(b"b\0c\0d\0", 6), 0);
    }

    #[tokio::test]
    async fn non_repository_is_absent_but_real_errors_remain_visible() {
        let dir = tempfile::tempdir().unwrap();
        assert!(info(dir.path().to_owned(), Arc::default()).await.is_none());

        command(
            dir.path(),
            &["init", "--template=", "--initial-branch=main"],
        );
        command(dir.path(), &["commit", "--allow-empty", "-m", "initial"]);
        let status = info(dir.path().to_owned(), Arc::default()).await.unwrap();
        assert!(matches!(
            &status,
            Info::Changes { branch, added: 0, removed: 0 } if branch == "main"
        ));
        assert_eq!(status.to_string(), "main");

        assert!(matches!(
            info(dir.path().join("missing"), Arc::default()).await,
            Some(Info::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn unborn_branch_counts_staged_and_untracked_lines() {
        let dir = tempfile::tempdir().unwrap();
        command(dir.path(), &["init", "--template=", "--initial-branch=new"]);
        std::fs::write(dir.path().join("staged"), "a\nb\n").unwrap();
        command(dir.path(), &["add", "staged"]);
        std::fs::write(dir.path().join("untracked"), "c").unwrap();
        match info(dir.path().to_owned(), Arc::default()).await {
            Some(Info::Changes {
                branch,
                added,
                removed,
            }) => {
                assert_eq!(branch, "new");
                assert_eq!((added, removed), (3, 0));
            }
            other => panic!("Expected Git changes, got {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "offline Git polling capacity benchmark; run alone"]
    async fn untracked_poll_benchmark() {
        fn read_chars() -> u64 {
            std::fs::read_to_string("/proc/self/io")
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("rchar: "))
                .unwrap()
                .parse()
                .unwrap()
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        command(root, &["init", "--template=", "--initial-branch=main"]);
        command(root, &["commit", "--allow-empty", "-m", "initial"]);
        let counts = Arc::new(LineCounts::default());
        let capacity = counts.0.lock().unwrap().cap().get();
        let payload = "abcdefg\n".repeat(1024);
        for n in 0..=capacity {
            std::fs::write(root.join(format!("sample-{n:08}.txt")), &payload).unwrap();
            let files = n + 1;
            if files < capacity {
                continue;
            }
            for poll in 0..4 {
                let start = Instant::now();
                let before = read_chars();
                let status = info(root.to_owned(), counts.clone()).await;
                let elapsed = start.elapsed();
                let bytes = read_chars() - before;
                assert!(
                    matches!(status, Some(Info::Changes { added, .. }) if added == files as u64 * 1024)
                );
                println!(
                    "GIT files={files} poll={poll} ms={:.3} rchar={bytes}",
                    elapsed.as_secs_f64() * 1000.
                );
            }
        }
    }
}
