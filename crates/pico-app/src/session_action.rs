//! Row-local age/action swapping. Pointer motion never enters the app update loop.
use iced::{
    Element, Event, Length, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
    mouse,
    widget::{container, space, text, tooltip},
};

use crate::theme;

pub const WIDTH: f32 = 64.;
pub const AGE_SIZE: f32 = 10.;
pub const UNDO_SIZE: f32 = 16.;
pub const HEIGHT: f32 = 18.;
pub const PADDING: f32 = 6.;
pub const RIGHT: f32 = 8.;

pub fn slot<'a, Message: 'a>(width: f32) -> Element<'a, Message> {
    space().width(width).height(HEIGHT).into()
}

pub fn row<'a, Message: Clone + 'a>(
    content: impl Into<Element<'a, Message>>,
    age: String,
    width: f32,
    label: impl Into<Element<'a, Message>>,
    tip: &'static str,
    on_press: Message,
) -> Element<'a, Message> {
    let age = container(text(age).size(AGE_SIZE).color(theme::MUTED))
        .align_right(width)
        .center_y(HEIGHT);
    // Only the button owns the tooltip, not the surrounding alignment space.
    let action = container(
        tooltip(
            theme::button(label)
                .padding(0)
                .on_press(on_press)
                .style(theme::sidebar_control),
            text(tip).size(12),
            tooltip::Position::Top,
        )
        .style(theme::footer_surface),
    )
    .align_right(width)
    .center_y(HEIGHT);
    Element::new(SessionAction {
        content: content.into(),
        age: age.into(),
        action: action.into(),
        width,
    })
}

struct SessionAction<'a, Message> {
    content: Element<'a, Message>,
    age: Element<'a, Message>,
    action: Element<'a, Message>,
    width: f32,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for SessionAction<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<bool>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(false)
    }
    fn children(&self) -> Vec<Tree> {
        [&self.content, &self.age, &self.action]
            .map(Tree::new)
            .into()
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content, &self.age, &self.action]);
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let content = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let size = content.size();
        let position = iced::Point::new(
            size.width - RIGHT - self.width,
            size.height - PADDING - HEIGHT,
        );
        let slot_limits = layout::Limits::new(Size::ZERO, Size::new(self.width, HEIGHT));
        let age = self
            .age
            .as_widget_mut()
            .layout(&mut tree.children[1], renderer, &slot_limits)
            .move_to(position);
        let action = self
            .action
            .as_widget_mut()
            .layout(&mut tree.children[2], renderer, &slot_limits)
            .move_to(position);
        layout::Node::with_children(size, vec![content, age, action])
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        for ((element, tree), layout) in [&mut self.content, &mut self.age, &mut self.action]
            .into_iter()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            element
                .as_widget_mut()
                .operate(tree, layout, renderer, operation);
        }
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
        let hovered = cursor.is_over(layout.bounds()) && cursor.is_over(*viewport);
        let previous = tree.state.downcast_mut::<bool>();
        if *previous != hovered {
            *previous = hovered;
            shell.request_redraw();
        }
        // The action gets first refusal; its clicks must never select the row.
        // Still send leave/release events when hidden to clear button/tooltip state.
        self.action.as_widget_mut().update(
            &mut tree.children[2],
            event,
            layout.children().nth(2).expect("action"),
            if hovered {
                cursor
            } else {
                mouse::Cursor::Unavailable
            },
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if !shell.is_event_captured() {
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout.children().next().expect("content"),
                cursor,
                renderer,
                clipboard,
                shell,
                viewport,
            );
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
        let base = self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().expect("content"),
            cursor,
            viewport,
            renderer,
        );
        if cursor.is_over(layout.bounds()) {
            base.max(self.action.as_widget().mouse_interaction(
                &tree.children[2],
                layout.children().nth(2).expect("action"),
                cursor,
                viewport,
                renderer,
            ))
        } else {
            base
        }
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
            layout.children().next().expect("content"),
            cursor,
            viewport,
        );
        let hovered = cursor.is_over(layout.bounds()) && cursor.is_over(*viewport);
        let (element, index) = if hovered {
            (&self.action, 2)
        } else {
            (&self.age, 1)
        };
        element.as_widget().draw(
            &tree.children[index],
            renderer,
            theme,
            style,
            layout.children().nth(index).expect("trailing"),
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
        if *tree.state.downcast_ref::<bool>() {
            self.action.as_widget_mut().overlay(
                &mut tree.children[2],
                layout.children().nth(2).expect("action"),
                renderer,
                viewport,
                translation,
            )
        } else {
            None
        }
    }
}
