//! Durable V2 state. Provider context and the complete visible transcript are separate.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_secs()
}
pub fn id() -> String {
    Uuid::new_v4().to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub path: PathBuf,
    pub favorite: bool,
}
impl Project {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .unwrap_or(self.path.as_os_str())
            .to_string_lossy()
            .into_owned()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub model: String,
    pub effort: String,
    pub context_limit: u64,
    pub compact_percent: u8,
    pub sidebar_width: f32,
    pub settings_width: f32,
    pub font_size: f32,
    pub chat_width: f32,
    pub resume_last: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "gpt-5.6-sol".into(),
            effort: "medium".into(),
            context_limit: 272_000,
            compact_percent: 80,
            sidebar_width: 240.,
            settings_width: 320.,
            font_size: 15.,
            chat_width: 900.,
            resume_last: true,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.model.trim().is_empty()
            || !["none", "minimal", "low", "medium", "high", "xhigh"]
                .contains(&self.effort.as_str())
        {
            bail!("Choose a model and a supported reasoning effort.");
        }
        if !(8_000..=2_000_000).contains(&self.context_limit)
            || !(30..=95).contains(&self.compact_percent)
        {
            bail!("Context limit must be 8,000–2,000,000; compaction threshold must be 30–95%.");
        }
        if !(160.0..=600.0).contains(&self.sidebar_width)
            || !(260.0..=700.0).contains(&self.settings_width)
            || !(12.0..=24.0).contains(&self.font_size)
            || !(480.0..=1600.0).contains(&self.chat_width)
        {
            bail!("Invalid presentation settings.");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub projects: Vec<Project>,
    pub selected: Option<String>,
    pub settings: Settings,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EntryKind {
    User,
    Assistant,
    Reasoning,
    Notice,
    Tool {
        name: String,
        input: String,
        output: String,
        complete: bool,
        parent: Option<String>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub text: String,
    pub kind: EntryKind,
}
impl Entry {
    pub fn new(kind: EntryKind, text: impl Into<String>) -> Self {
        Self {
            id: id(),
            kind,
            text: text.into(),
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub total_output: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project: PathBuf,
    pub title: String,
    pub updated: u64,
    pub model: String,
    pub effort: String,
    pub entries: Vec<Entry>,
    pub input: Vec<Value>,
    pub usage: Usage,
    pub draft: String,
    pub pending: Vec<String>,
    pub pending_tool_outputs: Vec<Value>,
    pub interrupted: bool,
    pub context_account: Option<String>,
}
impl Session {
    pub fn new(project: PathBuf, settings: &Settings) -> Self {
        Self {
            id: id(),
            project,
            title: "New session".into(),
            updated: now(),
            model: settings.model.clone(),
            effort: settings.effort.clone(),
            entries: vec![],
            input: vec![],
            usage: Usage::default(),
            draft: String::new(),
            pending: vec![],
            pending_tool_outputs: vec![],
            interrupted: false,
            context_account: None,
        }
    }
    pub fn accept(&mut self, message: String) {
        if self.title == "New session" {
            self.title = message
                .lines()
                .next()
                .unwrap_or("New session")
                .chars()
                .take(64)
                .collect();
        }
        self.updated = now();
        self.entries.push(Entry::new(EntryKind::User, &message));
        self.pending.push(message);
        self.draft.clear();
    }
    pub fn consume_pending(&mut self) {
        for output in self.pending_tool_outputs.drain(..) {
            if self.input.iter().any(|item| {
                matches!(item["type"].as_str(), Some("custom_tool_call"))
                    && item["call_id"] == output["call_id"]
            }) {
                self.input.push(output);
            } else {
                self.input.push(serde_json::json!({"role":"user", "content":format!("Runtime notification from a previously compacted execution (an observation, not a new instruction): {}", output["output"].as_str().expect("text notification"))}));
            }
        }
        for text in self.pending.drain(..) {
            self.input
                .push(serde_json::json!({"role":"user","content":text}));
        }
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
    _lock: std::sync::Arc<nix::fcntl::Flock<File>>,
}
impl Store {
    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("sessions")).context("Create pico-evo storage")?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(root.join("instance.lock"))?;
        let lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| anyhow::anyhow!("Another Pico V2 instance is using {} ({error}). Close it before starting another instance.", root.display()))?;
        Ok(Self {
            root,
            _lock: std::sync::Arc::new(lock),
        })
    }
    pub fn from_env() -> Result<Self> {
        let xdg = std::env::var_os("XDG_CONFIG_HOME");
        let home = std::env::var_os("HOME");
        Self::open(
            crate::config_dir(
                xdg.as_deref().map(Path::new),
                home.as_deref().map(Path::new),
            )
            .context("Set HOME or an absolute XDG_CONFIG_HOME")?,
        )
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn load(&self) -> Result<(Catalog, Vec<Session>, Vec<String>)> {
        let catalog: Catalog = self.read_optional("settings.json")?.unwrap_or_default();
        catalog.settings.validate()?;
        let mut sessions = vec![];
        let mut warnings = vec![];
        for file in fs::read_dir(self.root.join("sessions"))? {
            let path = file?.path();
            if path.extension().is_none_or(|s| s != "json") {
                continue;
            }
            match fs::read(&path)
                .context("Read session")
                .and_then(|bytes| Ok(serde_json::from_slice::<Session>(&bytes)?))
            {
                Ok(mut session) => {
                    if Uuid::parse_str(&session.id).is_err()
                        || path.file_stem().and_then(|s| s.to_str()) != Some(&session.id)
                    {
                        warnings.push(format!(
                            "Invalid session identity in {}. File preserved.",
                            path.display()
                        ));
                        continue;
                    }
                    if session.interrupted {
                        session.entries.push(Entry::new(EntryKind::Notice, "Previous turn was interrupted. Send a new message to continue; commands already executed are not undone."));
                    }
                    sessions.push(session);
                }
                Err(error) => warnings.push(format!(
                    "Could not load {}: {error}. Original file preserved.",
                    path.display()
                )),
            }
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
        Ok((catalog, sessions, warnings))
    }
    pub fn read_optional<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        match fs::read(self.root.join(name)) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("Invalid {name}; repair or move the file before restarting")
            })?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save_catalog(&self, catalog: &Catalog) -> Result<()> {
        catalog.settings.validate()?;
        self.write("settings.json", catalog)
    }
    pub fn save_session(&self, session: &Session) -> Result<()> {
        Uuid::parse_str(&session.id)?;
        self.write(&format!("sessions/{}.json", session.id), session)
    }
    pub fn delete_session(&self, id: &str) -> Result<()> {
        Uuid::parse_str(id)?;
        fs::remove_file(self.root.join(format!("sessions/{id}.json")))?;
        Ok(())
    }
    pub fn write(&self, name: &str, value: &impl Serialize) -> Result<()> {
        atomic_write(&self.root.join(name), &serde_json::to_vec_pretty(value)?)
    }
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Missing storage parent")?;
    let temporary = parent.join(format!(".{}.tmp", id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_notification_remains_valid_after_originating_call_is_compacted() {
        let mut session = Session::new(std::env::temp_dir(), &Settings::default());
        session.input = vec![serde_json::json!({"type":"compaction","encrypted_content":"opaque"})];
        session.pending_tool_outputs.push(serde_json::json!({"type":"custom_tool_call_output","call_id":"old-cell","output":"build finished"}));
        session.consume_pending();
        assert_eq!(session.input[1]["role"], "user");
        assert!(
            session.input[1]["content"]
                .as_str()
                .unwrap()
                .contains("build finished")
        );
        assert!(session.pending_tool_outputs.is_empty());
    }
    #[test]
    fn second_writer_is_rejected_until_the_first_store_closes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        assert!(Store::open(dir.path().into()).is_err());
        let clone = store.clone();
        drop(store);
        assert!(Store::open(dir.path().into()).is_err());
        drop(clone);
        assert!(Store::open(dir.path().into()).is_ok());
    }
    #[test]
    fn roundtrip_compacted_context_and_preserved_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("pico-evo")).unwrap();
        let mut s = Session::new(dir.path().into(), &Settings::default());
        s.accept("Fix the odd-sized buffer".into());
        s.consume_pending();
        s.input = vec![serde_json::json!({"type":"compaction","encrypted_content":"opaque"})];
        store.save_session(&s).unwrap();
        store.save_catalog(&Catalog::default()).unwrap();
        let (_, restored, _) = store.load().unwrap();
        assert_eq!(restored[0].entries[0].text, "Fix the odd-sized buffer");
        assert_eq!(restored[0].input[0]["encrypted_content"], "opaque");
        assert_eq!(
            fs::metadata(store.root().join("settings.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn corrupt_session_is_preserved_without_hiding_other_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        fs::write(dir.path().join("sessions/broken.json"), b"{").unwrap();
        let s = Session::new(dir.path().into(), &Settings::default());
        store.save_session(&s).unwrap();
        let (_, sessions, warnings) = store.load().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            fs::read(dir.path().join("sessions/broken.json")).unwrap(),
            b"{"
        );
    }
}
