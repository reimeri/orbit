//! Resolve click positions and track active drags at the root, in window coordinates.
use crate::ui::Message;
use iced::{
    Element, Event, Length, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
    mouse, touch,
};
use std::cell::Cell;

/// Requests from widgets, resolved at the root before reaching the application.
#[derive(Clone, Debug)]
pub enum Press {
    Resize(bool),
    SessionMenu(String),
}

pub fn pointer_input(
    content: Element<'_, Message>,
    capture_keyboard: bool,
) -> Element<'_, Message> {
    Element::new(PointerInput {
        content,
        capture_keyboard,
    })
}

struct PointerInput<'a> {
    content: Element<'a, Message>,
    capture_keyboard: bool,
}

#[derive(Default)]
struct State {
    dragging: Cell<bool>,
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
        // Footer menus own keyboard input, not the editor that may still have focus.
        // The window subscription handles navigation after we stop child delivery.
        if self.capture_keyboard && matches!(event, Event::Keyboard(_) | Event::InputMethod(_)) {
            shell.capture_event();
            return;
        }
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
        shell.merge(child_shell, |message| match message {
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
                }
            }
            message => message,
        });
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
        iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ))
    }

    fn divider() -> Element<'static, Message> {
        pointer_input(
            mouse_area(space().width(5).height(200))
                .on_press(Message::Press(Press::Resize(false)))
                .into(),
            false,
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
