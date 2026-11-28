use super::*;
use crate::git::{
    Info,
    changes::{File, Files, Line, Patch, Status},
};
use iced::widget::column;

const FILE_PAGE: usize = 50;
const LINE_PAGE: usize = 100;

#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    pub project: Option<PathBuf>,
    files: Option<Arc<Files>>,
    expanded: HashMap<PathBuf, Detail>,
    sequence: u64,
    page: usize,
}
#[derive(Default)]
struct Detail {
    request: u64,
    task: Option<iced::task::Handle>,
    reload: bool,
    page: usize,
    result: Option<Result<Arc<Patch>, String>>,
}
impl State {
    pub(super) fn clear(&mut self) {
        self.project = None;
        self.files = None;
        self.expanded.clear();
        self.page = 0;
    }
    pub(super) fn close(&mut self) {
        self.open = false;
        self.clear()
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Toggle,
    File(PathBuf),
    Snapshot(PathBuf, Option<Info>, Option<Arc<Files>>),
    Patch(PathBuf, u64, Result<Arc<Patch>, String>),
    Page(Option<PathBuf>, usize),
}
fn msg(a: Action) -> Message {
    Message::Changes(a)
}

impl App {
    pub(super) fn store_git(&mut self, path: PathBuf, info: Option<Info>) {
        if !self.git_pending.remove(&path) && !self.current().is_some_and(|s| s.project == path) {
            return;
        }
        if let Some(info) = info {
            self.git.insert(path, info);
        } else {
            self.git.remove(&path);
        }
    }
    pub(super) fn changes_action(&mut self, action: Action) -> Task<Message> {
        match action {
            Action::Toggle if self.changes.open => {
                self.changes.close();
                self.view_epoch = self.view_epoch.wrapping_add(1);
            }
            Action::Toggle => {
                self.worktrees.panel = false;
                self.worktrees.popup = None;
                self.agents_open = false;
                self.fork_panel = None;
                if self.settings_open {
                    let close = self.update_inner(Message::CancelSettings);
                    self.tasks.push(close);
                }
                self.changes.open = true;
                self.view_epoch = self.view_epoch.wrapping_add(1);
                return self.refresh_git();
            }
            Action::Snapshot(path, info, files) => {
                let active = self.changes.open && self.current().is_some_and(|s| s.project == path);
                self.store_git(path.clone(), info);
                if !active {
                    return Task::none();
                }
                self.changes.project = Some(path);
                let previous = std::mem::replace(&mut self.changes.files, files);
                if let Some(files) = &self.changes.files {
                    self.changes.page = self
                        .changes
                        .page
                        .min(files.entries.len().saturating_sub(1) / FILE_PAGE);
                    self.changes
                        .expanded
                        .retain(|p, _| files.entries.contains_key(p));
                    let mut reload = Vec::new();
                    for (path, detail) in &mut self.changes.expanded {
                        let file = &files.entries[path];
                        let changed = file.gitlink
                            || !previous.as_ref().is_some_and(|prev| {
                                prev.root == files.root
                                    && prev.base == files.base
                                    && prev.entries.get(path) == Some(file)
                            });
                        if detail.task.is_some() {
                            detail.reload |= changed;
                        } else if changed || detail.result.as_ref().is_some_and(Result::is_err) {
                            reload.push(path.clone());
                        }
                    }
                    return Task::batch(reload.into_iter().map(|p| self.load_patch(p)));
                }
            }
            Action::File(path) => {
                if self.changes.expanded.remove(&path).is_none()
                    && self
                        .changes
                        .files
                        .as_ref()
                        .is_some_and(|f| f.entries.contains_key(&path))
                {
                    self.changes
                        .expanded
                        .insert(path.clone(), Detail::default());
                    return self.load_patch(path);
                }
            }
            Action::Patch(path, id, result) => {
                if let Some(d) = self.changes.expanded.get_mut(&path)
                    && d.request == id
                {
                    if let Ok(patch) = &result
                        && let Patch::Rows(lines) = patch.as_ref()
                    {
                        d.page = d.page.min(lines.len().saturating_sub(1) / LINE_PAGE)
                    }
                    d.task = None;
                    d.result = Some(result);
                    if std::mem::take(&mut d.reload) && self.changes.files.is_some() {
                        return self.load_patch(path);
                    }
                }
            }
            Action::Page(path, n) => {
                if let Some(p) = path {
                    if let Some(d) = self.changes.expanded.get_mut(&p) {
                        d.page = n;
                    }
                } else {
                    self.changes.page = n
                }
            }
        }
        Task::none()
    }
    fn load_patch(&mut self, path: PathBuf) -> Task<Message> {
        let files = self
            .changes
            .files
            .as_ref()
            .expect("open file from snapshot");
        let (root, base, file) = (
            files.root.clone(),
            files.base.clone(),
            files.entries[&path].clone(),
        );
        self.changes.sequence += 1;
        let id = self.changes.sequence;
        let detail = self.changes.expanded.get_mut(&path).expect("expanded file");
        detail.request = id;
        let (task, handle) = Task::future(async move {
            static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
            let _permit = SLOTS.acquire().await.expect("patch semaphore never closes");
            let result = crate::git::changes::patch(&root, &base, &path, &file)
                .await
                .map(Arc::new)
                .map_err(|e| format!("{e:#}; will retry automatically."));
            msg(Action::Patch(path, id, result))
        })
        .abortable();
        detail.task = Some(handle.abort_on_drop());
        task
    }
    pub(super) fn changes_panel(&self) -> Element<'_, Message> {
        let colors = &self.preset().colors;
        let mut content = column![right_panel_header("Changes", msg(Action::Toggle))].spacing(12);
        let Some(s) = self.current() else {
            return self
                .panel_container(content.push(text("Select a project to see changes.").size(13)));
        };
        let project = self.session_project_label(&s.project, 16.).0;
        let label = match self.git_info() {
            Some(Info::Changes { branch, .. }) => format!("{project} / {branch}"),
            _ => project,
        };
        let card_edge = iced::Padding::ZERO.right(crate::scrollbar::GUTTER);
        content = content
            .push(
                row![
                    Icon::GitFork.colored(14., colors.muted),
                    ellipsized_text(&label, 12., Some(colors.muted))
                ]
                .spacing(6)
                .align_y(iced::Center),
            )
            .push(container(widget::rule::horizontal(1)).padding(card_edge));
        let Some(files) = &self.changes.files else {
            let notice = if self.changes.project.as_ref() != Some(&s.project) {
                "Loading changes…"
            } else if let Some(Info::Unavailable(error)) = self.git_info() {
                error
            } else {
                "Not a Git checkout."
            };
            return self.panel_container(content.push(text(notice).size(13).style(theme::muted)));
        };
        if let Some(Info::Changes { unreadable, .. }) = self.git_info()
            && *unreadable > 0
        {
            content = content.push(
                text(format!(
                    "Partial line counts: {unreadable} unreadable file(s)."
                ))
                .size(12),
            );
        }
        if let Some(Info::Changes { added, removed, .. }) = self.git_info() {
            content = content.push(
                row![
                    text(format!("CHANGED FILES · {}", files.entries.len()))
                        .size(12)
                        .style(theme::muted)
                        .width(Fill),
                    counts(*added, *removed, colors)
                ]
                .align_y(iced::Center)
                .padding(card_edge),
            );
        }
        if files.entries.is_empty() {
            return self.panel_container(
                content.push(text("No changed files.").size(13).style(theme::muted)),
            );
        }
        let mut rows = column![].spacing(8);
        for (path, file) in files
            .entries
            .iter()
            .skip(self.changes.page * FILE_PAGE)
            .take(FILE_PAGE)
        {
            rows = rows.push(self.changed_file(path, file));
        }
        if files.entries.len() > FILE_PAGE {
            content = content.push(pager(
                None,
                self.changes.page,
                files.entries.len().div_ceil(FILE_PAGE),
            ));
        }
        self.panel_container(content.push(scroll(rows).height(Fill)))
    }
    fn changed_file<'a>(&'a self, path: &'a Path, file: &'a File) -> Element<'a, Message> {
        let colors = &self.preset().colors;
        let detail = self.changes.expanded.get(path);
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        let mut title = row![crate::ellipsized_text::shrinking_ellipsized_text(
            &name,
            14.,
            Some(colors.text)
        )]
        .width(Fill)
        .spacing(6)
        .align_y(iced::Center);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            title = title.push(ellipsized_text(
                &parent.to_string_lossy(),
                12.,
                Some(colors.muted),
            ));
        }

        let (letter, ink) = match file.status {
            Status::Modified => ("M", colors.accent),
            Status::Added | Status::Untracked => ("A", colors.success),
            Status::Deleted => ("R", colors.danger),
            Status::TypeChanged => ("T", colors.warning),
            Status::Unmerged => ("U", colors.warning),
            Status::Recreated => ("C", colors.accent),
            Status::Unreadable => ("?", colors.warning),
        };
        let surface = iced::theme::palette::mix(colors.panel, ink, theme::BADGE_ALPHA);
        let badge = |glyph: Element<'a, Message>| {
            container(glyph)
                .center_x(20)
                .center_y(20)
                .style(move |_| container::Style {
                    background: Some(surface.into()),
                    text_color: Some(theme::contrasting_ink(ink, surface)),
                    border: iced::border::rounded(theme::RADIUS_SMALL),
                    ..Default::default()
                })
        };
        let expanded = detail.is_some();
        let chevron = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let inset = 8.;
        let header = row![
            badge(theme::primary_label(letter).size(12).into()),
            title,
            counts(file.added, file.removed, colors)
        ]
        .width(Fill)
        .padding(inset)
        .spacing(7)
        .align_y(iced::Center);
        // Hover the entire header, not just the badge. Only its glyph changes;
        // measurement and the single expand/collapse hit target stay the same.
        let indicator = container(badge(chevron.view(12.)))
            .padding(iced::Padding::ZERO.left(inset))
            .center_y(Fill);
        let button = theme::button(widget::hover(header, indicator))
            .width(Fill)
            .padding(0)
            .style(move |t, status| {
                if expanded {
                    theme::option_button(t, status, true)
                } else {
                    theme::text_button(t, status)
                }
            })
            .on_press(msg(Action::File(path.to_owned())));
        let button = tooltip(
            button,
            text(format!("{} · {}", file.status.label(), path.display())).size(12),
            tooltip::Position::Top,
        )
        .delay(Duration::from_millis(650))
        .style(theme::footer_surface);
        let mut card_rows = column![button];
        if let Some(d) = detail {
            let body: Element<_> = match &d.result {
                None => text("Loading diff…").size(12).into(),
                Some(Err(e)) => text(e).size(12).color(colors.danger).into(),
                Some(Ok(patch)) => match patch.as_ref() {
                    Patch::Notice(n) => text(n).size(12).style(theme::muted).into(),
                    Patch::Saved { path, bytes } => column![
                        text(format!("Complete patch · {bytes} bytes")).size(12),
                        text(path.to_string_lossy()).size(11).style(theme::muted),
                        theme::button(text("Open patch").size(12))
                            .on_press(Message::Link(path.display().to_string()))
                    ]
                    .spacing(4)
                    .into(),
                    Patch::Rows(lines) => self.patch_rows(path, lines, d.page),
                },
            };
            card_rows = card_rows.push(container(body).padding(4).width(Fill));
        }
        container(card_rows).style(card).width(Fill).into()
    }
    fn patch_rows<'a>(
        &self,
        path: &Path,
        lines: &'a [Line],
        page_index: usize,
    ) -> Element<'a, Message> {
        let colors = &self.preset().colors;
        let available = (self.panel_width()
            - RIGHT_PANEL_PADDING
            - theme::SCROLLBAR_EDGE_INSET
            - crate::scrollbar::GUTTER
            - 12.)
            .max(1.);
        let gutter = |label: String, width: f32| {
            text(label)
                .size(10)
                .font(iced::Font::MONOSPACE)
                .width(width)
                .align_x(iced::Right)
                .color(colors.muted)
        };
        let page_index = page_index.min(lines.len().saturating_sub(1) / LINE_PAGE);
        let page = &lines[(page_index * LINE_PAGE)..lines.len().min((page_index + 1) * LINE_PAGE)];
        let max = page
            .iter()
            .filter_map(|l| {
                if let Line::Code { old, new, .. } = l {
                    (*old).max(*new)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0) as u64;
        let w = (max.to_string().len() as f32 * 7. + 5.).max(25.);
        let mut code = column![row![gutter("OLD".into(), w), gutter("NEW".into(), w)].spacing(2)]
            .width(iced::Shrink);
        for line in page {
            let element: Element<_> = match line {
                Line::Gap(n) => row![
                    container(widget::rule::horizontal(1)).width(w * 2.),
                    text(format!("{n} unchanged lines"))
                        .size(11)
                        .color(colors.muted),
                    widget::rule::horizontal(1)
                ]
                .spacing(8)
                .align_y(iced::Center)
                .width(available)
                .into(),
                Line::Note(n) => text(n).size(11).style(theme::muted).into(),
                Line::Code {
                    old,
                    new,
                    text: source,
                    highlight,
                } => {
                    let (sign, ink) = if old.is_none() {
                        ("+", colors.success)
                    } else if new.is_none() {
                        ("−", colors.danger)
                    } else {
                        (" ", colors.muted)
                    };
                    let source = if let Some(range) = highlight {
                        widget::rich_text::<(), _, _, _>(vec![
                            widget::span(&source[..range.start]),
                            widget::span(&source[range.clone()]).background(ink.scale_alpha(0.32)),
                            widget::span(&source[range.end..]),
                        ])
                        .font(iced::Font::MONOSPACE)
                        .size(12)
                        .wrapping(text::Wrapping::None)
                        .into()
                    } else {
                        Element::from(
                            text(source)
                                .font(iced::Font::MONOSPACE)
                                .size(12)
                                .wrapping(text::Wrapping::None),
                        )
                    };
                    let row = row![
                        gutter(old.map(|n| n.to_string()).unwrap_or_default(), w),
                        gutter(new.map(|n| n.to_string()).unwrap_or_default(), w),
                        text(sign)
                            .size(12)
                            .width(13)
                            .align_x(iced::Center)
                            .color(ink),
                        source
                    ]
                    .spacing(2)
                    .align_y(iced::Center);
                    container(column![space().width(available).height(1), row].width(iced::Shrink))
                        .padding([2, 0])
                        .style(move |_| {
                            container::Style::default().background(if sign == " " {
                                colors.background
                            } else {
                                ink.scale_alpha(0.12)
                            })
                        })
                        .into()
                }
            };
            code = code.push(element);
        }
        let mut block = column![
            crate::scrollbar::horizontal(container(code).width(iced::Shrink).padding(2))
                .width(Fill)
        ];
        if lines.len() > LINE_PAGE {
            block = block.push(pager(
                Some(path),
                page_index,
                lines.len().div_ceil(LINE_PAGE),
            ))
        }
        block.into()
    }
}
fn counts<'a>(added: u64, removed: u64, colors: &theme::Colors) -> Element<'a, Message> {
    row![
        text(format!("+{added}")).size(12).color(colors.success),
        text(format!("−{removed}")).size(12).color(colors.danger)
    ]
    .spacing(5)
    .into()
}
fn pager(path: Option<&Path>, n: usize, pages: usize) -> Element<'static, Message> {
    let action = |n| msg(Action::Page(path.map(Path::to_owned), n));
    row![
        theme::button(Icon::ChevronUp.view(14.))
            .style(theme::text_button)
            .on_press_maybe(n.checked_sub(1).map(action)),
        text(format!("Page {} of {pages}", n + 1)).size(11),
        theme::button(Icon::ChevronDown.view(14.))
            .style(theme::text_button)
            .on_press_maybe((n + 1 < pages).then(|| action(n + 1)))
    ]
    .spacing(8)
    .align_y(iced::Center)
    .into()
}
