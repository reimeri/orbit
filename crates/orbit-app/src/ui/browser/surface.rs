//! A focusable native input target. iced coordinates and CEF input use DIPs;
//! the pixel buffer drawn by wgpu may be larger at fractional scale.
use super::{Action, Message, gpu::Pixels};
use iced::{
    Element, Event, Length, Point, Rectangle, Size,
    advanced::{
        Clipboard, InputMethod, Layout, Shell, Widget, clipboard, input_method, layout, renderer,
        widget::{Id, Operation, Tree, operation::Focusable, tree},
    },
    keyboard, mouse,
};
use orbit_core::browser::Frame;
use serde_json::{Value, json};
use std::{cell::Cell, rc::Rc};
#[derive(Default, Clone)]
pub(super) struct Focus(pub Rc<Cell<bool>>);
impl Focusable for Focus {
    fn is_focused(&self) -> bool {
        self.0.get()
    }
    fn focus(&mut self) {
        self.0.set(true);
    }
    fn unfocus(&mut self) {
        self.0.set(false);
    }
}
pub(super) struct Surface<'a> {
    pub id: &'a str,
    pub service: &'a orbit_core::browser::Browser,
    pub frame: Option<&'a Frame>,
    pub focus: Focus,
    pub selection: &'a str,
    pub disabled: bool,
    pub finding: bool,
    pub dip: (u32, u32),
    pub fixed: Option<(u32, u32)>,
}
impl Surface<'_> {
    // Both drawing and hit-testing use this centered content rectangle. The
    // outer sensor continues reporting the panel size, including letterboxing.
    fn preview(&self, bounds: Rectangle) -> Rectangle {
        let Some((w, h)) = self.fixed else {
            return bounds;
        };
        let scale = (bounds.width / w as f32)
            .min(bounds.height / h as f32)
            .min(1.);
        let size = Size::new(w as f32 * scale, h as f32 * scale);
        Rectangle::new(
            Point::new(
                bounds.center_x() - size.width / 2.,
                bounds.center_y() - size.height / 2.,
            ),
            size,
        )
    }
}
#[derive(Default)]
struct Input {
    down: bool,
    inside: bool,
    mods: keyboard::Modifiers,
    click: Option<iced::advanced::mouse::Click>,
    point: Point,
}
fn keycode(k: keyboard::Key<&str>, physical: keyboard::key::Physical) -> i32 {
    use keyboard::key::{Code, Physical};
    // Windows VK constants are not ASCII punctuation (e.g. '.' is not Delete).
    if let Physical::Code(c) = physical {
        let vk = match c {
            Code::Backquote => 0xc0,
            Code::Minus => 0xbd,
            Code::Equal => 0xbb,
            Code::BracketLeft => 0xdb,
            Code::BracketRight => 0xdd,
            Code::Backslash => 0xdc,
            Code::Semicolon => 0xba,
            Code::Quote => 0xde,
            Code::Comma => 0xbc,
            Code::Period => 0xbe,
            Code::Slash => 0xbf,
            _ => 0,
        };
        if vk != 0 {
            return vk;
        }
    }
    use keyboard::key::Named::*;
    match k {
        keyboard::Key::Character(c) if c.len() == 1 && c.as_bytes()[0].is_ascii_alphanumeric() => {
            c.as_bytes()[0].to_ascii_uppercase() as i32
        }
        keyboard::Key::Named(n) => match n {
            Enter => 13,
            Tab => 9,
            Escape => 27,
            Space => 32,
            Backspace => 8,
            Delete => 46,
            Insert => 45,
            ArrowLeft => 37,
            ArrowUp => 38,
            ArrowRight => 39,
            ArrowDown => 40,
            Home => 36,
            End => 35,
            PageUp => 33,
            PageDown => 34,
            Shift => 16,
            Control => 17,
            Alt => 18,
            _ => 0,
        },
        _ => 0,
    }
}
fn modifiers(m: keyboard::Modifiers) -> i32 {
    2 * i32::from(m.shift()) + 4 * i32::from(m.control()) + 8 * i32::from(m.alt())
}
impl Focusable for Surface<'_> {
    fn is_focused(&self) -> bool {
        self.focus.is_focused()
    }
    fn focus(&mut self) {
        if !self.focus.0.replace(true) {
            let _ = self
                .service
                .user(self.id, json!({"op":"focus","focus":true}));
        }
    }
    fn unfocus(&mut self) {
        if self.focus.0.replace(false) {
            let _ = self
                .service
                .user(self.id, json!({"op":"focus","focus":false}));
        }
    }
}
impl Widget<Message, iced::Theme, iced::Renderer> for Surface<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Input>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Input::default())
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn layout(
        &mut self,
        _: &mut Tree,
        _: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, Length::Fill, Length::Fill)
    }
    fn operate(
        &mut self,
        _: &mut Tree,
        layout: Layout<'_>,
        _: &iced::Renderer,
        op: &mut dyn Operation,
    ) {
        op.focusable(
            Some(&Id::new("browser-page")),
            self.preview(layout.bounds()),
            self,
        );
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &iced::Renderer,
        clip: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<Input>();
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = event {
            state.mods = *m;
            return;
        }
        if self.disabled {
            return;
        }
        let bounds = self.preview(layout.bounds());
        let pos = cursor.position_in(bounds);
        if let Some(p) = cursor.position() {
            state.point = Point::new(p.x - bounds.x, p.y - bounds.y);
        }
        let left = state.inside && pos.is_none();
        if matches!(event, Event::Mouse(_)) {
            state.inside = pos.is_some();
        }
        let x = (state.point.x * self.dip.0 as f32 / bounds.width) as i32;
        let y = (state.point.y * self.dip.1 as f32 / bounds.height) as i32;
        let send = |msg: Value| Message::Browser(Action::Input(self.id.to_owned(), msg));
        let action = match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if pos.is_none() {
                    if self.focus.0.replace(false) {
                        shell.publish(send(json!({"op":"focus","focus":false})));
                    }
                    return;
                }
                if self.frame.is_none() {
                    return;
                }
                self.focus.focus();
                state.down = true;
                let click = iced::advanced::mouse::Click::new(
                    state.point,
                    mouse::Button::Left,
                    state.click,
                );
                let count = match click.kind() {
                    iced::advanced::mouse::click::Kind::Single => 1,
                    iced::advanced::mouse::click::Kind::Double => 2,
                    iced::advanced::mouse::click::Kind::Triple => 3,
                };
                state.click = Some(click);
                shell.publish(Message::Browser(Action::PageFocus));
                send(json!({"op":"down","x":x,"y":y,"count":count,"mods":modifiers(state.mods)}))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.down => {
                state.down = false;
                send(json!({"op":"up","x":x,"y":y,"mods":modifiers(state.mods)}))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. })
                if pos.is_some() || left || state.down =>
            {
                send(
                    json!({"op":"move","leave":pos.is_none(),"x":x,"y":y,"mods":modifiers(state.mods)+16*i32::from(state.down)}),
                )
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta })
                if pos.is_some() && self.frame.is_some() =>
            {
                let (dx, dy) = match delta {
                    mouse::ScrollDelta::Lines { x, y } => (x * 40., y * 40.),
                    mouse::ScrollDelta::Pixels { x, y } => (*x, *y),
                };
                send(
                    json!({"op":"scroll","x":x,"y":y,"dx":dx.round() as i32,"dy":dy.round() as i32}),
                )
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers: m,
                physical_key,
                text,
                ..
            }) if self.focus.is_focused() => {
                use keyboard::key::Named;
                let k = key.as_ref();
                match (m.command(), k.clone()) {
                    (_, keyboard::Key::Named(Named::Escape)) if self.finding => {
                        Message::Browser(Action::EndFind)
                    }
                    (true, keyboard::Key::Character("l")) => Message::Browser(Action::AddressFocus),
                    (true, keyboard::Key::Character("f")) => Message::Browser(Action::FindFocus),
                    (true, keyboard::Key::Character("r"))
                    | (_, keyboard::Key::Named(Named::F5)) => send(json!({"op":"reload"})),
                    (true, keyboard::Key::Character("a")) => send(json!({"op":"select_all"})),
                    (true, keyboard::Key::Character(c @ ("c" | "x"))) => {
                        if !self.selection.is_empty() {
                            clip.write(clipboard::Kind::Standard, self.selection.into());
                        }
                        if c == "x" {
                            send(json!({"op":"cut"}))
                        } else {
                            shell.capture_event();
                            return;
                        }
                    }
                    (true, keyboard::Key::Character("v")) => {
                        if let Some(s) = clip.read(clipboard::Kind::Standard) {
                            send(json!({"op":"text","text":s}))
                        } else {
                            shell.capture_event();
                            return;
                        }
                    }
                    (_, keyboard::Key::Named(Named::ArrowLeft)) if m.alt() => {
                        send(json!({"op":"back"}))
                    }
                    (_, keyboard::Key::Named(Named::ArrowRight)) if m.alt() => {
                        send(json!({"op":"forward"}))
                    }
                    _ => send(
                        json!({"op":"keydown","code":keycode(k,*physical_key),"mods":modifiers(*m),"text":if m.command(){""}else{text.as_deref().unwrap_or("")}}),
                    ),
                }
            }
            Event::Keyboard(keyboard::Event::KeyReleased {
                key,
                physical_key,
                modifiers: m,
                ..
            }) if self.focus.is_focused() => send(
                json!({"op":"keyup","code":keycode(key.as_ref(),*physical_key),"mods":modifiers(*m)}),
            ),
            Event::InputMethod(input_method::Event::Commit(s)) if self.focus.is_focused() => {
                send(json!({"op":"text","text":s}))
            }
            Event::InputMethod(input_method::Event::Preedit(s, _)) if self.focus.is_focused() => {
                send(json!({"op":"preedit","text":s}))
            }
            Event::Window(iced::window::Event::Unfocused) if self.focus.0.replace(false) => {
                state.down = false;
                send(json!({"op":"focus","focus":false}))
            }
            Event::Window(iced::window::Event::RedrawRequested(_)) if self.focus.is_focused() => {
                shell.request_input_method(&InputMethod::<String>::Enabled {
                    cursor: Rectangle::new(
                        Point::new(bounds.x + state.point.x, bounds.y + state.point.y),
                        Size::new(2., 18.),
                    ),
                    purpose: input_method::Purpose::Normal,
                    preedit: None,
                });
                return;
            }
            _ => return,
        };
        shell.publish(action);
        shell.capture_event();
    }
    fn draw(
        &self,
        _: &Tree,
        renderer: &mut iced::Renderer,
        _: &iced::Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        _: &Rectangle,
    ) {
        if let Some(frame) = self.frame {
            iced_wgpu::primitive::Renderer::draw_primitive(
                renderer,
                self.preview(layout.bounds()),
                Pixels(frame.clone()),
            );
        }
    }
}
impl<'a> From<Surface<'a>> for Element<'a, Message> {
    fn from(s: Surface<'a>) -> Self {
        Element::new(s)
    }
}
