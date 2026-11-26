mod gpu;
mod surface;
use super::*;
use iced::widget::column;
use orbit_core::browser::{Frame, MAX_SIDE, Status, address};
use serde_json::{Value, json};

pub(super) struct State {
    pub open: bool,
    pub expanded: bool,
    mounted: Option<String>,
    frame: Option<Frame>,
    status: Status,
    location: String,
    editing: bool,
    find: Option<String>,
    size: Size,
    scale: f32,
    focus: surface::Focus,
    confirm: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            expanded: false,
            mounted: None,
            frame: None,
            status: Status::default(),
            location: String::new(),
            editing: false,
            find: None,
            size: Size::new(960., 720.),
            scale: 1.,
            focus: Default::default(),
            confirm: false,
        }
    }
}
impl State {
    fn dip(&self) -> (u32, u32) {
        let max = (MAX_SIDE as f32 / self.scale).floor();
        (
            self.size.width.round().clamp(1., max) as u32,
            self.size.height.round().clamp(1., max) as u32,
        )
    }
    pub fn page_focused(&self) -> bool {
        self.open && self.focus.0.get()
    }
}
#[derive(Clone, Debug)]
pub enum Action {
    Toggle,
    Data,
    Size(String, Size),
    Scale(f32),
    Location(String),
    Go,
    AddressFocus,
    PageFocus,
    Input(String, Value),
    Nav(&'static str),
    Zoom(i32),
    FindFocus,
    Query(String),
    Next,
    EndFind,
    Control,
    ConfirmProfile,
    Replace,
    Close,
    Expand,
}
fn msg(a: Action) -> Message {
    Message::Browser(a)
}
impl App {
    pub(super) fn sync_browser(&mut self) {
        if self.settings_open
            || self.agents_open
            || self.changes.open
            || self.worktrees.panel
            || self.fork_panel.is_some()
            || self.current_session.is_none()
        {
            self.browser.open = false
        }
        let next = self
            .current_session
            .as_ref()
            .filter(|_| self.browser.open)
            .map(|s| (s.id.clone(), s.project.clone()));
        if self.browser.mounted.as_deref() == next.as_ref().map(|x| x.0.as_str()) {
            return;
        }
        let Some(h) = self.harness.as_ref() else {
            return;
        };
        if let Some(id) = self.browser.mounted.take() {
            h.browser.hide(&id);
        }
        let size = self.browser.size;
        let scale = self.browser.scale;
        self.browser = State {
            open: next.is_some(),
            size,
            scale,
            ..State::default()
        };
        if let Some((id, path)) = next {
            self.browser.mounted = Some(id.clone());
            if let Err(e) = h.browser.show(&id, &path) {
                self.error = Some(format!("Could not open browser: {e:#}"));
            }
            self.refresh_browser();
        }
    }
    pub(super) fn refresh_browser(&mut self) {
        let s = &mut self.browser;
        let (Some(h), Some(id)) = (&self.harness, &s.mounted) else {
            return;
        };
        let status = h.browser.status(id);
        if s.status.url != status.url {
            s.editing = false;
        }
        if !s.editing && s.location != status.url {
            s.location = status.url.clone();
        }
        s.status = status;
        match h.browser.frame(id, s.frame.as_ref().map(|f| f.key)) {
            Ok(Some(f)) => s.frame = Some(f),
            Ok(None) => {}
            Err(e) => s.status.error = Some(e.to_string()),
        }
        if s.status.error.is_some() {
            s.frame = None;
        }
    }
    pub(super) fn browser_action(&mut self, a: Action) -> Task<Message> {
        if matches!(a, Action::Toggle) {
            self.browser.open = !self.browser.open;
            self.changes.close();
            self.worktrees.panel = false;
            self.worktrees.popup = None;
            self.agents_open = false;
            self.fork_panel = None;
            self.view_epoch = self.view_epoch.wrapping_add(1);
            if self.settings_open {
                return self.update_inner(Message::CancelSettings);
            }
            return Task::none();
        }
        if let Action::Scale(scale) = a {
            self.browser.scale = scale;
        }
        let (Some(h), Some(id)) = (&self.harness, self.browser.mounted.clone()) else {
            return Task::none();
        };
        let b = h.browser.clone();
        let op = match a {
            Action::Data => {
                self.refresh_browser();
                return Task::none();
            }
            Action::Location(s) => {
                self.browser.location = s;
                self.browser.editing = true;
                return Task::none();
            }
            Action::Go => match address(&self.browser.location) {
                Ok(url) => {
                    self.browser.editing = false;
                    json!({"op":"navigate","url":url})
                }
                Err(e) => {
                    self.error = Some(e.to_string());
                    return Task::none();
                }
            },
            Action::Size(ref owner, size) if owner == &id => {
                self.browser.size = size;
                let (w, h) = self.browser.dip();
                json!({"op":"size","width":w,"height":h,"scale":self.browser.scale})
            }
            Action::Scale(_) => {
                let (w, h) = self.browser.dip();
                json!({"op":"size","width":w,"height":h,"scale":self.browser.scale})
            }
            Action::Input(owner, op) if owner == id => op,
            Action::PageFocus => {
                self.browser.location = self.browser.status.url.clone();
                self.browser.editing = false;
                return widget::operation::focus("browser-page");
            }
            Action::AddressFocus => {
                let _ = b.user(&id, json!({"op":"focus","focus":false}));
                return widget::operation::focus("browser-location")
                    .chain(widget::operation::select_all("browser-location"));
            }
            Action::Nav(op) => {
                self.browser.editing = false;
                if op == "reload"
                    && let Some(s) = &self.current_session
                    && let Err(e) = b.show(&id, &s.project)
                {
                    self.error = Some(e.to_string());
                    return Task::none();
                }
                json!({"op":op})
            }
            Action::Zoom(step) => json!({"op":"zoom","step":step}),
            Action::FindFocus => {
                let _ = b.user(&id, json!({"op":"focus","focus":false}));
                self.browser.find = Some(String::new());
                return widget::operation::focus("browser-find");
            }
            Action::EndFind => {
                self.browser.find = None;
                json!({"op":"end_find"})
            }
            Action::Query(s) => {
                self.browser.find = Some(s.clone());
                if s.is_empty() {
                    json!({"op":"end_find"})
                } else {
                    json!({"op":"find","text":s})
                }
            }
            Action::Next => json!({"op":"find","text":self.browser.find,"next":true}),
            Action::Control => {
                if let Err(e) = b.control(&id, !self.browser.status.user_control) {
                    self.error = Some(e.to_string())
                }
                self.refresh_browser();
                return Task::none();
            }
            Action::ConfirmProfile => {
                self.browser.confirm = !self.browser.confirm;
                return Task::none();
            }
            Action::Replace => {
                if let Err(e) = b.set_isolated(&id, !self.browser.status.isolated) {
                    self.error = Some(e.to_string());
                    return Task::none();
                }
                self.browser.mounted = None;
                return Task::none();
            }
            Action::Expand => {
                self.browser.expanded = !self.browser.expanded;
                self.view_epoch = self.view_epoch.wrapping_add(1);
                return Task::none();
            }
            Action::Close => {
                self.browser.open = false;
                b.close(&id);
                return Task::none();
            }
            _ => return Task::none(),
        };
        if let Err(e) = b.user(&id, op) {
            self.error = Some(e.to_string())
        }
        Task::none()
    }
    pub(super) fn browser_panel(&self) -> Element<'_, Message> {
        let s = &self.browser;
        let state = &s.status;
        let nav = |icon: Icon, desc: &'static str, a: Action, enabled: bool| {
            tooltip(
                theme::button(icon.view(16.))
                    .padding(4)
                    .style(theme::text_button)
                    .on_press_maybe(enabled.then(|| msg(a))),
                text(desc).size(12),
                tooltip::Position::Bottom,
            )
            .style(card)
        };
        let mut content = column![
            right_panel_header("Browser", msg(Action::Toggle)),
            row![
                nav(Icon::ArrowLeft, "Back", Action::Nav("back"), state.back),
                nav(
                    Icon::ArrowRight,
                    "Forward",
                    Action::Nav("forward"),
                    state.forward
                ),
                nav(
                    if state.loading {
                        Icon::Square
                    } else {
                        Icon::RefreshCw
                    },
                    if state.loading {
                        "Stop loading"
                    } else {
                        "Reload"
                    },
                    Action::Nav(if state.loading { "stop" } else { "reload" }),
                    true
                ),
                nav(Icon::Search, "Find on page", Action::FindFocus, true),
                theme::button(text("−")).on_press(msg(Action::Zoom(-1))),
                theme::button(text("+")).on_press(msg(Action::Zoom(1))),
                nav(
                    Icon::ArrowUpRight,
                    "Expand or restore panel",
                    Action::Expand,
                    true
                ),
                space().width(Fill),
                nav(Icon::X, "Close this tab", Action::Close, true)
            ]
            .spacing(2),
            row![
                text_input("URL or localhost:port", &s.location)
                    .id("browser-location")
                    .on_input(|s| msg(Action::Location(s)))
                    .on_submit(msg(Action::Go))
                    .size(14),
                theme::button(text("Go").size(12)).on_press(msg(Action::Go))
            ]
            .spacing(4),
            row![
                theme::button(
                    text(if state.handoff_pending {
                        "Switching…"
                    } else if state.user_control {
                        "Hand to agent"
                    } else {
                        "Take control"
                    })
                    .size(12)
                )
                .on_press_maybe((!state.handoff_pending).then_some(msg(Action::Control))),
                theme::button(
                    text(if state.isolated {
                        "Isolated ▾"
                    } else {
                        "Project login ▾"
                    })
                    .size(12)
                )
                .on_press(msg(Action::ConfirmProfile))
            ]
            .spacing(4)
        ]
        .spacing(5);
        if s.editing && !state.url.is_empty() {
            content = content.push(ellipsized_text(&format!("Page: {}", state.url), 12., None));
        }
        if s.confirm {
            content = content.push(
                theme::button(
                    text(if state.isolated {
                        "Replace tab with project login"
                    } else {
                        "Replace tab with isolated login"
                    })
                    .size(12),
                )
                .on_press(msg(Action::Replace)),
            )
        }
        if let Some(f) = &s.find {
            content = content.push(row![
                text_input("Find", f)
                    .id("browser-find")
                    .on_input(|s| msg(Action::Query(s)))
                    .on_submit(msg(Action::Next)),
                nav(Icon::X, "Close find", Action::EndFind, true)
            ])
        }
        if let Some(e) = &state.error {
            content = content.push(text(e).size(13))
        }
        if let Some(id) = &s.mounted {
            let owner = id.clone();
            let resize = move |size| msg(Action::Size(owner.clone(), size));
            content = content.push(
                widget::sensor(surface::Surface {
                    id,
                    service: &self.harness.as_ref().unwrap().browser,
                    frame: s.frame.as_ref(),
                    focus: s.focus.clone(),
                    selection: &state.selection,
                    disabled: self.modal.is_some() || self.context_menu.is_some(),
                    dip: s.dip(),
                })
                .key(state.surface_id)
                .on_show(resize.clone())
                .on_resize(resize),
            );
        }
        container(content)
            .width(self.panel_width())
            .height(Fill)
            .padding(8)
            .style(right_panel)
            .into()
    }
}
pub(super) fn subscription(data: &Updates) -> iced::futures::stream::BoxStream<'static, Message> {
    let mut changed = data.0.browser.subscribe();
    changed.mark_changed();
    Box::pin(iced::stream::channel(1, async move |mut output| {
        while changed.changed().await.is_ok() {
            changed.borrow_and_update();
            if output.send(msg(Action::Data)).await.is_err() {
                break;
            }
        }
    }))
}
