//! Lucide disclosures with header-wide hover behavior for activity groups.
use iced::{
    Element, Length, Rectangle, Size,
    advanced::{Layout, Widget, layout, renderer, widget::Tree},
    mouse,
};

pub fn chevron_icon<'a, Message: 'a>(open: bool) -> Element<'a, Message> {
    (if open {
        crate::icons::Icon::ChevronDown
    } else {
        crate::icons::Icon::ChevronRight
    })
    .view(12.)
}

/// An overlay spanning the group header so hovering anywhere on the button
/// reveals its disclosure. The base header reserves the 12 px icon slot.
pub fn activity_disclosure<'a, Message: 'a>(
    open: bool,
    working: bool,
    tick: u64,
) -> Element<'a, Message> {
    Element::new(ActivityDisclosure {
        chevron: chevron_icon(open),
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
