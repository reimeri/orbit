//! Session sidebar: row preparation, rendering, and visible-checkout branch polling.
use super::*;
use crate::session_wave;
use iced::widget::column;
use orbit_core::agent::ConversationStatus;

#[derive(Clone)]
pub(super) enum SidebarRow {
    Heading(&'static str, usize),
    Session(usize, SessionStatus),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollapsedSection {
    Working,
    Deferred,
    Settled,
}

impl CollapsedSection {
    pub(super) const ALL: [Self; 3] = [Self::Working, Self::Deferred, Self::Settled];
    pub(super) const FILED: [Self; 2] = [Self::Deferred, Self::Settled];

    pub(super) fn from_filing(kind: FilingKind) -> Self {
        match kind {
            FilingKind::Deferred => Self::Deferred,
            FilingKind::Settled => Self::Settled,
        }
    }

    pub(super) fn scroll_id(self) -> &'static str {
        match self {
            Self::Working => "working-sessions",
            Self::Deferred => "deferred-sessions",
            Self::Settled => "settled-sessions",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::Deferred => "Deferred",
            Self::Settled => "Settled",
        }
    }
}

pub(super) struct CollapsedList {
    pub(super) rows: Vec<usize>,
    pub(super) list: crate::virtual_list::List,
    pub(super) open: bool,
    pub(super) search_open: bool,
}

impl CollapsedList {
    pub(super) fn new() -> Self {
        Self {
            rows: vec![],
            list: crate::virtual_list::List::with_overscan(COLLAPSED_ROW_HEIGHT),
            open: false,
            search_open: true,
        }
    }
}

const SETTLE_FONT_SIZE: f32 = 12.;
pub(super) const SESSION_METADATA_GAP: f32 = 6.;
const SIDEBAR_VERTICAL_INSET: f32 = 4.;
const SIDEBAR_CONTENT_PADDING: f32 = crate::session_action::RIGHT;
pub(super) const COLLAPSED_ROW_HEIGHT: f32 =
    crate::session_action::HEIGHT + 2. * crate::session_action::PADDING;
pub(super) const COLLAPSED_ROW_GAP: f32 = 3.;
pub(super) const COLLAPSED_VISIBLE_ROWS: usize = 10;

impl App {
    pub(super) fn session_status(&self, s: &SessionSummary) -> SessionStatus {
        let h = self.harness.as_ref();
        let c = h.and_then(|h| h.get(&s.id));
        let children = self.child_statuses.get(&s.id).copied().unwrap_or_default();
        if s.has_async_question
            || c.as_ref()
                .is_some_and(|c| c.questionnaires.as_ref().is_some_and(|q| q.has_pending()))
        {
            SessionStatus::Question
        } else if c.as_ref().is_some_and(|c| c.is_turn_active()) || children.running > 0 {
            SessionStatus::Working
        } else if children.needs_attention
            || s.interrupted
            || s.needs_attention
            || c.as_ref()
                .is_some_and(|c| c.status() == ConversationStatus::NeedsAttention)
        {
            SessionStatus::Error
        } else if s.unchecked_completion {
            SessionStatus::Unchecked
        } else {
            SessionStatus::Idle
        }
    }
    fn session_age_label(&self, session: &SessionSummary, status: SessionStatus) -> String {
        let children = self
            .child_statuses
            .get(&session.id)
            .copied()
            .unwrap_or_default();
        let age = age(session.updated);
        if children.running > 0 {
            format!("● {} · {age}", children.running)
        } else if children.needs_attention || status == SessionStatus::Question {
            format!("! · {age}")
        } else {
            age
        }
    }

    pub(super) fn prepare_sidebar(&mut self) {
        let needle = self.search.to_lowercase();
        let mut sections = [vec![], vec![], vec![]];
        for section in &mut self.collapsed {
            section.rows.clear();
        }
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
            if let Some(filing) = s.filing {
                self.collapsed[CollapsedSection::from_filing(filing.kind) as usize]
                    .rows
                    .push(index);
                continue;
            }
            let status = self.session_status(s);
            if status == SessionStatus::Working && self.catalog.settings.hide_working_sessions {
                self.collapsed[CollapsedSection::Working as usize]
                    .rows
                    .push(index);
                continue;
            }
            sections[status.section()].push(SidebarRow::Session(index, status));
        }
        self.sidebar_rows.clear();
        for (name, section) in ["NEEDS ATTENTION", "WORKING", "SESSIONS"]
            .into_iter()
            .zip(sections)
        {
            if !section.is_empty() {
                self.sidebar_rows
                    .push(SidebarRow::Heading(name, section.len()));
                self.sidebar_rows.extend(section);
            }
        }
        for kind in CollapsedSection::ALL {
            let section = &mut self.collapsed[kind as usize];
            if kind != CollapsedSection::Working {
                section.rows.sort_unstable_by_key(|&i| {
                    let s = &self.sessions[i];
                    (std::cmp::Reverse(s.filing.expect("filed row").at), &s.id)
                });
            }
            let follow_top = section.list.offset <= 1.;
            section.list.reset(section.rows.iter().map(|&index| {
                let key = *self
                    .sidebar_keys
                    .entry(self.sessions[index].id.clone())
                    .or_insert_with(|| {
                        let key = self.next_sidebar_key;
                        self.next_sidebar_key += 1;
                        key
                    });
                (key, COLLAPSED_ROW_HEIGHT + COLLAPSED_ROW_GAP)
            }));
            if follow_top {
                section.list.offset = 0.;
            }
            if kind != CollapsedSection::Working || self.catalog.settings.hide_working_sessions {
                self.tasks.push(widget::operation::scroll_to(
                    kind.scroll_id(),
                    scrollable::AbsoluteOffset {
                        x: 0.,
                        y: section.list.offset,
                    },
                ));
            }
        }
        self.sidebar_epoch = self.sidebar_epoch.wrapping_add(1);
        self.sidebar_list.reset(self.sidebar_rows.iter().map(|row| {
            match row {
                SidebarRow::Heading(name, _) => (
                    match *name {
                        "WORKING" => 0,
                        "NEEDS ATTENTION" => 1,
                        _ => 2,
                    },
                    36.,
                ),
                SidebarRow::Session(index, _) => (
                    *self
                        .sidebar_keys
                        .entry(self.sessions[*index].id.clone())
                        .or_insert_with(|| {
                            let key = self.next_sidebar_key;
                            self.next_sidebar_key += 1;
                            key
                        }),
                    53.,
                ),
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

    pub(super) fn sidebar_header(&self) -> Element<'_, Message> {
        let icon: Element<_> = if self.search_active {
            Icon::X.view(14.)
        } else {
            Icon::Search.view(14.)
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
                    theme::button(project_badge_named(&p.path, &p.name()))
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
                theme::button(container(Icon::Plus.view(16.)).center_x(18).center_y(24))
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

    pub(super) fn sidebar(&self) -> Element<'_, Message> {
        let colors = &self.preset().colors;
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
                SidebarRow::Heading(title, count) => container(
                    text(format!("{title}  {count}"))
                        .size(10)
                        .style(theme::muted),
                )
                .padding([10., SIDEBAR_CONTENT_PADDING])
                .into(),
                SidebarRow::Session(session_index, status) => {
                    let session = &self.sessions[*session_index];
                    let active = self.parent_id() == Some(session.id.as_str());
                    const BADGE_SIZE: f32 = 16.;
                    let (mut project, badge) =
                        self.session_project_label(&session.project, BADGE_SIZE);
                    if let Some(branch) = self.sidebar_branches.get(&session.project) {
                        project.push_str(" · ");
                        project.push_str(branch);
                    }
                    // Age restarts on accepted input and successful completion, not streaming.
                    let status = *status;
                    let age = self.session_age_label(session, status);
                    let project_line = |leading, trailing: Element<'static, Message>| {
                        row![
                            leading,
                            ellipsized_text(&project, 11., Some(colors.muted)),
                            trailing
                        ]
                        .spacing(SESSION_METADATA_GAP)
                        .align_y(iced::Center)
                    };
                    let metadata: Element<'_, Message> = if status == SessionStatus::Idle {
                        // The row-local hover widget paints the rest of this line.
                        container(badge)
                            .center_y(crate::session_action::HEIGHT)
                            .into()
                    } else {
                        let trailing: Element<'_, Message> = if status == SessionStatus::Unchecked {
                            container(container(space().width(8).height(8)).style(move |_| {
                                container::Style::default()
                                    .background(colors.accent)
                                    .border(iced::Border {
                                        radius: 4.into(),
                                        ..Default::default()
                                    })
                            }))
                            .width(12)
                            .center_y(crate::session_action::HEIGHT)
                            .into()
                        } else {
                            crate::session_action::age(age.clone())
                        };
                        project_line(badge, trailing).into()
                    };
                    let phase_offset = match (&self.launch, status) {
                        (crate::Launch::Marketing(_), SessionStatus::Working) => 0.125,
                        (crate::Launch::Marketing(_), SessionStatus::Question) => 0.4,
                        _ => 0.,
                    };
                    let content = mouse_area(
                        theme::button(session_wave::wave(
                            container(
                                column![ellipsized_text(&session.title, 13., None), metadata]
                                    .spacing(3),
                            )
                            .padding([crate::session_action::PADDING, SIDEBAR_CONTENT_PADDING])
                            .width(Fill),
                            status,
                            phase_offset,
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
                    if status == SessionStatus::Idle {
                        // Fit the project label separately beside the age and the controls.
                        // Switching between the two layouts needs only a redraw.
                        let line =
                            |trailing| project_line(space().width(BADGE_SIZE).into(), trailing);
                        let actions = row![
                            crate::session_action::control(
                                container(Icon::Clock.view(SETTLE_FONT_SIZE))
                                    .center_x(crate::session_action::HEIGHT)
                                    .center_y(crate::session_action::HEIGHT),
                                Message::FileSession(
                                    session.id.clone(),
                                    FilingAction::File(FilingKind::Deferred)
                                ),
                                "Defer session",
                            ),
                            crate::session_action::control(
                                row![
                                    Icon::Check.view(SETTLE_FONT_SIZE),
                                    text("Settle").size(SETTLE_FONT_SIZE)
                                ]
                                .spacing(4)
                                .align_y(iced::Center),
                                Message::FileSession(
                                    session.id.clone(),
                                    FilingAction::File(FilingKind::Settled)
                                ),
                                "Settle session",
                            )
                        ]
                        .spacing(crate::session_action::GAP)
                        .align_y(iced::Center);
                        crate::session_action::row(
                            content,
                            line(crate::session_action::age(age)),
                            line(actions.into()),
                        )
                    } else {
                        content.into()
                    }
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
                        .style(theme::muted),
                )
                .padding([0., SIDEBAR_CONTENT_PADDING])
                .into(),
            ));
        }
        let rows = crate::keyed_column::animated_column(rows);
        let sessions = scroll(rows)
            .id("sessions")
            .height(Fill)
            .on_scroll(move |v| {
                Message::SidebarScroll(epoch, v.absolute_offset().y, v.bounds().height)
            });
        list = list.push(self.session_list_focus.list(sessions));
        let sections = self.collapsed_sections();
        let collapsed_sections =
            widget::keyed_column(sections.iter().enumerate().map(|(i, &kind)| {
                (
                    kind,
                    self.collapsed_section(kind, i == 0, i + 1 == sections.len()),
                )
            }));
        container(list.push(collapsed_sections))
            .padding(iced::Padding {
                top: SIDEBAR_VERTICAL_INSET,
                right: theme::SCROLLBAR_EDGE_INSET,
                bottom: SIDEBAR_VERTICAL_INSET,
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

    pub(super) fn collapsed_sections(&self) -> &'static [CollapsedSection] {
        if self.catalog.settings.hide_working_sessions {
            &CollapsedSection::ALL
        } else {
            &CollapsedSection::FILED
        }
    }

    pub(super) fn collapsed_expanded(&self, kind: CollapsedSection) -> bool {
        if self.search.is_empty() {
            self.collapsed[kind as usize].open
        } else {
            self.collapsed[kind as usize].search_open
        }
    }

    pub(super) fn collapsed_list_visible(&self, kind: CollapsedSection) -> bool {
        self.collapsed_expanded(kind) && !self.collapsed[kind as usize].rows.is_empty()
    }

    pub(super) fn collapsed_list_height(&self, kind: CollapsedSection) -> f32 {
        // Each list gets the same cap whether or not siblings are expanded.
        // Unused space belongs to the main list, not to a sibling section.
        let cap = self.window_height * 0.45 / self.collapsed_sections().len() as f32;
        self.collapsed[kind as usize]
            .list
            .total()
            .min((COLLAPSED_ROW_HEIGHT + COLLAPSED_ROW_GAP) * COLLAPSED_VISIBLE_ROWS as f32)
            .min(cap)
    }

    fn collapsed_section(
        &self,
        kind: CollapsedSection,
        first: bool,
        last: bool,
    ) -> Element<'_, Message> {
        let collapsed = &self.collapsed[kind as usize];
        let expanded = self.collapsed_expanded(kind);
        let visible = self.collapsed_list_visible(kind);
        // Keep adjacent headers close without moving the hit target of a section below.
        let header_padding = iced::Padding::from([8., SIDEBAR_CONTENT_PADDING])
            .top(if first { 8. } else { SIDEBAR_VERTICAL_INSET })
            .bottom(if last || visible {
                8.
            } else {
                SIDEBAR_VERTICAL_INSET
            });
        let header = theme::button(
            row![
                text(kind.label()).size(12).width(Fill),
                text(collapsed.rows.len().to_string()).size(11),
                crate::chevron_icon::chevron_icon(expanded),
            ]
            .spacing(10)
            .align_y(iced::Center),
        )
        .width(Fill)
        .padding(header_padding)
        .style(sidebar_control)
        .on_press(Message::ToggleCollapsed(kind));
        let mut section = column![header];
        if visible {
            let range = collapsed.list.visible();
            let mut rows = vec![(
                usize::MAX,
                space().height(collapsed.list.prefix(range.start)).into(),
            )];
            for index in range.clone() {
                let session = &self.sessions[collapsed.rows[index]];
                let active = self.parent_id() == Some(session.id.as_str());
                let (_, badge) = self.session_project_label(&session.project, 16.);
                let row: Element<'_, Message> = if kind == CollapsedSection::Working {
                    let age = self.session_age_label(session, SessionStatus::Working);
                    let phase_offset = match self.launch {
                        crate::Launch::Marketing(_) => 0.125,
                        _ => 0.,
                    };
                    mouse_area(
                        theme::button(session_wave::wave(
                            container(
                                row![
                                    badge,
                                    ellipsized_text(&session.title, 13., None),
                                    crate::session_action::age(age),
                                ]
                                .spacing(6)
                                .align_y(iced::Center),
                            )
                            .padding([crate::session_action::PADDING, SIDEBAR_CONTENT_PADDING])
                            .width(Fill)
                            .height(COLLAPSED_ROW_HEIGHT),
                            SessionStatus::Working,
                            phase_offset,
                        ))
                        .padding(0)
                        .width(Fill)
                        .style(if active {
                            secondary
                        } else {
                            theme::text_button
                        })
                        .on_press(Message::Select(session.id.clone())),
                    )
                    .on_right_press(Message::Press(Press::SessionMenu(session.id.clone())))
                    .into()
                } else {
                    let age = age(session.filing.expect("filed row").at);
                    let trailing_width = text_width(&age, crate::session_action::AGE_SIZE)
                        .ceil()
                        .max(crate::session_action::UNDO_SIZE);
                    let content = mouse_area(
                        theme::button(
                            row![
                                badge,
                                ellipsized_text(&session.title, 13., None),
                                crate::session_action::slot(trailing_width),
                            ]
                            .spacing(6)
                            .align_y(iced::Center),
                        )
                        .padding([crate::session_action::PADDING, SIDEBAR_CONTENT_PADDING])
                        .width(Fill)
                        .height(COLLAPSED_ROW_HEIGHT)
                        .style(if active {
                            secondary
                        } else {
                            theme::text_button
                        })
                        .on_press(Message::Select(session.id.clone())),
                    )
                    .on_right_press(Message::Press(Press::SessionMenu(session.id.clone())));
                    crate::session_action::row(
                        content,
                        crate::session_action::age(age),
                        crate::session_action::control(
                            Icon::Undo2.view(crate::session_action::UNDO_SIZE),
                            Message::FileSession(session.id.clone(), FilingAction::Undo),
                            match kind {
                                CollapsedSection::Deferred => "Undo deferral",
                                CollapsedSection::Settled => "Unsettle session",
                                CollapsedSection::Working => unreachable!(),
                            },
                        ),
                    )
                };
                rows.push((
                    collapsed.list.order[index],
                    column![row, space().height(COLLAPSED_ROW_GAP)].into(),
                ));
            }
            rows.push((
                usize::MAX - 1,
                space()
                    .height((collapsed.list.total() - collapsed.list.prefix(range.end)).max(0.))
                    .into(),
            ));
            let epoch = self.sidebar_epoch;
            let sessions = scroll(crate::keyed_column::keyed_column(rows))
                .id(kind.scroll_id())
                .height(self.collapsed_list_height(kind))
                .on_scroll(move |v| {
                    Message::CollapsedScroll(kind, epoch, v.absolute_offset().y, v.bounds().height)
                });
            section = section.push(self.session_list_focus.list(sessions));
            if !last {
                section = section.push(space().height(SIDEBAR_VERTICAL_INSET));
            }
        }
        section.into()
    }
}

pub(super) fn branch_subscription(path: &Path) -> Subscription<Message> {
    Subscription::run_with(path.to_owned(), |path| {
        let path = path.clone();
        iced::stream::channel(1, async move |mut output| {
            let mut previous = None;
            loop {
                let branch =
                    tokio::time::timeout(Duration::from_secs(5), crate::git::sidebar_branch(&path))
                        .await;
                if let Ok(Ok(branch)) = branch
                    && previous.as_ref() != Some(&branch)
                {
                    previous = Some(branch.clone());
                    if output
                        .send(Message::SidebarBranch(path.clone(), branch))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        })
    })
}
