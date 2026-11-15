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

/// An overlay spanning the group header so hovering anywhere on the button
/// reveals its disclosure. The base header reserves the 12 px icon slot.
pub fn activity_disclosure<'a, Message: 'a>(
    open: bool,
    working: bool,
    tick: u64,
) -> Element<'a, Message> {
    Element::new(ActivityDisclosure {
        chevron: chevron_icon(open, crate::theme::MUTED),
        indicator: crate::status_circle::status_circle(working, tick),
        working,
    })
}

struct ActivityDisclosure<'a, Message> {
    chevron: Element<'a, Message>,
    indicator: Element<'a, Message>,
    working: bool,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for ActivityDisclosure<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.chevron), Tree::new(&self.indicator)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.chevron, &self.indicator]);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.max();
        let children = [&mut self.chevron, &mut self.indicator]
            .into_iter()
            .zip(&mut tree.children)
            .map(|(icon, tree)| {
                let node = icon.as_widget_mut().layout(tree, renderer, limits);
                let y = (size.height - node.size().height) / 2.;
                node.move_to(iced::Point::new(2., y))
            })
            .collect();
        layout::Node::with_children(size, children)
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
        let indicator = self.working && !cursor.is_over(layout.bounds());
        let index = usize::from(indicator);
        let icon = if indicator {
            &self.indicator
        } else {
            &self.chevron
        };
        icon.as_widget().draw(
            &tree.children[index],
            renderer,
            theme,
            style,
            layout.children().nth(index).expect("disclosure icon"),
            cursor,
            viewport,
        );
    }
}
