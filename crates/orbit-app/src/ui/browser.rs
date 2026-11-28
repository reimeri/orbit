mod gpu;
mod surface;
use super::*;
use iced::widget::column;
use orbit_core::browser::UserCommand as Cmd;
use orbit_core::browser::{Frame, MAX_SIDE, Status, address};

pub(super) struct State {
    pub expanded: bool,
    pub(super) mounted: Option<String>,
    frame: Option<Frame>,
    status: Status,
    location: String,
    editing: bool,
    find: Option<String>,
    size: Size,
    scale: f32,
    focus: surface::Focus,
    pub(super) menu: Option<Menu>,
}
impl Default for State {
    fn default() -> Self {
        Self {
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
            menu: None,
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
    pub(super) fn find_open(&self) -> bool {
        self.mounted.is_some() && self.find.is_some()
    }
    pub fn page_focused(&self) -> bool {
        self.mounted.is_some() && self.focus.0.get()
    }
}
const TOOL_HEIGHT: f32 = 28.;
const TOOL_INSET: f32 = 7.;
const MENU_WIDTH: f32 = 200.;
const COMPACT_WIDTH: f32 = 440.;
fn tool_width(panel_width: f32) -> f32 {
    if panel_width < COMPACT_WIDTH {
        24.
    } else {
        TOOL_HEIGHT
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Driver,
    More,
    Profile,
}
#[derive(Clone, Debug)]
pub enum Action {
    Toggle,
    Data(String, Result<Option<Frame>, String>),
    Size(String, Size),
    Scale(f32),
    Location(String),
    Go,
    AddressFocus,
    PageFocus,
    Input(String, Cmd),
    Nav(Cmd),
    Fit,
    FindFocus,
    Query(String),
    Next,
    EndFind,
    Control,
    Menu(Option<Menu>),
    Replace,
    Close,
    Expand,
}
fn msg(a: Action) -> Message {
    Message::Browser(a)
}
impl App {
    pub(super) fn sync_browser(&mut self) {
        if self.current_session.is_none() && self.right_panel == Some(RightPanel::Browser) {
            self.set_right_panel(None);
        }
        let next = self
            .current_session
            .as_ref()
            .filter(|_| self.right_panel == Some(RightPanel::Browser))
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
        if s.status.error.is_some()
            || s.frame
                .as_ref()
                .is_some_and(|f| Some(f.key.0) != s.status.surface_id)
        {
            s.frame = None;
        }
    }
    pub(super) fn browser_action(&mut self, a: Action) -> Task<Message> {
        if matches!(a, Action::Toggle) {
            self.toggle_right_panel(RightPanel::Browser);
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
            Action::Data(owner, frame) => {
                if owner != id {
                    return Task::none();
                }
                self.refresh_browser();
                let s = &mut self.browser;
                match frame {
                    Ok(Some(f))
                        if Some(f.key.0) == s.status.surface_id && s.status.error.is_none() =>
                    {
                        s.frame = Some(f)
                    }
                    Err(e) => {
                        s.frame = None;
                        s.status.error = Some(e);
                    }
                    _ => (),
                }
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
                    Cmd::Navigate { url }
                }
                Err(e) => {
                    self.error = Some(e.to_string());
                    return Task::none();
                }
            },
            Action::Size(ref owner, size) if owner == &id => {
                self.browser.size = size;
                let (w, h) = self.browser.dip();
                Cmd::Size {
                    width: w,
                    height: h,
                    scale: self.browser.scale,
                }
            }
            Action::Scale(_) => {
                let (w, h) = self.browser.dip();
                Cmd::Size {
                    width: w,
                    height: h,
                    scale: self.browser.scale,
                }
            }
            Action::Input(owner, op) if owner == id => op,
            Action::PageFocus => {
                self.browser.location = self.browser.status.url.clone();
                self.browser.editing = false;
                return widget::operation::focus("browser-page");
            }
            Action::AddressFocus => {
                if self.browser.find.take().is_some() {
                    let _ = b.user(&id, Cmd::EndFind);
                }
                let _ = b.user(&id, Cmd::Focus { focus: false });
                return widget::operation::focus("browser-location")
                    .chain(widget::operation::select_all("browser-location"));
            }
            Action::Nav(op) => {
                self.browser.editing = false;
                if matches!(op, Cmd::Reload)
                    && let Some(s) = &self.current_session
                    && let Err(e) = b.show(&id, &s.project)
                {
                    self.error = Some(e.to_string());
                    return Task::none();
                }
                op
            }
            Action::Fit => {
                self.browser.menu = None;
                Cmd::Viewport { fixed: None }
            }
            Action::FindFocus => {
                let _ = b.user(&id, Cmd::Focus { focus: false });
                self.browser.menu = None;
                self.browser.find.get_or_insert_with(String::new);
                return widget::operation::focus("browser-find");
            }
            Action::EndFind => {
                self.browser.find = None;
                Cmd::EndFind
            }
            Action::Query(s) => {
                self.browser.find = Some(s.clone());
                if s.is_empty() {
                    Cmd::EndFind
                } else {
                    Cmd::Find {
                        text: s,
                        next: false,
                    }
                }
            }
            Action::Next => {
                let Some(text) = self.browser.find.clone() else {
                    return Task::none();
                };
                Cmd::Find { text, next: true }
            }
            Action::Control => {
                self.browser.menu = None;
                if let Err(e) = b.control(&id, !self.browser.status.user_control) {
                    self.error = Some(e.to_string())
                }
                self.refresh_browser();
                return Task::none();
            }
            Action::Menu(menu) => {
                self.browser.menu = menu;
                if menu.is_some() {
                    self.footer_menu_open = false;
                    self.worktrees.popup = None;
                }
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
                self.browser.menu = None;
                self.browser.expanded = !self.browser.expanded;
                self.view_epoch = self.view_epoch.wrapping_add(1);
                return Task::none();
            }
            Action::Close => {
                self.set_right_panel(None);
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
        let compact = self.panel_width() < COMPACT_WIDTH;
        let button_width = tool_width(self.panel_width());
        let nav = |icon: Icon, desc: &'static str, a: Action, enabled: bool| {
            icon_tip(
                theme::button(
                    container(icon.view(16.))
                        .center_x(button_width)
                        .center_y(TOOL_HEIGHT),
                )
                .padding(0)
                .style(theme::text_button)
                .on_press_maybe(enabled.then(|| msg(a))),
                desc,
            )
        };
        let driver = if state.handoff_pending {
            "Switching…"
        } else if state.user_control {
            "You drive"
        } else {
            "Agent drives"
        };
        let icon = if state.handoff_pending {
            Icon::LoaderCircle
        } else if state.user_control {
            Icon::User
        } else {
            Icon::Bot
        };
        let mut chip = row![icon.spinning(16., state.handoff_pending)]
            .align_y(iced::Center)
            .spacing(6);
        if !compact {
            chip = chip
                .push(text(driver).size(13))
                .push(Icon::ChevronDown.view(12.));
        }

        // Keep the field at the same child index so resizing preserves its focus and selection.
        let mut prefix =
            row![container(widget::rule::vertical(1)).height(16)].align_y(iced::Center);
        if let Some((w, h)) = state.viewport {
            prefix = prefix.push(icon_tip(
                container(text(format!("{w}×{h}")).size(11).style(theme::muted)).padding([0, 3]),
                "Fixed viewport (CSS pixels)",
            ));
        } else if !compact {
            prefix = prefix.push(
                container(
                    if s.find.is_some() {
                        Icon::Search
                    } else {
                        Icon::Globe
                    }
                    .view(14.),
                )
                .padding([0, 4]),
            );
        }
        let mut bar = row![
            nav(Icon::ArrowLeft, "Back", Action::Nav(Cmd::Back), state.back),
            nav(
                Icon::ArrowRight,
                "Forward",
                Action::Nav(Cmd::Forward),
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
                Action::Nav(if state.loading {
                    Cmd::Stop
                } else {
                    Cmd::Reload
                }),
                true
            ),
            prefix,
        ]
        .align_y(iced::Center)
        .spacing(2);
        let field = if let Some(f) = &s.find {
            text_input("Find", f)
                .id("browser-find")
                .on_input(|s| msg(Action::Query(s)))
                .on_submit_maybe((!f.is_empty()).then_some(msg(Action::Next)))
        } else {
            text_input("Address", &s.location)
                .id("browser-location")
                .on_input(|s| msg(Action::Location(s)))
                .on_submit(msg(Action::Go))
        };
        bar = bar.push(
            field
                .width(Fill)
                .size(13)
                .padding([4, 3])
                .style(|t, status| {
                    let mut style = theme::input(t, status);
                    let c = theme::colors(t);
                    style.background = Color::TRANSPARENT.into();
                    style.border.color = match status {
                        text_input::Status::Focused { .. } => c.border,
                        _ => Color::TRANSPARENT,
                    };
                    style
                }),
        );
        if s.find.is_some() {
            bar = bar.push(nav(Icon::X, "Close find", Action::EndFind, true));
        }
        let bar = bar
            .push(icon_tip(
                theme::button(container(chip).center_y(TOOL_HEIGHT))
                    .padding([0, 7])
                    .style(secondary)
                    .on_press_maybe(
                        (!state.handoff_pending).then_some(msg(Action::Menu(Some(Menu::Driver)))),
                    ),
                driver,
            ))
            .push(nav(
                Icon::Ellipsis,
                "Browser options",
                Action::Menu(Some(Menu::More)),
                true,
            ))
            .push(container(widget::rule::vertical(1)).height(16))
            .push(nav(Icon::X, "Hide browser panel", Action::Toggle, true));
        let mut content = column![
            container(bar).width(Fill).padding(TOOL_INSET),
            widget::rule::horizontal(1)
        ];
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
                    frame: s.frame.as_ref().filter(|f| {
                        f.view == state.viewport.unwrap_or_else(|| s.dip())
                            && f.scale
                                == if state.viewport.is_some() {
                                    1.
                                } else {
                                    s.scale
                                }
                    }),
                    focus: s.focus.clone(),
                    selection: &state.selection,
                    finding: s.find.is_some(),
                    disabled: s.menu.is_some()
                        || self.modal.is_some()
                        || self.context_menu.is_some(),
                    fixed: state.viewport,
                    dip: state.viewport.unwrap_or_else(|| s.dip()),
                })
                .key(state.surface_id)
                .on_show(resize.clone())
                .on_resize(resize),
            );
        }
        container(content)
            .width(self.panel_width())
            .height(Fill)
            .style(right_panel)
            .into()
    }

    pub(super) fn browser_popup(&self, menu: Menu) -> Element<'_, Message> {
        let s = &self.browser;
        let choice = |label: &'static str, action: Action| {
            theme::button(text(label).size(13))
                .width(Fill)
                .padding([6, 9])
                .style(theme::text_button)
                .on_press(msg(action))
        };
        let actions = match menu {
            Menu::Driver => column![choice(
                if s.status.user_control {
                    "Hand to agent"
                } else {
                    "Take control"
                },
                Action::Control
            )],
            Menu::More => column![
                choice("Find on page", Action::FindFocus),
                choice(
                    if s.expanded {
                        "Restore panel size"
                    } else {
                        "Expand panel"
                    },
                    Action::Expand
                ),
                widget::rule::horizontal(1),
                choice(
                    if s.status.isolated {
                        "Login: isolated…"
                    } else {
                        "Login: project…"
                    },
                    Action::Menu(Some(Menu::Profile))
                ),
                choice("Close tab", Action::Close),
            ]
            .extend(
                s.status
                    .viewport
                    .map(|_| choice("Fit to panel", Action::Fit).into()),
            ),
            Menu::Profile => column![
                container(text("Replace this tab with a blank page?").size(13)).padding([6, 9]),
                choice(
                    if s.status.isolated {
                        "Use project login"
                    } else {
                        "Use isolated login"
                    },
                    Action::Replace
                ),
                choice("Cancel", Action::Menu(None)),
            ],
        };
        let popup = opaque(
            container(actions.spacing(2))
                .padding(5)
                .width(MENU_WIDTH)
                .style(card),
        );
        // Align the menu's right edge with its trigger; stay inside the browser panel.
        let button_width = tool_width(self.panel_width());
        let inset = TOOL_INSET
            + button_width
            + 5.
            + if menu == Menu::Driver {
                button_width + 2.
            } else {
                0.
            };
        container(popup)
            .align_right(Fill)
            .padding(
                iced::Padding::ZERO
                    .top(TOOL_INSET * 2. + TOOL_HEIGHT)
                    .right(
                        super::RAIL_WIDTH + inset.min((self.panel_width() - MENU_WIDTH).max(0.)),
                    ),
            )
            .into()
    }
}
pub(super) fn subscription(
    (data, id): &(Updates, String),
) -> iced::futures::stream::BoxStream<'static, Message> {
    let id = id.clone();
    let browser = data.0.browser.clone();
    let mut changed = browser.subscribe();
    changed.mark_changed();
    Box::pin(iced::stream::channel(1, async move |mut output| {
        let mut last = None;
        while changed.changed().await.is_ok() {
            changed.borrow_and_update();
            let b = browser.clone();
            let owner = id.clone();
            let frame = tokio::task::spawn_blocking(move || b.frame(&owner, last))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r.map_err(|e| e.to_string()));
            if let Ok(Some(f)) = &frame {
                last = Some(f.key);
            }
            if output
                .send(msg(Action::Data(id.clone(), frame)))
                .await
                .is_err()
            {
                break;
            }
        }
    }))
}
