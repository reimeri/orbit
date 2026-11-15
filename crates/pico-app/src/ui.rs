use crate::activity::{self, Row as ActivityRow};
use crate::scrollbar::scroll;
use crate::session_wave::{self, Status as SessionStatus};
use crate::theme::{
    self, ACCENT, BORDER, DANGER, MUTED, SUCCESS, WARNING, card, footer_surface, panel,
    right_panel, secondary, sidebar_control,
};
use crate::{
    chat_selection::{self, Position},
    chevron_icon::chevron_icon,
    ellipsized_text::ellipsized_text,
    pointer_input::{Press, pointer_input},
    tool_output,
};
#[cfg(test)]
use iced::Theme;
use iced::futures::SinkExt;
use iced::{
    Color, Element, Fill, Subscription, Task, keyboard, mouse,
    widget::{
        self, checkbox, column, container, markdown, mouse_area, opaque, pick_list, row,
        scrollable, slider, space, stack, text, text_editor, text_input, tooltip,
    },
    window,
};
use pico_core::CancellationToken;
use pico_core::{
    agent::Harness,
    auth::Login,
    models::{MODELS, model},
    store::{
        Catalog, Entry, EntryKind, NewSessionDraft, Project, Session, SessionSummary, Settings,
        Store, UserMessage,
    },
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::questionnaire;
use unicode_segmentation::UnicodeSegmentation;

// Measurement epochs reject stale sensor messages; they are not widget identity.
#[derive(Clone, PartialEq, Eq, Hash)]
enum TranscriptKey {
    Top,
    Empty,
    Entry {
        session: String,
        entry: String,
        role: usize,
    },
    Bottom,
}

#[derive(Clone)]
enum SidebarRow {
    Heading(&'static str, usize),
    Session(usize, SessionStatus),
}

type PendingLogin = Arc<tokio::sync::Mutex<Option<Login>>>;

const SIDEBAR_CONTENT_PADDING: f32 = 8.;
const RIGHT_PANEL_PADDING: f32 = 18.;
const FOOTER_MENU_TEXT_SIZE: f32 = 12.;
const FAST_OPTION: &str = "Fast mode";
const CHAT_OVERSCAN: f32 = 120.;
const COMPOSER_LINE_HEIGHT_FACTOR: f32 = 1.3;
const COMPOSER_MIN_LINES: usize = 2;
const COMPOSER_MAX_LINES: usize = 10;
const NOTICE_DURATION: Duration = Duration::from_secs(8);
const ACTIVITY_INTERVAL: Duration = Duration::from_millis(100);

struct Markdown {
    source: Arc<Entry>,
    content: markdown::Content,
    images: HashSet<String>,
}

pub struct App {
    harness: Option<Arc<Harness>>,
    catalog: Catalog,
    sessions: Vec<SessionSummary>,
    current_session: Option<Session>,
    draft_session: Option<Session>,
    new_draft: NewSessionDraft,
    new_draft_dirty: bool,
    transcript: crate::virtual_list::List,
    activity: activity::Projection,
    execution: HashSet<String>,
    sidebar_rows: Vec<SidebarRow>,
    sidebar_list: crate::virtual_list::List,
    catalog_revision: u64,
    view_epoch: u64,
    sidebar_epoch: u64,
    sidebar_keys: HashMap<String, usize>,
    io: crate::io_worker::Worker<Message>,
    tasks: Vec<Task<Message>>,
    io_busy: bool,
    closing: bool,
    close_requested: bool,
    submissions: HashMap<String, (UserMessage, String)>,
    revisions: HashMap<String, u64>,
    outputs: HashMap<String, tool_output::Output>,
    raw_tool_inputs: HashSet<String>,
    image_view: crate::image_view::Viewer,
    markdown_images: crate::markdown_images::Cache,
    api_key_draft: String,
    api_key_configured: bool,
    context_menu: Option<(String, iced::Point)>,
    footer_menu: Option<FooterMenu>,
    selected: Option<String>,
    search: String,
    search_active: bool,
    editor: text_editor::Content,
    attachments: Vec<pico_core::media::Media>,
    importing: usize,
    questionnaires: HashMap<String, questionnaire::Draft>,
    active_questionnaire: Option<String>,
    chat_selection: chat_selection::Shared,
    markdown: HashMap<String, Markdown>,
    expanded: HashSet<String>,
    groups: HashSet<String>,
    settings_open: bool,
    agents_open: bool,
    chat_positions: HashMap<String, (f32, bool)>,
    settings_tab: SettingsTab,
    notice: Option<(String, std::time::Instant)>,
    settings: Settings,
    context_text: String,
    threshold_text: String,
    modal: Option<Modal>,
    modal_input: String,
    error: Option<String>,
    account: Option<String>,
    login_cancel: Option<CancellationToken>,
    login_url: Option<String>,
    tick: u64,
    window_width: f32,
    window_height: f32,
    drag: Option<(bool, f32, f32)>,
    auto_scroll: bool,
    sidebar_follow_top: bool,
    git: HashMap<PathBuf, crate::git::Info>,
    git_pending: HashSet<PathBuf>,
    draft_dirty: bool,
    smoke: bool,
    smoke_stage: usize,
    smoke_passed: Arc<AtomicBool>,
    smoke_dir: Option<PathBuf>,
    window_id: Option<window::Id>,
    window_focused: bool,
    notifications: crate::notifications::Client,
    notification_selection: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    Account,
    Images,
    General,
    Models,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterControl {
    Model,
    Effort,
}
struct FooterMenu {
    control: FooterControl,
    options: Vec<String>,
    focused: usize,
}
#[derive(Clone, Debug)]
enum Modal {
    Image(pico_core::media::Media),
    MarkdownImage(Arc<crate::markdown_images::Loaded>),
    AddProject,
    Rename(String),
    Delete(String),
    Logout,
    Quit,
    Link(String),
}
#[derive(Clone, Debug)]
pub enum Selection {
    Picker,
    Session(String),
    Draft(PathBuf),
}
#[derive(Clone, Debug)]
pub enum Message {
    Notification(crate::notifications::Message),
    Image(crate::image_view::Message),
    OpenImage(pico_core::media::Media),
    MarkdownImage(crate::markdown_images::Message),
    OpenMarkdownImage(String),
    RetryMarkdownImage(String),
    ImageBackend(pico_core::image_tools::Backend),
    WebMode(pico_core::web_search::Mode),
    ImageModel(String),
    ImageQuality(String),
    ResetImageModel,
    CancelSettings,
    ApiKey(String),
    SaveApiKey,
    RemoveApiKey,
    ApiKeySaved(Result<bool, String>),
    ApiKeyStatus(Result<bool, String>),
    Selected(Result<(Selection, Option<String>), String>),
    NewDraftSaved(Result<(), String>),
    Sent(String, UserMessage, Result<(), String>),
    DraftSaved(String, UserMessage, Result<(), String>),
    Paste,
    ClipboardText(Option<String>),
    AttachmentLoaded(Result<crate::attachments::Input, String>),
    RemoveAttachment(usize),
    Deleted(String, Result<(), String>),
    Edited(Result<(), String>),
    Tick,
    Changed,
    GitTick,
    SaveDraft,
    AgeTick,
    Press(Press),
    SessionMenu(String, iced::Point),
    CopySessionId(String),
    CloseMenu,
    FooterMenu(FooterControl),
    CloseFooterMenu,
    ToolOutput(String, tool_output::Event),
    CopySelection,
    New,
    Select(String),
    Create(PathBuf),
    Search(String),
    ToggleSearch,
    Favorite(PathBuf),
    Editor(text_editor::Action),
    DeletePreviousWord,
    Questionnaire(String, questionnaire::Message),
    Send,
    ToggleSettings,
    ToggleAgents,
    StopAgent(String),
    ApplySettings,
    CatalogSaved(bool, Result<(), String>),
    SettingsTab(SettingsTab),
    Model(String),
    ModelEffort(String, String),
    ModelFast(String, bool),
    ToggleSessionFast,
    NoticeDismiss,
    NoticeTick,
    SessionModel(String),
    SessionEffort(String),
    Context(String),
    Threshold(String),
    Font(f32),
    ChatWidth(f32),
    Resume(bool),
    AddProject,
    Browse,
    Folder(Option<PathBuf>),
    ModalInput(String),
    Confirm,
    Dismiss,
    Rename(String),
    Delete(String),
    ToggleEntry(String),
    ToggleToolRaw(String),
    ToggleGroup(String),
    ToggleExecution(String),
    Scroll(u64, f32, f32, bool),
    RowSize(u64, usize, f32),
    SidebarScroll(u64, f32, f32),
    SidebarSize(u64, usize, f32),
    Link(String),
    Copy(String),
    Login,
    LoginStarted(Result<(PendingLogin, String), String>),
    LoggedIn(Result<String, String>),
    Account(Option<String>),
    CancelLogin,
    Logout,
    Git(PathBuf, Option<crate::git::Info>),
    External(Result<(), String>),
    ErrorDismiss,
    ResizeStart(bool, iced::Point),
    Event(iced::Event),
    WindowOpened(window::Id),
    Screenshot(window::Screenshot),
    Capture,
    Exit(Result<(), String>),
}
impl App {
    pub fn new(smoke: bool, smoke_passed: Arc<AtomicBool>) -> (Self, Task<Message>) {
        let smoke_dir = smoke.then(|| {
            std::env::temp_dir().join(format!("pico-evo-smoke-{}", pico_core::store::id()))
        });
        let loaded = (|| -> Result<_, String> {
            let store = if let Some(path) = &smoke_dir {
                Store::open(path.clone())
            } else {
                Store::from_env()
            }
            .map_err(|e| format!("{e:#}"))?;
            let (catalog, sessions, warnings) = store.load().map_err(|e| format!("{e:#}"))?;
            let draft = store.load_new_draft().map_err(|e| format!("{e:#}"))?;
            let harness = Harness::new(store, sessions).map_err(|e| format!("{e:#}"))?;
            Ok::<_, String>((harness, catalog, draft, warnings))
        })();
        let (harness, catalog, new_draft, error) = match loaded {
            Ok((h, c, d, w)) => (Some(h), c, d, (!w.is_empty()).then(|| w.join("\n"))),
            Err(error) => (
                None,
                Catalog::default(),
                NewSessionDraft::default(),
                Some(format!(
                    "Cannot open Pico V2 storage: {error}. Fix the indicated file/permissions and restart. Nothing has been reset."
                )),
            ),
        };
        let selected = if catalog.settings.resume_last {
            catalog.selected.clone()
        } else {
            None
        };
        let settings = catalog.settings.clone();
        let mut app = Self {
            harness,
            catalog,
            sessions: vec![],
            current_session: None,
            draft_session: None,
            new_draft,
            new_draft_dirty: false,
            transcript: crate::virtual_list::List::with_overscan(CHAT_OVERSCAN),
            activity: activity::Projection::default(),
            execution: HashSet::new(),
            sidebar_rows: vec![],
            sidebar_list: crate::virtual_list::List::default(),
            catalog_revision: u64::MAX,
            view_epoch: 0,
            sidebar_epoch: 0,
            sidebar_keys: HashMap::new(),
            io: crate::io_worker::Worker::new(),
            tasks: vec![],
            io_busy: false,
            closing: false,
            close_requested: false,
            submissions: HashMap::new(),
            attachments: vec![],
            importing: 0,
            revisions: HashMap::new(),
            outputs: HashMap::new(),
            raw_tool_inputs: HashSet::new(),
            image_view: Default::default(),
            markdown_images: Default::default(),
            api_key_draft: String::new(),
            api_key_configured: false,
            context_menu: None,
            footer_menu: None,
            selected,
            search: String::new(),
            search_active: false,
            editor: text_editor::Content::new(),
            questionnaires: HashMap::new(),
            active_questionnaire: None,
            chat_selection: Default::default(),
            markdown: HashMap::new(),
            expanded: HashSet::new(),
            groups: HashSet::new(),
            settings_open: false,
            agents_open: false,
            chat_positions: HashMap::new(),
            settings_tab: SettingsTab::Account,
            notice: None,
            context_text: settings.context_limit.to_string(),
            threshold_text: settings.compact_percent.to_string(),
            settings,
            modal: None,
            modal_input: String::new(),
            error,
            account: None,
            login_cancel: None,
            login_url: None,
            tick: 0,
            window_width: 1200.,
            window_height: 800.,
            drag: None,
            auto_scroll: true,
            sidebar_follow_top: true,
            git: HashMap::new(),
            git_pending: HashSet::new(),
            draft_dirty: false,
            smoke,
            smoke_stage: 0,
            smoke_passed,
            smoke_dir,
            window_id: None,
            window_focused: false,
            notifications: Default::default(),
            notification_selection: None,
        };
        if let Some(h) = &app.harness {
            let h = h.clone();
            app.tasks.push(app.io.run(move || {
                Message::ApiKeyStatus(
                    h.store
                        .api_key()
                        .map(|k| k.is_some())
                        .map_err(|e| format!("{e:#}")),
                )
            }));
        }
        app.refresh();
        let initial = app.selected.take();
        if let Some(id) = initial {
            app.select(Selection::Session(id));
        } else if app.catalog.settings.resume_last
            && let Some(path) = app.new_draft.project.clone()
        {
            app.select(Selection::Draft(path));
        }
        let task = if let Some(h) = &app.harness {
            let auth = h.auth.clone();
            Task::perform(async move { auth.status().await }, Message::Account)
        } else {
            Task::none()
        };
        let task = Task::batch(std::iter::once(task).chain(app.tasks.drain(..)));
        (app, task)
    }
    fn current(&self) -> Option<&Session> {
        self.current_session
            .as_ref()
            .or(self.draft_session.as_ref())
    }
    fn git_info(&self) -> Option<&crate::git::Info> {
        self.git.get(&self.current()?.project)
    }
    fn refresh_git(&mut self) -> Task<Message> {
        let Some(session) = self.current() else {
            return Task::none();
        };
        let path = session.project.clone();
        // A slow capture must not overlap another for the same project, but
        // must not delay a newly selected project's initial refresh either.
        if !self.git_pending.insert(path.clone()) {
            return Task::none();
        }
        Task::perform(
            async move {
                let info = crate::git::info(path.clone()).await;
                (path, info)
            },
            |(path, info)| Message::Git(path, info),
        )
    }
    fn running(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|id| self.harness.as_ref()?.get(id))
            .is_some_and(|c| c.is_running())
    }
    fn sync_questionnaires(&mut self) {
        let Some(h) = &self.harness else {
            return;
        };
        let previous: HashSet<_> = self.questionnaires.keys().cloned().collect();
        let mut live = HashSet::new();
        let mut active = None;
        for c in h.all() {
            if let Some(questions) = &c.questionnaires {
                for pending in questions.pending() {
                    live.insert(pending.id.clone());
                    if self.selected.as_deref() == Some(c.id.as_str()) && active.is_none() {
                        active = Some(pending.id.clone());
                    }
                    self.questionnaires
                        .entry(pending.id.clone())
                        .or_insert_with(|| questionnaire::Draft::new(pending));
                }
            }
        }
        self.questionnaires.retain(|id, _| live.contains(id));
        if active != self.active_questionnaire {
            if let Some(id) = &active
                && !self.settings_open
                && !self.search_active
                && self.modal.is_none()
                && self.footer_menu.is_none()
                && self.context_menu.is_none()
            {
                let id = id.clone();
                self.tasks.push(
                    self.questionnaires[&id]
                        .focus()
                        .map(move |m| Message::Questionnaire(id.clone(), m)),
                );
            }
            self.active_questionnaire = active;
        }
        if previous != live {
            self.prepare_sidebar();
        }
    }
    fn refresh(&mut self) -> bool {
        // Later smoke stages render synthetic UI fixtures, not harness-owned asks.
        // A delayed core notice must not replace them between captured frames.
        if self.smoke && self.smoke_stage >= 26 {
            return false;
        }
        self.sync_questionnaires();
        let Some(h) = &self.harness else {
            return false;
        };
        let catalog_revision = h.catalog_revision();
        if catalog_revision != self.catalog_revision {
            self.sessions = h.summaries();
            self.sessions
                .sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
            self.catalog_revision = catalog_revision;
            self.prepare_sidebar();
        }
        let h = self.harness.as_ref().expect("harness");
        let Some(conversation) = self.selected.as_ref().and_then(|id| h.get(id)) else {
            if self.current_session.take().is_some() {
                self.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
                self.activity = activity::Projection::default();
                self.view_epoch = self.view_epoch.wrapping_add(1);
            }
            self.outputs.clear();
            self.markdown.clear();
            self.chat_selection.borrow_mut().clear();
            self.prepare_visible();
            return false;
        };
        let revision = conversation.revision();
        if self.revisions.get(&conversation.id) == Some(&revision)
            && self
                .current_session
                .as_ref()
                .is_some_and(|s| s.id == conversation.id)
        {
            return false;
        }
        self.revisions.insert(conversation.id.clone(), revision);
        let snapshot = conversation.snapshot();
        let same = self
            .current_session
            .as_ref()
            .is_some_and(|s| s.id == snapshot.id);
        if !same {
            self.outputs.clear();
            self.markdown.clear();
            self.chat_selection.borrow_mut().clear();
            self.view_epoch = self.view_epoch.wrapping_add(1);
            self.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
            self.activity = activity::Projection::default();
        }
        // Persistent-tree diffs only visit changed leaves, not the full transcript.
        let empty = pico_core::history::History::default();
        let old = if same {
            &self.current_session.as_ref().expect("same session").entries
        } else {
            &empty
        };
        for (index, entry) in old.changes(&snapshot.entries) {
            // Markdown can reparse earlier blocks while streaming. Do not keep
            // stale byte/block positions when a selected entry changes.
            self.chat_selection.borrow_mut().invalidate_entry(index);
            let Some(entry) = entry else {
                continue;
            };
            if let EntryKind::Tool { input, output, .. } = &entry.kind
                && let Some(view) = self.outputs.get_mut(&entry.id)
            {
                view.set(input, output);
            }
            if let Some(markdown) = self.markdown.get_mut(&entry.id) {
                if entry.text.starts_with(&markdown.source.text) {
                    markdown
                        .content
                        .push_str(&entry.text[markdown.source.text.len()..]);
                } else {
                    markdown.content = markdown::Content::parse(&entry.text);
                }
                markdown.images = crate::markdown_images::urls(&markdown.content);
                markdown.source = snapshot.entries.shared(index);
            }
        }
        self.current_session = Some(snapshot);
        self.append_transcript();
        if self.auto_scroll {
            self.transcript.end();
        }
        self.prepare_visible();
        true
    }
    fn append_transcript(&mut self) {
        let Some(session) = &self.current_session else {
            return;
        };
        let Some(start) = self.activity.sync(&session.entries) else {
            return;
        };
        let position = self
            .transcript
            .order
            .partition_point(|&id| ActivityRow::from_id(id).source() < start);
        let rows = self
            .activity
            .rows(start, &self.groups, &self.execution, &self.expanded);
        let selection = self.chat_selection.borrow();
        let selected = selection
            .anchor
            .zip(selection.focus)
            .map(|(a, b)| a.entry.min(b.entry)..=a.entry.max(b.entry));
        drop(selection);
        if let Some(selected) = selected {
            let selected_entry = |id| match ActivityRow::from_id(id) {
                ActivityRow::Entry(i) if selected.contains(&i) => Some(i),
                _ => None,
            };
            if !self.transcript.order[position..]
                .iter()
                .copied()
                .filter_map(selected_entry)
                .eq(rows.iter().map(|(id, _)| *id).filter_map(selected_entry))
            {
                self.chat_selection.borrow_mut().clear();
            }
        }
        self.transcript.replace_tail(position, rows);
    }
    fn rebuild_transcript(&mut self) {
        self.chat_selection.borrow_mut().clear();
        let Some(session) = &self.current_session else {
            return;
        };
        self.activity.sync(&session.entries);
        self.transcript.reset(
            self.activity
                .rows(0, &self.groups, &self.execution, &self.expanded),
        );
    }
    fn prepare_visible(&mut self) {
        let mut visible_media = self.attachments.clone();
        let mut image_urls = HashSet::new();
        if let Some(session) = &self.current_session {
            let visible: Vec<_> = self.transcript.order[self.transcript.visible()]
                .iter()
                .filter_map(|&id| match ActivityRow::from_id(id) {
                    ActivityRow::Entry(i) => Some(i),
                    _ => None,
                })
                .collect();
            let ids: HashSet<_> = visible
                .iter()
                .map(|&i| session.entries[i].id.as_str())
                .collect();
            self.markdown.retain(|id, _| ids.contains(id.as_str()));
            self.outputs
                .retain(|id, _| ids.contains(id.as_str()) && self.expanded.contains(id));
            for &index in &visible {
                let entry = &session.entries[index];
                if let EntryKind::User { images } = &entry.kind {
                    visible_media.extend(images.iter().cloned());
                }
                // User text is literal, including newlines and Markdown syntax.
                if matches!(entry.kind, EntryKind::Assistant | EntryKind::Reasoning) {
                    let markdown = self.markdown.entry(entry.id.clone()).or_insert_with(|| {
                        let content = markdown::Content::parse(&entry.text);
                        Markdown {
                            source: session.entries.shared(index),
                            images: crate::markdown_images::urls(&content),
                            content,
                        }
                    });
                    image_urls.extend(markdown.images.iter().cloned());
                }
                if let EntryKind::Tool {
                    name,
                    input,
                    output,
                    media,
                    ..
                } = &entry.kind
                    && self.expanded.contains(&entry.id)
                {
                    visible_media.extend(media.iter().cloned());
                    self.outputs.entry(entry.id.clone()).or_insert_with(|| {
                        tool_output::Output::new(
                            name,
                            input,
                            output,
                            self.raw_tool_inputs.contains(&entry.id),
                        )
                    });
                }
            }
        }
        self.tasks.push(
            self.markdown_images
                .prepare(
                    self.current_session
                        .as_ref()
                        .map(|s| (s.id.as_str(), s.project.as_path())),
                    image_urls,
                )
                .map(Message::MarkdownImage),
        );
        self.tasks
            .push(self.image_view.prepare(&visible_media).map(Message::Image));
    }
    fn session_status(&self, s: &SessionSummary) -> SessionStatus {
        let h = self.harness.as_ref();
        let c = h.and_then(|h| h.get(&s.id));
        if c.as_ref()
            .is_some_and(|c| c.questionnaires.as_ref().is_some_and(|q| q.has_pending()))
        {
            SessionStatus::Question
        } else if c.as_ref().is_some_and(|c| c.is_turn_active())
            || h.is_some_and(|h| h.active_children(&s.id) > 0)
        {
            SessionStatus::Working
        } else if h.is_some_and(|h| h.children_need_attention(&s.id))
            || s.interrupted
            || s.needs_attention
            || c.as_ref()
                .is_some_and(|c| *c.status.lock().expect("status lock") == "Needs attention")
        {
            SessionStatus::Error
        } else if s.unchecked_completion {
            SessionStatus::Unchecked
        } else {
            SessionStatus::Idle
        }
    }
    fn prepare_sidebar(&mut self) {
        let needle = self.search.to_lowercase();
        let mut sections = [vec![], vec![], vec![]];
        for (index, s) in self.sessions.iter().enumerate() {
            if s.child.is_some() {
                continue;
            }
            if !needle.is_empty()
                && !s.title.to_lowercase().contains(&needle)
                && !s.project.to_string_lossy().to_lowercase().contains(&needle)
            {
                continue;
            }
            let status = self.session_status(s);
            sections[status.section()].push(SidebarRow::Session(index, status));
        }
        self.sidebar_rows.clear();
        for (name, section) in ["WORKING", "NEEDS ATTENTION", "SESSIONS"]
            .into_iter()
            .zip(sections)
        {
            if !section.is_empty() {
                self.sidebar_rows
                    .push(SidebarRow::Heading(name, section.len()));
                self.sidebar_rows.extend(section);
            }
        }
        self.sidebar_epoch = self.sidebar_epoch.wrapping_add(1);
        self.sidebar_list
            .reset(self.sidebar_rows.iter().map(|row| match row {
                SidebarRow::Heading(name, _) => (
                    match *name {
                        "WORKING" => 0,
                        "NEEDS ATTENTION" => 1,
                        _ => 2,
                    },
                    36.,
                ),
                SidebarRow::Session(index, _) => {
                    let next = self.sidebar_keys.len() + 3;
                    (
                        *self
                            .sidebar_keys
                            .entry(self.sessions[*index].id.clone())
                            .or_insert(next),
                        53.,
                    )
                }
            }));
        // Reset normally preserves the visible row, but at the top new rows
        // and sections should remain visible instead of pushing us down.
        if self.sidebar_follow_top {
            self.sidebar_list.offset = 0.;
        }
        self.tasks.push(widget::operation::scroll_to(
            "sessions",
            scrollable::AbsoluteOffset {
                x: 0.,
                y: self.sidebar_list.offset,
            },
        ));
    }
    fn save_catalog(&mut self, close_settings: bool) {
        self.catalog.selected = self.selected.clone();
        if let Some(h) = &self.harness {
            let h = h.clone();
            let catalog = self.catalog.clone();
            self.tasks.push(self.io.run(move || {
                Message::CatalogSaved(
                    close_settings,
                    h.store.save_catalog(&catalog).map_err(|e| {
                        format!(
                            "Could not save settings: {e:#}. Fix storage and retry before quitting."
                        )
                    }),
                )
            }));
        }
    }
    fn take_draft(&mut self) -> Option<(String, UserMessage)> {
        if !self.draft_dirty {
            return None;
        }
        let id = self.selected.clone()?;
        self.draft_dirty = false;
        Some((id, self.composer_message()))
    }
    fn save_draft(&mut self) -> bool {
        if self.io_busy || self.closing {
            return true;
        }
        if self.new_draft_dirty
            && let Some(h) = &self.harness
        {
            let h = h.clone();
            let draft = self.new_draft.clone();
            self.new_draft_dirty = false;
            self.tasks.push(self.io.run(move || Message::NewDraftSaved(
                h.store.save_new_draft(&draft).map_err(|e| format!("Could not save new-session draft: {e:#}. Keep Pico open and retry after fixing storage."))
            )));
        }
        if let Some((id, draft)) = self.take_draft()
            && let Some(h) = &self.harness
        {
            let h = h.clone();
            self.tasks.push(self.io.run(move || {
                let result=h.edit(&id,|s| { s.draft=draft.text.clone(); s.draft_images=draft.images.clone(); }).map_err(|e|format!("Could not save draft: {e:#}. Keep Pico open and retry after fixing storage."));
                Message::DraftSaved(id,draft,result)
            }));
        }
        true
    }
    fn restore_draft(&mut self) {
        self.editor = text_editor::Content::with_text(if self.draft_session.is_some() {
            &self.new_draft.message
        } else {
            self.current_session.as_ref().map_or("", |s| &s.draft)
        });
        self.attachments = if self.draft_session.is_some() {
            self.new_draft.images.clone()
        } else {
            self.current_session
                .as_ref()
                .map_or_else(Vec::new, |s| s.draft_images.clone())
        };
        self.draft_dirty = false;
        self.prepare_visible();
    }
    fn composer_message(&self) -> UserMessage {
        UserMessage {
            text: self.editor.text(),
            images: self.attachments.clone(),
        }
    }
    fn attachment_changed(&mut self) {
        if self.draft_session.is_some() {
            self.new_draft.images = self.attachments.clone();
            self.new_draft_dirty = true;
        } else {
            self.draft_dirty = true;
        }
        self.prepare_visible();
    }
    fn can_attach(&self) -> bool {
        self.current().is_some_and(|s| s.child.is_none())
            && !self.closing
            && !self.close_requested
            && (!self.io_busy || self.importing > 0)
            && self.modal.is_none()
            && self.active_questionnaire.is_none()
            && self.context_menu.is_none()
            && self.footer_menu.is_none()
    }
    fn import_file(&mut self, path: PathBuf) {
        if !self.can_attach() {
            return;
        }
        let root = self
            .harness
            .as_ref()
            .expect("active harness")
            .store
            .root()
            .join("attachments");
        self.importing += 1;
        self.io_busy = true;
        self.tasks.push(
            self.io
                .run(move || Message::AttachmentLoaded(crate::attachments::file(&root, &path))),
        );
    }
    fn select(&mut self, selection: Selection) {
        self.chat_selection.borrow_mut().clear();
        self.footer_menu = None;
        if self.io_busy {
            return;
        }
        if let Some(id) = &self.selected {
            self.chat_positions
                .insert(id.clone(), (self.transcript.offset, self.auto_scroll));
        }
        let draft = self.take_draft();
        let settings = self.catalog.settings.clone();
        let mut new_draft = self.new_draft.clone();
        match &selection {
            Selection::Picker => new_draft.project = None,
            Selection::Draft(path) => new_draft.project = Some(path.clone()),
            Selection::Session(_) => {}
        }
        if let Some(h) = &self.harness {
            let h = h.clone();
            self.io_busy = true;
            self.tasks.push(self.io.run(move || {
                Message::Selected(
                    (|| {
                        if let Some((old, draft)) = draft {
                            h.edit(&old, |s| { s.draft = draft.text; s.draft_images = draft.images; })?;
                        }
                        let id = if let Selection::Session(id) = &selection {
                            if h.load(id)?.snapshot().unchecked_completion {
                                h.edit(id, |s| s.unchecked_completion = false)?;
                            }
                            Some(id.as_str())
                        } else {
                            None
                        };
                        h.store.save_new_draft(&new_draft)?;
                        h.evict_inactive(id)?;
                        // Recovery is the last fallible step: once saved, its notice
                        // must reach the UI even if other selection work failed earlier.
                        let retired = id.map(|id| h.reconcile_model(id, &settings)).transpose()?.flatten();
                        let notice = retired.map(|retired| format!("{retired} is no longer in the model catalog. This session now uses {} with its default effort and Fast setting.", settings.model));
                        Ok::<_, pico_core::Error>((selection, notice))
                    })()
                    .map_err(|e| {
                        format!("Could not switch sessions: {e:#}. Fix storage and retry.")
                    }),
                )
            }));
        }
    }
    fn acknowledge_selected_attention(&mut self) {
        if let (Some(h), Some(id)) = (&self.harness, &self.selected)
            && let Some(attention) = h.attention(id)
        {
            let h = h.clone();
            self.tasks.push(self.io.run(move || {
                Message::Edited(h.acknowledge_attention(attention).map_err(|e| {
                    format!("Could not save attention acknowledgement: {e:#}. Keep Pico open, fix storage, then retry saving or revisit the session.")
                }))
            }));
        }
    }
    fn fail(&mut self, result: Result<(), impl std::fmt::Display>) {
        if let Err(e) = result {
            self.error = Some(e.to_string());
        }
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.update_inner(message);
        if !self.smoke
            && let Some(h) = &self.harness
        {
            self.notifications
                .sync(h, self.window_focused, self.selected.as_deref());
        }
        if !self.io_busy
            && !self.closing
            && !self.close_requested
            && let Some(id) = self.notification_selection.take()
            && self.sessions.iter().any(|s| s.id == id)
        {
            self.select(Selection::Session(id));
        }
        if !matches!(self.modal, Some(Modal::Image(_) | Modal::MarkdownImage(_))) {
            self.image_view.close();
        }
        if self.modal.is_some() || self.context_menu.is_some() {
            self.footer_menu = None;
        }
        let close = if self.close_requested && !self.io_busy && !self.closing {
            self.close_requested = false;
            self.quit()
        } else {
            Task::none()
        };
        Task::batch([task, close].into_iter().chain(self.tasks.drain(..)))
    }
    fn update_inner(&mut self, message: Message) -> Task<Message> {
        if (self.closing || self.close_requested)
            && matches!(
                message,
                Message::Editor(_)
                    | Message::Questionnaire(_, _)
                    | Message::DeletePreviousWord
                    | Message::Send
                    | Message::Select(_)
                    | Message::New
                    | Message::Create(_)
                    | Message::Confirm
                    | Message::SaveDraft
                    | Message::SessionModel(_)
                    | Message::SessionEffort(_)
                    | Message::ToggleSessionFast
                    | Message::Favorite(_)
                    | Message::ApplySettings
                    | Message::Font(_)
                    | Message::ChatWidth(_)
                    | Message::Resume(_)
            )
        {
            return Task::none();
        }
        if self.current().is_some_and(|s| s.child.is_some())
            && matches!(
                message,
                Message::Editor(_)
                    | Message::Questionnaire(_, _)
                    | Message::DeletePreviousWord
                    | Message::Send
                    | Message::SessionModel(_)
                    | Message::SessionEffort(_)
                    | Message::ToggleSessionFast
                    | Message::FooterMenu(_)
            )
        {
            return Task::none();
        }
        match message {
            Message::Notification(crate::notifications::Message::Open(event)) => {
                if !self.closing
                    && !self.close_requested
                    && self.notifications.contains(&event)
                    && self
                        .harness
                        .as_ref()
                        .is_some_and(|h| h.notification_is_current(&event))
                {
                    self.notification_selection = Some(event.session_id);
                    if let Some(id) = self.window_id {
                        // Native Wayland activation is compositor-controlled. Iced cannot
                        // apply notification activation tokens to an existing surface.
                        return window::request_user_attention(
                            id,
                            Some(window::UserAttention::Informational),
                        );
                    }
                }
            }
            Message::Notification(crate::notifications::Message::Unavailable(error)) => {
                self.notice = Some((error, std::time::Instant::now()));
            }
            Message::Paste => {
                if !self.can_attach() {
                    return Task::none();
                }
                self.importing += 1;
                self.io_busy = true;
                return iced::clipboard::read().map(Message::ClipboardText);
            }
            Message::ClipboardText(text) => {
                let root = self
                    .harness
                    .as_ref()
                    .expect("active harness")
                    .store
                    .root()
                    .join("attachments");
                self.tasks.push(self.io.run(move || {
                    Message::AttachmentLoaded(crate::attachments::clipboard(&root, text))
                }));
            }
            Message::AttachmentLoaded(result) => {
                self.importing -= 1;
                self.io_busy = self.importing > 0;
                match result {
                    Ok(crate::attachments::Input::Image(media)) => {
                        self.attachments.push(media);
                        self.attachment_changed();
                    }
                    Ok(crate::attachments::Input::Text(text)) => {
                        self.editor
                            .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                                Arc::new(text),
                            )));
                        if self.draft_session.is_some() {
                            self.new_draft.message = self.editor.text();
                            self.new_draft_dirty = true;
                        } else {
                            self.draft_dirty = true;
                        }
                    }
                    Ok(crate::attachments::Input::Empty) => {}
                    Err(error) => self.error = Some(format!("Could not attach image: {error}")),
                }
            }
            Message::RemoveAttachment(index) => {
                if !self.can_attach() || self.io_busy {
                    return Task::none();
                }
                self.attachments.remove(index);
                self.attachment_changed();
            }
            Message::Image(message) => {
                let skipped = matches!(message, crate::image_view::Message::Thumbnail(_, None));
                self.image_view.update(message);
                if skipped {
                    self.prepare_visible();
                }
            }
            Message::MarkdownImage(message) => self.markdown_images.update(message),
            Message::RetryMarkdownImage(url) => {
                self.markdown_images.retry(&url);
                self.prepare_visible();
            }
            Message::OpenMarkdownImage(url) => {
                if let Some(image) = self.markdown_images.loaded(&url) {
                    let task = self
                        .image_view
                        .open_bytes(&image.url, image.bytes.clone())
                        .map(Message::Image);
                    self.modal = Some(Modal::MarkdownImage(image));
                    return task;
                }
            }
            Message::OpenImage(media) => {
                let task = self.image_view.open(&media).map(Message::Image);
                self.modal = Some(Modal::Image(media));
                return task;
            }
            Message::WebMode(mode) => self.settings.web_search = mode,
            Message::ImageBackend(backend) => self.settings.images.backend = backend,
            Message::ImageModel(model) => self.settings.images.model = model,
            Message::ImageQuality(quality) => self.settings.images.quality = quality,
            Message::ResetImageModel => {
                self.settings.images.model = pico_core::image_tools::DEFAULT_API_MODEL.into()
            }
            Message::CancelSettings => {
                self.settings = self.catalog.settings.clone();
                self.context_text = self.settings.context_limit.to_string();
                self.threshold_text = self.settings.compact_percent.to_string();
                self.settings_open = false;
                self.api_key_draft.clear();
                self.view_epoch = self.view_epoch.wrapping_add(1);
            }
            Message::ApiKey(key) => self.api_key_draft = key,
            Message::SaveApiKey | Message::RemoveApiKey => {
                if let Some(h) = &self.harness {
                    let h = h.clone();
                    let key = if matches!(message, Message::RemoveApiKey) {
                        String::new()
                    } else {
                        self.api_key_draft.clone()
                    };
                    self.tasks.push(self.io.run(move || {
                        Message::ApiKeySaved(
                            h.store
                                .save_api_key(&key)
                                .map(|()| !key.trim().is_empty())
                                .map_err(|e| {
                                    format!("Could not save API key: {e:#}. Fix storage and retry.")
                                }),
                        )
                    }));
                }
            }
            Message::ApiKeySaved(result) => match result {
                Ok(configured) => {
                    self.api_key_configured = configured;
                    self.api_key_draft.clear();
                }
                Err(e) => self.error = Some(e),
            },
            Message::ApiKeyStatus(result) => match result {
                Ok(configured) => self.api_key_configured = configured,
                Err(e) => self.error = Some(e),
            },
            Message::Press(_) => unreachable!("press requests are resolved by the root widget"),
            Message::Selected(result) => {
                self.io_busy = false;
                match result {
                    Ok((selection, notice)) => {
                        if let Some(notice) = notice {
                            self.notice = Some((notice, std::time::Instant::now()));
                        }
                        self.draft_session = None;
                        self.selected = None;
                        match selection {
                            Selection::Session(id) => self.selected = Some(id),
                            Selection::Picker => self.new_draft.project = None,
                            Selection::Draft(path) => {
                                self.new_draft.project = Some(path.clone());
                                self.draft_session =
                                    Some(Session::new(path, &self.catalog.settings));
                            }
                        }
                        self.new_draft_dirty = false;
                        self.view_epoch = self.view_epoch.wrapping_add(1);
                        self.context_menu = None;
                        self.auto_scroll = true;
                        self.refresh();
                        self.acknowledge_selected_attention();
                        self.restore_draft();
                        let git = self.refresh_git();
                        self.tasks.push(git);
                        self.save_catalog(false);
                        if let Some((offset, follow)) = self
                            .selected
                            .as_ref()
                            .and_then(|id| self.chat_positions.get(id))
                            .copied()
                        {
                            self.auto_scroll = follow;
                            if !follow {
                                self.transcript.offset = offset;
                                self.prepare_visible();
                                return widget::operation::scroll_to(
                                    "chat",
                                    scrollable::AbsoluteOffset {
                                        x: 0.,
                                        y: offset + 20.,
                                    },
                                );
                            }
                        }
                        return bottom();
                    }
                    Err(error) => {
                        self.error = Some(error);
                        self.draft_dirty = self.selected.is_some();
                    }
                }
            }
            Message::NewDraftSaved(result) => {
                if let Err(error) = result {
                    self.error = Some(error);
                    self.new_draft_dirty = true;
                }
            }
            Message::DraftSaved(id, _draft, result) => {
                if let Err(error) = result {
                    self.error = Some(error);
                    if self.selected.as_ref() == Some(&id) {
                        self.draft_dirty = true;
                    }
                }
            }
            Message::Sent(id, text, result) => {
                self.io_busy = false;
                match result {
                    Ok(()) => {
                        if self.draft_session.as_ref().is_some_and(|s| s.id == id) {
                            self.draft_session = None;
                            self.new_draft = NewSessionDraft::default();
                            self.new_draft_dirty = false;
                            self.selected = Some(id.clone());
                            self.save_catalog(false);
                        }
                        self.submissions.remove(&id);
                        if self.selected.as_ref() == Some(&id) && self.composer_message() == text {
                            self.editor = text_editor::Content::new();
                            self.attachments.clear();
                            self.draft_dirty = false;
                        }
                        self.auto_scroll = true;
                        self.refresh();
                        return bottom();
                    }
                    Err(error) => {
                        self.error = Some(error);
                        self.draft_dirty = self.selected.is_some();
                        self.new_draft_dirty |= self.draft_session.is_some();
                    }
                }
            }
            Message::Deleted(id, result) => {
                self.io_busy = false;
                let deleted =
                    result.is_ok() || self.harness.as_ref().is_some_and(|h| h.get(&id).is_none());
                if deleted {
                    let selected = self.parent_id() == Some(id.as_str());
                    if selected {
                        self.draft_dirty = false;
                        self.selected = None;
                    }
                    self.refresh();
                    if selected {
                        self.restore_draft();
                    }
                    self.modal = None;
                }
                self.fail(result);
            }
            Message::Edited(result) => {
                self.fail(result);
                self.refresh();
            }
            Message::Exit(result) => match result {
                Ok(()) => return iced::exit(),
                Err(error) => {
                    self.closing = false;
                    self.draft_dirty = self.selected.is_some();
                    self.new_draft_dirty = true;
                    self.modal = None;
                    self.error = Some(error);
                }
            },
            Message::Tick => self.tick += 1,
            Message::AgeTick => {}
            Message::SaveDraft => {
                self.save_draft();
            }
            Message::Changed => {
                let before = self.catalog_revision;
                let changed = self.refresh();
                if before != self.catalog_revision
                    && !self.io_busy
                    && !self.closing
                    && let Some(h) = &self.harness
                {
                    let h = h.clone();
                    let selected = self.selected.clone();
                    self.tasks.push(self.io.run(move || {
                        Message::External(
                            h.evict_inactive(selected.as_deref())
                                .map_err(|e| format!("Could not release inactive session: {e:#}")),
                        )
                    }));
                }
                if changed && self.auto_scroll {
                    return bottom();
                }
            }
            Message::GitTick => return self.refresh_git(),
            Message::SessionMenu(id, position) => {
                self.context_menu = Some((id, position));
            }
            Message::CopySessionId(id) => {
                self.context_menu = None;
                return iced::clipboard::write(id);
            }
            Message::CloseMenu => self.context_menu = None,
            Message::CloseFooterMenu => self.footer_menu = None,
            Message::FooterMenu(control) => {
                if self
                    .footer_menu
                    .as_ref()
                    .is_some_and(|menu| menu.control == control)
                {
                    self.footer_menu = None;
                } else if !self.running()
                    && let Some(session) = self.current()
                {
                    let (options, selected) = match control {
                        FooterControl::Model => (model_options(), &session.model),
                        FooterControl::Effort => (
                            model(&session.model)
                                .expect("catalog model")
                                .efforts
                                .iter()
                                .copied()
                                .chain([FAST_OPTION])
                                .map(str::to_owned)
                                .collect(),
                            &session.effort,
                        ),
                    };
                    let focused = options
                        .iter()
                        .position(|option| option == selected)
                        .unwrap_or(0);
                    self.footer_menu = Some(FooterMenu {
                        control,
                        options,
                        focused,
                    });
                }
            }
            Message::ToolOutput(id, event) => {
                if let Some(view) = self.outputs.get_mut(&id) {
                    view.update(event);
                }
            }
            Message::New => self.select(Selection::Picker),
            Message::Select(id) => {
                self.select(Selection::Session(id));
                return bottom();
            }
            Message::Create(path) => self.select(Selection::Draft(path)),
            Message::Search(search) => {
                self.search = search;
                self.sidebar_follow_top = true;
                self.sidebar_list.offset = 0.;
                self.prepare_sidebar();
                return widget::operation::snap_to("sessions", scrollable::RelativeOffset::START);
            }
            Message::ToggleSearch => {
                self.search_active = !self.search_active;
                if self.search_active {
                    return widget::operation::focus("session-search");
                }
                return self.update_inner(Message::Search(String::new()));
            }
            Message::Favorite(path) => {
                if let Some(p) = self.catalog.projects.iter_mut().find(|p| p.path == path) {
                    p.favorite = !p.favorite;
                }
                self.save_catalog(false);
            }
            Message::Questionnaire(id, message) => {
                if self.io_busy
                    || self.modal.is_some()
                    || self.context_menu.is_some()
                    || self.footer_menu.is_some()
                    || self.search_active
                    || self.active_questionnaire.as_ref() != Some(&id)
                {
                    return Task::none();
                }
                if let Some(draft) = self.questionnaires.get_mut(&id) {
                    let update = draft.update(message);
                    if let Some(answers) = update.answers {
                        let result = self
                            .selected
                            .as_ref()
                            .and_then(|session| self.harness.as_ref()?.get(session))
                            .and_then(|c| c.questionnaires.clone())
                            .ok_or_else(|| "This questionnaire is no longer active".to_owned())
                            .and_then(|q| q.submit(&id, answers).map_err(|e| format!("{e:#}")));
                        match result {
                            Ok(()) => self.sync_questionnaires(),
                            Err(error) => draft.error = Some(error),
                        }
                    }
                    return update
                        .task
                        .map(move |message| Message::Questionnaire(id.clone(), message));
                }
            }
            Message::Editor(_) | Message::DeletePreviousWord => {
                if self.active_questionnaire.is_some()
                    || self.io_busy
                    || self.closing
                    || self.close_requested
                    || self.modal.is_some()
                    || self.context_menu.is_some()
                {
                    return Task::none();
                }
                let edit = match message {
                    Message::Editor(action) => {
                        let edit = action.is_edit();
                        self.editor.perform(action);
                        edit
                    }
                    Message::DeletePreviousWord => {
                        questionnaire::delete_previous_word(&mut self.editor);
                        true
                    }
                    _ => unreachable!("editor message"),
                };
                if edit && self.draft_session.is_some() {
                    self.new_draft.message = self.editor.text();
                    self.new_draft_dirty = true;
                } else {
                    self.draft_dirty |= edit;
                }
            }
            Message::Send => {
                if self.active_questionnaire.is_some()
                    || self.modal.is_some()
                    || self.context_menu.is_some()
                {
                    return Task::none();
                }
                if let (Some(h), Some(session)) = (&self.harness, self.current()) {
                    let message = self.composer_message();
                    if message.is_empty() {
                        return Task::none();
                    }
                    if self.io_busy {
                        return Task::none();
                    }
                    let h = h.clone();
                    let id = session.id.clone();
                    let draft = self.draft_session.clone();
                    let settings = self.catalog.settings.clone();
                    self.io_busy = true;
                    let submission = self
                        .submissions
                        .entry(id.clone())
                        .or_insert_with(|| (message.clone(), pico_core::store::id()));
                    if submission.0 != message {
                        *submission = (message.clone(), pico_core::store::id());
                    }
                    let submission = submission.1.clone();
                    self.tasks.push(self.io.run(move || {
                        let result = if let Some(draft) = draft {
                            h.send_new(draft, &submission, message.clone(), settings)
                        } else {
                            h.send(&id, &submission, message.clone(), settings)
                        }.map_err(|e| format!("Could not send message: {e:#}. Your message is preserved; fix the problem and retry."));
                        Message::Sent(id, message, result)
                    }));
                }
            }
            Message::StopAgent(id) => {
                if let Some(h) = &self.harness {
                    h.stop(&id);
                }
            }
            Message::ToggleAgents => {
                self.agents_open = !self.agents_open;
                if self.settings_open {
                    return self.update_inner(Message::CancelSettings);
                }
                self.view_epoch = self.view_epoch.wrapping_add(1);
            }
            Message::ToggleSettings => {
                if self.settings_open {
                    return self.update_inner(Message::CancelSettings);
                }
                self.view_epoch = self.view_epoch.wrapping_add(1);
                self.settings_open = true;
                self.agents_open = false;
            }
            Message::ApplySettings => {
                let result = (|| {
                    self.settings.context_limit = self
                        .context_text
                        .parse()
                        .map_err(|_| "Enter a numeric context limit".to_string())?;
                    self.settings.compact_percent = self
                        .threshold_text
                        .parse()
                        .map_err(|_| "Enter a numeric compaction percentage".to_string())?;
                    self.settings.validate().map_err(|e| e.to_string())
                })();
                if let Err(e) = result {
                    self.error = Some(e);
                } else {
                    self.catalog.settings = self.settings.clone();
                    self.save_catalog(true);
                }
            }
            Message::CatalogSaved(close_settings, result) => match result {
                Ok(()) if close_settings && self.settings_open => {
                    self.settings_open = false;
                    self.view_epoch = self.view_epoch.wrapping_add(1);
                }
                Ok(()) => {}
                Err(error) => self.error = Some(error),
            },
            Message::Model(value) => self.settings.model = value,
            Message::SettingsTab(tab) => self.settings_tab = tab,
            Message::ModelEffort(id, effort) => {
                self.settings
                    .models
                    .get_mut(&id)
                    .expect("catalog model")
                    .effort = effort
            }
            Message::ModelFast(id, fast) => {
                self.settings
                    .models
                    .get_mut(&id)
                    .expect("catalog model")
                    .fast = fast
            }
            Message::NoticeDismiss => self.notice = None,
            Message::NoticeTick => {
                if self
                    .notice
                    .as_ref()
                    .is_some_and(|(_, since)| since.elapsed() >= NOTICE_DURATION)
                {
                    self.notice = None;
                }
            }
            Message::Context(value) => self.context_text = value,
            Message::Threshold(value) => self.threshold_text = value,
            Message::Font(value) => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                self.settings.font_size = value;
                self.catalog.settings.font_size = value;
                self.save_catalog(false);
            }
            Message::ChatWidth(value) => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                self.settings.chat_width = value;
                self.catalog.settings.chat_width = value;
                self.save_catalog(false);
            }
            Message::Resume(value) => {
                self.settings.resume_last = value;
                self.catalog.settings.resume_last = value;
                self.save_catalog(false);
            }
            Message::SessionModel(value) => {
                self.footer_menu = None;
                if self.io_busy {
                    return Task::none();
                }
                let settings = self.catalog.settings.clone();
                if let Some(draft) = &mut self.draft_session {
                    draft.select_model(&value, &settings);
                } else if !self.running()
                    && let (Some(h), Some(id)) = (&self.harness, &self.selected)
                {
                    let h = h.clone();
                    let id = id.clone();
                    self.tasks.push(self.io.run(move || {
                        Message::Edited(
                            h.edit(&id, |s| s.select_model(&value, &settings))
                                .map_err(|e| format!("{e:#}")),
                        )
                    }));
                }
            }
            Message::SessionEffort(value) => {
                self.footer_menu = None;
                if self.io_busy {
                    return Task::none();
                }
                if let Some(draft) = &mut self.draft_session {
                    draft.effort = value;
                } else if !self.running()
                    && let (Some(h), Some(id)) = (&self.harness, &self.selected)
                {
                    let h = h.clone();
                    let id = id.clone();
                    self.tasks.push(self.io.run(move || {
                        Message::Edited(
                            h.edit(&id, |s| s.effort = value)
                                .map_err(|e| format!("{e:#}")),
                        )
                    }));
                }
            }
            Message::ToggleSessionFast => {
                self.footer_menu = None;
                if self.io_busy || self.running() {
                    return Task::none();
                }
                if let Some(draft) = &mut self.draft_session {
                    draft.fast = !draft.fast;
                } else if let (Some(h), Some(id)) = (&self.harness, &self.selected) {
                    let h = h.clone();
                    let id = id.clone();
                    self.tasks.push(self.io.run(move || {
                        Message::Edited(
                            h.edit(&id, |s| s.fast = !s.fast)
                                .map_err(|e| format!("{e:#}")),
                        )
                    }));
                }
            }
            Message::AddProject => {
                self.modal_input = std::env::var("HOME").unwrap_or_else(|_| "/".into());
                self.modal = Some(Modal::AddProject);
                return widget::operation::focus("modal-input");
            }
            Message::Browse => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Add project to Pico V2")
                            .pick_folder()
                            .await
                            .map(|f| f.path().to_owned())
                    },
                    Message::Folder,
                );
            }
            Message::Folder(Some(path)) => self.modal_input = path.display().to_string(),
            Message::Folder(None) => {}
            Message::ModalInput(value) => self.modal_input = value,
            Message::Dismiss => self.modal = None,
            Message::Confirm => return self.confirm(),
            Message::Rename(id) => {
                self.context_menu = None;
                if let Some(s) = self.sessions.iter().find(|s| s.id == id) {
                    self.modal_input = s.title.clone();
                    self.modal = Some(Modal::Rename(id));
                    return widget::operation::focus("modal-input");
                }
            }
            Message::Delete(id) => {
                self.context_menu = None;
                self.modal = Some(Modal::Delete(id));
            }
            Message::ToggleToolRaw(id) => {
                if !self.raw_tool_inputs.remove(&id) {
                    self.raw_tool_inputs.insert(id.clone());
                }
                if let Some(view) = self.outputs.get_mut(&id) {
                    view.set_raw(self.raw_tool_inputs.contains(&id));
                }
                if let Some(index) = self.current().and_then(|s| s.entries.index_of(&id)) {
                    self.chat_selection.borrow_mut().invalidate_entry(index);
                }
                self.view_epoch = self.view_epoch.wrapping_add(1);
                return widget::operation::snap_to(
                    format!("tool-output-{id}"),
                    scrollable::RelativeOffset::START,
                );
            }
            Message::ToggleEntry(id) => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                if self.expanded.remove(&id) {
                    self.outputs.remove(&id);
                } else {
                    if let Some(Entry {
                        kind:
                            EntryKind::Tool {
                                name,
                                input,
                                output,
                                ..
                            },
                        ..
                    }) = self.current().and_then(|s| s.entries.by_id(&id))
                    {
                        self.outputs.insert(
                            id.clone(),
                            tool_output::Output::new(
                                name,
                                input,
                                output,
                                self.raw_tool_inputs.contains(&id),
                            ),
                        );
                    }
                    self.expanded.insert(id.clone());
                }
                if let Some(session) = &self.current_session
                    && let Some(index) = session.entries.index_of(&id)
                {
                    let head = self.activity.block(index).range.start;
                    self.groups.insert(session.entries[head].id.clone());
                }
                self.rebuild_transcript();
                self.prepare_visible();
            }
            Message::ToggleExecution(id) => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                if !self.execution.remove(&id) {
                    self.execution.insert(id);
                }
                self.rebuild_transcript();
                self.prepare_visible();
            }
            Message::ToggleGroup(id) => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                if !self.groups.remove(&id) {
                    self.groups.insert(id);
                }
                self.rebuild_transcript();
                self.prepare_visible();
            }
            Message::Scroll(epoch, offset, height, at_bottom) => {
                if epoch != self.view_epoch {
                    return Task::none();
                }
                self.auto_scroll = at_bottom;
                self.transcript.offset = (offset - 20.).max(0.);
                self.transcript.viewport = height;
                self.prepare_visible();
            }
            Message::RowSize(epoch, index, height) => {
                if epoch != self.view_epoch {
                    return Task::none();
                }
                let spacing = self
                    .current()
                    .map_or(8., |s| ActivityRow::from_id(index).spacing(&s.entries));
                let visible = self.transcript.visible();
                let corrected = self.transcript.measure(index, height + spacing);
                if self.auto_scroll {
                    self.transcript.end();
                    self.prepare_visible();
                    return bottom();
                }
                if corrected {
                    self.prepare_visible();
                    return widget::operation::scroll_to(
                        "chat",
                        scrollable::AbsoluteOffset {
                            x: 0.,
                            y: self.transcript.offset + 20.,
                        },
                    );
                }
                // A shrinking measured row can expose uncached rows even when
                // the scroll anchor stays put. Prepare exactly what we render.
                if self.transcript.visible() != visible {
                    self.prepare_visible();
                }
            }
            Message::SidebarScroll(epoch, offset, height) => {
                if epoch != self.sidebar_epoch {
                    return Task::none();
                }
                self.sidebar_follow_top = offset <= 1.;
                self.sidebar_list.offset = offset;
                self.sidebar_list.viewport = height;
            }
            Message::SidebarSize(epoch, index, height) => {
                if epoch != self.sidebar_epoch {
                    return Task::none();
                }
                let corrected = self.sidebar_list.measure(index, height + 5.);
                if self.sidebar_follow_top {
                    self.sidebar_list.offset = 0.;
                    return widget::operation::snap_to(
                        "sessions",
                        scrollable::RelativeOffset::START,
                    );
                }
                if corrected {
                    return widget::operation::scroll_to(
                        "sessions",
                        scrollable::AbsoluteOffset {
                            x: 0.,
                            y: self.sidebar_list.offset,
                        },
                    );
                }
            }

            Message::Link(url) => {
                self.modal = Some(Modal::Link(url));
            }
            Message::CopySelection => {
                if let Some(session) = self.current()
                    && let Some((start, end)) = self.chat_selection.borrow().range()
                {
                    let settings = markdown::Settings::with_text_size(
                        self.catalog.settings.font_size,
                        theme::markdown_style(),
                    );
                    let entries = self
                        .transcript
                        .order
                        .iter()
                        .filter_map(|&id| match ActivityRow::from_id(id) {
                            ActivityRow::Entry(i) => Some(i),
                            _ => None,
                        })
                        .filter(|i| (start.entry..=end.entry).contains(i))
                        .map(|index| {
                            let entry = &session.entries[index];
                            let blocks = match &entry.kind {
                                EntryKind::Tool {
                                    name,
                                    input,
                                    output,
                                    parent,
                                    ..
                                } if self.expanded.contains(&entry.id) => {
                                    vec![
                                        chat_selection::Block {
                                            text: tool_output::display(
                                                name,
                                                input,
                                                output,
                                                self.raw_tool_inputs.contains(&entry.id),
                                            ),
                                            separator: "\n\n",
                                        },
                                        chat_selection::Block {
                                            text: tool_reference(&entry.id, parent.as_deref()),
                                            separator: "\n\n",
                                        },
                                    ]
                                }
                                EntryKind::Tool { .. } => vec![],
                                EntryKind::Notice
                                    if activity::subagent_notice(entry).is_some()
                                        && !self.expanded.contains(&entry.id) =>
                                {
                                    vec![]
                                }
                                EntryKind::User { .. } | EntryKind::Notice => {
                                    vec![chat_selection::Block {
                                        text: entry.text.clone(),
                                        separator: "\n\n",
                                    }]
                                }
                                _ => {
                                    let content = markdown::Content::parse(&entry.text);
                                    let viewer = chat_selection::Viewer::new(
                                        self.chat_selection.clone(),
                                        index,
                                    );
                                    let _ = markdown::view_with(content.items(), settings, &viewer);
                                    viewer.blocks.into_inner()
                                }
                            };
                            (index, blocks)
                        });
                    let value = chat_selection::copy_blocks(&self.chat_selection.borrow(), entries);
                    return iced::clipboard::write(value);
                }
            }
            Message::Copy(value) => return iced::clipboard::write(value),
            Message::Login => {
                if self.login_cancel.is_none()
                    && let Some(h) = &self.harness
                {
                    let auth = h.auth.clone();
                    self.login_cancel = Some(CancellationToken::new());
                    return Task::perform(
                        async move {
                            auth.begin()
                                .await
                                .map(|login| {
                                    let url = login.url.clone();
                                    (Arc::new(tokio::sync::Mutex::new(Some(login))), url)
                                })
                                .map_err(|e| format!("{e:#}"))
                        },
                        Message::LoginStarted,
                    );
                }
            }
            Message::LoginStarted(result) => match result {
                Ok((login, url)) => {
                    let auth = self.harness.as_ref().expect("auth harness").auth.clone();
                    let cancel = self.login_cancel.as_ref().expect("pending login").clone();
                    self.login_url = Some(url.clone());
                    return Task::batch([
                        external(url),
                        Task::perform(
                            async move {
                                let login = login.lock().await.take().expect("one login consumer");
                                auth.finish(login, cancel)
                                    .await
                                    .map_err(|e| format!("{e:#}"))
                            },
                            Message::LoggedIn,
                        ),
                    ]);
                }
                Err(e) => {
                    self.login_cancel = None;
                    self.error = Some(e);
                }
            },
            Message::LoggedIn(result) => {
                self.login_cancel = None;
                self.login_url = None;
                match result {
                    Ok(account) => {
                        self.account = Some(account);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Message::Account(account) => self.account = account,
            Message::CancelLogin => {
                if let Some(cancel) = &self.login_cancel {
                    cancel.cancel();
                }
                self.login_url = None;
            }
            Message::Logout => self.modal = Some(Modal::Logout),
            Message::Git(path, info) => {
                self.git_pending.remove(&path);
                if let Some(info) = info {
                    self.git.insert(path, info);
                } else {
                    self.git.remove(&path);
                }
            }
            Message::External(result) => self.fail(result),
            Message::ErrorDismiss => self.error = None,
            Message::ResizeStart(right, position) => {
                self.drag = Some((
                    right,
                    position.x,
                    if right {
                        self.catalog.settings.settings_width
                    } else {
                        self.catalog.settings.sidebar_width
                    },
                ));
            }
            Message::Event(event) => match event {
                iced::Event::Window(window::Event::Focused) => self.window_focused = true,
                iced::Event::Window(window::Event::Unfocused) => self.window_focused = false,
                iced::Event::Window(window::Event::FileDropped(path)) => self.import_file(path),
                iced::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    if let Some((right, start, width)) = self.drag {
                        self.view_epoch = self.view_epoch.wrapping_add(1);
                        self.sidebar_epoch = self.sidebar_epoch.wrapping_add(1);
                        let delta = position.x - start;
                        let limit = (self.window_width * 0.4).min(if right { 700. } else { 600. });
                        let value = (width + if right { -delta } else { delta })
                            .clamp(if right { 260. } else { 160. }, limit);
                        if right {
                            self.catalog.settings.settings_width = value;
                            self.settings.settings_width = value;
                        } else {
                            self.catalog.settings.sidebar_width = value;
                            self.settings.sidebar_width = value;
                        }
                    }
                }
                iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    if self.drag.take().is_some() {
                        self.save_catalog(false);
                    }
                }
                iced::Event::Window(window::Event::Resized(size)) => {
                    self.view_epoch = self.view_epoch.wrapping_add(1);
                    self.sidebar_epoch = self.sidebar_epoch.wrapping_add(1);
                    self.window_width = size.width;
                    self.window_height = size.height;
                }
                iced::Event::Window(window::Event::CloseRequested) => {
                    if self
                        .harness
                        .as_ref()
                        .is_some_and(|h| h.all().iter().any(|c| c.is_executing()))
                    {
                        self.modal = Some(Modal::Quit);
                    } else {
                        return self.quit();
                    }
                }
                iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    if let Some(menu) = &mut self.footer_menu {
                        use keyboard::key::Named;
                        match key.as_ref() {
                            keyboard::Key::Named(Named::Escape) => self.footer_menu = None,
                            keyboard::Key::Named(Named::ArrowUp) => {
                                menu.focused =
                                    (menu.focused + menu.options.len() - 1) % menu.options.len();
                            }
                            keyboard::Key::Named(Named::ArrowDown) => {
                                menu.focused = (menu.focused + 1) % menu.options.len();
                            }
                            keyboard::Key::Named(Named::Enter | Named::Tab) => {
                                let value = menu.options[menu.focused].clone();
                                let message = match menu.control {
                                    FooterControl::Model => Message::SessionModel(value),
                                    FooterControl::Effort if value == FAST_OPTION => {
                                        Message::ToggleSessionFast
                                    }
                                    FooterControl::Effort => Message::SessionEffort(value),
                                };
                                return self.update_inner(message);
                            }
                            _ => {}
                        }
                        return Task::none();
                    }
                    if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                        if self.context_menu.take().is_some() {
                        } else if self.modal.is_some() {
                            self.modal = None;
                        } else if self.search_active {
                            return self.update_inner(Message::ToggleSearch);
                        } else if self.settings_open {
                            return self.update_inner(Message::CancelSettings);
                        } else if self.agents_open {
                            self.agents_open = false;
                            self.view_epoch = self.view_epoch.wrapping_add(1);
                        } else {
                            if let (Some(h), Some(id)) = (&self.harness, &self.selected)
                                && (h.get(id).is_some_and(|s| s.is_executing())
                                    || h.active_children(id) > 0)
                            {
                                h.stop(id);
                            }
                            self.acknowledge_selected_attention();
                        }
                    } else if self.modal.is_none()
                        && self.context_menu.is_none()
                        && modifiers.control()
                        && key.as_ref() == keyboard::Key::Character("n")
                    {
                        self.select(Selection::Picker);
                    }
                }
                _ => {}
            },
            Message::WindowOpened(id) => {
                self.window_id = Some(id);
                if self.smoke {
                    return delayed_capture();
                }
            }
            Message::Capture => {
                if self.io_busy {
                    return delayed_capture();
                }
                if let Some(id) = self.window_id {
                    return window::screenshot(id).map(Message::Screenshot);
                }
            }
            Message::Screenshot(screenshot) => return self.smoke_frame(screenshot),
        }
        Task::none()
    }
    fn confirm(&mut self) -> Task<Message> {
        if self.io_busy || self.closing {
            return Task::none();
        }
        match self.modal.clone() {
            Some(Modal::AddProject) => match std::fs::canonicalize(self.modal_input.trim()) {
                Ok(path) if path.is_dir() => {
                    if !self.catalog.projects.iter().any(|p| p.path == path) {
                        self.catalog.projects.push(Project {
                            path,
                            favorite: false,
                        });
                        self.save_catalog(false);
                    }
                    self.modal = None;
                }
                Ok(_) => self.error = Some("Select a directory, not a file.".into()),
                Err(e) => {
                    self.error = Some(format!(
                        "Cannot open that project: {e}. Check the path or choose Browse."
                    ))
                }
            },
            Some(Modal::Rename(id)) => {
                let title = self.modal_input.trim().to_owned();
                if !title.is_empty()
                    && let Some(h) = &self.harness
                {
                    let h = h.clone();
                    self.tasks.push(self.io.run(move || {
                        Message::Edited(
                            h.edit(&id, |s| s.title = title)
                                .map_err(|e| format!("{e:#}")),
                        )
                    }));
                    self.modal = None;
                }
            }
            Some(Modal::Delete(id)) => {
                if let Some(h) = &self.harness {
                    let h = h.clone();
                    self.io_busy = true;
                    self.tasks.push(self.io.run(move || {
                        let result = h.remove(&id).map_err(|e| format!("{e:#}"));
                        Message::Deleted(id, result)
                    }));
                }
            }
            Some(Modal::Logout) => {
                if self
                    .harness
                    .as_ref()
                    .is_some_and(|h| h.all().iter().any(|s| s.is_executing()))
                {
                    self.error = Some("Stop running turns before signing out.".into());
                } else if let Some(h) = &self.harness {
                    let auth = h.auth.clone();
                    self.modal = None;
                    return Task::perform(
                        async move { auth.logout().await.map_err(|e| e.to_string()) },
                        |result| match result {
                            Ok(()) => Message::Account(None),
                            Err(e) => Message::External(Err(e)),
                        },
                    );
                }
            }
            Some(Modal::Image(_) | Modal::MarkdownImage(_)) => {}
            Some(Modal::Quit) => return self.quit(),
            Some(Modal::Link(url)) => {
                self.modal = None;
                if url.starts_with("https://") || url.starts_with("http://") {
                    return external(url);
                }
                self.error = Some("Only http and https links can be opened from chat.".into());
            }
            None => {}
        }
        Task::none()
    }
    fn quit(&mut self) -> Task<Message> {
        if self.closing {
            return Task::none();
        }
        if self.io_busy {
            self.close_requested = true;
            return Task::none();
        }
        self.closing = true;
        let draft = self.take_draft();
        let new_draft = self.new_draft.clone();
        let mut catalog = self.catalog.clone();
        catalog.selected = self.selected.clone();
        if let Some(cancel) = &self.login_cancel {
            cancel.cancel();
        }
        if let Some(h) = &self.harness {
            let h = h.clone();
            return self.io.run(move || {
                // Ordered after earlier jobs: no queued Send can start behind this barrier.
                h.stop_all();
                let result=(|| {
                    while h.all().iter().any(|s|s.is_executing()) {std::thread::sleep(Duration::from_millis(20));}
                    if let Some((id,draft))=draft {h.edit(&id,|s| { s.draft=draft.text; s.draft_images=draft.images; })?;}
                    h.store.save_new_draft(&new_draft)?;
                    h.store.save_catalog(&catalog)?;
                    h.flush()?;
                    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(h.auth.flush())
                })();
                Message::Exit(result.map_err(|e|format!("Could not finish saving: {e:#}. Keep Pico open, fix storage, then retry closing.")))
            });
        }
        iced::exit()
    }
    pub fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            window_events(),
            window::open_events().map(Message::WindowOpened),
            iced::time::every(Duration::from_secs(60)).map(|_| Message::AgeTick),
        ];
        if let Some(harness) = &self.harness {
            subscriptions.push(Subscription::run_with(Updates(harness.clone()), updates));
            // Waves reuse this clock, including Working rows whose own turn is
            // no longer running (terminal persistence or active children).
            if harness.all().iter().any(|c| c.is_running())
                || self
                    .sidebar_rows
                    .iter()
                    .any(|row| matches!(row, SidebarRow::Session(_, SessionStatus::Working)))
            {
                subscriptions.push(iced::time::every(ACTIVITY_INTERVAL).map(|_| Message::Tick));
            }
        }
        if self.current().is_some() {
            subscriptions.push(iced::time::every(Duration::from_secs(5)).map(|_| Message::GitTick));
        }
        if self.notice.is_some() {
            subscriptions
                .push(iced::time::every(Duration::from_secs(1)).map(|_| Message::NoticeTick));
        }
        if self.draft_dirty || self.new_draft_dirty {
            subscriptions
                .push(iced::time::every(Duration::from_secs(1)).map(|_| Message::SaveDraft));
        }
        if !self.smoke {
            subscriptions.push(self.notifications.subscription().map(Message::Notification));
        }
        subscriptions.push(self.image_view.subscription().map(Message::Image));
        Subscription::batch(subscriptions)
    }
    pub fn view(&self) -> Element<'_, Message> {
        pointer_input(
            self.content(),
            self.footer_menu.is_some(),
            self.chat_selection.clone(),
        )
    }
    fn content(&self) -> Element<'_, Message> {
        let mut center = column![self.center()].width(Fill).height(Fill);
        if let Some((notice, _)) = &self.notice {
            center = center.push(
                container(
                    row![
                        text(format!("Info · {notice}")).size(13).width(Fill),
                        theme::button("×")
                            .on_press(Message::NoticeDismiss)
                            .style(theme::text_button)
                    ]
                    .spacing(12)
                    .align_y(iced::Center),
                )
                .padding(12)
                .style(card),
            );
        }
        if let Some(error) = &self.error {
            center = center.push(
                container(
                    row![
                        text(error).size(13).width(Fill),
                        theme::button("Dismiss").on_press(Message::ErrorDismiss)
                    ]
                    .spacing(12),
                )
                .padding(12)
                .style(|_| container::Style {
                    background: Some(theme::ERROR_SURFACE.into()),
                    ..Default::default()
                }),
            );
        }
        let mut body = row![
            self.sidebar(),
            self.divider(false),
            center.push(self.footer())
        ]
        .height(Fill);
        if self.settings_open {
            body = body.push(self.divider(true)).push(self.settings_panel());
        } else if self.agents_open {
            body = body.push(self.divider(true)).push(self.agents_panel());
        }
        let rail = container(
            column![
                space().height(Fill),
                tooltip(
                    theme::button(
                        container(crate::agent_icon::agent_icon(self.agent_count() > 0))
                            .center_x(Fill)
                            .center_y(34)
                    )
                    .width(Fill)
                    .padding(0)
                    .on_press(Message::ToggleAgents)
                    .style(theme::text_button),
                    text("Agents").size(12),
                    tooltip::Position::Left,
                )
                .style(card),
                theme::button(container(text("⚙").size(24)).center_x(Fill).center_y(34))
                    .width(Fill)
                    .padding(0)
                    .on_press(Message::ToggleSettings)
                    .style(theme::text_button)
            ]
            .spacing(12)
            .width(Fill),
        )
        .padding([14, 8])
        .height(Fill)
        .width(50)
        .style(panel);
        // Keep the main UI at the same tree position when popups open/close,
        // otherwise iced discards its scroll offsets and other widget state.
        let layers = stack![body.push(rail)];
        if let Some(modal) = &self.modal {
            let shade = mouse_area(container(space().width(Fill).height(Fill)).style(|_| {
                container::Style {
                    background: Some(theme::SCRIM.into()),
                    ..Default::default()
                }
            }))
            .on_press(Message::Dismiss);
            layers
                .push(shade)
                .push(
                    container(opaque(self.modal_view(modal)))
                        .center_x(Fill)
                        .center_y(Fill),
                )
                .into()
        } else if let Some((id, position)) = &self.context_menu {
            let dismiss = mouse_area(space().width(Fill).height(Fill))
                .on_press(Message::CloseMenu)
                .on_right_press(Message::CloseMenu)
                .on_scroll(|_| Message::CloseMenu);
            let menu = container(
                column![
                    theme::button("Rename")
                        .width(Fill)
                        .on_press(Message::Rename(id.clone()))
                        .style(theme::text_button),
                    theme::button("Copy session ID")
                        .width(Fill)
                        .on_press(Message::CopySessionId(id.clone()))
                        .style(theme::text_button),
                    theme::button("Delete")
                        .width(Fill)
                        .on_press(Message::Delete(id.clone()))
                        .style(theme::text_button)
                ]
                .spacing(4),
            )
            .padding(6)
            .width(170)
            .style(card);
            let x = position.x.min(self.window_width - 180.).max(0.);
            let y = position.y.min(self.window_height - 140.).max(0.);
            let positioned = column![space().height(y), row![space().width(x), opaque(menu)]];
            layers.push(dismiss).push(positioned).into()
        } else if let Some(menu) = &self.footer_menu {
            let dismiss = mouse_area(space().width(Fill).height(Fill))
                .on_press(Message::CloseFooterMenu)
                .on_right_press(Message::CloseFooterMenu)
                .on_scroll(|_| Message::CloseFooterMenu);
            let right = 50.
                + if self.settings_open || self.agents_open {
                    self.catalog
                        .settings
                        .settings_width
                        .min(self.window_width * 0.4)
                        + 5.
                } else {
                    0.
                };
            // Anchor at the chat's right edge, but let a popup extend over the
            // left sidebar when the center is too narrow to read its choices.
            let positioned = row![
                container(column![space().height(Fill), self.footer_popup(menu)])
                    .align_right(Fill)
                    .padding(iced::Padding {
                        top: 0.,
                        right: 14.,
                        bottom: 42.,
                        left: 14.
                    }),
                space().width(right),
            ]
            .height(Fill);
            layers.push(dismiss).push(positioned).into()
        } else {
            layers.into()
        }
    }
    fn divider(&self, right: bool) -> Element<'_, Message> {
        mouse_area(
            container(space().width(5).height(Fill)).style(|_| container::Style {
                background: Some(BORDER.into()),
                ..Default::default()
            }),
        )
        .on_press(Message::Press(Press::Resize(right)))
        .interaction(mouse::Interaction::ResizingHorizontally)
        .into()
    }

    fn sidebar_header(&self) -> Element<'_, Message> {
        let icon: Element<_> = if self.search_active {
            text("×").size(14).into()
        } else {
            crate::search_icon::search_icon()
        };
        let control = theme::button(container(icon).center_x(18).center_y(24))
            .padding(0)
            .width(18)
            .height(24)
            .on_press(Message::ToggleSearch)
            .style(sidebar_control);
        let center: Element<_> = if self.search_active {
            text_input("Search sessions…", &self.search)
                .id("session-search")
                .on_input(Message::Search)
                .size(13)
                .padding([0, 4])
                .style(theme::search_input)
                .into()
        } else {
            let badges = row(self
                .catalog
                .projects
                .iter()
                .filter(|p| p.favorite)
                .map(|p| {
                    theme::button(project_badge(&p.path))
                        .padding(0)
                        .on_press(Message::Create(p.path.clone()))
                        .style(theme::text_button)
                        .into()
                }))
            .spacing(6)
            .align_y(iced::Center);
            container(
                crate::scrollbar::horizontal(badges)
                    .id("favorite-projects")
                    .direction(scrollable::Direction::Horizontal(
                        scrollable::Scrollbar::hidden(),
                    ))
                    .height(18),
            )
            .center_x(Fill)
            .into()
        };
        row![
            tooltip(
                control,
                text(if self.search_active {
                    "Close search (Esc)"
                } else {
                    "Search sessions"
                })
                .size(12),
                tooltip::Position::Bottom,
            )
            .style(card),
            center,
            tooltip(
                theme::button(container(text("+").size(16)).center_x(18).center_y(24))
                    .padding(0)
                    .width(18)
                    .height(24)
                    .on_press(Message::New)
                    .style(sidebar_control),
                text("New session").size(12),
                tooltip::Position::Bottom,
            )
            .style(card),
        ]
        .spacing(6)
        .align_y(iced::Center)
        .height(24)
        .into()
    }

    fn parent_id(&self) -> Option<&str> {
        self.current().map(|s| {
            s.child
                .as_ref()
                .map_or(s.id.as_str(), |c| c.parent_id.as_str())
        })
    }
    fn agent_count(&self) -> usize {
        self.parent_id()
            .and_then(|id| self.harness.as_ref().map(|h| h.active_children(id)))
            .unwrap_or(0)
    }
    fn agents_panel(&self) -> Element<'_, Message> {
        let mut content = column![
            container(right_panel_header("Agents", Message::ToggleAgents))
                .padding(iced::Padding::ZERO.right(crate::scrollbar::GUTTER)),
            text(format!("{} running", self.agent_count()))
                .size(12)
                .color(MUTED)
        ]
        .spacing(14);
        let runs = self
            .parent_id()
            .and_then(|id| self.harness.as_ref().map(|h| h.subagent_runs(id)))
            .unwrap_or_default();
        let mut cards = column![].spacing(10);
        for run in runs {
            let elapsed = run
                .finished
                .unwrap_or_else(pico_core::store::now)
                .saturating_sub(run.started);
            let activity = self
                .harness
                .as_ref()
                .and_then(|h| h.get(&run.session_id))
                .filter(|_| run.status == pico_core::agent::subagents::RunStatus::Running)
                .map(|c| c.status.lock().expect("status lock").clone());
            let status_color = if run.status.needs_attention() {
                DANGER
            } else if activity.is_some() {
                ACCENT
            } else {
                MUTED
            };
            let status = activity.as_deref().unwrap_or(run.status.label());
            let card_content = column![
                row![
                    text(run.profile.label()).size(13),
                    container(crate::ellipsized_text::shrinking_ellipsized_text(
                        status,
                        11.,
                        Some(status_color)
                    ))
                    .align_right(Fill),
                ]
                .spacing(10)
                .align_y(iced::Center),
                row![
                    ellipsized_text(&run.title, 13., None),
                    text(format!(
                        "{} · {} · {elapsed}s",
                        run.profile.model(),
                        run.profile.effort()
                    ))
                    .size(10)
                    .color(MUTED)
                    .wrapping(text::Wrapping::None),
                ]
                .spacing(10)
                .align_y(iced::Center),
            ]
            .spacing(7);
            cards = cards.push(
                theme::button(card_content)
                    .on_press(Message::Select(run.session_id))
                    .padding(12)
                    .width(Fill)
                    .style(theme::card_button),
            );
        }
        content = content.push(scroll(cards).height(Fill));
        container(content)
            .padding(iced::Padding::new(RIGHT_PANEL_PADDING).right(theme::SCROLLBAR_EDGE_INSET))
            .width(
                self.catalog
                    .settings
                    .settings_width
                    .min(self.window_width * 0.4),
            )
            .height(Fill)
            .style(right_panel)
            .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let mut list = column![
            container(self.sidebar_header()).padding(
                iced::Padding::ZERO
                    .left(SIDEBAR_CONTENT_PADDING)
                    .right(SIDEBAR_CONTENT_PADDING + 8. - theme::SCROLLBAR_EDGE_INSET),
            )
        ]
        .spacing(8);
        let epoch = self.sidebar_epoch;
        let range = self.sidebar_list.visible();
        let mut rows: Vec<(usize, bool, Element<'_, Message>)> = vec![(
            usize::MAX,
            false,
            space().height(self.sidebar_list.prefix(range.start)).into(),
        )];
        for index in range.clone() {
            let content: Element<'_, Message> = match &self.sidebar_rows[index] {
                SidebarRow::Heading(title, count) => {
                    container(text(format!("{title}  {count}")).size(10).color(MUTED))
                        .padding([10., SIDEBAR_CONTENT_PADDING])
                        .into()
                }
                SidebarRow::Session(session_index, status) => {
                    let session = &self.sessions[*session_index];
                    let active = self.parent_id() == Some(session.id.as_str());
                    let project = session
                        .project
                        .file_name()
                        .unwrap_or(session.project.as_os_str())
                        .to_string_lossy();
                    let badge = project_badge(&session.project);
                    // `updated` changes on accepted user submissions, not agent activity.
                    let children = self
                        .harness
                        .as_ref()
                        .map_or(0, |h| h.active_children(&session.id));
                    let attention = self.harness.as_ref().is_some_and(|h| {
                        h.children_need_attention(&session.id)
                            || h.get(&session.id).is_some_and(|c| {
                                c.questionnaires.as_ref().is_some_and(|q| q.has_pending())
                            })
                    });
                    let age = if children > 0 {
                        format!("● {children} · {}", age(session.updated))
                    } else if attention {
                        format!("! · {}", age(session.updated))
                    } else {
                        age(session.updated)
                    };
                    let status = *status;
                    let trailing: Element<'_, Message> = if status == SessionStatus::Unchecked {
                        container(container(space().width(8).height(8)).style(|_| {
                            container::Style::default()
                                .background(theme::ACCENT)
                                .border(iced::Border {
                                    radius: 4.into(),
                                    ..Default::default()
                                })
                        }))
                        .width(12)
                        .center_y(14)
                        .into()
                    } else {
                        text(age).size(10).color(MUTED).into()
                    };
                    let content = mouse_area(
                        theme::button(session_wave::wave(
                            container(
                                column![
                                    ellipsized_text(&session.title, 13., None),
                                    row![
                                        badge,
                                        ellipsized_text(&project, 11., Some(MUTED)),
                                        trailing
                                    ]
                                    .spacing(6)
                                    .align_y(iced::Center)
                                ]
                                .spacing(3),
                            )
                            .padding([6., SIDEBAR_CONTENT_PADDING])
                            .width(Fill),
                            status,
                        ))
                        .padding(0)
                        .width(Fill)
                        .on_press(Message::Select(session.id.clone()))
                        .style(if active {
                            secondary
                        } else {
                            theme::text_button
                        }),
                    )
                    .on_right_press(Message::Press(Press::SessionMenu(session.id.clone())));
                    content.into()
                }
            };
            let key = self.sidebar_list.order[index];
            // Measurements need a fresh epoch; animation/widget identity must
            // follow the session across filtering and section changes instead.
            let sensor = widget::sensor(content)
                .key((epoch, key))
                .on_show(move |size| Message::SidebarSize(epoch, key, size.height))
                .on_resize(move |size| Message::SidebarSize(epoch, key, size.height));
            rows.push((
                key,
                matches!(self.sidebar_rows[index], SidebarRow::Session(..)),
                column![sensor, space().height(5.)].into(),
            ));
        }
        rows.push((
            usize::MAX - 1,
            false,
            space()
                .height((self.sidebar_list.total() - self.sidebar_list.prefix(range.end)).max(0.))
                .into(),
        ));
        if self.sessions.is_empty() {
            rows.push((
                usize::MAX - 2,
                false,
                container(
                    text("Your sessions will appear here.")
                        .size(13)
                        .color(MUTED),
                )
                .padding([0., SIDEBAR_CONTENT_PADDING])
                .into(),
            ));
        }
        let rows = crate::keyed_column::animated_column(rows);
        list = list.push(
            scroll(rows)
                .id("sessions")
                .height(Fill)
                .on_scroll(move |v| {
                    Message::SidebarScroll(epoch, v.absolute_offset().y, v.bounds().height)
                }),
        );
        container(list)
            .padding(iced::Padding {
                top: 4.,
                right: theme::SCROLLBAR_EDGE_INSET,
                bottom: 14.,
                left: 8.,
            })
            .width(
                self.catalog
                    .settings
                    .sidebar_width
                    .min(self.window_width * 0.4),
            )
            .height(Fill)
            .style(panel)
            .into()
    }
    fn center(&self) -> Element<'_, Message> {
        let Some(session) = self.current() else {
            return self.new_session();
        };
        let epoch = self.view_epoch;
        let range = self.transcript.visible();
        let mut children: Vec<(TranscriptKey, Element<'_, Message>)> = vec![(
            TranscriptKey::Top,
            space().height(self.transcript.prefix(range.start)).into(),
        )];
        if session.entries.is_empty() {
            children.push((
                TranscriptKey::Empty,
                container(
                    column![
                        text("What are we building?").size(28),
                        text("Ask Pico to explore, implement, debug, or test this project.")
                            .color(MUTED),
                        text("Commands run with your user permissions, without approval prompts.")
                            .size(12)
                    ]
                    .spacing(14),
                )
                .padding([40, 0])
                .into(),
            ));
        }
        for &index in &self.transcript.order[range.clone()] {
            let projected = ActivityRow::from_id(index);
            let entry = &session.entries[projected.source()];
            // Resize-only sensors measure every mounted row, even outside the
            // viewport. Adding on_show would gate measurements on visibility,
            // mixing real scroll offsets with stale height estimates.
            let row = widget::sensor(self.activity_row(projected))
                .on_resize(move |size| Message::RowSize(epoch, index, size.height));
            children.push((
                TranscriptKey::Entry {
                    session: session.id.clone(),
                    entry: entry.id.clone(),
                    role: index - projected.source(),
                },
                column![row, space().height(projected.spacing(&session.entries))].into(),
            ));
        }
        children.push((
            TranscriptKey::Bottom,
            space()
                .height((self.transcript.total() - self.transcript.prefix(range.end)).max(0.))
                .into(),
        ));
        let transcript = crate::keyed_column::keyed_column(children);
        // The shared gutter replaces part of the existing transcript padding;
        // keep the text width and centered alignment unchanged.
        let transcript = container(transcript)
            .max_width(self.catalog.settings.chat_width - crate::scrollbar::GUTTER)
            .width(Fill)
            .padding(iced::Padding {
                top: 20.,
                bottom: 20.,
                left: 26.,
                right: 26. - crate::scrollbar::GUTTER,
            });
        let content = crate::transcript_viewport::observe(
            container(transcript).center_x(Fill),
            epoch,
            move |offset, height, content_height| {
                Message::Scroll(
                    epoch,
                    offset,
                    height,
                    offset + height >= content_height - 40.,
                )
            },
        );
        let chat = scroll(content).id("chat").height(Fill);
        let chat = container(chat).padding(iced::Padding::ZERO.right(6));
        let running = self.running();
        if let Some(child) = &session.child {
            use pico_core::agent::subagents::RunStatus;
            let status = self
                .harness
                .as_ref()
                .and_then(|h| {
                    h.subagent_runs(&child.parent_id)
                        .into_iter()
                        .find(|r| r.session_id == session.id)
                })
                .map(|r| r.status);
            let status_color = match status {
                Some(RunStatus::Running) => ACCENT,
                Some(RunStatus::Completed) => SUCCESS,
                Some(RunStatus::Failed | RunStatus::Interrupted) => DANGER,
                Some(RunStatus::Cancelled) => WARNING,
                None => MUTED,
            };
            let header = widget::responsive(move |size| {
                let compact = size.width < 640.;
                let mut back = row![crate::chevron_icon::back_arrow_icon(theme::TEXT)]
                    .spacing(8)
                    .align_y(iced::Center);
                if !compact {
                    back = back.push(text("Back to main agent").size(12));
                }
                let back = theme::button(container(back).center_y(32))
                    .padding([0, 8])
                    .on_press(Message::Select(child.parent_id.clone()))
                    .style(theme::text_button);
                let title: Element<'_, Message> = if compact {
                    space().width(Fill).into()
                } else {
                    let title = format!("{} · {}", child.profile.label(), session.title);
                    container(crate::ellipsized_text::shrinking_ellipsized_text(
                        &title, 13., None,
                    ))
                    .center_x(Fill)
                    .into()
                };
                let status = tooltip(
                    container(text("●").size(12).color(status_color))
                        .center_x(24)
                        .center_y(32),
                    text(status.map_or("Ready", RunStatus::label)).size(12),
                    tooltip::Position::Bottom,
                )
                .style(card);
                let stop = tooltip(
                    theme::button(container(text("■").size(16)).center_x(32).center_y(32))
                        .padding(0)
                        .on_press_maybe(running.then(|| Message::StopAgent(session.id.clone())))
                        .style(theme::text_button),
                    text("Stop").size(12),
                    tooltip::Position::Bottom,
                )
                .style(card);
                // Equal side widths keep the title at the actual header center.
                let side_width = if compact { 60. } else { 170. };
                row![
                    container(back).width(side_width),
                    title,
                    container(row![status, stop].spacing(4).align_y(iced::Center))
                        .align_right(side_width),
                ]
                .spacing(8)
                .align_y(iced::Center)
                .into()
            })
            .height(32);
            return column![container(header).padding(12).style(panel), chat]
                .width(Fill)
                .height(Fill)
                .into();
        }
        if let Some(id) = &self.active_questionnaire
            && let Some(draft) = self.questionnaires.get(id)
        {
            let id = id.clone();
            let panel = draft
                .view(
                    self.catalog.settings.font_size,
                    self.window_height * 0.65,
                    !self.io_busy
                        && !self.closing
                        && self.modal.is_none()
                        && self.context_menu.is_none()
                        && self.footer_menu.is_none()
                        && !self.search_active,
                    !self.settings_open
                        && !self.search_active
                        && self.modal.is_none()
                        && self.footer_menu.is_none()
                        && self.context_menu.is_none(),
                )
                .map(move |message| Message::Questionnaire(id.clone(), message));
            return column![
                chat,
                container(
                    container(panel)
                        .max_width(self.catalog.settings.chat_width)
                        .width(Fill)
                )
                .center_x(Fill)
                .padding([14, 24])
            ]
            .width(Fill)
            .height(Fill)
            .into();
        }
        let mut compose = column![].spacing(10);
        if !self.attachments.is_empty() {
            let images = row(self
                .attachments
                .iter()
                .enumerate()
                .map(|(index, media)| self.image_view.attachment(media, index)))
            .spacing(8);
            compose = compose.push(crate::scrollbar::horizontal(images));
        }
        compose = compose.push(self.composer_editor(running));
        if !session.pending.is_empty() && running {
            compose = compose.push(
                text(format!(
                    "{} steering message(s) waiting for the next boundary",
                    session.pending.len()
                ))
                .size(11)
                .color(ACCENT),
            );
        }
        column![
            chat,
            container(
                container(compose)
                    .max_width(self.catalog.settings.chat_width)
                    .width(Fill)
            )
            .center_x(Fill)
            .padding([14, 24])
        ]
        .width(Fill)
        .height(Fill)
        .into()
    }
    fn composer_editor(&self, running: bool) -> Element<'_, Message> {
        let line_height = self.catalog.settings.font_size * COMPOSER_LINE_HEIGHT_FACTOR;
        let editor = text_editor(&self.editor)
            .style(theme::editor)
            .placeholder(if self.io_busy || self.closing {
                "Saving…"
            } else if running {
                "Steer the agent after its current tool batch…"
            } else {
                "Message Pico…"
            })
            .on_action(Message::Editor)
            .key_binding(composer_key_binding)
            .line_height(iced::widget::text::LineHeight::Absolute(line_height.into()))
            .min_height(COMPOSER_MIN_LINES as f32 * line_height)
            .size(self.catalog.settings.font_size);
        crate::growing_editor::growing_editor(
            editor,
            &self.editor,
            COMPOSER_MAX_LINES as f32 * line_height + 2. * crate::growing_editor::PADDING,
            Message::Editor,
            |message| match message {
                Message::Editor(text_editor::Action::Scroll { lines }) => Some(*lines),
                _ => None,
            },
        )
    }

    fn activity_row(&self, projected: ActivityRow) -> Element<'_, Message> {
        let session = self.current().expect("visible transcript");
        let index = projected.source();
        let entry = &session.entries[index];
        match projected {
            ActivityRow::Entry(index) => self.entry_view(entry, index),
            ActivityRow::Activity(_) => {
                let block = self.activity.block(index);
                let summary = block.summary.as_ref().expect("activity summary");
                let active = block.range.end == session.entries.len() && self.running();
                let failed_notices = block
                    .notices
                    .iter()
                    .filter(|&&i| {
                        self.subagent_notice_run(&session.entries[i])
                            .is_some_and(|r| r.status.needs_attention())
                    })
                    .count();
                let failures = summary.failed + summary.code_failures + failed_notices;
                let working = active || summary.running > 0;
                let mut header = row![space().width(12).height(12)]
                    .spacing(8)
                    .align_y(iced::Center);
                header = header.push(ellipsized_text(&summary.label(active), 12., Some(MUTED)));
                if working && summary.tools > 0 {
                    let progress = if summary.running > 0 {
                        format!(
                            "{} completed · {} running",
                            summary.completed, summary.running
                        )
                    } else {
                        format!("{} completed", summary.completed)
                    };
                    header = header.push(text(progress).size(11).color(MUTED));
                }
                if failures > 0 {
                    header = header.push(text(format!("{failures} failed")).size(12).color(DANGER));
                }
                theme::button(stack![
                    container(header).padding([6, 2]).width(Fill),
                    crate::chevron_icon::activity_disclosure(
                        self.groups.contains(&entry.id),
                        working,
                        self.tick,
                    ),
                ])
                .padding(0)
                .width(Fill)
                .style(theme::text_button)
                .on_press(Message::ToggleGroup(entry.id.clone()))
                .into()
            }
            ActivityRow::Execution(_) => {
                let wrappers = self.activity.block(index).wrappers;
                container(
                    theme::button(
                        row![
                            chevron_icon(self.execution.contains(&entry.id), MUTED),
                            text(format!(
                                "Execution details · {wrappers} code-mode call{}",
                                if wrappers == 1 { "" } else { "s" }
                            ))
                            .size(11)
                            .color(MUTED)
                        ]
                        .spacing(8)
                        .align_y(iced::Center),
                    )
                    .padding([4, 2])
                    .style(theme::text_button)
                    .on_press(Message::ToggleExecution(entry.id.clone())),
                )
                .padding([0, 12])
                .into()
            }
            ActivityRow::Thinking(_) => {
                let range = &self.activity.block(index).thinking[&index];
                let preview = range
                    .clone()
                    .rev()
                    .map(|i| activity::reasoning_preview(&session.entries[i].text))
                    .find(|s| !s.is_empty())
                    .unwrap_or_else(|| "Thinking".into());
                container(
                    theme::button(
                        row![
                            chevron_icon(self.expanded.contains(&entry.id), MUTED),
                            ellipsized_text(&preview, 12., Some(MUTED))
                        ]
                        .spacing(8)
                        .align_y(iced::Center),
                    )
                    .padding([4, 2])
                    .width(Fill)
                    .style(theme::text_button)
                    .on_press(Message::ToggleEntry(entry.id.clone())),
                )
                .padding([0, 12])
                .width(Fill)
                .into()
            }
        }
    }
    fn subagent_notice_run(&self, entry: &Entry) -> Option<pico_core::agent::subagents::RunInfo> {
        self.harness
            .as_ref()?
            .subagent_run(&self.current()?.id, activity::subagent_notice(entry)?)
    }
    fn subagent_notice_view<'a>(&'a self, entry: &'a Entry, index: usize) -> Element<'a, Message> {
        let open = self.expanded.contains(&entry.id);
        let run = self.subagent_notice_run(entry);
        let (label, color) = match &run {
            Some(run) => (
                format!(
                    "{} · {} · {}",
                    run.profile.label(),
                    run.title,
                    run.status.label()
                ),
                if run.status.needs_attention() {
                    DANGER
                } else if run.status == pico_core::agent::subagents::RunStatus::Completed {
                    SUCCESS
                } else {
                    MUTED
                },
            ),
            None => ("Subagent update".into(), MUTED),
        };
        let mut block = column![
            theme::button(
                row![
                    chevron_icon(open, MUTED),
                    text("●").size(11).color(color),
                    ellipsized_text(&label, 12., Some(MUTED))
                ]
                .spacing(8)
                .align_y(iced::Center)
            )
            .on_press(Message::ToggleEntry(entry.id.clone()))
            .padding([4, 2])
            .style(theme::text_button)
            .width(Fill)
        ]
        .spacing(6);
        if open {
            if let Some(run) = run {
                block = block.push(
                    theme::button("Open subagent ↗")
                        .style(theme::text_button)
                        .on_press(Message::Select(run.session_id)),
                );
            }
            block = block.push(chat_selection::plain(
                &entry.text,
                12.,
                Some(MUTED),
                self.chat_selection.clone(),
                Position {
                    entry: index,
                    block: 0,
                    byte: 0,
                },
            ));
        }
        container(block)
            .padding(if open { [8, 12] } else { [0, 12] })
            .width(Fill)
            .style(if open { card } else { container::transparent })
            .into()
    }

    fn entry_view<'a>(&'a self, entry: &'a Entry, index: usize) -> Element<'a, Message> {
        match &entry.kind {
            EntryKind::Tool {
                name,
                input,
                complete,
                parent,
                failed,
                media,
                web,
                ..
            } => {
                let open = self.expanded.contains(&entry.id);
                let preview = activity::tool_preview(name, input);
                let mut block = column![
                    theme::button(
                        row![
                            chevron_icon(open, MUTED),
                            text(if *complete { "●" } else { "◌" })
                                .size(11)
                                .color(if *failed {
                                    DANGER
                                } else if *complete {
                                    SUCCESS
                                } else {
                                    ACCENT
                                }),
                            text(activity::tool_name(name)).size(12).color(theme::TEXT),
                            ellipsized_text(&preview, 12., Some(MUTED)),
                            text(if *failed {
                                "failed"
                            } else if !*complete {
                                "running"
                            } else {
                                ""
                            })
                            .size(11)
                            .color(if *failed {
                                DANGER
                            } else {
                                MUTED
                            })
                        ]
                        .spacing(9)
                        .align_y(iced::Center)
                    )
                    .on_press(Message::ToggleEntry(entry.id.clone()))
                    .style(theme::text_button)
                    .padding([4, 2])
                    .width(Fill)
                ]
                .spacing(4);
                let mut details = column![].spacing(8);
                if open
                    && name == "spawn_subagent"
                    && let (Some(h), Some(session)) = (&self.harness, self.current())
                    && let Some(run) = h.subagent_for_request(&session.id, &entry.id)
                {
                    details = details.push(
                        theme::button(
                            text(format!(
                                "{} · {} · {} ↗",
                                run.profile.label(),
                                run.title,
                                run.status.label()
                            ))
                            .size(12),
                        )
                        .on_press(Message::Select(run.session_id))
                        .style(theme::text_button),
                    );
                }
                if open {
                    let raw = self.raw_tool_inputs.contains(&entry.id);
                    details = details.push(
                        theme::button(
                            row![
                                chevron_icon(raw, MUTED),
                                text("Raw input").size(11).color(MUTED)
                            ]
                            .spacing(6)
                            .align_y(iced::Center),
                        )
                        .padding([2, 0])
                        .style(theme::text_button)
                        .on_press(Message::ToggleToolRaw(entry.id.clone())),
                    );
                    if let Some(metadata) = web {
                        details = details.push(
                            text(format!(
                                "{} web access · {}",
                                metadata.mode,
                                age(metadata.searched_at)
                            ))
                            .size(11)
                            .color(MUTED),
                        );
                        for source in &metadata.sources {
                            details = details.push(
                                theme::button(ellipsized_text(&source.title, 12., Some(ACCENT)))
                                    .on_press(Message::Link(source.url.clone()))
                                    .style(theme::text_button),
                            );
                        }
                    }
                    for item in media {
                        if item.thumbnail.is_some() {
                            details = details.push(self.image_view.thumbnail(item));
                        }
                    }
                    if let Some(view) = self.outputs.get(&entry.id) {
                        details = details.push(
                            container(view.view(&entry.id, self.chat_selection.clone(), index))
                                .padding(8)
                                .width(Fill)
                                .style(theme::tool_code),
                        );
                    }
                    details = details.push(chat_selection::plain(
                        &tool_reference(&entry.id, parent.as_deref()),
                        11.,
                        Some(MUTED),
                        self.chat_selection.clone(),
                        Position {
                            entry: index,
                            block: 1,
                            byte: 0,
                        },
                    ));
                    block = block.push(
                        container(container(details).padding(12).width(Fill).style(card))
                            .padding(iced::Padding {
                                left: 20.,
                                ..iced::Padding::ZERO
                            })
                            .width(Fill),
                    );
                }
                container(block).padding([0, 12]).width(Fill).into()
            }
            EntryKind::Notice if activity::subagent_notice(entry).is_some() => {
                self.subagent_notice_view(entry, index)
            }
            EntryKind::Notice => container(chat_selection::plain(
                &entry.text,
                12.,
                Some(WARNING),
                self.chat_selection.clone(),
                Position {
                    entry: index,
                    block: 0,
                    byte: 0,
                },
            ))
            .padding(12)
            .style(card)
            .into(),
            EntryKind::Reasoning => {
                let content: Element<'_, Message> =
                    if let Some(markdown) = self.markdown.get(&entry.id) {
                        markdown::view_with(
                            markdown.content.items(),
                            markdown::Settings::with_text_size(12., theme::markdown_style()),
                            &chat_selection::Viewer::new(self.chat_selection.clone(), index)
                                .with_images(&self.markdown_images),
                        )
                    } else {
                        chat_selection::plain(
                            &entry.text,
                            12.,
                            Some(MUTED),
                            self.chat_selection.clone(),
                            Position {
                                entry: index,
                                block: 0,
                                byte: 0,
                            },
                        )
                    };
                container(content)
                    .padding([6, 24])
                    .style(card)
                    .width(Fill)
                    .into()
            }
            _ => {
                let mut content = column![].spacing(8);
                if let EntryKind::User { images } = &entry.kind {
                    for media in images {
                        content = content.push(self.image_view.thumbnail(media));
                    }
                }
                if let Some(markdown) = self.markdown.get(&entry.id) {
                    content = content.push(markdown::view_with(
                        markdown.content.items(),
                        markdown::Settings::with_text_size(
                            self.catalog.settings.font_size,
                            theme::markdown_style(),
                        ),
                        &chat_selection::Viewer::new(self.chat_selection.clone(), index)
                            .with_images(&self.markdown_images),
                    ));
                } else {
                    content = content.push(chat_selection::plain(
                        &entry.text,
                        self.catalog.settings.font_size,
                        None,
                        self.chat_selection.clone(),
                        Position {
                            entry: index,
                            block: 0,
                            byte: 0,
                        },
                    ));
                }
                let user = matches!(entry.kind, EntryKind::User { .. });
                container(content)
                    .padding([if user { 12 } else { 6 }, 12])
                    .width(Fill)
                    .style(if user { card } else { container::transparent })
                    .into()
            }
        }
    }
    fn new_session(&self) -> Element<'_, Message> {
        let mut projects = column![].spacing(10);
        let mut ordered: Vec<_> = self.catalog.projects.iter().collect();
        ordered.sort_by_key(|p| !p.favorite);
        for project in ordered {
            projects = projects.push(
                row![
                    theme::button(
                        column![
                            text(project.name()).size(17),
                            text(project.path.display().to_string())
                                .size(12)
                                .color(MUTED)
                        ]
                        .spacing(7)
                    )
                    .on_press(Message::Create(project.path.clone()))
                    .padding(16)
                    .width(Fill)
                    .style(secondary),
                    theme::button(text(if project.favorite { "★" } else { "☆" }).size(22))
                        .on_press(Message::Favorite(project.path.clone()))
                        .style(theme::text_button)
                ]
                .spacing(8)
                .align_y(iced::Center),
            );
        }
        let mut content = column![
            text("PICO / V2").size(12).color(ACCENT),
            text("Start something new.").size(32),
            text("Choose a project. Your conversations and work stay together.")
                .size(15)
                .color(MUTED),
            projects,
            theme::button("+  Add project")
                .on_press_maybe(self.harness.as_ref().map(|_| Message::AddProject))
                .padding(12)
                .style(secondary)
        ]
        .spacing(22);
        if self.catalog.projects.is_empty() {
            content=content.push(text("Add a local folder to begin. Pico can read and edit files and run commands in your projects.").size(13).color(MUTED));
        }
        if self.account.is_none() {
            content=content.push(container(column![text("Connect your ChatGPT subscription").size(16),text("Sign in from Settings. Your login is saved securely in your private pico-evo configuration directory.").size(13).color(MUTED),theme::button("Open Settings →").on_press(Message::ToggleSettings).style(theme::text_button)].spacing(10)).padding(18).style(card));
        }
        container(scroll(
            container(content)
                .max_width(650. - crate::scrollbar::GUTTER)
                .padding(iced::Padding::new(36.).right(36. - crate::scrollbar::GUTTER)),
        ))
        .center_x(Fill)
        .center_y(Fill)
        .height(Fill)
        .width(Fill)
        .into()
    }
    fn settings_panel(&self) -> Element<'_, Message> {
        let mut tabs = row![].spacing(4);
        for (tab, label) in [
            (SettingsTab::Account, "Account"),
            (SettingsTab::General, "General"),
            (SettingsTab::Models, "Models"),
            (SettingsTab::Images, "Images"),
        ] {
            tabs = tabs.push(
                theme::button(text(label).size(13).width(Fill).align_x(iced::Center))
                    .width(Fill)
                    .padding([8, 4])
                    .on_press(Message::SettingsTab(tab))
                    .style(if self.settings_tab == tab {
                        theme::primary
                    } else {
                        theme::text_button
                    }),
            );
        }
        let header = column![
            right_panel_header("Settings", Message::ToggleSettings),
            tabs,
            widget::rule::horizontal(1)
        ]
        .spacing(12);
        let header = container(header).padding(iced::Padding::ZERO.right(crate::scrollbar::GUTTER));
        let mut settings = column![].spacing(14);
        match self.settings_tab {
            SettingsTab::Account => {
                if let Some(account) = &self.account {
                    settings = settings.push(text(account).size(14)).push(
                        theme::button("Sign out")
                            .on_press(Message::Logout)
                            .style(secondary),
                    );
                } else if self.login_cancel.is_some() {
                    settings = settings.push(text("Finish sign-in in your browser.").size(14));
                    if let Some(url) = &self.login_url {
                        settings = settings.push(
                            theme::button("Copy login URL")
                                .on_press(Message::Copy(url.clone()))
                                .style(theme::text_button),
                        );
                    }
                    settings = settings.push(
                        theme::button("Cancel login")
                            .on_press(Message::CancelLogin)
                            .style(secondary),
                    );
                } else {
                    settings = settings
                        .push(text("Use your ChatGPT subscription.").size(13).color(MUTED))
                        .push(
                            theme::button("Sign in with ChatGPT")
                                .on_press_maybe(self.harness.as_ref().map(|_| Message::Login))
                                .padding(10),
                        );
                }
                settings=settings.push(widget::rule::horizontal(1))
                    .push(text("OpenAI API key · images only").size(14))
                    .push(text(if self.api_key_configured {"A private API key is saved."}else{"No API key saved."}).size(12).color(MUTED))
                    .push(text_input(if self.api_key_configured {"Enter a replacement key"}else{"Enter API key"},&self.api_key_draft).style(theme::input).secure(true).on_input(Message::ApiKey).padding(8))
                    .push(row![theme::button("Save key").on_press_maybe((!self.api_key_draft.trim().is_empty()).then_some(Message::SaveApiKey)),theme::button("Remove key").on_press_maybe(self.api_key_configured.then_some(Message::RemoveApiKey))].spacing(8))
                    .push(text("Stored privately, separately from settings. No environment override. Signing out of ChatGPT does not remove this key.").size(12).color(MUTED));
            }
            SettingsTab::Images => {
                use pico_core::image_tools::{Backend, DEFAULT_API_MODEL, QUALITIES};
                settings=settings
                    .push(text("Image generation").size(18))
                    .push(text("Billing backend").size(12))
                    .push(pick_list([Backend::Codex,Backend::Api],Some(self.settings.images.backend),Message::ImageBackend).style(theme::pick_list).menu_style(theme::pick_list_menu).width(Fill))
                    .push(text("Codex uses your ChatGPT subscription. API generations are billed separately. Pico never switches billing automatically.").size(12).color(MUTED))
                    .push(text("API model · generation and editing").size(12))
                    .push(text_input(DEFAULT_API_MODEL,&self.settings.images.model).style(theme::input).on_input(Message::ImageModel).padding(8))
                    .push(theme::button("Reset API model to current default").on_press(Message::ResetImageModel).style(theme::text_button))
                    .push(text("Quality · both backends").size(12))
                    .push(pick_list(QUALITIES,Some(self.settings.images.quality.as_str()),|q|Message::ImageQuality(q.into())).style(theme::pick_list).menu_style(theme::pick_list_menu).width(Fill))
                    .push(text("Size is automatic. GIF and quality availability depend on your model and account. Unsupported choices return an error, never a silent downgrade.").size(12).color(MUTED));
                if self.settings.images.backend == Backend::Api && !self.api_key_configured {
                    settings = settings.push(
                        text("Add an API key in Account before generating.")
                            .size(12)
                            .color(ACCENT),
                    );
                }
            }
            SettingsTab::General => {
                use pico_core::web_search::Mode;
                settings = settings
                    .push(text("Web access · main agent").size(12))
                    .push(pick_list([Mode::Live, Mode::Cached, Mode::Disabled], Some(self.settings.web_search), Message::WebMode).style(theme::pick_list).menu_style(theme::pick_list_menu).width(Fill))
                    .push(text("Uses your ChatGPT subscription. Live permits external access; Cached requests cache-only access and may limit operations. Disabled prevents new web calls. No API billing or automatic fallback.").size(12).color(MUTED))
                    .push(widget::rule::horizontal(1))
                    .push(text("Default model").size(12))
                    .push(
                        pick_list(
                            model_options(),
                            Some(self.settings.model.clone()),
                            Message::Model,
                        ).style(theme::pick_list).menu_style(theme::pick_list_menu)
                        .width(Fill),
                    )
                    .push(text("Context token limit").size(12))
                    .push(
                        text_input("272000", &self.context_text).style(theme::input)
                            .on_input(Message::Context)
                            .padding(8),
                    )
                    .push(text("Auto-compact at (%)").size(12))
                    .push(
                        text_input("80", &self.threshold_text).style(theme::input)
                            .on_input(Message::Threshold)
                            .padding(8),
                    )
                    .push(widget::rule::horizontal(1))
                    .push(text("APPEARANCE").size(10).color(MUTED))
                    .push(
                        text(format!(
                            "Chat text · {:.0}px",
                            self.catalog.settings.font_size
                        ))
                        .size(12),
                    )
                    .push(
                        slider(12.0..=24.0, self.catalog.settings.font_size, Message::Font)
                            .step(1_f32),
                    )
                    .push(
                        text(format!(
                            "Chat width · {:.0}px",
                            self.catalog.settings.chat_width
                        ))
                        .size(12),
                    )
                    .push(
                        slider(
                            480.0..=1600.0,
                            self.catalog.settings.chat_width,
                            Message::ChatWidth,
                        )
                        .step(20_f32),
                    )
                    .push(
                        checkbox(self.catalog.settings.resume_last).style(theme::checkbox)
                            .label("Restore last session")
                            .on_toggle(Message::Resume),
                    );
            }
            SettingsTab::Models => {
                for model in MODELS {
                    let defaults = &self.settings.models[model.id];
                    settings = settings.push(
                        container(
                            column![
                                text(model.name).size(16),
                                text(model.id).size(11).color(MUTED),
                                text("Default effort").size(12),
                                pick_list(
                                    model.efforts,
                                    Some(defaults.effort.as_str()),
                                    move |effort| Message::ModelEffort(
                                        model.id.into(),
                                        effort.into()
                                    )
                                )
                                .style(theme::pick_list)
                                .menu_style(theme::pick_list_menu)
                                .width(Fill),
                                checkbox(defaults.fast)
                                    .style(theme::checkbox)
                                    .label("Fast by default")
                                    .on_toggle(move |fast| Message::ModelFast(
                                        model.id.into(),
                                        fast
                                    )),
                            ]
                            .spacing(10),
                        )
                        .padding(12)
                        .width(Fill)
                        .style(card),
                    );
                }
            }
        }
        let mut panel_content = column![header, scroll(settings).height(Fill)].spacing(14);
        if self.settings_tab != SettingsTab::Account {
            panel_content = panel_content.push(
                row![
                    theme::button("Save")
                        .on_press(Message::ApplySettings)
                        .style(secondary),
                    theme::button("Cancel")
                        .on_press(Message::CancelSettings)
                        .style(theme::text_button)
                ]
                .spacing(8),
            );
        }
        container(panel_content)
            .padding(iced::Padding::new(RIGHT_PANEL_PADDING).right(theme::SCROLLBAR_EDGE_INSET))
            .width(
                self.catalog
                    .settings
                    .settings_width
                    .min(self.window_width * 0.4),
            )
            .height(Fill)
            .style(right_panel)
            .into()
    }
    fn footer(&self) -> Element<'_, Message> {
        widget::responsive(|size| self.footer_content(size.width))
            .height(36)
            .into()
    }

    fn footer_content(&self, width: f32) -> Element<'_, Message> {
        let Some(session) = self.current() else {
            return container(
                row![
                    text("Pico V2").size(11).color(MUTED),
                    space().width(Fill),
                    text(if self.account.is_some() {
                        "ChatGPT connected"
                    } else {
                        "Not signed in"
                    })
                    .size(11)
                    .color(MUTED),
                ]
                .align_y(iced::Center),
            )
            .padding([8, 14])
            .height(36)
            .into();
        };
        let conversation = self.harness.as_ref().and_then(|h| h.get(&session.id));
        let running = conversation.as_ref().is_some_and(|c| c.is_running());
        let status = conversation
            .as_ref()
            .map(|c| c.status.lock().expect("status lock").clone())
            .unwrap_or_else(|| "Ready".into());
        let turn_usage = conversation
            .as_ref()
            .map_or(pico_core::agent::TurnUsage::default(), |c| {
                *c.turn_usage.lock().expect("turn usage lock")
            });
        let seconds = turn_usage.streaming_time.as_secs_f64();
        let output = turn_usage.output;
        let rate = if seconds > 0. {
            output as f64 / seconds
        } else {
            0.
        };
        let used = session.usage.input;
        let cache = (session.usage.total_cached as f64 * 100.
            / session.usage.total_input.max(1) as f64)
            .round();
        let tokens = format!(
            "{} / {} tokens",
            compact_tokens(used),
            compact_tokens(self.catalog.settings.context_limit)
        );
        let token_details = format!(
            "Input: {}\nOutput: {}",
            compact_tokens(session.usage.total_input),
            compact_tokens(session.usage.total_output),
        );
        let rate_details = format!(
            "Latest turn: {} output tokens\nElapsed: {seconds:.1}s",
            compact_tokens(output),
        );
        let cache_details = format!(
            "Session cached input: {}\nSession total input: {}",
            compact_tokens(session.usage.total_cached),
            compact_tokens(session.usage.total_input),
        );
        let effort_label = if session.fast {
            format!("{} · Fast", session.effort)
        } else {
            session.effort.clone()
        };
        let layout = FooterLayout::new(width, &session.model, &effort_label);
        let mut hidden = Vec::new();
        if !layout.git
            && let Some(git) = self.git_info()
        {
            hidden.push(git.to_string());
        }
        if !layout.usage {
            hidden.push(token_details.clone());
        }
        if !layout.rate {
            hidden.push(format!("TPS: {rate:.1}\n{rate_details}"));
        }
        if !layout.cache {
            hidden.push(format!("Cache: {cache:.0}%\n{cache_details}"));
        }
        let control = |kind, label: &str, width| {
            let open = self
                .footer_menu
                .as_ref()
                .is_some_and(|menu| menu.control == kind);
            let chip = theme::button(ellipsized_text(label, 11., None))
                .width(width)
                .padding([3, 4])
                .on_press_maybe(
                    (!running && session.child.is_none()).then_some(Message::FooterMenu(kind)),
                )
                .style(move |theme, status| {
                    let mut style = sidebar_control(theme, status);
                    if open {
                        style.text_color = theme.palette().text;
                    }
                    style
                });
            if running {
                footer_tip(
                    chip,
                    "Stop the current turn before changing model or effort.".into(),
                )
            } else if footer_text_width(label, 11.) > width - 8. {
                footer_tip(chip, label.into())
            } else {
                chip.into()
            }
        };
        let mut info = row![footer_tip(
            crate::status_circle::status_circle(running, self.tick),
            status.clone(),
        )]
        .spacing(4)
        .align_y(iced::Center);

        let path = session.project.display().to_string();
        let display_path = std::env::var_os("HOME")
            .and_then(|home| {
                session.project.strip_prefix(home).ok().map(|suffix| {
                    if suffix.as_os_str().is_empty() {
                        "~".into()
                    } else {
                        format!("~/{}", suffix.display())
                    }
                })
            })
            .unwrap_or_else(|| path.clone());
        if width < 240. {
            // At the minimum window size, two maximized sidebars leave only 112px.
            // Collapse all metadata into the status target instead of losing controls.
            let details = format!("{status}\n{path}\n\n{}", hidden.join("\n\n"));
            let controls_width = (width - 44.).max(0.);
            return container(
                row![
                    footer_tip(
                        crate::status_circle::status_circle(running, self.tick),
                        details
                    ),
                    control(FooterControl::Model, &session.model, controls_width * 0.65),
                    footer_separator(),
                    control(FooterControl::Effort, &effort_label, controls_width * 0.35),
                ]
                .spacing(2)
                .align_y(iced::Center),
            )
            .padding([6, 4])
            .height(36)
            .width(Fill)
            .into();
        }
        let full_path_width = footer_text_width(&display_path, 11.).ceil();
        let path_width = layout.path_width.min(240.).min(full_path_width);
        let path_label = if full_path_width <= path_width {
            display_path
        } else {
            shorten(&display_path, (path_width / 7.).max(1.) as usize)
        };
        let path_view = container(ellipsized_text(&path_label, 11., Some(MUTED))).width(path_width);
        info = info.push(
            if path_label != path || footer_text_width(&path_label, 11.) > path_width {
                footer_tip(path_view, path)
            } else {
                path_view.into()
            },
        );
        if layout.git
            && let Some(git) = self.git_info()
        {
            let view: Element<'_, Message> = match git {
                crate::git::Info::Changes {
                    branch,
                    added: 0,
                    removed: 0,
                } => container(ellipsized_text(branch, 11., Some(MUTED)))
                    .width(footer_text_width(branch, 11.).ceil().min(126.))
                    .into(),
                crate::git::Info::Changes {
                    branch,
                    added,
                    removed,
                } => {
                    let additions = format!("+{added}");
                    let deletions = format!("−{removed}");
                    let counts_width = footer_text_width(&additions, 11.)
                        + footer_text_width(&deletions, 11.)
                        + 8.;
                    row![
                        ellipsized_text(branch, 11., Some(MUTED)),
                        text(additions).size(11).color(SUCCESS),
                        text(deletions).size(11).color(DANGER),
                    ]
                    .spacing(4)
                    .align_y(iced::Center)
                    .width(
                        (footer_text_width(branch, 11.).ceil() + counts_width.ceil() + 2.)
                            .min(126.),
                    )
                    .into()
                }
                crate::git::Info::Unavailable(message) => {
                    container(ellipsized_text(message, 11., Some(MUTED)))
                        .width(126.)
                        .into()
                }
            };
            info = info
                .push(footer_separator())
                .push(footer_tip(view, git.to_string()));
        }
        if layout.usage {
            info = info.push(footer_separator()).push(footer_tip(
                text(tokens).size(11).color(MUTED),
                token_details,
            ));
        }
        if layout.rate {
            info = info.push(footer_separator()).push(footer_tip(
                text(format!("TPS: {rate:.0}")).size(11).color(MUTED),
                rate_details,
            ));
        }
        if layout.cache {
            info = info.push(footer_separator()).push(footer_tip(
                text(format!("{cache:.0}% cache")).size(11).color(MUTED),
                cache_details,
            ));
        }
        if !hidden.is_empty() {
            info = info.push(footer_separator()).push(footer_tip(
                text("…").size(13).color(MUTED),
                hidden.join("\n\n"),
            ));
        }
        info = info
            .push(space().width(Fill))
            .push(control(
                FooterControl::Model,
                &session.model,
                layout.model_width,
            ))
            .push(footer_separator())
            .push(control(
                FooterControl::Effort,
                &effort_label,
                layout.effort_width,
            ));
        container(info)
            .padding([6, 14])
            .height(36)
            .width(Fill)
            .into()
    }

    fn footer_popup<'a>(&'a self, menu: &'a FooterMenu) -> Element<'a, Message> {
        let session = self.current().expect("footer menu has a session");
        let selected = match menu.control {
            FooterControl::Model => &session.model,
            FooterControl::Effort => &session.effort,
        };
        let mut options = column![].spacing(2);
        for (index, option) in menu.options.iter().enumerate() {
            let chosen = if option == FAST_OPTION {
                session.fast
            } else {
                option == selected
            };
            let focused = menu.focused == index;
            let message = match menu.control {
                FooterControl::Model => Message::SessionModel(option.clone()),
                FooterControl::Effort if option == FAST_OPTION => Message::ToggleSessionFast,
                FooterControl::Effort => Message::SessionEffort(option.clone()),
            };
            let item = theme::button(
                row![
                    ellipsized_text(option, FOOTER_MENU_TEXT_SIZE, None),
                    text(if chosen { "✓" } else { "" })
                        .size(FOOTER_MENU_TEXT_SIZE)
                        .color(ACCENT)
                        .width(16),
                ]
                .spacing(8)
                .align_y(iced::Center),
            )
            .width(Fill)
            .height(30)
            .padding([6, 8])
            .on_press(message)
            .style(move |theme, status| theme::menu_button(theme, status, focused || chosen));
            options = options.push(
                if footer_text_width(option, FOOTER_MENU_TEXT_SIZE)
                    > 228. - crate::scrollbar::GUTTER
                {
                    footer_tip(item, option.clone())
                } else {
                    item.into()
                },
            );
        }
        opaque(
            container(scroll(options).height((menu.options.len() as f32 * 32. - 2.).min(240.)))
                .padding(6)
                .width(280)
                .style(footer_surface),
        )
    }
    fn modal_view(&self, modal: &Modal) -> Element<'_, Message> {
        let (title, body, confirm) = match modal {
            Modal::Image(media) => {
                return self.image_view.view(
                    media.path.display().to_string(),
                    format!("{} × {} · {} bytes", media.width, media.height, media.bytes),
                    self.window_width,
                    self.window_height,
                );
            }
            Modal::MarkdownImage(image) => {
                return self.image_view.view(
                    image.url.clone(),
                    format!(
                        "{} × {} · {} bytes",
                        image.width,
                        image.height,
                        image.bytes.len()
                    ),
                    self.window_width,
                    self.window_height,
                );
            }
            Modal::AddProject => (
                "Add project",
                "Choose a local folder. Commands run without approval prompts and can access files outside this folder.",
                "Add project",
            ),
            Modal::Rename(_) => (
                "Rename session",
                "Give this conversation a useful title.",
                "Rename",
            ),
            Modal::Delete(_) => (
                "Delete session?",
                "This permanently deletes the saved conversation and all its subagent conversations and results. Project files are not deleted. Stop the parent and its children first.",
                "Delete",
            ),
            Modal::Logout => (
                "Sign out?",
                "Remove saved ChatGPT credentials from this Pico V2 installation. Your sessions are kept.",
                "Sign out",
            ),
            Modal::Quit => (
                "Stop work and quit?",
                "Running turns and shell processes will be cancelled. Already completed commands are not undone. Sessions are saved.",
                "Stop and quit",
            ),
            Modal::Link(_) => (
                "Open link in browser?",
                "This link came from the conversation. Check its destination before opening it.",
                "Open link",
            ),
        };
        let mut content =
            column![text(title).size(24), text(body).size(14).color(MUTED)].spacing(18);
        match modal {
            Modal::AddProject | Modal::Rename(_) => {
                content = content.push(
                    text_input("", &self.modal_input)
                        .style(theme::input)
                        .id("modal-input")
                        .on_input(Message::ModalInput)
                        .on_submit(Message::Confirm)
                        .padding(10),
                );
                if matches!(modal, Modal::AddProject) {
                    content = content.push(
                        theme::button("Browse folders…")
                            .on_press(Message::Browse)
                            .style(secondary),
                    );
                }
            }
            Modal::Link(url) => content = content.push(text(url.clone()).size(13)),
            _ => {}
        }
        if let Some(error) = &self.error {
            content = content.push(text(error).size(12).color(DANGER));
        }
        content = content.push(
            row![
                space().width(Fill),
                theme::button("Cancel")
                    .on_press(Message::Dismiss)
                    .style(secondary),
                theme::button(confirm).on_press(Message::Confirm)
            ]
            .spacing(12),
        );
        container(content)
            .padding(28)
            .max_width(540)
            .style(card)
            .into()
    }
    fn smoke_frame(&mut self, screenshot: window::Screenshot) -> Task<Message> {
        let pixels = &screenshot.rgba;
        let valid = screenshot.size.width > 0
            && screenshot.size.height > 0
            && pixels.len() == screenshot.size.width as usize * screenshot.size.height as usize * 4
            && pixels.as_chunks::<4>().0.iter().any(|p| p != &pixels[..4]);
        if !valid {
            return iced::exit();
        }
        if let Some(path) = std::env::var_os("PICO_EVO_SMOKE_RGBA") {
            let path = PathBuf::from(path);
            let _ = std::fs::write(
                path.with_extension(format!("{}.rgba", self.smoke_stage)),
                pixels,
            );
            println!(
                "SMOKE_FRAME {} {} {}",
                self.smoke_stage, screenshot.size.width, screenshot.size.height
            );
        }
        self.smoke_stage += 1;
        match self.smoke_stage {
            1 => {
                let h = self.harness.as_ref().expect("smoke storage");
                let path = self.smoke_dir.clone().expect("smoke path");
                self.catalog.projects.push(Project {
                    path: path.clone(),
                    favorite: true,
                });
                let mut s = Session::new(path, &self.catalog.settings);
                s.title = "Phase 1 rendered-window check".into();
                s.usage.input = 60_721;
                s.usage.total_input = 160_721;
                s.usage.cached = 56_470;
                s.usage.total_cached = 126_470;
                s.usage.total_output = 1_237;
                s.entries = vec![
                    Entry::new(EntryKind::User { images: vec![] }, "Inspect this project and run its tests."),
                    Entry::new(
                        EntryKind::Assistant,
                        "## Project checked\n\nThe **agent loop** is ready.\n\n- Streaming Markdown\n- Durable sessions\n\n```rust\nfn main() { println!(\"Pico V2\"); }\n```\n\n[Documentation](https://openai.com)",
                    ),
                ].into();
                let tool = Entry {
                    id: "smoke-tool".into(),
                    text: String::new(),
                    kind: EntryKind::Tool {
                        name: "exec".into(),
                        input: "text(await tools.exec_command({cmd: 'cargo test'}));".into(),
                        output: "Script completed\n14 tests passed".into(),
                        complete: true,
                        failed: false,
                        cell_id: None,
                        parent: None,
                        media: vec![],
                        web: None,
                    },
                };
                s.entries.push(tool);
                let id = s.id.clone();
                h.insert(s);
                self.select(Selection::Session(id));
                let _ = self.update_inner(Message::ToggleEntry("smoke-tool".into()));
                // Session loading is asynchronous, so ToggleEntry may run before
                // its activity group is projected. Open the known fixture group
                // explicitly so the later large-output frames really exercise it.
                self.groups.insert("smoke-tool".into());
            }
            2 => {
                // Render a cross-message selection through rich text and lists,
                // including after the settings panel reflows the transcript.
                {
                    let mut selection = self.chat_selection.borrow_mut();
                    selection.anchor = Some(Position {
                        entry: 0,
                        block: 0,
                        byte: 8,
                    });
                    selection.focus = Some(Position {
                        entry: 1,
                        block: 2,
                        byte: 7,
                    });
                }
                self.git.insert(
                    self.current().unwrap().project.clone(),
                    crate::git::Info::Changes {
                        branch: "main".into(),
                        added: 123,
                        removed: 45,
                    },
                );
                self.settings_open = true;
                return Task::batch([
                    self.update_inner(Message::ToggleSearch),
                    bottom(),
                    delayed_capture(),
                ]);
            }
            3 => {
                self.chat_selection.borrow_mut().clear();
                let _ = self.update_inner(Message::ToggleSearch);
                self.modal = Some(Modal::AddProject);
                self.modal_input = "/tmp/example-project".into();
            }
            4 => {
                self.modal = None;
                self.settings_open = false;
                self.context_menu = self
                    .selected
                    .clone()
                    .map(|id| (id, iced::Point::new(160., 260.)));
            }
            5 => {
                self.context_menu = None;
                let output = format!(
                    "{}\nTAIL: complete output remains accessible",
                    "long output row with Unicode λ界 and asymmetric data 0123456789\n"
                        .repeat(16000)
                );
                self.harness
                    .as_ref()
                    .expect("smoke harness")
                    .edit(self.selected.as_ref().expect("smoke session"), |s| {
                        if let EntryKind::Tool { output: target, .. } =
                            &mut s.entries.last_mut().expect("smoke tool").kind
                        {
                            *target = output;
                        }
                    })
                    .expect("save smoke fixture");
                self.refresh();
                return Task::batch([bottom(), delayed_capture()]);
            }
            6 => {
                return Task::batch([
                    widget::operation::snap_to(
                        "tool-output-smoke-tool",
                        scrollable::RelativeOffset { x: 0., y: 0.5 },
                    ),
                    delayed_capture(),
                ]);
            }
            7 => {
                return Task::batch([
                    widget::operation::snap_to(
                        "tool-output-smoke-tool",
                        scrollable::RelativeOffset::END,
                    ),
                    delayed_capture(),
                ]);
            }
            8 => {
                self.settings_open = true;
                return Task::batch([bottom(), delayed_capture()]);
            }
            9 => {
                return Task::batch([
                    widget::operation::snap_to(
                        "tool-output-smoke-tool",
                        scrollable::RelativeOffset::END,
                    ),
                    delayed_capture(),
                ]);
            }
            10 => {
                let h = self.harness.as_ref().expect("smoke harness");
                h.edit(self.selected.as_ref().expect("smoke session"),|s| {
                    for i in 0..20000 {s.entries.push(Entry::new(EntryKind::Assistant,format!("### Historical message {i}\n\nAsymmetric **Markdown** content with λ and 界.")));}
                }).expect("large smoke transcript");
                self.refresh();
                for i in 0..10000 {
                    self.sessions.push(SessionSummary {
                        child: None,
                        id: format!("smoke-sidebar-{i}"),
                        title: format!("Historical session {i} with wrapping title"),
                        project: self.smoke_dir.clone().expect("smoke path"),
                        updated: 0,
                        interrupted: false,
                        needs_attention: false,
                        unchecked_completion: false,
                    });
                }
                self.prepare_sidebar();
                self.auto_scroll = false;
                self.transcript.offset = self.transcript.prefix(10000);
                self.prepare_visible();
                return Task::batch([
                    widget::operation::scroll_to(
                        "chat",
                        scrollable::AbsoluteOffset {
                            x: 0.,
                            y: self.transcript.offset + 20.,
                        },
                    ),
                    widget::operation::scroll_to(
                        "sessions",
                        scrollable::AbsoluteOffset {
                            x: 0.,
                            y: self.sidebar_list.prefix(5000),
                        },
                    ),
                    delayed_capture(),
                ]);
            }
            11 => {
                assert!(self.markdown.len() < 100, "bounded visible Markdown cache");
                assert!(
                    self.transcript.visible().start > 1000,
                    "middle of long transcript is reachable"
                );
                self.auto_scroll = true;
                self.transcript.end();
                self.prepare_visible();
                return Task::batch([bottom(), delayed_capture()]);
            }
            12 => {
                assert_eq!(
                    self.transcript.visible().end,
                    self.transcript.order.len(),
                    "auto-scroll reaches history tail"
                );
                self.settings_open = false;
                self.view_epoch = self.view_epoch.wrapping_add(1);
                return Task::batch([bottom(), delayed_capture()]);
            }
            13 => {
                // Force fresh equal-height measurements for a new list identity.
                self.view_epoch = self.view_epoch.wrapping_add(1);
                self.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
                self.activity = activity::Projection::default();
                self.append_transcript();
                self.transcript.end();
                self.prepare_visible();
                return Task::batch([bottom(), delayed_capture()]);
            }
            14 => {
                // Return the virtual sidebar to the top before moving surviving
                // visible rows with a search, then restore the full large list.
                return Task::batch([
                    self.update_inner(Message::Search(String::new())),
                    delayed_capture(),
                ]);
            }
            15 => {
                return Task::batch([
                    self.update_inner(Message::Search("Historical session 1".into())),
                    delayed_capture(),
                ]);
            }
            16 => {
                return Task::batch([
                    self.update_inner(Message::Search(String::new())),
                    delayed_capture(),
                ]);
            }
            17 => {
                self.settings_open = false;
                let _ = self.update_inner(Message::FooterMenu(FooterControl::Model));
            }
            18 => {
                let _ = self.update_inner(Message::FooterMenu(FooterControl::Effort));
            }
            19 => {
                self.footer_menu = None;
                self.settings_open = true;
                self.settings_tab = SettingsTab::General;
            }
            20 => self.settings_tab = SettingsTab::Models,
            21 => {
                self.harness
                    .as_ref()
                    .expect("smoke harness")
                    .edit(self.selected.as_ref().expect("smoke session"), |s| {
                        s.fast = true
                    })
                    .expect("smoke fast");
                self.refresh();
                let _ = self.update_inner(Message::FooterMenu(FooterControl::Effort));
            }
            22 => {
                self.footer_menu = None;
                self.settings_open = false;
                self.agents_open = true;
            }
            23 => {
                self.agents_open = false;
                let parent = self.selected.clone().expect("smoke parent");
                let mut child = Session::new(
                    self.smoke_dir.clone().expect("smoke path"),
                    &self.catalog.settings,
                );
                child.child = Some(pico_core::agent::subagents::Child {
                    parent_id: parent,
                    profile: pico_core::agent::subagents::Profile::Explore,
                });
                child.title = "Trace completion notifications and cancellation".into();
                child.model = "gpt-6-luna".into();
                child.effort = "high".into();
                child.entries.push(Entry::new(
                    EntryKind::User { images: vec![] },
                    "Inspect the completion notification path. Do not modify files.",
                ));
                child.entries.push(Entry::new(EntryKind::Assistant,"## Exploration result\n\nThe **parent wake-up** path preserves cancellation and uses durable completion notices.\n\n- Separate conversation context\n- Repeatable final results\n- No nested delegation"));
                let id = child.id.clone();
                self.harness.as_ref().expect("smoke harness").insert(child);
                self.select(Selection::Session(id));
            }
            24 => {
                self.agents_open = true;
                self.window_width = 860.;
                self.window_height = 560.;
                self.view_epoch = self.view_epoch.wrapping_add(1);
                if let Some(window) = self.window_id {
                    return Task::batch([
                        window::resize(window, iced::Size::new(860., 560.)),
                        delayed_capture(),
                    ]);
                }
            }
            25 => {
                let parent = self
                    .current()
                    .and_then(|s| s.child.as_ref())
                    .expect("smoke child")
                    .parent_id
                    .clone();
                self.agents_open = false;
                self.select(Selection::Session(parent));
                if let Some(window) = self.window_id {
                    return Task::batch([
                        window::resize(window, iced::Size::new(1200., 800.)),
                        delayed_capture(),
                    ]);
                }
            }
            26 => {
                let request = pico_core::questionnaire::Request::parse(r#"{"questions":[{"id":"project","question":"Here’s a demo of a single-choice question. What kind of project would you like to build? You can also enter your own answer.","kind":"select","options":["A website","A mobile app","A command-line tool"]},{"id":"details","question":"Describe the constraints and any additional requirements.","kind":"text"}]}"#).expect("smoke questionnaire");
                let id = "smoke-questionnaire".to_owned();
                self.questionnaires.insert(
                    id.clone(),
                    questionnaire::Draft::new(pico_core::questionnaire::Pending {
                        id: id.clone(),
                        request: Arc::new(request),
                    }),
                );
                self.active_questionnaire = Some(id);
            }
            27..=30 => {
                let draft = self
                    .questionnaires
                    .get_mut("smoke-questionnaire")
                    .expect("smoke questionnaire draft");
                let action = match self.smoke_stage {
                    27 => questionnaire::Action::Edit(1),
                    28 => questionnaire::Action::Edit(0),
                    29 => questionnaire::Action::Toggle,
                    30 => {
                        draft.update(questionnaire::Message {
                            question: 0,
                            action: questionnaire::Action::Toggle,
                        });
                        draft.update(questionnaire::Message {
                            question: 0,
                            action: questionnaire::Action::Replace(false),
                        });
                        draft.update(questionnaire::Message {
                            question: 0,
                            action: questionnaire::Action::Next,
                        });
                        questionnaire::Action::Editor(text_editor::Action::Edit(
                            text_editor::Edit::Paste(Arc::new(
                                "Long answer with Unicode λ界 and preserved newlines.\n"
                                    .repeat(700),
                            )),
                        ))
                    }
                    _ => unreachable!(),
                };
                let task = draft
                    .update(questionnaire::Message {
                        question: draft.current,
                        action,
                    })
                    .task
                    .map(|message| Message::Questionnaire("smoke-questionnaire".into(), message));
                return Task::batch([task, delayed_capture()]);
            }
            31 => {
                let request = serde_json::json!({"questions":[{"id":"many","question":"A long question that wraps rather than overflowing the available space. Choose an option, scroll through the complete list, or provide a custom answer.","kind":"select","options":(1..=20).map(|i| format!("Option {i}: preserve this complete descriptive label even in a narrow window with settings open.")).collect::<Vec<_>>()}]});
                let id = "smoke-questionnaire".to_owned();
                self.questionnaires.insert(
                    id.clone(),
                    questionnaire::Draft::new(pico_core::questionnaire::Pending {
                        id: id.clone(),
                        request: Arc::new(
                            pico_core::questionnaire::Request::parse(&request.to_string()).unwrap(),
                        ),
                    }),
                );
                self.active_questionnaire = Some(id);
                self.settings_open = true;
                self.settings_tab = SettingsTab::General;
                if let Some(window) = self.window_id {
                    return Task::batch([
                        window::resize(window, iced::Size::new(860., 560.)),
                        delayed_capture(),
                    ]);
                }
            }
            32 => {
                self.active_questionnaire = None;
                self.questionnaires.clear();
                self.settings_open = true;
                self.settings_tab = SettingsTab::Images;
            }
            33 => {
                self.settings_open = false;
                let media = pico_core::media::MediaStore::new(
                    self.smoke_dir
                        .as_ref()
                        .expect("smoke directory")
                        .join("image-smoke"),
                )
                .image(include_bytes!("fixtures/media.png"))
                .expect("valid PNG fixture");
                let mut session = Session::new(
                    self.smoke_dir.as_ref().unwrap().clone(),
                    &self.catalog.settings,
                );
                let entry = Entry {
                    id: "smoke-image".into(),
                    text: String::new(),
                    kind: EntryKind::Tool {
                        name: "view_image".into(),
                        input: "{path:example.png}".into(),
                        output: media.description(),
                        complete: true,
                        failed: false,
                        cell_id: None,
                        parent: None,
                        media: vec![media],
                        web: None,
                    },
                };
                session.entries.push(entry);
                self.current_session = Some(session);
                self.expanded.insert("smoke-image".into());
                self.groups.insert("smoke-image".into());
                self.rebuild_transcript();
                self.prepare_visible();
                return Task::batch(std::iter::once(delayed_capture()).chain(self.tasks.drain(..)));
            }
            34 => {
                let media = pico_core::media::MediaStore::new(
                    self.smoke_dir
                        .as_ref()
                        .expect("smoke directory")
                        .join("image-smoke"),
                )
                .image(include_bytes!("fixtures/media.gif"))
                .expect("valid GIF fixture");
                let task = self.image_view.open(&media).map(Message::Image);
                self.modal = Some(Modal::Image(media));
                return Task::batch([task, delayed_capture()]);
            }
            35 => {
                self.image_view.update(crate::image_view::Message::Toggle);
            }
            36 => {
                let media = pico_core::media::MediaStore::new(
                    self.smoke_dir
                        .as_ref()
                        .expect("smoke directory")
                        .join("image-smoke"),
                )
                .image(include_bytes!("fixtures/media.png"))
                .expect("valid PNG fixture");
                let task = self.image_view.open(&media).map(Message::Image);
                self.modal = Some(Modal::Image(media));
                return Task::batch([task, delayed_capture()]);
            }
            37 => {
                self.modal = None;
                self.image_view.close();
                let root = self.smoke_dir.as_ref().expect("smoke directory");
                std::fs::write(
                    root.join("markdown.gif"),
                    include_bytes!("fixtures/media.gif"),
                )
                .expect("GIF fixture");
                let mut session = Session::new(root.clone(), &self.catalog.settings);
                session.entries.push(Entry {
                    id: "smoke-markdown-image".into(),
                    text: "![Markdown GIF preview](markdown.gif \"Click to inspect\")\n\n![Missing image](missing.png)".into(),
                    kind: EntryKind::Assistant,
                });
                self.current_session = Some(session);
                self.markdown.clear();
                self.rebuild_transcript();
                self.prepare_visible();
                return Task::batch(std::iter::once(delayed_capture()).chain(self.tasks.drain(..)));
            }
            38 => {
                let image = self
                    .markdown_images
                    .loaded("markdown.gif")
                    .expect("Markdown GIF preview loaded");
                let task = self
                    .image_view
                    .open_bytes(&image.url, image.bytes.clone())
                    .map(Message::Image);
                self.modal = Some(Modal::MarkdownImage(image));
                return Task::batch([task, delayed_capture()]);
            }
            39 => {
                self.modal = None;
                self.image_view.close();
                self.settings_open = true;
                self.settings_tab = SettingsTab::General;
                let mut session = Session::new(
                    self.smoke_dir.as_ref().unwrap().clone(),
                    &self.catalog.settings,
                );
                session.entries.push(Entry {id:"smoke-web".into(),text:String::new(),kind:EntryKind::Tool {
                    name:"web__run".into(),input:r#"{"search_query":[{"q":"Rust documentation","domains":["rust-lang.org"]}]}"#.into(),
                    output:"Rust documentation (https://doc.rust-lang.org/book/)\nThe Rust Programming Language".into(),complete:true,failed:false, cell_id: None,parent:None,media:vec![],
                    web:Some(pico_core::web_search::Metadata{mode:pico_core::web_search::Mode::Live,searched_at:pico_core::store::now(),sources:vec![pico_core::web_search::Source{title:"The Rust Programming Language — a long source title that should fit in a constrained chat column".into(),url:"https://doc.rust-lang.org/book/".into()}],details:r#"[{"type":"text_result","url":"https://doc.rust-lang.org/book/"}]"#.into()})
                }});
                self.current_session = Some(session);
                self.expanded.insert("smoke-web".into());
                self.groups.insert("smoke-web".into());
                self.rebuild_transcript();
                self.prepare_visible();
            }
            40 => {
                self.settings_open = false;
                self.attachments = [
                    include_bytes!("fixtures/media.png").as_slice(),
                    include_bytes!("fixtures/media.gif").as_slice(),
                ]
                .into_iter()
                .map(|bytes| {
                    pico_core::media::MediaStore::new(
                        self.smoke_dir.as_ref().unwrap().join("attachment-smoke"),
                    )
                    .image(bytes)
                    .unwrap()
                })
                .collect();
                self.editor = text_editor::Content::with_text(
                    &(0..16)
                        .map(|line| {
                            format!("Draft line {line}: images are attached above this field.")
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                self.prepare_visible();
                return Task::batch(std::iter::once(delayed_capture()).chain(self.tasks.drain(..)));
            }
            41 => {
                self.settings_open = false;
                self.agents_open = false;
                self.attachments.clear();
                self.editor = text_editor::Content::new();
                self.groups.clear();
                self.expanded.clear();
                self.execution.clear();
                self.auto_scroll = false;
                self.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
                self.activity = activity::Projection::default();
                let mut session = Session::new(
                    self.smoke_dir.as_ref().unwrap().clone(),
                    &self.catalog.settings,
                );
                let tool =
                    |id: &str, name: &str, input: &str, parent: Option<&str>, complete, failed| {
                        Entry {
                            id: id.into(),
                            text: String::new(),
                            kind: EntryKind::Tool {
                                name: name.into(),
                                input: input.into(),
                                output: if failed {
                                    "Process exited with code 7\nA regression check failed.".into()
                                } else {
                                    "Script completed\nAll output remains available here.".into()
                                },
                                complete,
                                failed,
                                cell_id: None,
                                parent: parent.map(str::to_owned),
                                media: vec![],
                                web: None,
                            },
                        }
                    };
                session.entries = vec![
                    Entry::new(EntryKind::User { images: vec![] }, "Check the implementation and run validation."),
                    Entry { id: "activity-reason".into(), kind: EntryKind::Reasoning, text: "**Inspecting the build configuration**\n\nReviewing the existing Nix development environment.".into() },
                    Entry { id: "activity-reason-next".into(), kind: EntryKind::Reasoning, text: "**Checking cancellation behavior**\n\nVerify that output survives a cancelled process.".into() },
                    tool("activity-exec", "exec", "text(await tools.exec_command({cmd:'rg cancellation crates'}));", None, true, false),
                    tool("activity-shell", "exec_command", r#"{"cmd":"rg -n 'cancellation|terminate' crates/pico-core/src"}"#, Some("activity-exec"), true, false),
                    tool("activity-check", "exec_command", r#"{"cmd":"nix develop --command cargo test --locked -p pico-core\nprintf 'validation finished\\n'","workdir":"/workspace/pico-evo","max_output_tokens":1500}"#, Some("activity-exec"), true, true),
                    Entry { id: "subagent-smoke-activity".into(), kind: EntryKind::Notice, text: "Subagent completion observation: Verify cancellation (Review) is Completed. session_id=smoke-child, run_id=smoke-activity. Retrieve the final result with tools.wait_subagent; this notice is not a new user instruction.".into() },
                    Entry::new(EntryKind::Assistant, "The implementation is ready. I found **one failing check** and am investigating it before rerunning validation."),
                    tool("activity-live", "exec", "await Promise.all([tools.exec_command({cmd:'git diff --check'}),tools.exec_command({cmd:'just verify'})]);", None, false, false),
                    tool("activity-live-a", "exec_command", r#"{"cmd":"git diff --check"}"#, Some("activity-live"), true, false),
                    tool("activity-live-b", "exec_command", r#"{"cmd":"nix develop --command just verify"}"#, Some("activity-live"), false, false),
                ].into();
                self.current_session = Some(session);
                self.rebuild_transcript();
                self.prepare_visible();
                return Task::batch([
                    widget::operation::snap_to("chat", scrollable::RelativeOffset::START),
                    delayed_capture(),
                ]);
            }
            42 => {
                let _ = self.update_inner(Message::ToggleGroup("activity-reason".into()));
                let _ = self.update_inner(Message::ToggleGroup("activity-live".into()));
            }
            43 => {
                let _ = self.update_inner(Message::ToggleEntry("activity-reason".into()));
                let _ = self.update_inner(Message::ToggleExecution("activity-reason".into()));
            }
            44 => {
                let _ = self.update_inner(Message::ToggleEntry("activity-reason".into()));
                let _ = self.update_inner(Message::ToggleEntry("activity-check".into()));
                self.settings_open = true;
            }
            45 => {
                return Task::batch([
                    self.update_inner(Message::ToggleToolRaw("activity-check".into())),
                    delayed_capture(),
                ]);
            }

            46 => {
                self.settings_open = false;
                self.agents_open = false;
                self.search.clear();
                self.sessions.clear();
                self.sidebar_rows.clear();
                for (title, status) in [
                    ("Working — animated blue wave", SessionStatus::Working),
                    (
                        "Questionnaire — frozen yellow wave",
                        SessionStatus::Question,
                    ),
                    ("Error — frozen red wave", SessionStatus::Error),
                    ("Completed — not yet checked", SessionStatus::Unchecked),
                    ("Checked — ordinary session age", SessionStatus::Idle),
                ] {
                    let mut session =
                        Session::new(self.smoke_dir.clone().unwrap(), &self.catalog.settings);
                    session.title = title.into();
                    let index = self.sessions.len();
                    self.sessions.push(SessionSummary::from(&session));
                    if index != 2 && index != 4 {
                        let heading = ["WORKING", "NEEDS ATTENTION", "SESSIONS"][status.section()];
                        self.sidebar_rows
                            .push(SidebarRow::Heading(heading, if index == 0 { 1 } else { 2 }));
                    }
                    self.sidebar_rows.push(SidebarRow::Session(index, status));
                }
                self.sidebar_epoch = self.sidebar_epoch.wrapping_add(1);
                // These are new sessions, not movements of previously mounted rows.
                let key_base = self.sidebar_keys.len() + 3;
                self.sidebar_list
                    .reset(self.sidebar_rows.iter().enumerate().map(|(key, row)| {
                        (
                            key_base + key,
                            if matches!(row, SidebarRow::Heading(..)) {
                                36.
                            } else {
                                48.
                            },
                        )
                    }));
                self.sidebar_list.offset = 0.;
                return Task::batch([
                    widget::operation::scroll_to(
                        "sessions",
                        scrollable::AbsoluteOffset { x: 0., y: 0. },
                    ),
                    delayed_capture(),
                ]);
            }

            _ => {
                self.smoke_passed.store(true, Ordering::Relaxed);
                println!(
                    "SMOKE_OK: native Wayland rendered formatted tool panels/raw input, compact activity groups/thinking/execution details/failures, composer attachments/overflow scrollbar, web access settings/source links/details, Markdown images/error fallbacks/GIF modal, image thumbnails/modal/GIF play-pause/Images settings, picker, sidebar status waves/unchecked completion/favorites/search/reflow animations, chat, settings, modal, session/footer menus, Agents panel, read-only child navigation, questionnaires (select/edit/confirmation/collapse/large text/narrow scrolling), virtualized large output and 20,000-entry/10,000-session lists (middle/end/reflow/identity)"
                );
                return iced::exit();
            }
        }
        delayed_capture()
    }
}
impl Drop for App {
    fn drop(&mut self) {
        if let Some(h) = &self.harness {
            h.stop_all();
        }
        if let Some(cancel) = &self.login_cancel {
            cancel.cancel();
        }
        if let Some(path) = &self.smoke_dir {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}
// Budget against the center column, not the window (both sidebars can resize).
struct FooterLayout {
    model_width: f32,
    effort_width: f32,
    path_width: f32,
    git: bool,
    usage: bool,
    rate: bool,
    cache: bool,
}
impl FooterLayout {
    fn new(width: f32, model: &str, effort: &str) -> Self {
        let model_width =
            (footer_text_width(model, 11.).ceil() + 8.).min((width * 0.3).clamp(40., 180.));
        let effort_width =
            (footer_text_width(effort, 11.).ceil() + 8.).min((width * 0.2).clamp(28., 110.));
        let available = width - model_width - effort_width - 88.;
        let usage = available >= 260.;
        let git = available >= 420.;
        let rate = available >= 500.;
        let cache = available >= 580.;
        let path_width = available
            - if usage { 158. } else { 0. }
            - if git { 142. } else { 0. }
            - if rate { 76. } else { 0. }
            - if cache { 76. } else { 0. };
        Self {
            model_width,
            effort_width,
            path_width,
            git,
            usage,
            rate,
            cache,
        }
    }
}
/// Format token counts with at most three digits, rounding half up.
fn compact_tokens(tokens: u64) -> String {
    const SUFFIXES: [&str; 7] = ["", "k", "M", "B", "T", "Q", "Qi"];
    if tokens < 1000 {
        return tokens.to_string();
    }

    let mut unit = 1;
    let mut scale = 1000;
    while tokens / scale >= 1000 {
        unit += 1;
        scale *= 1000;
    }
    let mut decimals = match tokens / scale {
        0..=9 => 2,
        10..=99 => 1,
        _ => 0,
    };
    // Integer arithmetic preserves exact rounding even for large u64 counts.
    let step = scale / 10_u64.pow(decimals);
    let mut rounded = tokens / step + u64::from(tokens % step >= step / 2);
    if rounded == 1000 {
        if decimals == 0 {
            return format!("1.00{}", SUFFIXES[unit + 1]);
        }
        rounded /= 10;
        decimals -= 1;
    }
    let suffix = SUFFIXES[unit];
    if decimals == 0 {
        format!("{rounded}{suffix}")
    } else {
        let factor = 10_u64.pow(decimals);
        format!(
            "{}.{:0width$}{suffix}",
            rounded / factor,
            rounded % factor,
            width = decimals as usize,
        )
    }
}
// Match the label renderer so wide glyphs also get a full-value tooltip.
fn footer_text_width(value: &str, size: f32) -> f32 {
    use iced::advanced::text::{Paragraph, Renderer};
    <iced::Renderer as Renderer>::Paragraph::with_text(iced::advanced::Text {
        content: value,
        bounds: iced::Size::INFINITE,
        size: size.into(),
        line_height: text::LineHeight::Absolute((size * 1.3).round().into()),
        font: iced::Font::DEFAULT,
        align_x: Default::default(),
        align_y: iced::alignment::Vertical::Top,
        shaping: text::Shaping::Basic,
        wrapping: text::Wrapping::None,
    })
    .min_width()
}
fn footer_separator() -> Element<'static, Message> {
    text("·").size(11).color(MUTED).width(8).center().into()
}
fn footer_tip<'a>(
    content: impl Into<Element<'a, Message>>,
    details: String,
) -> Element<'a, Message> {
    tooltip(
        content,
        container(text(details).size(12)).max_width(440),
        tooltip::Position::Top,
    )
    .gap(6)
    .padding(8)
    .style(footer_surface)
    .into()
}
fn right_panel_header(title: &str, on_close: Message) -> Element<'_, Message> {
    row![
        text(title).size(22).width(Fill),
        theme::button("×")
            .on_press(on_close)
            .style(theme::text_button),
    ]
    .align_y(iced::Center)
    .into()
}

fn bottom() -> Task<Message> {
    widget::operation::snap_to("chat", scrollable::RelativeOffset::END)
}
fn delayed_capture() -> Task<Message> {
    Task::perform(
        async { tokio::time::sleep(Duration::from_millis(250)).await },
        |_| Message::Capture,
    )
}
fn project_badge(path: &Path) -> Element<'static, Message> {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let color = project_color(path);
    container(
        text(project_initials(&name))
            .size(8)
            .line_height(text::LineHeight::Absolute(10.into()))
            .color(color),
    )
    .center_x(16)
    .center_y(16)
    .style(move |_| container::Style {
        background: Some(
            Color {
                a: theme::BADGE_ALPHA,
                ..color
            }
            .into(),
        ),
        border: iced::Border {
            radius: theme::RADIUS_SMALL.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

fn project_initials(name: &str) -> String {
    let mut words = name
        .split(|ch: char| ch.is_whitespace() || ch == '-' || ch == '_')
        .filter(|word| !word.is_empty());
    let Some(first) = words.next() else {
        return String::new();
    };
    let mut letters = first.graphemes(true);
    let start = letters.next().expect("nonempty word");
    let end = words
        .next()
        .map(|word| word.graphemes(true).next().expect("nonempty word"))
        .or_else(|| letters.next_back());
    std::iter::once(start)
        .chain(end)
        .map(str::to_uppercase)
        .collect()
}

fn project_color(path: &Path) -> Color {
    // Fixed FNV-1a keeps the same path's color stable across runs.
    let hash = path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    theme::PROJECT_COLORS[(hash % theme::PROJECT_COLORS.len() as u64) as usize]
}

fn age(updated: u64) -> String {
    let seconds = pico_core::store::now().saturating_sub(updated);
    if seconds < 60 {
        "now".into()
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}
fn external(url: String) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move||std::process::Command::new("xdg-open").arg(&url).status().map_err(|e|format!("Could not open browser: {e}. Copy the login URL from Settings and open it manually.")).and_then(|status|if status.success(){Ok(())}else{Err("Browser launch failed. Copy the login URL from Settings and open it manually.".into())})).await.map_err(|e|e.to_string())?
        },
        Message::External,
    )
}

fn model_options() -> Vec<String> {
    MODELS.iter().map(|m| m.id.to_owned()).collect()
}
fn shorten(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.into();
    }
    let prefix = limit / 3;
    let suffix = limit.saturating_sub(prefix + 1);
    format!(
        "{}…{}",
        value.chars().take(prefix).collect::<String>(),
        value
            .chars()
            .rev()
            .take(suffix)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>()
    )
}

#[derive(Clone)]
struct Updates(Arc<Harness>);
impl std::hash::Hash for Updates {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}
fn updates(data: &Updates) -> iced::futures::stream::BoxStream<'static, Message> {
    let mut changes = data.0.subscribe();
    changes.mark_changed(); // Covers mutations before the subscription was installed.
    Box::pin(iced::stream::channel(
        1,
        async move |mut output: iced::futures::channel::mpsc::Sender<Message>| {
            while changes.changed().await.is_ok() {
                tokio::time::sleep(Duration::from_millis(50)).await;
                changes.borrow_and_update();
                if output.send(Message::Changed).await.is_err() {
                    break;
                }
            }
        },
    ))
}

fn composer_key_binding(event: text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    if matches!(event.status, text_editor::Status::Focused { .. }) {
        if (event.modifiers.command() && event.key.as_ref() == keyboard::Key::Character("v"))
            || (event.modifiers.shift()
                && event.key == keyboard::Key::Named(keyboard::key::Named::Insert))
        {
            return Some(text_editor::Binding::Custom(Message::Paste));
        }
        if event.key == keyboard::Key::Named(keyboard::key::Named::Backspace)
            && event.modifiers.control()
            && !event.modifiers.alt()
            && !event.modifiers.logo()
        {
            return Some(text_editor::Binding::Custom(Message::DeletePreviousWord));
        }
        if event.key == keyboard::Key::Named(keyboard::key::Named::Enter)
            && !event.modifiers.shift()
        {
            return Some(text_editor::Binding::Custom(Message::Send));
        }
    }
    text_editor::Binding::from_key_press(event)
}

fn tool_reference(id: &str, parent: Option<&str>) -> String {
    match parent {
        Some(parent) => format!("Code execution: {parent}"),
        None => format!("Call: {id}"),
    }
}

// Filter before producing application messages: even a no-op update rebuilds the UI.
fn window_events() -> Subscription<Message> {
    iced::event::listen_with(|event, _, _| match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed { .. })
        | iced::Event::Window(
            window::Event::Resized(_)
            | window::Event::Focused
            | window::Event::Unfocused
            | window::Event::CloseRequested
            | window::Event::FileDropped(_),
        ) => Some(Message::Event(event)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    mod sqlite {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../pico-core/src/test_support/sqlite.rs"
        ));
    }
    #[test]
    fn token_counts_use_three_digits_and_promote_rounded_units() {
        for (tokens, expected) in [
            (0, "0"),
            (9, "9"),
            (121, "121"),
            (999, "999"),
            (1000, "1.00k"),
            (1201, "1.20k"),
            (22_600, "22.6k"),
            (754_600, "755k"),
            (4_299_000, "4.30M"),
            (100_000, "100k"),
            (9_994, "9.99k"),
            (9_995, "10.0k"),
            (9_999, "10.0k"),
            (99_949, "99.9k"),
            (99_950, "100k"),
            (999_499, "999k"),
            (999_500, "1.00M"),
            (999_500_000, "1.00B"),
            (1_234_000_000_000, "1.23T"),
            (1_235_000_000_000_000, "1.24Q"),
            (999_500_000_000_000_000, "1.00Qi"),
            (u64::MAX, "18.4Qi"),
        ] {
            assert_eq!(compact_tokens(tokens), expected, "{tokens}");
        }
    }

    fn app() -> App {
        App::new(true, Arc::new(AtomicBool::new(false))).0
    }
    #[tokio::test]
    async fn user_messages_render_and_copy_literally_after_newline_and_paste() {
        use iced::advanced::widget::Operation;
        use iced_runtime::{UserInterface, user_interface::Cache};

        #[derive(Default)]
        struct TextContent(Vec<String>);
        impl Operation for TextContent {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn text(
                &mut self,
                _: Option<&iced::advanced::widget::Id>,
                _: iced::Rectangle,
                text: &str,
            ) {
                self.0.push(text.to_owned());
            }
        }

        let mut app = app();
        let root = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, root).await;
        type_message(&mut app, "First line λ").await;
        dispatch(
            &mut app,
            Message::Editor(text_editor::Action::Edit(text_editor::Edit::Enter)),
        )
        .await;
        // Deliver text through the native clipboard completion path.
        app.importing = 1;
        app.io_busy = true;
        dispatch(
            &mut app,
            Message::AttachmentLoaded(Ok(crate::attachments::Input::Text(
                "**literal**\n\n  indented\n![not an image](missing.png)".into(),
            ))),
        )
        .await;
        let expected = "First line λ\n**literal**\n\n  indented\n![not an image](missing.png)";
        assert_eq!(app.editor.text(), expected);
        dispatch(&mut app, Message::Send).await;
        let id = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        h.edit(&id, |s| {
            s.entries
                .push(Entry::new(EntryKind::Assistant, "**Still Markdown**"));
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        let saved = h.store.load_session(&id).unwrap();
        assert_eq!(saved.entries[0].text, expected);

        let mut renderer = crate::test_renderer::new(16.);
        let assistant = saved
            .entries
            .iter()
            .position(|e| matches!(e.kind, EntryKind::Assistant))
            .unwrap();
        for (index, expected_text) in [(0, expected), (assistant, "Still Markdown")] {
            let mut ui = UserInterface::build(
                app.entry_view(&saved.entries[index], index),
                iced::Size::new(800., 600.),
                Cache::new(),
                &mut renderer,
            );
            let mut text = TextContent::default();
            ui.operate(&renderer, &mut text);
            assert_eq!(text.0.concat(), expected_text);
        }

        // Copy must match the literal display, even after its row is unmounted.
        app.transcript.viewport = 1.;
        app.transcript.measure(assistant, 2000.);
        app.transcript.end();
        app.prepare_visible();
        assert!(!app.transcript.order[app.transcript.visible()].contains(&0));
        {
            let mut selection = app.chat_selection.borrow_mut();
            selection.anchor = Some(Position {
                entry: 0,
                block: 0,
                byte: 0,
            });
            selection.focus = Some(Position {
                entry: 0,
                block: 0,
                byte: expected.len(),
            });
        }
        assert_eq!(
            copied_text(app.update_inner(Message::CopySelection)).await,
            expected
        );
    }

    #[tokio::test]
    async fn activity_disclosure_swaps_on_header_hover_and_completion() {
        use iced::advanced::Renderer as _;
        use iced_runtime::{UserInterface, user_interface::Cache};
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path).await;
        let id = app.selected.clone().unwrap();
        let harness = app.harness.as_ref().unwrap().clone();
        let mut renderer = header_renderer();
        for width in [360., 860.] {
            let mut frames = Vec::new();
            let mut cache = Cache::new();
            // Sample activity over time without specifying its exact waveform/period.
            for tick in 0..16 {
                app.tick = tick;
                let mut icons = Vec::new();
                for open in [false, true] {
                    for complete in [false, true] {
                        harness
                            .edit(&id, |s| {
                                s.entries = vec![Entry {
                                    id: "group".into(),
                                    text: String::new(),
                                    kind: EntryKind::Tool {
                                        name: "exec_command".into(),
                                        input: r#"{"cmd":"sleep 1"}"#.into(),
                                        output: String::new(),
                                        complete,
                                        failed: false,
                                        cell_id: None,
                                        parent: None,
                                        media: vec![],
                                        web: None,
                                    },
                                }]
                                .into();
                            })
                            .unwrap();
                        dispatch(&mut app, Message::Changed).await;
                        if open {
                            app.groups.insert("group".into());
                        } else {
                            app.groups.remove("group");
                        }
                        let size = iced::Size::new(width, 100.);
                        let mut ui = UserInterface::build(
                            app.activity_row(ActivityRow::Activity(0)),
                            size,
                            cache,
                            &mut renderer,
                        );
                        let mut bounds = UiBounds::default();
                        ui.operate(&renderer, &mut bounds);
                        // Compare only the disclosure area before the first label,
                        // at the actual header height, not a fixed icon-size crop.
                        let icon_width = bounds
                            .labels
                            .iter()
                            .map(|(_, b)| b.x)
                            .min_by(f32::total_cmp)
                            .unwrap()
                            .floor() as usize;
                        let header_height = bounds
                            .containers
                            .iter()
                            .map(|b| b.y + b.height)
                            .max_by(f32::total_cmp)
                            .unwrap()
                            .ceil() as usize;
                        for cursor in [
                            mouse::Cursor::Unavailable,
                            // Hover the far edge, including the header's padding, not the icon.
                            mouse::Cursor::Available(iced::Point::new(width - 1., 1.)),
                            mouse::Cursor::Unavailable,
                        ] {
                            let mut messages = Vec::new();
                            ui.update(
                                &[iced::Event::Mouse(mouse::Event::CursorMoved {
                                    position: cursor
                                        .position()
                                        .unwrap_or(iced::Point::new(width + 1., 1.)),
                                })],
                                cursor,
                                &mut renderer,
                                &mut iced::advanced::clipboard::Null,
                                &mut messages,
                            );
                            assert!(messages.is_empty(), "hover must remain widget-local");
                            renderer.reset(iced::Rectangle::with_size(size));
                            ui.draw(
                                &mut renderer,
                                &theme::app(),
                                &iced::advanced::renderer::Style {
                                    text_color: theme::TEXT,
                                },
                                cursor,
                            );
                            let pixels = crate::test_renderer::screenshot(
                                &mut renderer,
                                iced::Size::new(width as u32, size.height as u32),
                                1.,
                                theme::BACKGROUND,
                            );
                            icons.push(
                                pixels
                                    .chunks_exact(width as usize * 4)
                                    .take(header_height)
                                    .flat_map(|row| row[..icon_width * 4].iter().copied())
                                    .collect::<Vec<_>>(),
                            );
                        }
                        assert!(matches!(
                            click_at(&mut ui, &mut renderer, iced::Point::new(width - 1., 1.)).as_slice(),
                            [Message::ToggleGroup(id)] if id == "group"
                        ));
                        cache = ui.into_cache();
                    }
                }
                for states in icons.as_chunks::<6>().0 {
                    assert_ne!(states[0], states[1], "working circle must become a chevron");
                    assert_eq!(states[0], states[2], "leaving restores the circle");
                    assert_eq!(states[1], states[3], "completion shows the same chevron");
                    assert_eq!(
                        states[3], states[4],
                        "completed hover does not change the icon"
                    );
                    assert_eq!(states[3], states[5]);
                }
                assert_ne!(icons[3], icons[9], "open and closed chevrons differ");
                frames.push(icons);
            }
            for index in [0, 2, 6, 8] {
                assert!(
                    frames.iter().any(|frame| frame[index] != frames[0][index]),
                    "working circle animates"
                );
            }
            for index in [1, 3, 4, 5, 7, 9, 10, 11] {
                assert!(
                    frames.iter().all(|frame| frame[index] == frames[0][index]),
                    "hovered and completed chevrons must not animate"
                );
            }
        }
    }

    #[test]
    fn tool_header_stays_fixed_above_padded_details_when_toggled() {
        use iced_runtime::{UserInterface, user_interface::Cache};
        let mut app = app();
        let entry = Entry {
            id: "stationary-tool".into(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: "exec_command".into(),
                input: r#"{"cmd":"printf 'λ'"}"#.into(),
                output: "λ".into(),
                complete: true,
                failed: false,
                cell_id: None,
                parent: Some("parent-call".into()),
                media: vec![],
                web: None,
            },
        };
        let mut renderer = header_renderer();
        for width in [360., 860.] {
            let mut cache = Cache::new();
            let mut header = None;
            for open in [false, true, false] {
                if open {
                    app.expanded.insert(entry.id.clone());
                    app.outputs.insert(
                        entry.id.clone(),
                        tool_output::Output::new(
                            "exec_command",
                            r#"{"cmd":"printf 'λ'"}"#,
                            "λ",
                            false,
                        ),
                    );
                } else {
                    app.expanded.remove(&entry.id);
                    app.outputs.remove(&entry.id);
                }
                let mut ui = UserInterface::build(
                    app.entry_view(&entry, 0),
                    iced::Size::new(width, 600.),
                    cache,
                    &mut renderer,
                );
                let mut bounds = UiBounds::default();
                ui.operate(&renderer, &mut bounds);
                let current = (bounds.label("exec_command"), bounds.label("●"));
                if let Some(header) = header {
                    assert_eq!(
                        current, header,
                        "expansion must not shift or resize the header"
                    );
                } else {
                    header = Some(current);
                }
                if open {
                    assert!(bounds.label("Command").y > current.0.y + current.0.height);
                    assert!(
                        bounds.label("Code execution: parent-call").y > bounds.label("Output").y
                    );
                } else {
                    assert!(!bounds.labels.iter().any(|(label, _)| label == "Command"));
                }
                // The original header remains the collapse/expand hit target.
                assert!(
                    matches!(click_at(&mut ui, &mut renderer, current.0.center()).as_slice(), [Message::ToggleEntry(id)] if id == &entry.id)
                );
                cache = ui.into_cache();
            }
        }
    }

    #[tokio::test]
    async fn formatted_tool_copy_and_raw_disclosure_survive_viewport_eviction() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path).await;
        let id = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        let input = r#"{ "cmd": "printf 'λ\n'", "workdir": "/tmp/a b" }"#;
        let output = "  result β\tend\r\n";
        h.edit(&id, |s| {
            s.entries = vec![
                Entry {
                    id: "formatted-tool".into(),
                    text: String::new(),
                    kind: EntryKind::Tool {
                        name: "exec_command".into(),
                        input: input.into(),
                        output: output.into(),
                        complete: true,
                        failed: false,
                        cell_id: None,
                        parent: Some("outer-call".into()),
                        media: vec![],
                        web: None,
                    },
                },
                Entry::new(EntryKind::Assistant, "After the tool."),
            ]
            .into();
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        dispatch(&mut app, Message::ToggleEntry("formatted-tool".into())).await;
        let formatted =
            "Command\nprintf 'λ\n'\n\nArguments\nworkdir: /tmp/a b\n\nOutput\n  result β\tend\r\n";
        for raw in [false, true, false] {
            if app.raw_tool_inputs.contains("formatted-tool") != raw {
                dispatch(&mut app, Message::ToggleToolRaw("formatted-tool".into())).await;
                assert!(
                    app.chat_selection.borrow().range().is_none(),
                    "format changes invalidate old byte positions"
                );
            }
            let expected = if raw {
                format!("Raw input\n{input}\n\n{formatted}")
            } else {
                formatted.into()
            };
            // Evict the rendered tool cache: Copy must reconstruct the same document.
            app.transcript.viewport = 1.;
            app.transcript.measure(1, 2000.);
            app.transcript.end();
            app.prepare_visible();
            assert!(!app.outputs.contains_key("formatted-tool"));
            {
                let mut selection = app.chat_selection.borrow_mut();
                selection.anchor = Some(Position {
                    entry: 0,
                    block: 0,
                    byte: 0,
                });
                selection.focus = Some(Position {
                    entry: 0,
                    block: 1,
                    byte: "Code execution: outer-call".len(),
                });
            }
            assert_eq!(
                copied_text(app.update_inner(Message::CopySelection)).await,
                format!("{expected}\n\nCode execution: outer-call")
            );
        }
        let saved = h.store.load_session(&id).unwrap();
        assert!(
            matches!(&saved.entries[0].kind, EntryKind::Tool { input: original, output: result, .. } if original == input && result == output)
        );
    }

    #[tokio::test]
    async fn activity_expansion_streaming_selection_and_details_preserve_source_entries() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path).await;
        let id = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        h.edit(&id, |s| {
            s.entries = vec![
                Entry {
                    id: "r1".into(),
                    kind: EntryKind::Reasoning,
                    text: "**First title**\n\nFirst full body λ".into(),
                },
                Entry {
                    id: "r2".into(),
                    kind: EntryKind::Reasoning,
                    text: "**Second title**\n\nSecond full body β".into(),
                },
                Entry {
                    id: "outer".into(),
                    text: String::new(),
                    kind: EntryKind::Tool {
                        name: "exec".into(),
                        input: "await tools.exec_command({cmd:'exit 7'})".into(),
                        output: "Script completed".into(),
                        complete: true,
                        failed: false,
                        cell_id: None,
                        parent: None,
                        media: vec![],
                        web: None,
                    },
                },
                Entry {
                    id: "shell".into(),
                    text: String::new(),
                    kind: EntryKind::Tool {
                        name: "exec_command".into(),
                        input: r#"{"cmd":"exit 7"}"#.into(),
                        output: String::new(),
                        complete: false,
                        failed: false,
                        cell_id: None,
                        parent: Some("outer".into()),
                        media: vec![],
                        web: None,
                    },
                },
                Entry::new(EntryKind::Assistant, "I will examine the result."),
            ]
            .into();
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        let collapsed = chat_text(&app);
        assert!(collapsed.contains("I will examine the result."));
        assert!(
            !collapsed.contains("First full body λ") && !collapsed.contains("Second full body β")
        );
        dispatch(&mut app, Message::ToggleGroup("r1".into())).await;
        dispatch(&mut app, Message::ToggleEntry("r1".into())).await;
        let expanded = chat_text(&app);
        assert!(expanded.contains("First full body λ") && expanded.contains("Second full body β"));
        assert!(
            !expanded.contains("await tools.exec_command"),
            "successful wrapper is secondary"
        );
        {
            let mut selection = app.chat_selection.borrow_mut();
            selection.anchor = Some(Position {
                entry: 0,
                block: 0,
                byte: 0,
            });
            selection.focus = Some(Position {
                entry: 1,
                block: 1,
                byte: "Second full body β".len(),
            });
        }
        let copied = copied_text(app.update_inner(Message::CopySelection)).await;
        assert!(copied.contains("First full body λ") && copied.contains("Second full body β"));
        dispatch(&mut app, Message::ToggleEntry("shell".into())).await;
        assert!(chat_text(&app).contains("Waiting for output"));
        h.edit(&id, |s| {
            if let EntryKind::Tool {
                output,
                complete,
                failed,
                ..
            } = &mut s.entries[3].kind
            {
                *output = "Process exited with code 7\ncomplete output β".into();
                *complete = true;
                *failed = true;
            }
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        assert!(
            chat_text(&app).contains("complete output β"),
            "streaming must preserve expanded details"
        );
        assert_eq!(app.activity.block(0).summary.as_ref().unwrap().failed, 1);
        dispatch(&mut app, Message::ToggleExecution("r1".into())).await;
        assert!(chat_text(&app).contains("await tools.exec_command"));
        dispatch(&mut app, Message::ToggleGroup("r1".into())).await;
        let collapsed = chat_text(&app);
        assert!(
            !collapsed.contains("First full body λ") && !collapsed.contains("complete output β")
        );
        assert!(collapsed.contains("I will examine the result."));
        assert!(app.outputs.is_empty());
        assert!(app.chat_selection.borrow().range().is_none());
        dispatch(&mut app, Message::ToggleGroup("r1".into())).await;
        assert!(
            chat_text(&app).contains("complete output β"),
            "manual detail state survives group collapse"
        );
        let saved = h.store.load_session(&id).unwrap();
        assert_eq!(saved.entries.len(), 5);
        assert!(
            matches!(&saved.entries[3].kind, EntryKind::Tool { failed: true, parent: Some(parent), output, .. } if parent == "outer" && output.ends_with("complete output β"))
        );
    }

    #[tokio::test]
    async fn web_settings_and_transcript_details_survive_save_cancel_and_reopen() {
        use pico_core::web_search::{Metadata, Mode, Source};
        let mut app = app();
        dispatch(&mut app, Message::ToggleSettings).await;
        dispatch(&mut app, Message::SettingsTab(SettingsTab::General)).await;
        assert_eq!(app.settings.web_search, Mode::Live);
        dispatch(&mut app, Message::WebMode(Mode::Cached)).await;
        dispatch(&mut app, Message::ApplySettings).await;
        assert_eq!(
            app.harness
                .as_ref()
                .unwrap()
                .store
                .load()
                .unwrap()
                .0
                .settings
                .web_search,
            Mode::Cached
        );
        dispatch(&mut app, Message::WebMode(Mode::Disabled)).await;
        dispatch(&mut app, Message::CancelSettings).await;
        assert_eq!(app.settings.web_search, Mode::Cached);
        let root = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, root).await;
        let id = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        let metadata = Metadata {
            mode: Mode::Cached,
            searched_at: 123,
            sources: vec![Source {
                title: "Official source".into(),
                url: "https://example.org/docs".into(),
            }],
            details: r#"[{"new_field":"complete UI-only metadata"}]"#.into(),
        };
        h.edit(&id, |s| {
            s.entries.push(Entry {
                id: "web-result".into(),
                text: String::new(),
                kind: EntryKind::Tool {
                    name: "web__run".into(),
                    input: "{}".into(),
                    output: "Agent text".into(),
                    complete: true,
                    failed: false,
                    cell_id: None,
                    parent: None,
                    media: vec![],
                    web: Some(metadata.clone()),
                },
            })
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        dispatch(&mut app, Message::ToggleEntry("web-result".into())).await;
        let _ = app.view();
        let loaded = h.store.load_session(&id).unwrap();
        assert!(
            matches!(&loaded.entries[0].kind,EntryKind::Tool{web:Some(actual),output,..} if actual==&metadata && output=="Agent text")
        );
        dispatch(&mut app, Message::Link(metadata.sources[0].url.clone())).await;
        assert!(matches!(&app.modal,Some(Modal::Link(url)) if url=="https://example.org/docs"));
        dispatch(&mut app, Message::Dismiss).await;
        dispatch(&mut app, Message::ToggleEntry("web-result".into())).await;
        assert!(!app.outputs.contains_key("web-result"));
    }

    #[tokio::test]
    async fn image_settings_credentials_and_modal_survive_real_ui_updates() {
        let mut app = app();
        dispatch(&mut app, Message::ToggleSettings).await;
        dispatch(&mut app, Message::SettingsTab(SettingsTab::Images)).await;
        dispatch(&mut app, Message::ImageModel("custom-image-model".into())).await;
        dispatch(
            &mut app,
            Message::ImageBackend(pico_core::image_tools::Backend::Api),
        )
        .await;
        dispatch(&mut app, Message::ImageQuality("high".into())).await;
        dispatch(&mut app, Message::ApplySettings).await;
        let saved = app.harness.as_ref().unwrap().store.load().unwrap().0;
        assert_eq!(saved.settings.images.model, "custom-image-model");
        assert_eq!(saved.settings.images.quality, "high");
        dispatch(&mut app, Message::ImageModel("discard-me".into())).await;
        dispatch(&mut app, Message::CancelSettings).await;
        assert_eq!(app.settings.images.model, "custom-image-model");
        dispatch(&mut app, Message::ApiKey("private-test-key".into())).await;
        dispatch(&mut app, Message::SaveApiKey).await;
        assert!(app.api_key_configured);
        assert!(app.api_key_draft.is_empty());
        assert_eq!(
            app.harness
                .as_ref()
                .unwrap()
                .store
                .api_key()
                .unwrap()
                .as_deref(),
            Some("private-test-key")
        );
        dispatch(&mut app, Message::RemoveApiKey).await;
        assert!(!app.api_key_configured);
        let root = app.smoke_dir.as_ref().unwrap().clone();
        let media = pico_core::media::MediaStore::new(root.join("test-media"))
            .image(include_bytes!("fixtures/media.gif"))
            .unwrap();
        stored_session(&mut app, root).await;
        let id = app.selected.clone().unwrap();
        let entry_id = pico_core::store::id();
        let h = app.harness.as_ref().unwrap().clone();
        h.edit(&id, |s| {
            s.entries.push(Entry {
                id: entry_id.clone(),
                text: String::new(),
                kind: EntryKind::Tool {
                    name: "image_gen__imagegen".into(),
                    input: "generate".into(),
                    output: media.description(),
                    complete: true,
                    failed: false,
                    cell_id: None,
                    parent: None,
                    media: vec![media.clone()],
                    web: None,
                },
            })
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        dispatch(&mut app, Message::ToggleEntry(entry_id)).await;
        dispatch(&mut app, Message::OpenImage(media.clone())).await;
        assert!(matches!(&app.modal,Some(Modal::Image(m)) if m==&media));
        let _ = app.view();
        dispatch(&mut app, Message::Image(crate::image_view::Message::Toggle)).await;
        let _ = app.view();
        dispatch(&mut app, Message::Dismiss).await;
        assert!(app.modal.is_none());
        let reloaded = h.store.load_session(&id).unwrap();
        assert!(
            matches!(&reloaded.entries[0].kind,EntryKind::Tool {media:m,..} if m==&vec![media])
        );
    }
    #[tokio::test]
    async fn markdown_images_load_in_chat_open_modal_and_retry_after_source_disappears() {
        let mut app = app();
        let root = app.smoke_dir.clone().unwrap();
        std::fs::write(root.join("chat.gif"), include_bytes!("fixtures/media.gif")).unwrap();
        stored_session(&mut app, root.clone()).await;
        let id = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        h.edit(&id, |s| {
            s.entries.push(Entry {
                id: "markdown-image".into(),
                text: "Before\n\n![Caption](chat.gif)\n\nAfter".into(),
                kind: EntryKind::Assistant,
            })
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        let loaded = app
            .markdown_images
            .loaded("chat.gif")
            .expect("visible image decoded");
        assert_eq!((loaded.width, loaded.height), (96, 64));
        dispatch(&mut app, Message::OpenMarkdownImage("chat.gif".into())).await;
        let Some(Modal::MarkdownImage(image)) = &app.modal else {
            panic!("image modal must open")
        };
        assert_eq!((image.width, image.height), (loaded.width, loaded.height));
        assert_eq!(image.bytes, loaded.bytes);
        let _ = app.view();
        dispatch(&mut app, Message::Image(crate::image_view::Message::Toggle)).await;
        dispatch(&mut app, Message::Dismiss).await;
        assert!(app.modal.is_none());
        std::fs::remove_file(root.join("chat.gif")).unwrap();
        stored_session(&mut app, root.clone()).await;
        assert!(app.markdown_images.loaded("chat.gif").is_none());
        dispatch(&mut app, Message::Select(id.clone())).await;
        assert!(
            app.markdown_images.loaded("chat.gif").is_none(),
            "reopening must read the source, not a saved copy"
        );
        std::fs::write(root.join("chat.gif"), include_bytes!("fixtures/media.gif")).unwrap();
        dispatch(&mut app, Message::RetryMarkdownImage("chat.gif".into())).await;
        assert!(app.markdown_images.loaded("chat.gif").is_some());
        let saved = h.store.load_session(&id).unwrap();
        assert_eq!(
            saved.entries[0].text,
            "Before\n\n![Caption](chat.gif)\n\nAfter"
        );
        assert!(matches!(saved.entries[0].kind, EntryKind::Assistant));
    }
    #[tokio::test]
    async fn late_core_notifications_do_not_replace_rendered_smoke_fixtures() {
        let mut app = app();
        app.smoke_stage = 26;
        let request = pico_core::questionnaire::Request::parse(
            r#"{"questions":[{"id":"q","question":"fixture","kind":"text"}]}"#,
        )
        .unwrap();
        app.questionnaires.insert(
            "smoke-questionnaire".into(),
            questionnaire::Draft::new(pico_core::questionnaire::Pending {
                id: "smoke-questionnaire".into(),
                request: Arc::new(request),
            }),
        );
        app.active_questionnaire = Some("smoke-questionnaire".into());
        dispatch(&mut app, Message::Changed).await;
        assert!(app.questionnaires.contains_key("smoke-questionnaire"));
        assert_eq!(
            app.active_questionnaire.as_deref(),
            Some("smoke-questionnaire")
        );
    }
    async fn stored_session(app: &mut App, path: PathBuf) {
        let session = Session::new(path, &app.catalog.settings);
        let id = session.id.clone();
        let h = app.harness.as_ref().unwrap();
        h.store.save_session(&session).unwrap();
        h.insert(session);
        dispatch(app, Message::Select(id)).await;
    }
    async fn dispatch(app: &mut App, message: Message) {
        let task = app.update(message);
        drain(app, task).await;
    }
    async fn drain(app: &mut App, task: Task<Message>) {
        use iced::futures::StreamExt;
        let mut pending = std::collections::VecDeque::from([task]);
        while let Some(task) = pending.pop_front() {
            if let Some(mut stream) = iced_runtime::task::into_stream(task) {
                while let Some(action) = stream.next().await {
                    if let iced_runtime::Action::Output(message) = action {
                        pending.push_back(app.update(message));
                    }
                }
            }
        }
    }
    async fn copied_text(task: Task<Message>) -> String {
        use iced::futures::StreamExt;
        let mut stream = iced_runtime::task::into_stream(task).expect("copy action");
        let mut copied = None;
        while let Some(action) = stream.next().await {
            if let iced_runtime::Action::Clipboard(iced_runtime::clipboard::Action::Write {
                target,
                contents,
            }) = action
            {
                assert_eq!(target, iced::advanced::clipboard::Kind::Standard);
                assert!(
                    copied.replace(contents).is_none(),
                    "one clipboard write per copy"
                );
            }
        }
        copied.expect("clipboard write")
    }

    async fn type_message(app: &mut App, message: &str) {
        dispatch(
            app,
            Message::Editor(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new(message.to_owned()),
            ))),
        )
        .await;
    }

    #[tokio::test]
    async fn sidebar_status_smoke_rows_are_visible_and_do_not_overlap() {
        use iced_runtime::{UserInterface, user_interface::Cache};
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path).await;
        let mut renderer = header_renderer();
        let cache = UserInterface::build(
            app.sidebar(),
            iced::Size::new(346., 600.),
            Cache::new(),
            &mut renderer,
        )
        .into_cache();
        app.smoke_stage = 45;
        let _ = app.smoke_frame(window::Screenshot::new(
            vec![0, 0, 0, 255, 1, 1, 1, 255],
            iced::Size::new(2, 1),
            1.,
        ));
        let mut ui = UserInterface::build(
            app.sidebar(),
            iced::Size::new(346., 600.),
            cache,
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let mut bottom = 0.;
        for session in &app.sessions {
            let title = bounds.label(&session.title);
            assert!(title.width > 0. && title.height > 0.);
            assert!(
                title.y >= bottom && title.y + title.height <= 600.,
                "status rows must be separate and visible: {title:?}"
            );
            bottom = title.y + title.height;
        }
    }

    #[tokio::test]
    async fn completion_dot_clears_only_when_its_session_is_opened() {
        use iced_runtime::{UserInterface, user_interface::Cache};
        use pico_core::agent::subagents::{Child, Profile};
        let mut renderer = header_renderer();
        let mut has_age_label = |app: &App| {
            let mut ui = UserInterface::build(
                app.sidebar(),
                iced::Size::new(346., 400.),
                Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            bounds
                .labels
                .iter()
                .any(|(text, _)| text == &age(app.sessions[0].updated))
        };
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path.clone()).await;
        let parent = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        h.edit(&parent, |s| s.unchecked_completion = true).unwrap();
        dispatch(&mut app, Message::Changed).await;
        dispatch(
            &mut app,
            Message::Event(iced::Event::Window(window::Event::Focused)),
        )
        .await;
        assert_eq!(
            app.session_status(&app.sessions[0]),
            SessionStatus::Unchecked,
            "refresh/focus is not opening a session"
        );

        assert!(
            !has_age_label(&app),
            "the completion dot replaces the age label"
        );

        let mut child = Session::new(path, &app.catalog.settings);
        child.child = Some(Child {
            parent_id: parent.clone(),
            profile: Profile::Explore,
        });
        let child_id = child.id.clone();
        h.store.save_session(&child).unwrap();
        h.insert(child);
        dispatch(&mut app, Message::Select(child_id)).await;
        assert!(
            h.store.load_session(&parent).unwrap().unchecked_completion,
            "opening a child does not check its parent"
        );
        dispatch(
            &mut app,
            Message::Event(iced::Event::Window(window::Event::Unfocused)),
        )
        .await;
        dispatch(&mut app, Message::Select(parent.clone())).await;
        assert!(
            !h.store.load_session(&parent).unwrap().unchecked_completion,
            "opening clears and saves the dot, regardless of keyboard focus"
        );
        let summary = app.sessions.iter().find(|s| s.id == parent).unwrap();
        assert_eq!(app.session_status(summary), SessionStatus::Idle);
        assert!(
            has_age_label(&app),
            "opening the session restores its age label"
        );
        let restarted = Harness::new(h.store.clone(), h.store.load().unwrap().1).unwrap();
        assert!(
            !restarted
                .load(&parent)
                .unwrap()
                .snapshot()
                .unchecked_completion
        );
    }

    #[tokio::test]
    async fn stopped_attention_clears_on_escape_or_revisit_not_on_refresh() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path.clone()).await;
        let id = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        let fail = || {
            h.edit(&id, |s| {
                s.needs_attention = true;
                s.entries
                    .push(Entry::new(EntryKind::Notice, "Request flagged by provider"));
            })
            .unwrap();
            *h.get(&id).unwrap().status.lock().unwrap() = "Needs attention".into();
        };
        fail();
        dispatch(&mut app, Message::Changed).await;
        assert!(app.current().unwrap().needs_attention);
        assert!(
            app.sidebar_rows
                .iter()
                .any(|row| matches!(row, SidebarRow::Heading("NEEDS ATTENTION", 1)))
        );
        app.modal = Some(Modal::Rename(id.clone()));
        dispatch(&mut app, footer_key(keyboard::key::Named::Escape)).await;
        assert!(app.modal.is_none());
        assert!(h.get(&id).unwrap().snapshot().needs_attention);
        dispatch(&mut app, footer_key(keyboard::key::Named::Escape)).await;
        assert!(!app.current().unwrap().needs_attention);
        assert!(
            !app.sidebar_rows
                .iter()
                .any(|row| matches!(row, SidebarRow::Heading("NEEDS ATTENTION", _)))
        );
        assert!(!h.store.load_session(&id).unwrap().needs_attention);
        assert_eq!(
            app.current().unwrap().entries.iter().last().unwrap().text,
            "Request flagged by provider"
        );

        fail();
        dispatch(&mut app, Message::Changed).await;
        dispatch(&mut app, Message::Selected(Err("Selection failed".into()))).await;
        assert!(h.get(&id).unwrap().snapshot().needs_attention);
        stored_session(&mut app, path).await;
        assert!(h.store.load_session(&id).unwrap().needs_attention);
        dispatch(&mut app, Message::Select(id.clone())).await;
        assert!(!app.current().unwrap().needs_attention);
        assert!(!h.store.load_session(&id).unwrap().needs_attention);
        assert!(
            !app.sidebar_rows
                .iter()
                .any(|row| matches!(row, SidebarRow::Heading("NEEDS ATTENTION", _)))
        );
    }

    #[tokio::test]
    async fn questionnaires_preserve_session_drafts_queue_and_modal_priority() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path.clone()).await;
        type_message(&mut app, "Unsent composer draft").await;
        let owner = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        let service = h.get(&owner).unwrap().questionnaires.clone().unwrap();
        let mut tasks = vec![];
        let cancel = CancellationToken::new();
        for id in ["first-ask", "second-ask"] {
            let service = service.clone();
            let cancel = cancel.clone();
            tasks.push(tokio::spawn(async move {
                service
                    .ask(
                        id,
                        r#"{"questions":[{"id":"q","question":"Required answer?","kind":"text"}]}"#,
                        &cancel,
                    )
                    .await
            }));
            tokio::time::timeout(Duration::from_secs(2), async {
                while !app.questionnaires.contains_key(id) {
                    dispatch(&mut app, Message::Changed).await;
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        assert_eq!(app.active_questionnaire.as_deref(), Some("first-ask"));
        assert!(
            app.sidebar_rows
                .iter()
                .any(|row| matches!(row, SidebarRow::Heading("NEEDS ATTENTION", 1)))
        );
        let q = |id: &str, action| {
            Message::Questionnaire(
                id.into(),
                questionnaire::Message {
                    question: 0,
                    action,
                },
            )
        };
        dispatch(
            &mut app,
            q(
                "first-ask",
                questionnaire::Action::Editor(text_editor::Action::Edit(text_editor::Edit::Paste(
                    Arc::new("Answer draft λ".into()),
                ))),
            ),
        )
        .await;
        dispatch(&mut app, q("first-ask", questionnaire::Action::Toggle)).await;
        stored_session(&mut app, path).await;
        assert!(app.active_questionnaire.is_none());
        type_message(&mut app, "Another session's draft").await;
        dispatch(&mut app, Message::Select(owner.clone())).await;
        assert_eq!(app.editor.text(), "Unsent composer draft");
        assert!(app.questionnaires["first-ask"].collapsed);
        dispatch(&mut app, q("first-ask", questionnaire::Action::Toggle)).await;
        // The ordinary composer must neither mutate nor submit while an ask is open.
        type_message(&mut app, "Should not be pasted").await;
        dispatch(&mut app, Message::Send).await;
        assert_eq!(app.editor.text(), "Unsent composer draft");
        app.modal = Some(Modal::Rename(owner.clone()));
        dispatch(&mut app, q("first-ask", questionnaire::Action::Next)).await;
        assert_eq!(service.pending().len(), 2);
        dispatch(
            &mut app,
            Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            })),
        )
        .await;
        assert!(app.modal.is_none());
        assert_eq!(service.pending().len(), 2);
        dispatch(&mut app, q("first-ask", questionnaire::Action::Next)).await;
        assert_eq!(app.active_questionnaire.as_deref(), Some("second-ask"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&tasks.remove(0).await.unwrap().unwrap())
                .unwrap(),
            serde_json::json!({"answers":[{"id":"q","answer":"Answer draft λ"}]})
        );
        cancel.cancel();
        assert!(tasks.remove(0).await.unwrap().is_err());
        dispatch(&mut app, Message::Changed).await;
        assert!(app.active_questionnaire.is_none());
        assert!(app.questionnaires.is_empty());
        assert_eq!(app.editor.text(), "Unsent composer draft");
    }

    #[tokio::test]
    async fn dismissing_settings_discards_drafts_but_keeps_saved_values() {
        for dismiss in [
            Message::CancelSettings,
            Message::ToggleSettings,
            Message::ToggleAgents,
            footer_key(keyboard::key::Named::Escape),
        ] {
            let mut app = app();
            let model = "gpt-6-astra";
            // Exercise both the original default and a subsequently saved value.
            for saved_fast in [false, true] {
                dispatch(&mut app, Message::ToggleSettings).await;
                dispatch(&mut app, Message::ModelFast(model.into(), saved_fast)).await;
                dispatch(&mut app, Message::Context("128000".into())).await;
                dispatch(&mut app, Message::Threshold("75".into())).await;
                dispatch(&mut app, Message::ApplySettings).await;
                assert!(!app.settings_open);

                dispatch(&mut app, Message::ToggleSettings).await;
                dispatch(&mut app, Message::ModelFast(model.into(), !saved_fast)).await;
                dispatch(&mut app, Message::Context("invalid".into())).await;
                dispatch(&mut app, Message::Threshold("42".into())).await;
                dispatch(&mut app, Message::ApiKey("unsaved-key".into())).await;
                dispatch(&mut app, Message::Font(19.)).await;
                assert_eq!(app.catalog.settings.models[model].fast, saved_fast);

                dispatch(&mut app, dismiss.clone()).await;
                assert!(!app.settings_open);
                assert!(app.api_key_draft.is_empty());
                dispatch(&mut app, Message::ToggleSettings).await;
                assert!(app.settings_open);
                assert_eq!(app.settings.models[model].fast, saved_fast);
                assert_eq!(app.context_text, "128000");
                assert_eq!(app.threshold_text, "75");
                assert_eq!(app.settings.font_size, 19.);
                let saved = app.harness.as_ref().unwrap().store.load().unwrap().0;
                assert_eq!(saved.settings.models[model].fast, saved_fast);
                assert_eq!(saved.settings.font_size, 19.);
                dispatch(&mut app, Message::CancelSettings).await;
            }
        }
    }

    #[tokio::test]
    async fn save_closes_settings_only_after_success_and_autosave_keeps_it_open() {
        let mut app = app();
        app.settings_open = true;
        dispatch(&mut app, Message::Font(17.)).await;
        assert!(app.settings_open);
        dispatch(&mut app, Message::Context("invalid".into())).await;
        dispatch(&mut app, Message::ApplySettings).await;
        assert!(app.settings_open);
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("numeric context limit")
        );
        dispatch(&mut app, Message::Context("128000".into())).await;
        let settings_path = app
            .harness
            .as_ref()
            .unwrap()
            .store
            .root()
            .join("settings.json");
        std::fs::remove_file(&settings_path).unwrap();
        std::fs::create_dir(&settings_path).unwrap();
        dispatch(&mut app, Message::ApplySettings).await;
        assert!(app.settings_open);
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("Could not save settings")
        );
        std::fs::remove_dir(&settings_path).unwrap();
        dispatch(&mut app, Message::ErrorDismiss).await;
        let epoch = app.view_epoch;
        let save = app.update(Message::ApplySettings);
        assert!(app.settings_open, "wait for persistence before closing");
        drain(&mut app, save).await;
        assert!(!app.settings_open);
        assert_ne!(app.view_epoch, epoch);
        assert!(app.error.is_none());
        assert_eq!(
            app.harness
                .as_ref()
                .unwrap()
                .store
                .load()
                .unwrap()
                .0
                .settings
                .context_limit,
            128000
        );
    }

    #[tokio::test]
    async fn model_defaults_footer_fast_and_retired_session_recovery_persist() {
        let mut app = app();
        let h = app.harness.as_ref().unwrap().clone();
        dispatch(
            &mut app,
            Message::ModelEffort("gpt-6-luna".into(), "medium".into()),
        )
        .await;
        dispatch(&mut app, Message::ModelFast("gpt-6-luna".into(), false)).await;
        dispatch(
            &mut app,
            Message::ModelEffort("gpt-6-astra".into(), "max".into()),
        )
        .await;
        dispatch(&mut app, Message::ModelFast("gpt-6-astra".into(), true)).await;
        dispatch(&mut app, Message::Model("gpt-6-astra".into())).await;
        dispatch(&mut app, Message::ApplySettings).await;
        let (catalog, _, _) = h.store.load().unwrap();
        assert_eq!(catalog.settings.model, "gpt-6-astra");
        assert!(catalog.settings.models["gpt-6-astra"].fast);
        dispatch(&mut app, Message::Create(PathBuf::from("/models"))).await;
        assert_eq!(app.current().unwrap().effort, "max");
        assert!(app.current().unwrap().fast);
        dispatch(&mut app, Message::SessionModel("gpt-6-luna".into())).await;
        assert_eq!(app.current().unwrap().effort, "medium");
        assert!(!app.current().unwrap().fast);
        dispatch(&mut app, Message::FooterMenu(FooterControl::Effort)).await;
        assert!(
            app.footer_menu
                .as_ref()
                .unwrap()
                .options
                .contains(&"none".into())
        );
        let fast_index = app
            .footer_menu
            .as_ref()
            .unwrap()
            .options
            .iter()
            .position(|s| s == FAST_OPTION)
            .unwrap();
        app.footer_menu.as_mut().unwrap().focused = fast_index;
        dispatch(&mut app, footer_key(keyboard::key::Named::Enter)).await;
        assert!(app.current().unwrap().fast);
        assert_eq!(app.current().unwrap().effort, "medium");
        // The persisted session uses a retired model and must recover without losing history.
        let mut session = Session::new(PathBuf::from("/models"), &catalog.settings);
        session.model = "retired-sol".into();
        session.effort = "low".into();
        session.fast = false;
        session.entries.push(Entry::new(
            EntryKind::User { images: vec![] },
            "Keep this transcript",
        ));
        session.draft = "Keep this draft".into();
        let id = session.id.clone();
        h.store.save_session(&session).unwrap();
        dispatch(&mut app, Message::Select(id.clone())).await;
        let saved = h.store.load_session(&id).unwrap();
        assert_eq!(saved.model, "gpt-6-astra");
        assert_eq!(saved.effort, "max");
        assert!(saved.fast);
        assert_eq!(saved.entries[0].text, "Keep this transcript");
        assert_eq!(app.editor.text(), "Keep this draft");
        assert!(app.notice.as_ref().unwrap().0.contains("retired-sol"));
        dispatch(&mut app, Message::SessionEffort("high".into())).await;
        dispatch(&mut app, Message::ToggleSessionFast).await;
        assert!(!h.store.load_session(&id).unwrap().fast);
        dispatch(&mut app, Message::NoticeDismiss).await;
        dispatch(&mut app, Message::New).await;
        dispatch(&mut app, Message::Select(id)).await;
        assert!(app.notice.is_none());
        assert_eq!(app.current().unwrap().effort, "high");
        assert!(!app.current().unwrap().fast);
        dispatch(&mut app, Message::FooterMenu(FooterControl::Effort)).await;
        assert!(
            !app.footer_menu
                .as_ref()
                .unwrap()
                .options
                .contains(&"none".into())
        );
        dispatch(&mut app, Message::FooterMenu(FooterControl::Model)).await;
        assert_eq!(
            app.footer_menu.as_ref().unwrap().options,
            MODELS
                .iter()
                .map(|model| model.id.to_owned())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn retired_model_recovery_retries_failed_selection_without_losing_notice() {
        let mut app = app();
        let h = app.harness.as_ref().unwrap().clone();
        let defaults = app.catalog.settings.models[&app.catalog.settings.model].clone();
        let mut session = Session::new(PathBuf::from("/recovery"), &app.catalog.settings);
        session.model = "retired-model".into();
        session.effort = "low".into();
        session.fast = true;
        let id = session.id.clone();
        h.store.save_session(&session).unwrap();
        let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
        for table in ["new_session_draft", "sessions"] {
            sqlite::fail_writes(&db, table, "fixture failure");
            dispatch(&mut app, Message::Select(id.clone())).await;
            assert!(app.error.as_ref().unwrap().contains("fixture failure"));
            assert!(app.selected.is_none());
            assert!(app.notice.is_none());
            assert_eq!(h.store.load_session(&id).unwrap().model, "retired-model");
            assert_eq!(h.get(&id).unwrap().snapshot().model, "retired-model");
            sqlite::allow_writes(&db, table);
        }
        dispatch(&mut app, Message::ErrorDismiss).await;
        dispatch(&mut app, Message::Select(id.clone())).await;
        assert_eq!(app.selected.as_ref(), Some(&id));
        assert!(app.error.is_none());
        assert!(app.notice.as_ref().unwrap().0.contains("retired-model"));
        let saved = h.store.load_session(&id).unwrap();
        assert_eq!(saved.model, app.catalog.settings.model);
        assert_eq!(saved.effort, defaults.effort);
        assert_eq!(saved.fast, defaults.fast);
        // Deterministic expiry, without sleeping or an always-on timer.
        app.notice.as_mut().unwrap().1 = std::time::Instant::now() - NOTICE_DURATION;
        dispatch(&mut app, Message::NoticeTick).await;
        assert!(app.notice.is_none());
    }

    #[tokio::test]
    async fn image_drafts_survive_navigation_storage_and_failed_image_only_send() {
        let mut app = app();
        let root = app.smoke_dir.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        dispatch(&mut app, Message::Create(root.clone())).await;
        let source = root.join("pasted.png");
        std::fs::write(&source, include_bytes!("fixtures/media.png")).unwrap();
        dispatch(
            &mut app,
            Message::Event(iced::Event::Window(window::Event::FileDropped(
                source.clone(),
            ))),
        )
        .await;
        std::fs::remove_file(&source).unwrap();
        let images = app.attachments.clone();
        assert_eq!(images.len(), 1);
        assert!(
            images[0].path.is_file(),
            "attachment is independent of its source"
        );
        dispatch(&mut app, Message::SaveDraft).await;
        assert_eq!(h.store.load_new_draft().unwrap().images, images);
        stored_session(&mut app, root.clone()).await;
        let existing = app.selected.clone().unwrap();
        assert!(app.attachments.is_empty());
        dispatch(&mut app, Message::Create(root.clone())).await;
        assert_eq!(app.attachments, images);
        // Import completion follows the same path for clipboard image bytes.
        app.importing = 1;
        app.io_busy = true;
        dispatch(
            &mut app,
            Message::AttachmentLoaded(Ok(crate::attachments::Input::Image(images[0].clone()))),
        )
        .await;
        assert_eq!(app.attachments.len(), 2);
        dispatch(&mut app, Message::RemoveAttachment(0)).await;
        assert_eq!(app.attachments, images);
        dispatch(&mut app, Message::OpenImage(images[0].clone())).await;
        assert!(matches!(app.modal, Some(Modal::Image(_))));
        dispatch(&mut app, Message::RemoveAttachment(0)).await;
        assert_eq!(app.attachments, images, "modal blocks background edits");
        dispatch(&mut app, Message::Dismiss).await;
        let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
        sqlite::fail_writes(&db, "sessions", "image fixture failure");
        dispatch(&mut app, Message::Send).await;
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("image fixture failure")
        );
        assert_eq!(app.attachments, images);
        sqlite::allow_writes(&db, "sessions");
        dispatch(&mut app, Message::Send).await;
        assert!(app.attachments.is_empty());
        let saved = h
            .store
            .load_session(app.selected.as_ref().unwrap())
            .unwrap();
        assert_eq!(saved.entries[0].text, "");
        assert!(
            matches!(&saved.entries[0].kind, EntryKind::User { images: saved_images } if saved_images == &images)
        );
        assert!(h.store.load_new_draft().unwrap().images.is_empty());
        dispatch(&mut app, Message::Select(existing.clone())).await;
        app.importing = 1;
        app.io_busy = true;
        dispatch(
            &mut app,
            Message::AttachmentLoaded(Ok(crate::attachments::Input::Image(images[0].clone()))),
        )
        .await;
        dispatch(&mut app, Message::New).await;
        assert_eq!(
            h.store.load_session(&existing).unwrap().draft_images,
            images
        );
        dispatch(&mut app, Message::Select(existing)).await;
        assert_eq!(app.attachments, images);
        dispatch(&mut app, Message::RemoveAttachment(0)).await;
        dispatch(&mut app, Message::SaveDraft).await;
        assert!(
            h.store
                .load_session(app.selected.as_ref().unwrap())
                .unwrap()
                .draft_images
                .is_empty()
        );
    }

    #[tokio::test]
    async fn multiple_file_drops_are_ordered_and_cannot_leak_into_another_session() {
        let mut app = app();
        let root = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, root.clone()).await;
        let id = app.selected.clone();
        let png = root.join("first.png");
        let gif = root.join("second.gif");
        std::fs::write(&png, include_bytes!("fixtures/media.png")).unwrap();
        std::fs::write(&gif, include_bytes!("fixtures/media.gif")).unwrap();
        let first = app.update(Message::Event(iced::Event::Window(
            window::Event::FileDropped(png),
        )));
        let second = app.update(Message::Event(iced::Event::Window(
            window::Event::FileDropped(gif),
        )));
        assert_eq!(app.importing, 2);
        dispatch(&mut app, Message::Create(root.clone())).await;
        dispatch(&mut app, Message::Send).await;
        assert_eq!(app.selected, id);
        drain(&mut app, first).await;
        assert!(app.io_busy);
        drain(&mut app, second).await;
        assert!(!app.io_busy);
        assert_eq!(
            app.attachments
                .iter()
                .map(|m| m.mime.as_str())
                .collect::<Vec<_>>(),
            ["image/png", "image/gif"]
        );
        let invalid = root.join("not-an-image.png");
        std::fs::write(&invalid, b"invalid image bytes").unwrap();
        dispatch(
            &mut app,
            Message::Event(iced::Event::Window(window::Event::FileDropped(invalid))),
        )
        .await;
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("Could not attach image")
        );
        assert_eq!(app.attachments.len(), 2);
        assert!(!app.io_busy);
    }

    #[tokio::test]
    async fn new_session_draft_follows_projects_but_not_existing_sessions() {
        let mut app = app();
        let h = app.harness.as_ref().unwrap().clone();
        dispatch(&mut app, Message::Create(PathBuf::from("/project-one"))).await;
        dispatch(&mut app, Message::Send).await;
        assert!(app.selected.is_none());
        assert!(h.summaries().is_empty());
        assert!(h.store.load().unwrap().1.is_empty());

        let message = "Investigate λ
Then implement the fix.";
        type_message(&mut app, message).await;
        dispatch(&mut app, Message::SessionModel("gpt-6-astra".into())).await;
        dispatch(&mut app, Message::SessionEffort("high".into())).await;
        dispatch(&mut app, Message::Create(PathBuf::from("/project-two"))).await;
        assert_eq!(app.editor.text(), message);
        assert_eq!(app.current().unwrap().model, app.catalog.settings.model);
        assert_eq!(
            app.current().unwrap().effort,
            app.catalog.settings.models[&app.catalog.settings.model].effort
        );
        assert!(h.summaries().is_empty());
        assert!(h.store.load().unwrap().1.is_empty());

        // Leaving the draft and editing an existing conversation must not replace it.
        stored_session(&mut app, PathBuf::from("/existing")).await;
        let existing = app.selected.clone().unwrap();
        type_message(&mut app, "Existing conversation follow-up").await;
        dispatch(&mut app, Message::Create(PathBuf::from("/project-three"))).await;
        assert_eq!(app.editor.text(), message);
        dispatch(&mut app, Message::Select(existing)).await;
        assert_eq!(app.editor.text(), "Existing conversation follow-up");
        dispatch(&mut app, Message::New).await;
        assert!(app.current().is_none());
        dispatch(&mut app, Message::Create(PathBuf::from("/project-two"))).await;
        assert_eq!(app.editor.text(), message);
        dispatch(&mut app, Message::SaveDraft).await;
        assert_eq!(h.store.load_new_draft().unwrap(), app.new_draft);

        // First send alone creates a session, in the final chosen project.
        dispatch(&mut app, Message::SessionEffort("low".into())).await;
        dispatch(&mut app, Message::Send).await;
        let id = app.selected.as_ref().unwrap();
        let saved = h.store.load_session(id).unwrap();
        assert_eq!(saved.project, Path::new("/project-two"));
        assert_eq!(saved.effort, "low");
        let users: Vec<_> = saved
            .entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::User { .. }))
            .collect();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].text, message);
        assert_eq!(h.store.load().unwrap().1.len(), 2);
        assert_eq!(
            h.store.load_new_draft().unwrap(),
            NewSessionDraft::default()
        );
        assert!(app.editor.text().is_empty());
        dispatch(&mut app, Message::Create(PathBuf::from("/project-one"))).await;
        assert!(app.editor.text().is_empty());
    }

    #[tokio::test]
    async fn failed_shared_draft_save_and_first_send_keep_message_for_retry() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        dispatch(&mut app, Message::Create(path)).await;
        type_message(&mut app, "  \n").await;
        dispatch(&mut app, Message::Send).await;
        assert!(h.store.load().unwrap().1.is_empty());
        type_message(&mut app, "Do not lose this message").await;
        let message = app.editor.text();
        let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
        sqlite::fail_writes(&db, "new_session_draft", "fixture draft failure");
        dispatch(&mut app, Message::SaveDraft).await;
        assert!(app.new_draft_dirty);
        assert_eq!(app.editor.text(), message);
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("fixture draft failure")
        );
        sqlite::allow_writes(&db, "new_session_draft");
        dispatch(&mut app, Message::SaveDraft).await;
        assert!(!app.new_draft_dirty);
        assert_eq!(h.store.load_new_draft().unwrap().message, message);

        sqlite::fail_writes(&db, "sessions", "fixture session failure");
        dispatch(&mut app, Message::Send).await;
        assert!(app.selected.is_none());
        assert_eq!(app.editor.text(), message);
        assert!(h.summaries().is_empty());
        assert!(h.store.load().unwrap().1.is_empty());
        assert_eq!(h.store.load_new_draft().unwrap().message, message);
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("fixture session failure")
        );
        sqlite::allow_writes(&db, "sessions");
        dispatch(&mut app, Message::Send).await;
        assert!(app.selected.is_some());
        assert!(app.editor.text().is_empty());
        assert_eq!(h.store.load().unwrap().1.len(), 1);
        assert_eq!(
            h.store.load_new_draft().unwrap(),
            NewSessionDraft::default()
        );
    }

    #[tokio::test]
    async fn deleting_populated_session_then_opening_draft_clears_transcript() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path.clone()).await;
        let id = app.selected.clone().unwrap();
        app.harness
            .as_ref()
            .unwrap()
            .edit(&id, |s| {
                s.entries.push(Entry::new(
                    EntryKind::User { images: vec![] },
                    "Old message",
                ));
            })
            .unwrap();
        dispatch(&mut app, Message::Changed).await;
        assert!(!app.transcript.order.is_empty());
        dispatch(&mut app, Message::Delete(id)).await;
        dispatch(&mut app, Message::Confirm).await;
        dispatch(&mut app, Message::Create(path)).await;
        assert!(app.transcript.order.is_empty());
        assert!(app.current().unwrap().entries.is_empty());
        let _ = app.center();
    }

    #[tokio::test]
    async fn child_navigation_preserves_parent_draft_scroll_and_disables_child_edits() {
        use pico_core::agent::subagents::{Child, Profile, RunInfo, RunStatus};
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path.clone()).await;
        let parent = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        h.edit(&parent, |s| {
            s.title = "Parent task".into();
            for n in 0..60 {
                s.entries.push(Entry::new(
                    EntryKind::Assistant,
                    format!("Historical parent message {n}"),
                ));
            }
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        type_message(&mut app, "Keep this unsent parent draft λ").await;
        let epoch = app.view_epoch;
        dispatch(&mut app, Message::Scroll(epoch, 437., 300., false)).await;
        let parent_offset = app.transcript.offset;
        let mut child = Session::new(path, &app.catalog.settings);
        child.child = Some(Child {
            parent_id: parent.clone(),
            profile: Profile::Explore,
        });
        child.title = "Inspect cancellation".into();
        child.model = "gpt-6-luna".into();
        child.effort = "high".into();
        child.entries.push(Entry::new(
            EntryKind::User { images: vec![] },
            "Inspect without edits",
        ));
        child
            .entries
            .push(Entry::new(EntryKind::Assistant, "Final inspection result"));
        let child_id = child.id.clone();
        h.store.save_session(&child).unwrap();
        let finished = pico_core::store::now();
        let run = RunInfo {
            run_id: pico_core::store::id(),
            session_id: child_id.clone(),
            parent_id: parent.clone(),
            request_id: "fixture-spawn".into(),
            profile: Profile::Explore,
            title: child.title.clone(),
            status: RunStatus::Completed,
            assignment: 1,
            started: finished - 17,
            finished: Some(finished),
            notified: true,
            attention_acknowledged: false,
        };
        let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
        db.execute(
            "INSERT INTO subagent_runs (id,parent,session,request,metadata,result,error) VALUES (?1,?2,?3,?4,?5,?6,NULL)",
            rusqlite::params![
                run.run_id,
                parent,
                child_id,
                run.request_id,
                serde_json::to_string(&run).unwrap(),
                "Final inspection result"
            ],
        )
        .unwrap();
        app.harness = Some(Harness::new(h.store.clone(), h.store.load().unwrap().1).unwrap());
        app.catalog_revision = u64::MAX;
        dispatch(&mut app, Message::ToggleAgents).await;
        assert!(app.agents_open && !app.settings_open);
        // A terminal run must not display stale runtime activity. The entire
        // card, including its padding outside the labels, opens the child.
        *app.harness
            .as_ref()
            .unwrap()
            .load(&child_id)
            .unwrap()
            .status
            .lock()
            .unwrap() = "Thinking…".into();
        let mut renderer = header_renderer();
        let messages = {
            let mut ui = iced_runtime::UserInterface::build(
                app.agents_panel(),
                iced::Size::new(480., 560.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            assert!(
                !bounds.labels.iter().any(|(label, _)| {
                    matches!(label.as_str(), "Open" | "Stop" | "Thinking…")
                })
            );
            let profile = bounds.label("Explore");
            let title = bounds.label("Inspect cancellation");
            let card = bounds
                .containers
                .iter()
                .filter(|b| b.contains(profile.center()) && b.contains(title.center()))
                .min_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)))
                .unwrap();
            let padding = iced::Point::new((card.x + profile.x) / 2., (card.y + profile.y) / 2.);
            let mut messages = Vec::new();
            ui.update(
                &[
                    iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(padding),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            messages
        };
        assert!(matches!(messages.as_slice(), [Message::Select(id)] if id == &child_id));
        for message in messages {
            dispatch(&mut app, message).await;
        }
        assert_eq!(app.parent_id(), Some(parent.as_str()));
        assert!(
            app.sidebar_rows
                .iter()
                .filter_map(|row| match row {
                    SidebarRow::Session(i, _) => Some(&app.sessions[*i]),
                    _ => None,
                })
                .all(|s| s.child.is_none())
        );
        assert_eq!(
            app.harness
                .as_ref()
                .unwrap()
                .store
                .load_session(&parent)
                .unwrap()
                .draft,
            "Keep this unsent parent draft λ"
        );
        let original = app.current().unwrap().clone();
        type_message(&mut app, "Forbidden direct child edit").await;
        dispatch(&mut app, Message::SessionModel("gpt-6-astra".into())).await;
        dispatch(&mut app, Message::Send).await;
        assert!(app.editor.text().is_empty());
        assert_eq!(app.current().unwrap().model, original.model);
        assert_eq!(app.current().unwrap().entries.len(), original.entries.len());
        let mut renderer = header_renderer();
        for (width, height) in [(1200., 800.), (860., 560.)] {
            app.window_width = width;
            app.window_height = height;
            let mut ui = iced_runtime::UserInterface::build(
                app.view(),
                iced::Size::new(width, height),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            let profile = bounds.label("Explore");
            let status = bounds.label("Completed");
            let title = bounds.label("Inspect cancellation");
            let metadata = bounds.label("gpt-6-luna · high · 17s");
            assert!((profile.center_y() - status.center_y()).abs() <= 1.);
            assert!((title.center_y() - metadata.center_y()).abs() <= 1.);
            assert!(title.y > profile.y);
            assert!(title.x + title.width <= metadata.x);
            assert!((status.x + status.width - metadata.x - metadata.width).abs() <= 1.);
            for label in [
                "Agents",
                "Completed",
                "Explore",
                "Inspect cancellation",
                "■",
            ] {
                let rect = bounds.label(label);
                assert!(
                    rect.x >= 0. && rect.x + rect.width <= width,
                    "{label} clipped at {width}: {rect:?}"
                );
                assert!(
                    rect.y >= 0. && rect.y + rect.height <= height,
                    "{label} clipped vertically"
                );
            }
        }
        // Measure the actual available center width, independently of side panels.
        let conversation = app.harness.as_ref().unwrap().get(&child_id).unwrap();
        for (width, running, wide) in [
            (1000., false, true),
            (1000., true, true),
            (320., true, false),
            (320., false, false),
        ] {
            conversation.running.store(running, Ordering::Release);
            let mut ui = iced_runtime::UserInterface::build(
                app.center(),
                iced::Size::new(width, 560.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            let stop = bounds.label("■");
            let status = bounds.label("●");
            assert_eq!(status.center_y(), stop.center_y());
            assert!(status.x + status.width < stop.x);
            assert!(stop.x + stop.width <= width);
            let title = "Explore · Inspect cancellation";
            for label in [title, "Back to main agent"] {
                assert_eq!(bounds.labels.iter().any(|(s, _)| s == label), wide);
            }
            if wide {
                let title = bounds.label(title);
                let back = bounds.label("Back to main agent");
                assert!((title.center_x() - width / 2.).abs() < 1.);
                assert!((title.center_y() - stop.center_y()).abs() < 1.);
                assert!((back.center_y() - stop.center_y()).abs() < 1.);
                assert!(back.x + back.width < title.x);
                assert!(title.x + title.width < status.x);
            }
            assert!(!bounds.labels.iter().any(|(s, _)| {
                s == "Completed"
                    || s == "Stop"
                    || s.contains("Read-only profile instructions")
                    || s.contains("Parent task")
            }));
            let back = bounds
                .containers
                .iter()
                .filter(|rect| rect.center_y() == stop.center_y() && rect.x + rect.width < status.x)
                .min_by(|a, b| a.width.total_cmp(&b.width))
                .expect("back button content");
            for (point, back) in [(back.center(), true), (stop.center(), false)] {
                let mut messages = Vec::new();
                ui.update(
                    &[
                        iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    ],
                    mouse::Cursor::Available(point),
                    &mut renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut messages,
                );
                if back {
                    assert!(matches!(messages.as_slice(), [Message::Select(id)] if id == &parent));
                } else if running {
                    assert!(
                        matches!(messages.as_slice(), [Message::StopAgent(id)] if id == &child_id)
                    );
                } else {
                    assert!(messages.is_empty(), "completed child must not be stoppable");
                }
            }
        }
        conversation.running.store(false, Ordering::Release);
        dispatch(&mut app, Message::Select(parent.clone())).await;
        assert_eq!(app.editor.text(), "Keep this unsent parent draft λ");
        assert!(!app.auto_scroll);
        assert_eq!(app.transcript.offset, parent_offset);
        dispatch(&mut app, Message::ToggleSettings).await;
        assert!(app.settings_open && !app.agents_open);
        dispatch(&mut app, Message::ToggleAgents).await;
        assert!(app.agents_open && !app.settings_open);
        dispatch(&mut app, Message::Select(child_id.clone())).await;
        dispatch(&mut app, Message::Delete(parent.clone())).await;
        dispatch(&mut app, Message::Confirm).await;
        assert!(app.selected.is_none());
        assert!(app.current().is_none());
        assert!(
            app.harness
                .as_ref()
                .unwrap()
                .store
                .load_session(&child_id)
                .is_err()
        );
    }

    fn header_renderer() -> iced::Renderer {
        crate::test_renderer::new(16.)
    }

    #[test]
    fn composer_grows_and_shrinks_within_its_configured_limits() {
        use iced::advanced::{layout, widget::Tree};

        let mut app = app();
        let renderer = header_renderer();
        let measure = |app: &App, width| {
            let mut editor = app.composer_editor(false);
            let mut tree = Tree::new(editor.as_widget());
            editor
                .as_widget_mut()
                .layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(iced::Size::ZERO, iced::Size::new(width, f32::INFINITY)),
                )
                .size()
                .height
        };
        let assert_height = |actual: f32, expected: f32| {
            assert!(
                (actual - expected).abs() < 0.01,
                "height {actual}, expected {expected}"
            );
        };
        for font_size in [app.catalog.settings.font_size, 20.] {
            app.catalog.settings.font_size = font_size;
            app.editor = text_editor::Content::new();
            let min_height = measure(&app, 400.);
            let line_height = app
                .editor
                .with_editor(|editor| editor.buffer().metrics().line_height);
            assert_eq!(line_height, font_size * COMPOSER_LINE_HEIGHT_FACTOR);
            assert_height(
                min_height,
                COMPOSER_MIN_LINES as f32 * line_height + 2. * crate::growing_editor::PADDING,
            );
            let max_height =
                min_height + (COMPOSER_MAX_LINES - COMPOSER_MIN_LINES) as f32 * line_height;
            let overflow_lines = COMPOSER_MAX_LINES + 7;
            for lines in [
                0,
                1,
                COMPOSER_MIN_LINES,
                COMPOSER_MIN_LINES + 1,
                COMPOSER_MAX_LINES - 1,
                COMPOSER_MAX_LINES,
                overflow_lines,
            ] {
                let value = vec!["line"; lines].join("\n");
                app.editor = text_editor::Content::with_text(&value);
                let visible_lines = lines.clamp(COMPOSER_MIN_LINES, COMPOSER_MAX_LINES);
                assert_height(
                    measure(&app, 400.),
                    min_height + (visible_lines - COMPOSER_MIN_LINES) as f32 * line_height,
                );
            }
            // Cursor scrolling must not grow the field or discard overflow.
            app.editor
                .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
            assert_height(measure(&app, 400.), max_height);
            assert_eq!(app.editor.line_count(), overflow_lines);
            app.editor.perform(text_editor::Action::Select(
                text_editor::Motion::DocumentStart,
            ));
            app.editor
                .perform(text_editor::Action::Edit(text_editor::Edit::Backspace));
            assert!(app.editor.is_empty());
            assert_height(measure(&app, 400.), min_height);

            let wrapped = "Words that wrap naturally. ".repeat(8);
            app.editor = text_editor::Content::with_text(&wrapped);
            let height = measure(&app, 400.);
            let narrow = measure(&app, 100.);
            let wide = measure(&app, wrapped.chars().count() as f32 * font_size);
            if COMPOSER_MAX_LINES > COMPOSER_MIN_LINES {
                assert!(
                    narrow > wide,
                    "wrapping grows the composer when its limits allow growth"
                );
            }
            assert!(
                min_height <= wide
                    && wide <= height
                    && height <= narrow
                    && narrow <= max_height + 0.01
            );
            assert_height(wide, min_height);
            assert_eq!(app.editor.text(), wrapped);
        }
    }

    fn word_delete_events(ctrl_w: bool) -> [iced::Event; 2] {
        let (key, code) = if ctrl_w {
            (
                keyboard::Key::Character("w".into()),
                keyboard::key::Code::KeyW,
            )
        } else {
            (
                keyboard::Key::Named(keyboard::key::Named::Backspace),
                keyboard::key::Code::Backspace,
            )
        };
        [
            iced::Event::Keyboard(keyboard::Event::ModifiersChanged(keyboard::Modifiers::CTRL)),
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key: keyboard::key::Physical::Code(code),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::CTRL,
                text: None,
                repeat: false,
            }),
        ]
    }

    #[test]
    fn text_fields_delete_words_or_selection_with_both_shortcuts() {
        use iced::advanced::widget::operation::{focusable, text_input as input};
        let mut renderer = header_renderer();
        for ctrl_w in [false, true] {
            // Cursor/selection columns here are grapheme indices, not bytes.
            for (value, start, end, expected) in [
                ("prefix foo/bar tail", 14, 14, "prefix foo/ tail"),
                ("prefix foo/ tail", 11, 11, "prefix foo tail"),
                ("אחד שני שלישי", 8, 8, "אחד שלישי"),
                ("keep cafe\u{301}  tail", 11, 11, "keep tail"),
                ("prefix foobar tail", 9, 11, "prefix foar tail"),
                ("unchanged", 0, 0, "unchanged"),
            ] {
                let view = pointer_input(
                    text_input("", value)
                        .id("field")
                        .on_input(Message::Search)
                        .into(),
                    false,
                    Default::default(),
                );
                let mut ui = iced_runtime::UserInterface::build(
                    view,
                    iced::Size::new(400., 40.),
                    iced_runtime::user_interface::Cache::new(),
                    &mut renderer,
                );
                ui.operate(&renderer, &mut focusable::focus("field".into()));
                ui.operate(
                    &renderer,
                    &mut input::select_range("field".into(), start, end),
                );
                let mut messages = Vec::new();
                ui.update(
                    &word_delete_events(ctrl_w),
                    mouse::Cursor::Unavailable,
                    &mut renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut messages,
                );
                assert!(
                    matches!(messages.as_slice(), [Message::Search(text)] if text == expected),
                    "Ctrl+{} on {value:?}: {messages:?}",
                    if ctrl_w { "W" } else { "Backspace" }
                );
            }
        }
    }

    fn delete_word_in_composer(app: &mut App, ctrl_w: bool) {
        let mut renderer = header_renderer();
        let messages = {
            let view = pointer_input(app.composer_editor(false), false, Default::default());
            let mut ui = iced_runtime::UserInterface::build(
                view,
                iced::Size::new(400., 100.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            // Focus the real widget, but keep the cursor/selection set by the case.
            ui.update(
                &[iced::Event::Mouse(mouse::Event::ButtonPressed(
                    mouse::Button::Left,
                ))],
                mouse::Cursor::Available(iced::Point::new(10., 10.)),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut Vec::new(),
            );
            let mut messages = Vec::new();
            ui.update(
                &word_delete_events(ctrl_w),
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            messages
        };
        for message in messages {
            let _ = app.update(message);
        }
    }

    #[tokio::test]
    async fn composer_word_deletion_preserves_suffix_selection_and_draft_saving() {
        let mut app = app();
        let root = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, root).await;
        for ctrl_w in [false, true] {
            for (value, line, column, selection, expected) in [
                ("prefix foo/bar tail", 0, 14, None, "prefix foo/ tail"),
                ("prefix foo/ tail", 0, 11, None, "prefix foo tail"),
                ("אחד שני שלישי", 0, "אחד שני ".len(), None, "אחד שלישי"),
                (
                    "first\nkeep cafe\u{301}  tail",
                    1,
                    13,
                    None,
                    "first\nkeep tail",
                ),
                (
                    "prefix foobar tail",
                    0,
                    9,
                    Some((0, 11)),
                    "prefix foar tail",
                ),
                ("first\nsecond tail", 1, 0, None, "firstsecond tail"),
                ("unchanged", 0, 0, None, "unchanged"),
                ("", 0, 0, None, ""),
            ] {
                app.editor = text_editor::Content::with_text(value);
                app.editor.move_to(text_editor::Cursor {
                    position: text_editor::Position { line, column },
                    selection: selection
                        .map(|(line, column)| text_editor::Position { line, column }),
                });
                app.draft_dirty = false;
                delete_word_in_composer(&mut app, ctrl_w);
                assert_eq!(
                    app.editor.text(),
                    expected,
                    "Ctrl+{} on {value:?}",
                    if ctrl_w { "W" } else { "Backspace" }
                );
                if expected != value {
                    dispatch(&mut app, Message::SaveDraft).await;
                    let saved = app
                        .harness
                        .as_ref()
                        .unwrap()
                        .store
                        .load_session(app.selected.as_ref().unwrap())
                        .unwrap();
                    assert_eq!(saved.draft, expected, "real edits must survive saving");
                }
            }
        }
        app.editor = text_editor::Content::with_text("must stay");
        app.editor
            .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
        app.io_busy = true;
        delete_word_in_composer(&mut app, true);
        assert_eq!(
            app.editor.text(),
            "must stay",
            "saving must still block edits"
        );
    }

    fn displayed_text(view: Element<'_, Message>, size: iced::Size) -> String {
        let mut renderer = header_renderer();
        let mut ui = iced_runtime::UserInterface::build(
            view,
            size,
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        bounds
            .labels
            .into_iter()
            .map(|(text, _)| text)
            .collect::<Vec<_>>()
            .concat()
    }
    fn entry_text(app: &App, index: usize) -> String {
        displayed_text(
            app.entry_view(&app.current().unwrap().entries[index], index),
            iced::Size::new(900., 1350.),
        )
    }
    fn chat_text(app: &App) -> String {
        displayed_text(app.center(), iced::Size::new(930., 1350.))
    }

    fn click_at(
        ui: &mut iced_runtime::UserInterface<'_, Message, Theme, iced::Renderer>,
        renderer: &mut iced::Renderer,
        point: iced::Point,
    ) -> Vec<Message> {
        let mut messages = Vec::new();
        ui.update(
            &[
                iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(point),
            renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        messages
    }

    #[derive(Default)]
    struct UiBounds {
        labels: Vec<(String, iced::Rectangle)>,
        containers: Vec<iced::Rectangle>,
        scrolls: Vec<(iced::advanced::widget::Id, iced::Rectangle)>,
    }
    impl iced::advanced::widget::Operation for UiBounds {
        fn traverse(
            &mut self,
            operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation),
        ) {
            operate(self);
        }
        fn container(&mut self, _: Option<&iced::advanced::widget::Id>, bounds: iced::Rectangle) {
            self.containers.push(bounds);
        }
        fn scrollable(
            &mut self,
            id: Option<&iced::advanced::widget::Id>,
            bounds: iced::Rectangle,
            _: iced::Rectangle,
            _: iced::Vector,
            _: &mut dyn iced::advanced::widget::operation::Scrollable,
        ) {
            if let Some(id) = id {
                self.scrolls.push((id.clone(), bounds));
            }
        }
        fn text(
            &mut self,
            _: Option<&iced::advanced::widget::Id>,
            bounds: iced::Rectangle,
            value: &str,
        ) {
            self.labels.push((value.into(), bounds));
        }
    }
    impl UiBounds {
        fn leftmost_control(&self) -> iced::Point {
            self.containers
                .iter()
                .min_by(|a, b| a.x.total_cmp(&b.x).then(a.width.total_cmp(&b.width)))
                .unwrap()
                .center()
        }
        fn label(&self, value: &str) -> iced::Rectangle {
            self.labels
                .iter()
                .find(|(text, _)| text == value)
                .unwrap_or_else(|| panic!("missing label {value}"))
                .1
        }
    }
    #[tokio::test]
    async fn right_panel_headers_match_and_close_their_own_panel() {
        let mut app = app();
        let mut renderer = header_renderer();
        for width in [1200., 860.] {
            app.window_width = width;
            let mut headers = Vec::new();
            for agents in [false, true] {
                let (title, panel) = if agents {
                    ("Agents", app.agents_panel())
                } else {
                    ("Settings", app.settings_panel())
                };
                let mut ui = iced_runtime::UserInterface::build(
                    panel,
                    iced::Size::new(width, 560.),
                    iced_runtime::user_interface::Cache::new(),
                    &mut renderer,
                );
                let mut bounds = UiBounds::default();
                ui.operate(&renderer, &mut bounds);
                let title = bounds.label(title);
                let close = bounds.label("×");
                assert_eq!(title.x, RIGHT_PANEL_PADDING);
                // The title is centered within the taller close-button row.
                assert!(title.y >= RIGHT_PANEL_PADDING);
                assert_eq!(title.center_y(), close.center_y());
                headers.push((title, close));

                let mut messages = Vec::new();
                ui.update(
                    &[
                        iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    ],
                    mouse::Cursor::Available(close.center()),
                    &mut renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut messages,
                );
                assert!(matches!(
                    (agents, messages.as_slice()),
                    (true, [Message::ToggleAgents]) | (false, [Message::ToggleSettings])
                ));
            }
            assert_eq!(headers[0], headers[1], "headers differ at width {width}");
        }
    }

    fn footer_key(key: keyboard::key::Named) -> Message {
        Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(key),
            modified_key: keyboard::Key::Named(key),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Enter),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        }))
    }

    #[tokio::test]
    async fn git_refreshes_on_selection_and_idle_ticks() {
        let mut app = app();
        let repo = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "--template=", "--initial-branch=main"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let file = repo.path().join("new.txt");
        std::fs::write(&file, "one\ntwo\n").unwrap();

        // Selection must fetch without waiting for a timer or an agent turn.
        dispatch(&mut app, Message::Create(repo.path().to_owned())).await;
        assert!(!app.running());
        assert!(matches!(
            app.git_info(),
            Some(crate::git::Info::Changes {
                added: 2,
                removed: 0,
                ..
            })
        ));

        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        dispatch(&mut app, Message::GitTick).await;
        assert!(matches!(
            app.git_info(),
            Some(crate::git::Info::Changes {
                added: 3,
                removed: 0,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn git_cache_follows_projects_and_retains_background_results() {
        let mut app = app();
        let a = PathBuf::from("/project-a");
        let b = PathBuf::from("/project-b");
        let changes = |added| {
            Some(crate::git::Info::Changes {
                branch: "main".into(),
                added,
                removed: 0,
            })
        };

        // Drop capture tasks and deliver their results explicitly to model
        // switching projects while Git is still running, without timing races.
        let _ = app.update(Message::Selected(Ok((Selection::Draft(a.clone()), None))));
        assert!(app.git_pending.contains(&a));
        assert!(app.git_info().is_none());
        let _ = app.update(Message::Git(a.clone(), changes(2)));
        let _ = app.update(Message::GitTick);
        assert!(app.git_pending.contains(&a));
        dispatch(&mut app, Message::GitTick).await;
        assert!(
            app.git_pending.contains(&a),
            "a pending capture must not be duplicated"
        );
        assert_eq!(app.git_info().unwrap().to_string(), "main  +2 −0");

        let _ = app.update(Message::Selected(Ok((Selection::Draft(b.clone()), None))));
        assert!(
            app.git_pending.contains(&b),
            "project A must not block B's refresh"
        );
        assert!(
            app.git_info().is_none(),
            "never display another project's counts"
        );
        let _ = app.update(Message::Git(b.clone(), changes(7)));
        let _ = app.update(Message::Git(a.clone(), changes(3)));
        assert_eq!(app.git_info().unwrap().to_string(), "main  +7 −0");

        let _ = app.update(Message::Selected(Ok((Selection::Draft(a.clone()), None))));
        assert_eq!(
            app.git_info().unwrap().to_string(),
            "main  +3 −0",
            "returning to a project immediately uses its latest cached result"
        );
        assert!(
            app.git_pending.contains(&a),
            "cached projects also refresh on selection"
        );
        let _ = app.update(Message::Selected(Ok((Selection::Draft(a.clone()), None))));
        assert_eq!(app.git_info().unwrap().to_string(), "main  +3 −0");
        let _ = app.update(Message::Git(a, None));
        assert!(
            app.git_info().is_none(),
            "a non-repository result clears the cache"
        );
        let _ = app.update(Message::Selected(Ok((Selection::Draft(b), None))));
        assert_eq!(app.git_info().unwrap().to_string(), "main  +7 −0");
    }

    #[tokio::test]
    async fn footer_omits_absent_git_and_zero_diff_counts() {
        let mut app = app();
        dispatch(&mut app, Message::Create(PathBuf::from("/footer-project"))).await;
        let mut renderer = header_renderer();
        for (status, expected_branch, expected_counts) in [
            (None, false, None),
            (
                Some(crate::git::Info::Changes {
                    branch: "main".into(),
                    added: 0,
                    removed: 0,
                }),
                true,
                None,
            ),
            (
                Some(crate::git::Info::Changes {
                    branch: "main".into(),
                    added: 0,
                    removed: 3,
                }),
                true,
                Some(("+0", "−3")),
            ),
        ] {
            let expected_details = expected_counts.map_or("main", |_| "main  +0 −3");
            if let Some(status) = &status {
                assert_eq!(status.to_string(), expected_details);
            }
            // Apply refresh results so a non-repository result also clears old status.
            let path = app.current().unwrap().project.clone();
            let _ = app.update(Message::Git(path, status));
            let mut ui = iced_runtime::UserInterface::build(
                app.footer_content(1400.),
                iced::Size::new(1400., 100.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            assert_eq!(
                bounds.labels.iter().any(|(label, _)| label == "main"),
                expected_branch
            );
            let text = bounds
                .labels
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let counts: Vec<_> = text
                .split_whitespace()
                .filter(|token| token.starts_with('+') || token.starts_with('−'))
                .collect();
            assert_eq!(
                counts,
                expected_counts.map_or_else(Vec::new, |(added, removed)| vec![added, removed])
            );
        }
        let path = app.current().unwrap().project.clone();
        let _ = app.update(Message::Git(path, None));
        assert!(
            app.git_info().is_none(),
            "a non-repository refresh clears old Git status"
        );
    }

    #[tokio::test]
    async fn footer_stays_at_chat_bottom_and_controls_fit_with_both_sidebars() {
        let mut app = app();
        // This layout fixture uses the plain effort chip, independently of saved defaults.
        app.catalog
            .settings
            .models
            .get_mut(&app.catalog.settings.model)
            .unwrap()
            .fast = false;
        dispatch(&mut app, Message::Create(PathBuf::from("/footer-project"))).await;
        app.git.insert(
            app.current().unwrap().project.clone(),
            crate::git::Info::Changes {
                branch: "long-feature-branch-name".into(),
                added: 12345,
                removed: 678,
            },
        );
        app.settings_open = true;
        let mut renderer = header_renderer();
        for (width, height, left, right) in [(1600., 800., 260., 320.), (860., 560., 344., 344.)] {
            app.window_width = width;
            app.window_height = height;
            app.catalog.settings.sidebar_width = left;
            app.catalog.settings.settings_width = right;
            let mut ui = iced_runtime::UserInterface::build(
                app.view(),
                iced::Size::new(width, height),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            let model = bounds.label(&app.current().unwrap().model);
            let effort = bounds.label(&app.current().unwrap().effort);
            let footer = bounds
                .containers
                .iter()
                .filter(|b| {
                    b.y > 0.
                        && b.y + b.height == height
                        && b.contains(model.center())
                        && b.contains(effort.center())
                })
                .min_by(|a, b| a.height.total_cmp(&b.height))
                .expect("bottom-anchored footer containing both controls");
            assert!(
                footer.height < height / 4.,
                "footer must not take a fill-height share"
            );
            assert!(footer.x > left && footer.x + footer.width < width - right);
            for control in [model, effort] {
                assert!(
                    control.width > 0. && control.height > 0.,
                    "control must remain clickable"
                );
                assert!(
                    control.x >= footer.x && control.x + control.width <= footer.x + footer.width
                );
                assert!(control.y >= footer.y && control.y + control.height <= height);
            }
            assert!(model.x + model.width <= effort.x);
            assert!(
                bounds
                    .containers
                    .iter()
                    .any(|b| b.x == 0. && b.width == left && b.height == height)
            );
            assert!(
                bounds
                    .containers
                    .iter()
                    .any(|b| b.x > footer.x + footer.width
                        && b.x + b.width == width
                        && b.y == 0.
                        && b.height == height
                        && b.contains(bounds.label("⚙").center()))
            );
            let has_tps = bounds
                .labels
                .iter()
                .any(|(label, _)| label.starts_with("TPS:"));
            assert_eq!(
                has_tps,
                width == 1600.,
                "metrics collapse using the available center width"
            );
            if width == 1600. {
                let added = bounds.label("+12345");
                let removed = bounds.label("−678");
                assert!(added.y >= footer.y && removed.y >= footer.y);
                assert!(added.x + added.width <= removed.x);
                assert!(removed.x + removed.width <= footer.x + footer.width);
            } else {
                assert!(!bounds.labels.iter().any(|(label, _)| label == "+12345"));
            }
            drop(ui);
            dispatch(&mut app, Message::FooterMenu(FooterControl::Model)).await;
            let mut ui = iced_runtime::UserInterface::build(
                app.view(),
                iced::Size::new(width, height),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut menu_bounds = UiBounds::default();
            ui.operate(&renderer, &mut menu_bounds);
            let option = menu_bounds.label("gpt-6-luna");
            assert!(
                option.width >= footer_text_width("gpt-6-luna", FOOTER_MENU_TEXT_SIZE),
                "popup options must stay readable even with a narrow chat"
            );
            drop(ui);
            dispatch(&mut app, Message::CloseFooterMenu).await;
        }
    }

    #[tokio::test]
    async fn footer_menus_select_with_keyboard_and_dismiss_without_closing_settings() {
        use keyboard::key::Named;
        let mut app = app();
        dispatch(&mut app, Message::Create(PathBuf::from("/footer-project"))).await;
        app.settings_open = true;
        let old_model = app.current().unwrap().model.clone();
        dispatch(&mut app, Message::FooterMenu(FooterControl::Model)).await;
        let menu = app.footer_menu.as_ref().unwrap();
        assert_eq!(menu.options[menu.focused], old_model);
        let expected = menu.options[(menu.focused + 1) % menu.options.len()].clone();
        dispatch(&mut app, footer_key(Named::ArrowDown)).await;
        dispatch(&mut app, footer_key(Named::Enter)).await;
        assert_eq!(app.current().unwrap().model, expected);
        assert!(app.footer_menu.is_none());
        dispatch(&mut app, Message::FooterMenu(FooterControl::Effort)).await;
        dispatch(&mut app, footer_key(Named::Escape)).await;
        assert!(app.footer_menu.is_none());
        assert!(app.settings_open);
        dispatch(&mut app, Message::FooterMenu(FooterControl::Effort)).await;
        let menu = app.footer_menu.as_ref().unwrap();
        let expected =
            menu.options[(menu.focused + menu.options.len() - 1) % menu.options.len()].clone();
        dispatch(&mut app, footer_key(Named::ArrowUp)).await;
        dispatch(&mut app, footer_key(Named::Tab)).await;
        assert_eq!(app.current().unwrap().effort, expected);
        dispatch(&mut app, Message::FooterMenu(FooterControl::Model)).await;
        let messages = {
            let mut renderer = header_renderer();
            let mut ui = iced_runtime::UserInterface::build(
                app.view(),
                iced::Size::new(1200., 800.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            click_at(&mut ui, &mut renderer, iced::Point::ORIGIN)
        };
        assert!(matches!(messages.as_slice(), [Message::CloseFooterMenu]));
        dispatch(&mut app, messages.into_iter().next().unwrap()).await;
        assert!(app.footer_menu.is_none());
        dispatch(&mut app, Message::FooterMenu(FooterControl::Effort)).await;
        let messages = {
            let mut renderer = header_renderer();
            let mut ui = iced_runtime::UserInterface::build(
                app.view(),
                iced::Size::new(1200., 800.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            let mut messages = Vec::new();
            ui.update(
                &[
                    iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(
                    bounds
                        .labels
                        .iter()
                        .rev()
                        .find(|(label, _)| label == "high")
                        .unwrap()
                        .1
                        .center(),
                ),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            messages
        };
        assert!(
            matches!(messages.as_slice(), [Message::SessionEffort(value)] if value == "high"),
            "unexpected popup messages: {messages:?}"
        );
        dispatch(&mut app, messages.into_iter().next().unwrap()).await;
        assert_eq!(app.current().unwrap().effort, "high");
        assert!(app.footer_menu.is_none());
    }

    #[tokio::test]
    async fn quit_confirmation_releases_footer_keyboard_ownership() {
        let mut app = app();
        stored_session(&mut app, PathBuf::from("/running")).await;
        let running = app
            .harness
            .as_ref()
            .unwrap()
            .get(app.selected.as_ref().unwrap())
            .unwrap();
        running.running.store(true, Ordering::Release);
        stored_session(&mut app, PathBuf::from("/idle")).await;
        dispatch(&mut app, Message::FooterMenu(FooterControl::Model)).await;
        assert!(app.footer_menu.is_some());
        dispatch(
            &mut app,
            Message::Event(iced::Event::Window(window::Event::CloseRequested)),
        )
        .await;
        assert!(matches!(app.modal, Some(Modal::Quit)));
        assert!(app.footer_menu.is_none());
        dispatch(&mut app, footer_key(keyboard::key::Named::Escape)).await;
        assert!(app.modal.is_none());
        running.running.store(false, Ordering::Release);
    }

    #[tokio::test]
    async fn sidebar_header_centers_only_favorites_and_opens_draft_in_clicked_project() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap().join("favorite-project");
        app.catalog.projects = vec![
            Project {
                path: PathBuf::from("/not-starred"),
                favorite: false,
            },
            Project {
                path: path.clone(),
                favorite: true,
            },
        ];
        let mut renderer = header_renderer();
        let create = {
            let mut ui = iced_runtime::UserInterface::build(
                app.sidebar_header(),
                iced::Size::new(346., 100.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            assert!(matches!(
                click_at(&mut ui, &mut renderer, bounds.leftmost_control()).as_slice(),
                [Message::ToggleSearch]
            ));
            assert!(matches!(
                click_at(&mut ui, &mut renderer, bounds.label("+").center()).as_slice(),
                [Message::New]
            ));
            let messages = click_at(
                &mut ui,
                &mut renderer,
                bounds
                    .label(&project_initials(
                        &path.file_name().unwrap().to_string_lossy(),
                    ))
                    .center(),
            );
            assert!(matches!(messages.as_slice(), [Message::Create(p)] if p == &path));
            messages.into_iter().next().unwrap()
        };
        dispatch(&mut app, create).await;
        assert_eq!(app.current().unwrap().project, path);
        assert!(app.selected.is_none());
        assert!(app.sessions.is_empty());
        dispatch(&mut app, Message::New).await;
        assert!(app.selected.is_none(), "+ must open the project chooser");
    }

    #[tokio::test]
    async fn project_badge_labels_are_centered_in_header_and_session_rows() {
        use iced::advanced::widget::{Id, Operation};
        #[derive(Default)]
        struct BadgeBounds {
            containers: Vec<iced::Rectangle>,
            labels: Vec<(iced::Rectangle, iced::Rectangle)>,
        }
        impl Operation for BadgeBounds {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn container(&mut self, _id: Option<&Id>, bounds: iced::Rectangle) {
                self.containers.push(bounds);
            }
            fn text(&mut self, _id: Option<&Id>, bounds: iced::Rectangle, text: &str) {
                if text == "PE" {
                    let badge = self
                        .containers
                        .iter()
                        .rev()
                        .find(|badge| badge.contains(bounds.center()))
                        .unwrap();
                    self.labels.push((*badge, bounds));
                }
            }
        }
        let mut app = app();
        let path = PathBuf::from("/pico-evo");
        app.catalog.projects.push(Project {
            path: path.clone(),
            favorite: true,
        });
        stored_session(&mut app, path).await;
        let mut renderer = header_renderer();
        let mut ui = iced_runtime::UserInterface::build(
            app.sidebar(),
            iced::Size::new(240., 640.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = BadgeBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert_eq!(
            bounds.labels.len(),
            2,
            "header and session badges must both be checked"
        );
        for (badge, label) in bounds.labels {
            assert_eq!(
                label.center(),
                badge.center(),
                "initials must stay centered inside the badge"
            );
        }
    }

    #[tokio::test]
    async fn sidebar_favorites_scroll_without_moving_header_controls() {
        let mut app = app();
        app.catalog.projects = (0..12)
            .map(|i| Project {
                path: PathBuf::from(format!("/project-{i}")),
                favorite: true,
            })
            .collect();
        let mut renderer = header_renderer();
        let mut ui = iced_runtime::UserInterface::build(
            app.sidebar_header(),
            iced::Size::new(160., 100.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let favorites = bounds
            .scrolls
            .iter()
            .find(|(id, _)| id == &iced::advanced::widget::Id::new("favorite-projects"))
            .unwrap()
            .1;
        let search = bounds.leftmost_control();
        let new = bounds.label("+").center();
        assert!(
            matches!(click_at(&mut ui, &mut renderer, iced::Point::new(favorites.x + 1., favorites.center_y())).as_slice(), [Message::Create(p)] if p == Path::new("/project-0"))
        );
        ui.update(
            &[iced::Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: -1000., y: 0. },
            })],
            mouse::Cursor::Available(favorites.center()),
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Vec::new(),
        );
        assert!(
            matches!(click_at(&mut ui, &mut renderer, iced::Point::new(favorites.x + favorites.width - 1., favorites.center_y())).as_slice(), [Message::Create(p)] if p == Path::new("/project-11"))
        );
        assert!(matches!(
            click_at(&mut ui, &mut renderer, search).as_slice(),
            [Message::ToggleSearch]
        ));
        assert!(matches!(
            click_at(&mut ui, &mut renderer, new).as_slice(),
            [Message::New]
        ));
    }

    #[tokio::test]
    async fn sidebar_follows_top_but_preserves_browsing_anchor() {
        use iced::advanced::widget::{Id, Operation, operation::Scrollable};
        use iced::futures::StreamExt;
        use iced_runtime::{UserInterface, user_interface::Cache};

        #[derive(Default)]
        struct Offset(Option<f32>);
        impl Operation for Offset {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
                visit(self);
            }
            fn scrollable(
                &mut self,
                id: Option<&Id>,
                _: iced::Rectangle,
                _: iced::Rectangle,
                translation: iced::Vector,
                _: &mut dyn Scrollable,
            ) {
                if id == Some(&Id::new("sessions")) {
                    self.0 = Some(translation.y);
                }
            }
        }

        // Check the real scroll widget as well as the virtual list, retaining
        // widget state so a missing scroll correction cannot pass unnoticed.
        async fn check(app: &mut App, task: Task<Message>, cache: Cache, expected: f32) -> Cache {
            assert_eq!(app.sidebar_list.offset, expected);
            let task = Task::batch(std::iter::once(task).chain(app.tasks.drain(..)));
            let mut renderer = header_renderer();
            let mut ui = UserInterface::build(
                app.sidebar(),
                iced::Size::new(346., 340.),
                cache,
                &mut renderer,
            );
            if let Some(mut actions) = iced_runtime::task::into_stream(task) {
                while let Some(action) = actions.next().await {
                    if let iced_runtime::Action::Widget(mut operation) = action {
                        ui.operate(&renderer, operation.as_mut());
                    }
                }
            }
            let mut offset = Offset::default();
            ui.operate(&renderer, &mut offset);
            assert_eq!(offset.0, Some(expected));
            ui.into_cache()
        }

        let mut app = app();
        app.tasks.clear();
        app.sessions = (0..40)
            .map(|i| SessionSummary {
                child: None,
                id: format!("sticky-session-{i}"),
                title: format!("Session {i}"),
                project: PathBuf::from("/project"),
                updated: 0,
                interrupted: false,
                needs_attention: false,
                unchecked_completion: false,
            })
            .collect();
        app.prepare_sidebar();
        let mut cache = check(&mut app, Task::none(), Cache::new(), 0.).await;

        // Inserting a new section used to anchor the old SESSIONS heading,
        // hiding the new top row even though the user had not scrolled.
        let mut new = app.sessions[0].clone();
        new.id = "new-session".into();
        new.needs_attention = true;
        app.sessions.insert(0, new);
        app.prepare_sidebar();
        cache = check(&mut app, Task::none(), cache, 0.).await;

        let offset = app.sidebar_list.prefix(12) + 7.;
        let anchor = app.sidebar_list.order[12];
        let epoch = app.sidebar_epoch;
        let _ = app.update(Message::SidebarScroll(epoch, offset, 300.));
        assert!(!app.sidebar_follow_top);
        let task = widget::operation::scroll_to(
            "sessions",
            scrollable::AbsoluteOffset { x: 0., y: offset },
        );
        cache = check(&mut app, task, cache, offset).await;

        // Move an existing session above the viewport while browsing.
        let mut moved = app.sessions.pop().unwrap();
        moved.needs_attention = true;
        app.sessions.insert(0, moved);
        app.prepare_sidebar();
        assert_eq!(app.sidebar_list.order[13], anchor);
        let anchor_index = app
            .sidebar_list
            .order
            .iter()
            .position(|&id| id == anchor)
            .unwrap();
        let moved_offset = app.sidebar_list.prefix(anchor_index) + 7.;
        assert!(
            moved_offset > offset,
            "inserting above the anchor advances its scroll offset"
        );
        cache = check(&mut app, Task::none(), cache, moved_offset).await;

        // Late scroll notifications from the old layout must not re-enable
        // following. A measured height above the anchor still corrects it.
        let _ = app.update(Message::SidebarScroll(epoch, 0., 300.));
        assert!(!app.sidebar_follow_top);
        let previous_height = app.sidebar_list.prefix(1) - app.sidebar_list.prefix(0);
        let task = app.update(Message::SidebarSize(
            app.sidebar_epoch,
            1,
            previous_height * 1.5,
        ));
        let measured_offset = app.sidebar_list.prefix(anchor_index) + 7.;
        assert!(
            measured_offset > moved_offset,
            "a taller row above the anchor advances it"
        );
        cache = check(&mut app, task, cache, measured_offset).await;

        // Returning within one pixel of the top resumes following. Removing
        // the first section and later moving a session back into it stay pinned.
        let _ = app.update(Message::SidebarScroll(app.sidebar_epoch, 0.5, 300.));
        assert!(app.sidebar_follow_top);
        for session in &mut app.sessions {
            session.needs_attention = false;
        }
        app.prepare_sidebar();
        cache = check(&mut app, Task::none(), cache, 0.).await;
        app.sessions.last_mut().unwrap().needs_attention = true;
        app.prepare_sidebar();
        cache = check(&mut app, Task::none(), cache, 0.).await;
        let task = app.update(Message::SidebarSize(app.sidebar_epoch, 1, 63.));
        let _ = check(&mut app, task, cache, 0.).await;
    }

    #[tokio::test]
    async fn sidebar_search_focuses_filters_and_clears_on_close() {
        use iced::futures::StreamExt;
        let mut app = app();
        for (path, title) in [("/alpha", "Zulu notes"), ("/beta", "Control")] {
            stored_session(&mut app, PathBuf::from(path)).await;
            app.harness
                .as_ref()
                .unwrap()
                .edit(app.selected.as_ref().unwrap(), |s| s.title = title.into())
                .unwrap();
        }
        app.refresh();
        assert!(!app.search_active);
        let task = app.update(Message::ToggleSearch);
        assert!(app.search_active);
        let mut renderer = header_renderer();
        let input = {
            let mut ui = iced_runtime::UserInterface::build(
                app.sidebar_header(),
                iced::Size::new(346., 100.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            // Execute the returned focus operation, then type without clicking the input.
            let mut actions = iced_runtime::task::into_stream(task).unwrap();
            while let Some(action) = actions.next().await {
                if let iced_runtime::Action::Widget(mut operation) = action {
                    ui.operate(&renderer, operation.as_mut());
                }
            }
            let mut messages = Vec::new();
            ui.update(
                &[iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Character("z".into()),
                    modified_key: keyboard::Key::Character("z".into()),
                    physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyZ),
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                    text: Some("z".into()),
                    repeat: false,
                })],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            assert!(matches!(messages.as_slice(), [Message::Search(query)] if query == "z"));
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            assert!(matches!(
                click_at(&mut ui, &mut renderer, bounds.label("×").center()).as_slice(),
                [Message::ToggleSearch]
            ));
            messages.into_iter().next().unwrap()
        };
        dispatch(&mut app, input).await;
        let visible: Vec<_> = app
            .sidebar_rows
            .iter()
            .filter_map(|r| match r {
                SidebarRow::Session(i, _) => Some(app.sessions[*i].title.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(visible, ["Zulu notes"]);
        let epoch = app.sidebar_epoch;
        dispatch(&mut app, Message::SidebarScroll(epoch, 100., 300.)).await;
        assert!(!app.sidebar_follow_top);
        dispatch(&mut app, Message::ToggleSearch).await;
        assert!(!app.search_active);
        assert!(app.search.is_empty());
        assert_eq!(app.sidebar_list.offset, 0.);
        assert!(app.sidebar_follow_top);
        assert_eq!(
            app.sidebar_rows
                .iter()
                .filter(|r| matches!(r, SidebarRow::Session(..)))
                .count(),
            2
        );
        dispatch(&mut app, Message::ToggleSearch).await;
        dispatch(&mut app, Message::Search("BETA".into())).await;
        assert_eq!(
            app.sidebar_rows
                .iter()
                .filter(|r| matches!(r, SidebarRow::Session(..)))
                .count(),
            1
        );
        dispatch(
            &mut app,
            Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            })),
        )
        .await;
        assert!(!app.search_active);
        assert!(app.search.is_empty());
        assert_eq!(
            app.sidebar_rows
                .iter()
                .filter(|r| matches!(r, SidebarRow::Session(..)))
                .count(),
            2
        );
    }

    #[test]
    fn project_badge_initials_follow_name_words() {
        for (name, expected) in [
            ("pico-evo", "PE"),
            ("Pico", "PO"),
            ("a", "A"),
            ("  rust__language-server ", "RL"),
            ("my project tools", "MP"),
            ("éclair", "ÉR"),
            ("界", "界"),
            ("a\u{301}", "A\u{301}"),
            ("", ""),
        ] {
            assert_eq!(project_initials(name), expected, "{name:?}");
        }
    }

    #[tokio::test]
    async fn sidebar_work_keeps_activity_ticks_until_frozen_or_idle() {
        use iced::futures::{StreamExt, stream};
        use iced_runtime::futures::subscription;

        let mut app = app();
        let session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
        app.sessions.push(SessionSummary::from(&session));
        assert!(app.harness.as_ref().unwrap().all().is_empty());
        // A Working presentation can outlive is_running(), or represent children.
        // Exercise the production subscription without a running conversation.
        for status in [
            SessionStatus::Working,
            SessionStatus::Question,
            SessionStatus::Idle,
            SessionStatus::Working,
        ] {
            app.sidebar_rows = vec![SidebarRow::Session(0, status)];
            let messages = stream::select_all(
                subscription::into_recipes(app.subscription())
                    .into_iter()
                    .map(|recipe| recipe.stream(stream::pending().boxed())),
            );
            let mut ticks =
                messages.filter(|message| std::future::ready(matches!(message, Message::Tick)));
            let timeout = if status == SessionStatus::Working {
                ACTIVITY_INTERVAL + Duration::from_secs(1)
            } else {
                ACTIVITY_INTERVAL * 3
            };
            let next = tokio::time::timeout(timeout, ticks.next()).await;
            if status == SessionStatus::Working {
                assert!(
                    matches!(next, Ok(Some(Message::Tick))),
                    "Working rows need the existing activity clock"
                );
            } else {
                assert!(
                    next.is_err(),
                    "frozen/idle rows must not keep an activity clock alive"
                );
            }
        }
    }

    #[tokio::test]
    async fn window_subscription_ignores_motion_and_redraws() {
        use iced::futures::{StreamExt, stream};
        use iced_runtime::futures::subscription;
        let events = [
            iced::Event::Mouse(mouse::Event::CursorMoved {
                position: iced::Point::new(125., 77.),
            }),
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            iced::Event::Window(window::Event::RedrawRequested(std::time::Instant::now())),
            iced::Event::Window(window::Event::CloseRequested),
        ];
        let mut messages = Vec::new();
        for recipe in subscription::into_recipes(window_events()) {
            let input =
                stream::iter(
                    events
                        .clone()
                        .map(|event| subscription::Event::Interaction {
                            window: window::Id::unique(),
                            event,
                            status: iced::event::Status::Ignored,
                        }),
                )
                .boxed();
            messages.extend(recipe.stream(input).collect::<Vec<_>>().await);
        }
        assert!(matches!(
            messages.as_slice(),
            [Message::Event(iced::Event::Window(
                window::Event::CloseRequested
            ))]
        ));
    }

    #[tokio::test]
    async fn escape_dismisses_ui_before_stopping_only_the_selected_running_agent() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path.clone()).await;
        let other = app
            .harness
            .as_ref()
            .unwrap()
            .get(app.selected.as_ref().unwrap())
            .unwrap();
        stored_session(&mut app, path).await;
        let selected = app
            .harness
            .as_ref()
            .unwrap()
            .get(app.selected.as_ref().unwrap())
            .unwrap();
        let escape = Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        }));
        let idle_status = selected.status.lock().unwrap().clone();
        dispatch(&mut app, escape.clone()).await;
        assert_eq!(*selected.status.lock().unwrap(), idle_status);

        selected.running.store(true, Ordering::Release);
        other.running.store(true, Ordering::Release);
        app.settings_open = true;
        dispatch(&mut app, Message::ToggleSearch).await;
        dispatch(&mut app, Message::Search("no matches".into())).await;
        app.modal = Some(Modal::AddProject);
        app.context_menu = Some((other.id.clone(), iced::Point::ORIGIN));
        dispatch(&mut app, escape.clone()).await;
        assert!(app.context_menu.is_none());
        assert!(app.modal.is_some());
        assert!(app.settings_open);
        assert_eq!(*selected.status.lock().unwrap(), idle_status);
        dispatch(&mut app, escape.clone()).await;
        assert!(app.modal.is_none());
        assert!(app.search_active);
        assert!(app.settings_open);
        assert_eq!(*selected.status.lock().unwrap(), idle_status);
        dispatch(&mut app, escape.clone()).await;
        assert!(!app.search_active);
        assert!(app.search.is_empty());
        assert!(app.settings_open);
        assert_eq!(*selected.status.lock().unwrap(), idle_status);
        dispatch(&mut app, escape.clone()).await;
        assert!(!app.settings_open);
        assert_eq!(*selected.status.lock().unwrap(), idle_status);
        let other_status = other.status.lock().unwrap().clone();
        dispatch(&mut app, escape).await;
        assert_eq!(*selected.status.lock().unwrap(), "Stopping…");
        assert_eq!(*other.status.lock().unwrap(), other_status);
        selected.running.store(false, Ordering::Release);
        other.running.store(false, Ordering::Release);
    }

    #[tokio::test]
    async fn resizing_uses_press_position_and_saves_both_panel_widths() {
        let mut app = app();
        app.catalog.settings.sidebar_width = 260.;
        app.catalog.settings.settings_width = 340.;
        for (right, start, end) in [(false, 262., 315.), (true, 808., 777.)] {
            dispatch(
                &mut app,
                Message::ResizeStart(right, iced::Point::new(start, 50.)),
            )
            .await;
            dispatch(
                &mut app,
                Message::Event(iced::Event::Mouse(mouse::Event::CursorMoved {
                    position: iced::Point::new(end, 70.),
                })),
            )
            .await;
            dispatch(
                &mut app,
                Message::Event(iced::Event::Mouse(mouse::Event::ButtonReleased(
                    mouse::Button::Left,
                ))),
            )
            .await;
            assert!(app.drag.is_none());
        }
        assert_eq!(app.catalog.settings.sidebar_width, 313.);
        assert_eq!(app.catalog.settings.settings_width, 371.);
        let (saved, _, _) = app.harness.as_ref().unwrap().store.load().unwrap();
        assert_eq!(saved.settings.sidebar_width, 313.);
        assert_eq!(saved.settings.settings_width, 371.);
    }

    #[tokio::test]
    async fn pending_transitions_block_stale_edits_autosaves_and_measurements() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path.clone()).await;
        let first = app.selected.clone().unwrap();
        dispatch(
            &mut app,
            Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('a'))),
        )
        .await;
        let task = app.update(Message::Create(path));
        assert!(app.io_busy);
        dispatch(
            &mut app,
            Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('b'))),
        )
        .await;
        assert_eq!(app.editor.text(), "a");
        let old_epoch = app.view_epoch;
        drain(&mut app, task).await;
        let offset = app.transcript.offset;
        dispatch(&mut app, Message::Scroll(old_epoch, 200000., 300., false)).await;
        dispatch(&mut app, Message::RowSize(old_epoch, 0, 100000.)).await;
        assert_eq!(app.transcript.offset, offset);
        dispatch(&mut app, Message::Select(first.clone())).await;
        assert_eq!(app.editor.text(), "a");
        let send = app.update(Message::Send);
        dispatch(&mut app, Message::SaveDraft).await;
        dispatch(
            &mut app,
            Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('c'))),
        )
        .await;
        assert_eq!(app.editor.text(), "a");
        drain(&mut app, send).await;
        assert!(app.editor.text().is_empty());
        assert!(
            app.harness
                .as_ref()
                .unwrap()
                .store
                .load_session(&first)
                .unwrap()
                .draft
                .is_empty()
        );
    }
    #[tokio::test]
    async fn shutdown_waits_for_submission_and_reports_catalog_failure() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        dispatch(&mut app, Message::Create(path)).await;
        dispatch(
            &mut app,
            Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('x'))),
        )
        .await;
        let send = app.update(Message::Send);
        let _ = app.quit();
        assert!(app.close_requested);
        let settings = app
            .harness
            .as_ref()
            .unwrap()
            .store
            .root()
            .join("settings.json");
        std::fs::remove_file(&settings).unwrap();
        std::fs::create_dir(&settings).unwrap();
        drain(&mut app, send).await;
        assert!(!app.closing);
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("Could not finish saving")
        );
        assert!(
            app.harness
                .as_ref()
                .unwrap()
                .all()
                .iter()
                .all(|c| !c.is_running())
        );
        std::fs::remove_dir(settings).unwrap();
        let quit = app.quit();
        drain(&mut app, quit).await;
        assert!(app.closing);
    }
    #[tokio::test]
    async fn streaming_reparse_invalidates_even_a_zero_length_selection_anchor() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path).await;
        let h = app.harness.as_ref().unwrap().clone();
        let id = app.selected.clone().unwrap();
        h.edit(&id, |s| {
            s.entries.push(Entry::new(EntryKind::Assistant, "*é"))
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        // Press between the literal '*' and 'é', before the first drag motion.
        let at = Position {
            entry: 0,
            block: 0,
            byte: 1,
        };
        {
            let mut selection = app.chat_selection.borrow_mut();
            selection.anchor = Some(at);
            selection.focus = Some(at);
            selection.dragging = true;
        }
        h.edit(&id, |s| {
            s.entries.push(Entry::new(
                EntryKind::Reasoning,
                "**Unrelated later activity**",
            ));
            s.entries
                .push(Entry::new(EntryKind::Assistant, "Later response"));
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        assert_eq!(app.chat_selection.borrow().anchor, Some(at));
        assert!(
            app.chat_selection.borrow().dragging,
            "appends must not interrupt selection of unchanged earlier content"
        );
        h.edit(&id, |s| s.entries[0].text.push('*')).unwrap();
        dispatch(&mut app, Message::Changed).await;
        // The closing '*' reparses the block to just 'é'. Byte 1 is now inside
        // its UTF-8 encoding, so retaining that anchor would panic on Copy.
        assert!(app.chat_selection.borrow().anchor.is_none());
        assert!(!app.chat_selection.borrow().dragging);
    }
    #[tokio::test]
    async fn transcript_layout_work_is_bounded_across_scroll_resize_and_identity_changes() {
        use crate::chat_selection::tests::shaped_paragraphs;
        use iced::advanced::Renderer as _;
        use iced_runtime::{UserInterface, user_interface::Cache};
        let mut app = app();
        let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
        for i in 0..64 {
            session.entries.push(Entry::new(
                EntryKind::Assistant,
                format!("Message {i:03}: asymmetric café text."),
            ));
        }
        app.selected = Some(session.id.clone());
        app.current_session = Some(session);
        app.rebuild_transcript();
        app.transcript.viewport = 600.;
        app.transcript.offset = 1200.;
        app.prepare_visible();
        let mut renderer = crate::test_renderer::new(16.);
        let mut build = |app: &App, cache, width| {
            let before = shaped_paragraphs();
            let mut ui = UserInterface::build(
                app.view(),
                iced::Size::new(width, 900.),
                cache,
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            for &index in &app.transcript.order[app.transcript.visible()] {
                assert!(
                    bounds
                        .labels
                        .iter()
                        .any(|(text, _)| text == &app.current().unwrap().entries[index].text),
                    "the current viewport must display the current entries"
                );
            }
            ui.operate(
                &renderer,
                &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                    iced::advanced::widget::Id::new("chat"),
                    scrollable::AbsoluteOffset {
                        x: None,
                        y: Some(app.transcript.offset),
                    },
                ),
            );
            renderer.reset(iced::Rectangle::with_size(iced::Size::new(width, 900.)));
            ui.draw(
                &mut renderer,
                &theme::app(),
                &iced::advanced::renderer::Style {
                    text_color: theme::TEXT,
                },
                mouse::Cursor::Unavailable,
            );
            let shaped = shaped_paragraphs() - before;
            let viewport = bounds
                .scrolls
                .iter()
                .find(|(id, _)| id == &iced::advanced::widget::Id::new("chat"))
                .unwrap()
                .1;
            let text_region = bounds
                .labels
                .iter()
                .filter(|(text, _)| text.starts_with("Message "))
                .find_map(|(_, bounds)| {
                    let bounds = *bounds - iced::Vector::new(0., app.transcript.offset);
                    if bounds.y < viewport.y
                        || bounds.y + bounds.height > viewport.y + viewport.height
                    {
                        return None;
                    }
                    bounds.intersection(&viewport)
                })
                .expect("a complete transcript message must be visible");
            crate::test_renderer::assert_matches_fresh(
                &mut renderer,
                app.view(),
                iced::Size::new(width, 900.),
                "chat",
                app.transcript.offset,
                text_region,
            );
            (ui.into_cache(), shaped)
        };
        let (mut cache, count) = build(&app, Cache::new(), 1280.);
        assert!(
            count <= app.transcript.visible().len(),
            "shape only mounted entries"
        );
        let visible = app.transcript.visible();
        let row_height =
            app.transcript.prefix(visible.start + 1) - app.transcript.prefix(visible.start);
        app.transcript.offset += row_height;
        app.prepare_visible();
        assert_eq!(app.transcript.visible(), visible.start + 1..visible.end + 1);
        let (next, count) = build(&app, cache, 1280.);
        cache = next;
        assert!(count <= 1, "scrolling reshapes at most the entering entry");
        app.transcript.offset -= row_height;
        app.prepare_visible();
        let (next, count) = build(&app, cache, 1280.);
        cache = next;
        assert!(
            count <= 1,
            "returning to a viewport reuses unchanged survivors"
        );
        assert!(
            app.markdown.len() <= 2 * app.transcript.visible().len(),
            "retained text remains proportional to the viewport, not the history"
        );
        app.view_epoch += 1;
        let (next, count) = build(&app, cache, 1280.);
        cache = next;
        assert_eq!(
            count, 0,
            "measurement identity must not destroy text caches"
        );
        let (next, count) = build(&app, cache, 900.);
        cache = next;
        assert_eq!(count, 0, "width changes resize existing paragraphs");
        // Replacing a single entry identity must not reshape unrelated entries.
        let index = app.transcript.order[app.transcript.visible().start];
        let session = app.current_session.as_mut().unwrap();
        session.entries = session
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                if i == index {
                    Entry::new(
                        entry.kind.clone(),
                        format!("Message {i:03}: replacement café text."),
                    )
                } else {
                    entry.clone()
                }
            })
            .collect();
        app.prepare_visible();
        let (next, count) = build(&app, cache, 900.);
        cache = next;
        assert!(
            count <= 1,
            "only the replaced entry can require new shaping"
        );
        // Changing sessions must not trigger work outside the mounted viewport.
        let id = pico_core::store::id();
        app.current_session.as_mut().unwrap().id = id.clone();
        app.selected = Some(id);
        let (_, count) = build(&app, cache, 900.);
        assert!(
            count <= app.transcript.visible().len(),
            "shape only mounted entries"
        );
    }

    // Drive real redraw feedback and scroll operations, without wheel input.
    async fn settle_chat(
        app: &mut App,
        mut cache: iced_runtime::user_interface::Cache,
        renderer: &mut iced::Renderer,
        size: iced::Size,
    ) -> iced_runtime::user_interface::Cache {
        use iced::futures::StreamExt;
        use iced_runtime::UserInterface;
        let mut operations: Vec<Box<dyn iced::advanced::widget::Operation>> = vec![];
        for _ in 0..32 {
            let mut ui = UserInterface::build(app.center(), size, cache, renderer);
            for mut operation in operations.drain(..) {
                ui.operate(renderer, operation.as_mut());
            }
            let mut messages = vec![];
            ui.update(
                &[iced::Event::Window(window::Event::RedrawRequested(
                    iced::time::Instant::now(),
                ))],
                mouse::Cursor::Unavailable,
                renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            cache = ui.into_cache();
            if messages.is_empty() {
                return cache;
            }
            let mut tasks: std::collections::VecDeque<_> = messages
                .into_iter()
                .map(|message| app.update_inner(message))
                .collect();
            while let Some(task) = tasks.pop_front() {
                if let Some(mut stream) = iced_runtime::task::into_stream(task) {
                    while let Some(action) = stream.next().await {
                        match action {
                            iced_runtime::Action::Widget(operation) => operations.push(operation),
                            iced_runtime::Action::Output(message) => {
                                tasks.push_back(app.update_inner(message))
                            }
                            _ => panic!("geometry feedback must not cause external side effects"),
                        }
                    }
                }
            }
        }
        panic!("transcript geometry did not settle");
    }

    #[tokio::test]
    async fn fitting_chat_expansions_keep_prose_mounted_with_real_viewport_feedback() {
        use iced_runtime::user_interface::Cache;

        let mut app = app();
        let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
        session.entries.push(Entry::new(
            EntryKind::User { images: vec![] },
            "Please examine these changes.\n\nKeep the first message visible.",
        ));
        session.entries.push(Entry::new(
            EntryKind::Assistant,
            "I will inspect the **implementation**.\n\nThen I will verify the behavior.",
        ));
        let mut groups = vec![];
        let mut prose = vec![0, 1];
        for count in [13, 7] {
            let head = session.entries.len();
            for i in 0..count {
                session.entries.push(Entry::new(
                    EntryKind::Reasoning,
                    format!("**Checking item {i}**"),
                ));
                session.entries.push(Entry::new(
                    EntryKind::Tool {
                        name: "exec_command".into(),
                        input: format!(r#"{{"cmd":"check item {i}"}}"#),
                        output: "Checked".into(),
                        complete: true,
                        failed: false,
                        cell_id: None,
                        parent: None,
                        media: vec![],
                        web: None,
                    },
                    "",
                ));
            }
            groups.push(session.entries[head].id.clone());
            prose.push(session.entries.len());
            session.entries.push(Entry::new(EntryKind::Assistant,
                "Checks completed.\n\n- First result\n- Second result\nAll expected content remains visible."));
        }
        app.selected = Some(session.id.clone());
        app.current_session = Some(session);
        app.rebuild_transcript();
        app.prepare_visible();
        let mut renderer = crate::test_renderer::new(16.);
        let tall = iced::Size::new(930., 1350.);
        let mut cache = settle_chat(&mut app, Cache::new(), &mut renderer, tall).await;
        assert!(
            app.transcript.viewport > tall.height / 2. && app.transcript.viewport < tall.height
        );
        for (group, follow) in groups.iter().zip([false, true]) {
            app.auto_scroll = follow;
            let _ = app.update_inner(Message::ToggleGroup(group.clone()));
            cache = settle_chat(&mut app, cache, &mut renderer, tall).await;
            assert!(
                app.transcript.total() > 720.,
                "fixture must exceed the old fallback window"
            );
            assert!(
                app.transcript.total() < app.transcript.viewport,
                "total={} viewport={}",
                app.transcript.total(),
                app.transcript.viewport
            );
            assert_eq!(app.transcript.offset, 0.);
            assert_eq!(app.transcript.visible(), 0..app.transcript.order.len());
            for &index in &prose {
                assert!(app.transcript.order[app.transcript.visible()].contains(&index));
                let rendered = entry_text(&app, index);
                let entry = &app.current().unwrap().entries[index];
                if matches!(entry.kind, EntryKind::User { .. }) {
                    assert_eq!(rendered, entry.text);
                } else {
                    assert!(
                        rendered.contains("implementation") || rendered.contains("First result")
                    );
                    assert!(
                        !rendered.contains("**") && !rendered.contains("- First result"),
                        "prepared prose must render Markdown, not the raw-source fallback"
                    );
                }
            }
            // Reflow into overflow, then back to a fitting pane without scrolling.
            cache = settle_chat(&mut app, cache, &mut renderer, iced::Size::new(930., 500.)).await;
            assert!(
                app.transcript.total() > app.transcript.viewport,
                "the narrow pane overflows"
            );
            cache = settle_chat(&mut app, cache, &mut renderer, tall).await;
            assert_eq!(app.transcript.offset, 0.);
            assert_eq!(app.transcript.visible(), 0..app.transcript.order.len());
            let _ = app.update_inner(Message::ToggleGroup(group.clone()));
            cache = settle_chat(&mut app, cache, &mut renderer, tall).await;
        }
        // Session changes reset the index but can retain equal-sized widget trees.
        app.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
        app.view_epoch += 1;
        let id = pico_core::store::id();
        app.current_session.as_mut().unwrap().id = id.clone();
        app.selected = Some(id);
        app.rebuild_transcript();
        app.prepare_visible();
        let _ = settle_chat(&mut app, cache, &mut renderer, tall).await;
        assert!(
            app.transcript.viewport > tall.height / 2. && app.transcript.viewport < tall.height
        );
        assert_eq!(app.transcript.visible(), 0..app.transcript.order.len());
    }

    #[tokio::test]
    async fn opening_child_at_bottom_measures_offscreen_mounted_rows_without_scrolling() {
        use iced_runtime::{UserInterface, user_interface::Cache};
        use pico_core::agent::subagents::{Child, Profile};

        let mut app = app();
        let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
        session.child = Some(Child {
            parent_id: "parent".into(),
            profile: Profile::Review,
        });
        // Keep a large history unmounted while the final exchange is measured.
        for i in 0..200 {
            session.entries.push(Entry::new(
                EntryKind::Assistant,
                format!("Historical finding {i}"),
            ));
        }
        for (kind, text) in [
            (
                EntryKind::User { images: vec![] },
                "Inspect these changes.\n".repeat(6),
            ),
            (EntryKind::Reasoning, "Reviewing the implementation".into()),
            (
                EntryKind::Assistant,
                "An earlier finding with **details**.\n\n".repeat(20),
            ),
            (
                EntryKind::User { images: vec![] },
                "Please verify the result.\n".repeat(7),
            ),
            (EntryKind::Reasoning, "Checking the regression".into()),
            (
                EntryKind::Assistant,
                "Verification completed.\n\nThe result is consistent.".into(),
            ),
            (EntryKind::Reasoning, "Preparing the final report".into()),
            (
                EntryKind::Assistant,
                "Final report with **verified results**.\n\n".repeat(30),
            ),
        ] {
            session.entries.push(Entry::new(kind, text));
        }
        app.selected = Some(session.id.clone());
        app.current_session = Some(session);
        app.auto_scroll = true;
        app.transcript = crate::virtual_list::List::with_overscan(256.);
        app.rebuild_transcript();
        // Explicit fixture: this mounted reply was estimated too short.
        app.transcript.measure(202, 1.);
        app.transcript.end();
        app.prepare_visible();
        assert!(app.transcript.visible().contains(&202));
        assert!(
            app.transcript.visible().start > 0,
            "historical entries remain virtualized"
        );

        let estimate = app.transcript.prefix(203) - app.transcript.prefix(202);
        let mut renderer = header_renderer();
        let size = iced::Size::new(1462., 1320.);
        let mut ui = UserInterface::build(app.center(), size, Cache::new(), &mut renderer);
        // Opening a child snaps the actual scrollable to the bottom before its
        // first redraw. The earlier tall reply is mounted but above the viewport.
        ui.operate(
            &renderer,
            &mut iced::advanced::widget::operation::scrollable::snap_to::<()>(
                widget::Id::new("chat"),
                scrollable::RelativeOffset::END.into(),
            ),
        );
        let cache = ui.into_cache();
        let _ = settle_chat(&mut app, cache, &mut renderer, size).await;
        assert!(app.auto_scroll);
        assert!(
            app.transcript.prefix(203) - app.transcript.prefix(202) > estimate,
            "the tall offscreen mounted reply must be measured rather than retain its estimate"
        );
        let visible = app.transcript.visible();
        assert_eq!(visible.end, app.transcript.order.len());
        assert!(visible.start > 0, "measurement must retain virtualization");
        assert!(entry_text(&app, 207).contains("Final report with verified results."));
    }

    #[tokio::test]
    async fn chat_measurements_prepare_newly_exposed_rows_without_a_scroll() {
        let mut app = app();
        let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
        for i in 0..30 {
            session.entries.push(Entry::new(
                EntryKind::Assistant,
                format!("**Row {i}** with λ"),
            ));
        }
        app.current_session = Some(session);
        app.auto_scroll = false;
        app.rebuild_transcript();
        app.transcript.viewport = 240.;
        app.prepare_visible();
        let epoch = app.view_epoch;
        dispatch(&mut app, Message::RowSize(epoch, 0, 1000.)).await;
        assert_eq!(app.transcript.visible(), 0..1);
        assert_eq!(entry_text(&app, 0), "Row 0 with λ");
        // Shrinking an entry at the viewport start does not move the anchor,
        // but must still prepare Markdown for the rows that just became visible.
        dispatch(&mut app, Message::RowSize(epoch, 0, 20.)).await;
        assert_eq!(app.transcript.offset, 0.);
        assert!(app.transcript.visible().end > 1);

        for &i in &app.transcript.order[app.transcript.visible()] {
            assert_eq!(entry_text(&app, i), format!("Row {i} with λ"));
        }
        // A scrollbar jump refreshes the narrow window in one update, and end
        // anchoring uses the actual small viewport rather than the 600px fallback.
        dispatch(&mut app, Message::Scroll(epoch, 2020., 240., false)).await;
        assert!(app.transcript.visible().start > 1);
        for &i in &app.transcript.order[app.transcript.visible()] {
            assert_eq!(entry_text(&app, i), format!("Row {i} with λ"));
        }

        app.transcript.end();
        app.prepare_visible();
        assert_eq!(app.transcript.visible().end, app.transcript.order.len());
        assert_eq!(entry_text(&app, 29), "Row 29 with λ");
    }

    #[tokio::test]
    async fn long_transcript_only_caches_viewport_and_appends_preserve_heights() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path).await;
        let h = app.harness.as_ref().unwrap().clone();
        let id = app.selected.clone().unwrap();
        h.edit(&id, |s| {
            for i in 0..20000 {
                s.entries.push(Entry::new(
                    EntryKind::Assistant,
                    format!("## Message {i}\n\nDistinct body"),
                ));
            }
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        assert_eq!(app.transcript.order.len(), 20000);
        assert!(app.markdown.len() < 100);
        app.transcript.measure(19999, 333.);
        let before = app.transcript.total();
        h.edit(&id, |s| {
            s.entries
                .push(Entry::new(EntryKind::Assistant, "new entry"))
        })
        .unwrap();
        dispatch(&mut app, Message::Changed).await;
        assert_eq!(
            app.transcript.prefix(20000),
            before,
            "existing row heights survive append"
        );
        assert_eq!(
            app.transcript.prefix(20000) - app.transcript.prefix(19999),
            333.
        );
        assert!(
            app.transcript.total() > before,
            "appended entry contributes height"
        );
        assert!(app.markdown.len() < 100);
        let _ = app.view();
    }
    #[tokio::test]
    async fn popup_transitions_preserve_sidebar_and_chat_scroll() {
        use iced::advanced::widget::{Id, Operation, operation::Scrollable};
        use iced_runtime::{UserInterface, user_interface::Cache};
        #[derive(Default)]
        struct Offsets(Vec<(String, f32)>);
        impl Operation for Offsets {
            fn traverse(&mut self, f: &mut dyn FnMut(&mut dyn Operation)) {
                f(self);
            }
            fn scrollable(
                &mut self,
                id: Option<&Id>,
                _: iced::Rectangle,
                _: iced::Rectangle,
                translation: iced::Vector,
                _: &mut dyn Scrollable,
            ) {
                for name in ["sessions", "chat"] {
                    if id == Some(&Id::new(name)) {
                        self.0.push((name.into(), translation.y));
                    }
                }
            }
        }
        let mut app = app();
        let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
        for i in 0..64 {
            session.entries.push(Entry::new(
                EntryKind::Assistant,
                format!("Message {i}: asymmetric café text."),
            ));
            app.sessions.push(SessionSummary {
                child: None,
                id: format!("scroll-session-{i}"),
                title: format!("Session {i}"),
                project: app.smoke_dir.clone().unwrap(),
                updated: 0,
                interrupted: false,
                needs_attention: false,
                unchecked_completion: false,
            });
        }
        app.selected = Some(session.id.clone());
        app.current_session = Some(session);
        app.rebuild_transcript();
        app.prepare_visible();
        app.prepare_sidebar();
        let mut renderer = header_renderer();
        let size = iced::Size::new(1280., 900.);
        let mut ui = UserInterface::build(app.view(), size, Cache::new(), &mut renderer);
        for (name, y) in [("sessions", 370.), ("chat", 810.)] {
            ui.operate(
                &renderer,
                &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                    Id::new(name),
                    scrollable::AbsoluteOffset {
                        x: None,
                        y: Some(y),
                    },
                ),
            );
        }
        let mut before = Offsets::default();
        ui.operate(&renderer, &mut before);
        assert_eq!(
            before.0,
            vec![("sessions".into(), 370.), ("chat".into(), 810.)]
        );
        let mut cache = ui.into_cache();
        // Keep the real widget cache across every transition. Application-level
        // viewport offsets alone cannot detect scrollable state being discarded.
        for (message, expected_popups) in [
            (
                Message::SessionMenu("scroll-session-0".into(), iced::Point::new(120., 230.)),
                (true, false, false),
            ),
            (Message::CloseMenu, (false, false, false)),
            (
                Message::SessionMenu("scroll-session-0".into(), iced::Point::new(120., 230.)),
                (true, false, false),
            ),
            (
                Message::Rename("scroll-session-0".into()),
                (false, true, false),
            ),
            (Message::Dismiss, (false, false, false)),
            (
                Message::Delete("scroll-session-0".into()),
                (false, true, false),
            ),
            (Message::Dismiss, (false, false, false)),
            (
                Message::FooterMenu(FooterControl::Model),
                (false, false, true),
            ),
            (Message::CloseFooterMenu, (false, false, false)),
        ] {
            let transition = format!("{message:?}");
            let _ = app.update(message);
            assert_eq!(
                (
                    app.context_menu.is_some(),
                    app.modal.is_some(),
                    app.footer_menu.is_some()
                ),
                expected_popups,
                "{transition}",
            );
            let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
            let mut offsets = Offsets::default();
            ui.operate(&renderer, &mut offsets);
            assert_eq!(
                offsets.0, before.0,
                "scroll positions changed on {transition}"
            );
            cache = ui.into_cache();
        }
    }
    #[tokio::test]
    async fn session_menu_targets_the_clicked_session_and_releases_deleted_output() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path.clone()).await;
        let first = app.selected.clone().unwrap();
        stored_session(&mut app, path).await;
        let second = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        h.edit(&first, |s| {
            s.entries.push(Entry {
                id: "long-tool".into(),
                text: String::new(),
                kind: EntryKind::Tool {
                    name: "exec".into(),
                    input: "code".into(),
                    output: "long output".repeat(10000),
                    complete: true,
                    failed: false,
                    cell_id: None,
                    parent: None,
                    media: vec![],
                    web: None,
                },
            })
        })
        .unwrap();
        dispatch(&mut app, Message::Select(first.clone())).await;
        dispatch(&mut app, Message::ToggleEntry("long-tool".into())).await;
        assert!(app.outputs.contains_key("long-tool"));
        dispatch(&mut app, Message::ToggleEntry("long-tool".into())).await;
        assert!(!app.outputs.contains_key("long-tool"));
        dispatch(&mut app, Message::ToggleEntry("long-tool".into())).await;
        dispatch(&mut app, Message::Select(second.clone())).await;
        assert!(app.outputs.is_empty());
        dispatch(
            &mut app,
            Message::SessionMenu(first.clone(), iced::Point::new(120., 230.)),
        )
        .await;
        dispatch(
            &mut app,
            Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('x'))),
        )
        .await;
        assert!(app.editor.text().is_empty());
        dispatch(&mut app, Message::Rename(first.clone())).await;
        dispatch(
            &mut app,
            Message::ModalInput("Renamed other session".into()),
        )
        .await;
        dispatch(&mut app, Message::Confirm).await;
        assert_eq!(app.selected.as_deref(), Some(second.as_str()));
        assert_eq!(
            h.load(&first).unwrap().snapshot().title,
            "Renamed other session"
        );
        dispatch(
            &mut app,
            Message::SessionMenu(first.clone(), iced::Point::new(120., 230.)),
        )
        .await;
        assert_eq!(
            copied_text(app.update(Message::CopySessionId(first.clone()))).await,
            first
        );
        assert!(app.context_menu.is_none());
        assert_eq!(app.selected.as_deref(), Some(second.as_str()));
        dispatch(
            &mut app,
            Message::SessionMenu(first.clone(), iced::Point::new(120., 230.)),
        )
        .await;
        dispatch(&mut app, Message::CloseMenu).await;
        assert!(app.context_menu.is_none());
        dispatch(
            &mut app,
            Message::SessionMenu(first.clone(), iced::Point::new(120., 230.)),
        )
        .await;
        dispatch(&mut app, Message::Delete(first.clone())).await;
        dispatch(&mut app, Message::Confirm).await;
        assert!(h.get(&first).is_none());
        assert_eq!(app.selected.as_deref(), Some(second.as_str()));
        assert!(app.outputs.is_empty());
        dispatch(
            &mut app,
            Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('d'))),
        )
        .await;
        dispatch(&mut app, Message::Delete(second)).await;
        dispatch(&mut app, Message::Confirm).await;
        assert!(app.selected.is_none());
        assert!(app.editor.text().is_empty());
        assert!(app.error.is_none());
    }
    #[tokio::test]
    async fn change_stream_delivers_pre_subscription_and_coalesced_updates() {
        use iced::futures::StreamExt;
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path).await;
        let id = app.selected.clone().unwrap();
        let h = app.harness.as_ref().unwrap().clone();
        h.edit(&id, |s| s.title = "before subscription".into())
            .unwrap();
        let mut stream = updates(&Updates(h.clone()));
        let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap();
        dispatch(&mut app, event).await;
        assert_eq!(app.current().unwrap().title, "before subscription");
        h.edit(&id, |s| s.title = "first change".into()).unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap();
        h.edit(&id, |s| s.title = "latest change while event queued".into())
            .unwrap();
        dispatch(&mut app, event).await;
        assert_eq!(
            app.current().unwrap().title,
            "latest change while event queued"
        );
        // A refresh may acknowledge the newer revision too. Permit either
        // coalescing it away or delivering one already-queued notification.
        if let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(80), stream.next()).await
        {
            dispatch(&mut app, event).await;
        }
        assert!(
            !app.refresh(),
            "unchanged sessions should not be resnapshotted"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(80), stream.next())
                .await
                .is_err(),
            "idle subscription must stay asleep"
        );
    }
    #[tokio::test]
    async fn modal_blocks_composer_edits_and_failed_draft_save_keeps_text() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        stored_session(&mut app, path).await;
        dispatch(
            &mut app,
            Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('a'))),
        )
        .await;
        let selected = app.selected.clone().unwrap();
        dispatch(&mut app, Message::Rename(selected)).await;
        dispatch(
            &mut app,
            Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('x'))),
        )
        .await;
        assert_eq!(app.editor.text(), "a");
        dispatch(&mut app, Message::Dismiss).await;
        let session = app.current().unwrap().id.clone();
        let db = rusqlite::Connection::open(
            app.harness
                .as_ref()
                .unwrap()
                .store
                .root()
                .join("sessions.sqlite3"),
        )
        .unwrap();
        sqlite::fail_writes(&db, "sessions", "fixture disk failure");
        dispatch(&mut app, Message::SaveDraft).await;
        assert_eq!(app.editor.text(), "a");
        assert!(app.draft_dirty);
        assert!(app.error.as_ref().unwrap().contains("fixture disk failure"));
        sqlite::allow_writes(&db, "sessions");
        dispatch(&mut app, Message::SaveDraft).await;
        assert!(!app.draft_dirty);
        assert_eq!(
            app.harness
                .as_ref()
                .unwrap()
                .get(&session)
                .unwrap()
                .snapshot()
                .draft,
            "a"
        );
    }
    #[tokio::test]
    async fn invalid_project_stays_in_modal_without_creating_state() {
        let mut app = app();
        dispatch(&mut app, Message::AddProject).await;
        app.modal_input = app
            .smoke_dir
            .as_ref()
            .unwrap()
            .join("does-not-exist")
            .display()
            .to_string();
        dispatch(&mut app, Message::Confirm).await;
        assert!(matches!(app.modal, Some(Modal::AddProject)));
        assert!(app.error.as_ref().unwrap().contains("Cannot open"));
        assert!(app.catalog.projects.is_empty());
    }
}
