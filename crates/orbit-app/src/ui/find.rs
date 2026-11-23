use super::*;
use crate::conversation_find::{Frame, Index, Paint, Results};
use iced::{
    Rectangle,
    advanced::widget::{Id, Operation, operation::Scrollable},
    time::Instant,
};
use std::{ops::Range, rc::Rc, sync::Mutex};

const INPUT: &str = "conversation-find";
const SPINNER_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub enum Action {
    Open,
    Query(String),
    Step(bool), // backwards
    Close,
    Finished(u64, Result<Option<Arc<Results>>, String>),
    Located {
        token: u64,
        target: Rectangle,
        chat: Rectangle,
        inner: Rectangle,
    },
}

pub(super) struct Find {
    session: String,
    query: String,
    count: String,
    spinner_at: Option<Instant>,
    index: Arc<Mutex<Index>>,
    busy: Option<(u64, CancellationToken)>,
    results: Option<Arc<Results>>,
    active: Option<usize>,
    reveal: Option<u64>,
    error: Option<String>,
    pub(super) frame: Rc<Frame>,
}

impl Drop for Find {
    fn drop(&mut self) {
        if let Some((_, cancel)) = &self.busy {
            cancel.cancel();
        }
    }
}

impl Find {
    // Keep the displayed count distinct from navigable results: a replacement
    // query invalidates the old hits immediately, but not the last count.
    fn select_match(&mut self, active: Option<usize>, total: usize) {
        self.active = active;
        self.count = active.map_or_else(
            || "No matches".to_owned(),
            |n| format!("{} of {total}", n + 1),
        );
    }

    fn position(&self) -> Option<Position> {
        Some(self.results.as_ref()?.at(self.active?).0)
    }
    pub(super) fn paint(&self, index: usize, entry: &Entry) -> Paint {
        let hits = self.results.as_ref().and_then(|r| r.hits(index, entry));
        let active = hits.as_ref().and_then(|_| {
            let (pos, hit) = self.results.as_ref()?.at(self.active?);
            (pos.entry == index).then_some(hit)
        });
        Paint {
            frame: Some(self.frame.clone()),
            hits,
            active,
            reveal: self.reveal,
        }
    }
}

impl App {
    pub(super) fn chat_range(&self) -> Range<usize> {
        let range = self.transcript.visible();
        if let Some(find) = &self.find
            && find.reveal.is_some()
            && let Some(at) = find.position()
            && let Some(pos) = self.transcript.position(at.entry)
            && !range.contains(&pos)
        {
            self.transcript
                .visible_at(self.transcript.prefix(pos), self.transcript.viewport)
        } else {
            range
        }
    }

    pub(super) fn find_sync(&mut self) {
        let Some(find) = &mut self.find else { return };
        let Some(session) = self
            .current_session
            .as_ref()
            .filter(|s| s.id == find.session)
        else {
            self.find = None;
            return;
        };
        if find.busy.is_some()
            || find.query.is_empty()
            || find.error.is_some()
            || find
                .results
                .as_ref()
                .is_some_and(|r| r.source.ptr_eq(&session.entries))
        {
            return;
        }
        let source = session.entries.clone();
        let query = find.query.clone();
        let previous = find.results.clone();
        let index = find.index.clone();
        let cancel = CancellationToken::new();
        self.find_serial += 1;
        let serial = self.find_serial;
        find.busy = Some((serial, cancel.clone()));
        find.spinner_at
            .get_or_insert_with(|| Instant::now() + SPINNER_DELAY);
        self.tasks.push(Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    index
                        .lock()
                        .expect("find worker")
                        .search(source, previous.as_deref(), &query, &cancel)
                        .map(|r| r.map(Arc::new))
                })
                .await
                .map_err(|e| e.to_string())?
            },
            move |result| Message::Find(Action::Finished(serial, result)),
        ));
    }

    pub(super) fn find_action(&mut self, action: Action) -> Task<Message> {
        if matches!(action, Action::Open) {
            let Some(session) = self.current_session.as_ref() else {
                return Task::none();
            };
            if self.modal.is_some()
                || self.settings_open
                || self.context_menu.is_some()
                || self.switching_session
            {
                return Task::none();
            }
            self.histories.active.separate();
            self.file_completion.dismiss();
            self.find.get_or_insert_with(|| Find {
                session: session.id.clone(),
                query: String::new(),
                count: String::new(),
                spinner_at: None,
                index: Default::default(),
                busy: None,
                results: None,
                active: None,
                reveal: None,
                error: None,
                frame: Default::default(),
            });
            self.auto_scroll = false;
            return widget::operation::focus(INPUT).chain(widget::operation::select_all(INPUT));
        }
        let Some(find) = &mut self.find else {
            return Task::none();
        };
        match action {
            Action::Open => unreachable!(),
            Action::Close => {
                self.find = None;
                self.prepare_visible();
                if let Some(id) = &self.active_questionnaire {
                    let id = id.clone();
                    return self.questionnaires[&id]
                        .focus()
                        .map(move |m| Message::Questionnaire(id.clone(), m));
                }
                return widget::operation::focus("composer");
            }
            Action::Query(query) => {
                if let Some((_, cancel)) = &find.busy {
                    cancel.cancel()
                }
                find.spinner_at = (!query.is_empty()).then(|| Instant::now() + SPINNER_DELAY);
                if query.is_empty() {
                    find.count.clear();
                }
                find.query = query;
                find.results = None;
                find.active = None;
                find.reveal = None;
                find.error = None;
                self.prepare_visible();
                self.find_sync();
            }
            Action::Finished(serial, result) => {
                if find.busy.as_ref().is_none_or(|(id, _)| *id != serial) {
                    return Task::none();
                }
                let (_, cancel) = find.busy.take().unwrap();
                if !cancel.is_cancelled() {
                    find.spinner_at = None;
                    match result {
                        Ok(Some(results)) => {
                            let initial = find.results.is_none();
                            let old = find.position();
                            let (first, last) = old
                                .map(|at| (at, at))
                                .or(find.frame.visible.get())
                                .unwrap_or_else(|| {
                                    let at = |y| Position {
                                        entry: self
                                            .transcript
                                            .order
                                            .get(self.transcript.at(y))
                                            .map_or(results.source.len(), |&i| {
                                                ActivityRow::from_id(i).source()
                                            }),
                                        block: 0,
                                        byte: 0,
                                    };
                                    (
                                        at(self.transcript.offset),
                                        at(self.transcript.offset + self.transcript.viewport),
                                    )
                                });
                            find.select_match(results.nearest(first, last), results.len());
                            find.results = Some(results);
                            if initial && find.active.is_some() {
                                self.find_serial += 1;
                                find.reveal = Some(self.find_serial);
                            } else if old != find.position() {
                                // A streaming reparse may remove the current occurrence.
                                // Update the counter, but do not take over scrolling.
                                find.reveal = None;
                            }
                            self.prepare_visible();
                        }
                        Ok(None) => (),
                        Err(e) => find.error = Some(e),
                    }
                }
                self.find_sync();
            }
            Action::Step(back) => {
                let Some(n) = find.active else {
                    return Task::none();
                };
                let len = find.results.as_ref().unwrap().len();
                find.select_match(
                    Some(if back {
                        (n + len - 1) % len
                    } else {
                        (n + 1) % len
                    }),
                    len,
                );
                self.find_serial += 1;
                find.reveal = Some(self.find_serial);
                self.auto_scroll = false;
                self.prepare_visible();
                return widget::operation::focus(INPUT);
            }
            Action::Located {
                token,
                target,
                chat,
                inner,
            } => {
                if find.reveal != Some(token) {
                    return Task::none();
                }
                let delta = |start: f32, extent: f32, edge: f32, room: f32| {
                    let end = start + extent.min(room);
                    if start < edge - 0.5 || end > edge + room + 0.5 {
                        start - edge - (room - extent).max(0.) / 2.
                    } else {
                        0.
                    }
                };
                let y = delta(target.y, target.height, chat.y, chat.height);
                let x = delta(target.x, target.width, inner.x, inner.width);
                if y == 0. && x == 0. {
                    find.reveal = None;
                    self.prepare_visible();
                } else {
                    return iced_runtime::task::effect(iced_runtime::Action::widget(Reveal {
                        chat,
                        inner,
                        x,
                        y,
                    }));
                }
            }
        }
        Task::none()
    }
}

impl Find {
    pub(super) fn popup(&self) -> Element<'_, Message> {
        let count = match &self.error {
            Some(error) => text(format!("Find failed: {error}")),
            None => text(&self.count),
        };
        let icon = match self.spinner_at {
            Some(at) => Icon::Search.loader_after(15., at),
            None => Icon::Search.view(15.),
        };
        let button = |icon: Icon, label: &'static str, action: Option<Action>| {
            tooltip(
                theme::button(icon.view(16.))
                    .padding(4)
                    .on_press_maybe(action.map(Message::Find))
                    .style(theme::text_button),
                text(label).size(12),
                tooltip::Position::Bottom,
            )
            .style(card)
        };
        let popup = container(
            row![
                icon,
                text_input("Find in conversation", &self.query)
                    .id(INPUT)
                    .size(14)
                    .padding([3, 6])
                    .style(theme::search_input)
                    .on_input(|s| Message::Find(Action::Query(s))),
                count.size(12),
                button(
                    Icon::ChevronUp,
                    "Previous (Shift+Enter)",
                    self.active.map(|_| Action::Step(true))
                ),
                button(
                    Icon::ChevronDown,
                    "Next (Enter)",
                    self.active.map(|_| Action::Step(false))
                ),
                button(Icon::X, "Close (Esc)", Some(Action::Close))
            ]
            .spacing(4)
            .align_y(iced::Center),
        )
        .padding([4, 6])
        .max_width(370)
        .style(card);
        // Only the popup captures pointer events; its alignment space is transparent.
        container(opaque(popup))
            .padding(iced::Padding::new(8.).right(crate::scrollbar::GUTTER + 8.))
            .align_right(Fill)
            .into()
    }
}

// Nested code fences and wide tables have anonymous horizontal scrollables.
// The widget supplies its immediate viewport in content coordinates on redraw.
struct Reveal {
    chat: Rectangle,
    inner: Rectangle,
    x: f32,
    y: f32,
}
impl Operation for Reveal {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self)
    }
    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content: Rectangle,
        translation: iced::Vector,
        state: &mut dyn Scrollable,
    ) {
        let viewport = bounds + translation;
        if self.y != 0. && id == Some(&"chat".into()) && viewport == self.chat {
            state.scroll_by(
                scrollable::AbsoluteOffset { x: 0., y: self.y },
                bounds,
                content,
            );
        }
        if self.x != 0. && viewport == self.inner && content.width > bounds.width {
            state.scroll_by(
                scrollable::AbsoluteOffset { x: self.x, y: 0. },
                bounds,
                content,
            );
        }
    }
}
