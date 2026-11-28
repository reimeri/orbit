mod browser;
mod capture;
mod changes;
mod composer;
pub(crate) mod find;
mod forks;
mod imports;
#[cfg(any(test, feature = "staged-capture"))]
mod marketing;
mod panels;
mod transcript;
use panels::RightPanel;
mod prompt_rail;
mod settings;
mod sidebar;
#[cfg(any(test, feature = "staged-capture"))]
mod smoke;
mod usage_panel;
mod worktrees;

use sidebar::{CollapsedList, CollapsedSection, SidebarRow};

use crate::activity::{self, Row as ActivityRow};
use crate::editor_history::{Direction, Draft as DraftKey, DraftHistories};
use crate::io_worker::Lane;
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
        ConversationStatus, FilingAction, FilingOutcome, Harness,
        subagents::{ChildStatus, Profile, RunInfo, RunKey},
    },
    auth::Login,
    code_mode::ToolStop,
    models::{MODELS, model},
    shell::CommandStatus,
    store::{
        Catalog, Entry, EntryKind, FilingKind, NewSessionDraft, Project, Session, SessionSummary,
        Settings, Store, UserMessage,
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
#[cfg(any(test, feature = "staged-capture"))]
use orbit_core::store::Filing;
use unicode_segmentation::UnicodeSegmentation;

// Measurement epochs reject stale sensor messages; they are not widget identity.
#[derive(Clone, PartialEq, Eq, Hash)]
enum TranscriptKey {
    ForkPoint,
    Compacting,
    Top,
    Entry {
        session: String,
        entry: String,
        role: usize,
    },
    Bottom,
}

#[derive(Default, PartialEq)]
struct Disclosures {
    groups: HashSet<String>,
    execution: HashSet<String>,
    expanded: HashSet<String>,
    raw_tool_inputs: HashSet<String>,
}

// Only keep geometry for the row being read, not an inactive transcript.
struct ChatPosition {
    row: usize,
    inside: f32,
    height: f32,
    message: Option<(String, crate::message_window::Heights)>,
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

const RAIL_WIDTH: f32 = 50.;
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
    images: HashMap<String, (usize, usize)>,
    window: Option<crate::message_window::Window>,
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
        crate::message_window::Window::sync(&mut self.window, self.content.items(), from, size);
    }

    fn refresh_images(&mut self) {
        self.images.clear();
        if !self.content.images().is_empty() {
            let items = self.content.items();
            let range = self.window.as_ref().map_or(0..items.len(), |w| w.visible());
            self.images = crate::markdown_images::urls(&items[range.clone()], range.start);
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
    current_status: ConversationStatus,
    draft_session: Option<Session>,
    new_draft: NewSessionDraft,
    new_draft_dirty: bool,
    saving_new_draft: bool,
    saving_session_draft: Option<String>,
    pruning_activity: bool,
    usage_summary: Option<Result<Arc<orbit_core::store::activity::Summary>, String>>,
    transcript: crate::virtual_list::List,
    activity: activity::Projection,
    details: Disclosures,
    parked_details: HashMap<String, Disclosures>,
    session_list_focus: crate::session_list_focus::SessionListFocus,
    sidebar_rows: Vec<SidebarRow>,
    child_statuses: HashMap<String, ChildStatus>,
    sidebar_list: crate::virtual_list::List,
    collapsed: [CollapsedList; 3],
    catalog_revision: u64,
    view_epoch: u64,
    sidebar_epoch: u64,
    sidebar_keys: HashMap<String, usize>,
    next_sidebar_key: usize,
    io: crate::io_worker::Worker,
    // One pending cleanup per body; the bit records a sweep requested since enqueue.
    evictions: HashMap<String, bool>,
    // Present until project deletion is acknowledged: newer snapshots must
    // incorporate its outcome instead of writing the old project list.
    catalog_save_after_delete: Option<bool>,
    tasks: Vec<Task<Message>>,
    io_busy: bool,
    switching_session: bool,
    sending_message: bool,
    send_after_import: Option<String>, // Session ID, including the provisional new-session ID.
    closing: bool,
    close_requested: bool,
    submissions: HashMap<String, (UserMessage, String)>,
    current_revision: u64,
    outputs: HashMap<String, tool_output::Output>,
    tool_previews: HashMap<String, (Arc<Entry>, String)>,
    historical_runs: HashMap<String, Option<RunInfo>>,
    historical_read_pending: bool,
    recent_previews: crate::recent_previews::RecentPreviews,
    image_view: crate::image_view::Viewer,
    markdown_images: crate::markdown_images::Cache,
    equations: crate::equations::Cache,
    api_key_draft: String,
    api_key_configured: bool,
    forks: forks::Index,
    right_panel: Option<RightPanel>,
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
    attachment_epoch: u64,
    imports: imports::Imports,
    questionnaires: HashMap<String, questionnaire::Draft>,
    active_questionnaire: Option<String>,
    chat_selection: chat_selection::Shared,
    markdown: HashMap<String, Markdown>,
    prompt_windows: HashMap<String, crate::message_window::Prompt>,
    message_heights: HashMap<String, crate::message_window::Heights>,
    browser: browser::State,
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
    project_request: u64,
    error: Option<String>,
    account: Option<String>,
    login_cancel: Option<CancellationToken>,
    login_url: Option<String>,
    tick: u64,
    footer_widths: std::cell::RefCell<lru::LruCache<String, f32>>,
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
    #[cfg(any(test, feature = "staged-capture"))]
    smoke_stage: usize,
    smoke_fixtures: SmokeFixtures,
    #[cfg(any(test, feature = "staged-capture"))]
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
    Spelling(spellcheck::Target, Option<Vec<String>>),
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
    Browser(browser::Action),
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
    SpellingMenu(spellcheck::Target, iced::Point),
    CorrectSpelling(spellcheck::Target, usize),
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
    RemoveAttachment(usize, u64),
    Deleted(String, Result<(), String>),
    Edited(Result<(), String>),
    Tick,
    Changed,
    GitTick,
    SaveDraft,
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
    ToggleCollapsed(CollapsedSection),
    CollapsedScroll(CollapsedSection, u64, f32, f32),
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
    QuestionResolved(String, Result<(), String>),
    ComposerEnter(String),
    Send(String),
    ToggleSettings,
    ToggleAgents,
    Changes(changes::Action),
    Worktree(worktrees::Action),
    StopAgent(String),
    StopTool {
        session: String,
        entry: String,
    },
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
    HideWorking(bool),
    AddProject,
    Browse,
    Folder(u64, Option<PathBuf>),
    ProjectResolved(u64, Result<PathBuf, String>),
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
    RowGeometry(
        u64,
        usize,
        Arc<Entry>,
        f32,
        Option<crate::message_window::Geometry>,
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
    Evicted(String, Result<(), String>),
    AgeTick,
    ActivityPruned(Result<(), String>),
    ErrorDismiss,
    ResizeStart(bool, iced::Point),
    Event(iced::Event),
    WindowOpened(window::Id),
    Screenshot(window::Screenshot),
    #[cfg(any(test, feature = "staged-capture"))]
    CheckScreenshot(window::Screenshot),
    #[cfg(any(test, feature = "staged-capture"))]
    SmokeOpenTool(String),
    #[cfg(any(test, feature = "staged-capture"))]
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
            current_status: ConversationStatus::Ready,
            draft_session: None,
            new_draft,
            new_draft_dirty: false,
            saving_new_draft: false,
            saving_session_draft: None,
            pruning_activity: false,
            usage_summary: None,
            transcript: crate::virtual_list::List::with_overscan(CHAT_OVERSCAN),
            activity: activity::Projection::default(),
            details: Default::default(),
            parked_details: Default::default(),
            session_list_focus: Default::default(),
            sidebar_rows: vec![],
            child_statuses: HashMap::new(),
            sidebar_list: crate::virtual_list::List::default(),
            collapsed: std::array::from_fn(|_| CollapsedList::new()),
            catalog_revision: u64::MAX,
            view_epoch: 0,
            sidebar_epoch: 0,
            sidebar_keys: HashMap::new(),
            next_sidebar_key: 3,
            io,
            evictions: HashMap::new(),
            catalog_save_after_delete: None,
            tasks: vec![],
            io_busy: false,
            switching_session: false,
            sending_message: false,
            send_after_import: None,
            closing: false,
            close_requested: false,
            submissions: HashMap::new(),
            attachments: vec![],
            attachment_epoch: 0,
            imports: Default::default(),
            current_revision: 0,
            outputs: HashMap::new(),
            tool_previews: Default::default(),
            historical_runs: HashMap::new(),
            historical_read_pending: false,
            recent_previews: Default::default(),
            image_view: Default::default(),
            markdown_images: Default::default(),
            equations: Default::default(),
            api_key_draft: String::new(),
            api_key_configured: false,
            forks: Default::default(),
            right_panel: None,
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
            message_heights: HashMap::new(),
            prompt_windows: HashMap::new(),
            browser: Default::default(),
            pending_settings: None,
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
            project_request: 0,
            error,
            account: None,
            login_cancel: None,
            login_url: None,
            tick: 0,
            footer_widths: std::cell::RefCell::new(lru::LruCache::new(32.try_into().unwrap())),
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
            #[cfg(any(test, feature = "staged-capture"))]
            smoke_stage: 0,
            smoke_fixtures: SmokeFixtures::default(),
            #[cfg(any(test, feature = "staged-capture"))]
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
            app.tasks.push(app.io.run_in([Lane::ApiKey], move || {
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
    // Only run after deletion: don't scan the library on a scroll or timer tick.
    fn prune_deleted_views(&mut self) {
        let ids: HashSet<_> = self.sessions.iter().map(|s| &s.id).collect();
        self.sidebar_keys.retain(|id, _| ids.contains(id));
        self.chat_positions.retain(|id, _| ids.contains(id));
        self.parked_details.retain(|id, _| ids.contains(id));
        self.submissions.retain(|id, _| {
            ids.contains(id) || self.draft_session.as_ref().is_some_and(|s| s.id == *id)
        });
        self.histories
            .retain(|key| !matches!(key, DraftKey::Session(id) if !ids.contains(id)));
        let paths: HashSet<_> = self
            .sessions
            .iter()
            .map(|s| &s.project)
            .chain(self.catalog.projects.iter().map(|p| &p.path))
            .chain(self.new_draft.project.iter())
            .collect();
        self.sidebar_branches.retain(|p, _| paths.contains(p));
        self.git.retain(|p, _| paths.contains(p));
        self.git_pending.retain(|p| paths.contains(p));
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
        let detailed = self.right_panel == Some(RightPanel::Changes);
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
        let previous: HashSet<_> = self
            .questionnaires
            .iter()
            .filter(|(_, d)| d.async_owner.is_none())
            .map(|(id, _)| id.clone())
            .collect();
        let mut active = self.smoke_fixtures.questionnaire.clone();
        let mut live: HashSet<_> = active.iter().cloned().collect();
        for c in h.all() {
            if let Some(questions) = &c.questionnaires {
                let selected = self.selected.as_ref() == Some(&c.id);
                for pending in questions.pending() {
                    live.insert(pending.id.clone());
                    if selected && active.is_none() {
                        active = Some(pending.id.clone());
                    }
                    self.questionnaires
                        .entry(pending.id.clone())
                        .or_insert_with(|| questionnaire::Draft::new(pending, None));
                }
                if selected
                    && active.is_none()
                    && let Some(pending) = c
                        .state
                        .lock()
                        .expect("session lock")
                        .async_questions
                        .first()
                        .cloned()
                {
                    active = Some(pending.id.clone());
                    self.questionnaires
                        .entry(pending.id.clone())
                        .or_insert_with(|| questionnaire::Draft::new(pending, Some(c.id.clone())));
                }
            }
        }
        self.questionnaires
            .retain(|id, d| d.async_owner.is_some() || live.contains(id));
        if active != self.active_questionnaire {
            if let Some(previous) = self
                .active_questionnaire
                .as_ref()
                .and_then(|id| self.questionnaires.get_mut(id))
            {
                previous.separate_edits();
            }
            if let Some(id) = &active
                && self.right_panel != Some(RightPanel::Settings)
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
                let owners: HashSet<_> = self
                    .sessions
                    .iter()
                    .filter(|s| s.has_async_question)
                    .map(|s| &s.id)
                    .collect();
                self.questionnaires.retain(|_, draft| {
                    draft
                        .async_owner
                        .as_ref()
                        .is_none_or(|owner| owners.contains(owner))
                });
                self.sessions
                    .sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
                self.forks = forks::Index::build(&self.sessions);
                self.prepare_sidebar();
            }
        }
        // Keep the outgoing view mounted until Selected commits the destination.
        if self.switching_session || self.smoke_fixtures.transcript {
            return false;
        }
        let h = self.harness.as_ref().expect("harness");
        let Some(conversation) = self.selected.as_ref().and_then(|id| h.get(id)) else {
            if self.current_session.take().is_some() {
                self.details = Default::default();
                self.equations.leave_session();
                self.current_status = ConversationStatus::Ready;
                self.find = None;
                self.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
                self.activity = activity::Projection::default();
                self.view_epoch = self.view_epoch.wrapping_add(1);
            }
            self.outputs.clear();
            self.historical_runs.clear();
            self.markdown.clear();
            self.prompt_windows.clear();
            self.message_heights.clear();
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
            self.equations.leave_session();
            self.hold_prompt_at_bottom = false;
            self.outputs.clear();
            self.historical_runs.clear();
            self.markdown.clear();
            self.prompt_windows.clear();
            self.message_heights.clear();
            self.chat_selection.borrow_mut().clear();
            self.view_epoch = self.view_epoch.wrapping_add(1);
            // Size the first inner text window for the actual pane, not the
            // default viewport used before the first redraw.
            let viewport = self.transcript.viewport;
            self.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
            self.transcript.viewport = viewport;
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
            if index < old.len()
                && matches!(entry.kind, EntryKind::User { .. })
                && old[index].text != entry.text
            {
                self.prompt_windows.remove(&entry.id);
                self.message_heights.remove(&entry.id);
            }
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
                        &self.transcript,
                        pos,
                        self.auto_scroll,
                    );
                }
                markdown.refresh_images();
            }
        }
        let bookmark = self.chat_positions.remove(&snapshot.id);
        self.current_status = conversation.status();
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
            if let Some((id, heights)) = bookmark.message {
                self.message_heights.insert(id, heights);
            }
            self.auto_scroll = false;
        } else if self.auto_scroll {
            self.transcript.end();
        }
        self.prepare_visible();
        true
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
            self.tasks
                .push(self.io.run_in([Lane::Family(h.family_id(&id))], move || {
                    Message::SessionSettingsEdited(h.edit(&id, edit).map_err(|e| format!("{e:#}")))
                }));
        }
    }
    fn save_catalog(&mut self) {
        if self.closing {
            return;
        }
        if let Some(deferred) = &mut self.catalog_save_after_delete {
            *deferred = true;
            return;
        }
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
    fn remember_chat_position(&mut self) {
        if self.auto_scroll {
            return;
        }
        let pos = self.transcript.at(self.transcript.offset);
        if let Some(&row) = self.transcript.order.get(pos) {
            let session = self.current_session.as_ref().expect("mounted transcript");
            let message = match ActivityRow::from_id(row) {
                ActivityRow::Entry(i) => {
                    let id = &session.entries[i].id;
                    self.markdown
                        .get_mut(id)
                        .and_then(|md| md.window.as_mut())
                        .or_else(|| self.prompt_windows.get_mut(id).map(|p| &mut p.window))
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
                    message,
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
        // A queued autosave can still fail. Keep the current draft until selection
        // succeeds and flush it here if its last save hasn't been acknowledged.
        let draft = self
            .selected
            .as_ref()
            .filter(|id| self.draft_dirty || self.saving_session_draft.as_ref() == Some(id))
            .map(|id| (id.clone(), self.composer_message()));
        let settings = self.catalog.settings.clone();
        let project = match &selection {
            Selection::Picker => None,
            Selection::Draft(path) => Some(path),
            Selection::Session(_) => self.new_draft.project.as_ref(),
        };
        let new_draft = (self.new_draft_dirty
            || self.saving_new_draft
            || project != self.new_draft.project.as_ref())
        .then(|| NewSessionDraft {
            project: project.cloned(),
            ..self.new_draft.clone()
        });
        if let Some(h) = &self.harness {
            let h = h.clone();
            self.io_busy = true;
            self.switching_session = true;
            let mut timer =
                std::env::var_os("ORBIT_TRACE_SESSION_SWITCH").map(|_| std::time::Instant::now());
            let mut lanes = Vec::new();
            if new_draft.is_some() || !matches!(selection, Selection::Session(_)) {
                lanes.push(Lane::NewDraft);
            }
            if let Some((id, _)) = &draft {
                lanes.push(Lane::Family(h.family_id(id)));
            }
            if let Selection::Session(id) = &selection {
                lanes.push(Lane::Family(h.family_id(id)));
            }
            self.tasks.push(self.io.run_in(lanes, move || {
                let mut mark = |phase: &str| {
                    if let Some(last) = &mut timer {
                        let now = std::time::Instant::now();
                        eprintln!("orbit session switch: {phase}: {:?}", now - *last);
                        *last = now;
                    }
                };
                mark("storage queue");
                Message::Selected(
                    (|| {
                        if let Some((old, draft)) = draft {
                            h.edit(&old, |s| { s.draft = draft.text; s.draft_images = draft.images; })?;
                        }
                        mark("outgoing draft");
                        let id = if let Selection::Session(id) = &selection {
                            if fork_entry.is_none() {
                                let conversation = h.load(id)?;
                                mark("load");
                                if conversation.state.lock().expect("session lock").unchecked_completion {
                                    h.edit(id, |s| s.unchecked_completion = false)?;
                                }
                                mark("completion acknowledgement");
                            }
                            Some(id.as_str())
                        } else {
                            None
                        };
                        if let Some(draft) = new_draft {
                            h.store.save_new_draft(&draft)?;
                        }
                        mark("shared draft");
                        if let (Some(source), Some(entry)) = (id, fork_entry) {
                            // Create last: a failed switch must never leave a hidden duplicate fork.
                            let fork = h.fork(source, &entry, &settings)?;
                            return Ok((Selection::Session(fork), None));
                        }
                        // Reconcile last so a later selection error cannot suppress the notice after recovery.
                        let replaced = match id {
                            Some(id) => h.reconcile_model(id, &settings)?,
                            None => false,
                        };
                        mark("model reconciliation");
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
    fn release_inactive_sessions(&mut self) {
        if let Some(h) = self.harness.clone() {
            for c in h.all() {
                self.release_idle_session(&h, &c);
            }
        }
    }
    fn release_idle_session(&mut self, h: &Arc<Harness>, c: &orbit_core::agent::Conversation) {
        if self.selected.as_ref() == Some(&c.id) || !c.idle_body() {
            return;
        }
        if let Some(recheck) = self.evictions.get_mut(&c.id) {
            *recheck = true;
            return;
        }
        self.evictions.insert(c.id.clone(), false);
        let h = h.clone();
        let id = c.id.clone();
        self.tasks
            .push(self.io.run_in([Lane::Family(h.family_id(&id))], move || {
                let result = h
                    .evict_idle(&id)
                    .map_err(|e| format!("Could not release inactive session: {e:#}"));
                Message::Evicted(id, result)
            }));
    }
    fn acknowledge_focused_completion(&mut self) {
        if self.window_focused
            && !self.closing
            && let (Some(h), Some(session)) = (&self.harness, &self.current_session)
            && session.unchecked_completion
        {
            let h = h.clone();
            let id = session.id.clone();
            self.tasks.push(self.io.run_in([Lane::Family(h.family_id(&id))], move || {
                Message::Edited(h.edit(&id, |s| s.unchecked_completion = false).map_err(|e| {
                    format!("Could not save completion acknowledgement: {e:#}. Keep Orbit open, fix storage, then retry saving or revisit the session.")
                }))
            }));
        }
    }
    fn stop_agent(&mut self, id: String) {
        if let Some(h) = self.harness.clone() {
            // Queue behind earlier actions on this family, never an unrelated filing wait.
            self.tasks
                .push(self.io.run_in([Lane::Family(h.family_id(&id))], move || {
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
            self.tasks.push(self.io.run_in([Lane::Family(h.family_id(id))], move || {
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
        if let Message::Browser(action @ browser::Action::Data(..)) = message {
            return self.browser_action(action);
        }
        let apply_timer = (matches!(message, Message::Selected(_))
            && std::env::var_os("ORBIT_TRACE_SESSION_SWITCH").is_some())
        .then(std::time::Instant::now);
        let geometry = (
            self.view_epoch,
            self.transcript.offset,
            self.transcript.viewport,
            self.transcript.total(),
        );
        let task = self.update_inner(message);
        self.sync_browser();
        self.advance_imports();
        if let Some(RightPanel::Forks(panel)) = &self.right_panel
            && self.fork_family() != Some(panel.family.as_str())
        {
            self.set_right_panel(None);
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
        if let Some(ContextMenu(MenuTarget::Spelling(target, _), _)) = &self.context_menu
            && (!self.spellcheck.still_marked(target)
                || self.io_busy
                || self.modal.is_some()
                || self.active_questionnaire.is_some())
        {
            self.context_menu = None;
        }
        if self.modal.is_some() || self.context_menu.is_some() {
            self.browser.menu = None;
        }
        if self.modal.is_some()
            || self.context_menu.is_some()
            || self.active_questionnaire.is_some()
        {
            self.histories.active.separate();
            self.footer_menu_open = false;
            self.worktrees.popup = None;
        }
        if let Some(owner) = &self.send_after_import {
            if !self.composer_available() || self.current().is_none_or(|s| &s.id != owner) {
                self.cancel_import_send(
                    "Pending send cancelled. Review the draft and press Enter to send.",
                );
            } else if !self.composer_importing() {
                // Dispatch before another edit or selection can overtake the import.
                let owner = self.send_after_import.take().expect("waiting send");
                self.send(owner);
            }
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
        if let Some(start) = apply_timer {
            eprintln!("orbit session switch: UI update: {:?}", start.elapsed());
        }
        Task::batch([task, close].into_iter().chain(self.tasks.drain(..)))
    }
    fn update_inner(&mut self, message: Message) -> Task<Message> {
        if self.closing
            && matches!(
                message,
                Message::Event(_)
                    | Message::SaveApiKey
                    | Message::RemoveApiKey
                    | Message::StopAgent(_)
                    | Message::StopTool { .. }
                    | Message::FileSession(..)
                    | Message::Worktree(_)
            )
        {
            return Task::none();
        }
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
                    | Message::ComposerEnter(_)
                    | Message::Send(_)
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
                    | Message::HideWorking(_)
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
                    | Message::HideWorking(_)
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
                    | Message::ComposerEnter(_)
                    | Message::Send(_)
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
            Message::RemoveAttachment(index, epoch) => {
                if !self.composer_available()
                    || epoch != self.attachment_epoch
                    || index >= self.attachments.len()
                {
                    return Task::none();
                }
                self.cancel_import_send("Pending send cancelled because the attachments changed.");
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
            Message::SpellingMenu(target, position) => {
                if self.composer_available()
                    && !self.imports.holds_paste_cursor()
                    && self.spellcheck.still_marked(&target)
                {
                    let task = self.spellcheck.suggest(target.clone(), &self.editor);
                    self.context_menu =
                        Some(ContextMenu(MenuTarget::Spelling(target, None), position));
                    return task.map(Message::Spellcheck);
                }
            }
            Message::Spellcheck(spellcheck::Message::Suggested(target, result)) => {
                if let Some(ContextMenu(MenuTarget::Spelling(active, choices), _)) =
                    &mut self.context_menu
                    && *active == target
                {
                    match result {
                        Ok(words) => *choices = Some(words),
                        Err(error) => {
                            self.context_menu = None;
                            self.error = Some(error);
                        }
                    }
                }
            }
            Message::CorrectSpelling(target, index) => {
                if let Some(ContextMenu(MenuTarget::Spelling(active, Some(choices)), _)) =
                    &self.context_menu
                    && *active == target
                {
                    let replacement = Arc::new(choices[index].clone());
                    self.context_menu = None;
                    if self.composer_available() && !self.imports.holds_paste_cursor() {
                        let before = self
                            .histories
                            .active
                            .transaction(&mut self.editor, |editor| {
                                target.mark.replace(editor, replacement)
                            });
                        self.file_completion.edited(&self.editor);
                        self.composer_edited(before, false);
                        return widget::operation::focus("composer");
                    }
                }
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
                if self.right_panel == Some(RightPanel::Settings) {
                    self.set_right_panel(None);
                } else {
                    self.discard_settings_edits();
                }
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
                    self.tasks.push(self.io.run_in([Lane::ApiKey], move || {
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
                            if self.right_panel == Some(RightPanel::Worktrees) {
                                self.set_right_panel(None);
                            }
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
                        // Cleanup may wait for other turns. Queue it only once the
                        // destination is ready; failed loads keep the old chat resident.
                        self.release_inactive_sessions();
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
                            let outgoing = std::mem::take(&mut self.details);
                            if outgoing != Disclosures::default()
                                && let Some(previous) = &self.current_session
                            {
                                self.parked_details.insert(previous.id.clone(), outgoing);
                            }
                            if let Some(saved) = self
                                .selected
                                .as_ref()
                                .and_then(|id| self.parked_details.remove(id))
                            {
                                self.details = saved;
                            }
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
                        if self.right_panel == Some(RightPanel::Worktrees) {
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
                            if self.right_panel != Some(RightPanel::Worktrees) {
                                let task = self.worktree_action(worktrees::Action::Panel);
                                self.tasks.push(task);
                            }
                            self.worktrees.bases = false;
                        }
                        self.error = Some(error);
                    }
                }
            }
            Message::NewDraftSaved(result) => {
                self.saving_new_draft = false;
                if let Err(error) = result {
                    self.error = Some(error);
                    self.new_draft_dirty = true;
                }
            }
            Message::DraftSaved(id, result) => {
                self.saving_session_draft = None;
                if let Err(error) = result {
                    self.error = Some(error);
                    if self.selected.as_ref() == Some(&id) {
                        self.draft_dirty = true;
                    }
                }
            }
            Message::Sent(id, text, result) => {
                self.sending_message = false;
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
                let deleted = result.is_ok()
                    || self
                        .harness
                        .as_ref()
                        .is_some_and(|h| !h.summaries().iter().any(|s| s.id == id));
                if deleted {
                    let selected = self.parent_id() == Some(id.as_str());
                    if selected {
                        self.draft_dirty = false;
                        self.selected = None;
                    }
                    self.refresh();
                    self.prune_deleted_views();
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
                if before != self.catalog_revision && !self.io_busy && !self.closing {
                    self.release_inactive_sessions();
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
                self.prune_deleted_views();
                if selected_deleted {
                    self.restore_draft();
                }
                if self.catalog_save_after_delete.take() == Some(true) {
                    self.save_catalog();
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
            Message::ToggleCollapsed(kind) => {
                let section = &mut self.collapsed[kind as usize];
                if self.search.is_empty() {
                    section.open = !section.open;
                } else {
                    section.search_open = !section.search_open;
                }
                section.list.offset = 0.;
            }
            Message::CollapsedScroll(kind, epoch, offset, height) => {
                if epoch == self.sidebar_epoch {
                    let list = &mut self.collapsed[kind as usize].list;
                    list.offset = offset;
                    list.viewport = height;
                }
            }
            Message::Create(path) => self.select(Selection::Draft(path)),
            Message::Search(search) => {
                self.search = search;
                for section in &mut self.collapsed {
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
                    if let Some(resolution) = update.resolution {
                        if draft.async_owner.is_some() {
                            let h = self.harness.clone().expect("questionnaire harness");
                            let session = self.selected.clone().expect("questionnaire session");
                            let settings = self.catalog.settings.clone();
                            self.io_busy = true;
                            return self.io.run_in(
                                [Lane::Family(h.family_id(&session))],
                                move || {
                                    let result = match resolution {
                                        questionnaire::Resolution::Answer(answers) => {
                                            h.answer_question(&session, &id, answers, settings)
                                        }
                                        questionnaire::Resolution::Dismiss => {
                                            h.dismiss_question(&session, &id)
                                        }
                                    };
                                    Message::QuestionResolved(
                                        id,
                                        result.map_err(|e| format!("{e:#}")),
                                    )
                                },
                            );
                        }
                        let questionnaire::Resolution::Answer(answers) = resolution else {
                            unreachable!("blocking questionnaires cannot be dismissed")
                        };
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
            Message::QuestionResolved(id, result) => {
                self.io_busy = false;
                match result {
                    Ok(()) => {
                        self.questionnaires.remove(&id);
                    }
                    Err(error) => {
                        if let Some(draft) = self.questionnaires.get_mut(&id) {
                            draft.error = Some(format!(
                                "{error}. Check storage or session status and retry."
                            ));
                        } else {
                            self.error = Some(error);
                        }
                    }
                }
                self.refresh();
            }
            Message::FileCompletion(crate::file_completion::Message::PendingEnter) => {
                self.notice = Some(("Not sent: file search is still running. Press Enter again when results are ready.".into(), std::time::Instant::now()));
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
                let accepting = matches!(message, crate::file_completion::Message::Accept { .. });
                let edited = if accepting {
                    self.histories
                        .active
                        .transaction(&mut self.editor, |editor| {
                            self.file_completion.update(message, editor)
                        })
                } else {
                    self.file_completion.update(message, &mut self.editor)
                };
                if edited {
                    self.cancel_import_send("Pending send cancelled because the draft changed.");
                    self.composer_edited(before, false);
                } else if accepting {
                    self.notice = Some((
                        "Suggestion changed. Review the draft and try again.".into(),
                        std::time::Instant::now(),
                    ));
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
                if matches!(
                    message,
                    Message::Editor(text_editor::Action::Edit(_))
                        | Message::DeletePreviousWord
                        | Message::ComposerHistory(_)
                ) {
                    let reason = if self.imports.holds_paste_cursor() {
                        "Pending send cancelled. The paste is still in progress; retry your edit when it finishes."
                    } else {
                        "Pending send cancelled because you edited the draft."
                    };
                    self.cancel_import_send(reason);
                }
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
            Message::ComposerEnter(owner) => {
                // Widget events may be batched: resolve the intent against the
                // draft *after* processing earlier edits, not against the last view.
                if self.current().is_some_and(|s| s.id == owner)
                    && self.composer_available()
                    && let Some(action) = self.file_completion.enter()
                {
                    if self.imports.holds_paste_cursor()
                        && !matches!(action, crate::file_completion::Message::PendingEnter)
                    {
                        self.notice = Some((
                            "Not sent: wait for the paste to finish before accepting a suggestion."
                                .into(),
                            std::time::Instant::now(),
                        ));
                    } else {
                        return self.update_inner(Message::FileCompletion(action));
                    }
                } else {
                    self.send(owner);
                }
            }
            Message::Send(owner) => self.send(owner),
            Message::StopAgent(id) => self.stop_agent(id),
            Message::StopTool { session, entry } => {
                if let Some(conversation) = self.harness.as_ref().and_then(|h| h.get(&session)) {
                    conversation.stop_tool(&entry);
                }
            }
            Message::Worktree(action) => return self.worktree_action(action),
            Message::Changes(action) => return self.changes_action(action),
            Message::Browser(a) => return self.browser_action(a),
            Message::ToggleAgents => self.toggle_right_panel(RightPanel::Agents),
            Message::ToggleSettings => {
                self.toggle_right_panel(RightPanel::Settings);
                if self.right_panel == Some(RightPanel::Settings)
                    && self.settings_tab == SettingsTab::General
                {
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
                        close_on_success: self.right_panel == Some(RightPanel::Settings),
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
                    SettingsSave::Saved
                        if pending.close_on_success
                            && self.right_panel == Some(RightPanel::Settings) =>
                    {
                        self.set_right_panel(None);
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
            Message::HideWorking(value) => {
                self.settings.hide_working_sessions = value;
                self.catalog.settings.hide_working_sessions = value;
                self.collapsed[CollapsedSection::Working as usize].open = false;
                self.prepare_sidebar();
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
                self.project_request += 1;
                self.modal_input = std::env::var("HOME").unwrap_or_else(|_| "/".into());
                self.modal = Some(Modal::AddProject);
                return widget::operation::focus("modal-input");
            }
            Message::Browse => {
                self.project_request += 1;
                let request = self.project_request;
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Add project to Orbit")
                            .pick_folder()
                            .await
                            .map(|f| f.path().to_owned())
                    },
                    move |path| Message::Folder(request, path),
                );
            }
            Message::Folder(request, Some(path))
                if request == self.project_request
                    && matches!(self.modal, Some(Modal::AddProject)) =>
            {
                self.modal_input = path.display().to_string()
            }
            Message::Folder(..) => {}
            Message::ProjectResolved(request, result) => {
                if !self.closing
                    && !self.close_requested
                    && request == self.project_request
                    && matches!(self.modal, Some(Modal::AddProject))
                {
                    match result {
                        Ok(path) => {
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
                        Err(error) => self.error = Some(error),
                    }
                }
            }
            Message::ModalInput(value) => {
                self.project_request += 1;
                self.modal_input = value;
            }
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
                if !self.details.raw_tool_inputs.remove(&id) {
                    self.details.raw_tool_inputs.insert(id.clone());
                }
                if let Some(view) = self.outputs.get_mut(&id) {
                    view.set_raw(self.details.raw_tool_inputs.contains(&id));
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
                if self.details.expanded.remove(&id) {
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
                                self.details.raw_tool_inputs.contains(&id),
                            ),
                        );
                    }
                    self.details.expanded.insert(id.clone());
                }
                if let Some(session) = &self.current_session
                    && let Some(index) = session.entries.index_of(&id)
                {
                    let head = self.activity.block(index).range.start;
                    self.details.groups.insert(session.entries[head].id.clone());
                }
                self.rebuild_transcript();
                self.prepare_visible();
            }
            Message::ToggleExecution(id) => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                if !self.details.execution.remove(&id) {
                    self.details.execution.insert(id);
                }
                self.rebuild_transcript();
                self.prepare_visible();
            }
            Message::ToggleGroup(id) => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                if !self.details.groups.remove(&id) {
                    self.details.groups.insert(id);
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
            Message::RowGeometry(epoch, index, source, height, geometry) => {
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
                } else if let Some(prompt) = self.prompt_windows.get_mut(&source.id) {
                    geometry.map(|g| prompt.window.update(g))
                } else {
                    if let Some(g) = geometry {
                        g.save_heights(self.message_heights.entry(source.id.clone()).or_default());
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
                    let order = &self.transcript.order;
                    let lo = order
                        .partition_point(|&id| ActivityRow::from_id(id).source() < start.entry);
                    let hi =
                        order.partition_point(|&id| ActivityRow::from_id(id).source() <= end.entry);
                    let entries = order[lo..hi]
                        .iter()
                        .filter_map(|&id| match ActivityRow::from_id(id) {
                            ActivityRow::Entry(i) => Some(i),
                            _ => None,
                        })
                        .map(|index| {
                            let entry = &session.entries[index];
                            let blocks = match &entry.kind {
                                EntryKind::Tool {
                                    name,
                                    input,
                                    output,
                                    parent,
                                    ..
                                } if self.details.expanded.contains(&entry.id) => {
                                    vec![
                                        chat_selection::Block {
                                            text: tool_output::display(
                                                name,
                                                input,
                                                output,
                                                self.details.raw_tool_inputs.contains(&entry.id),
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
                                        && !self.details.expanded.contains(&entry.id) =>
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
                                    let parsed;
                                    let content = if let Some(md) = self.markdown.get(&entry.id) {
                                        &md.content
                                    } else {
                                        parsed = markdown::Content::parse(&entry.text);
                                        &parsed
                                    };
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
            Message::LoginStarted(_)
                if self.login_cancel.as_ref().is_some_and(|c| c.is_cancelled()) =>
            {
                self.login_cancel = None;
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
                if let Some(branch) = branch
                    && self.sessions.iter().any(|s| s.project == path)
                {
                    self.sidebar_branches.insert(path, branch);
                } else {
                    self.sidebar_branches.remove(&path);
                }
            }
            Message::Git(path, info) => {
                let needs_details = self.right_panel == Some(RightPanel::Changes)
                    && self.current().is_some_and(|s| s.project == path);
                self.store_git(path, info);
                if needs_details {
                    return self.refresh_git();
                }
            }
            Message::AgeTick => {
                if !self.closing
                    && !self.pruning_activity
                    && let Some(h) = &self.harness
                {
                    self.pruning_activity = true;
                    let store = h.store.clone();
                    return self.io.run_in([Lane::Maintenance], move || {
                        Message::ActivityPruned(
                            store.prune_activity().map_err(|e| {
                                format!("Could not expire activity measurements: {e:#}")
                            }),
                        )
                    });
                }
            }
            Message::ActivityPruned(result) => {
                self.pruning_activity = false;
                self.fail(result);
            }
            Message::Evicted(id, result) => {
                let recheck = self.evictions.remove(&id) == Some(true);
                // It may have been reloaded and left again before this acknowledgement.
                if recheck
                    && result.is_ok()
                    && !self.io_busy
                    && !self.closing
                    && let Some(h) = self.harness.clone()
                    && let Some(c) = h.get(&id)
                {
                    self.release_idle_session(&h, &c);
                }
                self.fail(result);
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
                iced::Event::Window(window::Event::Rescaled(s)) => {
                    return self.browser_action(browser::Action::Scale(s));
                }
                iced::Event::Window(window::Event::Focused) => {
                    self.window_focused = true;
                    self.acknowledge_focused_completion();
                }
                iced::Event::Window(window::Event::Unfocused) => {
                    self.window_focused = false;
                    if self.drag.take().is_some() {
                        self.save_catalog();
                    }
                    self.browser.menu = None;
                    if matches!(
                        self.context_menu,
                        Some(ContextMenu(MenuTarget::Spelling(..), _))
                    ) {
                        self.context_menu = None;
                    }
                    self.cancel_import_send(
                        "Pending send cancelled because the window lost focus.",
                    );
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
                        let minimum: f32 = if right { 260. } else { 160. };
                        let value = (width + if right { -delta } else { delta })
                            .clamp(minimum, limit.max(minimum));
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
                    if self.browser.menu.is_some() {
                        if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                            self.browser.menu = None;
                        }
                        return Task::none();
                    }
                    if self.browser.page_focused()
                        && self.modal.is_none()
                        && self.context_menu.is_none()
                    {
                        return Task::none();
                    }
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
                        } else if self.browser.find_open() {
                            return self.browser_action(browser::Action::EndFind);
                        } else if let Some(close_search) = close_search {
                            return self.update_inner(close_search);
                        } else if self.right_panel == Some(RightPanel::Worktrees) {
                            let action = if self.worktrees.bases {
                                worktrees::Action::Bases
                            } else if self.worktrees.confirm.is_some() {
                                worktrees::Action::CancelRemove
                            } else {
                                worktrees::Action::Panel
                            };
                            return self.worktree_action(action);
                        } else if self.right_panel.is_some() {
                            self.set_right_panel(None);
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
                return Task::batch([
                    self.start_capture(id),
                    window::scale_factor(id).map(|s| Message::Browser(browser::Action::Scale(s))),
                ]);
            }
            #[cfg(any(test, feature = "staged-capture"))]
            Message::CheckScreenshot(screenshot) => return self.check_screenshot(screenshot),
            #[cfg(any(test, feature = "staged-capture"))]
            Message::SmokeOpenTool(session_id) => return self.smoke_open_tool(&session_id),
            #[cfg(any(test, feature = "staged-capture"))]
            Message::Capture => return self.capture_frame(),
            Message::Screenshot(screenshot) => return self.smoke_frame(screenshot),
        }
        Task::none()
    }
    fn change_filing(&mut self, id: String, action: FilingAction) {
        if let Some(h) = self.harness.clone() {
            self.tasks
                .push(self.io.run_in([Lane::Family(h.family_id(&id))], move || {
                    match h.file_session(&id, action) {
                        Ok(FilingOutcome::RunningCommands(kind)) => {
                            Message::RunningCommandsAtFile(id, kind)
                        }
                        Ok(FilingOutcome::Applied) => Message::Edited(Ok(())),
                        Err(error) => Message::Edited(Err(format!("{error:#}"))),
                    }
                }));
        }
    }
    fn confirm(&mut self) -> Task<Message> {
        if self.io_busy || self.closing {
            return Task::none();
        }
        match self.modal.clone() {
            Some(Modal::AddProject) => {
                self.project_request += 1;
                let request = self.project_request;
                let path = self.modal_input.trim().to_owned();
                return Task::perform(
                    async move {
                        let path = tokio::fs::canonicalize(path).await.map_err(|e| {
                            format!(
                                "Cannot open that project: {e}. Check the path or choose Browse."
                            )
                        })?;
                        if !tokio::fs::metadata(&path)
                            .await
                            .map_err(|e| format!("Cannot inspect project: {e}"))?
                            .is_dir()
                        {
                            return Err("Select a directory, not a file.".into());
                        }
                        Ok(path)
                    },
                    move |r| Message::ProjectResolved(request, r),
                );
            }
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
                    self.catalog_save_after_delete = Some(false);
                    self.tasks.push(self.io.barrier_catalog(move |writer| {
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
                    self.tasks
                        .push(self.io.run_in([Lane::Family(h.family_id(&id))], move || {
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
                    self.tasks
                        .push(self.io.run_in([Lane::Family(h.family_id(&id))], move || {
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
            return self.io.barrier_catalog(move |writer| {
                // Ordered after earlier jobs: no queued Send can start behind this barrier.
                h.stop_all();
                let result=(|| {
                    let deadline = std::time::Instant::now() + Duration::from_secs(20);
                    while h.all().iter().any(|s| s.is_executing()) {
                        anyhow::ensure!(std::time::Instant::now() < deadline, "Agent work has not stopped yet; wait for it to finish and retry closing");
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    if let Some((id,draft))=draft {h.edit(&id,|s| { s.draft=draft.text; s.draft_images=draft.images; })?;}
                    h.store.save_new_draft(&new_draft)?;
                    writer.save_state(&h.store, catalog)?;
                    h.flush()?;
                    h.browser.shutdown()?;
                    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(h.auth.flush())
                })();
                Message::Exit(result.map_err(|e|format!("Could not finish saving or closing: {e:#}. Orbit remains open; resolve the issue and retry.")))
            });
        }
        iced::exit()
    }
    pub fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = vec![
            window_events(),
            window::open_events().map(Message::WindowOpened),
        ];
        let mounted_rows = &self.sidebar_rows[self.sidebar_list.visible()];
        if let Some(harness) = &self.harness {
            subscriptions.push(Subscription::run_with(Updates(harness.clone()), updates));
            if let Some(id) = &self.browser.mounted {
                subscriptions.push(Subscription::run_with(
                    (Updates(harness.clone()), id.clone()),
                    browser::subscription,
                ));
            }
            subscriptions
                .push(iced::time::every(Duration::from_secs(60)).map(|_| Message::AgeTick));
            if self.current().is_none() {
                subscriptions.push(Subscription::run_with(
                    Updates(harness.clone()),
                    usage_panel::summaries,
                ));
            }
            // Only the selected turn and mounted Working rows use the activity clock.
            if self.running()
                || mounted_rows
                    .iter()
                    .any(|row| matches!(row, SidebarRow::Session(_, SessionStatus::Working)))
                || self.collapsed_list_visible(CollapsedSection::Working)
            {
                subscriptions.push(iced::time::every(ACTIVITY_INTERVAL).map(|_| Message::Tick));
            }
        }
        // Poll each mounted checkout once, independent of which session is selected.
        let mut checkouts = HashSet::new();
        for row in mounted_rows {
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
            self.footer_menu_open
                || self.browser.menu.is_some()
                || self.worktrees.popup.is_some()
                || matches!(
                    self.context_menu,
                    Some(ContextMenu(MenuTarget::Spelling(..), _))
                ),
            self.chat_selection.clone(),
        )
    }
    fn content(&self) -> Element<'_, Message> {
        let colors = &self.preset().colors;
        #[cfg(any(test, feature = "staged-capture"))]
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
                        container(Icon::CircleAlert.colored(16., colors.danger)).center_y(24),
                        container(
                            text(error)
                                .size(14)
                                .line_height(text::LineHeight::Absolute(20.into()))
                                .wrapping(text::Wrapping::WordOrGlyph)
                        )
                        .padding([2, 0])
                        .width(Fill),
                        icon_tip(
                            theme::button(Icon::X.view(16.))
                                .padding(4)
                                .style(theme::text_button)
                                .on_press(Message::ErrorDismiss),
                            "Dismiss error"
                        )
                    ]
                    .spacing(8),
                )
                .padding([8, 12])
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
        if let Some(panel) = &self.right_panel {
            body = body.push(self.divider(true)).push(match panel {
                RightPanel::Browser => self.browser_panel(),
                RightPanel::Settings => self.settings_panel(),
                RightPanel::Changes => self.changes_panel(),
                RightPanel::Worktrees => self.worktrees_panel(),
                RightPanel::Agents => self.agents_panel(),
                RightPanel::Forks(panel) => self.forks_panel(panel),
            });
        }
        let mut controls = column![space().height(Fill)];
        if self.fork_family().is_some() {
            controls = controls.push(rail_button(
                Icon::GitFork.view(22.),
                "Forks",
                matches!(self.right_panel, Some(RightPanel::Forks(_))),
                Message::ShowForks(None),
            ));
        }
        if self.current_session.is_some() {
            controls = controls.push(rail_button(
                Icon::Globe.view(22.),
                "Browser",
                self.right_panel == Some(RightPanel::Browser),
                Message::Browser(browser::Action::Toggle),
            ));
        }
        let rail = container(
            controls
                .push(rail_button(
                    Icon::Diff.view(22.),
                    "Changes",
                    self.right_panel == Some(RightPanel::Changes),
                    Message::Changes(changes::Action::Toggle),
                ))
                .push(rail_button(
                    Icon::FolderOpen.view(22.),
                    "Worktrees",
                    self.right_panel == Some(RightPanel::Worktrees),
                    Message::Worktree(worktrees::Action::Panel),
                ))
                .push(rail_button(
                    crate::agent_icon::agent_icon(self.agent_count() > 0),
                    "Agents",
                    self.right_panel == Some(RightPanel::Agents),
                    Message::ToggleAgents,
                ))
                .push(rail_button(
                    Icon::Settings.view(24.),
                    "Settings",
                    self.right_panel == Some(RightPanel::Settings),
                    Message::ToggleSettings,
                ))
                .spacing(12)
                .width(Fill),
        )
        .padding([14, 8])
        .height(Fill)
        .width(RAIL_WIDTH)
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
            const VISIBLE_SUGGESTIONS: usize = 5;
            const SUGGESTION_HEIGHT: f32 = 28.;
            const ROW_GAP: f32 = 4.;
            let dismiss = dismiss_layer(Message::CloseMenu);
            let actions = match target {
                MenuTarget::Spelling(target, choices) => match choices {
                    None => column![text("Looking up suggestions…").size(14)],
                    Some(words) if words.is_empty() => {
                        column![text("No spelling suggestions").size(14)]
                    }
                    Some(words) => column(words.iter().enumerate().map(|(index, word)| {
                        theme::button(ellipsized_text(word, 14., None))
                            .height(SUGGESTION_HEIGHT)
                            .width(Fill)
                            .style(theme::text_button)
                            .on_press(Message::CorrectSpelling(target.clone(), index))
                            .into()
                    })),
                },
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
            let menu_width = 200_f32.min(self.window_width);
            let mut list = scroll(actions.spacing(ROW_GAP));
            if matches!(target, MenuTarget::Spelling(_, Some(words)) if words.len() > VISIBLE_SUGGESTIONS)
            {
                list = list
                    .height(VISIBLE_SUGGESTIONS as f32 * (SUGGESTION_HEIGHT + ROW_GAP) - ROW_GAP);
            }
            let menu = container(list).padding(6).width(menu_width).style(card);
            let x = position.x.min(self.window_width - menu_width - 10.).max(0.);
            let y = position.y.clamp(0., self.window_height);
            let anchored = if y > self.window_height / 2. {
                container(opaque(menu)).align_bottom(y)
            } else {
                container(column![space().height(y), opaque(menu)])
            };
            let positioned = row![space().width(x), anchored];
            layers.push(dismiss).push(positioned).into()
        } else if let Some(menu) = self.browser.menu {
            layers
                .push(opaque(dismiss_layer(Message::Browser(
                    browser::Action::Menu(None),
                ))))
                .push(self.browser_popup(menu))
                .into()
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
    fn panel_width(&self) -> f32 {
        if self.right_panel == Some(RightPanel::Browser) && self.browser.expanded {
            return (self.window_width
                - self
                    .catalog
                    .settings
                    .sidebar_width
                    .min(self.window_width * 0.4)
                - 310.)
                .max(self.window_width * 0.4);
        }
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
            right_panel_header("Agents", Message::ToggleAgents),
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
            let config = &run.configuration;
            let name = model(&config.model).map_or(config.model.as_str(), |m| m.name);
            let metadata = format!("{name} · {} · {elapsed}s", config.effort);
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
        let compacting = matches!(
            self.current_status,
            ConversationStatus::CompactingContext | ConversationStatus::RecoveringContext
        );
        let chat: Element<'_, Message> = if !compacting
            && session.entries.is_empty()
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
                let content = self.message_row(session, entry, projected);
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
                    && let Some(window) = self
                        .markdown
                        .get(&entry.id)
                        .and_then(|md| md.window.as_ref())
                        .or_else(|| self.prompt_windows.get(&entry.id).map(|p| &p.window))
                {
                    let expected =
                        self.transcript.height(position) - projected.spacing(&session.entries);
                    window.observe_row(content, expected, move |height, geometry| {
                        Message::RowGeometry(
                            epoch,
                            index,
                            session.entries.shared(projected.source()),
                            height,
                            geometry,
                        )
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
            if compacting {
                children.push((
                    TranscriptKey::Compacting,
                    text(ConversationStatus::CompactingContext.to_string())
                        .size(12)
                        .style(theme::muted)
                        .into(),
                ));
            }
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
                    self.right_panel != Some(RightPanel::Settings)
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
                .map(|(index, media)| {
                    self.image_view
                        .attachment(media, index, self.attachment_epoch)
                })
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
        let manual_compaction = self
            .harness
            .as_ref()
            .and_then(|h| h.get(&session.id))
            .is_some_and(|c| c.is_manually_compacting());
        let (send_icon, action) = if self.sending_message {
            (Icon::LoaderCircle, "Sending message…")
        } else if self.send_after_import.is_some() {
            (Icon::LoaderCircle, "Waiting for attachments…")
        } else if manual_compaction {
            (Icon::ArrowUp, "Wait for compaction to finish")
        } else if running {
            (Icon::ArrowRight, "Steer agent")
        } else {
            (Icon::ArrowUp, "Send message")
        };
        let mut toolbar = row![
            opaque(self.model_selector(session, running)),
            space().width(Fill)
        ];
        if self.send_after_import.is_some() {
            toolbar = toolbar.push(text("Sending after import…").size(12));
        }
        let toolbar = toolbar
            .push(opaque(footer_tip(
                theme::button(send_icon.spinning(16., self.sending_message))
                    .width(TOOLBAR_BUTTON_HEIGHT)
                    .height(TOOLBAR_BUTTON_HEIGHT)
                    .padding(5)
                    .style(secondary)
                    .on_press_maybe(
                        (!manual_compaction
                            && !self.io_busy
                            && self.send_after_import.is_none()
                            && (!self.editor.is_empty()
                                || !self.attachments.is_empty()
                                || self.composer_importing()))
                        .then(|| Message::Send(session.id.clone())),
                    ),
                action.into(),
            )))
            .align_y(iced::Center)
            .padding(iced::Padding::new(TOOLBAR_INSET).top(0.));
        let toolbar = mouse_area(toolbar)
            .on_press(Message::FocusComposer)
            .interaction(mouse::Interaction::Text);
        let editor = container(column![
            self.composer_editor(&session.id, running && !manual_compaction),
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
        let right = RAIL_WIDTH
            + if self.right_panel.is_some() {
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
                if self.footer_menu_open {
                    return secondary(theme, widget::button::Status::Pressed);
                }
                let mut style = secondary(theme, status);
                if status != widget::button::Status::Pressed {
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
        let label_width = |label: &str| {
            let mut widths = self.footer_widths.borrow_mut();
            if let Some(width) = widths.get(label) {
                return *width;
            }
            let width = text_width(label, 11.).ceil();
            widths.put(label.to_owned(), width);
            width
        };
        let name_width = label_width(&name).min(160.);
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
                    unreadable,
                } => (
                    branch,
                    (*added != 0 || *removed != 0 || *unreadable != 0).then(|| {
                        (
                            if *unreadable == 0 {
                                format!("+{added}")
                            } else {
                                format!("≥+{added}")
                            },
                            format!("−{removed}"),
                        )
                    }),
                ),
                crate::git::Info::Unavailable(message) => (message, None),
            };
            let measured_label = label_width(label).min(126.);
            let pill = matches!(git, crate::git::Info::Changes { .. });
            let mut width = measured_label;
            if pill {
                width += 2. * pill_inset + branch_icon_size + icon_gap;
                if draft {
                    width += chevron_size + icon_gap;
                }
            }
            if let Some((added, removed)) = &counts {
                width += 8. + 2. * pill_inset + label_width(added) + label_width(removed);
            }
            (width, measured_label, label, counts, pill, git)
        });

        // Reserve from most to least important. Once an item doesn't fit, no
        // lower-priority item may reappear in the remaining gap.
        let mut remaining = width;
        let mut reserve = |cost| {
            remaining -= cost;
            remaining >= 0.
        };
        let context = reserve(label_width(&tokens));
        let show_git = git.as_ref().is_none_or(|(width, ..)| reserve(10. + width));
        let project = reserve(10. + 16. + 4. + name_width);
        let cache = reserve(16. + label_width(&cache_label));
        let tps = reserve(16. + label_width(&rate_label));

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
                    .style(if self.right_panel == Some(RightPanel::Changes) {
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
fn rail_button<'a>(
    icon: Element<'a, Message>,
    label: &'static str,
    open: bool,
    on_press: Message,
) -> Element<'a, Message> {
    tooltip(
        theme::button(container(icon).center_x(Fill).center_y(34))
            .width(Fill)
            .padding(0)
            .style(if open { secondary } else { theme::text_button })
            .on_press(on_press),
        text(label).size(12),
        tooltip::Position::Left,
    )
    .style(card)
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

fn composer_key_binding(
    owner: &str,
    event: text_editor::KeyPress,
) -> Option<text_editor::Binding<Message>> {
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
            return Some(text_editor::Binding::Custom(
                if event.modifiers.is_empty() {
                    Message::ComposerEnter(owner.to_owned())
                } else {
                    Message::Send(owner.to_owned())
                },
            ));
        }
    }
    text_editor::Binding::from_key_press(event)
}

fn tool_actions(
    entry: &str,
    raw: bool,
    stop: Option<(&str, ToolStop)>,
) -> Element<'static, Message> {
    let mut actions = row![
        theme::button(
            row![
                chevron_icon(raw),
                text("Raw input").size(11).style(theme::muted)
            ]
            .spacing(6)
            .align_y(iced::Center)
        )
        .padding([2, 0])
        .style(theme::text_button)
        .on_press(Message::ToggleToolRaw(entry.to_owned()))
    ]
    .align_y(iced::Center);
    if let Some((session, state)) = stop {
        let available = state == ToolStop::Available;
        actions = actions.push(space().width(Fill)).push(
            theme::button(
                text(if available { "Stop" } else { "Stopping…" })
                    .size(11)
                    .style(theme::muted),
            )
            .padding([2, 6])
            .style(theme::text_button)
            .on_press_maybe(available.then(|| Message::StopTool {
                session: session.to_owned(),
                entry: entry.to_owned(),
            })),
        );
    }
    actions.into()
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
            | window::Event::Rescaled(_)
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
