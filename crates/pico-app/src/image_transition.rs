//! Geometry captured at activation, before scrolling coordinates are resolved at the root.
use iced::{
    Element, Length, Point, Rectangle, Size, Vector,
    advanced::{
        Layout, Widget, image as primitive, layout, renderer,
        widget::{Tree, tree},
    },
    mouse,
    widget::image,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::ui::Message;

#[derive(Clone, Debug)]
pub struct Source {
    pub bounds: Rectangle,
    pub clip: Rectangle,
    pub handle: image::Handle,
    hidden: Arc<AtomicBool>,
}

impl Source {
    pub fn translate(&mut self, offset: Vector) {
        self.bounds = self.bounds + offset;
        self.clip = self.clip + offset;
    }

    pub fn hide(&self, hidden: bool) {
        self.hidden.store(hidden, Ordering::Relaxed);
    }
}

/// Wrap a button whose single descendant chain ends in a contain-fitted image.
/// Keeping the button preserves its normal press/release and touch behavior.
pub fn preview<'a>(
    content: impl Into<Element<'a, Message>>,
    handle: image::Handle,
) -> Element<'a, Message> {
    Element::new(Preview {
        content: content.into(),
        handle,
    })
}

struct Preview<'a> {
    content: Element<'a, Message>,
    handle: image::Handle,
}

impl Widget<Message, iced::Theme, iced::Renderer> for Preview<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Arc<AtomicBool>>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Arc::new(AtomicBool::new(false)))
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
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        use primitive::Renderer as _;
        let mut messages = Vec::new();
        let mut local = iced::advanced::Shell::new(&mut messages);
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            &mut local,
            viewport,
        );
        shell.merge(local, |message| {
            if matches!(
                message,
                Message::OpenImage(_) | Message::OpenMarkdownImage(_)
            ) && let Some(position) = cursor.position()
            {
                let mut picture = layout;
                while let Some(child) = picture.children().next() {
                    picture = child;
                }
                let bounds = picture.bounds();
                let natural = renderer
                    .measure_image(&self.handle)
                    .expect("decoded preview");
                let fitted = iced::ContentFit::Contain.fit(
                    Size::new(natural.width as f32, natural.height as f32),
                    bounds.size(),
                );
                let bounds = Rectangle::new(
                    Point::new(
                        bounds.center_x() - fitted.width / 2.,
                        bounds.center_y() - fitted.height / 2.,
                    ),
                    fitted,
                );
                // Scrollable translates both the cursor and viewport. Store relative
                // geometry; PointerInput restores window coordinates at the root.
                let mut source = Source {
                    bounds,
                    clip: *viewport,
                    handle: self.handle.clone(),
                    hidden: tree.state.downcast_ref::<Arc<AtomicBool>>().clone(),
                };
                source.translate(Vector::new(-position.x, -position.y));
                Message::OpenImageFrom(Box::new(message), source)
            } else {
                message
            }
        });
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
        if !tree
            .state
            .downcast_ref::<Arc<AtomicBool>>()
            .load(Ordering::Relaxed)
        {
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
    }
}

pub fn interpolate(from: Rectangle, to: Rectangle, amount: f32) -> Rectangle {
    Rectangle {
        x: from.x + (to.x - from.x) * amount,
        y: from.y + (to.y - from.y) * amount,
        width: from.width + (to.width - from.width) * amount,
        height: from.height + (to.height - from.height) * amount,
    }
}

pub struct Flight {
    pub handle: image::Handle,
    pub bounds: Rectangle,
    pub clip: Rectangle,
    pub opacity: f32,
}

impl Widget<Message, iced::Theme, iced::Renderer> for Flight {
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
    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        use primitive::Renderer as _;
        if let Some(clip) = self.clip.intersection(viewport) {
            renderer.with_layer(clip, |renderer| {
                renderer.draw_image(
                    primitive::Image::new(self.handle.clone())
                        .opacity(self.opacity)
                        .snap(false),
                    self.bounds,
                    clip,
                );
            });
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use iced::{
        Event,
        advanced::{clipboard, widget::operation},
        widget::{column, container, scrollable, space},
    };

    pub fn activate(content: Element<'_, Message>, position: Point) -> Message {
        let mut renderer = crate::test_renderer::new(16.);
        let mut ui = iced_runtime::UserInterface::build(
            crate::pointer_input::pointer_input(content, false, Default::default()),
            Size::new(800., 600.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        ui.update(
            &[
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(position),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(messages.len(), 1);
        messages.remove(0)
    }

    #[test]
    fn scrolled_preview_captures_painted_bounds_and_clip_in_window_coordinates() {
        let handle = image::Handle::from_rgba(240, 80, vec![255; 240 * 80 * 4]);
        let content = preview(
            crate::theme::button(image(handle.clone()).width(100).height(100))
                .padding(0)
                .on_press(Message::OpenMarkdownImage("fixture.png".into())),
            handle,
        );
        let mut renderer = crate::test_renderer::new(16.);
        let mut ui = iced_runtime::UserInterface::build(
            crate::pointer_input::pointer_input(
                container(
                    scrollable(column![space().height(500), content])
                        .id("images")
                        .width(200)
                        .height(100),
                )
                .padding(17)
                .into(),
                false,
                Default::default(),
            ),
            Size::new(800., 600.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        ui.operate(
            &renderer,
            &mut operation::scrollable::scroll_to(
                "images".into(),
                operation::scrollable::AbsoluteOffset {
                    x: Some(0.),
                    y: Some(450.),
                },
            ),
        );
        let mut messages = Vec::new();
        ui.update(
            &[
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(Point::new(37., 107.)),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let [Message::OpenImageFrom(message, source)] = messages.as_slice() else {
            panic!("{messages:?}")
        };
        assert!(
            matches!(message.as_ref(), Message::OpenMarkdownImage(url) if url == "fixture.png")
        );
        assert_eq!(source.bounds.x, 17.);
        assert!((source.bounds.y - (17. + 50. + 100. / 3.)).abs() < 0.001);
        assert!((source.bounds.width - 100.).abs() < 0.001);
        assert!((source.bounds.height - 100. / 3.).abs() < 0.001);
        assert_eq!(
            source.clip,
            Rectangle::new(Point::new(17., 17.), Size::new(200., 100.))
        );
        assert!(source.bounds.intersection(&source.clip).unwrap().height < source.bounds.height);
        assert_eq!(
            interpolate(
                source.bounds,
                Rectangle::with_size(Size::new(600., 200.)),
                0.
            ),
            source.bounds
        );
    }

    #[test]
    fn flight_clips_without_stretching_a_partly_visible_image() {
        use iced::advanced::Renderer as _;
        // The red lower half must stay clipped, not be squeezed into the visible area.
        let pixels: Vec<u8> = (0..40)
            .flat_map(|y| {
                (0..120).flat_map(move |_| {
                    if y < 20 {
                        [255, 255, 255, 255]
                    } else {
                        [255, 0, 0, 255]
                    }
                })
            })
            .collect();
        let handle = image::Handle::from_rgba(120, 40, pixels);
        let bounds = Rectangle::new(Point::new(20., 80.), Size::new(120., 40.));
        let clip = Rectangle::with_size(Size::new(200., 100.));
        let mut renderer = crate::test_renderer::new(16.);
        let mut ui = iced_runtime::UserInterface::build(
            Element::new(Flight {
                handle,
                bounds,
                clip,
                opacity: 1.,
            }),
            Size::new(200., 150.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        renderer.reset(Rectangle::with_size(Size::new(200., 150.)));
        ui.draw(
            &mut renderer,
            &crate::theme::app(),
            &renderer::Style {
                text_color: iced::Color::WHITE,
            },
            mouse::Cursor::Unavailable,
        );
        let pixels = crate::test_renderer::screenshot(
            &mut renderer,
            Size::new(200, 150),
            1.,
            iced::Color::BLACK,
        );
        let pixel = |x: usize, y: usize| &pixels[(y * 200 + x) * 4..][..3];
        assert_eq!(pixel(30, 90), &[255, 255, 255]);
        assert_eq!(pixel(30, 110), &[0, 0, 0]);
        assert_eq!(pixel(10, 90), &[0, 0, 0]);
    }
}
