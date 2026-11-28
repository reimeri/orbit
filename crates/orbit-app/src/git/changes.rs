//! The optional per-file projection of the same HEAD-to-working-tree scan as the footer.
use super::{FileVersion, local_command};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    io::{Read, Seek},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub status: Status,
    pub added: u64,
    pub removed: u64,
    pub gitlink: bool,
    stamp: Option<FileVersion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Added,
    Untracked,
    Unreadable,
    Modified,
    Deleted,
    TypeChanged,
    Unmerged,
    Recreated,
}
impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Untracked => "Added (untracked)",
            Self::Unreadable => "Unreadable · line count unknown",
            Self::Modified => "Modified",
            Self::Deleted => "Removed",
            Self::TypeChanged => "Type changed",
            Self::Unmerged => "Unmerged",
            Self::Recreated => "Recreated",
        }
    }
}
#[derive(Debug)]
pub struct Files {
    pub root: PathBuf,
    pub base: String,
    pub entries: BTreeMap<PathBuf, File>,
}
impl Files {
    pub(super) fn new(root: PathBuf, base: String) -> Self {
        Self {
            root,
            base,
            entries: BTreeMap::new(),
        }
    }
    // With --no-renames, each NUL-delimited raw header has exactly one path.
    // The numstat records follow the raw records in the same Git invocation.
    pub(super) fn read_status<'a>(&mut self, mut bytes: &'a [u8]) -> Result<&'a [u8], String> {
        while bytes.starts_with(b":") {
            let (header, rest) = cut(bytes)?;
            let (path, rest) = cut(rest)?;
            bytes = rest;
            let status = match header.last() {
                Some(b'A') => Status::Added,
                Some(b'D') => Status::Deleted,
                Some(b'M') => Status::Modified,
                Some(b'T') => Status::TypeChanged,
                Some(b'U') => Status::Unmerged,
                _ => return Err("Unknown Git file status".into()),
            };
            let path = Path::new(OsStr::from_bytes(path));
            let stamp = match fs::symlink_metadata(self.root.join(path)) {
                Ok(m) => Some(FileVersion::from(&m)),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    None
                }
                Err(e) => return Err(format!("Could not inspect {}: {e}", path.display())),
            };
            self.entries.insert(
                path.to_owned(),
                File {
                    status,
                    added: 0,
                    removed: 0,
                    // Only a live submodule needs polling without a changed file stamp.
                    gitlink: header.get(8..14) == Some(b"160000"),
                    stamp,
                },
            );
        }
        Ok(bytes)
    }
    pub(super) fn add_untracked(&mut self, path: &Path, count: u64, stamp: Option<FileVersion>) {
        self.entries
            .entry(path.to_owned())
            .and_modify(|f| {
                f.added += count;
                f.status = if stamp.is_some() {
                    Status::Recreated
                } else {
                    Status::Unreadable
                };
                f.gitlink = false;
                f.stamp = stamp;
            })
            .or_insert(File {
                status: if stamp.is_some() {
                    Status::Untracked
                } else {
                    Status::Unreadable
                },
                added: count,
                removed: 0,
                gitlink: false,
                stamp,
            });
    }
    pub(super) fn set_counts(&mut self, path: &Path, a: u64, d: u64) -> Result<(), String> {
        let file = self
            .entries
            .get_mut(path)
            .ok_or("Git numstat path missing from raw diff")?;
        file.added = a;
        file.removed = d;
        Ok(())
    }
}

fn cut(bytes: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let n = bytes
        .iter()
        .position(|b| *b == 0)
        .ok_or("Incomplete Git status record")?;
    Ok((&bytes[..n], &bytes[n + 1..]))
}

#[derive(Debug)]
pub enum Patch {
    Rows(Vec<Line>),
    Notice(String),
    Saved { path: PathBuf, bytes: u64 },
}

#[derive(Debug)]
pub enum Line {
    Code {
        old: Option<usize>,
        new: Option<usize>,
        text: String,
        highlight: Option<std::ops::Range<usize>>,
    },
    Gap(usize),
    Note(String),
}

pub async fn patch(root: &Path, base: &str, path: &Path, file: &File) -> anyhow::Result<Patch> {
    if file.status == Status::Unreadable {
        return Ok(Patch::Notice(
            "Permission denied. Restore read access to view the complete diff.".into(),
        ));
    }
    let spool = tempfile::Builder::new()
        .prefix("orbit-diff-")
        .suffix(".patch")
        .tempfile_in("/tmp")?;
    // Recreated means Git lists both a tracked deletion and an untracked
    // addition for this path. Append both to the same review.
    let parts: &[bool] = match file.status {
        Status::Untracked => &[true],
        Status::Recreated => &[false, true],
        _ => &[false],
    };
    for &untracked in parts {
        let mut cmd = local_command(root);
        cmd.args([
            "--literal-pathspecs",
            "-c",
            "diff.suppressBlankEmpty=false",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "--submodule=short",
            "--unified=3",
            "--output-indicator-old=-",
            "--output-indicator-new=+",
            "--output-indicator-context= ",
        ]);
        if untracked {
            let root_fd = fs::File::open(root)?;
            let pinned = super::pin_untracked(&root_fd, path)?;
            let meta = pinned.metadata()?;
            let input = if meta.is_file() {
                super::read_pinned(&pinned)?
            } else if meta.is_symlink() {
                use std::io::Write;
                let target = rustix::fs::readlinkat(&pinned, "", Vec::new())?;
                let mut link = tempfile::tempfile()?;
                link.write_all(target.as_bytes())
                    .and_then(|()| link.rewind())?;
                link
            } else {
                return Ok(Patch::Notice("Not a regular file; no text diff.".into()));
            };
            cmd.args(["--no-index", "--", "/dev/null", "-"])
                .stdin(input);
        } else {
            // Literal pathspecs also match descendants. With a file/directory
            // replacement those are additions when this row is a deletion, or vice versa.
            match file.status {
                Status::Deleted | Status::Recreated => {
                    cmd.arg("--diff-filter=D");
                }
                Status::Added => {
                    cmd.arg("--diff-filter=A");
                }
                _ => {}
            }
            cmd.arg(base).arg("--").arg(path);
        }
        let child = cmd
            .stdout(spool.as_file().try_clone()?)
            .stderr(Stdio::piped())
            .spawn()?;
        let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .map_err(|_| anyhow::anyhow!("Diff timed out"))??;
        if !(out.status.success()
            || (untracked && out.status.code() == Some(1) && out.stderr.is_empty()))
        {
            anyhow::bail!(
                "Could not load diff: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    tokio::task::spawn_blocking(move || -> anyhow::Result<Patch> {
        let mut spool = spool;
        let bytes = spool.as_file().metadata()?.len();
        if bytes > 1024 * 1024 {
            let (_, path) = spool.keep()?;
            return Ok(Patch::Saved { path, bytes });
        }
        spool.rewind()?;
        let mut data = Vec::with_capacity(bytes as usize);
        spool.read_to_end(&mut data)?;
        match std::str::from_utf8(&data) {
            Ok(diff) if !diff.lines().any(|line| line.len() > 16 * 1024) => parse(diff),
            _ => {
                let (_, path) = spool.keep()?;
                Ok(Patch::Saved { path, bytes })
            }
        }
    })
    .await?
}

fn parse(diff: &str) -> anyhow::Result<Patch> {
    let mut rows = Vec::new();
    let mut at = None;
    let mut meta = Vec::new();
    for line in diff.split_terminator('\n') {
        if line.starts_with("diff --git ") {
            at = None;
            continue;
        }
        if let Some(header) = line.strip_prefix("@@ -") {
            let (old, new) = header
                .split_once(" +")
                .ok_or_else(|| anyhow::anyhow!("Invalid Git hunk"))?;
            let start = |s: &str| -> anyhow::Result<usize> {
                Ok(s.split([',', ' ']).next().expect("range").parse()?)
            };
            let old = start(old)?;
            let new = start(new)?;
            if let Some((prev, _)) = at
                && old > prev
            {
                rows.push(Line::Gap(old - prev));
            }
            at = Some((old, new));
        } else if let Some((old, new)) = &mut at {
            let (a, b) = match line.as_bytes().first() {
                Some(b'-') => (true, false),
                Some(b'+') => (false, true),
                Some(b' ') => (true, true),
                Some(b'\\') => {
                    rows.push(Line::Note(line.trim_start_matches("\\ ").into()));
                    continue;
                }
                _ => anyhow::bail!("Invalid line in Git hunk"),
            };
            rows.push(Line::Code {
                old: a.then_some(*old),
                new: b.then_some(*new),
                text: line[1..].into(),
                highlight: None,
            });
            *old += usize::from(a);
            *new += usize::from(b);
        } else if line.starts_with("old mode ") || line.starts_with("new mode ") {
            rows.push(Line::Note(line.into()));
        } else if line.starts_with("Binary files ") {
            rows.push(Line::Note("Binary file; no text diff.".into()))
        } else if line.contains(" mode ") {
            meta.push(line.to_owned())
        }
    }
    // Highlight the differing middle of an adjacent removal/replacement.
    for i in 1..rows.len() {
        let (before, after) = rows.split_at_mut(i);
        if let (
            Line::Code {
                new: None,
                text: a,
                highlight: ah,
                ..
            },
            Line::Code {
                old: None,
                text: b,
                highlight: bh,
                ..
            },
        ) = (&mut before[i - 1], &mut after[0])
        {
            let prefix = a
                .chars()
                .zip(b.chars())
                .take_while(|(x, y)| x == y)
                .map(|(x, _)| x.len_utf8())
                .sum::<usize>();
            let suffix = a[prefix..]
                .chars()
                .rev()
                .zip(b[prefix..].chars().rev())
                .take_while(|(x, y)| x == y)
                .map(|(x, _)| x.len_utf8())
                .sum::<usize>();
            if !a[..prefix].trim().is_empty() || !a[a.len() - suffix..].trim().is_empty() {
                *ah = Some(prefix..a.len() - suffix);
                *bh = Some(prefix..b.len() - suffix);
            }
        }
    }
    if rows.is_empty() {
        Ok(Patch::Notice(if meta.is_empty() {
            "No text changes.".into()
        } else {
            meta.join("\n")
        }))
    } else {
        Ok(Patch::Rows(rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_edit_highlights_the_changed_prefix_not_the_common_suffix() {
        let Patch::Rows(rows) = parse("@@ -1 +1 @@\n-旧 tail\n+new tail\n").unwrap() else {
            panic!("rows")
        };
        let edits: Vec<_> = rows
            .iter()
            .filter_map(|r| match r {
                Line::Code {
                    text,
                    highlight: Some(h),
                    ..
                } => Some(&text[h.clone()]),
                _ => None,
            })
            .collect();
        assert_eq!(edits, ["旧", "new"]);
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@invalid",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[tokio::test]
    async fn literal_paths_staged_and_unstaged_edits_share_one_numbered_diff() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        git(root, &["init", "--template=", "--initial-branch=main"]);
        let path = Path::new("src/[x].txt");
        fs::create_dir(root.join("src")).unwrap();
        let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
        fs::write(root.join(path), &original).unwrap();
        fs::write(root.join("src/x.txt"), "different\n").unwrap();
        git(root, &["add", "src"]);
        git(root, &["commit", "-qm", "base"]);
        let staged = original.replace("line 2\n", "line two\nextra\n");
        fs::write(root.join(path), &staged).unwrap();
        git(root, &["add", "src"]);
        fs::write(root.join(path), staged.replace("line 19\n", "")).unwrap();
        fs::write(root.join("src/x.txt"), "not part of literal patch\n").unwrap();
        fs::write(root.join("src/binary"), [0, 1]).unwrap();
        let (info, files) = super::super::capture(root.join("src"), Default::default(), true).await;
        let files = files.unwrap();
        assert_eq!(info.unwrap().to_string(), "main  +3 −3");
        let file = &files.entries[path];
        assert_eq!(
            (file.status.label(), file.added, file.removed),
            ("Modified", 2, 2)
        );
        let Patch::Rows(rows) = patch(&files.root, &files.base, path, file).await.unwrap() else {
            panic!("expected text patch")
        };
        assert!(rows.iter().any(|r| matches!(r, Line::Gap(10))));
        assert!(
            rows.iter()
                .any(|r| matches!(r,Line::Code{old:None,new:Some(3),text,..} if text=="extra"))
        );
        assert!(
            rows.iter()
                .any(|r| matches!(r,Line::Code{old:Some(19),new:None,text,..} if text=="line 19"))
        );
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r,Line::Code{text,..} if text.contains("literal patch")))
        );
        let path = Path::new("src/binary");
        assert!(
            matches!(patch(root,&files.base,path,&files.entries[path]).await.unwrap(),Patch::Rows(rows) if rows.iter().any(|r|matches!(r,Line::Note(n) if n.contains("Binary"))))
        );
    }

    #[tokio::test]
    async fn recreated_file_keeps_deleted_and_untracked_content() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        git(root, &["init", "--template="]);
        let p = Path::new("same");
        fs::write(root.join(p), "before\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "base"]);
        git(root, &["rm", "--cached", "same"]);
        fs::write(root.join(p), "after\nagain\n").unwrap();
        let (_, files) = super::super::capture(root.into(), Default::default(), true).await;
        let files = files.unwrap();
        let entry = &files.entries[p];
        assert_eq!(
            (entry.status, entry.added, entry.removed),
            (Status::Recreated, 2, 1)
        );
        let Patch::Rows(rows) = patch(root, &files.base, p, entry).await.unwrap() else {
            panic!("expected both patches")
        };
        assert!(
            rows.iter()
                .any(|r| matches!(r,Line::Code{new:None,text,..} if text=="before"))
        );
        assert!(
            rows.iter()
                .any(|r| matches!(r,Line::Code{old:None,text,..} if text=="again"))
        );
    }

    #[tokio::test]
    async fn untracked_diff_does_not_follow_replaced_parent_directory() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        git(root, &["init", "--template="]);
        fs::create_dir(root.join("src")).unwrap();
        let p = Path::new("src/new");
        fs::write(root.join(p), "original").unwrap();
        let (_, files) = super::super::capture(root.into(), Default::default(), true).await;
        let files = files.unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("new"), "outside").unwrap();
        fs::rename(root.join("src"), root.join("moved")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root.join("src")).unwrap();
        assert!(
            patch(root, &files.base, p, &files.entries[p])
                .await
                .is_err()
        );
    }
}
