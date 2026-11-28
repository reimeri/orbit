//! Message actions and the family panel are separate from subagent navigation.
use super::*;
use iced::widget::column;

const CONTROLS_HEIGHT: f32 = 26.;

#[derive(Default)]
pub(super) struct Index {
    families: HashMap<String, Vec<(usize, usize)>>,
    descendants: HashMap<String, HashMap<String, usize>>,
}
impl Index {
    pub(super) fn build(sessions: &[SessionSummary]) -> Self {
        let mut index = Self::default();
        let mut members = HashMap::<&str, Vec<usize>>::new();
        for (i, s) in sessions.iter().enumerate() {
            if let Some(o) = &s.origin {
                members.entry(&o.family).or_default().push(i);
                *index
                    .descendants
                    .entry(o.parent.clone())
                    .or_default()
                    .entry(o.entry.clone())
                    .or_default() += 1;
            }
        }
        for (i, s) in sessions.iter().enumerate() {
            if let Some(family) = members.get_mut(s.id.as_str()) {
                family.push(i);
            }
        }
        // Cache the preorder together with the summaries; streaming and scrolling
        // should not rebuild a tree or scan unrelated sessions on each frame.
        for (family, mut members) in members {
            // Order siblings by fork point, not by sidebar activity. A new reply
            // in a background branch must not move cards under the pointer.
            members.sort_unstable_by_key(|&i| {
                (
                    sessions[i].origin.as_ref().map(|o| o.inherited),
                    &sessions[i].id,
                )
            });
            let ids: HashSet<_> = members.iter().map(|&i| sessions[i].id.as_str()).collect();
            let mut children = HashMap::<&str, Vec<usize>>::new();
            let mut stack = Vec::new();
            for &i in members.iter().rev() {
                if let Some(o) = &sessions[i].origin
                    && ids.contains(o.parent.as_str())
                {
                    children.entry(&o.parent).or_default().push(i);
                } else {
                    stack.push((0, i));
                }
            }
            let mut rows = Vec::with_capacity(members.len());
            while let Some((depth, i)) = stack.pop() {
                rows.push((depth, i));
                if let Some(list) = children.get(sessions[i].id.as_str()) {
                    stack.extend(list.iter().map(|&j| (depth + 1, j)));
                }
            }
            index.families.insert(family.to_owned(), rows);
        }
        index
    }
}
const CARD_HEIGHT: f32 = 64.;
const CARD_GAP: f32 = 6.;

#[derive(PartialEq, Eq)]
pub(super) struct Panel {
    pub(super) family: String,
    source: Option<(String, String)>,
}
impl Panel {
    fn highlights(&self, s: &SessionSummary) -> bool {
        self.source.as_ref().is_some_and(|(parent, entry)| {
            s.origin
                .as_ref()
                .is_some_and(|o| &o.parent == parent && &o.entry == entry)
        })
    }
}
impl App {
    pub(super) fn fork_family(&self) -> Option<&str> {
        let s = self.current_session.as_ref()?;
        let family = s.origin.as_ref().map_or(&s.id, |o| &o.family);
        self.forks
            .families
            .get(family)
            .is_some_and(|rows| rows.len() > 1)
            .then_some(family)
    }
    pub(super) fn show_forks(&mut self, entry: Option<String>) -> Task<Message> {
        if entry.is_none() && matches!(self.right_panel, Some(RightPanel::Forks(_))) {
            self.set_right_panel(None);
            return Task::none();
        }
        let Some(family) = self.fork_family() else {
            return Task::none();
        };
        let panel = Panel {
            family: family.to_owned(),
            source: entry.map(|e| (self.selected.clone().expect("stored session"), e)),
        };
        let rows = &self.forks.families[family];
        let index = rows
            .iter()
            .position(|&(_, i)| {
                let s = &self.sessions[i];
                if panel.source.is_some() {
                    panel.highlights(s)
                } else {
                    Some(&s.id) == self.selected.as_ref()
                }
            })
            .unwrap_or(0);
        self.set_right_panel(Some(RightPanel::Forks(panel)));
        widget::operation::scroll_to(
            "fork-list",
            scrollable::AbsoluteOffset {
                x: 0.,
                y: (index as f32) * (CARD_HEIGHT + CARD_GAP),
            },
        )
    }
    pub(super) fn fork_row<'a>(
        &'a self,
        session: &'a Session,
        entry: &'a Entry,
        projected: ActivityRow,
    ) -> Element<'a, Message> {
        let content = self.activity_row(projected);
        let ActivityRow::Entry(i) = projected else {
            return content;
        };
        let EntryKind::User { sent_at, .. } = entry.kind else {
            return content;
        };
        let prompt = if session.can_fork(i) {
            self.prompt_controls(session, &entry.id, sent_at, content)
        } else {
            content
        };
        // Size against the actual transcript lane, including after a panel resize.
        use iced::Length::FillPortion;
        row![
            space().width(FillPortion(1)),
            container(prompt).align_right(FillPortion(2))
        ]
        .into()
    }

    fn prompt_controls<'a>(
        &'a self,
        session: &'a Session,
        eid: &str,
        sent_at: u64,
        content: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let sid = &session.id;
        let mut controls = row![].align_y(iced::Center).height(CONTROLS_HEIGHT);
        if let Some(n) = self.forks.descendants.get(sid).and_then(|m| m.get(eid)) {
            controls = controls.push(
                theme::button(Icon::GitFork.label(
                    text(format!("{n} {}", if *n == 1 { "fork" } else { "forks" })),
                    12.,
                ))
                .padding([2, 8])
                .style(theme::text_button)
                .on_press(Message::ShowForks(Some(eid.to_owned()))),
            );
        }
        let fork = theme::button(container(Icon::GitFork.view(16.)).center(CONTROLS_HEIGHT))
            .padding(0)
            .style(theme::sidebar_control)
            .on_press_maybe((!self.io_busy).then(|| Message::Fork(sid.clone(), eid.to_owned())));
        let time: Element<'_, Message> = if let Ok(seconds) = i64::try_from(sent_at)
            && let Some(utc) = chrono::DateTime::from_timestamp(seconds, 0)
        {
            let local = utc
                .with_timezone(&chrono::Local)
                .format("%A %-I:%M %p")
                .to_string();
            container(crate::ellipsized_text::shrinking_ellipsized_text(
                &local,
                12.,
                Some(self.preset().colors.muted),
            ))
            .align_right(Fill)
            .padding(iced::Padding::ZERO.right(4))
            .into()
        } else {
            space().width(Fill).into()
        };
        controls = controls.push(time).push(icon_tip(fork, "Fork from here"));

        // The base includes the controls' space, so hovering either part reveals
        // them without changing the measured row height or covering message text.
        container(widget::hover(
            column![content, space().height(CONTROLS_HEIGHT)]
                .width(Fill)
                .align_x(iced::Right),
            container(controls).align_bottom(Fill),
        ))
        .padding(iced::Padding::ZERO.top(CONTROLS_HEIGHT))
        .into()
    }

    pub(super) fn fork_separator<'a>(
        &'a self,
        origin: &'a orbit_core::store::ForkOrigin,
    ) -> Element<'a, Message> {
        let parent = self.sessions.iter().find(|s| s.id == origin.parent);
        let colors = &self.preset().colors;
        widget::responsive(move |size| {
            let mut line = row![Icon::GitFork.colored(14., colors.muted)]
                .spacing(7)
                .align_y(iced::Center);
            if size.width >= 180. {
                line = line.push(text("Forked here").size(12).color(colors.muted));
            }
            line = line.push(widget::rule::horizontal(1));
            if let Some(s) = parent {
                let mut link = row![Icon::ArrowUpRight.view(13.)]
                    .spacing(4)
                    .align_y(iced::Center);
                if size.width >= 240. {
                    link = link.push(text("View parent").size(12));
                }
                line = line.push(
                    tooltip(
                        theme::button(link)
                            .padding([2, 4])
                            .style(theme::text_button)
                            .on_press_maybe((!self.io_busy).then(|| {
                                Message::ViewOriginal(origin.parent.clone(), origin.entry.clone())
                            })),
                        text(format!("View prompt in {}", s.title)).size(12),
                        tooltip::Position::Top,
                    )
                    .style(card),
                );
            } else if size.width >= 180. {
                line = line.push(text("Parent deleted").size(11).color(colors.muted));
            } else {
                line = line.push(icon_tip(
                    Icon::Info.colored(12., colors.muted),
                    "Parent deleted",
                ));
            }
            container(line).center_y(Fill).into()
        })
        .height(36)
        .into()
    }
    pub(super) fn forks_panel<'a>(&'a self, panel: &'a Panel) -> Element<'a, Message> {
        let rows = &self.forks.families[&panel.family];
        let count = rows.len();
        let mut items = column![].spacing(CARD_GAP);
        let colors = &self.preset().colors;
        for &(depth, i) in rows {
            let s = &self.sessions[i];
            let current = self.selected.as_ref() == Some(&s.id);
            let highlighted = panel.highlights(s);
            let role = if highlighted {
                "From this prompt"
            } else if s.origin.is_none() {
                "Original"
            } else {
                "Fork"
            };
            let mut top = row![
                Icon::GitFork.view(12.),
                text(role).size(11).color(if highlighted {
                    colors.accent
                } else {
                    colors.muted
                })
            ]
            .spacing(5)
            .align_y(iced::Center);
            if current {
                top = top
                    .push(space().width(Fill))
                    .push(text("Current").size(11).color(colors.accent));
            }
            let mut label = column![top, ellipsized_text(&s.title, 14., None)].spacing(2);
            if let Some(o) = &s.origin {
                label = label.push(ellipsized_text(&o.excerpt, 11., Some(colors.muted)));
            }
            let button = theme::button(label)
                .height(CARD_HEIGHT)
                .width(Fill)
                .padding([4, 8])
                .on_press_maybe((!current && !self.io_busy).then(|| Message::Select(s.id.clone())))
                .style(move |theme, status| {
                    let mut style = crate::theme::card_button(theme, status);
                    if highlighted {
                        style.border.color = crate::theme::colors(theme).accent;
                    }
                    style
                });
            items = items.push(
                container(button).padding(iced::Padding::ZERO.left((depth.min(5) * 10) as f32)),
            );
        }
        self.panel_container(
            column![
                container(right_panel_header("Forks", Message::ShowForks(None)))
                    .padding(iced::Padding::ZERO.right(crate::scrollbar::GUTTER)),
                text(format!("{count} related conversations"))
                    .size(12)
                    .style(theme::muted),
                scroll(items).id("fork-list").height(Fill)
            ]
            .spacing(14),
        )
    }
}
