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
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
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

pub struct LineCounts(Mutex<lru::LruCache<FileVersion, u64>>);

impl Default for LineCounts {
    fn default() -> Self {
        Self(Mutex::new(lru::LruCache::new(
            NonZeroUsize::new(8192).unwrap(),
        )))
    }
}

pub async fn info(path: PathBuf, counts: Arc<LineCounts>) -> Option<Info> {
    let deadline = Instant::now() + Duration::from_secs(15);
    match tokio::time::timeout_at(deadline.into(), read_info(&path, deadline, counts)).await {
        Ok(Ok(info)) => info,
        Ok(Err(error)) => Some(Info::Unavailable(error)),
        Err(_) => Some(Info::Unavailable(
            "Git status timed out; will retry automatically".into(),
        )),
    }
}

async fn git(path: &Path, args: &[&str]) -> Result<Output, String> {
    tokio::process::Command::new("git")
        .arg("--no-optional-locks")
        .env("LC_ALL", "C")
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| {
            format!(
                "Could not run Git: {e}. Check that Git is installed; will retry automatically."
            )
        })
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
) -> Result<Option<Info>, String> {
    let root = git(path, &["rev-parse", "--show-toplevel"]).await?;
    // Not being a repository is normal for a workspace. Keep other failures
    // (permissions, unsafe ownership, missing Git, etc.) visible and retryable.
    if !root.status.success()
        && root
            .stderr
            .starts_with(b"fatal: not a git repository (or any")
    {
        return Ok(None);
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
    let base = String::from_utf8_lossy(&base);
    let diff = successful(
        git(
            root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--numstat",
                "-z",
                base.trim_end(),
                "--",
            ],
        )
        .await?,
    )?;
    let (mut added, removed) = numstat(&diff)?;
    let untracked =
        successful(git(root, &["ls-files", "--others", "--exclude-standard", "-z"]).await?)?;
    let root = root.to_owned();
    added +=
        tokio::task::spawn_blocking(move || untracked_lines(&root, &untracked, deadline, &counts))
            .await
            .map_err(|e| {
                format!("Could not count untracked lines: {e}; will retry automatically")
            })??;
    Ok(Some(Info::Changes {
        branch,
        added,
        removed,
    }))
}

fn numstat(bytes: &[u8]) -> Result<(u64, u64), String> {
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
        added += count()?;
        removed += count()?;
    }
    Ok((added, removed))
}

fn untracked_lines(
    root: &Path,
    paths: &[u8],
    deadline: Instant,
    counts: &LineCounts,
) -> Result<u64, String> {
    let mut total = 0;
    for name in paths.split(|b| *b == 0).filter(|name| !name.is_empty()) {
        if Instant::now() >= deadline {
            return Err("Git status timed out; will retry automatically".into());
        }
        let path = root.join(OsStr::from_bytes(name));
        let count = || -> std::io::Result<u64> {
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Ok(1); // Git stores the link target as a single line.
            }
            if !metadata.is_file() {
                return Ok(0);
            }
            let version = FileVersion::from(&metadata);
            if let Some(lines) = counts.0.lock().expect("line counts").get(&version).copied() {
                return Ok(lines);
            }
            let file = std::fs::File::open(&path)?;
            let lines = count_lines(&file, deadline)?;
            // Never memoize a partial scan or a file replaced/edited while we
            // read it. Check the open descriptor, not a new lookup by pathname.
            if FileVersion::from(&file.metadata()?) == version {
                counts.0.lock().expect("line counts").put(version, lines);
            }
            Ok(lines)
        };
        match count() {
            Ok(lines) => total += lines,
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
}
