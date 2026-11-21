//! Resolve click positions and track active drags at the root, in window coordinates.
use crate::ui::Message;
use iced::{
    Element, Event, Length, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, overlay, renderer,
        widget::{Id, Operation, Tree, operation, tree},
    },
    keyboard, mouse, touch,
};
use std::cell::Cell;

/// Place below a popup in a stack to dismiss it without consuming outside input.
/// The popup must block input within its own bounds (for example with `opaque`).
pub fn dismiss_layer(message: Message) -> Element<'static, Message> {
    Element::new(DismissLayer(message))
}

struct DismissLayer(Message);

impl Widget<Message, iced::Theme, iced::Renderer> for DismissLayer {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(Length::Fill, Length::Fill, Size::ZERO))
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if cursor.is_over(layout.bounds())
            && matches!(
                event,
                Event::Mouse(
                    mouse::Event::ButtonPressed(mouse::Button::Left | mouse::Button::Right)
                        | mouse::Event::WheelScrolled { .. }
                ) | Event::Touch(touch::Event::FingerPressed { .. })
            )
        {
            shell.publish(self.0.clone());
            // Leave the event uncaptured so the same input reaches the UI below.
        }
    }

    fn draw(
        &self,
        _tree: &Tree,
        _renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
    }
}

/// Requests from widgets, resolved at the root before reaching the application.
#[derive(Clone, Debug)]
pub enum Press {
    Resize(bool),
    SessionMenu(String),
    ProjectMenu(std::path::PathBuf),
}

pub fn pointer_input(
    content: Element<'_, Message>,
    capture_keyboard: bool,
    selection: crate::chat_selection::Shared,
) -> Element<'_, Message> {
    Element::new(PointerInput {
        content,
        capture_keyboard,
        selection,
    })
}

struct PointerInput<'a> {
    content: Element<'a, Message>,
    capture_keyboard: bool,
    selection: crate::chat_selection::Shared,
}

#[derive(Default)]
struct State {
    dragging: Cell<bool>,
}

// Buttons activate on release, but iced editors blur on the initial outside
// press. Resolve the list hit in window coordinates before forwarding the
// press, so holding a session row never paints an unfocused composer.
struct SessionListPress {
    cursor: mouse::Cursor,
    over_list: bool,
    composer_focused: bool,
}

impl Operation for SessionListPress {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        _: Rectangle,
        _: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        self.over_list |= self.cursor.is_over(bounds)
            && id.is_some_and(|id| {
                ["sessions", "deferred-sessions", "settled-sessions"]
                    .into_iter()
                    .any(|name| *id == Id::new(name))
            });
    }

    fn focusable(&mut self, id: Option<&Id>, _: Rectangle, state: &mut dyn operation::Focusable) {
        self.composer_focused |= state.is_focused() && id == Some(&Id::new("composer"));
    }
}

impl Widget<Message, iced::Theme, iced::Renderer> for PointerInput<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // An open footer picker blocks typing/submission in the editor underneath.
        // The window subscription handles Escape; other picker keystrokes are ignored.
        if self.capture_keyboard && matches!(event, Event::Keyboard(_) | Event::InputMethod(_)) {
            shell.capture_event();
            return;
        }
        // Chat selection owns Copy ahead of the composer, which can still have
        // keyboard focus. Any new click/keyboard navigation returns ownership.
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            physical_key,
            modifiers,
            ..
        }) = event
        {
            if modifiers.command()
                && key.to_latin(*physical_key) == Some('c')
                && self.selection.borrow().range().is_some()
            {
                shell.publish(Message::CopySelection);
                shell.capture_event();
                return;
            }
            if matches!(key, keyboard::Key::Character(_))
                || matches!(
                    key,
                    keyboard::Key::Named(
                        keyboard::key::Named::Escape
                            | keyboard::key::Named::Tab
                            | keyboard::key::Named::Enter
                            | keyboard::key::Named::Backspace
                            | keyboard::key::Named::Delete
                            | keyboard::key::Named::ArrowLeft
                            | keyboard::key::Named::ArrowRight
                            | keyboard::key::Named::ArrowUp
                            | keyboard::key::Named::ArrowDown
                            | keyboard::key::Named::Home
                            | keyboard::key::Named::End
                            | keyboard::key::Named::PageUp
                            | keyboard::key::Named::PageDown
                    )
                )
            {
                self.selection.borrow_mut().clear();
                shell.request_redraw();
            }
        }
        if matches!(event, Event::Window(iced::window::Event::Unfocused)) {
            self.selection.borrow_mut().clear();
            shell.request_redraw();
        }
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) {
            self.selection.borrow_mut().begin_press();
            shell.request_redraw();
        }
        // TextInput already supports Ctrl+Backspace. Normalize Ctrl+W here so
        // every focused text field shares that behavior without tracking focus.
        let word_backspace = match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                location,
                repeat,
                ..
            }) if *modifiers == keyboard::Modifiers::CTRL
                && key.to_latin(*physical_key) == Some('w') =>
            {
                Some(Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Backspace),
                    modified_key: keyboard::Key::Named(keyboard::key::Named::Backspace),
                    physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Backspace),
                    location: *location,
                    modifiers: *modifiers,
                    text: None,
                    repeat: *repeat,
                }))
            }
            _ => None,
        };
        let event = word_backspace.as_ref().unwrap_or(event);
        if crate::chat_selection::selection_motion(event) {
            self.selection.borrow_mut().begin_motion();
        }
        let retain_focus = if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) {
            let mut press = SessionListPress {
                cursor,
                over_list: false,
                composer_focused: false,
            };
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                &mut press,
            );
            press.over_list && press.composer_focused
        } else {
            false
        };
        let state = tree.state.downcast_mut::<State>();
        let mut messages = Vec::new();
        let mut child_shell = Shell::new(&mut messages);
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            &mut child_shell,
            viewport,
        );
        if retain_focus && child_shell.is_event_captured() {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                &mut operation::focusable::focus("composer".into()),
            );
        }
        shell.merge(child_shell, |message| match message {
            Message::OpenImageFrom(message, mut source) => {
                let position = cursor.position().expect("image activation has a cursor");
                source.translate(Vector::new(position.x, position.y));
                Message::OpenImageFrom(message, source)
            }
            Message::Press(press) => {
                // MouseArea only emits press requests while the pointer is over it.
                // Read the root cursor: descendants of Scrollable see translated coordinates.
                let position = match event {
                    Event::Touch(touch::Event::FingerPressed { position, .. }) => *position,
                    _ => cursor.position().expect("press has a cursor position"),
                };
                match press {
                    Press::Resize(right) => {
                        state.dragging.set(true);
                        Message::ResizeStart(right, position)
                    }
                    Press::SessionMenu(id) => Message::SessionMenu(id, position),
                    Press::ProjectMenu(path) => Message::ProjectMenu(path, position),
                }
            }
            message => message,
        });
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) {
            self.selection.borrow_mut().finish_press();
        }
        if crate::chat_selection::selection_motion(event)
            && self.selection.borrow_mut().finish_motion()
        {
            shell.request_redraw();
        }
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
        ) {
            self.selection.borrow_mut().dragging = false;
        }
        // Widget-local state changes during the press event, before the next event
        // in the same batch. No subscription installation race or idle motion messages.
        if state.dragging.get() {
            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    shell.publish(Message::Event(event.clone()));
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    state.dragging.set(false);
                    shell.publish(Message::Event(event.clone()));
                }
                _ => {}
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{
        Point,
        advanced::{clipboard, widget::operation},
        widget::{column, mouse_area, scrollable, space},
        window,
    };
    use iced_runtime::{UserInterface, user_interface};

    fn renderer() -> iced::Renderer {
        crate::test_renderer::new(16.)
    }

    fn divider() -> Element<'static, Message> {
        pointer_input(
            mouse_area(space().width(5).height(200))
                .on_press(Message::Press(Press::Resize(false)))
                .into(),
            false,
            Default::default(),
        )
    }

    #[test]
    fn menu_capture_blocks_keyboard_and_ime_from_focused_input() {
        for capture in [false, true] {
            let mut renderer = renderer();
            let view = pointer_input(
                iced::widget::text_input("", "")
                    .id("composer")
                    .on_input(Message::Search)
                    .on_submit(Message::Send)
                    .into(),
                capture,
                Default::default(),
            );
            let mut ui = UserInterface::build(
                view,
                Size::new(300., 40.),
                user_interface::Cache::new(),
                &mut renderer,
            );
            ui.operate(
                &renderer,
                &mut operation::focusable::focus("composer".into()),
            );
            let mut messages = Vec::new();
            ui.update(
                &[
                    Event::InputMethod(iced::advanced::input_method::Event::Commit("界".into())),
                    Event::Keyboard(iced::keyboard::Event::KeyPressed {
                        key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
                        modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter),
                        physical_key: iced::keyboard::key::Physical::Code(
                            iced::keyboard::key::Code::Enter,
                        ),
                        location: iced::keyboard::Location::Standard,
                        modifiers: iced::keyboard::Modifiers::empty(),
                        text: None,
                        repeat: false,
                    }),
                ],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            if capture {
                assert!(messages.is_empty());
            } else {
                assert!(
                    messages
                        .iter()
                        .any(|m| matches!(m, Message::Search(value) if value == "界"))
                );
                assert!(messages.iter().any(|m| matches!(m, Message::Send)));
            }
        }
    }

    #[test]
    fn motion_only_publishes_during_drag_including_same_batch_and_rebuild() {
        let mut renderer = renderer();
        let bounds = Size::new(600., 400.);
        let mut ui = UserInterface::build(
            divider(),
            bounds,
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        let start = Point::new(2., 53.);
        let moved = Point::new(147., 91.);
        let motion = Event::Mouse(mouse::Event::CursorMoved { position: moved });
        let (state, _) = ui.update(
            std::slice::from_ref(&motion),
            mouse::Cursor::Available(moved),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(messages.is_empty(), "idle movement must not update the app");
        assert!(matches!(
            state,
            user_interface::State::Updated {
                redraw_request: window::RedrawRequest::Wait,
                ..
            }
        ));

        // No app update/subscription refresh between press and motion. This
        // exercises event forwarding; Iced supplies one cursor snapshot per batch.
        ui.update(
            &[
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                motion.clone(),
            ],
            mouse::Cursor::Available(start),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(
            matches!(messages.as_slice(), [Message::ResizeStart(false, point),
            Message::Event(Event::Mouse(mouse::Event::CursorMoved { position }))]
            if *point == start && *position == moved)
        );
        messages.clear();

        // A normal application rebuild must preserve the widget's active drag.
        let mut ui = UserInterface::build(divider(), bounds, ui.into_cache(), &mut renderer);
        ui.update(
            &[
                motion.clone(),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                motion,
            ],
            mouse::Cursor::Available(moved),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(
            matches!(
                messages.as_slice(),
                [
                    Message::Event(Event::Mouse(mouse::Event::CursorMoved { .. })),
                    Message::Event(Event::Mouse(mouse::Event::ButtonReleased(
                        mouse::Button::Left
                    )))
                ]
            ),
            "release outside the divider must stop subsequent motion messages"
        );
    }

    #[test]
    fn scrolled_session_press_uses_window_coordinates_and_preserves_left_click() {
        let mut renderer = renderer();
        let row = mouse_area(
            iced::widget::button(space().width(180).height(70))
                .padding(0)
                .on_press(Message::Select("session".into())),
        )
        .on_right_press(Message::Press(Press::SessionMenu("session".into())));
        let view = pointer_input(
            scrollable(column![space().height(500), row])
                .id("sessions")
                .width(200)
                .height(100)
                .into(),
            false,
            Default::default(),
        );
        let mut ui = UserInterface::build(
            view,
            Size::new(600., 400.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        ui.operate(
            &renderer,
            &mut operation::scrollable::scroll_to(
                "sessions".into(),
                operation::scrollable::AbsoluteOffset {
                    x: Some(0.),
                    y: Some(450.),
                },
            ),
        );
        let cursor = Point::new(37., 63.);
        let mut messages = Vec::new();
        ui.update(
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Right,
            ))],
            mouse::Cursor::Available(cursor),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(
            matches!(messages.as_slice(), [Message::SessionMenu(id, position)]
            if id == "session" && *position == cursor)
        );
        messages.clear();
        ui.update(
            &[
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(cursor),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(matches!(messages.as_slice(), [Message::Select(id)] if id == "session"));
    }
}
