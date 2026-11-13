//! A small, font-independent magnifying glass for the sidebar control.
use iced::{
    Color, Element, Length, Rectangle, Size,
    advanced::{Layout, Renderer as _, Widget, layout, renderer, widget::Tree},
    mouse,
};

pub fn search_icon<'a, Message: 'a>() -> Element<'a, Message> {
    Element::new(SearchIcon)
}

struct SearchIcon;

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for SearchIcon {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(14.), Length::Fixed(14.))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(14., 14., Size::ZERO))
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let origin = layout.position();
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle::new(origin, Size::new(11., 11.)),
                border: iced::Border {
                    color: style.text_color,
                    width: 1.,
                    radius: 5.5.into(),
                },
                ..Default::default()
            },
            Color::TRANSPARENT,
        );
        // Overlapping round dots form the short diagonal handle without a font
        // dependency or an additional graphics feature/backend.
        for step in 0..7 {
            let offset = 9. + step as f32 * 0.5;
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(
                        origin + iced::Vector::new(offset, offset),
                        Size::new(1.5, 1.5),
                    ),
                    border: iced::Border {
                        radius: 0.75.into(),
                        ..Default::default()
                    },
                    snap: false,
                    ..Default::default()
                },
                style.text_color,
            );
        }
    }
}
