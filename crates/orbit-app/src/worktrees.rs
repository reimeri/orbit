//! Local Git worktrees. Run on a worker, never on the window thread.
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    cmp::Reverse,
    ffi::{OsStr, OsString},
    fs,
    io::{BufRead, ErrorKind},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::Duration,
};

const MARKER: &str = "orbit-created";

async fn git<S: AsRef<OsStr>, const N: usize>(dir: &Path, args: [S; N]) -> Result<Vec<u8>> {
    let out = tokio::time::timeout(
        Duration::from_secs(60),
        crate::git::local_command(dir).args(args).output(),
    )
    .await
    .context("Git timed out. Inspect the refreshed worktree list before retrying.")?
    .with_context(|| format!("Cannot run Git in {}", dir.display()))?;
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(out.stdout)
}

async fn git_path(dir: &Path, flag: &str) -> Result<PathBuf> {
    let bytes = git(dir, ["rev-parse", "--path-format=absolute", flag]).await?;
    Ok(PathBuf::from(OsStr::from_bytes(
        bytes.strip_suffix(b"\n").unwrap_or(&bytes),
    )))
}

#[derive(Clone, Debug)]
pub struct Checkout {
    pub path: PathBuf,
    pub branch: Option<OsString>,
    pub label: String,
    pub primary: bool,
    pub managed: bool,
    pub locked: bool,
    pub available: bool,
    created: Option<u64>,
}

impl Checkout {
    pub fn removable_from(&self, current: &Path) -> bool {
        self.managed
            && !self.primary
            && self.branch.is_some()
            && self.available
            && !self.locked
            && self.path != current
    }
}

#[derive(Clone, Debug)]
pub struct Repo {
    pub current: PathBuf,
    common: PathBuf,
    managed: PathBuf,
    pub trees: Vec<Checkout>,
    pub branches: Vec<OsString>,
    pub base: Option<OsString>,
}

fn reflog_birth(common: &Path, branch: &OsStr) -> Result<Option<u64>> {
    let file = match fs::File::open(common.join("logs/refs/heads").join(branch)) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut first = Vec::new();
    std::io::BufReader::new(file).read_until(b'\n', &mut first)?;
    // The old object ID is all zeros on ref creation. An expired/pruned
    // first entry is not evidence of the branch's creation date.
    let header = first
        .split(|&b| b == b'\t')
        .next()
        .expect("split always yields a field");
    if header
        .split(|&b| b == b' ')
        .next()
        .is_some_and(|oid| !oid.is_empty() && oid.iter().all(|&b| b == b'0'))
    {
        return Ok(header
            .rsplit(|&b| b == b' ')
            .nth(1)
            .and_then(|s| std::str::from_utf8(s).ok()?.parse().ok()));
    }
    Ok(None)
}

impl Repo {
    pub async fn read(project: &Path, state: &Path) -> Result<Self> {
        let current = git_path(project, "--show-toplevel").await?;
        let common = tokio::fs::canonicalize(git_path(project, "--git-common-dir").await?).await?;
        let key = format!("{:x}", Sha256::digest(common.as_os_str().as_bytes()));
        let managed = tokio::fs::canonicalize(state)
            .await?
            .join("worktrees")
            .join(key);
        let refs = git(
            project,
            ["for-each-ref", "--format=%(refname)", "refs/heads/"],
        )
        .await?;
        let branches: Vec<_> = refs
            .split(|&b| b == b'\n')
            .filter_map(|line| line.strip_prefix(b"refs/heads/"))
            .map(|v| OsStr::from_bytes(v).to_owned())
            .collect();
        let origin = git(
            project,
            [
                "for-each-ref",
                "--format=%(symref)",
                "refs/remotes/origin/HEAD",
            ],
        )
        .await?;
        let preferred = match origin.as_slice() {
            b"refs/remotes/origin/master\n" => ["master", "main"],
            _ => ["main", "master"],
        };
        let base = preferred
            .iter()
            .find_map(|b| branches.iter().find(|name| *name == OsStr::new(b)).cloned());
        let listing = git(project, ["worktree", "list", "--porcelain", "-z"]).await?;
        let mut fields = listing.split(|&b| b == 0);
        let mut trees = Vec::new();
        let mut primary = true;
        while let Some(record) = fields.next().filter(|s| !s.is_empty()) {
            let path = PathBuf::from(OsStr::from_bytes(
                record
                    .strip_prefix(b"worktree ")
                    .context("Invalid worktree record")?,
            ));
            let mut branch = None;
            let mut head = String::new();
            let mut locked = false;
            let mut missing = false;
            let mut bare = false;
            for field in fields.by_ref().take_while(|s| !s.is_empty()) {
                if let Some(name) = field.strip_prefix(b"branch refs/heads/") {
                    branch = Some(OsStr::from_bytes(name).to_owned());
                } else if let Some(hash) = field.strip_prefix(b"HEAD ") {
                    head = format!(
                        "{} (detached)",
                        String::from_utf8_lossy(&hash[..7.min(hash.len())])
                    );
                } else {
                    locked |= field == b"locked" || field.starts_with(b"locked ");
                    missing |= field == b"prunable" || field.starts_with(b"prunable ");
                    bare |= field == b"bare";
                }
            }
            let is_primary = std::mem::replace(&mut primary, false);
            if bare {
                continue;
            }
            let available = !missing && tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_dir());
            let mut created = None;
            let mut owned = false;
            if available && path.parent() == Some(&managed) {
                let marker = git_path(&path, "--absolute-git-dir").await?.join(MARKER);
                match tokio::fs::read_to_string(marker).await {
                    Ok(stamp) => {
                        owned = true;
                        if branch.as_deref() == path.file_name() {
                            created = Some(
                                stamp
                                    .parse()
                                    .context("Invalid Orbit worktree creation date")?,
                            );
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            if created.is_none()
                && let Some(branch) = &branch
            {
                let common = common.clone();
                let branch = branch.clone();
                created =
                    tokio::task::spawn_blocking(move || reflog_birth(&common, &branch)).await??;
            }
            let label = branch
                .as_ref()
                .map_or(head, |b| b.to_string_lossy().into_owned());
            trees.push(Checkout {
                path,
                branch,
                label,
                primary: is_primary,
                managed: owned,
                locked,
                available,
                created,
            });
        }
        trees.sort_by(|a, b| {
            let key = |c: &Checkout| (base.is_none() || c.branch != base, Reverse(c.created));
            key(a)
                .cmp(&key(b))
                .then_with(|| a.label.cmp(&b.label))
                .then_with(|| a.path.cmp(&b.path))
        });
        Ok(Self {
            current,
            common,
            managed,
            trees,
            branches,
            base,
        })
    }

    /// Check against a live Git root, not just an existing directory: a
    /// checkout could have been removed/replaced since the list opened.
    pub async fn open(&self, path: &Path) -> Result<PathBuf> {
        ensure!(
            git_path(path, "--show-toplevel").await? == path
                && tokio::fs::canonicalize(git_path(path, "--git-common-dir").await?).await?
                    == self.common,
            "{} is no longer this repository’s checkout. Refresh the list.",
            path.display()
        );
        Ok(path.to_owned())
    }

    pub async fn create(&self, base: &OsStr) -> Result<PathBuf> {
        let candidates = (0..16).map(|n| {
            let id = orbit_core::store::id();
            let index =
                |offset| usize::from_str_radix(&id[offset..offset + 2], 16).expect("UUID hex");
            let adjectives = [
                "amber", "brisk", "coral", "dawn", "frost", "quiet", "silver", "swift", "umber",
                "wild", "misty", "lunar",
            ];
            let nouns = [
                "cedar", "comet", "finch", "fox", "heron", "lark", "otter", "pine", "raven",
                "river", "spruce", "wren",
            ];
            let mut name = format!(
                "{}-{}",
                adjectives[index(0) % adjectives.len()],
                nouns[index(2) % nouns.len()]
            );
            if n > 0 {
                name.push('-');
                name.push_str(&id[4..8]);
            }
            name
        });
        self.create_from_candidates(base, candidates).await
    }

    async fn create_from_candidates(
        &self,
        base: &OsStr,
        candidates: impl IntoIterator<Item = String>,
    ) -> Result<PathBuf> {
        self.open(&self.current).await?;
        let mut reference = OsString::from("refs/heads/");
        reference.push(base);
        tokio::fs::create_dir_all(&self.managed).await?;
        for name in candidates {
            let path = self.managed.join(&name);
            match tokio::fs::create_dir(&path).await {
                Ok(()) => (),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
            if let Err(error) = git(
                &self.current,
                [
                    "worktree".as_ref(),
                    "add".as_ref(),
                    "-b".as_ref(),
                    name.as_ref(),
                    "--".as_ref(),
                    path.as_os_str(),
                    &reference,
                ],
            )
            .await
            {
                // Only release our empty reservation. Never recursively undo Git.
                if tokio::fs::try_exists(&path).await? {
                    tokio::fs::remove_dir(&path).await.with_context(|| {
                        format!(
                            "{error}. Inspect the preserved directory {}",
                            path.display()
                        )
                    })?;
                }
                if error
                    .to_string()
                    .contains(&format!("branch named '{name}' already exists"))
                {
                    continue;
                }
                return Err(error);
            }
            let mark = async {
                tokio::fs::write(
                    git_path(&path, "--absolute-git-dir").await?.join(MARKER),
                    orbit_core::store::now().to_string(),
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            mark.with_context(|| format!("Worktree created at {} but not marked as managed. Refresh and open it from the list.", path.display()))?;
            return Ok(path);
        }
        bail!("Could not reserve a unique worktree name. Try again.")
    }

    pub async fn remove(&self, path: &Path, state: &Path) -> Result<()> {
        self.open(path).await?;
        let fresh = Self::read(&self.current, state).await?;
        ensure!(
            fresh
                .trees
                .iter()
                .any(|t| t.path == path && t.removable_from(&fresh.current)),
            "This worktree cannot be removed."
        );
        // Git's removal check ignores ignored files, and user status
        // configuration can hide untracked files. Neither is disposable.
        ensure!(
            git(
                path,
                [
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--ignored=matching",
                    "--untracked-files=all",
                    "--ignore-submodules=none"
                ]
            )
            .await?
            .is_empty(),
            "Worktree contains local changes, untracked or ignored files. Preserve them before removing {}.",
            path.display()
        );
        git(
            &self.current,
            [
                "worktree".as_ref(),
                "remove".as_ref(),
                "--".as_ref(),
                path.as_os_str(),
            ],
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command<S: AsRef<OsStr>>(dir: &Path, args: &[S]) {
        use std::process::Command;
        let out = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@localhost",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[tokio::test]
    async fn select_is_read_only_create_uses_the_chosen_ref_and_remove_keeps_the_branch() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas");
        let state = dir.path().join("orbit");
        let external = dir.path().join("tab\tand\nspace checkout");
        fs::create_dir(&main).unwrap();
        fs::create_dir(&state).unwrap();
        command(&main, &["init", "--template=", "-b", "main"]);
        command(&main, &["commit", "--allow-empty", "-m", "first"]);
        command(
            &main,
            &[
                "worktree".as_ref(),
                "add".as_ref(),
                "-b".as_ref(),
                "topic/trails".as_ref(),
                external.as_os_str(),
            ],
        );
        command(&external, &["commit", "--allow-empty", "-m", "second"]);
        fs::write(external.join("unsaved"), b"keep this").unwrap();

        let subdir = external.join("nested");
        fs::create_dir(&subdir).unwrap();
        let repo = Repo::read(&subdir, &state).await.unwrap();
        assert_eq!(repo.current, external);
        assert_eq!(repo.base.as_deref(), Some(OsStr::new("main")));
        assert_eq!(repo.trees[0].path, main);
        assert_eq!(repo.open(&external).await.unwrap(), external);
        assert!(
            !state.join("worktrees").exists(),
            "discovery/selection must not allocate a worktree"
        );
        assert!(repo.remove(&main, &state).await.is_err());
        // A generated name may collide with an unmounted branch or with a
        // directory that Git would otherwise adopt. Neither may be reused.
        command(&main, &["branch", "reserved-name"]);
        fs::create_dir_all(repo.managed.join("occupied")).unwrap();
        let from_default = repo
            .create_from_candidates(
                repo.base.as_deref().unwrap(),
                ["reserved-name", "occupied", "fresh"].map(str::to_owned),
            )
            .await
            .unwrap();
        assert_eq!(from_default.file_name(), Some(OsStr::new("fresh")));
        assert!(
            repo.managed
                .join("occupied")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
        let from_topic = repo.create("topic/trails".as_ref()).await.unwrap();
        let head = async |dir: &Path| git(dir, ["rev-parse", "HEAD"]).await.unwrap();
        assert_ne!(head(&main).await, head(&external).await);
        assert_eq!(head(&from_default).await, head(&main).await);
        assert_eq!(head(&from_topic).await, head(&external).await);
        assert!(!from_topic.join("unsaved").exists());
        let repo = Repo::read(&from_default, &state).await.unwrap();
        assert_eq!(repo.trees.len(), 4);
        assert_eq!(repo.branches.len(), 5);
        assert_eq!(repo.trees.iter().filter(|t| t.managed).count(), 2);
        assert!(repo.remove(&external, &state).await.is_err());
        assert!(repo.remove(&from_default, &state).await.is_err());

        command(&main, &["config", "status.showUntrackedFiles", "no"]);
        fs::create_dir_all(main.join(".git/info")).unwrap();
        fs::write(main.join(".git/info/exclude"), "*.secret\n").unwrap();
        fs::write(from_topic.join("local.secret"), "ignored but valuable").unwrap();
        assert!(repo.remove(&from_topic, &state).await.is_err());
        fs::remove_file(from_topic.join("local.secret")).unwrap();
        fs::write(from_topic.join("dirty"), "important").unwrap();
        assert!(repo.remove(&from_topic, &state).await.is_err());
        assert_eq!(fs::read(from_topic.join("dirty")).unwrap(), b"important");
        fs::remove_file(from_topic.join("dirty")).unwrap();
        command(
            &main,
            &["worktree".as_ref(), "lock".as_ref(), from_topic.as_os_str()],
        );
        assert!(repo.remove(&from_topic, &state).await.is_err());
        command(
            &main,
            &[
                "worktree".as_ref(),
                "unlock".as_ref(),
                from_topic.as_os_str(),
            ],
        );
        command(&from_topic, &["checkout", "--detach"]);
        assert!(repo.remove(&from_topic, &state).await.is_err());
        command(
            &from_topic,
            &[
                "checkout",
                from_topic.file_name().unwrap().to_str().unwrap(),
            ],
        );
        repo.remove(&from_topic, &state).await.unwrap();
        assert!(!from_topic.exists());
        assert!(
            Repo::read(&main, &state)
                .await
                .unwrap()
                .branches
                .iter()
                .any(|b| Some(b.as_os_str()) == from_topic.file_name())
        );
    }

    #[tokio::test]
    async fn default_base_is_a_committed_local_branch_and_respects_origin_head() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        command(path, &["init", "--template=", "-b", "trunk"]);
        let read = async || Repo::read(path, path).await.unwrap();
        assert!(read().await.branches.is_empty());
        command(path, &["commit", "--allow-empty", "-m", "initial"]);
        assert!(read().await.base.is_none());
        for name in ["master", "main"] {
            command(path, &["branch", name]);
            assert_eq!(read().await.base.as_deref(), Some(OsStr::new(name)));
        }
        command(path, &["update-ref", "refs/remotes/origin/master", "HEAD"]);
        command(
            path,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/master",
            ],
        );
        let repo = read().await;
        assert_eq!(repo.base.as_deref(), Some(OsStr::new("master")));
        assert_eq!(
            repo.trees.len(),
            1,
            "branches without a checkout are base choices only"
        );
    }
}
