//! Font-independent disclosure chevrons shared by all expandable chat rows.
use iced::{
    Color, Element, Length, Rectangle, Size,
    advanced::{Layout, Renderer as _, Widget, layout, renderer, widget::Tree},
    mouse,
};

pub fn chevron_icon<'a, Message: 'a>(open: bool, color: Color) -> Element<'a, Message> {
    Element::new(Chevron { open, color })
}

struct Chevron {
    open: bool,
    color: Color,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Chevron {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(12.), Length::Fixed(12.))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(12., 12., Size::ZERO))
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        // As with the search icon's handle, overlapping round dots draw smooth
        // diagonal strokes on both renderers without a font or graphics feature.
        for step in 0..=16 {
            let along = 2. + step as f32 * 0.5;
            let across = 8. - (along - 6.).abs();
            let (x, y) = if self.open {
                (along, across)
            } else {
                (across, along)
            };
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(
                        layout.position() + iced::Vector::new(x - 0.75, y - 0.75),
                        Size::new(1.5, 1.5),
                    ),
                    border: iced::Border {
                        radius: 0.75.into(),
                        ..Default::default()
                    },
                    snap: false,
                    ..Default::default()
                },
                self.color,
            );
        }
    }
}
