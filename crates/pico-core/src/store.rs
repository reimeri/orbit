//! Durable V2 state. Provider context and the complete visible transcript are separate.
use crate::{
    history::{History, Record},
    models::{DEFAULT_MODEL, MODELS, model},
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelDefaults {
    pub effort: String,
    pub fast: bool,
}
impl Default for ModelDefaults {
    fn default() -> Self {
        Self {
            effort: "medium".into(),
            fast: false,
        }
    }
}
fn model_defaults() -> BTreeMap<String, ModelDefaults> {
    MODELS
        .iter()
        .map(|m| (m.id.into(), ModelDefaults::default()))
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub web_search: crate::web_search::Mode,
    #[serde(default)]
    pub images: crate::image_tools::Settings,
    pub model: String,
    #[serde(default = "model_defaults")]
    pub models: BTreeMap<String, ModelDefaults>,
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
            images: crate::image_tools::Settings::default(),
            web_search: crate::web_search::Mode::Live,
            model: DEFAULT_MODEL.into(),
            models: model_defaults(),
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
    /// Catalog updates retire old IDs without retaining compatibility aliases.
    fn refresh_models(&mut self) {
        self.models.retain(|id, _| model(id).is_some());
        for m in MODELS {
            self.models.entry(m.id.into()).or_default();
        }
        if model(&self.model).is_none() {
            self.model = DEFAULT_MODEL.into();
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.images.validate()?;
        if model(&self.model).is_none() {
            bail!("Choose a default model from the Models tab.");
        }
        if self.models.len() != MODELS.len() {
            bail!("Model preferences must match the app catalog.");
        }
        for m in MODELS {
            let defaults = self
                .models
                .get(m.id)
                .with_context(|| format!("Missing defaults for {}", m.name))?;
            if !m.efforts.contains(&defaults.effort.as_str()) {
                bail!("Choose a supported reasoning effort for {}.", m.name);
            }
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
/// A composer draft or accepted user submission. Media bytes stay out of SQLite.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    pub text: String,
    pub images: Vec<crate::media::Media>,
}
impl UserMessage {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty()
    }
    pub fn content(&self) -> Value {
        if self.images.is_empty() {
            return serde_json::json!(self.text);
        }
        let mut parts = Vec::new();
        if !self.text.is_empty() {
            parts.push(crate::media::Part::InputText {
                text: self.text.clone(),
            });
        }
        for media in &self.images {
            parts.push(crate::media::Part::InputText {
                text: format!(
                    "Attached image: {}{}",
                    media.description(),
                    if media.mime == "image/gif" {
                        " (model sees the first frame; animation is preserved in the saved file)"
                    } else {
                        ""
                    }
                ),
            });
            parts.push(crate::media::Part::PicoMedia {
                media: media.clone(),
            });
        }
        serde_json::json!(parts)
    }
}
impl From<String> for UserMessage {
    fn from(text: String) -> Self {
        Self {
            text,
            images: vec![],
        }
    }
}
impl From<&str> for UserMessage {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}
/// Unsent first message, shared across projects and separate from session history.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NewSessionDraft {
    pub project: Option<PathBuf>,
    pub message: String,
    pub images: Vec<crate::media::Media>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EntryKind {
    User {
        images: Vec<crate::media::Media>,
    },
    Assistant,
    Reasoning,
    Notice,
    Tool {
        name: String,
        input: String,
        output: String,
        complete: bool,
        #[serde(default)]
        failed: bool,
        #[serde(default)]
        cell_id: Option<String>,
        parent: Option<String>,
        #[serde(default)]
        media: Vec<crate::media::Media>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        web: Option<crate::web_search::Metadata>,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub text: String,
    pub kind: EntryKind,
}
impl Record for Entry {
    fn id(&self) -> Option<&str> {
        Some(&self.id)
    }
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
    #[serde(default)]
    pub total_input: u64,
    #[serde(default)]
    pub total_cached: u64,
    pub total_output: u64,
}
impl Usage {
    pub(crate) fn record_response(&mut self, response: Self) {
        let total_input = self.total_input + response.input;
        let total_output = self.total_output + response.output;
        let total_cached = self.total_cached + response.cached;
        *self = response;
        self.total_input = total_input;
        self.total_output = total_output;
        self.total_cached = total_cached;
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub child: Option<crate::agent::subagents::Child>,
    #[serde(default)]
    pub subagent_notices: Vec<String>,
    pub project: PathBuf,
    pub title: String,
    pub updated: u64,
    pub model: String,
    pub effort: String,
    #[serde(default)]
    pub fast: bool,
    #[serde(skip)]
    pub entries: History<Entry>,
    #[serde(skip)]
    pub input: History<Value>,
    pub summary: String,
    pub summarized_entries: usize,
    pub usage: Usage,
    pub draft: String,
    pub draft_images: Vec<crate::media::Media>,
    pub pending: Vec<UserMessage>,
    pub pending_submissions: Vec<String>,
    pub pending_tool_outputs: Vec<Value>,
    pub interrupted: bool,
    pub needs_attention: bool,
    pub context_account: Option<String>,
}
impl Session {
    pub fn new(project: PathBuf, settings: &Settings) -> Self {
        Self {
            id: id(),
            child: None,
            subagent_notices: vec![],
            project,
            title: "New session".into(),
            updated: now(),
            model: settings.model.clone(),
            effort: settings.models[&settings.model].effort.clone(),
            fast: settings.models[&settings.model].fast,
            entries: History::default(),
            input: History::default(),
            summary: String::new(),
            summarized_entries: 0,
            usage: Usage::default(),
            draft: String::new(),
            draft_images: vec![],
            pending: vec![],
            pending_submissions: vec![],
            pending_tool_outputs: vec![],
            interrupted: false,
            needs_attention: false,
            context_account: None,
        }
    }
    pub fn select_model(&mut self, id: &str, settings: &Settings) {
        let defaults = &settings.models[id];
        self.model = id.into();
        self.effort = defaults.effort.clone();
        self.fast = defaults.fast;
    }
    /// Returns the retired model ID, for a user-facing informational notice.
    pub fn reconcile_model(&mut self, settings: &Settings) -> Option<String> {
        if model(&self.model).is_some() {
            return None;
        }
        let retired = self.model.clone();
        self.select_model(&settings.model, settings);
        Some(retired)
    }
    pub fn accept(&mut self, message: UserMessage) {
        self.accept_submission(id(), message);
    }
    pub fn accept_submission(&mut self, submission: String, message: UserMessage) {
        if self.title == "New session" {
            self.title = message
                .text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("Image message")
                .chars()
                .take(64)
                .collect();
        }
        self.updated = now();
        self.pending_submissions.push(submission.clone());
        self.entries.push(Entry {
            id: submission,
            kind: EntryKind::User {
                images: message.images.clone(),
            },
            text: message.text.clone(),
        });
        self.pending.push(message);
        self.draft.clear();
        self.draft_images.clear();
    }
    pub fn consume_pending(&mut self) {
        for notice in self.subagent_notices.drain(..) {
            self.input
                .push(serde_json::json!({"role":"user", "content":notice}));
        }
        self.pending_submissions.clear();
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
        for message in self.pending.drain(..) {
            self.input.push(
                serde_json::json!({"type":"message","role":"user","content":message.content()}),
            );
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    #[serde(default)]
    pub child: Option<crate::agent::subagents::Child>,
    pub needs_attention: bool,
    pub id: String,
    pub project: PathBuf,
    pub title: String,
    pub updated: u64,
    pub interrupted: bool,
}
impl From<&Session> for SessionSummary {
    fn from(s: &Session) -> Self {
        Self {
            child: s.child.clone(),
            needs_attention: s.needs_attention,
            id: s.id.clone(),
            project: s.project.clone(),
            title: s.title.clone(),
            updated: s.updated,
            interrupted: s.interrupted,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    root: PathBuf,
    pub(crate) db: Arc<Mutex<Connection>>,
    _lock: Arc<nix::fcntl::Flock<File>>,
}
impl Store {
    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root).context("Create pico-evo storage")?;
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
        let path = root.join("sessions.sqlite3");
        let db = Connection::open(&path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS new_session_draft (singleton INTEGER PRIMARY KEY CHECK(singleton=1), data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, updated INTEGER NOT NULL, summary TEXT NOT NULL, metadata TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS session_order ON sessions(updated DESC, id);
            CREATE TABLE IF NOT EXISTS entries (session TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, position INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY(session,position));
            CREATE TABLE IF NOT EXISTS subagent_runs (id TEXT PRIMARY KEY, parent TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, session TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, request TEXT NOT NULL, metadata TEXT NOT NULL, result TEXT, error TEXT, UNIQUE(parent,request));
            CREATE INDEX IF NOT EXISTS subagent_parent ON subagent_runs(parent);
            CREATE TABLE IF NOT EXISTS context (session TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, position INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY(session,position));")?;
        Ok(Self {
            root,
            db: Arc::new(Mutex::new(db)),
            _lock: Arc::new(lock),
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
    pub fn load(&self) -> Result<(Catalog, Vec<SessionSummary>, Vec<String>)> {
        let mut catalog: Catalog = self.read_optional("settings.json")?.unwrap_or_default();
        catalog.settings.refresh_models();
        catalog.settings.validate()?;
        let db = self.db.lock().expect("database lock");
        let mut query = db.prepare("SELECT id,summary FROM sessions ORDER BY updated DESC,id")?;
        let rows = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut summaries = vec![];
        let mut warnings = vec![];
        for row in rows {
            let (id, json) = row?;
            let parsed = (|| {
                let summary: SessionSummary = serde_json::from_str(&json)?;
                Uuid::parse_str(&id)?;
                if summary.id != id {
                    bail!("Session identity mismatch");
                }
                Ok::<_, anyhow::Error>(summary)
            })();
            match parsed {Ok(summary)=>summaries.push(summary),Err(error)=>warnings.push(format!("Could not load session {id}: {error:#}. Database row preserved; restore it from a backup or repair it before retrying."))}
        }
        Ok((catalog, summaries, warnings))
    }
    pub fn load_session(&self, id: &str) -> Result<Session> {
        let db = self.db.lock().expect("database lock");
        let metadata: String = db
            .query_row("SELECT metadata FROM sessions WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .context("Session no longer exists")?;
        let mut session: Session = serde_json::from_str(&metadata)?;
        if session.id != id {
            bail!("Session metadata identity mismatch; database preserved");
        }
        session.entries = read_history(&db, "entries", id)?;
        session.input = read_history(&db, "context", id)?;
        Ok(session)
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
    pub fn load_new_draft(&self) -> Result<NewSessionDraft> {
        let db = self.db.lock().expect("database lock");
        let json: Option<String> = db
            .query_row(
                "SELECT data FROM new_session_draft WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match json {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(NewSessionDraft::default()),
        }
    }
    pub fn save_new_draft(&self, draft: &NewSessionDraft) -> Result<()> {
        self.db.lock().expect("database lock").execute(
            "INSERT INTO new_session_draft VALUES (1,?1) ON CONFLICT(singleton) DO UPDATE SET data=excluded.data",
            [serde_json::to_string(draft)?],
        )?;
        Ok(())
    }
    /// Commit the first message and consume its shared draft in one transaction.
    pub fn commit_new_session(&self, session: &Session) -> Result<()> {
        self.save_session_transaction(None, session, true, None)
    }
    pub fn save_session(&self, session: &Session) -> Result<()> {
        self.save_changes(None, session)
    }
    pub fn save_changes(&self, before: Option<&Session>, session: &Session) -> Result<()> {
        self.save_session_transaction(before, session, false, None)
    }
    pub(crate) fn save_session_transaction(
        &self,
        before: Option<&Session>,
        session: &Session,
        consume_draft: bool,
        run: Option<&crate::agent::subagents::RunInfo>,
    ) -> Result<()> {
        Uuid::parse_str(&session.id)?;
        let mut db = self.db.lock().expect("database lock");
        let tx = db.transaction()?;
        tx.execute("INSERT INTO sessions VALUES (?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET updated=excluded.updated,summary=excluded.summary,metadata=excluded.metadata",
            params![session.id,session.updated as i64,serde_json::to_string(&SessionSummary::from(session))?,serde_json::to_string(session)?])?;
        write_history(
            &tx,
            "entries",
            &session.id,
            before.map(|s| &s.entries),
            &session.entries,
        )?;
        write_history(
            &tx,
            "context",
            &session.id,
            before.map(|s| &s.input),
            &session.input,
        )?;
        if let Some(run) = run {
            tx.execute("INSERT INTO subagent_runs (id,parent,session,request,metadata) VALUES (?1,?2,?3,?4,?5)", params![run.run_id,run.parent_id,run.session_id,run.request_id,serde_json::to_string(run)?])?;
        }
        if consume_draft {
            tx.execute("DELETE FROM new_session_draft", [])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.db
            .lock()
            .expect("database lock")
            .execute("DELETE FROM sessions WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn api_key(&self) -> Result<Option<String>> {
        self.read_optional::<String>("image-api-key.json")
            .map(|v| v.filter(|s| !s.is_empty()))
    }
    pub fn save_api_key(&self, key: &str) -> Result<()> {
        let key = key.trim();
        if key.chars().any(char::is_whitespace) {
            bail!("API key cannot contain whitespace");
        }
        self.write("image-api-key.json", &key)
    }
    pub fn write(&self, name: &str, value: &impl Serialize) -> Result<()> {
        atomic_write(&self.root.join(name), &serde_json::to_vec(value)?)
    }
}
fn read_history<T: Record + serde::de::DeserializeOwned>(
    db: &Connection,
    table: &str,
    id: &str,
) -> Result<History<T>> {
    let mut query = db.prepare(&format!(
        "SELECT position,data FROM {table} WHERE session=?1 ORDER BY position"
    ))?;
    let rows = query.query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    rows.enumerate()
        .map(|(expected, row)| {
            let (position, json) = row?;
            if position != expected as i64 {
                bail!("Invalid {table} position in session {id}; database preserved");
            }
            Ok(serde_json::from_str(&json)?)
        })
        .collect()
}

fn write_history<T: Record + Serialize>(
    db: &Connection,
    table: &str,
    id: &str,
    before: Option<&History<T>>,
    next: &History<T>,
) -> Result<()> {
    let empty = History::default();
    let mut upsert = db.prepare_cached(&format!("INSERT INTO {table} VALUES (?1,?2,?3) ON CONFLICT(session,position) DO UPDATE SET data=excluded.data"))?;
    let mut delete = db.prepare_cached(&format!(
        "DELETE FROM {table} WHERE session=?1 AND position=?2"
    ))?;
    if before.is_none() {
        db.execute(&format!("DELETE FROM {table} WHERE session=?1"), [id])?;
    }
    for (index, value) in before.unwrap_or(&empty).changes(next) {
        if let Some(value) = value {
            upsert.execute(params![id, index as i64, serde_json::to_string(value)?])?;
        } else {
            delete.execute(params![id, index as i64])?;
        }
    }
    Ok(())
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
    fn image_submissions_survive_reopen_and_project_only_at_provider_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(3, 2, image::Rgba([13, 170, 82, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let image = crate::media::MediaStore::new(dir.path().join("attachments"))
            .image(png.get_ref())
            .unwrap();
        let message = UserMessage {
            text: String::new(),
            images: vec![image.clone()],
        };
        assert!(!message.is_empty());
        let mut session = Session::new(dir.path().into(), &Settings::default());
        session.draft_images = message.images.clone();
        session.accept_submission("image-only".into(), message.clone());
        assert_eq!(session.title, "Image message");
        assert!(session.draft_images.is_empty());
        session.accept_submission(
            "steering".into(),
            UserMessage {
                text: "Compare this image".into(),
                images: vec![image.clone()],
            },
        );
        let id = session.id.clone();
        {
            let store = Store::open(dir.path().join("state")).unwrap();
            store.save_session(&session).unwrap();
        }
        let store = Store::open(dir.path().join("state")).unwrap();
        let mut saved = store.load_session(&id).unwrap();
        assert_eq!(saved.pending[0], message);
        assert!(
            matches!(&saved.entries[0].kind, EntryKind::User { images } if images == &vec![image.clone()])
        );
        saved.consume_pending();
        assert_eq!(saved.input.len(), 2);
        assert_eq!(saved.input[1]["content"][0]["text"], "Compare this image");
        assert_eq!(
            crate::media::recent(saved.input.iter().cloned(), 1).unwrap(),
            vec![image]
        );
        let mut wire = serde_json::json!(saved.input.iter().collect::<Vec<_>>());
        assert!(!wire.to_string().contains("data:image/"));
        crate::media::project(&mut wire, false).unwrap();
        assert_eq!(wire[0]["content"][1]["type"], "input_image");
        assert!(
            wire[0]["content"][1]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(saved.input[0]["content"][1]["type"], "pico_media");
    }

    #[test]
    fn cumulative_usage_survives_reopen_without_replacing_current_context_usage() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        for (input, output, cached) in [(1201, 17, 1000), (2345, 43, 2200)] {
            session.usage.record_response(Usage {
                input,
                output,
                cached,
                ..Default::default()
            });
        }
        {
            let store = Store::open(dir.path().into()).unwrap();
            store.save_session(&session).unwrap();
        }
        let store = Store::open(dir.path().into()).unwrap();
        let mut restored = store.load_session(&session.id).unwrap();
        assert_eq!(restored.usage.total_input, 3546);
        assert_eq!(restored.usage.total_output, 60);
        assert_eq!(restored.usage.total_cached, 3200);
        assert_eq!(restored.usage.input, 2345);
        assert_eq!(restored.usage.cached, 2200);
        // Compaction resets the context estimate, not session-wide totals.
        restored.usage.input = 0;
        restored.usage.record_response(Usage {
            input: 100,
            output: 9,
            cached: 0,
            ..Default::default()
        });
        assert_eq!(restored.usage.total_input, 3646);
        assert_eq!(restored.usage.total_output, 69);
        assert_eq!(restored.usage.total_cached, 3200);
        assert_eq!(restored.usage.cached, 0);
        let existing: Usage = serde_json::from_value(serde_json::json!({
            "input":1234,"output":12,"cached":1000,"total_output":57
        }))
        .unwrap();
        assert_eq!(existing.total_input, 0);
    }

    #[test]
    fn model_capabilities_and_catalog_refresh_preserve_current_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let mut catalog = Catalog::default();
        // The catalog supplies capabilities; this test protects validation and refresh,
        // not today's model names, count, or default reasoning effort.
        for (index, model) in MODELS.iter().enumerate() {
            let preferences = catalog.settings.models.get_mut(model.id).unwrap();
            preferences.effort = model.efforts.last().unwrap().to_string();
            preferences.fast = index % 2 == 0;
        }
        catalog.settings.validate().unwrap();
        let mut expected = catalog.settings.models.clone();
        for model in MODELS {
            // Also reject efforts that are valid for a different model.
            let unsupported = MODELS
                .iter()
                .flat_map(|other| other.efforts.iter().copied())
                .filter(|effort| !model.efforts.contains(effort))
                .chain(std::iter::once("unsupported-fixture-effort"));
            for effort in unsupported {
                catalog.settings.models.get_mut(model.id).unwrap().effort = effort.into();
                assert!(
                    catalog.settings.validate().is_err(),
                    "{} accepted {effort}",
                    model.id
                );
            }
            catalog.settings.models.get_mut(model.id).unwrap().effort =
                expected[model.id].effort.clone();
        }
        catalog.settings.models.remove(DEFAULT_MODEL);
        expected.insert(DEFAULT_MODEL.into(), ModelDefaults::default());
        catalog.settings.model = "retired-fixture-model".into();
        catalog.settings.models.insert(
            "retired-fixture-model".into(),
            ModelDefaults {
                effort: "low".into(),
                fast: true,
            },
        );
        // Simulate settings saved by a previous catalog; normal writes reject retired IDs.
        store.write("settings.json", &catalog).unwrap();
        let (loaded, _, _) = store.load().unwrap();
        assert_eq!(loaded.settings.model, DEFAULT_MODEL);
        assert_eq!(loaded.settings.models, expected);
        loaded.settings.validate().unwrap();
    }

    #[test]
    fn shared_draft_survives_reopen_and_is_consumed_atomically_with_first_message() {
        let dir = tempfile::tempdir().unwrap();
        let draft = NewSessionDraft {
            images: vec![],
            project: Some(dir.path().join("second-project")),
            message: "First message λ\nWith another line".into(),
        };
        {
            let store = Store::open(dir.path().into()).unwrap();
            store.save_new_draft(&draft).unwrap();
            assert!(store.load().unwrap().1.is_empty());
        }
        let store = Store::open(dir.path().into()).unwrap();
        assert_eq!(store.load_new_draft().unwrap(), draft);
        let mut session = Session::new(draft.project.clone().unwrap(), &Settings::default());
        session.accept_submission("first-submission".into(), draft.message.clone().into());
        // Failure consuming the draft must also roll back the new session.
        let db = Connection::open(dir.path().join("sessions.sqlite3")).unwrap();
        crate::test_support::sqlite::fail_writes(
            &db,
            "new_session_draft",
            "fixture consume failure",
        );
        assert!(store.commit_new_session(&session).is_err());
        assert!(store.load().unwrap().1.is_empty());
        assert_eq!(store.load_new_draft().unwrap(), draft);
        crate::test_support::sqlite::allow_writes(&db, "new_session_draft");
        store.commit_new_session(&session).unwrap();
        drop(db);
        drop(store);
        let store = Store::open(dir.path().into()).unwrap();
        assert_eq!(store.load_new_draft().unwrap(), NewSessionDraft::default());
        let saved = store.load_session(&session.id).unwrap();
        assert_eq!(saved.entries.len(), 1);
        assert_eq!(saved.entries[0].id, "first-submission");
        assert_eq!(saved.entries[0].text, draft.message);
    }

    #[test]
    fn incremental_save_only_changes_touched_rows_and_is_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        for text in ["first", "middle", "last"] {
            session.entries.push(Entry::new(EntryKind::Assistant, text));
        }
        session.input = vec![serde_json::json!({"role":"user","content":"context"})].into();
        store.save_session(&session).unwrap();
        let before = session.clone();
        let db = Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        for (operation, row) in [("INSERT", "NEW"), ("UPDATE", "OLD"), ("DELETE", "OLD")] {
            db.execute_batch(&format!("CREATE TRIGGER protect_history_{operation} BEFORE {operation} ON entries WHEN {row}.position != 1 BEGIN SELECT RAISE(FAIL,'unchanged entry rewritten'); END;")).unwrap();
        }
        crate::test_support::sqlite::fail_writes(&db, "context", "unchanged context rewritten");
        session.entries[1].text = "edited middle".into();
        session.draft = "draft only metadata".into();
        store.save_changes(Some(&before), &session).unwrap();
        assert_eq!(
            store.load_session(&session.id).unwrap().entries[1].text,
            "edited middle"
        );
        let before = session.clone();
        session.entries[0].text = "must roll back".into();
        session.title = "must also roll back".into();
        assert!(store.save_changes(Some(&before), &session).is_err());
        let restored = store.load_session(&session.id).unwrap();
        assert_eq!(restored.entries[0].text, "first");
        assert_eq!(restored.title, before.title);
        assert_eq!(
            before.entries[0].text, "first",
            "snapshots must remain immutable"
        );
    }
    #[test]
    fn late_notification_remains_valid_after_originating_call_is_compacted() {
        let mut session = Session::new(std::env::temp_dir(), &Settings::default());
        session.input =
            vec![serde_json::json!({"type":"compaction","encrypted_content":"opaque"})].into();
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
        s.input =
            vec![serde_json::json!({"type":"compaction","encrypted_content":"opaque"})].into();
        store.save_session(&s).unwrap();
        store.save_catalog(&Catalog::default()).unwrap();
        let (_, restored, _) = store.load().unwrap();
        assert_eq!(
            store.load_session(&restored[0].id).unwrap().entries[0].text,
            "Fix the odd-sized buffer"
        );
        assert_eq!(
            store.load_session(&restored[0].id).unwrap().input[0]["encrypted_content"],
            "opaque"
        );
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
    fn legacy_files_are_untouched_and_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        fs::create_dir(dir.path().join("sessions")).unwrap();
        fs::write(dir.path().join("sessions/broken.json"), b"{").unwrap();
        let s = Session::new(dir.path().into(), &Settings::default());
        store.save_session(&s).unwrap();
        let (_, sessions, warnings) = store.load().unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(warnings.is_empty());
        assert_eq!(
            fs::read(dir.path().join("sessions/broken.json")).unwrap(),
            b"{"
        );
    }
}
