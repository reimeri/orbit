//! Keep a session list's handled press out of the composer's outside-click handler.
use crate::ui::Message;
use iced::{
    Element, Event, Length, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
    mouse,
};
use std::cell::Cell;

#[derive(Default)]
pub struct SessionListFocus {
    handled: Cell<bool>,
}

impl SessionListFocus {
    pub fn list<'a>(&'a self, content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
        Element::new(FocusBoundary {
            content: content.into(),
            role: Role::List,
            focus: self,
        })
    }

    pub fn composer<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        // A preceding view may have had lists but no composer to consume their press.
        self.handled.set(false);
        Element::new(FocusBoundary {
            content,
            role: Role::Composer,
            focus: self,
        })
    }
}

enum Role {
    List,
    Composer,
}

struct FocusBoundary<'a> {
    content: Element<'a, Message>,
    role: Role,
    focus: &'a SessionListFocus,
}

impl Widget<Message, iced::Theme, iced::Renderer> for FocusBoundary<'_> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }
    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }
    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }
    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
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
        self.content.as_widget_mut().layout(tree, renderer, limits)
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
            .operate(tree, layout, renderer, operation);
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
        // Iced's Row dispatches to the sidebar before the composer, even after capture.
        let left_press = matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        );
        if left_press && matches!(self.role, Role::Composer) && self.focus.handled.take() {
            return;
        }
        let list_press = left_press
            && matches!(self.role, Role::List)
            && !shell.is_event_captured()
            && cursor.is_over(layout.bounds());
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
        if list_press && shell.is_event_captured() {
            self.focus.handled.set(true);
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
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
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
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}
