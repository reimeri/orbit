//! Message actions and the family panel are separate from subagent navigation.
use super::*;
use iced::widget::column;

pub(super) const ACTION_GAP: f32 = 8.;
const ACTION_WIDTH: f32 = 26. - crate::scrollbar::GUTTER;

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
        if entry.is_none() && self.fork_panel.take().is_some() {
            self.view_epoch = self.view_epoch.wrapping_add(1);
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
        self.fork_panel = Some(panel);
        self.agents_open = false;
        self.view_epoch = self.view_epoch.wrapping_add(1);
        let close_settings = if self.settings_open {
            self.update_inner(Message::CancelSettings)
        } else {
            Task::none()
        };
        Task::batch([
            close_settings,
            widget::operation::scroll_to(
                "fork-list",
                scrollable::AbsoluteOffset {
                    x: 0.,
                    y: (index as f32) * (CARD_HEIGHT + CARD_GAP),
                },
            ),
        ])
    }
    pub(super) fn fork_row<'a>(
        &'a self,
        session: &'a Session,
        entry: &'a Entry,
        projected: ActivityRow,
    ) -> Element<'a, Message> {
        let content = self.activity_row(projected);
        let padded =
            container(content).padding(iced::Padding::ZERO.right(ACTION_GAP + ACTION_WIDTH));
        if !matches!(projected, ActivityRow::Entry(i) if session.can_fork(i)) {
            return padded.into();
        }
        let sid = &session.id;
        let eid = &entry.id;
        let menu_open = self.modal.is_none()
            && matches!(&self.context_menu, Some(ContextMenu::Prompt(s,e)) if s == sid && e == eid);
        let compact = self.window_width
            - self
                .catalog
                .settings
                .sidebar_width
                .min(self.window_width * 0.4)
            - 60.
            - if self.right_panel_open() {
                self.panel_width()
            } else {
                0.
            }
            < 400.;
        const MENU_WIDTH: f32 = 28.;
        let trigger_width = if compact { MENU_WIDTH } else { ACTION_WIDTH };
        let button = theme::button(
            container(text("⋯").size(16))
                .center_x(trigger_width)
                .center_y(MENU_WIDTH),
        )
        .padding(0)
        .on_press_maybe((!menu_open).then(|| Message::PromptMenu(sid.clone(), eid.clone())))
        .style(theme::text_button);
        let mut trigger = stack![container(button).align_right(MENU_WIDTH)];
        if menu_open {
            let action = theme::button(container(Icon::GitFork.view(16.)).center(MENU_WIDTH - 4.))
                .padding(0)
                .on_press(Message::Fork(sid.clone(), eid.clone()))
                .style(theme::text_button);
            let menu = column![action].padding(2);
            // Float relative to the trigger, not the click. The scrollable supplies
            // the translated bounds even when the prompt moves during reflow.
            trigger = trigger.push(widget::float(opaque(menu)).translate(
                move |bounds, viewport| {
                    let below = bounds.height + 3.;
                    let dy = if bounds.y + below + bounds.height <= viewport.y + viewport.height {
                        below
                    } else {
                        -below
                    };
                    iced::Vector::new((MENU_WIDTH - trigger_width) / 2., dy)
                },
            ));
        }
        let slot = if compact {
            container(trigger).align_right(Fill)
        } else {
            container(trigger)
                .align_right(Fill)
                .padding(iced::Padding::ZERO.top(10))
        };
        let row: Element<'a, Message> = if compact {
            column![padded, slot].spacing(ACTION_GAP).into()
        } else if menu_open {
            stack![padded, slot].into()
        } else {
            widget::hover(padded, slot)
        };
        if let Some(n) = self.forks.descendants.get(sid).and_then(|m| m.get(eid)) {
            column![
                row,
                theme::button(Icon::GitFork.label(
                    text(format!("{n} {}", if *n == 1 { "fork" } else { "forks" })),
                    12.
                ))
                .padding([2, 8])
                .style(theme::text_button)
                .on_press(Message::ShowForks(Some(eid.clone())))
            ]
            .spacing(4)
            .into()
        } else {
            row
        }
    }

    pub(super) fn fork_separator<'a>(
        &'a self,
        origin: &'a pico_core::store::ForkOrigin,
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
            container(line)
                .center_y(Fill)
                .padding(iced::Padding::ZERO.right(ACTION_GAP + ACTION_WIDTH))
                .into()
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
    pub(super) fn jump_to_prompt(&mut self, entry: &str) -> Task<Message> {
        let Some(index) = self.current().and_then(|s| s.entries.index_of(entry)) else {
            return Task::none();
        };
        let Some(pos) = self.transcript.order.iter().position(|&id| id == index) else {
            return Task::none();
        };
        let offset = self.transcript.prefix(pos);
        self.auto_scroll = false;
        self.transcript.offset = offset;
        self.prepare_visible();
        widget::operation::scroll_to(
            "chat",
            scrollable::AbsoluteOffset {
                x: 0.,
                y: offset + 20.,
            },
        )
    }
}
