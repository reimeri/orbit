//! Durable V2 state. Provider context and the complete visible transcript are separate.
mod forks;
use crate::{
    agent::subagents::Profile,
    history::{History, Record},
    models::{DEFAULT_MODEL, MODELS, model},
};
use anyhow::{Context, Result, bail};
pub use forks::ForkOrigin;
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
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub stashed: bool,
}
impl Project {
    pub fn name(&self) -> String {
        if let Some(name) = &self.display_name {
            return name.clone();
        }
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentSettings {
    pub model: String,
    pub effort: String,
    pub fast: bool,
}

fn subagent_defaults() -> BTreeMap<Profile, SubagentSettings> {
    Profile::ALL
        .into_iter()
        .map(|p| (p, p.defaults()))
        .collect()
}

pub const DEFAULT_THEME: &str = "Moonfly";

fn default_theme() -> String {
    DEFAULT_THEME.to_owned()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpellcheckSettings {
    pub enabled: bool,
    pub language: String,
}

impl Default for SpellcheckSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            language: "en_US".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub images: crate::image_tools::Settings,
    #[serde(default)]
    pub spellcheck: SpellcheckSettings,
    pub model: String,
    #[serde(default = "model_defaults")]
    pub models: BTreeMap<String, ModelDefaults>,
    #[serde(default = "subagent_defaults")]
    pub subagents: BTreeMap<Profile, SubagentSettings>,
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
            theme: default_theme(),
            images: crate::image_tools::Settings::default(),
            spellcheck: SpellcheckSettings::default(),
            model: DEFAULT_MODEL.into(),
            models: model_defaults(),
            subagents: subagent_defaults(),
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
        for profile in Profile::ALL {
            let config = self
                .subagents
                .entry(profile)
                .or_insert_with(|| profile.defaults());
            if model(&config.model).is_none() {
                *config = profile.defaults();
            }
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
        for profile in Profile::ALL {
            let config = self
                .subagents
                .get(&profile)
                .with_context(|| format!("Missing configuration for {}.", profile.label()))?;
            let selected = model(&config.model)
                .with_context(|| format!("Choose a supported model for {}.", profile.label()))?;
            if !selected.efforts.contains(&config.effort.as_str()) {
                bail!(
                    "Choose a supported reasoning effort for {}.",
                    profile.label()
                );
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
    #[serde(default)]
    pub files: Vec<crate::file_tags::Attachment>,
    /// Explicit skill bodies captured at submission, never developer instructions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
}
impl UserMessage {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty()
    }
    pub fn content(&self) -> Value {
        if self.images.is_empty() && self.files.is_empty() && self.skills.is_empty() {
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
            parts.push(crate::media::Part::OrbitMedia {
                media: media.clone(),
            });
        }
        let mut parts: Vec<_> = parts
            .into_iter()
            .map(|part| serde_json::to_value(part).expect("content part"))
            .collect();
        for skill in &self.skills {
            parts.push(serde_json::json!({"type":"orbit_skill", "text":skill}));
        }
        for file in &self.files {
            if file.image.is_some() {
                parts.push(serde_json::json!({"type":"input_text", "text":format!("Attached workspace image {} ({} bytes)", file.name, file.bytes)}));
            }
            parts.push(file.content());
        }
        serde_json::json!(parts)
    }
}
impl From<String> for UserMessage {
    fn from(text: String) -> Self {
        Self {
            text,
            images: vec![],
            skills: vec![],
            files: vec![],
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
        /// Unix time in seconds when this submission was accepted.
        #[serde(skip_serializing_if = "Option::is_none")]
        sent_at: Option<u64>,
        images: Vec<crate::media::Media>,
        #[serde(default)]
        files: Vec<crate::file_tags::Attachment>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skills: Vec<String>,
        /// Submitted during an active turn and not yet consumed at a model boundary.
        #[serde(default)]
        steering: bool,
    },
    Assistant,
    Reasoning,
    Notice,
    Tool {
        name: String,
        input: String,
        output: String,
        complete: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_status: Option<crate::shell::CommandStatus>,
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
/// A response may execute tools before its final output arrives. Keep the raw
/// completed items and results durable until they can be committed in model order.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InProgressResponse {
    pub items: im::OrdMap<usize, Value>,
    pub results: im::OrdMap<usize, Value>,
}

/// Request-level effort for this model's context. Later changes live in ordered input.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct EffortBaseline {
    model: String,
    pub(crate) effort: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilingKind {
    Deferred,
    Settled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub kind: FilingKind,
    pub at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub origin: Option<ForkOrigin>,
    #[serde(skip)]
    pub(crate) points: im::OrdMap<usize, Arc<forks::Point>>,
    #[serde(default)]
    pub draft_files: Vec<crate::file_tags::Attachment>,
    pub id: String,
    #[serde(default)]
    pub child: Option<crate::agent::subagents::Child>,
    #[serde(default)]
    pub subagent_notices: Vec<String>,
    pub project: PathBuf,
    pub title: String,
    /// Explicit renames win even if the chosen text equals the automatic fallback.
    #[serde(default)]
    pub title_manually_set: bool,
    /// Sidebar age/order: last accepted message, answer, or successful completion.
    pub updated: u64,
    pub model: String,
    pub effort: String,
    pub(crate) effort_baseline: Option<EffortBaseline>,
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
    #[serde(default)]
    pub in_progress_response: Option<InProgressResponse>,
    pub interrupted: bool,
    pub needs_attention: bool,
    /// A successful main turn completed since the session was last opened.
    #[serde(default)]
    pub unchecked_completion: bool,
    /// Sidebar category and filing time; absent for active sessions.
    pub filing: Option<Filing>,
    pub context_account: Option<String>,
}
impl Session {
    pub fn new(project: PathBuf, settings: &Settings) -> Self {
        Self {
            id: id(),
            origin: None,
            points: Default::default(),
            draft_files: vec![],
            child: None,
            subagent_notices: vec![],
            project,
            title: "New session".into(),
            title_manually_set: false,
            updated: now(),
            model: settings.model.clone(),
            effort: settings.models[&settings.model].effort.clone(),
            effort_baseline: None,
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
            in_progress_response: None,
            interrupted: false,
            needs_attention: false,
            unchecked_completion: false,
            filing: None,
            context_account: None,
        }
    }
    pub(crate) fn latest_effort_update(&self) -> Option<&Value> {
        self.input
            .iter()
            .rev()
            .find(|item| item["type"] == "configuration_update")
    }

    /// Run at the turn boundary, before appending project guidance or the user's message.
    pub(crate) fn prepare_effort(&mut self) {
        let Some(base) = self
            .effort_baseline
            .as_ref()
            .filter(|base| base.model == self.model)
        else {
            // Switching models already starts a full request. Don't replay settings from a
            // different model (e.g. Luna's "none"); leave all conversational items in order.
            if self.latest_effort_update().is_some() {
                self.input = self
                    .input
                    .iter()
                    .filter(|item| item["type"] != "configuration_update")
                    .cloned()
                    .collect();
            }
            self.effort_baseline = Some(EffortBaseline {
                model: self.model.clone(),
                effort: self.effort.clone(),
            });
            return;
        };
        let effective = self
            .latest_effort_update()
            .map(|item| item["reasoning"]["effort"].as_str().expect("effort update"))
            .unwrap_or(&base.effort);
        if effective != self.effort {
            self.input.push(serde_json::json!({"type":"configuration_update","reasoning":{"effort":self.effort}}));
        }
    }

    pub fn select_model(&mut self, id: &str, settings: &Settings) {
        let defaults = &settings.models[id];
        self.model = id.into();
        self.effort = defaults.effort.clone();
        self.fast = defaults.fast;
    }
    /// Returns whether the session was switched to the configured default.
    pub fn reconcile_model(&mut self, settings: &Settings) -> bool {
        if model(&self.model).is_some() {
            return false;
        }
        self.select_model(&settings.model, settings);
        true
    }
    pub fn rename(&mut self, title: String) {
        self.title = title;
        self.title_manually_set = true;
    }
    pub(crate) fn needs_title(&self) -> bool {
        !self.title_manually_set
            && self.title == "New session"
            && self.child.is_none()
            && !self
                .entries
                .iter()
                .any(|e| matches!(e.kind, EntryKind::User { .. }))
    }
    pub fn accept(&mut self, message: UserMessage) {
        self.accept_submission(id(), message, false);
    }
    pub fn accept_submission(&mut self, submission: String, message: UserMessage, steering: bool) {
        self.accept_submission_at(submission, message, steering, now());
    }
    pub(crate) fn accept_submission_at(
        &mut self,
        submission: String,
        message: UserMessage,
        steering: bool,
        accepted_at: u64,
    ) {
        if self.needs_title()
            || (!self.title_manually_set
                && self
                    .origin
                    .as_ref()
                    .is_some_and(|o| o.inherited == self.entries.len()))
        {
            self.title = message
                .text
                .lines()
                .find(|s| !s.trim().is_empty())
                .unwrap_or("Image message")
                .chars()
                .take(64)
                .collect();
        }
        self.updated = accepted_at;
        self.unchecked_completion = false;
        self.filing = None;
        self.pending_submissions.push(submission.clone());
        self.entries.push(Entry {
            id: submission,
            kind: EntryKind::User {
                images: message.images.clone(),
                files: message.files.clone(),
                skills: message.skills.clone(),
                sent_at: Some(accepted_at),
                steering,
            },
            text: message.text.clone(),
        });
        self.pending.push(message);
        self.draft.clear();
        self.draft_images.clear();
        self.draft_files.clear();
    }
    pub fn consume_pending(&mut self) {
        for notice in self.subagent_notices.drain(..) {
            self.input
                .push(serde_json::json!({"role":"user", "content":notice}));
        }
        for submission in self.pending_submissions.drain(..) {
            let entry = self
                .entries
                .by_id_mut(&submission)
                .expect("accepted submission");
            let EntryKind::User { steering, .. } = &mut entry.kind else {
                unreachable!("submissions are user entries");
            };
            *steering = false;
        }
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
    pub origin: Option<ForkOrigin>,
    #[serde(default)]
    pub child: Option<crate::agent::subagents::Child>,
    pub needs_attention: bool,
    /// A successful main turn completed since the session was last opened.
    #[serde(default)]
    pub unchecked_completion: bool,
    /// Sidebar category and filing time; absent for active sessions.
    pub filing: Option<Filing>,
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
            origin: s.origin.clone(),
            needs_attention: s.needs_attention,
            unchecked_completion: s.unchecked_completion,
            filing: s.filing,
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
    #[cfg(test)]
    pub(crate) save_trace:
        Option<std::sync::mpsc::Sender<(String, crate::test_support::SaveTiming)>>,
}
impl Store {
    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root).context("Create orbit storage")?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(root.join("instance.lock"))?;
        let lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| anyhow::anyhow!("Another Orbit instance is using {} ({error}). Close it before starting another instance.", root.display()))?;
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
            CREATE TABLE IF NOT EXISTS fork_media (session TEXT REFERENCES sessions(id) ON DELETE CASCADE, owner TEXT NOT NULL, PRIMARY KEY(session,owner));
            CREATE INDEX IF NOT EXISTS fork_media_owner ON fork_media(owner);
            CREATE TABLE IF NOT EXISTS fork_points (session TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, position INTEGER NOT NULL, state TEXT NOT NULL, entries TEXT NOT NULL, input TEXT NOT NULL, PRIMARY KEY(session,position));
            CREATE TABLE IF NOT EXISTS context (session TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, position INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY(session,position));")?;
        use crate::agent::subagents::{ORDINAL, PENDING, UNACKNOWLEDGED};
        db.execute_batch(&format!("
            CREATE INDEX IF NOT EXISTS subagent_child_assignment ON subagent_runs(session, {ORDINAL} DESC);
            CREATE INDEX IF NOT EXISTS subagent_pending ON subagent_runs(session) WHERE {PENDING};
            CREATE INDEX IF NOT EXISTS subagent_attention ON subagent_runs(session, {ORDINAL}) WHERE {UNACKNOWLEDGED};
        "))?;
        Ok(Self {
            root,
            db: Arc::new(Mutex::new(db)),
            _lock: Arc::new(lock),
            #[cfg(test)]
            save_trace: None,
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
    pub(crate) fn load_catalog(&self) -> Result<Catalog> {
        let mut catalog: Catalog = self.read_optional("settings.json")?.unwrap_or_default();
        catalog.settings.refresh_models();
        catalog.settings.validate()?;
        Ok(catalog)
    }
    pub fn load(&self) -> Result<(Catalog, Vec<SessionSummary>, Vec<String>)> {
        let catalog = self.load_catalog()?;
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
    pub(crate) fn contains_session(&self, id: &str) -> Result<bool> {
        Ok(self.db.lock().expect("database lock").query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
            [id],
            |row| row.get(0),
        )?)
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
        forks::load(&db, &mut session)?;
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
        #[cfg(test)]
        let waiting = std::time::Instant::now();
        let mut db = self.db.lock().expect("database lock");
        #[cfg(test)]
        let acquired = std::time::Instant::now();
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
        forks::save(&tx, before, session)?;
        if let Some(run) = run {
            tx.execute("INSERT INTO subagent_runs (id,parent,session,request,metadata) VALUES (?1,?2,?3,?4,?5)", params![run.run_id,run.parent_id,run.session_id,run.request_id,serde_json::to_string(run)?])?;
        }
        if consume_draft {
            tx.execute("DELETE FROM new_session_draft", [])?;
        }
        #[cfg(test)]
        let committing = std::time::Instant::now();
        tx.commit()?;
        #[cfg(test)]
        if let Some(trace) = &self.save_trace {
            let commit = committing.elapsed();
            drop(db);
            trace
                .send((
                    session.id.clone(),
                    crate::test_support::SaveTiming::Database {
                        wait: acquired - waiting,
                        body: committing - acquired,
                        commit,
                    },
                ))
                .expect("live benchmark receiver");
        }
        Ok(())
    }
    /// Media paths are embedded in immutable tool/context records (including
    /// compacted context). Keep ancestor directories until their last user goes.
    pub(crate) fn delete_sessions(
        &self,
        ids: &std::collections::HashSet<String>,
    ) -> Result<Vec<String>> {
        let mut db = self.db.lock().expect("database lock");
        let tx = db.transaction()?;
        let mut candidates = ids.clone();
        for id in ids {
            let mut stmt = tx.prepare_cached("SELECT owner FROM fork_media WHERE session=?1")?;
            for row in stmt.query_map([id], |r| r.get::<_, String>(0))? {
                candidates.insert(row?);
            }
            tx.execute("DELETE FROM sessions WHERE id=?1", [id])?;
        }
        let mut unused = Vec::new();
        for owner in candidates {
            Uuid::parse_str(&owner)?;
            if !tx.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 UNION ALL SELECT 1 FROM fork_media WHERE owner=?1)", [&owner], |r| r.get::<_,bool>(0))? {
                unused.push(owner);
            }
        }
        tx.commit()?;
        Ok(unused)
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
/// The new file is visible to readers, but its rename may not survive a crash.
#[derive(Debug)]
pub struct PublishedButNotSynced;
impl std::fmt::Display for PublishedButNotSynced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("File was replaced, but syncing its directory failed")
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
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .context(PublishedButNotSynced)?;
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
    fn default_looking_titles_do_not_erase_manual_or_first_message_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        for manual in [false, true] {
            let mut session = Session::new(dir.path().into(), &Settings::default());
            if manual {
                session.rename("New session".into());
            } else {
                session.accept("New session".into());
            }
            store.save_session(&session).unwrap();
            let mut restored = store.load_session(&session.id).unwrap();
            assert!(!restored.needs_title());
            restored.accept("This must not rename the conversation".into());
            assert_eq!(restored.title, "New session");
        }
    }

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
            skills: vec![],
            files: vec![],
            text: String::new(),
            images: vec![image.clone()],
        };
        assert!(!message.is_empty());
        let mut session = Session::new(dir.path().into(), &Settings::default());
        session.draft_images = message.images.clone();
        session.accept_submission_at("image-only".into(), message.clone(), false, 1_750_000_123);
        assert_eq!(session.title, "Image message");
        assert!(session.draft_images.is_empty());
        session.accept_submission(
            "steering".into(),
            UserMessage {
                skills: vec![],
                files: vec![],
                text: "Compare this image".into(),
                images: vec![image.clone()],
            },
            true,
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
            matches!(&saved.entries[0].kind, EntryKind::User { images, steering: false, sent_at: Some(1_750_000_123), .. } if images == &vec![image.clone()])
        );
        assert!(matches!(
            saved.entries[1].kind,
            EntryKind::User { steering: true, .. }
        ));
        saved.consume_pending();
        assert!(saved.pending_submissions.is_empty());
        assert!(matches!(
            saved.entries[1].kind,
            EntryKind::User {
                steering: false,
                ..
            }
        ));
        store.save_changes(Some(&session), &saved).unwrap();
        assert!(matches!(
            store.load_session(&id).unwrap().entries[1].kind,
            EntryKind::User {
                steering: false,
                ..
            }
        ));
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
        assert_eq!(saved.input[0]["content"][1]["type"], "orbit_media");
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
        for (index, model) in MODELS.iter().enumerate() {
            let preferences = catalog.settings.models.get_mut(model.id).unwrap();
            preferences.effort = model.efforts.last().unwrap().to_string();
            preferences.fast = index % 2 == 0;
        }
        catalog.settings.validate().unwrap();
        let mut expected = catalog.settings.models.clone();
        for model in MODELS {
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
        // Reproduce the November 2026 refresh: both older families are retired,
        // while Luna's customized preferences must survive.
        for (retired, replacement) in [
            ("gpt-6-astra", crate::models::ASTRA.id),
            ("gpt-6.1-sol", crate::models::SOL.id),
        ] {
            catalog.settings.models.remove(replacement);
            expected.insert(replacement.into(), ModelDefaults::default());
            catalog.settings.models.insert(
                retired.into(),
                ModelDefaults {
                    effort: "max".into(),
                    fast: true,
                },
            );
        }
        catalog.settings.models.remove(crate::models::TERRA.id);
        expected.insert(crate::models::TERRA.id.into(), ModelDefaults::default());
        catalog.settings.model = "gpt-6.1-sol".into();
        catalog.settings.context_limit = 128_000;
        catalog.settings.images.model = "gpt-image-2.5-sunburst".into();
        // Simulate settings saved by a previous catalog; normal writes reject retired IDs.
        store.write("settings.json", &catalog).unwrap();
        let (loaded, _, _) = store.load().unwrap();
        assert_eq!(loaded.settings.model, DEFAULT_MODEL);
        assert_eq!(loaded.settings.models, expected);
        assert_eq!(loaded.settings.context_limit, 128_000);
        assert_eq!(loaded.settings.images.model, "gpt-image-2.5-sunburst");
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
        session.accept_submission(
            "first-submission".into(),
            draft.message.clone().into(),
            false,
        );
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
        let store = Store::open(dir.path().join("orbit")).unwrap();
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
}
