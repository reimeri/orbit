mod changes;
pub(crate) mod find;
mod forks;
mod imports;
mod marketing;
mod prompt_rail;
mod settings;
mod sidebar;
mod smoke;
mod usage_panel;
mod worktrees;

use sidebar::{FiledSection, SidebarRow};

use crate::activity::{self, Row as ActivityRow};
use crate::editor_history::{Direction, Draft as DraftKey, DraftHistories};
use crate::scrollbar::scroll;
use crate::session_wave::Status as SessionStatus;
use crate::theme::{self, card, footer_surface, panel, right_panel, secondary, sidebar_control};
use crate::{
    chat_selection::{self, Position},
    chevron_icon::chevron_icon,
    ellipsized_text::ellipsized_text,
    icons::{Icon, tip as icon_tip},
    pointer_input::{Press, dismiss_layer, pointer_input},
    spellcheck, tool_output,
};
use iced::Theme;
use iced::futures::SinkExt;
use iced::{
    Color, Element, Fill, Size, Subscription, Task, keyboard, mouse,
    widget::{
        self, column, container, markdown, mouse_area, opaque, row, scrollable, space, stack, text,
        text_editor, text_input, tooltip,
    },
    window,
};
use orbit_core::CancellationToken;
use orbit_core::{
    agent::{
        FilingAction, FilingOutcome, Harness,
        subagents::{ChildStatus, Profile, RunInfo, RunKey},
    },
    auth::Login,
    models::{MODELS, model},
    shell::CommandStatus,
    store::{
        Catalog, Entry, EntryKind, Filing, FilingKind, NewSessionDraft, Project, Session,
        SessionSummary, Settings, Store, UserMessage,
    },
};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
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
    ForkPoint,
    Top,
    Entry {
        session: String,
        entry: String,
        role: usize,
    },
    Bottom,
}

// Only keep geometry for the row being read, not an inactive transcript.
struct ChatPosition {
    row: usize,
    inside: f32,
    height: f32,
    markdown: Option<(String, crate::markdown_window::Heights)>,
}

struct PendingSettings {
    submitted: Settings,
    close_on_success: bool,
}

#[derive(Clone, Debug)]
pub enum SettingsSave {
    Saved,
    NotSaved(String),
    PublishedButNotSynced(String),
}

type PendingLogin = Arc<tokio::sync::Mutex<Option<Login>>>;

const RIGHT_PANEL_PADDING: f32 = 18.;
const TOOLBAR_BUTTON_HEIGHT: f32 = 26.;
const TOOLBAR_INSET: f32 = 6.;
const FOOTER_HEIGHT: f32 = 36.;
const FOOTER_MENU_TEXT_SIZE: f32 = 12.;
const FOOTER_MENU_ROW_HEIGHT: f32 = 30.;
const FOOTER_MENU_ROW_GAP: f32 = 2.;
const CHAT_VERTICAL_PADDING: f32 = 20.;
const CHAT_OVERSCAN: f32 = 120.;
const COMPOSER_LINE_HEIGHT_FACTOR: f32 = 1.3;
const COMPOSER_MIN_LINES: usize = 1;
const COMPOSER_MAX_LINES: usize = 10;
const NOTICE_DURATION: Duration = Duration::from_secs(8);
const ACTIVITY_INTERVAL: Duration = Duration::from_millis(100);

struct Markdown {
    source: Arc<Entry>,
    content: markdown::Content,
    images: HashSet<String>,
    window: Option<crate::markdown_window::Window>,
    // Small eager replies can cross the threshold mid-stream while being read.
    eager_heights: std::cell::RefCell<Vec<f32>>,
}

impl Markdown {
    fn sync_window(&mut self, font_size: f32, from: usize) {
        let size = if matches!(self.source.kind, EntryKind::Reasoning) {
            12.
        } else {
            font_size
        };
        crate::markdown_window::Window::sync(&mut self.window, self.content.items(), from, size);
    }

    fn refresh_images(&mut self) {
        self.images.clear();
        if !self.content.images().is_empty() {
            let items = self.content.items();
            let visible = self.window.as_ref().map_or(items, |w| w.visible(items));
            self.images = crate::markdown_images::urls(visible);
        }
    }
    fn view<'a>(
        &'a self,
        settings: markdown::Settings,
        viewer: &chat_selection::Viewer<'a>,
    ) -> Element<'a, Message> {
        if let Some(window) = &self.window {
            window.view(self.content.items(), settings, viewer)
        } else {
            crate::transcript_viewport::observe_layout(
                markdown::view_with(self.content.items(), settings, viewer),
                move |layout, _| {
                    let mut heights = self.eager_heights.borrow_mut();
                    heights.clear();
                    // Include the trailing gap: on handover, the old tail is now
                    // followed by more content.
                    heights.extend(
                        layout
                            .children()
                            .map(|child| child.bounds().height + settings.spacing.0),
                    );
                    None
                },
            )
        }
    }
}

fn transcript_run_key(entry: &Entry) -> Option<RunKey<'_>> {
    if let Some(id) = activity::subagent_notice(entry) {
        Some(RunKey::Id(id))
    } else if matches!(&entry.kind, EntryKind::Tool { name, .. } if name == "spawn_subagent") {
        Some(RunKey::Request(&entry.id))
    } else {
        None
    }
}

/// Parts of the displayed frame supplied by the capture instead of the harness.
#[derive(Default)]
struct SmokeFixtures {
    questionnaire: Option<String>,
    transcript: bool,
    sidebar: bool,
}

pub struct App {
    harness: Option<Arc<Harness>>,
    catalog: Catalog,
    sessions: Vec<SessionSummary>,
    current_session: Option<Session>,
    draft_session: Option<Session>,
    new_draft: NewSessionDraft,
    new_draft_dirty: bool,
    usage_summary: Option<Result<Arc<orbit_core::store::activity::Summary>, String>>,
    transcript: crate::virtual_list::List,
    activity: activity::Projection,
    execution: HashSet<String>,
    session_list_focus: crate::session_list_focus::SessionListFocus,
    sidebar_rows: Vec<SidebarRow>,
    child_statuses: HashMap<String, ChildStatus>,
    sidebar_list: crate::virtual_list::List,
    filed: [FiledSection; 2],
    catalog_revision: u64,
    view_epoch: u64,
    sidebar_epoch: u64,
    sidebar_keys: HashMap<String, usize>,
    io: crate::io_worker::Worker,
    tasks: Vec<Task<Message>>,
    io_busy: bool,
    switching_session: bool,
    closing: bool,
    close_requested: bool,
    submissions: HashMap<String, (UserMessage, String)>,
    current_revision: u64,
    outputs: HashMap<String, tool_output::Output>,
    historical_runs: HashMap<String, Option<RunInfo>>,
    historical_read_pending: bool,
    raw_tool_inputs: HashSet<String>,
    recent_previews: crate::recent_previews::RecentPreviews,
    image_view: crate::image_view::Viewer,
    markdown_images: crate::markdown_images::Cache,
    equations: crate::equations::Cache,
    api_key_draft: String,
    api_key_configured: bool,
    forks: forks::Index,
    fork_panel: Option<forks::Panel>,
    jump_after_select: Option<(String, String)>,
    context_menu: Option<ContextMenu>,
    stashed_open: bool,
    footer_menu_open: bool,
    selected: Option<String>,
    search: String,
    search_active: bool,
    find: Option<find::Find>,
    find_serial: u64,
    editor: text_editor::Content,
    histories: DraftHistories,
    spellcheck: spellcheck::State,
    file_completion: crate::file_completion::Completion,
    attachments: Vec<orbit_core::media::Media>,
    imports: imports::Imports,
    questionnaires: HashMap<String, questionnaire::Draft>,
    active_questionnaire: Option<String>,
    chat_selection: chat_selection::Shared,
    markdown: HashMap<String, Markdown>,
    markdown_heights: HashMap<String, crate::markdown_window::Heights>,
    expanded: HashSet<String>,
    groups: HashSet<String>,
    settings_open: bool,
    agents_open: bool,
    changes: changes::State,
    worktrees: worktrees::State,
    chat_positions: HashMap<String, ChatPosition>,
    settings_tab: SettingsTab,
    notice: Option<(String, std::time::Instant)>,
    settings: Settings,
    pending_settings: Option<PendingSettings>,
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
    hold_prompt_at_bottom: bool,
    sidebar_follow_top: bool,
    sidebar_branches: HashMap<PathBuf, String>,
    git: HashMap<PathBuf, crate::git::Info>,
    git_pending: HashSet<PathBuf>,
    git_counts: Arc<crate::git::LineCounts>,
    draft_dirty: bool,
    launch: crate::Launch,
    smoke_stage: usize,
    smoke_fixtures: SmokeFixtures,
    screenshot_check: Option<crate::screenshot_check::Check>,
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
enum FooterControl {
    Model,
    Effort,
}
#[derive(Clone, Debug)]
struct ContextMenu(MenuTarget, iced::Point);
#[derive(Clone, Debug)]
enum MenuTarget {
    Session(String),
    Project(PathBuf),
}
#[derive(Clone, Debug)]
enum Modal {
    Image(orbit_core::media::Media),
    MarkdownImage(Arc<crate::markdown_images::Loaded>),
    AddProject,
    RenameProject(PathBuf),
    DeleteProject(PathBuf),
    Rename(String),
    Delete(String),
    StopCommandsAndFile(String, FilingKind),
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
    Find(find::Action),
    FileCompletion(crate::file_completion::Message),
    Notification(crate::notifications::Message),
    Image(crate::image_view::Message),
    OpenImage(orbit_core::media::Media),
    OpenImageFrom(Box<Message>, crate::image_transition::Source),
    MarkdownImage(crate::markdown_images::Message),
    RequestEquation(String),
    CopyEquation {
        entry_id: String,
        block: usize,
        source: String,
    },
    ClearEquationCopy(std::time::Instant),
    EquationRendered(crate::equations::Rendered),
    OpenMarkdownImage(String),
    RetryMarkdownImage(String),
    ImageBackend(orbit_core::image_tools::Backend),
    Spellcheck(spellcheck::Message),
    SpellcheckEnabled(bool),
    SpellcheckLanguage(String),
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
    DraftSaved(String, Result<(), String>),
    Paste,
    ClipboardImageOffered(u64),
    AttachmentLoaded(Result<crate::attachments::Input, String>),
    AttachmentPublished(Result<(), String>),
    AttachmentSaveFailed(String, Option<crate::attachments::Input>),
    RemoveAttachment(usize),
    Deleted(String, Result<(), String>),
    Edited(Result<(), String>),
    Tick,
    Changed,
    GitTick,
    SaveDraft,
    AgeTick,
    UsageSummary(Result<Arc<orbit_core::store::activity::Summary>, String>),
    Press(Press),
    SessionMenu(String, iced::Point),
    ProjectMenu(PathBuf, iced::Point),
    RenameProject(PathBuf),
    StashProject(PathBuf),
    DeleteProject(PathBuf),
    ProjectDeleted(PathBuf, Result<(), String>),
    ToggleStashed,
    CopySessionId(String),
    CloseMenu,
    Fork(String, String),
    ShowForks(Option<String>),
    ViewOriginal(String, String),
    ToggleFooterMenu,
    CloseFooterMenu,
    ToolOutput(String, tool_output::Event),
    CopySelection,
    New,
    Select(String),
    FileSession(String, FilingAction),
    RunningCommandsAtFile(String, FilingKind),
    ToggleFiled(FilingKind),
    FiledScroll(FilingKind, u64, f32, f32),
    Create(PathBuf),
    Search(String),
    ToggleSearch,
    Favorite(PathBuf),
    FocusComposer,
    Editor(text_editor::Action),
    EditorScroll(crate::growing_editor::ScrollTo),
    DeletePreviousWord,
    ComposerHistory(Direction),
    Questionnaire(String, questionnaire::Message),
    Send,
    ToggleSettings,
    ToggleAgents,
    Changes(changes::Action),
    Worktree(worktrees::Action),
    StopAgent(String),
    ApplySettings,
    CatalogSaved(Result<(), String>),
    SettingsSaved(SettingsSave),
    SettingsTab(SettingsTab),
    Model(String),
    ModelEffort(String, String),
    ModelFast(String, bool),
    SubagentModel(Profile, orbit_core::models::Model),
    SubagentEffort(Profile, String),
    SubagentFast(Profile, bool),
    SessionFast(bool),
    SessionSettingsEdited(Result<(), String>),
    NoticeDismiss,
    NoticeTick,
    SessionModel(String),
    SessionEffort(String),
    Context(String),
    Threshold(String),
    Theme(&'static str),
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
    HistoricalRun(String, String, Result<Option<RunInfo>, String>),
    ToggleToolRaw(String),
    ToggleGroup(String),
    ToggleExecution(String),
    Scroll(u64, f32, f32, bool),
    JumpToLatest,
    JumpToPrompt(String),
    RowSize(u64, usize, f32),
    MarkdownGeometry(
        u64,
        usize,
        Arc<Entry>,
        f32,
        Option<crate::markdown_window::Geometry>,
    ),
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
    SidebarBranch(PathBuf, Option<String>),
    Git(PathBuf, Option<crate::git::Info>),
    External(Result<(), String>),
    ErrorDismiss,
    ResizeStart(bool, iced::Point),
    Event(iced::Event),
    WindowOpened(window::Id),
    Screenshot(window::Screenshot),
    CheckScreenshot(window::Screenshot),
    SmokeOpenTool(String),
    Capture,
    Exit(Result<(), String>),
}
impl App {
    fn preset(&self) -> &'static theme::Preset {
        theme::preset(&self.catalog.settings.theme)
    }

    pub fn theme(&self) -> Theme {
        self.preset().theme.clone()
    }

    pub fn new(launch: crate::Launch, smoke_passed: Arc<AtomicBool>) -> (Self, Task<Message>) {
        let smoke_dir = (!matches!(launch, crate::Launch::Normal))
            .then(|| std::env::temp_dir().join(format!("orbit-smoke-{}", orbit_core::store::id())));
        let loaded = (|| -> Result<_, String> {
            let store = if let Some(path) = &smoke_dir {
                Store::open(path.clone())
            } else {
                Store::from_env()
            }
            .map_err(|e| format!("{e:#}"))?;
            let (catalog, sessions, mut warnings) = store.load().map_err(|e| format!("{e:#}"))?;
            let draft = store.load_new_draft().map_err(|e| format!("{e:#}"))?;
            let harness = Harness::new(store, sessions).map_err(|e| format!("{e:#}"))?;
            warnings.extend_from_slice(&harness.startup_warnings);
            Ok::<_, String>((harness, catalog, draft, warnings))
        })();
        let (harness, catalog, new_draft, error) = match loaded {
            Ok((h, c, d, w)) => (Some(h), c, d, (!w.is_empty()).then(|| w.join("\n"))),
            Err(error) => (
                None,
                Catalog::default(),
                NewSessionDraft::default(),
                Some(format!(
                    "Cannot open Orbit storage: {error}. Fix the indicated file/permissions and restart. Nothing has been reset."
                )),
            ),
        };
        let selected = if catalog.settings.resume_last {
            catalog.selected.clone()
        } else {
            None
        };
        let settings = catalog.settings.clone();
        let io = crate::io_worker::Worker::new(catalog.clone());
        let mut app = Self {
            harness,
            catalog,
            sessions: vec![],
            current_session: None,
            draft_session: None,
            new_draft,
            new_draft_dirty: false,
            usage_summary: None,
            transcript: crate::virtual_list::List::with_overscan(CHAT_OVERSCAN),
            activity: activity::Projection::default(),
            execution: HashSet::new(),
            session_list_focus: Default::default(),
            sidebar_rows: vec![],
            child_statuses: HashMap::new(),
            sidebar_list: crate::virtual_list::List::default(),
            filed: std::array::from_fn(|_| FiledSection::new()),
            catalog_revision: u64::MAX,
            view_epoch: 0,
            sidebar_epoch: 0,
            sidebar_keys: HashMap::new(),
            io,
            tasks: vec![],
            io_busy: false,
            switching_session: false,
            closing: false,
            close_requested: false,
            submissions: HashMap::new(),
            attachments: vec![],
            imports: Default::default(),
            current_revision: 0,
            outputs: HashMap::new(),
            historical_runs: HashMap::new(),
            historical_read_pending: false,
            raw_tool_inputs: HashSet::new(),
            recent_previews: Default::default(),
            image_view: Default::default(),
            markdown_images: Default::default(),
            equations: Default::default(),
            api_key_draft: String::new(),
            api_key_configured: false,
            forks: Default::default(),
            fork_panel: None,
            jump_after_select: None,
            context_menu: None,
            stashed_open: false,
            footer_menu_open: false,
            selected,
            search: String::new(),
            search_active: false,
            find: None,
            find_serial: 0,
            editor: text_editor::Content::new(),
            histories: DraftHistories::default(),
            spellcheck: spellcheck::State::new(),
            file_completion: Default::default(),
            questionnaires: HashMap::new(),
            active_questionnaire: None,
            chat_selection: Default::default(),
            markdown: HashMap::new(),
            markdown_heights: HashMap::new(),
            expanded: HashSet::new(),
            groups: HashSet::new(),
            settings_open: false,
            pending_settings: None,
            agents_open: false,
            changes: Default::default(),
            worktrees: Default::default(),
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
            hold_prompt_at_bottom: false,
            sidebar_follow_top: true,
            sidebar_branches: HashMap::new(),
            git: HashMap::new(),
            git_pending: HashSet::new(),
            git_counts: Arc::default(),
            draft_dirty: false,
            launch,
            smoke_stage: 0,
            smoke_fixtures: SmokeFixtures::default(),
            screenshot_check: None,
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
        let counts = self.git_counts.clone();
        let detailed = self.changes.open;
        Task::future(async move {
            let (info, files) = crate::git::capture(path.clone(), counts, detailed).await;
            if detailed {
                Message::Changes(changes::Action::Snapshot(path, info, files))
            } else {
                Message::Git(path, info)
            }
        })
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
        let mut active = self.smoke_fixtures.questionnaire.clone();
        let mut live: HashSet<_> = active.iter().cloned().collect();
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
            if let Some(previous) = self
                .active_questionnaire
                .as_ref()
                .and_then(|id| self.questionnaires.get_mut(id))
            {
                previous.separate_edits();
            }
            if let Some(id) = &active
                && !self.settings_open
                && !self.search_active
                && self.find.is_none()
                && self.modal.is_none()
                && !self.footer_menu_open
                && self.worktrees.popup.is_none()
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
        if previous != live && !self.smoke_fixtures.sidebar {
            self.prepare_sidebar();
        }
    }
    fn refresh(&mut self) -> bool {
        self.sync_questionnaires();
        let Some(h) = &self.harness else {
            return false;
        };
        let catalog_revision = h.catalog_revision();
        if catalog_revision != self.catalog_revision {
            self.catalog_revision = catalog_revision;
            self.child_statuses = h.child_statuses();
            if !self.smoke_fixtures.sidebar {
                self.sessions = h.summaries();
                self.sessions
                    .sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
                self.forks = forks::Index::build(&self.sessions);
                self.prepare_sidebar();
            }
        }
        // The worker may already have evicted the outgoing conversation.
        // Keep its view mounted until Selected commits the incoming snapshot.
        if self.switching_session || self.smoke_fixtures.transcript {
            return false;
        }
        let h = self.harness.as_ref().expect("harness");
        let Some(conversation) = self.selected.as_ref().and_then(|id| h.get(id)) else {
            if self.current_session.take().is_some() {
                self.find = None;
                self.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
                self.activity = activity::Projection::default();
                self.view_epoch = self.view_epoch.wrapping_add(1);
            }
            self.outputs.clear();
            self.historical_runs.clear();
            self.markdown.clear();
            self.markdown_heights.clear();
            self.chat_selection.borrow_mut().clear();
            self.prepare_visible();
            return false;
        };
        let revision = conversation.revision();
        let previous = self
            .current_session
            .as_ref()
            .filter(|s| s.id == conversation.id);
        if previous.is_some() && self.current_revision == revision {
            return false;
        }
        self.current_revision = revision;
        let snapshot = conversation.snapshot();
        if previous.is_none() {
            self.hold_prompt_at_bottom = false;
            self.outputs.clear();
            self.historical_runs.clear();
            self.markdown.clear();
            self.markdown_heights.clear();
            self.chat_selection.borrow_mut().clear();
            self.view_epoch = self.view_epoch.wrapping_add(1);
            self.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
            self.activity = activity::Projection::default();
        }
        // The history diff skips shared subtrees.
        let empty = orbit_core::history::History::default();
        let old = previous.map_or(&empty, |session| &session.entries);
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
                let was_eager = markdown.window.is_none();
                let from = if let Some(suffix) = entry.text.strip_prefix(&markdown.source.text) {
                    markdown.content.push_str(suffix)
                } else {
                    markdown.content = markdown::Content::parse(&entry.text);
                    markdown.window = None;
                    markdown.eager_heights.get_mut().clear();
                    0
                };
                markdown.source = snapshot.entries.shared(index);
                markdown.sync_window(self.catalog.settings.font_size, from);
                if was_eager
                    && let Some(window) = &mut markdown.window
                    && let Some(pos) = self.transcript.position(index)
                {
                    window.restore(
                        markdown
                            .eager_heights
                            .take()
                            .into_iter()
                            .enumerate()
                            .collect(),
                        self.transcript.offset - self.transcript.prefix(pos),
                        self.transcript.viewport,
                    );
                }
                markdown.refresh_images();
            }
        }
        let bookmark = self.chat_positions.remove(&snapshot.id);
        self.current_session = Some(snapshot);
        self.find_sync();
        self.acknowledge_focused_completion();
        self.append_transcript();
        if let Some(bookmark) = bookmark
            && let Some(pos) = self.transcript.position(bookmark.row)
        {
            // The rest of the rebuilt index uses estimates. Seed the actual
            // row height before seeking, so even a deep offset stays in its row.
            self.transcript.measure(bookmark.row, bookmark.height);
            self.transcript.offset = self.transcript.prefix(pos) + bookmark.inside;
            if let Some((id, heights)) = bookmark.markdown {
                self.markdown_heights.insert(id, heights);
            }
            self.auto_scroll = false;
        } else if self.auto_scroll {
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
    fn jump_to_prompt(&mut self, entry: &str) -> Task<Message> {
        let Some(index) = self.current().and_then(|s| s.entries.index_of(entry)) else {
            return Task::none();
        };
        let Some(pos) = self.transcript.position(index) else {
            return Task::none();
        };
        // Near the tail the prompt may not reach the top of the pane. Mount
        // the rows for the reachable viewport: if already at the bottom, iced
        // won't report any scroll change to correct an overshooting row window.
        let max_scroll = (self.transcript.total() + 2. * CHAT_VERTICAL_PADDING
            - self.transcript.viewport)
            .max(0.);
        let scroll_y = (self.transcript.prefix(pos) + CHAT_VERTICAL_PADDING).min(max_scroll);
        if let Some(find) = &mut self.find {
            find.reveal = None;
        }
        self.hold_prompt_at_bottom = true;
        self.auto_scroll = false;
        self.transcript.offset = (scroll_y - CHAT_VERTICAL_PADDING).max(0.);
        self.prepare_visible();
        widget::operation::scroll_to("chat", scrollable::AbsoluteOffset { x: 0., y: scroll_y })
    }
    fn measure_row(&mut self, index: usize, height: f32, inner: Option<f32>) -> Task<Message> {
        let spacing = self
            .current()
            .map_or(8., |s| ActivityRow::from_id(index).spacing(&s.entries));
        let visible = self.transcript.visible();
        // Decide which layer owns the anchor using the SAME pre-measurement
        // row bounds, including when a resizing row crosses the viewport edge.
        let Some(pos) = self.transcript.position(index) else {
            return Task::none();
        };
        let height = height + spacing;
        let size_changed = (height - self.transcript.height(pos)).abs() >= 0.5;
        let inside = if (self.transcript.prefix(pos)..self.transcript.prefix(pos + 1))
            .contains(&self.transcript.offset)
        {
            inner.unwrap_or(0.)
        } else {
            0.
        };
        let corrected = self.transcript.measure(index, height);
        if self.auto_scroll && size_changed {
            self.transcript.end();
            self.prepare_visible();
            return bottom();
        }
        if !self.auto_scroll && (corrected || inside != 0.) {
            self.transcript.offset = (self.transcript.offset + inside).max(0.);
            self.prepare_visible();
            return widget::operation::scroll_to(
                "chat",
                scrollable::AbsoluteOffset {
                    x: 0.,
                    y: self.transcript.offset + CHAT_VERTICAL_PADDING,
                },
            );
        }
        if inner.is_some() || self.transcript.visible() != visible {
            self.prepare_visible();
        }
        Task::none()
    }
    fn prepare_visible(&mut self) {
        let mut visible_media = self.attachments.clone();
        let mut image_urls = HashSet::new();
        if let Some(session) = &self.current_session {
            let visible: Vec<_> = self
                .chat_range()
                .filter_map(
                    |pos| match ActivityRow::from_id(self.transcript.order[pos]) {
                        ActivityRow::Entry(i) => Some((pos, i)),
                        _ => None,
                    },
                )
                .collect();
            let ids: HashSet<_> = visible
                .iter()
                .map(|&(_, i)| session.entries[i].id.as_str())
                .collect();
            for (id, markdown) in self.markdown.extract_if(|id, _| !ids.contains(id.as_str())) {
                // Keep only numerical measurements in this session, not the
                // parsed items, text, images or widget trees.
                if let Some(window) = markdown.window
                    && !window.known.is_empty()
                {
                    self.markdown_heights.insert(id, window.known);
                }
            }
            self.outputs
                .retain(|id, _| ids.contains(id.as_str()) && self.expanded.contains(id));
            self.historical_runs
                .retain(|id, _| ids.contains(id.as_str()));
            for &(position, index) in &visible {
                let entry = &session.entries[index];
                if !self.historical_read_pending
                    && (matches!(entry.kind, EntryKind::Notice)
                        || (self.expanded.contains(&entry.id)
                            && matches!(entry.kind, EntryKind::Tool { complete: true, .. })))
                    && let Some(key) = transcript_run_key(entry)
                    && let Some(h) = &self.harness
                    && !self.historical_runs.contains_key(&entry.id)
                    && h.resident_subagent(&session.id, key).is_none()
                {
                    // One lookup at a time: scrolling cannot queue obsolete reads ahead of saves.
                    self.historical_read_pending = true;
                    let parent = session.id.clone();
                    let entry = session.entries.shared(index);
                    let h = h.clone();
                    self.tasks.push(self.io.run(move || {
                        let result = h
                            .subagent_run(
                                &parent,
                                transcript_run_key(&entry).expect("assignment row"),
                            )
                            .map_err(|e| format!("{e:#}"));
                        Message::HistoricalRun(parent, entry.id.clone(), result)
                    }));
                }
                if let EntryKind::User { images, files, .. } = &entry.kind {
                    visible_media.extend(images.iter().cloned());
                    visible_media.extend(files.iter().filter_map(|file| file.image.clone()));
                }
                // User text is literal, including newlines and Markdown syntax.
                if matches!(entry.kind, EntryKind::Assistant | EntryKind::Reasoning) {
                    let markdown = self.markdown.entry(entry.id.clone()).or_insert_with(|| {
                        let content = markdown::Content::parse(&entry.text);
                        let mut markdown = Markdown {
                            eager_heights: Default::default(),
                            images: HashSet::new(),
                            window: None,
                            source: session.entries.shared(index),
                            content,
                        };
                        markdown.sync_window(self.catalog.settings.font_size, 0);
                        if let Some(window) = &mut markdown.window {
                            window.restore(
                                self.markdown_heights.remove(&entry.id).unwrap_or_default(),
                                self.transcript.offset - self.transcript.prefix(position),
                                self.transcript.viewport,
                            );
                        }
                        markdown.refresh_images();
                        markdown
                    });
                    // A value-equal final reconciliation is invisible to the
                    // history diff, but geometry messages carry the Arc identity.
                    if !std::ptr::eq(entry, markdown.source.as_ref()) {
                        markdown.source = session.entries.shared(index);
                    }
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
                    &mut self.recent_previews,
                    self.current_session
                        .as_ref()
                        .map(|s| (s.id.as_str(), s.project.as_path())),
                    image_urls,
                )
                .map(Message::MarkdownImage),
        );
        self.tasks.push(
            self.image_view
                .prepare(&mut self.recent_previews, &visible_media)
                .map(Message::Image),
        );
    }
    fn edit_session_settings(&mut self, edit: impl FnOnce(&mut Session) + Send + 'static) {
        if self.io_busy || self.running() || self.active_questionnaire.is_some() {
            return;
        }
        if let Some(draft) = &mut self.draft_session {
            edit(draft);
        } else if let (Some(h), Some(id)) = (&self.harness, &self.selected) {
            let h = h.clone();
            let id = id.clone();
            self.io_busy = true;
            self.tasks.push(self.io.run(move || {
                Message::SessionSettingsEdited(h.edit(&id, edit).map_err(|e| format!("{e:#}")))
            }));
        }
    }
    fn save_catalog(&mut self) {
        self.catalog.selected = self.selected.clone();
        if let Some(h) = &self.harness {
            let store = h.store.clone();
            let catalog = self.catalog.clone();
            self.tasks.push(self.io.run_catalog(move |writer| {
                Message::CatalogSaved(writer.save_state(&store, catalog).map_err(|e| {
                    format!(
                        "Could not save settings: {e:#}. Fix storage and retry before quitting."
                    )
                }))
            }));
        }
    }
    fn reset_settings_editor(&mut self, settings: Settings) {
        self.context_text = settings.context_limit.to_string();
        self.threshold_text = settings.compact_percent.to_string();
        self.settings = settings;
    }
    fn take_draft(&mut self) -> Option<(String, UserMessage)> {
        if !self.draft_dirty {
            return None;
        }
        let id = self.selected.clone()?;
        self.draft_dirty = false;
        Some((id, self.composer_message()))
    }
    fn save_draft(&mut self) {
        if self.io_busy {
            return;
        }
        if self.new_draft_dirty
            && let Some(h) = &self.harness
        {
            let h = h.clone();
            let draft = self.new_draft.clone();
            self.new_draft_dirty = false;
            self.tasks.push(self.io.run(move || Message::NewDraftSaved(
                h.store.save_new_draft(&draft).map_err(|e| format!("Could not save new-session draft: {e:#}. Keep Orbit open and retry after fixing storage."))
            )));
        }
        if let Some((id, draft)) = self.take_draft()
            && let Some(h) = &self.harness
        {
            let h = h.clone();
            self.tasks.push(self.io.run(move || {
                let result = h.edit(&id, |s| {
                    s.draft = draft.text;
                    s.draft_images = draft.images;
                }).map_err(|e| {
                    format!("Could not save draft: {e:#}. Keep Orbit open and retry after fixing storage.")
                });
                Message::DraftSaved(id, result)
            }));
        }
    }
    fn restore_draft(&mut self) {
        let workspace = self
            .current()
            .filter(|s| s.child.is_none())
            .map(|s| (s.id.clone(), s.project.clone()));
        let cache = if let Some(dir) = &self.smoke_dir {
            dir.join("file-cache")
        } else {
            orbit_core::file_index::cache_dir()
        };
        if let Err(error) = self.file_completion.workspace(workspace, cache) {
            self.error = Some(format!("Could not start file search: {error:#}"));
        }
        let editor = text_editor::Content::with_text(if self.draft_session.is_some() {
            &self.new_draft.message
        } else {
            self.current_session.as_ref().map_or("", |s| &s.draft)
        });
        let owner = self.draft_key();
        self.histories.switch(&self.editor, owner, &editor);
        self.editor = editor;
        self.attachments = if self.draft_session.is_some() {
            self.new_draft.images.clone()
        } else {
            self.current_session
                .as_ref()
                .map_or_else(Vec::new, |s| s.draft_images.clone())
        };
        self.draft_dirty = false;
        self.spellcheck.clear();
        self.check_draft(false);
        self.prepare_visible();
    }
    fn queue_spellcheck(&mut self, debounce: bool) {
        self.tasks
            .push(self.spellcheck.schedule(debounce).map(Message::Spellcheck));
    }
    fn check_draft(&mut self, debounce: bool) {
        if self.catalog.settings.spellcheck.enabled && !self.editor.is_empty() {
            self.queue_spellcheck(debounce);
        } else {
            self.spellcheck.clear();
        }
    }
    fn discover_dictionaries(&mut self) {
        if self.spellcheck.languages.is_none() {
            self.queue_spellcheck(false);
        }
    }
    fn composer_edited(&mut self, before: spellcheck::BeforeEdit, typing: bool) {
        before.retain_untouched(&mut self.spellcheck.marks, &self.editor);
        self.spellcheck.typing_at = typing.then(|| self.editor.cursor().position);
        self.check_draft(true);
        if self.draft_session.is_some() {
            self.new_draft.message = self.editor.text();
            self.new_draft_dirty = true;
        } else {
            self.draft_dirty = true;
        }
    }
    fn composer_message(&self) -> UserMessage {
        UserMessage {
            skills: vec![],
            files: vec![],
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
            && !self.io_busy
            && self.modal.is_none()
            && self.active_questionnaire.is_none()
            && self.context_menu.is_none()
            && !self.footer_menu_open
            && self.worktrees.popup.is_none()
    }
    fn remember_chat_position(&mut self) {
        if self.auto_scroll {
            return;
        }
        let pos = self.transcript.at(self.transcript.offset);
        if let Some(&row) = self.transcript.order.get(pos) {
            let session = self.current_session.as_ref().expect("mounted transcript");
            let markdown = match ActivityRow::from_id(row) {
                ActivityRow::Entry(i) => {
                    let id = &session.entries[i].id;
                    self.markdown
                        .get_mut(id)
                        .and_then(|md| md.window.as_mut())
                        .map(|window| (id.clone(), std::mem::take(&mut window.known)))
                }
                _ => None,
            };
            self.chat_positions.insert(
                session.id.clone(),
                ChatPosition {
                    row,
                    inside: self.transcript.offset - self.transcript.prefix(pos),
                    height: self.transcript.height(pos),
                    markdown,
                },
            );
        }
    }
    fn select(&mut self, selection: Selection) {
        self.select_with_fork(selection, None);
    }
    fn select_with_fork(&mut self, selection: Selection, fork_entry: Option<String>) {
        self.chat_selection.borrow_mut().clear();
        self.footer_menu_open = false;
        self.worktrees.popup = None;
        if self.io_busy {
            return;
        }
        self.find = None;
        self.file_completion.dismiss();
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
            self.switching_session = true;
            self.tasks.push(self.io.run(move || {
                Message::Selected(
                    (|| {
                        if let Some((old, draft)) = draft {
                            h.edit(&old, |s| { s.draft = draft.text; s.draft_images = draft.images; })?;
                        }
                        if let (Selection::Session(source), Some(entry)) = (&selection, fork_entry) {
                            h.store.save_new_draft(&new_draft)?;
                            h.evict_inactive(Some(source))?;
                            // Create last: a failed switch must never leave a hidden duplicate fork.
                            let fork = h.fork(source, &entry, &settings)?;
                            return Ok((Selection::Session(fork), None));
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
                        // Reconcile last so a later selection error cannot suppress the notice after recovery.
                        let replaced = match id {
                            Some(id) => h.reconcile_model(id, &settings)?,
                            None => false,
                        };
                        let notice = replaced.then(|| format!("The previous model is no longer available. This session now uses {} with its default effort and Fast setting.", model(&settings.model).expect("catalog model")));
                        Ok::<_, orbit_core::Error>((selection, notice))
                    })()
                    .map_err(|e| {
                        format!("Could not switch sessions: {e:#}. Fix storage and retry.")
                    }),
                )
            }));
        }
    }
    fn acknowledge_focused_completion(&mut self) {
        if self.window_focused
            && !self.closing
            && let (Some(h), Some(session)) = (&self.harness, &self.current_session)
            && session.unchecked_completion
        {
            let h = h.clone();
            let id = session.id.clone();
            self.tasks.push(self.io.run(move || {
                Message::Edited(h.edit(&id, |s| s.unchecked_completion = false).map_err(|e| {
                    format!("Could not save completion acknowledgement: {e:#}. Keep Orbit open, fix storage, then retry saving or revisit the session.")
                }))
            }));
        }
    }
    fn stop_agent(&mut self, id: String) {
        if let Some(h) = self.harness.clone() {
            // Queue with Send and File: waiting for the lifecycle lock must not stall input.
            self.tasks.push(self.io.run(move || {
                h.stop(&id);
                Message::Changed
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
                    format!("Could not save attention acknowledgement: {e:#}. Keep Orbit open, fix storage, then retry saving or revisit the session.")
                }))
            }));
        }
    }
    fn fail(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.error = Some(error);
        }
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        let geometry = (
            self.view_epoch,
            self.transcript.offset,
            self.transcript.viewport,
            self.transcript.total(),
        );
        let task = self.update_inner(message);
        self.advance_imports();
        if self
            .fork_panel
            .as_ref()
            .is_some_and(|p| self.fork_family() != Some(p.family.as_str()))
        {
            self.fork_panel = None;
            self.view_epoch = self.view_epoch.wrapping_add(1);
        }
        if geometry
            != (
                self.view_epoch,
                self.transcript.offset,
                self.transcript.viewport,
                self.transcript.total(),
            )
        {
            self.image_view.invalidate_return();
        }
        if self.smoke_dir.is_none()
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
        if self.modal.is_some()
            || self.context_menu.is_some()
            || self.active_questionnaire.is_some()
        {
            self.histories.active.separate();
            self.footer_menu_open = false;
            self.worktrees.popup = None;
        }
        let close = if self.close_requested
            && self.imports.is_empty()
            && !self.io_busy
            && self.pending_settings.is_none()
            && !self.closing
        {
            self.close_requested = false;
            self.quit()
        } else {
            Task::none()
        };
        Task::batch([task, close].into_iter().chain(self.tasks.drain(..)))
    }
    fn update_inner(&mut self, message: Message) -> Task<Message> {
        if self.io_busy && matches!(message, Message::Favorite(_) | Message::StashProject(_)) {
            return Task::none();
        }
        if (self.closing || self.close_requested)
            && matches!(
                message,
                Message::Editor(_)
                    | Message::EditorScroll(_)
                    | Message::Questionnaire(_, _)
                    | Message::ComposerHistory(_)
                    | Message::DeletePreviousWord
                    | Message::Send
                    | Message::Select(_)
                    | Message::New
                    | Message::Create(_)
                    | Message::Confirm
                    | Message::SaveDraft
                    | Message::SessionModel(_)
                    | Message::SessionEffort(_)
                    | Message::SessionFast(_)
                    | Message::Favorite(_)
                    | Message::StashProject(_)
                    | Message::ApplySettings
                    | Message::Theme(_)
                    | Message::Font(_)
                    | Message::ChatWidth(_)
                    | Message::Resume(_)
            )
        {
            return Task::none();
        }
        if let Some(pending) = &mut self.pending_settings
            && matches!(
                message,
                Message::Model(_)
                    | Message::ModelEffort(_, _)
                    | Message::ModelFast(_, _)
                    | Message::SubagentModel(_, _)
                    | Message::SubagentEffort(_, _)
                    | Message::SubagentFast(_, _)
                    | Message::Context(_)
                    | Message::Threshold(_)
                    | Message::SpellcheckEnabled(_)
                    | Message::SpellcheckLanguage(_)
                    | Message::ImageBackend(_)
                    | Message::ImageModel(_)
                    | Message::ImageQuality(_)
                    | Message::ResetImageModel
                    | Message::SettingsTab(_)
                    | Message::ApiKey(_)
                    | Message::Theme(_)
                    | Message::Font(_)
                    | Message::ChatWidth(_)
                    | Message::Resume(_)
            )
        {
            pending.close_on_success = false;
        }
        if self.current().is_some_and(|s| s.child.is_some())
            && matches!(
                message,
                Message::Editor(_)
                    | Message::EditorScroll(_)
                    | Message::Questionnaire(_, _)
                    | Message::ComposerHistory(_)
                    | Message::DeletePreviousWord
                    | Message::Send
                    | Message::SessionModel(_)
                    | Message::SessionEffort(_)
                    | Message::SessionFast(_)
                    | Message::ToggleFooterMenu
            )
        {
            return Task::none();
        }
        match message {
            Message::Notification(crate::notifications::Message::Open(open)) => {
                let crate::notifications::Open { event, token } = open;
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
                        if let Some(token) = token {
                            return window::activate_with_token(id, token);
                        }
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
            Message::Paste => self.import(None),
            Message::ClipboardImageOffered(paste) => self.imports.image_offered(paste),
            Message::AttachmentLoaded(result) => self.import_loaded(result),
            Message::AttachmentPublished(released) => self.import_published(released),
            Message::AttachmentSaveFailed(error, pending) => {
                self.import_save_failed(error, pending)
            }
            Message::RemoveAttachment(index) => {
                if !self.can_attach() {
                    return Task::none();
                }
                self.attachments.remove(index);
                self.attachment_changed();
            }
            Message::Image(message) => {
                let skipped = matches!(message, crate::image_view::Message::Thumbnail(_, None));
                self.image_view.update(message);
                if self.image_view.finished_closing() {
                    self.modal = None;
                }
                if skipped {
                    self.prepare_visible();
                }
            }
            Message::RequestEquation(source) => {
                return self
                    .equations
                    .request(source)
                    .map(Message::EquationRendered);
            }
            Message::EquationRendered(result) => {
                return self.equations.update(result).map(Message::EquationRendered);
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
            Message::OpenImageFrom(message, source) => {
                let task = self.update_inner(*message);
                let (natural, gif) = match &self.modal {
                    Some(Modal::Image(media)) => (
                        Size::new(media.width as f32, media.height as f32),
                        media.mime == "image/gif",
                    ),
                    Some(Modal::MarkdownImage(image)) => (
                        Size::new(image.width as f32, image.height as f32),
                        image.bytes.starts_with(b"GIF8"),
                    ),
                    _ => return task,
                };
                self.image_view.animate_from(source, natural, gif);
                return task;
            }
            Message::OpenImage(media) => {
                let task = self.image_view.open(&media).map(Message::Image);
                self.modal = Some(Modal::Image(media));
                return task;
            }
            Message::SpellcheckEnabled(enabled) => self.settings.spellcheck.enabled = enabled,
            Message::SpellcheckLanguage(language) => self.settings.spellcheck.language = language,
            Message::Spellcheck(spellcheck::Message::Due(revision)) => {
                let config = &self.catalog.settings.spellcheck;
                return self
                    .spellcheck
                    .check(
                        revision,
                        &self.editor,
                        config.enabled.then(|| config.language.clone()),
                    )
                    .map(Message::Spellcheck);
            }
            Message::Spellcheck(spellcheck::Message::Checked(revision, report)) => {
                self.spellcheck.complete(revision, report);
            }
            Message::ImageBackend(backend) => self.settings.images.backend = backend,
            Message::ImageModel(model) => self.settings.images.model = model,
            Message::ImageQuality(quality) => self.settings.images.quality = quality,
            Message::ResetImageModel => {
                self.settings.images.model = orbit_core::image_tools::DEFAULT_API_MODEL.into()
            }
            Message::CancelSettings => {
                let draft = if let Some(pending) = &mut self.pending_settings {
                    pending.close_on_success = false;
                    crate::io_worker::with_live_controls(&pending.submitted, &self.catalog.settings)
                } else {
                    self.catalog.settings.clone()
                };
                self.reset_settings_editor(draft);
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
                self.switching_session = false;
                self.io_busy = false;
                match result {
                    Ok((selection, notice)) => {
                        if self.worktrees.retry.as_ref().is_some_and(|(_, target)| matches!(&selection, Selection::Draft(path) if path == target)) {
                            self.worktrees.retry = None;
                            self.worktrees.panel = false;
                        }
                        self.worktrees.popup = None;
                        self.worktrees.bases = false;
                        if let Some(notice) = notice {
                            self.notice = Some((notice, std::time::Instant::now()));
                        }
                        self.imports.left_composer();
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
                        // Selection hands the view back to the harness, even when no
                        // catalog revision changed while a fixture owned the sidebar.
                        if self.smoke_fixtures.sidebar {
                            self.catalog_revision = u64::MAX;
                        }
                        self.smoke_fixtures = SmokeFixtures::default();
                        self.view_epoch = self.view_epoch.wrapping_add(1);
                        self.context_menu = None;
                        if self.current_session.as_ref().map(|s| &s.id) != self.selected.as_ref() {
                            self.remember_chat_position();
                            self.auto_scroll = true;
                        }
                        self.refresh();
                        self.acknowledge_selected_attention();
                        self.restore_draft();
                        if self
                            .current()
                            .is_some_and(|session| session.child.is_none())
                            && self.active_questionnaire.is_none()
                            && self.modal.is_none()
                        {
                            self.tasks.push(widget::operation::focus("composer"));
                        }
                        if self.changes.project.as_deref()
                            != self.current().map(|s| s.project.as_path())
                        {
                            self.changes.clear();
                        }
                        let git = self.refresh_git();
                        self.tasks.push(git);
                        if self.worktrees.panel {
                            let task = self.load_worktrees();
                            self.tasks.push(task);
                        }
                        self.save_catalog();
                        if let Some((source, entry)) = self.jump_after_select.take()
                            && Some(&source) == self.selected.as_ref()
                        {
                            return self.jump_to_prompt(&entry);
                        }
                        if !self.auto_scroll {
                            return widget::operation::scroll_to(
                                "chat",
                                scrollable::AbsoluteOffset {
                                    x: 0.,
                                    y: if self.transcript.offset == 0. {
                                        0.
                                    } else {
                                        self.transcript.offset + CHAT_VERTICAL_PADDING
                                    },
                                },
                            );
                        }
                        return bottom();
                    }
                    Err(error) => {
                        self.jump_after_select = None;
                        if self.worktrees.retry.as_ref().is_some_and(|(origin, _)| {
                            self.current().is_some_and(|s| &s.project == origin)
                        }) {
                            if !self.worktrees.panel {
                                let task = self.worktree_action(worktrees::Action::Panel);
                                self.tasks.push(task);
                            }
                            self.worktrees.bases = false;
                        }
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
            Message::DraftSaved(id, result) => {
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
                        self.error = None;
                        if self.draft_session.as_ref().is_some_and(|s| s.id == id) {
                            self.draft_session = None;
                            self.new_draft = NewSessionDraft::default();
                            self.new_draft_dirty = false;
                            self.selected = Some(id.clone());
                            self.save_catalog();
                        }
                        self.submissions.remove(&id);
                        if self.selected.as_ref() == Some(&id) && self.composer_message() == text {
                            self.histories.reset(Some(DraftKey::Session(id.clone())));
                            self.editor = text_editor::Content::new();
                            self.spellcheck.clear();
                            self.file_completion.edited(&self.editor);
                            self.attachments.clear();
                            // A local action did not pass through submission acceptance,
                            // which normally clears the persisted draft.
                            self.draft_dirty = text.text == crate::file_completion::COMPACT;
                            if self.draft_dirty {
                                self.save_draft();
                            }
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
                    self.histories
                        .retain(|key| !matches!(key, DraftKey::Session(s) if s == &id));
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
            Message::UsageSummary(result) => self.usage_summary = Some(result),
            Message::AgeTick => {
                // While the picker is mounted its periodic summary read also prunes.
                if self.current().is_some()
                    && let Some(h) = &self.harness
                {
                    let store = h.store.clone();
                    return self.io.run(move || {
                        Message::External(
                            store.prune_activity().map_err(|e| {
                                format!("Could not expire activity measurements: {e:#}")
                            }),
                        )
                    });
                }
            }
            Message::SaveDraft => {
                self.imports.retry_on_save = false;
                self.save_draft();
            }
            Message::Changed => {
                let before = self.catalog_revision;
                let changed = self.refresh();
                if changed {
                    self.image_view.invalidate_return();
                }
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
            Message::Fork(s, e) => {
                if self.selected.as_ref() == Some(&s) && !self.closing && !self.close_requested {
                    self.select_with_fork(Selection::Session(s), Some(e));
                }
            }
            Message::ShowForks(entry) => return self.show_forks(entry),
            Message::ViewOriginal(s, e) => {
                if !self.io_busy {
                    self.jump_after_select = Some((s.clone(), e));
                    self.select(Selection::Session(s));
                }
            }
            Message::SessionMenu(id, position) => {
                self.context_menu = Some(ContextMenu(MenuTarget::Session(id), position));
            }
            Message::ProjectMenu(path, position) => {
                self.context_menu = Some(ContextMenu(MenuTarget::Project(path), position));
            }
            Message::ToggleStashed => self.stashed_open = !self.stashed_open,
            Message::RenameProject(path) => {
                self.context_menu = None;
                if let Some(project) = self.catalog.projects.iter().find(|p| p.path == path) {
                    self.modal_input = project.name();
                    self.modal = Some(Modal::RenameProject(path));
                    return widget::operation::focus("modal-input");
                }
            }
            Message::StashProject(path) => {
                self.context_menu = None;
                if let Some(project) = self.catalog.projects.iter_mut().find(|p| p.path == path) {
                    project.stashed = !project.stashed;
                    self.save_catalog();
                }
            }
            Message::DeleteProject(path) => {
                self.context_menu = None;
                self.modal = Some(Modal::DeleteProject(path));
            }
            Message::ProjectDeleted(path, result) => {
                self.io_busy = false;
                let selected_deleted = self
                    .selected
                    .as_ref()
                    .is_some_and(|id| self.harness.as_ref().is_some_and(|h| h.get(id).is_none()));
                if selected_deleted {
                    self.draft_dirty = false;
                    self.selected = None;
                }
                if result.is_ok() {
                    self.catalog.projects.retain(|p| p.path != path);
                    if self.new_draft.project.as_ref() == Some(&path) {
                        self.new_draft.project = None;
                        self.draft_session = None;
                    }
                    self.modal = None;
                }
                self.refresh();
                if result.is_ok() {
                    self.histories.retain(|key| match key {
                        DraftKey::New => true,
                        DraftKey::Session(id) => self.sessions.iter().any(|s| &s.id == id),
                    });
                }
                if selected_deleted {
                    self.restore_draft();
                }
                self.fail(result);
            }
            Message::CopySessionId(id) => {
                self.context_menu = None;
                return iced::clipboard::write(id);
            }
            Message::CloseMenu => self.context_menu = None,
            Message::CloseFooterMenu => self.footer_menu_open = false,
            Message::ToggleFooterMenu => {
                self.worktrees.popup = None;
                if self.footer_menu_open {
                    self.footer_menu_open = false;
                } else if !self.running()
                    && self.active_questionnaire.is_none()
                    && self.current().is_some()
                {
                    self.footer_menu_open = true;
                }
            }
            Message::ToolOutput(id, event) => {
                if let Some(view) = self.outputs.get_mut(&id) {
                    view.update(event);
                }
            }
            Message::New => self.select(Selection::Picker),
            Message::Select(id) => self.select(Selection::Session(id)),
            Message::FileSession(id, action) => self.change_filing(id, action),
            Message::RunningCommandsAtFile(id, kind) => {
                if self.modal.is_none() && !self.closing && !self.close_requested {
                    self.modal = Some(Modal::StopCommandsAndFile(id, kind));
                }
            }
            Message::ToggleFiled(kind) => {
                let section = &mut self.filed[kind as usize];
                if self.search.is_empty() {
                    section.open = !section.open;
                } else {
                    section.search_open = !section.search_open;
                }
                section.list.offset = 0.;
            }
            Message::FiledScroll(kind, epoch, offset, height) => {
                if epoch == self.sidebar_epoch {
                    let list = &mut self.filed[kind as usize].list;
                    list.offset = offset;
                    list.viewport = height;
                }
            }
            Message::Create(path) => self.select(Selection::Draft(path)),
            Message::Search(search) => {
                self.search = search;
                for section in &mut self.filed {
                    section.search_open = true;
                    section.list.offset = 0.;
                }
                self.sidebar_follow_top = true;
                self.sidebar_list.offset = 0.;
                self.prepare_sidebar();
                return widget::operation::snap_to("sessions", scrollable::RelativeOffset::START);
            }
            Message::Find(action) => return self.find_action(action),
            Message::ToggleSearch => {
                if self.find.take().is_some() {
                    self.prepare_visible();
                }
                self.histories.active.separate();
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
                self.save_catalog();
            }
            Message::Questionnaire(id, message) => {
                if self.io_busy
                    || self.modal.is_some()
                    || self.context_menu.is_some()
                    || self.footer_menu_open
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
            Message::FileCompletion(message) => {
                let result = matches!(
                    &message,
                    crate::file_completion::Message::Results(_)
                        | crate::file_completion::Message::Skills(..)
                );
                if !result && !self.file_completion_enabled() {
                    return Task::none();
                }
                let refocus = matches!(
                    &message,
                    crate::file_completion::Message::Accept { .. }
                        | crate::file_completion::Message::Refresh
                        | crate::file_completion::Message::Ignored
                );
                let before = spellcheck::BeforeEdit::capture(&self.editor);
                let edited = if matches!(message, crate::file_completion::Message::Accept { .. }) {
                    self.histories
                        .active
                        .transaction(&mut self.editor, |editor| {
                            self.file_completion.update(message, editor)
                        })
                } else {
                    self.file_completion.update(message, &mut self.editor)
                };
                if edited {
                    self.composer_edited(before, false);
                }
                if refocus {
                    return widget::operation::focus("composer");
                }
            }
            Message::FocusComposer => return widget::operation::focus("composer"),
            Message::Editor(_)
            | Message::EditorScroll(_)
            | Message::DeletePreviousWord
            | Message::ComposerHistory(_) => {
                if self.active_questionnaire.is_some()
                    || self.imports.holds_paste_cursor()
                    || self.io_busy
                    || self.modal.is_some()
                    || self.context_menu.is_some()
                {
                    return Task::none();
                }
                let (before, typing) = match message {
                    Message::Editor(action) => {
                        let before = match &action {
                            text_editor::Action::Edit(edit) => {
                                Some(spellcheck::BeforeEdit::for_edit(&self.editor, edit))
                            }
                            _ => None,
                        };
                        let typing = matches!(&action,
                            text_editor::Action::Edit(text_editor::Edit::Insert(c))
                            if matches!(c, '\'' | '’') || (!c.is_whitespace() && !c.is_ascii_punctuation())
                        );
                        self.histories.active.perform(&mut self.editor, action);
                        (before, typing)
                    }
                    Message::EditorScroll(scroll) => {
                        scroll.apply(&mut self.editor);
                        (None, false)
                    }
                    Message::DeletePreviousWord => {
                        let before = spellcheck::BeforeEdit::replacement(&self.editor, 0);
                        self.histories
                            .active
                            .transaction(&mut self.editor, questionnaire::delete_previous_word);
                        (Some(before), false)
                    }
                    Message::ComposerHistory(direction) => {
                        let before = spellcheck::BeforeEdit::capture(&self.editor);
                        if !self.histories.active.replay(&mut self.editor, direction) {
                            return Task::none();
                        }
                        // The edit may be far from the current caret, and may span
                        // multiple ranges (e.g. indent). Recheck the restored text.
                        self.spellcheck.marks.clear();
                        (Some(before), false)
                    }
                    _ => unreachable!("editor message"),
                };
                self.file_completion.edited(&self.editor);
                if let Some(before) = before {
                    self.composer_edited(before, typing);
                } else if self.spellcheck.typing_at.is_some_and(|at| {
                    let cursor = self.editor.cursor();
                    cursor.position != at || cursor.selection.is_some()
                }) {
                    self.spellcheck.typing_at = None;
                }
            }
            Message::Send => {
                if self.worktrees.popup.is_some() {
                    return Task::none();
                }
                self.file_completion.dismiss();
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
                    if self.io_busy || self.composer_importing() {
                        return Task::none();
                    }
                    let h = h.clone();
                    let id = session.id.clone();
                    let draft = self.draft_session.clone();
                    let workspace = session.project.clone();
                    let settings = self.catalog.settings.clone();
                    self.io_busy = true;
                    if message.text == crate::file_completion::COMPACT {
                        if draft.is_some() || !message.images.is_empty() {
                            self.io_busy = false;
                            self.error = Some(
                                if !message.images.is_empty() {
                                    "Remove the attachments before running /compact."
                                } else {
                                    "There is no conversation context to compact yet."
                                }
                                .into(),
                            );
                            return Task::none();
                        }
                        self.tasks.push(self.io.run(move || {
                            let result = h.compact(&id, settings).map_err(|e| format!("{e:#}"));
                            Message::Sent(id, message, result)
                        }));
                        return Task::none();
                    }
                    let submission = self
                        .submissions
                        .entry(id.clone())
                        .or_insert_with(|| (message.clone(), orbit_core::store::id()));
                    if submission.0 != message {
                        *submission = (message.clone(), orbit_core::store::id());
                    }
                    let submission = submission.1.clone();
                    self.tasks.push(self.io.run(move || {
                        let result = (|| {
                            if !workspace.is_dir() {
                                anyhow::ensure!(!workspace.starts_with(h.store.root().canonicalize()?.join("worktrees")),
                                    "The worktree at {} is missing. Restore the checkout to continue this chat or start a new session in another worktree.", workspace.display());
                            }
                            if let Some(draft) = draft {
                                h.send_new(draft, &submission, message.clone(), settings)
                            } else {
                                h.send(&id, &submission, message.clone(), settings)
                            }
                        })().map_err(|e| format!("Could not send message: {e:#}. Your message is preserved; fix the problem and retry."));
                        Message::Sent(id, message, result)
                    }));
                }
            }
            Message::StopAgent(id) => self.stop_agent(id),
            Message::Worktree(action) => return self.worktree_action(action),
            Message::Changes(action) => return self.changes_action(action),
            Message::ToggleAgents => {
                self.changes.close();
                self.worktrees.panel = false;
                self.worktrees.popup = None;
                self.fork_panel = None;
                self.agents_open = !self.agents_open;
                if self.settings_open {
                    return self.update_inner(Message::CancelSettings);
                }
                self.view_epoch = self.view_epoch.wrapping_add(1);
            }
            Message::ToggleSettings => {
                self.changes.close();
                self.worktrees.panel = false;
                self.worktrees.popup = None;
                if self.settings_open {
                    return self.update_inner(Message::CancelSettings);
                }
                self.view_epoch = self.view_epoch.wrapping_add(1);
                self.settings_open = true;
                self.agents_open = false;
                self.fork_panel = None;
                if self.settings_tab == SettingsTab::General {
                    self.discover_dictionaries();
                }
            }
            Message::ApplySettings => {
                if self.pending_settings.is_some() {
                    return Task::none();
                }
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
                } else if let Some(h) = &self.harness {
                    let store = h.store.clone();
                    let submitted = self.settings.clone();
                    self.pending_settings = Some(PendingSettings {
                        submitted: self.settings.clone(),
                        close_on_success: self.settings_open,
                    });
                    self.tasks.push(self.io.run_catalog(move |writer| {
                        Message::SettingsSaved(match writer.apply(&store, submitted) {
                            Ok(()) => SettingsSave::Saved,
                            Err(e) if e.is::<orbit_core::store::PublishedButNotSynced>() =>
                                SettingsSave::PublishedButNotSynced(format!("Settings are active, but crash durability is unconfirmed: {e:#}. Check storage and select Save to retry.")),
                            Err(e) => SettingsSave::NotSaved(format!("Could not save settings: {e:#}. Check disk space and permissions, then select Save to retry.")),
                        })
                    }));
                }
            }
            Message::CatalogSaved(result) => self.fail(result),
            Message::SettingsSaved(result) => {
                let pending = self
                    .pending_settings
                    .take()
                    .expect("one Settings save in flight");
                if !matches!(result, SettingsSave::NotSaved(_)) {
                    let spellcheck_changed =
                        pending.submitted.spellcheck != self.catalog.settings.spellcheck;
                    self.catalog.settings = crate::io_worker::with_live_controls(
                        &pending.submitted,
                        &self.catalog.settings,
                    );
                    if spellcheck_changed {
                        self.spellcheck.clear();
                        self.spellcheck.unavailable = None;
                        if self.catalog.settings.spellcheck.enabled {
                            self.queue_spellcheck(false);
                        }
                    }
                }
                match result {
                    SettingsSave::Saved if pending.close_on_success && self.settings_open => {
                        self.settings_open = false;
                        self.view_epoch = self.view_epoch.wrapping_add(1);
                    }
                    SettingsSave::Saved => {}
                    SettingsSave::NotSaved(error) | SettingsSave::PublishedButNotSynced(error) => {
                        if self.close_requested && matches!(self.modal, Some(Modal::Quit)) {
                            self.modal = None;
                        }
                        self.close_requested = false;
                        self.error = Some(error);
                    }
                }
            }
            Message::Model(value) => self.settings.model = value,
            Message::SettingsTab(tab) => {
                if tab == SettingsTab::General && self.settings_tab != tab {
                    self.discover_dictionaries();
                }
                self.settings_tab = tab;
            }
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
            Message::SubagentModel(profile, model) => {
                let config = self
                    .settings
                    .subagents
                    .get_mut(&profile)
                    .expect("built-in profile");
                config.model = model.id.into();
                if !model.efforts.contains(&config.effort.as_str()) {
                    config.effort = profile.defaults().effort;
                }
            }
            Message::SubagentEffort(profile, effort) => {
                self.settings
                    .subagents
                    .get_mut(&profile)
                    .expect("built-in profile")
                    .effort = effort;
            }
            Message::SubagentFast(profile, fast) => {
                self.settings
                    .subagents
                    .get_mut(&profile)
                    .expect("built-in profile")
                    .fast = fast;
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
            Message::Theme(name) => {
                self.settings.theme = name.to_owned();
                self.catalog.settings.theme = name.to_owned();
                self.save_catalog();
            }
            Message::Font(value) => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                self.settings.font_size = value;
                self.catalog.settings.font_size = value;
                self.save_catalog();
            }
            Message::ChatWidth(value) => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                self.settings.chat_width = value;
                self.catalog.settings.chat_width = value;
                self.save_catalog();
            }
            Message::Resume(value) => {
                self.settings.resume_last = value;
                self.catalog.settings.resume_last = value;
                self.save_catalog();
            }
            Message::SessionSettingsEdited(result) => {
                self.io_busy = false;
                self.fail(result);
                self.refresh();
            }
            Message::SessionModel(value) => {
                let settings = self.catalog.settings.clone();
                self.edit_session_settings(move |s| s.select_model(&value, &settings));
            }
            Message::SessionEffort(value) => self.edit_session_settings(move |s| s.effort = value),
            Message::SessionFast(value) => self.edit_session_settings(move |s| s.fast = value),
            Message::AddProject => {
                self.modal_input = std::env::var("HOME").unwrap_or_else(|_| "/".into());
                self.modal = Some(Modal::AddProject);
                return widget::operation::focus("modal-input");
            }
            Message::Browse => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Add project to Orbit")
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
            Message::Dismiss => {
                if matches!(self.modal, Some(Modal::Quit)) {
                    self.close_requested = false;
                }
                if !self.image_view.request_close(std::time::Instant::now()) {
                    self.modal = None;
                }
            }
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
            Message::HistoricalRun(parent, id, result) => {
                self.historical_read_pending = false;
                if self.current().is_some_and(|s| s.id == parent) {
                    let run = match result {
                        Ok(run) => run,
                        Err(error) => {
                            self.error = Some(error);
                            None
                        }
                    };
                    self.historical_runs.insert(id, run);
                    self.view_epoch = self.view_epoch.wrapping_add(1);
                }
                // Re-evaluate the current viewport, not the viewport that requested the read.
                self.prepare_visible();
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
            Message::JumpToPrompt(entry) => return self.jump_to_prompt(&entry),
            Message::JumpToLatest => {
                if let Some(find) = &mut self.find {
                    find.reveal = None;
                }
                self.auto_scroll = true;
                self.transcript.end();
                self.prepare_visible();
                return bottom();
            }
            Message::Scroll(epoch, offset, height, at_bottom) => {
                if epoch != self.view_epoch {
                    return Task::none();
                }
                // Composer growth can leave the old absolute offset above the
                // new bottom without the reader having scrolled away.
                let follow_resize = self.auto_scroll && height < self.transcript.viewport;
                // A jump to a recent prompt can be clamped to the bottom. That
                // geometry report isn't a request to follow a growing reply.
                self.hold_prompt_at_bottom &= at_bottom && !self.auto_scroll;
                self.auto_scroll = follow_resize || (at_bottom && !self.hold_prompt_at_bottom);
                self.transcript.offset = (offset - CHAT_VERTICAL_PADDING).max(0.);
                self.transcript.viewport = height;
                self.prepare_visible();
                if follow_resize {
                    return bottom();
                }
            }
            Message::MarkdownGeometry(epoch, index, source, height, geometry) => {
                if epoch != self.view_epoch
                    || !self
                        .current()
                        .and_then(|s| s.entries.by_id(&source.id))
                        .is_some_and(|e| std::ptr::eq(e, source.as_ref()))
                {
                    return Task::none();
                }
                // Scroll is delivered before the row's measurement. A jump past
                // the old estimated extent can already have evicted this body;
                // its actual outer height and sampled item heights still count.
                let inner = if let Some(markdown) = self.markdown.get_mut(&source.id)
                    && let Some(window) = &mut markdown.window
                {
                    let delta = geometry.map(|g| window.update(g));
                    if delta.is_some() {
                        markdown.refresh_images();
                    }
                    delta
                } else {
                    if let Some(g) = geometry {
                        g.save_heights(self.markdown_heights.entry(source.id.clone()).or_default());
                    }
                    None
                };
                return self.measure_row(index, height, inner);
            }
            Message::RowSize(epoch, index, height) => {
                if epoch == self.view_epoch {
                    return self.measure_row(index, height, None);
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
                        theme::markdown_style(&self.preset().theme),
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
            Message::CopyEquation {
                entry_id,
                block,
                source,
            } => {
                let until = std::time::Instant::now() + Duration::from_secs(2);
                self.equations.mark_copied(entry_id, block, until);
                return Task::batch([
                    iced::clipboard::write(source),
                    Task::perform(
                        async move { tokio::time::sleep_until(until.into()).await },
                        move |_| Message::ClearEquationCopy(until),
                    ),
                ]);
            }
            Message::ClearEquationCopy(until) => self.equations.clear_copied(until),
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
            Message::SidebarBranch(path, branch) => {
                if let Some(branch) = branch {
                    self.sidebar_branches.insert(path, branch);
                } else {
                    self.sidebar_branches.remove(&path);
                }
            }
            Message::Git(path, info) => {
                let needs_details =
                    self.changes.open && self.current().is_some_and(|s| s.project == path);
                self.store_git(path, info);
                if needs_details {
                    return self.refresh_git();
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
                iced::Event::Window(window::Event::Focused) => {
                    self.window_focused = true;
                    self.acknowledge_focused_completion();
                }
                iced::Event::Window(window::Event::Unfocused) => {
                    self.window_focused = false;
                    self.histories.active.separate();
                    if let Some(draft) = self
                        .active_questionnaire
                        .as_ref()
                        .and_then(|id| self.questionnaires.get_mut(id))
                    {
                        draft.separate_edits();
                    }
                }
                iced::Event::Window(window::Event::FileDropped(path)) => self.import(Some(path)),
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
                        self.save_catalog();
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
                    if self.worktrees.popup.is_some() {
                        return self.worktree_key(key);
                    }
                    if self.footer_menu_open {
                        if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                            self.footer_menu_open = false;
                        }
                        return Task::none();
                    }
                    if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                        self.histories.active.separate();
                        // One central Escape handler preserves modal/search shortcuts
                        // even when their native text inputs capture the key.
                        let close_search = if self.find.is_some() {
                            Some(Message::Find(find::Action::Close))
                        } else {
                            self.search_active.then_some(Message::ToggleSearch)
                        };
                        if self.file_completion_enabled() && self.file_completion.is_open() {
                            if let Some(close_search) = close_search {
                                // Both controls are visible. The composer keeps focus on
                                // Escape, whereas a native search input releases it.
                                return widget::operation::is_focused("composer").map(
                                    move |focused| {
                                        if focused {
                                            Message::FileCompletion(
                                                crate::file_completion::Message::Dismiss,
                                            )
                                        } else {
                                            close_search.clone()
                                        }
                                    },
                                );
                            }
                            self.file_completion.dismiss();
                            return Task::none();
                        }
                        if self.context_menu.take().is_some() {
                        } else if self.modal.is_some() {
                            return self.update_inner(Message::Dismiss);
                        } else if let Some(close_search) = close_search {
                            return self.update_inner(close_search);
                        } else if self.settings_open {
                            return self.update_inner(Message::CancelSettings);
                        } else if self.fork_panel.is_some() {
                            return self.show_forks(None);
                        } else if self.changes.open {
                            return self.changes_action(changes::Action::Toggle);
                        } else if self.worktrees.panel {
                            if self.worktrees.bases {
                                return self.worktree_action(worktrees::Action::Bases);
                            }
                            if self.worktrees.confirm.is_some() {
                                return self.worktree_action(worktrees::Action::CancelRemove);
                            }
                            return self.worktree_action(worktrees::Action::Panel);
                        } else if self.agents_open {
                            self.agents_open = false;
                            self.view_epoch = self.view_epoch.wrapping_add(1);
                        } else {
                            if let (Some(h), Some(id)) = (&self.harness, &self.selected)
                                && (h.get(id).is_some_and(|s| s.is_executing())
                                    || h.active_children(id) > 0)
                            {
                                self.stop_agent(id.clone());
                            }
                            self.acknowledge_selected_attention();
                        }
                    } else if key.as_ref() == keyboard::Key::Character("f")
                        && modifiers == keyboard::Modifiers::CTRL
                    {
                        return self.find_action(find::Action::Open);
                    } else if self.find.is_some()
                        && self.modal.is_none()
                        && self.context_menu.is_none()
                        && key == keyboard::Key::Named(keyboard::key::Named::Enter)
                        && matches!(
                            modifiers,
                            keyboard::Modifiers::NONE | keyboard::Modifiers::SHIFT
                        )
                    {
                        return widget::operation::is_focused("conversation-find").then(
                            move |focused| {
                                if focused {
                                    Task::done(Message::Find(find::Action::Step(modifiers.shift())))
                                } else {
                                    Task::none()
                                }
                            },
                        );
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
                return self.start_capture(id);
            }
            Message::CheckScreenshot(screenshot) => return self.check_screenshot(screenshot),
            Message::SmokeOpenTool(session_id) => return self.smoke_open_tool(&session_id),
            Message::Capture => return self.capture_frame(),
            Message::Screenshot(screenshot) => return self.smoke_frame(screenshot),
        }
        Task::none()
    }
    fn change_filing(&mut self, id: String, action: FilingAction) {
        if let Some(h) = self.harness.clone() {
            self.tasks
                .push(self.io.run(move || match h.file_session(&id, action) {
                    Ok(FilingOutcome::RunningCommands(kind)) => {
                        Message::RunningCommandsAtFile(id, kind)
                    }
                    Ok(FilingOutcome::Applied) => Message::Edited(Ok(())),
                    Err(error) => Message::Edited(Err(format!("{error:#}"))),
                }));
        }
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
                            display_name: None,
                            stashed: false,
                        });
                        self.save_catalog();
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
            Some(Modal::RenameProject(path)) => {
                let name = self.modal_input.trim().to_owned();
                if name.is_empty() {
                    self.error = Some("Enter a project name.".into());
                } else if let Some(project) =
                    self.catalog.projects.iter_mut().find(|p| p.path == path)
                {
                    project.display_name = Some(name);
                    self.save_catalog();
                    self.modal = None;
                }
            }
            Some(Modal::DeleteProject(path)) => {
                if let Some(h) = &self.harness {
                    let h = h.clone();
                    let mut catalog = self.catalog.clone();
                    catalog.projects.retain(|p| p.path != path);
                    let mut draft = self.new_draft.clone();
                    if draft.project.as_ref() == Some(&path) {
                        draft.project = None;
                    }
                    self.io_busy = true;
                    self.tasks.push(self.io.run_catalog(move |writer| {
                        let result = (|| {
                            h.remove_project(&path)?;
                            h.store.save_new_draft(&draft)?;
                            if catalog.selected.as_ref().is_some_and(|id| h.get(id).is_none()) {
                                catalog.selected = None;
                            }
                            writer.save_state(&h.store, catalog)
                        })().map_err(|e| format!("Could not finish deleting this project: {e:#}. Fix the problem and retry."));
                        Message::ProjectDeleted(path, result)
                    }));
                }
            }
            Some(Modal::Rename(id)) => {
                let title = self.modal_input.trim().to_owned();
                if !title.is_empty()
                    && let Some(h) = &self.harness
                {
                    let h = h.clone();
                    self.tasks.push(self.io.run(move || {
                        Message::Edited(
                            h.edit(&id, |s| s.rename(title))
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
            Some(Modal::StopCommandsAndFile(id, kind)) => {
                self.modal = None;
                self.change_filing(id, FilingAction::StopCommandsAndFile(kind));
            }
            Some(Modal::Quit) => return self.quit(),
            Some(Modal::Link(url)) => {
                self.modal = None;
                match link_destination(&url, self.current().map(|s| s.project.as_path())) {
                    Ok(destination) => return external(destination),
                    Err(error) => self.error = Some(error),
                }
            }
            None => {}
        }
        Task::none()
    }
    fn quit(&mut self) -> Task<Message> {
        if self.closing {
            return Task::none();
        }
        self.imports.retry_on_save = false;
        if self.io_busy || !self.imports.is_empty() || self.pending_settings.is_some() {
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
            return self.io.run_catalog(move |writer| {
                // Ordered after earlier jobs: no queued Send can start behind this barrier.
                h.stop_all();
                let result=(|| {
                    while h.all().iter().any(|s|s.is_executing()) {std::thread::sleep(Duration::from_millis(20));}
                    if let Some((id,draft))=draft {h.edit(&id,|s| { s.draft=draft.text; s.draft_images=draft.images; })?;}
                    h.store.save_new_draft(&new_draft)?;
                    writer.save_state(&h.store, catalog)?;
                    h.flush()?;
                    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(h.auth.flush())
                })();
                Message::Exit(result.map_err(|e|format!("Could not finish saving: {e:#}. Keep Orbit open, fix storage, then retry closing.")))
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
            if self.current().is_none() {
                subscriptions.push(Subscription::run_with(
                    Updates(harness.clone()),
                    usage_panel::summaries,
                ));
            }
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
        // Poll each mounted checkout once, independent of which session is selected.
        let mut checkouts = HashSet::new();
        for row in &self.sidebar_rows[self.sidebar_list.visible()] {
            if let SidebarRow::Session(index, _) = row {
                let path = &self.sessions[*index].project;
                if checkouts.insert(path) {
                    subscriptions.push(sidebar::branch_subscription(path));
                }
            }
        }
        if self.current().is_some() {
            subscriptions.push(iced::time::every(Duration::from_secs(5)).map(|_| Message::GitTick));
        }
        if self.notice.is_some() {
            subscriptions
                .push(iced::time::every(Duration::from_secs(1)).map(|_| Message::NoticeTick));
        }
        if self.draft_dirty || self.new_draft_dirty || self.imports.retry_on_save {
            subscriptions
                .push(iced::time::every(Duration::from_secs(1)).map(|_| Message::SaveDraft));
        }
        if self.smoke_dir.is_none() {
            subscriptions.push(self.notifications.subscription().map(Message::Notification));
        }
        subscriptions.push(
            self.file_completion
                .subscription()
                .map(Message::FileCompletion),
        );
        subscriptions.push(self.image_view.subscription().map(Message::Image));
        Subscription::batch(subscriptions)
    }
    pub fn view(&self) -> Element<'_, Message> {
        pointer_input(
            self.content(),
            self.footer_menu_open || self.worktrees.popup.is_some(),
            self.chat_selection.clone(),
        )
    }
    fn content(&self) -> Element<'_, Message> {
        let colors = &self.preset().colors;
        if let Some(check) = &self.screenshot_check {
            return check.view();
        }
        let mut notices = column![];
        if let Some((notice, _)) = &self.notice {
            notices = notices.push(
                container(
                    row![
                        Icon::Info.colored(16., colors.accent),
                        text(notice).size(13).width(Fill),
                        icon_tip(
                            theme::button(container(Icon::X.view(16.)).center_x(20).center_y(22))
                                .on_press(Message::NoticeDismiss)
                                .style(theme::text_button),
                            "Dismiss notice"
                        )
                    ]
                    .spacing(12)
                    .align_y(iced::Center),
                )
                .padding(12)
                .style(card),
            );
        }
        if let Some(error) = &self.error {
            notices = notices.push(
                container(
                    row![
                        Icon::CircleAlert.colored(16., colors.danger),
                        text(error).size(13).width(Fill),
                        theme::button(theme::primary_label("Dismiss"))
                            .on_press(Message::ErrorDismiss)
                    ]
                    .spacing(12),
                )
                .padding(12)
                .style(move |_| container::Style {
                    background: Some(colors.error_surface.into()),
                    ..Default::default()
                }),
            );
        }
        let mut body = row![
            self.sidebar(),
            self.divider(false),
            column![notices, self.center(), self.footer()]
                .width(Fill)
                .height(Fill)
        ]
        .height(Fill);
        if self.settings_open {
            body = body.push(self.divider(true)).push(self.settings_panel());
        } else if self.changes.open {
            body = body.push(self.divider(true)).push(self.changes_panel());
        } else if self.worktrees.panel {
            body = body.push(self.divider(true)).push(self.worktrees_panel());
        } else if self.agents_open {
            body = body.push(self.divider(true)).push(self.agents_panel());
        } else if let Some(panel) = &self.fork_panel {
            body = body.push(self.divider(true)).push(self.forks_panel(panel));
        }
        let mut controls = column![space().height(Fill)];
        if self.fork_family().is_some() {
            controls = controls.push(
                tooltip(
                    theme::button(
                        container(Icon::GitFork.view(22.))
                            .center_x(Fill)
                            .center_y(34),
                    )
                    .width(Fill)
                    .padding(0)
                    .on_press(Message::ShowForks(None))
                    .style(theme::text_button),
                    text("Forks").size(12),
                    tooltip::Position::Left,
                )
                .style(card),
            );
        }
        let rail = container(
            controls
                .push(
                    tooltip(
                        theme::button(container(Icon::Diff.view(22.)).center_x(Fill).center_y(34))
                            .width(Fill)
                            .padding(0)
                            .style(if self.changes.open {
                                secondary
                            } else {
                                theme::text_button
                            })
                            .on_press(Message::Changes(changes::Action::Toggle)),
                        text("Changes").size(12),
                        tooltip::Position::Left,
                    )
                    .style(card),
                )
                .push(
                    tooltip(
                        theme::button(
                            container(Icon::FolderOpen.view(22.))
                                .center_x(Fill)
                                .center_y(34),
                        )
                        .width(Fill)
                        .padding(0)
                        .style(theme::text_button)
                        .on_press(Message::Worktree(worktrees::Action::Panel)),
                        text("Worktrees").size(12),
                        tooltip::Position::Left,
                    )
                    .style(card),
                )
                .push(
                    tooltip(
                        theme::button(
                            container(crate::agent_icon::agent_icon(self.agent_count() > 0))
                                .center_x(Fill)
                                .center_y(34),
                        )
                        .width(Fill)
                        .padding(0)
                        .on_press(Message::ToggleAgents)
                        .style(theme::text_button),
                        text("Agents").size(12),
                        tooltip::Position::Left,
                    )
                    .style(card),
                )
                .push(
                    tooltip(
                        theme::button(
                            container(Icon::Settings.view(24.))
                                .center_x(Fill)
                                .center_y(34),
                        )
                        .width(Fill)
                        .padding(0)
                        .on_press(Message::ToggleSettings)
                        .style(theme::text_button),
                        text("Settings").size(12),
                        tooltip::Position::Left,
                    )
                    .style(card),
                )
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
            let shade = mouse_area(container(space().width(Fill).height(Fill)).style(move |_| {
                container::Style {
                    background: Some(
                        iced::Color {
                            a: theme::SCRIM.a * self.image_view.backdrop_opacity(),
                            ..theme::SCRIM
                        }
                        .into(),
                    ),
                    ..Default::default()
                }
            }))
            .on_press(Message::Dismiss);
            layers
                .push(opaque(shade))
                .push(
                    container(opaque(self.modal_view(modal)))
                        .center_x(Fill)
                        .center_y(Fill),
                )
                .into()
        } else if let Some(ContextMenu(target, position)) = &self.context_menu {
            let dismiss = dismiss_layer(Message::CloseMenu);
            let actions = match target {
                MenuTarget::Session(id) => column![
                    theme::button(Icon::Pencil.label("Rename", 16.))
                        .width(Fill)
                        .on_press(Message::Rename(id.clone()))
                        .style(theme::text_button),
                    theme::button(Icon::Copy.label("Copy session ID", 16.))
                        .width(Fill)
                        .on_press(Message::CopySessionId(id.clone()))
                        .style(theme::text_button),
                    widget::rule::horizontal(1),
                    theme::button(Icon::Trash2.label("Delete", 16.))
                        .width(Fill)
                        .on_press(Message::Delete(id.clone()))
                        .style(theme::danger_text),
                ],
                MenuTarget::Project(path) => {
                    let stashed = self
                        .catalog
                        .projects
                        .iter()
                        .any(|p| p.path == *path && p.stashed);
                    column![
                        theme::button(Icon::Pencil.label("Rename", 16.))
                            .width(Fill)
                            .on_press(Message::RenameProject(path.clone()))
                            .style(theme::text_button),
                        theme::button(
                            (if stashed {
                                Icon::ArchiveRestore
                            } else {
                                Icon::Archive
                            })
                            .label(if stashed { "Restore" } else { "Stash" }, 16.)
                        )
                        .width(Fill)
                        .on_press(Message::StashProject(path.clone()))
                        .style(theme::text_button),
                        widget::rule::horizontal(1),
                        theme::button(Icon::Trash2.label("Delete", 16.))
                            .width(Fill)
                            .on_press(Message::DeleteProject(path.clone()))
                            .style(theme::danger_text),
                    ]
                }
            };
            let menu_width = 200.;
            let menu = container(actions.spacing(4))
                .padding(6)
                .width(menu_width)
                .style(card);
            let x = position.x.min(self.window_width - menu_width - 10.).max(0.);
            let y = position.y.min(self.window_height - 140.).max(0.);
            let positioned = column![space().height(y), row![space().width(x), opaque(menu)]];
            layers.push(dismiss).push(positioned).into()
        } else if let Some(offset) = self.worktrees.popup {
            let layers = layers.push(dismiss_layer(Message::Worktree(
                worktrees::Action::ClosePopup,
            )));
            let popup_width = 235_f32.min(self.window_width);
            let Some(popup) = self.worktree_popup(popup_width) else {
                return layers.into();
            };
            let (left, center) = self.center_dimensions();
            let width = (center - 2. * self.chat_inset()).min(self.catalog.settings.chat_width);
            let x =
                (left + (center - width) / 2. + offset).clamp(0., self.window_width - popup_width);
            let positioned = column![
                space().height(Fill),
                row![space().width(x), popup],
                space().height(FOOTER_HEIGHT)
            ];
            layers.push(positioned).into()
        } else if self.footer_menu_open {
            let dismiss = dismiss_layer(Message::CloseFooterMenu);
            let (left, center) = self.center_dimensions();
            let inset = self.chat_inset();
            let width = (center - 2. * inset).min(self.catalog.settings.chat_width);
            let menu_width = 480_f32.min(self.window_width - 16.);
            // Follow the toolbar chip, shifting left only if needed to keep all choices on screen.
            let x = (left + (center - width) / 2. + TOOLBAR_INSET)
                .min(self.window_width - menu_width - 8.);
            let positioned = column![
                space().height(Fill),
                row![space().width(x), self.footer_popup(menu_width)],
                space().height(FOOTER_HEIGHT + TOOLBAR_INSET + TOOLBAR_BUTTON_HEIGHT + 4.),
            ];
            layers.push(dismiss).push(positioned).into()
        } else {
            layers.into()
        }
    }
    fn divider(&self, right: bool) -> Element<'_, Message> {
        let colors = &self.preset().colors;
        mouse_area(
            container(space().width(5).height(Fill)).style(move |_| container::Style {
                background: Some(colors.border.into()),
                ..Default::default()
            }),
        )
        .on_press(Message::Press(Press::Resize(right)))
        .interaction(mouse::Interaction::ResizingHorizontally)
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
    fn right_panel_open(&self) -> bool {
        self.settings_open
            || self.agents_open
            || self.fork_panel.is_some()
            || self.worktrees.panel
            || self.changes.open
    }
    fn panel_width(&self) -> f32 {
        self.catalog
            .settings
            .settings_width
            .min(self.window_width * 0.4)
    }

    fn panel_container<'a>(&self, content: widget::Column<'a, Message>) -> Element<'a, Message> {
        container(content)
            .padding(iced::Padding::new(RIGHT_PANEL_PADDING).right(theme::SCROLLBAR_EDGE_INSET))
            .width(self.panel_width())
            .height(Fill)
            .style(right_panel)
            .into()
    }

    fn agents_panel(&self) -> Element<'_, Message> {
        let colors = &self.preset().colors;
        let mut content = column![
            container(right_panel_header("Agents", Message::ToggleAgents))
                .padding(iced::Padding::ZERO.right(crate::scrollbar::GUTTER)),
            text(format!("{} running", self.agent_count()))
                .size(12)
                .style(theme::muted)
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
                .unwrap_or_else(orbit_core::store::now)
                .saturating_sub(run.started);
            let activity = self
                .harness
                .as_ref()
                .and_then(|h| h.get(&run.session_id))
                .filter(|_| run.status == orbit_core::agent::subagents::RunStatus::Running)
                .map(|c| c.status().to_string());
            let status_color = if run.status.needs_attention() {
                colors.danger
            } else if activity.is_some() {
                colors.accent
            } else {
                colors.muted
            };
            let status = activity.as_deref().unwrap_or(run.status.label());
            let metadata = match &run.configuration {
                Some(config) => {
                    let name = model(&config.model).map_or(config.model.as_str(), |m| m.name);
                    format!("{name} · {} · {elapsed}s", config.effort)
                }
                None => format!("{elapsed}s"),
            };
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
                    text(metadata)
                        .size(10)
                        .style(theme::muted)
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
        self.panel_container(content)
    }

    fn center(&self) -> Element<'_, Message> {
        let colors = &self.preset().colors;
        let Some(session) = self.current() else {
            return self.new_session();
        };
        let chat: Element<'_, Message> = if session.entries.is_empty()
            && session.child.is_none()
            && session.origin.is_none()
        {
            let (name, badge) = self.session_project_label(&session.project, 32.);
            container(
                column![
                    text("What are we building?").size(36).align_x(iced::Center),
                    row![
                        badge,
                        crate::ellipsized_text::shrinking_ellipsized_text(
                            &name,
                            24.,
                            Some(colors.muted)
                        ),
                    ]
                    .spacing(12)
                    .align_y(iced::Center),
                ]
                .spacing(18)
                .align_x(iced::Center),
            )
            .padding(24)
            .center_x(Fill)
            .center_y(Fill)
            .into()
        } else {
            let epoch = self.view_epoch;
            let range = self.chat_range();
            let mut children: Vec<(TranscriptKey, Element<'_, Message>)> = vec![(
                TranscriptKey::Top,
                space().height(self.transcript.prefix(range.start)).into(),
            )];
            // Attach the marker to the last projected inherited row. The same
            // resize sensor accounts for it when groups expand or rows reflow.
            let boundary = session.origin.as_ref().map(|o| {
                self.transcript
                    .order
                    .partition_point(|&id| ActivityRow::from_id(id).source() < o.inherited)
            });
            if self.transcript.order.is_empty()
                && let Some(o) = &session.origin
            {
                children.push((TranscriptKey::ForkPoint, self.fork_separator(o)));
            }
            for (position, &index) in range.clone().zip(&self.transcript.order[range.clone()]) {
                let projected = ActivityRow::from_id(index);
                let entry = &session.entries[projected.source()];
                // Resize-only sensors measure every mounted row, even outside the
                // viewport. Adding on_show would gate measurements on visibility,
                // mixing real scroll offsets with stale height estimates.
                let content = self.fork_row(session, entry, projected);
                let content: Element<'_, Message> = match (&session.origin, boundary) {
                    (Some(o), Some(0)) if position == 0 => {
                        column![self.fork_separator(o), content].into()
                    }
                    (Some(o), Some(n)) if position + 1 == n => {
                        column![content, self.fork_separator(o)].into()
                    }
                    _ => content,
                };
                let row = if let ActivityRow::Entry(_) = projected
                    && let Some(md) = self.markdown.get(&entry.id)
                    && let Some(window) = &md.window
                {
                    let expected =
                        self.transcript.height(position) - projected.spacing(&session.entries);
                    window.observe_row(content, expected, move |height, geometry| {
                        Message::MarkdownGeometry(epoch, index, md.source.clone(), height, geometry)
                    })
                } else {
                    widget::sensor(content)
                        .on_resize(move |size| Message::RowSize(epoch, index, size.height))
                        .into()
                };
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
            let content = crate::transcript_viewport::observe(
                self.chat_lane(
                    container(transcript).padding([CHAT_VERTICAL_PADDING, 0.]),
                    crate::scrollbar::GUTTER,
                ),
                epoch,
                self.find.as_ref().map(|f| f.frame.clone()),
                move |offset, height, content_height| {
                    Message::Scroll(
                        epoch,
                        offset,
                        height,
                        offset + height >= content_height - 40.,
                    )
                },
            );
            scroll(content).id("chat").height(Fill).into()
        };
        // Keep the scrollable at the base of the same stack even when Find is closed.
        let mut chat = stack![chat].width(Fill).height(Fill);
        if !self.activity.prompts.is_empty() {
            chat = chat.push(prompt_rail::rail(
                session,
                &self.activity.prompts,
                self.catalog.settings.chat_width,
            ));
        }
        if let Some(find) = &self.find {
            chat = chat.push(find.popup());
        }
        if !self.auto_scroll {
            let diameter = 32.;
            let jump = theme::button(container(Icon::ArrowDown.view(16.)).center(diameter))
                .padding(0)
                .style(move |theme, status| {
                    let mut style = secondary(theme, status);
                    style.border.radius = (diameter / 2.).into();
                    style
                })
                .on_press(Message::JumpToLatest);
            chat = chat.push(
                container(footer_tip(jump, "Jump to latest".into()))
                    .padding(8)
                    .center_x(Fill)
                    .align_bottom(Fill),
            );
        }
        let running = self.running();
        if let Some(child) = &session.child {
            use orbit_core::agent::subagents::RunStatus;
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
                Some(RunStatus::Running) => colors.accent,
                Some(RunStatus::Completed) => colors.success,
                Some(RunStatus::Failed | RunStatus::Interrupted) => colors.danger,
                Some(RunStatus::Cancelled) => colors.warning,
                None => colors.muted,
            };
            let header = widget::responsive(move |size| {
                let compact = size.width < 640.;
                let mut back = row![Icon::ArrowLeft.view(12.)]
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
                    theme::button(container(Icon::Square.view(16.)).center_x(32).center_y(32))
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
                        && !self.footer_menu_open
                        && self.worktrees.popup.is_none(),
                    !self.settings_open
                        && !self.search_active
                        && self.find.is_none()
                        && self.modal.is_none()
                        && !self.footer_menu_open
                        && self.worktrees.popup.is_none()
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
        // Keep the editor in a fixed sibling slot as attachments and completions come and go.
        let mut above_editor = column![];
        let gap = 10;
        let mut pending = self.imports.tiles(self.draft_key()).peekable();
        if !self.attachments.is_empty() || pending.peek().is_some() {
            let images = row(self
                .attachments
                .iter()
                .enumerate()
                .map(|(index, media)| self.image_view.attachment(media, index))
                .chain(pending.map(crate::image_view::PendingAttachment::view)))
            .spacing(8);
            above_editor = above_editor
                .push(crate::scrollbar::horizontal(images))
                .push(space().height(gap));
        }
        if self.file_completion.is_open() && self.file_completion_enabled() {
            above_editor = above_editor
                .push(
                    self.file_completion
                        .view(colors)
                        .map(Message::FileCompletion),
                )
                .push(space().height(gap));
        }
        let compacting = self
            .harness
            .as_ref()
            .and_then(|h| h.get(&session.id))
            .is_some_and(|c| c.is_manually_compacting());
        let (send_icon, action) = if compacting {
            (Icon::ArrowUp, "Wait for compaction to finish")
        } else if running {
            (Icon::ArrowRight, "Steer agent")
        } else {
            (Icon::ArrowUp, "Send message")
        };
        let toolbar = row![
            opaque(self.model_selector(session, running)),
            space().width(Fill),
            opaque(footer_tip(
                theme::button(send_icon.view(16.))
                    .width(TOOLBAR_BUTTON_HEIGHT)
                    .height(TOOLBAR_BUTTON_HEIGHT)
                    .padding(5)
                    .style(secondary)
                    .on_press_maybe(
                        (!compacting
                            && !self.io_busy
                            && !self.composer_importing()
                            && (!self.editor.is_empty() || !self.attachments.is_empty()))
                        .then_some(Message::Send)
                    ),
                action.into(),
            )),
        ]
        .align_y(iced::Center)
        .padding(iced::Padding::new(TOOLBAR_INSET).top(0.));
        let toolbar = mouse_area(toolbar)
            .on_press(Message::FocusComposer)
            .interaction(mouse::Interaction::Text);
        let editor = container(column![
            self.composer_editor(running && !compacting),
            toolbar
        ])
        .style(card);
        let compose = column![above_editor, editor];
        column![chat, self.chat_lane(compose, 0.)]
            .width(Fill)
            .height(Fill)
            .into()
    }

    fn center_dimensions(&self) -> (f32, f32) {
        let left = self
            .catalog
            .settings
            .sidebar_width
            .min(self.window_width * 0.4)
            + 5.;
        let right = 50.
            + if self.right_panel_open() {
                self.panel_width() + 5.
            } else {
                0.
            };
        (left, self.window_width - left - right)
    }

    fn chat_inset(&self) -> f32 {
        if self.center_dimensions().1 < 240. {
            crate::scrollbar::GUTTER
        } else {
            24.
        }
    }

    fn chat_lane<'a>(
        &self,
        content: impl Into<Element<'a, Message>>,
        reserved_right: f32,
    ) -> Element<'a, Message> {
        // The transcript's scroll wrapper has already supplied part of the right inset.
        let inset = self.chat_inset();
        container(
            container(content)
                .max_width(self.catalog.settings.chat_width)
                .width(Fill),
        )
        .center_x(Fill)
        .padding(
            iced::Padding::ZERO
                .left(inset)
                .right(inset - reserved_right),
        )
        .into()
    }

    fn model_selector(&self, session: &Session, running: bool) -> Element<'_, Message> {
        let label = format!(
            "{} {}",
            model(&session.model).expect("catalog model").name,
            effort_name(&session.effort)
        );
        let width = (text_width(&label, 12.).ceil() + 28. + if session.fast { 16. } else { 0. })
            .min(
                self.center_dimensions().1
                    - 2. * self.chat_inset()
                    - 2. * TOOLBAR_INSET
                    - TOOLBAR_BUTTON_HEIGHT,
            );
        let mut contents = row![ellipsized_text(&label, 12., None)]
            .spacing(4)
            .height(Fill)
            .align_y(iced::Center);
        if session.fast {
            contents = contents.push(Icon::Zap.view(12.));
        }
        let chevron = if self.footer_menu_open {
            Icon::ChevronUp
        } else {
            Icon::ChevronDown
        };
        theme::button(contents.push(chevron.view(12.)))
            .width(width)
            .height(TOOLBAR_BUTTON_HEIGHT)
            .padding([4, 6])
            .style(|theme, status| {
                let mut style = secondary(theme, status);
                if matches!(
                    status,
                    widget::button::Status::Active | widget::button::Status::Disabled
                ) {
                    style.background = None;
                }
                style
            })
            // The dismiss layer handles a second press; don't reopen on release.
            .on_press_maybe(
                (!self.footer_menu_open && !running).then_some(Message::ToggleFooterMenu),
            )
            .into()
    }

    fn file_completion_enabled(&self) -> bool {
        !self.imports.holds_paste_cursor()
            && !self.io_busy
            && !self.closing
            && !self.close_requested
            && self.modal.is_none()
            && self.context_menu.is_none()
            && self.active_questionnaire.is_none()
            && !self.footer_menu_open
            && self.worktrees.popup.is_none()
    }
    fn composer_editor(&self, running: bool) -> Element<'_, Message> {
        let line_height = self.catalog.settings.font_size * COMPOSER_LINE_HEIGHT_FACTOR;
        let editor = text_editor(&self.editor)
            .id("composer")
            .style(theme::inline_answer_editor)
            .placeholder(if running {
                "Steer the agent after its current tool batch…"
            } else {
                "Message Orbit…"
            })
            .on_action(Message::Editor)
            .key_binding(|event| {
                if self.file_completion_enabled()
                    && self.file_completion.is_open()
                    && event.key == keyboard::Key::Named(keyboard::key::Named::Escape)
                {
                    // Let the central handler dismiss the picker without the
                    // native Unfocus binding taking focus away from the composer.
                    return None;
                }
                self.file_completion_enabled()
                    .then(|| self.file_completion.binding(&event))
                    .flatten()
                    .map(|message| text_editor::Binding::Custom(Message::FileCompletion(message)))
                    .or_else(|| composer_key_binding(event))
            })
            .line_height(iced::widget::text::LineHeight::Absolute(line_height.into()))
            .min_height(COMPOSER_MIN_LINES as f32 * line_height)
            .size(self.catalog.settings.font_size);
        let editor = crate::growing_editor::growing_editor(
            editor,
            &self.editor,
            &self.spellcheck.marks,
            self.spellcheck.typing_at,
            COMPOSER_MAX_LINES as f32 * line_height + 2. * crate::growing_editor::PADDING,
            Message::EditorScroll,
            |message| match message {
                Message::Editor(text_editor::Action::Scroll { lines }) => Some(*lines),
                _ => None,
            },
        );
        self.session_list_focus.composer(editor)
    }

    fn activity_row(&self, projected: ActivityRow) -> Element<'_, Message> {
        let colors = &self.preset().colors;
        let session = self.current().expect("visible transcript");
        let index = projected.source();
        let entry = &session.entries[index];
        match projected {
            ActivityRow::Entry(index) => self.entry_view(entry, index),
            ActivityRow::Activity(_) => {
                let block = self.activity.block(index);
                let summary = block.summary.as_ref().expect("activity summary");
                let active = block.range.end == session.entries.len() && self.running();
                let working = active || summary.running > 0;
                let mut header = row![space().width(12).height(12)]
                    .spacing(8)
                    .align_y(iced::Center);
                header = header.push(ellipsized_text(
                    &summary.label(active),
                    12.,
                    Some(colors.muted),
                ));
                if working && summary.tools > 0 {
                    let progress = if summary.running > 0 {
                        format!(
                            "{} completed · {} running",
                            summary.completed, summary.running
                        )
                    } else {
                        format!("{} completed", summary.completed)
                    };
                    header = header.push(text(progress).size(11).style(theme::muted));
                }
                if summary.show_failures() {
                    header = header.push(
                        text(format!("{} failed", summary.failed))
                            .size(12)
                            .color(colors.danger),
                    );
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
                            chevron_icon(self.execution.contains(&entry.id)),
                            text(format!(
                                "Execution details · {wrappers} code-mode call{}",
                                if wrappers == 1 { "" } else { "s" }
                            ))
                            .size(11)
                            .style(theme::muted)
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
                            chevron_icon(self.expanded.contains(&entry.id)),
                            ellipsized_text(&preview, 12., Some(colors.muted))
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
    fn transcript_run(&self, entry: &Entry) -> Option<RunInfo> {
        self.harness
            .as_ref()?
            .resident_subagent(&self.current()?.id, transcript_run_key(entry)?)
            .or_else(|| self.historical_runs.get(&entry.id)?.clone())
    }
    fn subagent_notice_view<'a>(&'a self, entry: &'a Entry, index: usize) -> Element<'a, Message> {
        let colors = &self.preset().colors;
        let open = self.expanded.contains(&entry.id);
        let run = self.transcript_run(entry);
        let (label, color) = match &run {
            Some(run) => (
                format!(
                    "{} · {} · {}",
                    run.profile.label(),
                    run.title,
                    run.status.label()
                ),
                if run.status.needs_attention() {
                    colors.danger
                } else if run.status == orbit_core::agent::subagents::RunStatus::Completed {
                    colors.success
                } else {
                    colors.muted
                },
            ),
            None => ("Subagent update".into(), colors.muted),
        };
        let mut block = column![
            theme::button(
                row![
                    chevron_icon(open),
                    text("●").size(11).color(color),
                    ellipsized_text(&label, 12., Some(colors.muted))
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
                    theme::button(Icon::ArrowUpRight.label("Open subagent", 16.))
                        .style(theme::text_button)
                        .on_press(Message::Select(run.session_id)),
                );
            }
            block = block.push(chat_selection::plain(
                &entry.text,
                12.,
                Some(colors.muted),
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
        let colors = &self.preset().colors;
        match &entry.kind {
            EntryKind::Tool {
                name,
                input,
                complete,
                parent,
                failed,
                media,
                web,
                command_status,
                ..
            } => {
                let open = self.expanded.contains(&entry.id);
                let preview = activity::tool_preview(name, input);
                let status = activity::tool_status(*complete, *failed, *command_status);
                let status_color = match status {
                    CommandStatus::Running => colors.warning,
                    CommandStatus::Succeeded => colors.success,
                    CommandStatus::Failed | CommandStatus::Interrupted => colors.danger,
                };
                let mut block = column![
                    theme::button(
                        row![
                            chevron_icon(open),
                            text("●").size(11).color(status_color),
                            text(activity::tool_name(name)).size(12).color(colors.text),
                            ellipsized_text(&preview, 12., Some(colors.muted)),
                            text(match status {
                                CommandStatus::Running => "running",
                                CommandStatus::Succeeded => "",
                                CommandStatus::Failed => "failed",
                                CommandStatus::Interrupted => "interrupted",
                            })
                            .size(11)
                            .color(
                                if matches!(
                                    status,
                                    CommandStatus::Failed | CommandStatus::Interrupted
                                ) {
                                    colors.danger
                                } else {
                                    colors.muted
                                }
                            )
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
                    && let Some(run) = self.transcript_run(entry)
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
                                chevron_icon(raw),
                                text("Raw input").size(11).style(theme::muted)
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
                            .style(theme::muted),
                        );
                        for source in &metadata.sources {
                            details = details.push(
                                theme::button(ellipsized_text(
                                    &source.title,
                                    12.,
                                    Some(colors.accent),
                                ))
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
                        Some(colors.muted),
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
                Some(colors.warning),
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
                        markdown.view(
                            markdown::Settings::with_text_size(
                                12.,
                                theme::markdown_style(&self.preset().theme),
                            ),
                            &chat_selection::Viewer::new(self.chat_selection.clone(), index)
                                .with_images(&self.markdown_images)
                                .with_equations(&self.equations, &entry.id),
                        )
                    } else {
                        chat_selection::plain(
                            &entry.text,
                            12.,
                            Some(colors.muted),
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
                if let EntryKind::User {
                    images,
                    files,
                    steering,
                    ..
                } = &entry.kind
                {
                    if *steering {
                        content = content.push(
                            text("Steering · waiting for the next boundary")
                                .size(11)
                                .color(colors.accent),
                        );
                    }
                    for media in images {
                        content = content.push(self.image_view.thumbnail(media));
                    }
                    for file in files {
                        content = content.push(
                            text(format!("{} · {} bytes captured", file.name, file.bytes))
                                .size(11)
                                .style(theme::muted),
                        );
                        if let Some(image) = &file.image {
                            content = content.push(self.image_view.thumbnail(image));
                        }
                    }
                }
                let paint = self
                    .find
                    .as_ref()
                    .map(|f| f.paint(index, entry))
                    .unwrap_or_default();
                if let Some(markdown) = self.markdown.get(&entry.id) {
                    let mut viewer =
                        chat_selection::Viewer::new(self.chat_selection.clone(), index)
                            .with_images(&self.markdown_images)
                            .with_equations(&self.equations, &entry.id);
                    viewer.find = paint;
                    content = content.push(markdown.view(
                        markdown::Settings::with_text_size(
                            self.catalog.settings.font_size,
                            theme::markdown_style(&self.preset().theme),
                        ),
                        &viewer,
                    ));
                } else {
                    content = content.push(chat_selection::plain_find(
                        &entry.text,
                        self.catalog.settings.font_size,
                        None,
                        self.chat_selection.clone(),
                        Position {
                            entry: index,
                            block: 0,
                            byte: 0,
                        },
                        paint,
                    ));
                }
                if matches!(entry.kind, EntryKind::User { .. }) {
                    container(content).padding(12).style(card).into()
                } else {
                    container(content).padding([6, 12]).width(Fill).into()
                }
            }
        }
    }
    fn session_project_label(
        &self,
        checkout: &Path,
        size: f32,
    ) -> (String, Element<'static, Message>) {
        let path = self.catalog.project_origin(checkout);
        let name = self
            .catalog
            .projects
            .iter()
            .find(|p| p.path == path)
            .map(Project::name)
            .unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            });
        let badge = project_badge_sized(path, &name, size);
        (name, badge)
    }

    fn project_row(&self, project: &Project) -> Element<'_, Message> {
        let name = project.name();
        let open = theme::button(
            row![
                project_badge_named(&project.path, &name),
                ellipsized_text(&name, 13., None),
            ]
            .spacing(12)
            .align_y(iced::Center),
        )
        .on_press(Message::Create(project.path.clone()))
        .padding([6, 10])
        .width(Fill)
        .style(theme::text_button);
        let favorite = icon_tip(
            theme::button(
                container(
                    (if project.favorite {
                        Icon::StarFilled
                    } else {
                        Icon::Star
                    })
                    .view(18.),
                )
                .center_y(24),
            )
            .on_press(Message::Favorite(project.path.clone()))
            .padding([3, 8])
            .style(theme::text_button),
            if project.favorite {
                "Remove from favorites"
            } else {
                "Add to favorites"
            },
        );
        mouse_area(row![open, favorite].align_y(iced::Center))
            .on_right_press(Message::Press(Press::ProjectMenu(project.path.clone())))
            .into()
    }

    fn new_session(&self) -> Element<'_, Message> {
        widget::responsive(|size| {
            const INSET: f32 = 36.;
            const GAP: f32 = 36.;
            const MIN_PICKER: f32 = 300.;
            let wide = self.harness.is_some()
                && size.width >= INSET * 2. + MIN_PICKER + GAP + usage_panel::WIDTH;
            let mut columns = row![container(self.project_picker()).width(Fill)];
            let mut max_width = 650.;
            if wide {
                columns = columns
                    .spacing(GAP)
                    .push(usage_panel::view(self.usage_summary.as_ref()));
                max_width += GAP + usage_panel::WIDTH;
            }
            container(scroll(
                container(columns)
                    .max_width(max_width - crate::scrollbar::GUTTER)
                    .padding(iced::Padding::new(INSET).right(INSET - crate::scrollbar::GUTTER)),
            ))
            .center_x(Fill)
            .height(Fill)
            .into()
        })
        .into()
    }

    fn project_picker(&self) -> Element<'_, Message> {
        let mut projects = column![].spacing(2);
        let mut stashed = column![].spacing(2);
        let mut ordered: Vec<_> = self.catalog.projects.iter().collect();
        ordered.sort_by_key(|p| !p.favorite);
        let stashed_count = ordered.iter().filter(|p| p.stashed).count();
        for project in ordered {
            if project.stashed {
                if self.stashed_open {
                    stashed = stashed.push(self.project_row(project));
                }
            } else {
                projects = projects.push(self.project_row(project));
            }
        }
        let mut content = column![
            column![
                text("New session").size(18),
                text("Choose a project to get started")
                    .size(13)
                    .style(theme::muted),
            ]
            .spacing(10),
            projects,
            theme::button(Icon::FolderPlus.label("Add project", 13.))
                .on_press_maybe(self.harness.as_ref().map(|_| Message::AddProject))
                .padding([6, 10])
                .style(theme::text_button)
        ]
        .spacing(14);
        if stashed_count > 0 {
            content = content.push(
                theme::button(
                    row![
                        chevron_icon(self.stashed_open),
                        text(format!("Stashed projects ({stashed_count})")).size(13)
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                )
                .on_press(Message::ToggleStashed)
                .padding([6, 10])
                .style(theme::text_button),
            );
            if self.stashed_open {
                content = content.push(stashed);
            }
        }
        if self.catalog.projects.is_empty() {
            content=content.push(text("Add a local folder to begin. Orbit can read and edit files and run commands in your projects.").size(13).style(theme::muted));
        }
        if self.account.is_none() {
            content=content.push(container(column![text("Connect your ChatGPT subscription").size(16),text("Sign in from Settings. Your login is saved securely in your private orbit configuration directory.").size(13).style(theme::muted),theme::button(Icon::Settings.label("Open Settings", 16.)).on_press(Message::ToggleSettings).style(theme::text_button)].spacing(10)).padding(18).style(card));
        }
        content.into()
    }
    fn footer(&self) -> Element<'_, Message> {
        let footer =
            widget::responsive(|size| self.footer_content(size.width)).height(FOOTER_HEIGHT);
        if self.current().is_some() {
            self.chat_lane(footer, 0.)
        } else {
            footer.into()
        }
    }

    fn footer_content(&self, width: f32) -> Element<'_, Message> {
        let colors = &self.preset().colors;
        let Some(session) = self.current() else {
            return container(
                row![
                    text("Orbit")
                        .size(11)
                        .style(theme::muted)
                        .wrapping(text::Wrapping::None),
                    space().width(Fill),
                    text(if self.account.is_some() {
                        "ChatGPT connected"
                    } else {
                        "Not signed in"
                    })
                    .size(11)
                    .style(theme::muted)
                    .wrapping(text::Wrapping::None),
                ]
                .align_y(iced::Center),
            )
            .padding([9, 14])
            .height(36)
            .into();
        };
        let conversation = self.harness.as_ref().and_then(|h| h.get(&session.id));
        let turn_usage = conversation
            .as_ref()
            .map_or(orbit_core::agent::TurnUsage::default(), |c| {
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
        let (name, badge) = self.session_project_label(&session.project, 16.);
        let name_width = text_width(&name, 11.).ceil().min(160.);
        let rate_label = format!("TPS: {rate:.0}");
        let cache_label = format!("{cache:.0}% cache");
        let draft = self.draft_session.is_some();
        let branch_icon_size = 12.;
        let chevron_size = 10.;
        let icon_gap = 3.;
        let pill_inset = 6.;
        let git = self.git_info().map(|git| {
            let (label, counts) = match git {
                crate::git::Info::Changes {
                    branch,
                    added,
                    removed,
                } => (
                    branch,
                    (*added != 0 || *removed != 0)
                        .then(|| (format!("+{added}"), format!("−{removed}"))),
                ),
                crate::git::Info::Unavailable(message) => (message, None),
            };
            let label_width = text_width(label, 11.).ceil().min(126.);
            let pill = matches!(git, crate::git::Info::Changes { .. });
            let mut width = label_width;
            if pill {
                width += 2. * pill_inset + branch_icon_size + icon_gap;
                if draft {
                    width += chevron_size + icon_gap;
                }
            }
            if let Some((added, removed)) = &counts {
                width += 8.
                    + 2. * pill_inset
                    + text_width(added, 11.).ceil()
                    + text_width(removed, 11.).ceil();
            }
            (width, label_width, label, counts, pill, git)
        });

        // Reserve from most to least important. Once an item doesn't fit, no
        // lower-priority item may reappear in the remaining gap.
        let mut remaining = width;
        let mut reserve = |cost| {
            remaining -= cost;
            remaining >= 0.
        };
        let context = reserve(text_width(&tokens, 11.).ceil());
        let show_git = git.as_ref().is_none_or(|(width, ..)| reserve(10. + width));
        let project = reserve(10. + 16. + 4. + name_width);
        let cache = reserve(16. + text_width(&cache_label, 11.).ceil());
        let tps = reserve(16. + text_width(&rate_label, 11.).ceil());

        let mut info = row![].align_y(iced::Center);
        if project {
            let badge = row![
                badge,
                container(ellipsized_text(&name, 11., Some(colors.muted))).width(name_width),
            ]
            .spacing(4)
            .align_y(iced::Center);
            info = info
                .push(footer_tip(badge, session.project.display().to_string()))
                .push(space().width(10));
        }
        if show_git && let Some((width, label_width, label, counts, pill, git)) = git {
            let label = container(ellipsized_text(label, 11., (!pill).then_some(colors.muted)))
                .width(label_width);
            let control: Element<_> = if pill {
                let open = self.worktrees.popup.is_some();
                let icon = if open && self.worktrees.slow_popup_load {
                    Icon::LoaderCircle.spinning(branch_icon_size, true)
                } else {
                    Icon::GitFork.view(branch_icon_size)
                };
                let mut content = row![icon, label].spacing(icon_gap).align_y(iced::Center);
                if draft {
                    content = content.push(Icon::ChevronDown.view(chevron_size));
                }
                let offset = if project { 30. + name_width } else { 0. };
                let button = theme::button(content)
                    .padding([2., pill_inset])
                    .style(if open { secondary } else { sidebar_control })
                    .on_press_maybe(
                        (draft && !self.io_busy && !open)
                            .then_some(Message::Worktree(worktrees::Action::Popup(offset))),
                    );
                if draft {
                    button.into()
                } else {
                    footer_tip(
                        button,
                        format!("This chat stays on its original worktree · {git}"),
                    )
                }
            } else {
                footer_tip(label, git.to_string())
            };
            let mut group = row![control].align_y(iced::Center).spacing(4.);
            if let Some((added, removed)) = counts {
                group = group.push(
                    theme::button(
                        row![
                            text(added).size(11).color(colors.success),
                            text(removed).size(11).color(colors.danger)
                        ]
                        .spacing(4.),
                    )
                    .padding([2., pill_inset])
                    .style(if self.changes.open {
                        secondary
                    } else {
                        theme::text_button
                    })
                    .on_press(Message::Changes(changes::Action::Toggle)),
                );
            }
            info = info.push(group.width(width)).push(space().width(10));
        }
        info = info.push(space().width(Fill));
        if context {
            info = info.push(footer_tip(
                text(tokens).size(11).style(theme::muted),
                token_details,
            ));
        }
        for (visible, label, details) in [
            (tps, rate_label, rate_details),
            (cache, cache_label, cache_details),
        ] {
            if visible {
                info = info.push(footer_separator()).push(footer_tip(
                    text(label).size(11).style(theme::muted),
                    details,
                ));
            }
        }
        container(info)
            .center_y(FOOTER_HEIGHT)
            .padding(iced::Padding::ZERO.bottom(4))
            .width(Fill)
            .into()
    }

    fn footer_options(&self, control: FooterControl) -> Vec<String> {
        match control {
            FooterControl::Model => MODELS.iter().map(|m| m.id.to_owned()).collect(),
            FooterControl::Effort => model(&self.current().expect("footer session").model)
                .expect("catalog model")
                .efforts
                .iter()
                .map(|value| (*value).into())
                .collect(),
        }
    }

    fn footer_menu_choice(
        &self,
        label: &str,
        checked: bool,
        highlighted: bool,
        message: Option<Message>,
    ) -> Element<'static, Message> {
        theme::button(
            row![
                ellipsized_text(label, FOOTER_MENU_TEXT_SIZE, None),
                if checked {
                    Icon::Check.colored(16., self.preset().colors.accent)
                } else {
                    space().width(16).height(16).into()
                },
            ]
            .spacing(8)
            .align_y(iced::Center),
        )
        .width(Fill)
        .height(FOOTER_MENU_ROW_HEIGHT)
        .padding([6, 8])
        .on_press_maybe(message)
        .style(move |theme, status| theme::menu_button(theme, status, highlighted))
        .into()
    }
    fn footer_popup(&self, available_width: f32) -> Element<'_, Message> {
        let session = self.current().expect("footer menu has a session");
        let enabled = !self.io_busy && !self.running();
        let list_height = (self.window_height - 170.).clamp(60., 240.);
        let heading_height = 28.;
        let column_spacing = 6.;
        let choices = |control, title| {
            let mut options = column![].spacing(FOOTER_MENU_ROW_GAP);
            for option in self.footer_options(control) {
                let (label, chosen, message) = match control {
                    FooterControl::Model => (
                        model(&option).expect("catalog model").name,
                        option == session.model,
                        Message::SessionModel(option.clone()),
                    ),
                    FooterControl::Effort => (
                        effort_name(&option),
                        option == session.effort,
                        Message::SessionEffort(option.clone()),
                    ),
                };
                options = options.push(self.footer_menu_choice(
                    label,
                    chosen,
                    chosen,
                    enabled.then_some(message),
                ));
            }
            column![
                container(text(title).size(12).style(theme::muted))
                    .height(heading_height)
                    .padding([6, 8]),
                scroll(options).height(list_height),
            ]
            .spacing(column_spacing)
        };
        let mut speed = row![].spacing(2);
        for (label, fast) in [("Standard", false), ("Fast", true)] {
            let chosen = session.fast == fast;
            let label = if fast {
                Icon::Zap.label(label, 12.)
            } else {
                text(label).size(12).into()
            };
            speed = speed.push(
                theme::button(label)
                    .padding([5, 8])
                    .on_press_maybe(enabled.then_some(Message::SessionFast(fast)))
                    .style(move |theme, status| theme::menu_button(theme, status, chosen)),
            );
        }
        opaque(
            container(
                column![
                    row![
                        choices(FooterControl::Model, "Model").width(iced::Length::FillPortion(3)),
                        widget::rule::vertical(1),
                        choices(FooterControl::Effort, "Reasoning")
                            .width(iced::Length::FillPortion(2)),
                    ]
                    // The fill-height divider must not make this shrink-sized popup
                    // absorb the overlay's spare height on tall windows.
                    .height(heading_height + column_spacing + list_height)
                    .spacing(6),
                    widget::rule::horizontal(1),
                    speed,
                ]
                .spacing(8),
            )
            .padding(6)
            .width(available_width.min(480.))
            .style(card),
        )
    }
    fn modal_view(&self, modal: &Modal) -> Element<'_, Message> {
        let colors = &self.preset().colors;
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
            Modal::RenameProject(_) => (
                "Rename project",
                "Change the display name in Orbit. The folder on disk is not renamed.",
                "Rename",
            ),
            Modal::DeleteProject(_) => (
                "Delete project?",
                "Permanently delete this project from Orbit and all its saved conversations, including subagents and results. Files in the project folder are not deleted. Stop all running sessions first.",
                "Delete project",
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
                "Remove saved ChatGPT credentials from this Orbit installation. Your sessions are kept.",
                "Sign out",
            ),
            Modal::StopCommandsAndFile(_, kind) => (
                match kind {
                    FilingKind::Settled => "Stop running commands and settle?",
                    FilingKind::Deferred => "Stop running commands and defer?",
                },
                "Commands are still running in this session. Stopping them won’t undo changes they’ve already made. Other sessions won’t be affected.",
                match kind {
                    FilingKind::Settled => "Stop commands and settle",
                    FilingKind::Deferred => "Stop commands and defer",
                },
            ),
            Modal::Quit => (
                "Stop work and quit?",
                "Running turns and shell processes will be cancelled. Already completed commands are not undone. Sessions are saved.",
                "Stop and quit",
            ),
            Modal::Link(_) => (
                "Open link?",
                "This link came from the conversation. Check its destination before opening it in the default application.",
                "Open link",
            ),
        };
        let mut content = column![
            text(title).size(24),
            text(body).size(14).style(theme::muted)
        ]
        .spacing(18);
        match modal {
            Modal::AddProject | Modal::Rename(_) | Modal::RenameProject(_) => {
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
                        theme::button(Icon::FolderOpen.label("Browse folders…", 16.))
                            .on_press(Message::Browse)
                            .style(secondary),
                    );
                }
            }
            Modal::DeleteProject(path) => {
                if let Some(project) = self.catalog.projects.iter().find(|p| p.path == *path) {
                    content = content.push(text(project.name()).size(16));
                }
                content = content.push(
                    text(path.display().to_string())
                        .size(13)
                        .style(theme::muted),
                );
            }
            Modal::StopCommandsAndFile(id, _) => {
                if let Some(session) = self.sessions.iter().find(|s| s.id == *id) {
                    content = content.push(text(session.title.clone()).size(16));
                }
            }
            Modal::Link(url) => content = content.push(text(url.clone()).size(13)),
            _ => {}
        }
        if let Some(error) = &self.error {
            content = content.push(text(error).size(12).color(colors.danger));
        }
        let confirm = if matches!(modal, Modal::Delete(_) | Modal::DeleteProject(_)) {
            theme::action(confirm).style(theme::danger)
        } else {
            theme::primary_action(confirm)
        };
        content = content.push(
            row![
                space().width(Fill),
                theme::cancel("Cancel").on_press(Message::Dismiss),
                confirm.on_press(Message::Confirm)
            ]
            .spacing(12),
        );
        container(content)
            .padding(28)
            .max_width(540)
            .style(card)
            .into()
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
fn text_width(value: &str, size: f32) -> f32 {
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
    text("·")
        .size(11)
        .style(theme::muted)
        .width(16)
        .center()
        .into()
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
        icon_tip(
            theme::button(container(Icon::X.view(16.)).center_x(20).center_y(22))
                .on_press(on_close)
                .style(theme::text_button),
            "Close panel"
        ),
    ]
    .align_y(iced::Center)
    .into()
}

fn bottom() -> Task<Message> {
    widget::operation::snap_to("chat", scrollable::RelativeOffset::END)
}

fn project_badge_named(path: &Path, name: &str) -> Element<'static, Message> {
    project_badge_sized(path, name, 16.)
}

fn project_badge_sized(path: &Path, name: &str, size: f32) -> Element<'static, Message> {
    let color = project_color(path);
    container(
        text(project_initials(name))
            .size(size / 2.)
            .line_height(text::LineHeight::Absolute((size * 0.625).into())),
    )
    .center_x(size)
    .center_y(size)
    .style(move |t| container::Style {
        text_color: Some(theme::contrasting_ink(
            color,
            iced::theme::palette::mix(theme::colors(t).panel, color, theme::BADGE_ALPHA),
        )),
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
    let seconds = orbit_core::store::now().saturating_sub(updated);
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
/// Resolve Markdown destinations without letting arbitrary URI schemes reach xdg-open.
fn link_destination(destination: &str, workspace: Option<&Path>) -> Result<OsString, String> {
    if destination.is_empty() {
        return Err("The link destination is empty.".into());
    }
    let url = match url::Url::parse(destination) {
        Ok(url) => url,
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            let base = if Path::new(destination).is_absolute() {
                Path::new("/")
            } else {
                workspace.ok_or("Select a conversation to open project-relative links.")?
            };
            url::Url::from_directory_path(base)
                .map_err(|()| "The conversation project must be an absolute path.")?
                .join(destination)
                .map_err(|error| format!("Invalid link destination: {error}"))?
        }
        Err(error) => return Err(format!("Invalid link destination: {error}")),
    };
    match url.scheme() {
        "http" | "https" => Ok(url.as_str().into()),
        "file" => url
            .to_file_path()
            .map(PathBuf::into_os_string)
            .map_err(|()| "Only local file:// URLs can be opened.".into()),
        _ => Err("Use a local path, file:// URL, or http/https link.".into()),
    }
}

fn external(destination: impl Into<OsString>) -> Task<Message> {
    let destination = destination.into();
    Task::perform(
        async move {
            let recovery = format!(
                "Open {} manually, or check its default application with xdg-open.",
                destination.to_string_lossy()
            );
            // Some desktop openers stay alive until the document is closed. Wait
            // asynchronously so exiting Orbit drops the wait, not the external app.
            let status = tokio::process::Command::new("xdg-open")
                .arg(destination)
                .status()
                .await
                .map_err(|error| format!("Could not launch xdg-open: {error}. {recovery}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("xdg-open failed ({status}). {recovery}"))
            }
        },
        Message::External,
    )
}

fn effort_name(effort: &str) -> &'static str {
    match effort {
        "none" => "None",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra High",
        "max" => "Max",
        _ => unreachable!("catalog effort"),
    }
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
    if let Some(direction) = Direction::from_key_press(&event) {
        return Some(text_editor::Binding::Custom(Message::ComposerHistory(
            direction,
        )));
    }
    if questionnaire::word_delete(&event) {
        return Some(text_editor::Binding::Custom(Message::DeletePreviousWord));
    }
    if matches!(event.status, text_editor::Status::Focused { .. }) {
        if event.key == keyboard::Key::Named(keyboard::key::Named::Tab)
            && matches!(
                event.modifiers,
                keyboard::Modifiers::NONE | keyboard::Modifiers::SHIFT
            )
        {
            let edit = if event.modifiers.shift() {
                text_editor::Edit::Unindent
            } else {
                text_editor::Edit::Indent
            };
            return Some(text_editor::Binding::Custom(Message::Editor(
                text_editor::Action::Edit(edit),
            )));
        }
        if (event.modifiers.command() && event.key.as_ref() == keyboard::Key::Character("v"))
            || (event.modifiers.shift()
                && event.key == keyboard::Key::Named(keyboard::key::Named::Insert))
        {
            return Some(text_editor::Binding::Custom(Message::Paste));
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
mod tests;
