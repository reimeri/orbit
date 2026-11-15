//! Font-independent disclosure chevrons and back navigation arrow.
use iced::{
    Color, Element, Length, Rectangle, Size,
    advanced::{Layout, Renderer as _, Widget, layout, renderer, widget::Tree},
    mouse,
};

pub fn chevron_icon<'a, Message: 'a>(open: bool, color: Color) -> Element<'a, Message> {
    Element::new(Chevron {
        direction: if open {
            Direction::Down
        } else {
            Direction::Right
        },
        color,
    })
}

pub fn back_arrow_icon<'a, Message: 'a>(color: Color) -> Element<'a, Message> {
    Element::new(Chevron {
        direction: Direction::Back,
        color,
    })
}

enum Direction {
    Right,
    Down,
    Back,
}

struct Chevron {
    direction: Direction,
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
        if matches!(self.direction, Direction::Back) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(
                        layout.position() + iced::Vector::new(2., 5.25),
                        Size::new(9., 1.5),
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
        // As with the search icon's handle, overlapping round dots draw smooth
        // diagonal strokes on both renderers without a font or graphics feature.
        for step in 0..=16 {
            let along = 2. + step as f32 * 0.5;
            let across = 8. - (along - 6.).abs();
            let (x, y) = match self.direction {
                Direction::Down => (along, across),
                Direction::Right => (across, along),
                Direction::Back => (10. - across, along),
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
