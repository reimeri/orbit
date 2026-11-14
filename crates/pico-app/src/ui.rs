use crate::{
    chat_selection::{self, Position},
    ellipsized_text::ellipsized_text,
    pointer_input::{Press, pointer_input},
    tool_output,
};
use iced::futures::SinkExt;
use iced::{
    Color, Element, Fill, Subscription, Task, Theme, keyboard, mouse,
    widget::{
        self, button, checkbox, column, container, markdown, mouse_area, opaque, pick_list, row,
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
        Store,
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

use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone)]
enum SidebarRow {
    Heading(&'static str, usize),
    Session(usize),
}

type PendingLogin = Arc<tokio::sync::Mutex<Option<Login>>>;

const MUTED: Color = Color::from_rgb8(141, 149, 164);
const PANEL: Color = Color::from_rgb8(29, 32, 37);
const BORDER: Color = Color::from_rgb8(48, 53, 61);
const ACCENT: Color = Color::from_rgb8(137, 175, 210);
const SIDEBAR_CONTENT_PADDING: f32 = 8.;
const FAST_OPTION: &str = "Fast mode";

pub struct App {
    harness: Option<Arc<Harness>>,
    catalog: Catalog,
    sessions: Vec<SessionSummary>,
    current_session: Option<Session>,
    draft_session: Option<Session>,
    new_draft: NewSessionDraft,
    new_draft_dirty: bool,
    transcript: crate::virtual_list::List,
    transcript_count: usize,
    child_counts: HashMap<String, usize>,
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
    submissions: HashMap<String, (String, String)>,
    revisions: HashMap<String, u64>,
    outputs: HashMap<String, tool_output::Output>,
    context_menu: Option<(String, iced::Point)>,
    footer_menu: Option<FooterMenu>,
    selected: Option<String>,
    search: String,
    search_active: bool,
    editor: text_editor::Content,
    chat_selection: chat_selection::Shared,
    markdown: HashMap<String, (Arc<Entry>, markdown::Content)>,
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
    git: Option<crate::git::Info>,
    git_pending: bool,
    git_cost: Duration,
    draft_dirty: bool,
    smoke: bool,
    smoke_stage: usize,
    smoke_passed: Arc<AtomicBool>,
    smoke_dir: Option<PathBuf>,
    window_id: Option<window::Id>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    Account,
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
    Selected(Result<(Selection, Option<String>), String>),
    NewDraftSaved(Result<(), String>),
    Sent(String, String, Result<(), String>),
    DraftSaved(String, String, Result<(), String>),
    Deleted(String, Result<(), String>),
    Edited(Result<(), String>),
    Tick,
    Changed,
    GitTick,
    SaveDraft,
    AgeTick,
    Press(Press),
    SessionMenu(String, iced::Point),
    CloseMenu,
    FooterMenu(FooterControl),
    CloseFooterMenu,
    ToolOutput(String, tool_output::Event),
    CopyOutput(String),
    CopySelection,
    New,
    Select(String),
    Create(PathBuf),
    Search(String),
    ToggleSearch,
    Favorite(PathBuf),
    Editor(text_editor::Action),
    DeletePreviousWord,
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
    ToggleGroup(String),
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
    Git(String, crate::git::Info, Duration),
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
            transcript: crate::virtual_list::List::default(),
            transcript_count: 0,
            child_counts: HashMap::new(),
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
            revisions: HashMap::new(),
            outputs: HashMap::new(),
            context_menu: None,
            footer_menu: None,
            selected,
            search: String::new(),
            search_active: false,
            editor: text_editor::Content::new(),
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
            git: None,
            git_pending: false,
            git_cost: Duration::ZERO,
            draft_dirty: false,
            smoke,
            smoke_stage: 0,
            smoke_passed,
            smoke_dir,
            window_id: None,
        };
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
    fn running(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|id| self.harness.as_ref()?.get(id))
            .is_some_and(|c| c.is_running())
    }
    fn refresh(&mut self) -> bool {
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
                self.transcript = crate::virtual_list::List::default();
                self.transcript_count = 0;
                self.child_counts.clear();
                self.view_epoch = self.view_epoch.wrapping_add(1);
            }
            self.outputs.clear();
            self.markdown.clear();
            self.chat_selection.borrow_mut().clear();
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
            self.transcript = crate::virtual_list::List::default();
            self.transcript_count = 0;
            self.child_counts.clear();
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
            if let Some((source, content)) = self.markdown.get_mut(&entry.id) {
                if entry.text.starts_with(&source.text) {
                    content.push_str(&entry.text[source.text.len()..]);
                } else {
                    *content = markdown::Content::parse(&entry.text);
                }
                *source = snapshot.entries.shared(index);
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
        for (offset, entry) in session.entries.range(self.transcript_count).enumerate() {
            if let EntryKind::Tool {
                parent: Some(parent),
                ..
            } = &entry.kind
            {
                *self.child_counts.entry(parent.clone()).or_default() += 1;
                if !self.groups.contains(parent) {
                    continue;
                }
            }
            let height = match &entry.kind {
                EntryKind::Tool { .. } => 70.,
                EntryKind::Reasoning if !self.expanded.contains(&entry.id) => 40.,
                _ => 120.,
            };
            self.transcript.push(self.transcript_count + offset, height);
        }
        self.transcript_count = session.entries.len();
    }
    fn rebuild_transcript(&mut self) {
        self.chat_selection.borrow_mut().clear();
        let Some(session) = &self.current_session else {
            return;
        };
        self.transcript_count = session.entries.len();
        self.child_counts.clear();
        for entry in session.entries.iter() {
            if let EntryKind::Tool {
                parent: Some(parent),
                ..
            } = &entry.kind
            {
                *self.child_counts.entry(parent.clone()).or_default() += 1;
            }
        }
        self.transcript.reset(session.entries.iter().enumerate().filter(|(_,e)|!matches!(&e.kind,EntryKind::Tool {parent:Some(p),..} if !self.groups.contains(p))).map(|(i,e)| {
            let height=match &e.kind {EntryKind::Tool {..}=>70.,EntryKind::Reasoning if !self.expanded.contains(&e.id)=>40.,_=>120.};
            (i,height)
        }));
    }
    fn prepare_visible(&mut self) {
        if let Some(session) = &self.current_session {
            let visible = &self.transcript.order[self.transcript.visible()];
            let ids: HashSet<_> = visible
                .iter()
                .map(|&i| session.entries[i].id.as_str())
                .collect();
            self.markdown.retain(|id, _| ids.contains(id.as_str()));
            self.outputs
                .retain(|id, _| ids.contains(id.as_str()) && self.expanded.contains(id));
            for &index in visible {
                let entry = &session.entries[index];
                if matches!(entry.kind, EntryKind::User | EntryKind::Assistant)
                    || matches!(entry.kind, EntryKind::Reasoning)
                        && self.expanded.contains(&entry.id)
                {
                    self.markdown.entry(entry.id.clone()).or_insert_with(|| {
                        (
                            session.entries.shared(index),
                            markdown::Content::parse(&entry.text),
                        )
                    });
                }
                if let EntryKind::Tool { input, output, .. } = &entry.kind
                    && self.expanded.contains(&entry.id)
                {
                    self.outputs
                        .entry(entry.id.clone())
                        .or_insert_with(|| tool_output::Output::new(input, output));
                }
            }
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
            let c = self.harness.as_ref().and_then(|h| h.get(&s.id));
            let category = if c.as_ref().is_some_and(|c| c.is_running())
                || self
                    .harness
                    .as_ref()
                    .is_some_and(|h| h.active_children(&s.id) > 0)
            {
                0
            } else if self
                .harness
                .as_ref()
                .is_some_and(|h| h.children_need_attention(&s.id))
                || s.interrupted
                || s.needs_attention
                || c.as_ref()
                    .is_some_and(|c| *c.status.lock().expect("status lock") == "Needs attention")
            {
                1
            } else {
                2
            };
            sections[category].push(SidebarRow::Session(index));
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
                SidebarRow::Session(index) => {
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
    fn take_draft(&mut self) -> Option<(String, String)> {
        if !self.draft_dirty {
            return None;
        }
        let id = self.selected.clone()?;
        self.draft_dirty = false;
        Some((id, self.editor.text()))
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
                let result=h.edit(&id,|s|s.draft=draft.clone()).map_err(|e|format!("Could not save draft: {e:#}. Keep Pico open and retry after fixing storage."));
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
        self.draft_dirty = false;
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
                            h.edit(&old, |s| s.draft = draft)?;
                        }
                        let id = if let Selection::Session(id) = &selection {
                            h.load(id)?;
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
    fn fail(&mut self, result: Result<(), impl std::fmt::Display>) {
        if let Err(e) = result {
            self.error = Some(e.to_string());
        }
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.update_inner(message);
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
                        self.restore_draft();
                        self.git = None;
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
                        if self.selected.as_ref() == Some(&id) && self.editor.text() == text {
                            self.editor = text_editor::Content::new();
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
                match result {
                    Ok(()) => {
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
                    Err(error) => self.error = Some(error),
                }
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
            Message::GitTick => {
                if !self.git_pending
                    && let Some(s) = self.current()
                {
                    let id = s.id.clone();
                    let path = s.project.clone();
                    self.git_pending = true;
                    return Task::perform(
                        async move {
                            let start = std::time::Instant::now();
                            let result = crate::git::info(path).await;
                            (id, result, start.elapsed())
                        },
                        |(id, info, cost)| Message::Git(id, info, cost),
                    );
                }
            }
            Message::SessionMenu(id, position) => {
                self.context_menu = Some((id, position));
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
            Message::CopyOutput(id) => {
                if let Some(Entry {
                    kind: EntryKind::Tool { output, .. },
                    ..
                }) = self.current().and_then(|s| s.entries.by_id(&id))
                {
                    return iced::clipboard::write(output.clone());
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
            Message::Editor(_) | Message::DeletePreviousWord => {
                if self.io_busy
                    || self.closing
                    || self.close_requested
                    || self.modal.is_some()
                    || self.context_menu.is_some()
                {
                    return Task::none();
                }
                let action = if let Message::Editor(action) = message {
                    action
                } else {
                    let cursor = self.editor.cursor();
                    if cursor.selection.is_none() {
                        let mut start = cursor.position;
                        if start.column > 0 {
                            let line = self.editor.line(start.line).expect("cursor line exists");
                            // Match TextInput's Unicode word boundaries, in logical
                            // order: visual WordLeft moves forward on RTL lines.
                            start.column = line.text[..start.column]
                                .split_word_bound_indices()
                                .rfind(|(_, word)| !word.trim_start().is_empty())
                                .map_or(0, |(index, _)| index);
                        } else if start.line > 0 {
                            start.line -= 1;
                            start.column = self
                                .editor
                                .line(start.line)
                                .expect("previous cursor line exists")
                                .text
                                .len();
                        }
                        self.editor.move_to(text_editor::Cursor {
                            position: cursor.position,
                            selection: Some(start),
                        });
                    }
                    text_editor::Action::Edit(text_editor::Edit::Backspace)
                };
                let edit = action.is_edit();
                self.editor.perform(action);
                if edit && self.draft_session.is_some() {
                    self.new_draft.message = self.editor.text();
                    self.new_draft_dirty = true;
                } else {
                    self.draft_dirty |= edit;
                }
            }
            Message::Send => {
                if self.modal.is_some() || self.context_menu.is_some() {
                    return Task::none();
                }
                if let (Some(h), Some(session)) = (&self.harness, self.current()) {
                    let message = self.editor.text();
                    if message.trim().is_empty() {
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
                self.settings_open = false;
                self.view_epoch = self.view_epoch.wrapping_add(1);
            }
            Message::ToggleSettings => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                self.settings_open = !self.settings_open;
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
                    .is_some_and(|(_, since)| since.elapsed() >= Duration::from_secs(8))
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
            Message::ToggleEntry(id) => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                if self.expanded.remove(&id) {
                    self.outputs.remove(&id);
                } else {
                    if let Some(Entry {
                        kind: EntryKind::Tool { input, output, .. },
                        ..
                    }) = self.current().and_then(|s| s.entries.by_id(&id))
                    {
                        self.outputs
                            .insert(id.clone(), tool_output::Output::new(input, output));
                    }
                    self.expanded.insert(id);
                }
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
                let corrected = self.transcript.measure(index, height + 18.);
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
            }
            Message::SidebarScroll(epoch, offset, height) => {
                if epoch != self.sidebar_epoch {
                    return Task::none();
                }
                self.sidebar_list.offset = offset;
                self.sidebar_list.viewport = height;
            }
            Message::SidebarSize(epoch, index, height) => {
                if epoch != self.sidebar_epoch {
                    return Task::none();
                }
                if self.sidebar_list.measure(index, height + 5.) {
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
                        markdown::Style::from_palette(Theme::Dark.palette()),
                    );
                    let entries = self
                        .transcript
                        .order
                        .iter()
                        .copied()
                        .filter(|i| (start.entry..=end.entry).contains(i))
                        .map(|index| {
                            let entry = &session.entries[index];
                            let blocks = match &entry.kind {
                                EntryKind::Tool { input, output, .. }
                                    if self.expanded.contains(&entry.id) =>
                                {
                                    vec![chat_selection::Block {
                                        text: tool_output::display(input, output),
                                        separator: "\n\n",
                                    }]
                                }
                                EntryKind::Tool { .. } => vec![],
                                EntryKind::Reasoning if !self.expanded.contains(&entry.id) => {
                                    vec![]
                                }
                                EntryKind::Notice => vec![chat_selection::Block {
                                    text: entry.text.clone(),
                                    separator: "\n\n",
                                }],
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
            Message::Copy(id) => {
                if let Some(entry) = self.current().and_then(|s| s.entries.by_id(&id)) {
                    return iced::clipboard::write(entry.text.clone());
                }
            }
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
            Message::Git(id, info, cost) => {
                self.git_cost = cost;
                self.git_pending = false;
                if self.current().is_some_and(|s| s.id == id) {
                    self.git = Some(info);
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
                            self.view_epoch = self.view_epoch.wrapping_add(1);
                            self.settings_open = false;
                        } else if self.agents_open {
                            self.agents_open = false;
                            self.view_epoch = self.view_epoch.wrapping_add(1);
                        } else if let (Some(h), Some(id)) = (&self.harness, &self.selected)
                            && (h.get(id).is_some_and(|s| s.is_running())
                                || h.active_children(id) > 0)
                        {
                            h.stop(id);
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
                    if let Some((id,draft))=draft {h.edit(&id,|s|s.draft=draft)?;}
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
            if harness.all().iter().any(|c| c.is_running()) {
                subscriptions
                    .push(iced::time::every(Duration::from_millis(100)).map(|_| Message::Tick));
            }
        }
        if self.current().is_some() {
            subscriptions.push(
                iced::time::every(
                    self.git_cost
                        .saturating_mul(10)
                        .max(Duration::from_secs(if self.running() { 5 } else { 60 }))
                        .min(Duration::from_secs(300)),
                )
                .map(|_| Message::GitTick),
            );
        }
        if self.notice.is_some() {
            subscriptions
                .push(iced::time::every(Duration::from_secs(1)).map(|_| Message::NoticeTick));
        }
        if self.draft_dirty || self.new_draft_dirty {
            subscriptions
                .push(iced::time::every(Duration::from_secs(1)).map(|_| Message::SaveDraft));
        }
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
                        button("×")
                            .on_press(Message::NoticeDismiss)
                            .style(button::text)
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
                        button("Dismiss").on_press(Message::ErrorDismiss)
                    ]
                    .spacing(12),
                )
                .padding(12)
                .style(|_| container::Style {
                    background: Some(Color::from_rgb8(66, 39, 43).into()),
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
                    button(
                        container(crate::agent_icon::agent_icon(self.agent_count() > 0))
                            .center_x(Fill)
                            .center_y(34)
                    )
                    .width(Fill)
                    .padding(0)
                    .on_press(Message::ToggleAgents)
                    .style(button::text),
                    text("Agents").size(12),
                    tooltip::Position::Left,
                )
                .style(card),
                button(container(text("⚙").size(24)).center_x(Fill).center_y(34))
                    .width(Fill)
                    .padding(0)
                    .on_press(Message::ToggleSettings)
                    .style(button::text)
            ]
            .spacing(12)
            .width(Fill),
        )
        .padding([14, 8])
        .height(Fill)
        .width(50)
        .style(panel);
        let base: Element<_> = body.push(rail).into();
        if let Some(modal) = &self.modal {
            let shade = mouse_area(container(space().width(Fill).height(Fill)).style(|_| {
                container::Style {
                    background: Some(Color::from_rgba(0., 0., 0., 0.65).into()),
                    ..Default::default()
                }
            }))
            .on_press(Message::Dismiss);
            stack![
                base,
                shade,
                container(opaque(self.modal_view(modal)))
                    .center_x(Fill)
                    .center_y(Fill)
            ]
            .into()
        } else if let Some((id, position)) = &self.context_menu {
            let dismiss = mouse_area(space().width(Fill).height(Fill))
                .on_press(Message::CloseMenu)
                .on_right_press(Message::CloseMenu)
                .on_scroll(|_| Message::CloseMenu);
            let menu = container(
                column![
                    button("Rename")
                        .width(Fill)
                        .on_press(Message::Rename(id.clone()))
                        .style(button::text),
                    button("Delete")
                        .width(Fill)
                        .on_press(Message::Delete(id.clone()))
                        .style(button::text)
                ]
                .spacing(4),
            )
            .padding(6)
            .width(170)
            .style(card);
            let x = position.x.min(self.window_width - 180.).max(0.);
            let y = position.y.min(self.window_height - 100.).max(0.);
            let positioned = column![space().height(y), row![space().width(x), opaque(menu)]];
            stack![base, dismiss, positioned].into()
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
            stack![base, dismiss, positioned].into()
        } else {
            base
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
        let control = button(container(icon).center_x(18).center_y(24))
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
                .style(|theme, status| {
                    let mut style = text_input::default(theme, status);
                    style.background = Color::from_rgb8(39, 42, 48).into();
                    style.border = iced::Border {
                        radius: 3.into(),
                        ..Default::default()
                    };
                    style.placeholder = MUTED;
                    style
                })
                .into()
        } else {
            let badges = row(self
                .catalog
                .projects
                .iter()
                .filter(|p| p.favorite)
                .map(|p| {
                    tooltip(
                        button(project_badge(&p.path))
                            .padding(0)
                            .on_press(Message::Create(p.path.clone()))
                            .style(button::text),
                        text(format!("New session in {}\n{}", p.name(), p.path.display())).size(12),
                        tooltip::Position::Bottom,
                    )
                    .style(card)
                    .into()
                }))
            .spacing(6)
            .align_y(iced::Center);
            container(
                scrollable(badges)
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
                button(container(text("+").size(16)).center_x(18).center_y(24))
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
            row![
                text("Agents").size(20).width(Fill),
                button("×")
                    .on_press(Message::ToggleAgents)
                    .style(button::text)
            ],
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
            let mut card_content = column![
                row![
                    text(run.profile.label()).size(13).width(Fill),
                    text(run.status.label())
                        .size(11)
                        .color(if run.status.needs_attention() {
                            Color::from_rgb8(224, 130, 130)
                        } else {
                            MUTED
                        })
                ],
                text(run.title).size(13),
                text(format!(
                    "{} · {} · {elapsed}s",
                    run.profile.model(),
                    run.profile.effort()
                ))
                .size(10)
                .color(MUTED),
            ]
            .spacing(7);
            if let Some(activity) = activity {
                card_content = card_content.push(text(activity).size(11).color(ACCENT));
            }
            let mut actions = row![
                button("Open")
                    .on_press(Message::Select(run.session_id.clone()))
                    .style(button::text)
            ]
            .spacing(10);
            if run.status == pico_core::agent::subagents::RunStatus::Running {
                actions = actions.push(
                    button("Stop")
                        .on_press(Message::StopAgent(run.session_id))
                        .style(button::text),
                );
            }
            cards = cards.push(
                container(card_content.push(actions))
                    .padding(12)
                    .width(Fill)
                    .style(card),
            );
        }
        content = content.push(scrollable(cards).height(Fill));
        if let Some(parent) = self.parent_id() {
            content = content.push(
                button("Stop parent and children")
                    .on_press_maybe(
                        (self.agent_count() > 0
                            || self
                                .harness
                                .as_ref()
                                .and_then(|h| h.get(parent))
                                .is_some_and(|c| c.is_running()))
                        .then(|| Message::StopAgent(parent.into())),
                    )
                    .style(button::text),
            );
        }
        container(content)
            .padding(16)
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
        let mut list =
            column![container(self.sidebar_header()).padding([0., SIDEBAR_CONTENT_PADDING])]
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
                SidebarRow::Session(session_index) => {
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
                    let attention = self
                        .harness
                        .as_ref()
                        .is_some_and(|h| h.children_need_attention(&session.id));
                    let age = if children > 0 {
                        format!("● {children} · {}", age(session.updated))
                    } else if attention {
                        format!("! · {}", age(session.updated))
                    } else {
                        age(session.updated)
                    };
                    let content = mouse_area(
                        button(
                            column![
                                ellipsized_text(&session.title, 13., None),
                                row![
                                    badge,
                                    ellipsized_text(&project, 11., Some(MUTED)),
                                    text(age).size(10).color(MUTED)
                                ]
                                .spacing(6)
                                .align_y(iced::Center)
                            ]
                            .spacing(3),
                        )
                        .padding([6., SIDEBAR_CONTENT_PADDING])
                        .width(Fill)
                        .on_press(Message::Select(session.id.clone()))
                        .style(if active {
                            secondary
                        } else {
                            button::text
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
                matches!(self.sidebar_rows[index], SidebarRow::Session(_)),
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
        let rows = crate::animated_column::animated_column(rows);
        list = list.push(
            scrollable(rows)
                .id("sessions")
                .height(Fill)
                .on_scroll(move |v| {
                    Message::SidebarScroll(epoch, v.absolute_offset().y, v.bounds().height)
                }),
        );
        container(list)
            .padding(iced::Padding {
                top: 4.,
                right: 8.,
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
        let mut children: Vec<((u64, usize), Element<'_, Message>)> = vec![(
            (epoch, usize::MAX),
            space().height(self.transcript.prefix(range.start)).into(),
        )];
        if session.entries.is_empty() {
            children.push((
                (epoch, 0),
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
            let entry = &session.entries[index];
            let row = widget::sensor(self.entry_view(entry, index))
                .key((epoch, index))
                .on_show(move |size| Message::RowSize(epoch, index, size.height))
                .on_resize(move |size| Message::RowSize(epoch, index, size.height));
            children.push(((epoch, index), column![row, space().height(18.)].into()));
        }
        children.push((
            (epoch, usize::MAX - 1),
            space()
                .height((self.transcript.total() - self.transcript.prefix(range.end)).max(0.))
                .into(),
        ));
        let transcript = widget::keyed_column(children);
        let transcript = container(transcript)
            .max_width(self.catalog.settings.chat_width)
            .width(Fill)
            .padding([20, 26]);
        let chat = scrollable(container(transcript).center_x(Fill))
            .id("chat")
            .height(Fill)
            .on_scroll(move |v| {
                let offset = v.absolute_offset();
                let bounds = v.bounds();
                let content = v.content_bounds();
                Message::Scroll(
                    epoch,
                    offset.y,
                    bounds.height,
                    offset.y + bounds.height >= content.height - 40.,
                )
            });
        let running = self.running();
        if let Some(child) = &session.child {
            let parent_title = self
                .sessions
                .iter()
                .find(|s| s.id == child.parent_id)
                .map_or("Parent", |s| s.title.as_str());
            let status = self
                .harness
                .as_ref()
                .and_then(|h| {
                    h.subagent_runs(&child.parent_id)
                        .into_iter()
                        .find(|r| r.session_id == session.id)
                })
                .map_or("Ready", |r| r.status.label());
            let header = column![
                row![
                    button(ellipsized_text(
                        &format!("← Back to {parent_title}"),
                        12.,
                        None
                    ))
                    .width(Fill)
                    .on_press(Message::Select(child.parent_id.clone()))
                    .style(button::text),
                    button("Stop")
                        .on_press_maybe(running.then(|| Message::StopAgent(session.id.clone())))
                        .style(button::text)
                ]
                .spacing(8)
                .align_y(iced::Center),
                row![
                    ellipsized_text(
                        &format!("{} · {}", child.profile.label(), session.title),
                        13.,
                        None
                    ),
                    text(status).size(11).color(MUTED)
                ]
                .spacing(8)
            ]
            .spacing(8);
            return column![container(header).padding(12).style(panel),chat,
                container(text("Read-only profile instructions · Shell access is not sandboxed · Continue through the parent").size(11).color(MUTED)).padding(12)
            ].width(Fill).height(Fill).into();
        }
        let editor = text_editor(&self.editor)
            .placeholder(if self.io_busy || self.closing {
                "Saving…"
            } else if running {
                "Steer the agent after its current tool batch…"
            } else {
                "Message Pico…"
            })
            .on_action(Message::Editor)
            .key_binding(composer_key_binding)
            .height(100)
            .padding(12)
            .size(self.catalog.settings.font_size);
        let mut compose = column![editor].spacing(10);
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
    fn entry_view<'a>(&'a self, entry: &'a Entry, index: usize) -> Element<'a, Message> {
        match &entry.kind {
            EntryKind::Tool {
                name,
                input,
                complete,
                parent,
                ..
            } => {
                let open = self.expanded.contains(&entry.id);
                let children = self.child_counts.get(&entry.id).copied().unwrap_or(0);
                let preview = input
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .chars()
                    .take(100)
                    .collect::<String>();
                let mut block = column![
                    button(
                        row![
                            text(if open { "▾" } else { "▸" }).color(MUTED),
                            text(if *complete { "✓" } else { "•" }).color(ACCENT),
                            text(name).size(13),
                            text(preview).size(11).color(MUTED).width(Fill)
                        ]
                        .spacing(9)
                        .align_y(iced::Center)
                    )
                    .on_press(Message::ToggleEntry(entry.id.clone()))
                    .style(button::text)
                    .width(Fill)
                ]
                .spacing(8);
                if name == "spawn_subagent"
                    && let (Some(h), Some(session)) = (&self.harness, self.current())
                    && let Some(run) = h.subagent_for_request(&session.id, &entry.id)
                {
                    block = block.push(
                        button(
                            text(format!(
                                "{} · {} · {} ↗",
                                run.profile.label(),
                                run.title,
                                run.status.label()
                            ))
                            .size(12),
                        )
                        .on_press(Message::Select(run.session_id))
                        .style(button::text),
                    );
                }
                if children > 0 {
                    block = block.push(
                        button(
                            text(format!(
                                "{}  {children} nested tool calls",
                                if self.groups.contains(&entry.id) {
                                    "▾"
                                } else {
                                    "▸"
                                }
                            ))
                            .size(11),
                        )
                        .on_press(Message::ToggleGroup(entry.id.clone()))
                        .style(button::text),
                    );
                }
                if open {
                    if let Some(view) = self.outputs.get(&entry.id) {
                        block =
                            block.push(view.view(&entry.id, self.chat_selection.clone(), index));
                    }
                    block = block.push(
                        button("Copy output")
                            .on_press(Message::CopyOutput(entry.id.clone()))
                            .style(button::text),
                    );
                }
                container(block)
                    .padding(if parent.is_some() { [10, 22] } else { [10, 12] })
                    .width(Fill)
                    .style(card)
                    .into()
            }
            EntryKind::Notice => container(chat_selection::plain(
                &entry.text,
                12.,
                Some(Color::from_rgb8(224, 186, 120)),
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
            EntryKind::Reasoning if !self.expanded.contains(&entry.id) => {
                button(text("▸  Thinking").size(12).color(MUTED))
                    .on_press(Message::ToggleEntry(entry.id.clone()))
                    .style(button::text)
                    .into()
            }
            _ => {
                let name = match entry.kind {
                    EntryKind::User => "YOU",
                    EntryKind::Reasoning => "THINKING",
                    _ => "PICO",
                };
                let mut content = column![
                    row![
                        text(name)
                            .size(10)
                            .color(if matches!(entry.kind, EntryKind::User) {
                                MUTED
                            } else {
                                ACCENT
                            }),
                        space().width(Fill),
                        button(text("Copy").size(10))
                            .on_press(Message::Copy(entry.id.clone()))
                            .style(button::text)
                    ]
                    .align_y(iced::Center)
                ]
                .spacing(8);
                if let Some((_, items)) = self.markdown.get(&entry.id) {
                    content = content.push(markdown::view_with(
                        items.items(),
                        markdown::Settings::with_text_size(
                            self.catalog.settings.font_size,
                            markdown::Style::from_palette(Theme::Dark.palette()),
                        ),
                        &chat_selection::Viewer::new(self.chat_selection.clone(), index),
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
                if matches!(entry.kind, EntryKind::Reasoning) {
                    content = content.push(
                        button("▴ Collapse thinking")
                            .on_press(Message::ToggleEntry(entry.id.clone()))
                            .style(button::text),
                    );
                }
                container(content)
                    .padding(12)
                    .width(Fill)
                    .style(if matches!(entry.kind, EntryKind::User) {
                        card
                    } else {
                        container::transparent
                    })
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
                    button(
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
                    button(text(if project.favorite { "★" } else { "☆" }).size(22))
                        .on_press(Message::Favorite(project.path.clone()))
                        .style(button::text)
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
            button("+  Add project")
                .on_press_maybe(self.harness.as_ref().map(|_| Message::AddProject))
                .padding(12)
                .style(secondary)
        ]
        .spacing(22);
        if self.catalog.projects.is_empty() {
            content=content.push(text("Add a local folder to begin. Pico can read and edit files and run commands in your projects.").size(13).color(MUTED));
        }
        if self.account.is_none() {
            content=content.push(container(column![text("Connect your ChatGPT subscription").size(16),text("Sign in from Settings. Your login is saved securely in your private pico-evo configuration directory.").size(13).color(MUTED),button("Open Settings →").on_press(Message::ToggleSettings).style(button::text)].spacing(10)).padding(18).style(card));
        }
        container(scrollable(container(content).max_width(650).padding(36)))
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
        ] {
            tabs = tabs.push(
                button(text(label).size(13).width(Fill).align_x(iced::Center))
                    .width(Fill)
                    .padding([8, 4])
                    .on_press(Message::SettingsTab(tab))
                    .style(if self.settings_tab == tab {
                        button::primary
                    } else {
                        button::text
                    }),
            );
        }
        let header = column![
            row![
                text("Settings").size(22),
                space().width(Fill),
                button("×")
                    .on_press(Message::ToggleSettings)
                    .style(button::text)
            ],
            tabs,
            widget::rule::horizontal(1)
        ]
        .spacing(12);
        let mut settings = column![].spacing(14);
        match self.settings_tab {
            SettingsTab::Account => {
                if let Some(account) = &self.account {
                    settings = settings.push(text(account).size(14)).push(
                        button("Sign out")
                            .on_press(Message::Logout)
                            .style(secondary),
                    );
                } else if self.login_cancel.is_some() {
                    settings = settings.push(text("Finish sign-in in your browser.").size(14));
                    if let Some(url) = &self.login_url {
                        settings = settings.push(
                            button("Copy login URL")
                                .on_press(Message::Copy(url.clone()))
                                .style(button::text),
                        );
                    }
                    settings = settings.push(
                        button("Cancel login")
                            .on_press(Message::CancelLogin)
                            .style(secondary),
                    );
                } else {
                    settings = settings
                        .push(text("Use your ChatGPT subscription.").size(13).color(MUTED))
                        .push(
                            button("Sign in with ChatGPT")
                                .on_press_maybe(self.harness.as_ref().map(|_| Message::Login))
                                .padding(10),
                        );
                }
            }
            SettingsTab::General => {
                settings = settings
                    .push(text("Default model").size(12))
                    .push(
                        pick_list(
                            model_options(),
                            Some(self.settings.model.clone()),
                            Message::Model,
                        )
                        .width(Fill),
                    )
                    .push(text("Context token limit").size(12))
                    .push(
                        text_input("272000", &self.context_text)
                            .on_input(Message::Context)
                            .padding(8),
                    )
                    .push(text("Auto-compact at (%)").size(12))
                    .push(
                        text_input("80", &self.threshold_text)
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
                        checkbox(self.catalog.settings.resume_last)
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
                                .width(Fill),
                                checkbox(defaults.fast).label("Fast by default").on_toggle(
                                    move |fast| Message::ModelFast(model.id.into(), fast)
                                ),
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
        let mut panel_content = column![header, scrollable(settings).height(Fill)].spacing(14);
        if self.settings_tab != SettingsTab::Account {
            panel_content = panel_content.push(
                button("Save")
                    .on_press(Message::ApplySettings)
                    .style(secondary),
            );
        }
        container(panel_content)
            .padding(18)
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
        let cache = (session.usage.cached as f64 * 100. / used.max(1) as f64).round();
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
            "Cached input: {}\nTotal input: {}",
            compact_tokens(session.usage.cached),
            compact_tokens(used),
        );
        let effort_label = if session.fast {
            format!("{} · Fast", session.effort)
        } else {
            session.effort.clone()
        };
        let layout = FooterLayout::new(width, &session.model, &effort_label);
        let mut hidden = Vec::new();
        if !layout.git
            && let Some(git) = &self.git
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
            let chip = button(ellipsized_text(label, 11., None))
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
        let dot = if running {
            ["◦", "○", "●", "○"][(self.tick as usize / 2) % 4]
        } else {
            "●"
        };
        let mut info = row![footer_tip(
            container(
                text(dot)
                    .size(11)
                    .color(if running { ACCENT } else { MUTED })
            )
            .width(12),
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
                        text(dot)
                            .size(11)
                            .color(if running { ACCENT } else { MUTED }),
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
            && let Some(git) = &self.git
        {
            let view: Element<'_, Message> = match git {
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
                        text(additions)
                            .size(11)
                            .color(Color::from_rgb8(125, 192, 154)),
                        text(deletions)
                            .size(11)
                            .color(Color::from_rgb8(220, 120, 120)),
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
            let item = button(
                row![
                    ellipsized_text(option, 12., None),
                    text(if chosen { "✓" } else { "" })
                        .size(12)
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
            .style(move |theme, status| {
                let mut style = button::text(theme, status);
                if focused
                    || chosen
                    || matches!(status, button::Status::Hovered | button::Status::Pressed)
                {
                    style.background = Some(Color::from_rgb8(42, 42, 52).into());
                }
                style.border.radius = 4.into();
                style
            });
            options = options.push(if footer_text_width(option, 12.) > 228. {
                footer_tip(item, option.clone())
            } else {
                item.into()
            });
        }
        opaque(
            container(scrollable(options).height((menu.options.len() as f32 * 32. - 2.).min(240.)))
                .padding(6)
                .width(280)
                .style(footer_surface),
        )
    }
    fn modal_view(&self, modal: &Modal) -> Element<'_, Message> {
        let (title, body, confirm) = match modal {
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
                        .id("modal-input")
                        .on_input(Message::ModalInput)
                        .on_submit(Message::Confirm)
                        .padding(10),
                );
                if matches!(modal, Modal::AddProject) {
                    content = content.push(
                        button("Browse folders…")
                            .on_press(Message::Browse)
                            .style(secondary),
                    );
                }
            }
            Modal::Link(url) => content = content.push(text(url.clone()).size(13)),
            _ => {}
        }
        if let Some(error) = &self.error {
            content = content.push(text(error).size(12).color(Color::from_rgb8(224, 130, 130)));
        }
        content = content.push(
            row![
                space().width(Fill),
                button("Cancel").on_press(Message::Dismiss).style(secondary),
                button(confirm).on_press(Message::Confirm)
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
                s.usage.total_output = 1_237;
                s.entries = vec![
                    Entry::new(EntryKind::User, "Inspect this project and run its tests."),
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
                        parent: None,
                    },
                };
                s.entries.push(tool);
                let id = s.id.clone();
                h.insert(s);
                self.select(Selection::Session(id));
                let _ = self.update_inner(Message::ToggleEntry("smoke-tool".into()));
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
                self.git = Some(crate::git::Info::Changes {
                    branch: "main".into(),
                    added: 123,
                    removed: 45,
                });
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
                self.transcript = crate::virtual_list::List::default();
                self.transcript_count = 0;
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
                    EntryKind::User,
                    "Inspect the completion notification path. Do not modify files.",
                ));
                child.entries.push(Entry::new(EntryKind::Assistant,"## Exploration result\n\nThe **parent wake-up** path preserves cancellation and uses durable completion notices.\n\n- Separate conversation context\n- Repeatable final results\n- No nested delegation"));
                let id = child.id.clone();
                self.harness.as_ref().expect("smoke harness").insert(child);
                self.select(Selection::Session(id));
            }
            24 => {
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
            _ => {
                self.smoke_passed.store(true, Ordering::Relaxed);
                println!(
                    "SMOKE_OK: native Wayland rendered picker, sidebar favorites/search/reflow animations, chat, settings, modal, session/footer menus, Agents panel, read-only child navigation, virtualized large output and 20,000-entry/10,000-session lists (middle/end/reflow/identity)"
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
fn footer_surface(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgb8(30, 30, 36).into()),
        border: iced::Border {
            radius: 6.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn sidebar_control(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::text(theme, status);
    style.text_color = if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        theme.palette().text
    } else {
        MUTED
    };
    style
}

fn secondary(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::secondary(theme, status);
    style.background = Some(
        if matches!(status, button::Status::Hovered) {
            Color::from_rgb8(55, 62, 74)
        } else {
            Color::from_rgb8(43, 49, 58)
        }
        .into(),
    );
    style.text_color = if matches!(status, button::Status::Disabled) {
        MUTED
    } else {
        theme.palette().text
    };
    style
}
fn right_panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgb8(24, 27, 31).into()),
        ..Default::default()
    }
}
fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(PANEL.into()),
        ..Default::default()
    }
}
fn card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(PANEL.into()),
        border: iced::Border {
            color: BORDER,
            width: 1.,
            radius: 8.into(),
        },
        ..Default::default()
    }
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
        background: Some(Color { a: 0.18, ..color }.into()),
        border: iced::Border {
            radius: 3.into(),
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
    const COLORS: [Color; 8] = [
        Color::from_rgb8(197, 193, 112),
        Color::from_rgb8(137, 190, 146),
        Color::from_rgb8(118, 188, 194),
        Color::from_rgb8(139, 170, 221),
        Color::from_rgb8(184, 151, 217),
        Color::from_rgb8(216, 145, 177),
        Color::from_rgb8(221, 158, 122),
        Color::from_rgb8(199, 181, 137),
    ];
    // Fixed FNV-1a keeps the same path's color stable across runs.
    let hash = path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    COLORS[(hash % COLORS.len() as u64) as usize]
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

// Filter before producing application messages: even a no-op update rebuilds the UI.
fn window_events() -> Subscription<Message> {
    iced::event::listen_with(|event, _, _| match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed { .. })
        | iced::Event::Window(window::Event::Resized(_) | window::Event::CloseRequested) => {
            Some(Message::Event(event))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        session
            .entries
            .push(Entry::new(EntryKind::User, "Keep this transcript"));
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
            ["gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"]
        );
    }

    #[tokio::test]
    async fn retired_model_recovery_retries_failed_selection_without_losing_notice() {
        let mut app = app();
        let h = app.harness.as_ref().unwrap().clone();
        let mut session = Session::new(PathBuf::from("/recovery"), &app.catalog.settings);
        session.model = "retired-model".into();
        session.effort = "low".into();
        session.fast = true;
        let id = session.id.clone();
        h.store.save_session(&session).unwrap();
        let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
        for (name, trigger) in [
            ("fail_draft", "BEFORE INSERT ON new_session_draft"),
            ("fail_model", "BEFORE UPDATE ON sessions"),
        ] {
            db.execute_batch(&format!(
                "CREATE TRIGGER {name} {trigger} BEGIN SELECT RAISE(FAIL,'fixture failure'); END;"
            ))
            .unwrap();
            dispatch(&mut app, Message::Select(id.clone())).await;
            assert!(app.error.as_ref().unwrap().contains("fixture failure"));
            assert!(app.selected.is_none());
            assert!(app.notice.is_none());
            assert_eq!(h.store.load_session(&id).unwrap().model, "retired-model");
            assert_eq!(h.get(&id).unwrap().snapshot().model, "retired-model");
            db.execute_batch(&format!("DROP TRIGGER {name};")).unwrap();
        }
        dispatch(&mut app, Message::ErrorDismiss).await;
        dispatch(&mut app, Message::Select(id.clone())).await;
        assert_eq!(app.selected.as_ref(), Some(&id));
        assert!(app.error.is_none());
        assert!(app.notice.as_ref().unwrap().0.contains("retired-model"));
        let saved = h.store.load_session(&id).unwrap();
        assert_eq!(saved.model, "gpt-6.1-sol");
        assert_eq!(saved.effort, "medium");
        assert!(!saved.fast);
        // Deterministic expiry, without sleeping or an always-on timer.
        app.notice.as_mut().unwrap().1 = std::time::Instant::now() - Duration::from_secs(9);
        dispatch(&mut app, Message::NoticeTick).await;
        assert!(app.notice.is_none());
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
            .filter(|e| e.kind == EntryKind::User)
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
        db.execute_batch("CREATE TRIGGER fail_draft BEFORE INSERT ON new_session_draft BEGIN SELECT RAISE(FAIL,'fixture draft failure'); END;").unwrap();
        dispatch(&mut app, Message::SaveDraft).await;
        assert!(app.new_draft_dirty);
        assert_eq!(app.editor.text(), message);
        assert!(
            app.error
                .as_ref()
                .unwrap()
                .contains("fixture draft failure")
        );
        db.execute_batch("DROP TRIGGER fail_draft;").unwrap();
        dispatch(&mut app, Message::SaveDraft).await;
        assert!(!app.new_draft_dirty);
        assert_eq!(h.store.load_new_draft().unwrap().message, message);

        db.execute_batch("CREATE TRIGGER fail_session BEFORE INSERT ON sessions BEGIN SELECT RAISE(FAIL,'fixture session failure'); END;").unwrap();
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
        db.execute_batch("DROP TRIGGER fail_session;").unwrap();
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
                s.entries.push(Entry::new(EntryKind::User, "Old message"));
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
        child
            .entries
            .push(Entry::new(EntryKind::User, "Inspect without edits"));
        child
            .entries
            .push(Entry::new(EntryKind::Assistant, "Final inspection result"));
        let child_id = child.id.clone();
        h.store.save_session(&child).unwrap();
        let run = RunInfo {
            run_id: pico_core::store::id(),
            session_id: child_id.clone(),
            parent_id: parent.clone(),
            request_id: "fixture-spawn".into(),
            profile: Profile::Explore,
            title: child.title.clone(),
            status: RunStatus::Completed,
            assignment: 1,
            started: pico_core::store::now() - 17,
            finished: Some(pico_core::store::now()),
            notified: true,
        };
        let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
        db.execute(
            "INSERT INTO subagent_runs VALUES (?1,?2,?3,?4,?5,?6,NULL)",
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
        dispatch(&mut app, Message::Select(child_id.clone())).await;
        assert_eq!(app.parent_id(), Some(parent.as_str()));
        assert!(
            app.sidebar_rows
                .iter()
                .filter_map(|row| match row {
                    SidebarRow::Session(i) => Some(&app.sessions[*i]),
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
            for label in ["Agents", "Completed", "Open", "Stop"] {
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
        iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ))
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
            let view = pointer_input(
                text_editor(&app.editor)
                    .on_action(Message::Editor)
                    .key_binding(composer_key_binding)
                    .into(),
                false,
                Default::default(),
            );
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
            assert!(matches!(messages.as_slice(), [Message::DeletePreviousWord]));
            messages
        };
        for message in messages {
            let _ = app.update(message);
        }
    }

    #[tokio::test]
    async fn composer_word_deletion_preserves_suffix_selection_and_draft_saving() {
        let mut app = app();
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
                assert!(
                    app.draft_dirty,
                    "word deletion must be saved like other edits"
                );
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

    fn click_header(
        ui: &mut iced_runtime::UserInterface<'_, Message, Theme, iced::Renderer>,
        renderer: &mut iced::Renderer,
        x: f32,
    ) -> Vec<Message> {
        let mut messages = Vec::new();
        ui.update(
            &[
                iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(iced::Point::new(x, 12.)),
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
        fn label(&self, value: &str) -> iced::Rectangle {
            self.labels
                .iter()
                .find(|(text, _)| text == value)
                .unwrap_or_else(|| panic!("missing label {value}"))
                .1
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
    async fn footer_stays_at_chat_bottom_and_controls_fit_with_both_sidebars() {
        let mut app = app();
        dispatch(&mut app, Message::Create(PathBuf::from("/footer-project"))).await;
        app.git = Some(crate::git::Info::Changes {
            branch: "long-feature-branch-name".into(),
            added: 12345,
            removed: 678,
        });
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
            for label in [
                &app.current().unwrap().model,
                &app.current().unwrap().effort,
            ] {
                let control = bounds.label(label);
                assert!(
                    control.y >= height - 36.,
                    "footer must not take a fill-height share: {control:?}"
                );
                assert!(control.x >= left + 5. && control.x + control.width <= width - right - 55.);
                assert!(
                    control.width >= 14.,
                    "control must remain clickable: {control:?}"
                );
            }
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
                    .any(|b| b.x == width - 50. && b.height == height)
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
                assert!(added.y >= height - 36. && removed.y >= height - 36.);
                assert!(added.x + added.width <= removed.x);
                assert!(removed.x + removed.width <= width - right - 55.);
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
                option.width >= footer_text_width("gpt-6-luna", 12.),
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
            click_header(&mut ui, &mut renderer, 12.)
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
                iced::Size::new(346., 24.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            assert!(matches!(
                click_header(&mut ui, &mut renderer, 12.).as_slice(),
                [Message::ToggleSearch]
            ));
            assert!(matches!(
                click_header(&mut ui, &mut renderer, 334.).as_slice(),
                [Message::New]
            ));
            let messages = click_header(&mut ui, &mut renderer, 173.);
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
                if bounds.size() == iced::Size::new(16., 16.) {
                    self.containers.push(bounds);
                }
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
            iced::Size::new(160., 24.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        assert!(
            matches!(click_header(&mut ui, &mut renderer, 38.).as_slice(), [Message::Create(p)] if p == Path::new("/project-0"))
        );
        ui.update(
            &[iced::Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: -1000., y: 0. },
            })],
            mouse::Cursor::Available(iced::Point::new(80., 12.)),
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Vec::new(),
        );
        assert!(
            matches!(click_header(&mut ui, &mut renderer, 122.).as_slice(), [Message::Create(p)] if p == Path::new("/project-11"))
        );
        assert!(matches!(
            click_header(&mut ui, &mut renderer, 12.).as_slice(),
            [Message::ToggleSearch]
        ));
        assert!(matches!(
            click_header(&mut ui, &mut renderer, 148.).as_slice(),
            [Message::New]
        ));
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
                iced::Size::new(346., 24.),
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
            assert!(matches!(
                click_header(&mut ui, &mut renderer, 12.).as_slice(),
                [Message::ToggleSearch]
            ));
            messages.into_iter().next().unwrap()
        };
        dispatch(&mut app, input).await;
        let visible: Vec<_> = app
            .sidebar_rows
            .iter()
            .filter_map(|r| match r {
                SidebarRow::Session(i) => Some(app.sessions[*i].title.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(visible, ["Zulu notes"]);
        app.sidebar_list.offset = 100.;
        dispatch(&mut app, Message::ToggleSearch).await;
        assert!(!app.search_active);
        assert!(app.search.is_empty());
        assert_eq!(app.sidebar_list.offset, 0.);
        assert_eq!(
            app.sidebar_rows
                .iter()
                .filter(|r| matches!(r, SidebarRow::Session(_)))
                .count(),
            2
        );
        dispatch(&mut app, Message::ToggleSearch).await;
        dispatch(&mut app, Message::Search("BETA".into())).await;
        assert_eq!(
            app.sidebar_rows
                .iter()
                .filter(|r| matches!(r, SidebarRow::Session(_)))
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
                .filter(|r| matches!(r, SidebarRow::Session(_)))
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
        h.edit(&id, |s| s.entries[0].text.push('*')).unwrap();
        dispatch(&mut app, Message::Changed).await;
        // The closing '*' reparses the block to just 'é'. Byte 1 is now inside
        // its UTF-8 encoding, so retaining that anchor would panic on Copy.
        assert!(app.chat_selection.borrow().anchor.is_none());
        assert!(!app.chat_selection.borrow().dragging);
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
        assert_eq!(app.transcript.total(), before + 120.);
        assert!(app.markdown.len() < 100);
        let _ = app.view();
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
                    parent: None,
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
        let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap();
        dispatch(&mut app, event).await;
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
        db.execute_batch("CREATE TRIGGER fail_save BEFORE UPDATE ON sessions BEGIN SELECT RAISE(FAIL,'fixture disk failure'); END;").unwrap();
        dispatch(&mut app, Message::SaveDraft).await;
        assert_eq!(app.editor.text(), "a");
        assert!(app.draft_dirty);
        assert!(app.error.as_ref().unwrap().contains("fixture disk failure"));
        db.execute_batch("DROP TRIGGER fail_save;").unwrap();
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
