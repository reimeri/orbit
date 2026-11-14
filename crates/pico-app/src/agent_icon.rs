//! A font-independent robot with a compact running-subagent indicator.
use iced::{
    Color, Element, Length, Rectangle, Size,
    advanced::{Layout, Renderer as _, Widget, layout, renderer, widget::Tree},
    mouse,
};

pub fn agent_icon<'a, Message: 'a>(running: bool) -> Element<'a, Message> {
    Element::new(AgentIcon { running })
}

struct AgentIcon {
    running: bool,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for AgentIcon {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(26.), Length::Fixed(26.))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(26., 26., Size::ZERO))
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let origin = layout.position();
        // Antenna, ears, eyes and mouth are simple rounded strokes.
        for (x, y, width, height) in [
            (12., 3., 2., 5.),
            (11., 2., 4., 3.),
            (1., 12., 2., 6.),
            (23., 12., 2., 6.),
            (7., 12., 3., 3.),
            (16., 12., 3., 3.),
            (9., 18., 8., 2.),
        ] {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(
                        origin + iced::Vector::new(x, y),
                        Size::new(width, height),
                    ),
                    border: iced::Border {
                        radius: 1.5.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                style.text_color,
            );
        }
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle::new(origin + iced::Vector::new(3., 7.), Size::new(20., 16.)),
                border: iced::Border {
                    color: style.text_color,
                    width: 1.5,
                    radius: 4.into(),
                },
                ..Default::default()
            },
            Color::TRANSPARENT,
        );
        if self.running {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(origin + iced::Vector::new(20., 0.), Size::new(6., 6.)),
                    border: iced::Border {
                        radius: 3.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                theme.palette().primary,
            );
        }
    }
}
