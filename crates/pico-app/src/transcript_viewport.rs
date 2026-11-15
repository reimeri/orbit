//! Observe the actual scroll viewport, even when its content does not overflow.
//!
//! Place directly inside the chat scrollable: iced supplies its viewport in
//! content coordinates, including the effective (clamped) scroll translation.
//! Unlike `Scrollable::on_scroll`, this also reports fitting content. Epochs
//! ensure a new transcript/reflow receives geometry even if its size is unchanged.
use iced::{
    Element, Event, Length, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
    mouse, window,
};

pub fn observe<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    epoch: u64,
    on_change: impl Fn(f32, f32, f32) -> Message + 'a,
) -> Element<'a, Message> {
    Element::new(Viewport {
        content: content.into(),
        epoch,
        on_change: Box::new(on_change),
    })
}

struct Viewport<'a, Message> {
    content: Element<'a, Message>,
    epoch: u64,
    on_change: Box<dyn Fn(f32, f32, f32) -> Message + 'a>,
}

#[derive(Default)]
struct State {
    last: Option<(u64, Rectangle, Rectangle)>,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Viewport<'_, Message> {
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
        if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            let bounds = layout.bounds();
            let geometry = (self.epoch, *viewport, bounds);
            let state = tree.state.downcast_mut::<State>();
            if state.last != Some(geometry) {
                state.last = Some(geometry);
                // Publish before row measurements so bottom-following uses the
                // actual viewport height from the very first frame.
                shell.publish((self.on_change)(
                    viewport.y - bounds.y,
                    viewport.height,
                    // The gutter container may be taller than fitting content;
                    // scroll extent still includes at least the whole viewport.
                    bounds.height.max(viewport.height),
                ));
            }
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
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
        advanced::clipboard,
        widget::{container, space},
    };
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn reports_fitting_scrolling_resizing_and_new_epochs_without_idle_messages() {
        let mut renderer = crate::test_renderer::new(16.);
        let mut cache = Cache::new();
        let mut frame = |epoch, content_height, height, wheel| {
            let content = observe(
                space().height(content_height).width(Length::Fill),
                epoch,
                |offset, height, content| (offset, height, content),
            );
            // Nonzero origin ensures offsets are content-relative, not window Y.
            let view = container(crate::scrollbar::scroll(content).height(height)).padding(17);
            let mut ui = UserInterface::build(
                view,
                Size::new(500., 1400.),
                std::mem::take(&mut cache),
                &mut renderer,
            );
            let mut events = vec![];
            if let Some(y) = wheel {
                events.push(Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Pixels { x: 0., y },
                }));
            }
            events.push(Event::Window(window::Event::RedrawRequested(
                iced::time::Instant::now(),
            )));
            let mut messages = vec![];
            ui.update(
                &events,
                mouse::Cursor::Available(Point::new(100., 100.)),
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            cache = ui.into_cache();
            messages
        };
        // Iced gives the content a minimum height equal to the viewport.
        assert_eq!(frame(0, 800., 1200., None), [(0., 1200., 1200.)]);
        assert!(frame(0, 800., 1200., None).is_empty());
        assert!(frame(0, 800., 1200., Some(-90.)).is_empty());
        assert_eq!(frame(1, 800., 1200., None), [(0., 1200., 1200.)]);
        assert_eq!(frame(1, 800., 300., None), [(0., 300., 800.)]);
        assert_eq!(frame(1, 800., 300., Some(-90.)), [(90., 300., 800.)]);
        assert!(frame(1, 800., 300., None).is_empty());
        // No wheel event: layout alone clamps an old scroll offset when content fits.
        assert_eq!(frame(1, 200., 300., None), [(0., 300., 300.)]);
        assert_eq!(frame(1, 200., 1200., None), [(0., 1200., 1200.)]);
        assert!(frame(1, 200., 1200., None).is_empty());
    }
}
