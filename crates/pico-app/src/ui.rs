use crate::{
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
    store::{Catalog, Entry, EntryKind, Project, Session, SessionSummary, Settings, Store},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone)]
enum SidebarRow {
    Heading(&'static str, usize),
    Session(usize, usize),
}

type PendingLogin = Arc<tokio::sync::Mutex<Option<Login>>>;

const MUTED: Color = Color::from_rgb8(141, 149, 164);
const PANEL: Color = Color::from_rgb8(29, 32, 37);
const BORDER: Color = Color::from_rgb8(48, 53, 61);
const ACCENT: Color = Color::from_rgb8(137, 175, 210);
const EFFORTS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];

pub struct App {
    harness: Option<Arc<Harness>>,
    catalog: Catalog,
    sessions: Vec<SessionSummary>,
    current_session: Option<Session>,
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
    selected: Option<String>,
    search: String,
    project_filter: Option<PathBuf>,
    editor: text_editor::Content,
    markdown: HashMap<String, (Arc<Entry>, markdown::Content)>,
    expanded: HashSet<String>,
    groups: HashSet<String>,
    settings_open: bool,
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
    git: String,
    git_pending: bool,
    git_cost: Duration,
    draft_dirty: bool,
    smoke: bool,
    smoke_stage: usize,
    smoke_passed: Arc<AtomicBool>,
    smoke_dir: Option<PathBuf>,
    window_id: Option<window::Id>,
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
pub enum Message {
    Selected(Result<Option<String>, String>),
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
    ToolOutput(String, tool_output::Event),
    CopyOutput(String),
    New,
    Select(String),
    Create(PathBuf),
    Search(String),
    Filter(PathBuf),
    Favorite(PathBuf),
    Editor(text_editor::Action),
    Send,
    Stop,
    ToggleSettings,
    ApplySettings,
    Model(String),
    Effort(String),
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
    Bottom,
    Link(String),
    Copy(String),
    Login,
    LoginStarted(Result<(PendingLogin, String), String>),
    LoggedIn(Result<String, String>),
    Account(Option<String>),
    CancelLogin,
    Logout,
    Git(String, String, Duration),
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
            let harness = Harness::new(store, sessions).map_err(|e| format!("{e:#}"))?;
            Ok::<_, String>((harness, catalog, warnings))
        })();
        let (harness, catalog, error) = match loaded {
            Ok((h, c, w)) => (Some(h), c, (!w.is_empty()).then(|| w.join("\n"))),
            Err(error) => (
                None,
                Catalog::default(),
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
            selected,
            search: String::new(),
            project_filter: None,
            editor: text_editor::Content::new(),
            markdown: HashMap::new(),
            expanded: HashSet::new(),
            groups: HashSet::new(),
            settings_open: false,
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
            git: String::new(),
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
        if initial.is_some() {
            app.select(initial);
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
        self.current_session.as_ref()
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
            self.current_session = None;
            self.outputs.clear();
            self.markdown.clear();
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
            if self
                .project_filter
                .as_ref()
                .is_some_and(|p| p != &s.project)
                || !needle.is_empty()
                    && !s.title.to_lowercase().contains(&needle)
                    && !s.project.to_string_lossy().to_lowercase().contains(&needle)
            {
                continue;
            }
            let c = self.harness.as_ref().and_then(|h| h.get(&s.id));
            let category = if c.as_ref().is_some_and(|c| c.is_running()) {
                0
            } else if s.interrupted
                || s.needs_attention
                || c.as_ref()
                    .is_some_and(|c| *c.status.lock().expect("status lock") == "Needs attention")
            {
                1
            } else {
                2
            };
            sections[category].push(SidebarRow::Session(index, category));
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
                        72.,
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
    fn save_catalog(&mut self) -> bool {
        self.catalog.selected = self.selected.clone();
        if let Some(h) = &self.harness {
            let h = h.clone();
            let catalog = self.catalog.clone();
            self.tasks.push(self.io.run(move || {
                Message::External(h.store.save_catalog(&catalog).map_err(|e| {
                    format!(
                        "Could not save settings: {e:#}. Fix storage and retry before quitting."
                    )
                }))
            }));
        }
        true
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
        self.editor =
            text_editor::Content::with_text(self.current().map(|s| s.draft.as_str()).unwrap_or(""));
    }
    fn select(&mut self, id: Option<String>) {
        if self.io_busy {
            return;
        }
        let draft = self.take_draft();
        if let Some(h) = &self.harness {
            let h = h.clone();
            self.io_busy = true;
            self.tasks.push(self.io.run(move || {
                Message::Selected(
                    (|| {
                        if let Some((old, draft)) = draft {
                            h.edit(&old, |s| s.draft = draft)?;
                        }
                        if let Some(id) = &id {
                            h.load(id)?;
                        }
                        h.evict_inactive(id.as_deref())?;
                        Ok::<_, pico_core::Error>(id)
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
                    | Message::Send
                    | Message::Select(_)
                    | Message::New
                    | Message::Create(_)
                    | Message::Confirm
                    | Message::SaveDraft
                    | Message::SessionModel(_)
                    | Message::SessionEffort(_)
                    | Message::Favorite(_)
                    | Message::ApplySettings
                    | Message::Font(_)
                    | Message::ChatWidth(_)
                    | Message::Resume(_)
            )
        {
            return Task::none();
        }
        match message {
            Message::Press(_) => unreachable!("press requests are resolved by the root widget"),
            Message::Selected(result) => {
                self.io_busy = false;
                match result {
                    Ok(id) => {
                        self.selected = id;
                        self.context_menu = None;
                        self.auto_scroll = true;
                        self.refresh();
                        self.restore_draft();
                        self.git.clear();
                        self.save_catalog();
                        return bottom();
                    }
                    Err(error) => {
                        self.error = Some(error);
                        self.draft_dirty = true;
                    }
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
                        self.draft_dirty = true;
                    }
                }
            }
            Message::Deleted(id, result) => {
                self.io_busy = false;
                match result {
                    Ok(()) => {
                        if self.selected.as_ref() == Some(&id) {
                            self.draft_dirty = false;
                            self.selected = None;
                            self.current_session = None;
                            self.restore_draft();
                        }
                        self.refresh();
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
                    self.draft_dirty = true;
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
                            let result = git_info(path).await;
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
            Message::New => self.select(None),
            Message::Select(id) => {
                self.select(Some(id));
                return bottom();
            }
            Message::Create(path) => {
                if !self.io_busy
                    && let Some(h) = &self.harness
                {
                    let h = h.clone();
                    let settings = self.catalog.settings.clone();
                    let draft = self.take_draft();
                    self.io_busy = true;
                    self.tasks.push(self.io.run(move || {
                        Message::Selected(
                            (|| {
                                if let Some((id, draft)) = draft {
                                    h.edit(&id, |s| s.draft = draft)?;
                                }
                                let session = Session::new(path, &settings);
                                let id = session.id.clone();
                                h.store.save_session(&session)?;
                                h.insert(session);
                                h.evict_inactive(Some(&id))?;
                                Ok::<_, pico_core::Error>(Some(id))
                            })()
                            .map_err(|e| format!("Could not create session: {e:#}")),
                        )
                    }));
                }
            }
            Message::Search(search) => {
                self.search = search;
                self.sidebar_list.offset = 0.;
                self.prepare_sidebar();
                return widget::operation::snap_to("sessions", scrollable::RelativeOffset::START);
            }
            Message::Filter(path) => {
                self.project_filter = if self.project_filter.as_ref() == Some(&path) {
                    None
                } else {
                    Some(path)
                };
                self.sidebar_list.offset = 0.;
                self.prepare_sidebar();
            }
            Message::Favorite(path) => {
                if let Some(p) = self.catalog.projects.iter_mut().find(|p| p.path == path) {
                    p.favorite = !p.favorite;
                }
                self.save_catalog();
            }
            Message::Editor(action) => {
                if self.io_busy
                    || self.closing
                    || self.close_requested
                    || self.modal.is_some()
                    || self.context_menu.is_some()
                {
                    return Task::none();
                }
                let edit = action.is_edit();
                self.editor.perform(action);
                self.draft_dirty |= edit;
            }
            Message::Send => {
                if self.modal.is_some() || self.context_menu.is_some() {
                    return Task::none();
                }
                if let (Some(h), Some(id)) = (&self.harness, &self.selected) {
                    let message = self.editor.text();
                    if message.trim().is_empty() {
                        return Task::none();
                    }
                    if self.io_busy {
                        return Task::none();
                    }
                    let h = h.clone();
                    let id = id.clone();
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
                        let result = h
                            .send(&id, &submission, message.clone(), settings)
                            .map_err(|e| format!("{e:#}"));
                        Message::Sent(id, message, result)
                    }));
                }
            }
            Message::Stop => {
                if let (Some(h), Some(id)) = (&self.harness, &self.selected)
                    && let Some(s) = h.get(id)
                {
                    s.cancel();
                }
            }
            Message::ToggleSettings => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                self.settings_open = !self.settings_open;
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
                    self.save_catalog();
                }
            }
            Message::Model(value) => self.settings.model = value,
            Message::Effort(value) => self.settings.effort = value,
            Message::Context(value) => self.context_text = value,
            Message::Threshold(value) => self.threshold_text = value,
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
            Message::SessionModel(value) => {
                if !self.running()
                    && let (Some(h), Some(id)) = (&self.harness, &self.selected)
                {
                    let h = h.clone();
                    let id = id.clone();
                    self.tasks.push(self.io.run(move || {
                        Message::Edited(
                            h.edit(&id, |s| s.model = value)
                                .map_err(|e| format!("{e:#}")),
                        )
                    }));
                }
            }
            Message::SessionEffort(value) => {
                if !self.running()
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

            Message::Bottom => {
                self.auto_scroll = true;
                self.transcript.end();
                self.prepare_visible();
                return bottom();
            }
            Message::Link(url) => {
                self.modal = Some(Modal::Link(url));
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
                if self.selected.as_deref() == Some(&id) {
                    self.git = info;
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
                        .is_some_and(|h| h.all().iter().any(|c| c.is_running()))
                    {
                        self.modal = Some(Modal::Quit);
                    } else {
                        return self.quit();
                    }
                }
                iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                        if self.context_menu.take().is_some() {
                        } else if self.modal.is_some() {
                            self.modal = None;
                        } else {
                            self.settings_open = false;
                        }
                    } else if self.modal.is_none()
                        && self.context_menu.is_none()
                        && modifiers.control()
                        && key.as_ref() == keyboard::Key::Character("n")
                    {
                        self.select(None);
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
                    .is_some_and(|h| h.all().iter().any(|s| s.is_running()))
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
                    while h.all().iter().any(|s|s.is_running()) {std::thread::sleep(Duration::from_millis(20));}
                    if let Some((id,draft))=draft {h.edit(&id,|s|s.draft=draft)?;}
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
        if self.selected.is_some() {
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
        if self.draft_dirty {
            subscriptions
                .push(iced::time::every(Duration::from_secs(1)).map(|_| Message::SaveDraft));
        }
        Subscription::batch(subscriptions)
    }
    pub fn view(&self) -> Element<'_, Message> {
        pointer_input(self.content())
    }
    fn content(&self) -> Element<'_, Message> {
        let mut body = row![self.sidebar(), self.divider(false), self.center()].height(Fill);
        if self.settings_open {
            body = body.push(self.divider(true)).push(self.settings_panel());
        }
        let rail = container(
            column![
                button(container(text("+").size(22)).center_x(Fill).center_y(34))
                    .width(Fill)
                    .padding(0)
                    .on_press(Message::New)
                    .style(button::text),
                space().height(Fill),
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
        let mut root = column![body.push(rail), self.footer()].height(Fill);
        if let Some(error) = &self.error {
            root = root.push(
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
        let base: Element<_> = root.into();
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

    fn sidebar(&self) -> Element<'_, Message> {
        let mut list = column![
            row![
                text("PICO").size(20).color(ACCENT),
                space().width(Fill),
                button("+").on_press(Message::New).style(button::text)
            ]
            .align_y(iced::Center),
            text_input("Search sessions or projects…", &self.search)
                .on_input(Message::Search)
                .padding(9)
        ]
        .spacing(14);
        let favorites: Vec<_> = self
            .catalog
            .projects
            .iter()
            .filter(|p| p.favorite)
            .collect();
        if !favorites.is_empty() {
            list = list.push(text("FAVORITES").size(10).color(MUTED));
            for project in favorites {
                let active = self.project_filter.as_ref() == Some(&project.path);
                list = list.push(
                    button(
                        text(format!("★  {}", project.name()))
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    )
                    .on_press(Message::Filter(project.path.clone()))
                    .style(if active { secondary } else { button::text })
                    .width(Fill),
                );
            }
        }
        if let Some(path) = &self.project_filter {
            list = list.push(
                button(
                    text(format!(
                        "×  Clear {} filter",
                        path.file_name()
                            .unwrap_or(path.as_os_str())
                            .to_string_lossy()
                    ))
                    .size(11),
                )
                .on_press(Message::Filter(path.clone()))
                .style(button::text),
            );
        }
        let epoch = self.sidebar_epoch;
        let range = self.sidebar_list.visible();
        let mut rows: Vec<((u64, usize), Element<'_, Message>)> = vec![(
            (epoch, usize::MAX),
            space().height(self.sidebar_list.prefix(range.start)).into(),
        )];
        for index in range.clone() {
            let content: Element<'_, Message> = match &self.sidebar_rows[index] {
                SidebarRow::Heading(title, count) => {
                    container(text(format!("{title}  {count}")).size(10).color(MUTED))
                        .padding([10, 0])
                        .into()
                }
                SidebarRow::Session(session_index, section) => {
                    let session = &self.sessions[*session_index];
                    let section = *section;
                    let active = self.selected.as_ref() == Some(&session.id);
                    let project = session
                        .project
                        .file_name()
                        .unwrap_or(session.project.as_os_str())
                        .to_string_lossy();
                    let marker = if section == 0 {
                        ["·", "•", "●", "•"][(self.tick as usize / 5) % 4]
                    } else if section == 1 {
                        "!"
                    } else {
                        "·"
                    };
                    let age = age(session.updated);
                    let content = mouse_area(
                        button(
                            column![
                                row![
                                    text(marker).color(if section == 0 { ACCENT } else { MUTED }),
                                    text(session.title.clone())
                                        .size(13)
                                        .width(Fill)
                                        .wrapping(text::Wrapping::WordOrGlyph)
                                ]
                                .spacing(7),
                                row![
                                    text(project.to_string())
                                        .size(11)
                                        .color(MUTED)
                                        .width(Fill)
                                        .wrapping(text::Wrapping::WordOrGlyph),
                                    text(age).size(10).color(MUTED)
                                ]
                                .spacing(4)
                            ]
                            .spacing(5),
                        )
                        .padding(10)
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
            let sensor = widget::sensor(content)
                .key((epoch, key))
                .on_show(move |size| Message::SidebarSize(epoch, key, size.height))
                .on_resize(move |size| Message::SidebarSize(epoch, key, size.height));
            rows.push(((epoch, key), column![sensor, space().height(5.)].into()));
        }
        rows.push((
            (epoch, usize::MAX - 1),
            space()
                .height((self.sidebar_list.total() - self.sidebar_list.prefix(range.end)).max(0.))
                .into(),
        ));
        if self.sessions.is_empty() {
            rows.push((
                (epoch, usize::MAX - 2),
                text("Your sessions will appear here.")
                    .size(13)
                    .color(MUTED)
                    .into(),
            ));
        }
        let rows = widget::keyed_column(rows);
        list = list.push(
            scrollable(rows)
                .id("sessions")
                .height(Fill)
                .on_scroll(move |v| {
                    Message::SidebarScroll(epoch, v.absolute_offset().y, v.bounds().height)
                }),
        );
        container(list)
            .padding(14)
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
            let row = widget::sensor(self.entry_view(entry, session))
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
        let editor = text_editor(&self.editor)
            .placeholder(if self.io_busy || self.closing {
                "Saving…"
            } else if running {
                "Steer the agent after its current tool batch…"
            } else {
                "Message Pico…"
            })
            .on_action(Message::Editor)
            .key_binding(|event| {
                if matches!(event.status, text_editor::Status::Focused { .. })
                    && event.key == keyboard::Key::Named(keyboard::key::Named::Enter)
                    && !event.modifiers.shift()
                {
                    Some(text_editor::Binding::Custom(Message::Send))
                } else {
                    text_editor::Binding::from_key_press(event)
                }
            })
            .height(100)
            .padding(12)
            .size(self.catalog.settings.font_size);
        let mut compose = column![editor].spacing(10);
        if running || !self.auto_scroll {
            let mut actions = row![space().width(Fill)].spacing(8).align_y(iced::Center);
            if !self.auto_scroll {
                actions = actions.push(
                    button("↓ Latest")
                        .on_press(Message::Bottom)
                        .style(button::text),
                );
            }
            if running {
                actions = actions.push(
                    button("■ Stop")
                        .on_press(Message::Stop)
                        .style(button::danger),
                );
            }
            compose = compose.push(actions);
        }
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
    fn entry_view<'a>(&'a self, entry: &'a Entry, _session: &'a Session) -> Element<'a, Message> {
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
                        let id = entry.id.clone();
                        block = block.push(
                            view.view(&entry.id)
                                .map(move |event| Message::ToolOutput(id.clone(), event)),
                        );
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
            EntryKind::Notice => container(
                text(&entry.text)
                    .size(12)
                    .color(Color::from_rgb8(224, 186, 120)),
            )
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
                    content = content.push(
                        markdown::view(
                            items.items(),
                            markdown::Settings::with_text_size(
                                self.catalog.settings.font_size,
                                markdown::Style::from_palette(Theme::Dark.palette()),
                            ),
                        )
                        .map(|uri| Message::Link(uri.to_string())),
                    );
                } else {
                    content = content.push(text(&entry.text).size(self.catalog.settings.font_size));
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
        let mut settings = column![
            row![
                text("Settings").size(22),
                space().width(Fill),
                button("×")
                    .on_press(Message::ToggleSettings)
                    .style(button::text)
            ],
            text("ACCOUNT").size(10).color(MUTED)
        ]
        .spacing(14);
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
        settings=settings.push(widget::rule::horizontal(1)).push(text("DEFAULTS FOR NEW SESSIONS").size(10).color(MUTED)).push(text("Model").size(12)).push(text_input("e.g. gpt-5.6-sol",&self.settings.model).on_input(Message::Model).padding(8)).push(text("Reasoning effort").size(12)).push(pick_list(EFFORTS,Some(self.settings.effort.as_str()),|v|Message::Effort(v.into())).width(Fill)).push(text("Context token limit").size(12)).push(text_input("272000",&self.context_text).on_input(Message::Context).padding(8)).push(text("Auto-compact at (%)").size(12)).push(text_input("80",&self.threshold_text).on_input(Message::Threshold).padding(8)).push(text("Set the context limit to your model's usable input window. Compaction preserves the full transcript on disk.").size(11).color(MUTED)).push(button("Apply defaults").on_press(Message::ApplySettings).style(secondary)).push(widget::rule::horizontal(1)).push(text("APPEARANCE").size(10).color(MUTED)).push(text(format!("Chat text · {:.0}px",self.catalog.settings.font_size)).size(12)).push(slider(12.0..=24.0,self.catalog.settings.font_size,Message::Font).step(1_f32)).push(text(format!("Chat width · {:.0}px",self.catalog.settings.chat_width)).size(12)).push(slider(480.0..=1600.0,self.catalog.settings.chat_width,Message::ChatWidth).step(20_f32)).push(checkbox(self.catalog.settings.resume_last).label("Restore last session").on_toggle(Message::Resume)).push(widget::rule::horizontal(1)).push(text("EXECUTION").size(10).color(MUTED)).push(text("Unrestricted shell access\nNo sandbox or command approval prompts. Use projects and instructions you trust.").size(12).color(MUTED));
        if let Some(h) = &self.harness {
            settings = settings.push(
                text(format!("Storage\n{}", h.store.root().display()))
                    .size(11)
                    .color(MUTED),
            );
        }
        container(scrollable(settings))
            .padding(18)
            .width(
                self.catalog
                    .settings
                    .settings_width
                    .min(self.window_width * 0.4),
            )
            .height(Fill)
            .style(panel)
            .into()
    }
    fn footer(&self) -> Element<'_, Message> {
        let mut info = row![].spacing(10).align_y(iced::Center);
        if let Some(session) = self.current() {
            let conversation = self.harness.as_ref().and_then(|h| h.get(&session.id));
            let status = conversation
                .as_ref()
                .map(|c| c.status.lock().expect("status lock").clone())
                .unwrap_or_else(|| "Ready".into());
            let seconds = conversation
                .as_ref()
                .map_or(0., |c| c.duration().as_secs_f64());
            let dot = if conversation.as_ref().is_some_and(|c| c.is_running()) {
                ["◦", "○", "●", "○"][(self.tick as usize / 2) % 4]
            } else {
                "·"
            };
            info = info.push(
                tooltip(
                    text(format!("{dot} {}", shorten(&status, 22)))
                        .size(11)
                        .color(ACCENT),
                    text(status),
                    tooltip::Position::Top,
                )
                .style(card),
            );
            let used = session.usage.input;
            let cache = (session.usage.cached * 100).checked_div(used).unwrap_or(0);
            let rate = conversation.as_ref().map_or(0., |c| {
                if seconds > 0. {
                    session
                        .usage
                        .total_output
                        .saturating_sub(c.output_baseline.load(Ordering::Relaxed))
                        as f64
                        / seconds
                } else {
                    0.
                }
            });
            let details = format!(
                "{seconds:.0}s · {used} / {}k ctx · {} out · {cache}% cache · {rate:.1} tok/s",
                self.catalog.settings.context_limit / 1000,
                session.usage.total_output
            );
            let usage = if self.window_width >= 1450. {
                details.clone()
            } else if self.window_width >= 1200. {
                format!(
                    "{seconds:.0}s · {used} ctx · {} out",
                    session.usage.total_output
                )
            } else if self.window_width >= 1050. {
                format!("{used} ctx")
            } else {
                String::new()
            };
            let show_git = self.window_width >= 1150.;
            let reserved =
                430. + usage.chars().count() as f32 * 6. + if show_git { 150. } else { 0. };
            let path = session.project.display().to_string();
            let path_chars = ((self.window_width - reserved).max(100.) / 6.) as usize;
            info = info.push(
                tooltip(
                    container(
                        text(shorten(&path, path_chars))
                            .size(11)
                            .color(MUTED)
                            .wrapping(text::Wrapping::None),
                    )
                    .clip(true)
                    .width(Fill),
                    text(path),
                    tooltip::Position::Top,
                )
                .style(card),
            );
            if show_git {
                info = info.push(
                    tooltip(
                        text(shorten(&self.git, 24)).size(11).color(MUTED),
                        text(&self.git),
                        tooltip::Position::Top,
                    )
                    .style(card),
                );
            }
            // All usage information stays inspectable on hover, even in a narrow window.
            info = info.push(
                tooltip(
                    text(if usage.is_empty() {
                        "ⓘ".into()
                    } else {
                        usage
                    })
                    .size(10)
                    .color(MUTED),
                    text(details),
                    tooltip::Position::Top,
                )
                .style(card),
            );
            info = info
                .push(
                    pick_list(
                        model_options(&session.model, &self.catalog.settings.model),
                        Some(session.model.clone()),
                        Message::SessionModel,
                    )
                    .text_size(11)
                    .padding(5),
                )
                .push(
                    pick_list(EFFORTS, Some(session.effort.as_str()), |v| {
                        Message::SessionEffort(v.into())
                    })
                    .text_size(11)
                    .padding(5),
                );
        } else {
            info = info
                .push(text("Pico V2").size(11).color(MUTED))
                .push(
                    text("Native Wayland · OpenAI · Code mode")
                        .size(11)
                        .color(MUTED),
                )
                .push(space().width(Fill))
                .push(
                    text(if self.account.is_some() {
                        "ChatGPT connected"
                    } else {
                        "Not signed in"
                    })
                    .size(11)
                    .color(MUTED),
                );
        }
        container(info)
            .padding([8, 14])
            .width(Fill)
            .style(panel)
            .into()
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
                "This permanently deletes the saved conversation. Project files are not deleted. Stop any running turn first.",
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
                self.select(Some(id));
                let _ = self.update_inner(Message::ToggleEntry("smoke-tool".into()));
            }
            2 => self.settings_open = true,
            3 => {
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
                    "Latest reaches history tail"
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
            _ => {
                self.smoke_passed.store(true, Ordering::Relaxed);
                println!(
                    "SMOKE_OK: native Wayland rendered picker, chat, settings, modal, session menu, virtualized large output and 20,000-entry/10,000-session lists (middle/end/reflow/identity)"
                );
                return iced::exit();
            }
        }
        if self.smoke_stage == 2 {
            Task::batch([bottom(), delayed_capture()])
        } else {
            delayed_capture()
        }
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
async fn git_info(path: PathBuf) -> String {
    let mut command = tokio::process::Command::new("git");
    command.kill_on_drop(true);
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        command
            .args([
                "--no-optional-locks",
                "status",
                "--porcelain=v1",
                "--branch",
                "--untracked-files=normal",
            ])
            .current_dir(path)
            .output(),
    )
    .await;
    let result = match result {
        Ok(result) => result,
        Err(_) => return "Git status timed out; will retry automatically".into(),
    };
    match result {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut lines = text.lines();
            let branch = lines
                .next()
                .unwrap_or("")
                .trim_start_matches("## ")
                .split("...")
                .next()
                .unwrap_or("");
            let mut added = 0;
            let mut modified = 0;
            let mut deleted = 0;
            for line in lines {
                let state = line.get(..2).unwrap_or("");
                if state.contains('D') {
                    deleted += 1;
                } else if state == "??" || state.contains('A') {
                    added += 1;
                } else {
                    modified += 1;
                }
            }
            format!("{branch}  +{added} ~{modified} −{deleted}")
        }
        Ok(_) => "Not a Git checkout".into(),
        Err(_) => "Git unavailable".into(),
    }
}

fn model_options(current: &str, default: &str) -> Vec<String> {
    let mut models = [current, default, "gpt-5.6-sol", "gpt-5.4", "gpt-5.3-codex"]
        .map(str::to_owned)
        .to_vec();
    models.sort();
    models.dedup();
    models
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
    fn app() -> App {
        App::new(true, Arc::new(AtomicBool::new(false))).0
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
        dispatch(&mut app, Message::Create(path.clone())).await;
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
    async fn long_transcript_only_caches_viewport_and_appends_preserve_heights() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        dispatch(&mut app, Message::Create(path)).await;
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
    #[test]
    fn model_order_is_stable_unique_and_includes_custom_selection() {
        let expected = vec!["gpt-5.3-codex", "gpt-5.4", "gpt-5.6-sol", "my-model"];
        for _ in 0..10 {
            assert_eq!(model_options("my-model", "gpt-5.4"), expected);
        }
    }
    #[tokio::test]
    async fn session_menu_targets_the_clicked_session_and_releases_deleted_output() {
        let mut app = app();
        let path = app.smoke_dir.clone().unwrap();
        dispatch(&mut app, Message::Create(path.clone())).await;
        let first = app.selected.clone().unwrap();
        dispatch(&mut app, Message::Create(path)).await;
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
        dispatch(&mut app, Message::Create(path)).await;
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
        dispatch(&mut app, Message::Create(path)).await;
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
