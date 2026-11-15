//! Identity-preserving, viewport-sized columns with optional row transitions.
//! Unlike iced's splice-based keyed column, equal-length sliding windows and
//! reorders move the actual child trees. Unmounted state is dropped immediately.
//! Animation stays widget-local, without app messages or an idle timer.
use iced::{
    Element, Event, Length, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
    mouse,
    time::{Duration, Instant},
    window,
};
use std::{collections::HashMap, hash::Hash};

const DURATION: Duration = Duration::from_millis(180);

pub fn keyed_column<'a, Key: Clone + Eq + Hash + 'static, Message: 'a>(
    rows: Vec<(Key, Element<'a, Message>)>,
) -> Element<'a, Message> {
    animated_column(
        rows.into_iter()
            .map(|(key, row)| (key, false, row))
            .collect(),
    )
}

/// Only rows marked `true` move. Headings and virtualization spacers snap.
pub fn animated_column<'a, Key: Clone + Eq + Hash + 'static, Message: 'a>(
    rows: Vec<(Key, bool, Element<'a, Message>)>,
) -> Element<'a, Message> {
    let keys = rows
        .iter()
        .map(|(key, animate, _)| (key.clone(), *animate))
        .collect();
    let children = rows.into_iter().map(|(_, _, row)| row).collect();
    Element::new(KeyedColumn { keys, children })
}

struct KeyedColumn<'a, Key, Message> {
    keys: Vec<(Key, bool)>,
    children: Vec<Element<'a, Message>>,
}

struct Motion {
    from: f32,
    to: f32,
    started: Instant,
}

impl Motion {
    fn y(&self, now: Instant) -> f32 {
        let t = ((now - self.started).as_secs_f32() / DURATION.as_secs_f32()).min(1.);
        self.to + (self.from - self.to) * (1. - t).powi(3)
    }

    fn active(&self, now: Instant) -> bool {
        self.from != self.to && now - self.started < DURATION
    }
}

struct State<Key> {
    now: Instant,
    rows: HashMap<Key, Motion>,
    keys: Vec<Key>,
    animating: bool,
}

impl<Key> Default for State<Key> {
    fn default() -> Self {
        Self {
            now: Instant::now(),
            rows: HashMap::new(),
            keys: Vec::new(),
            animating: false,
        }
    }
}

impl<Key: Clone + Eq + Hash> State<Key> {
    fn layout(&mut self, keys: &[(Key, bool)], node: layout::Node) -> layout::Node {
        if !keys.iter().any(|(_, animate)| *animate) {
            self.rows.clear();
            self.animating = false;
            return node;
        }
        // Keep only mounted rows: scrolling a large library must not accumulate
        // widget state, nor make a newly appearing row fly in from a stale position.
        let mut previous = std::mem::take(&mut self.rows);
        self.animating = false;
        let children = keys
            .iter()
            .zip(node.children())
            .map(|((key, animate), child)| {
                if !animate {
                    return child.clone();
                }
                let target = child.bounds().y;
                let mut motion = previous.remove(key).unwrap_or(Motion {
                    from: target,
                    to: target,
                    started: self.now,
                });
                if motion.to != target {
                    // Typing again mid-transition starts at the current interpolated
                    // position, not the previous destination.
                    motion = Motion {
                        from: motion.y(self.now),
                        to: target,
                        started: self.now,
                    };
                }
                let y = motion.y(self.now);
                self.animating |= motion.active(self.now);
                self.rows.insert(key.clone(), motion);
                child.clone().translate(Vector::new(0., y - target))
            })
            .collect();
        // Natural extent stays at the final layout; only row positions move.
        // Drawing, hit testing, sensors and operations all share this layout.
        layout::Node::with_children(node.size(), children)
    }
}

impl<Key: Clone + Eq + Hash + 'static, Message> Widget<Message, iced::Theme, iced::Renderer>
    for KeyedColumn<'_, Key, Message>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Key>>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::<Key> {
            keys: self.keys.iter().map(|(key, _)| key.clone()).collect(),
            ..State::<Key>::default()
        })
    }
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }
    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State<Key>>();
        state.now = Instant::now();
        let mut previous: HashMap<_, _> = state
            .keys
            .drain(..)
            .zip(std::mem::take(&mut tree.children))
            .collect();
        tree.children = self
            .keys
            .iter()
            .zip(&self.children)
            .map(|((key, _), child)| {
                let mut tree = previous.remove(key).unwrap_or_else(|| Tree::new(child));
                tree.diff(child);
                tree
            })
            .collect();
        state
            .keys
            .extend(self.keys.iter().map(|(key, _)| key.clone()));
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = layout::flex::resolve(
            layout::flex::Axis::Vertical,
            renderer,
            limits,
            Length::Fill,
            Length::Shrink,
            iced::Padding::ZERO,
            0.,
            iced::Alignment::Start,
            &mut self.children,
            &mut tree.children,
        );
        tree.state
            .downcast_mut::<State<Key>>()
            .layout(&self.keys, node)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, tree), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(tree, layout, renderer, operation);
            }
        });
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
        let state = tree.state.downcast_mut::<State<Key>>();
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            if state.animating {
                // iced may repeat this event after relayout. Advance only once
                // per timestamp so its layout-stabilization loop converges.
                if *now > state.now {
                    shell.invalidate_layout();
                }
                if state.rows.values().any(|motion| motion.active(*now)) {
                    shell.request_redraw();
                }
            }
            state.now = state.now.max(*now);
        }
        // Rows can cross during reordering. Last drawn is topmost, so it must
        // receive pointer input first (children respect event capture).
        for ((child, tree), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .rev()
        {
            child.as_widget_mut().update(
                tree, event, layout, cursor, renderer, clipboard, shell, viewport,
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
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .rev()
            .map(|((child, tree), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .find(|interaction| *interaction != mouse::Interaction::None)
            .unwrap_or_default()
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
        for ((child, tree), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
        {
            child
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, iced::Theme, iced::Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
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
        widget::{button, mouse_area},
    };

    fn targets(rows: &[(usize, bool, f32)]) -> (Vec<(usize, bool)>, layout::Node) {
        let keys = rows.iter().map(|&(id, animate, _)| (id, animate)).collect();
        let children = rows
            .iter()
            .map(|&(_, _, y)| layout::Node::new(Size::new(200., 40.)).move_to(Point::new(0., y)))
            .collect();
        (
            keys,
            layout::Node::with_children(Size::new(200., 300.), children),
        )
    }

    fn positions(state: &mut State<usize>, rows: &[(usize, bool, f32)]) -> Vec<f32> {
        let (keys, node) = targets(rows);
        state
            .layout(&keys, node)
            .children()
            .iter()
            .map(|node| node.bounds().y)
            .collect()
    }

    #[test]
    fn filtering_retargeting_and_reordering_follow_session_identity() {
        let mut state = State::default();
        let start = state.now;
        assert_eq!(
            positions(
                &mut state,
                &[
                    (0, false, 0.),
                    (11, true, 36.),
                    (22, true, 100.),
                    (33, true, 180.)
                ]
            ),
            [0., 36., 100., 180.]
        );
        assert!(!state.animating);

        // Filter out 11. The heading snaps; surviving sessions keep their positions.
        assert_eq!(
            positions(
                &mut state,
                &[(0, false, 8.), (22, true, 44.), (33, true, 124.)]
            ),
            [8., 100., 180.]
        );
        state.now = start + DURATION / 2;
        let halfway = positions(
            &mut state,
            &[(0, false, 8.), (22, true, 44.), (33, true, 124.)],
        );
        assert_eq!(halfway[0], 8.);
        assert!(halfway[1] > 44. && halfway[1] < 100.);
        assert!(halfway[2] > 124. && halfway[2] < 180.);

        // Retarget without jumping. Returning 11 is new, not an animation from stale state.
        assert_eq!(
            positions(
                &mut state,
                &[(33, true, 36.), (11, true, 100.), (22, true, 180.)]
            ),
            [halfway[2], 100., halfway[1]]
        );
        state.now = start + DURATION;
        let retargeted = positions(
            &mut state,
            &[(33, true, 36.), (11, true, 100.), (22, true, 180.)],
        );
        assert!(retargeted[0] > 36. && retargeted[0] < halfway[2]);
        assert_eq!(retargeted[1], 100.);
        assert!(retargeted[2] > halfway[1] && retargeted[2] < 180.);
        state.now = start + DURATION + DURATION / 2;
        assert_eq!(
            positions(
                &mut state,
                &[(33, true, 36.), (11, true, 100.), (22, true, 180.)]
            ),
            [36., 100., 180.]
        );
        assert!(!state.animating);
    }

    #[test]
    fn virtual_viewport_changes_do_not_animate_scrolling_or_retain_old_rows() {
        let mut state = State::default();
        // The top spacer changes but surviving rows' content coordinates do not.
        positions(
            &mut state,
            &[(0, false, 0.), (50, true, 500.), (51, true, 560.)],
        );
        assert_eq!(
            positions(
                &mut state,
                &[(0, false, 0.), (51, true, 560.), (52, true, 620.)]
            ),
            [0., 560., 620.]
        );
        assert!(!state.animating);
        assert_eq!(state.rows.len(), 2);
        assert!(!state.rows.contains_key(&50));
        positions(&mut state, &[]);
        assert!(state.rows.is_empty());
        assert!(!state.animating);
    }

    fn buttons(rows: &[(usize, f32)]) -> Element<'static, usize> {
        animated_column(
            rows.iter()
                .map(|&(id, height)| {
                    (
                        id,
                        true,
                        mouse_area(button("session").width(200).height(height).on_press(id))
                            .on_right_press(id + 100)
                            .into(),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn animation_frames_move_hit_targets_and_stop_requesting_redraws() {
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ));
        let limits = layout::Limits::new(Size::ZERO, Size::new(200., 400.));
        let viewport = Rectangle::with_size(Size::new(200., 400.));
        let mut content = buttons(&[(1, 80.), (2, 80.), (3, 40.)]);
        let mut tree = Tree::new(&content);
        let start = tree.state.downcast_ref::<State<usize>>().now;
        content
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        content = buttons(&[(3, 40.)]);
        content.as_widget().diff(&mut tree);
        tree.state.downcast_mut::<State<usize>>().now = start;
        let mut node = content
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        assert_eq!(node.children()[0].bounds().y, 160.);
        assert_eq!(
            node.size().height,
            40.,
            "scroll extent uses the final layout"
        );

        for (elapsed, redraw) in [
            (DURATION / 2, true),
            (DURATION, false),
            (DURATION * 2, false),
        ] {
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            content.as_widget_mut().update(
                &mut tree,
                &Event::Window(window::Event::RedrawRequested(start + elapsed)),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &viewport,
            );
            assert_eq!(
                shell.redraw_request() == window::RedrawRequest::NextFrame,
                redraw
            );
            assert_eq!(shell.is_layout_invalid(), elapsed <= DURATION);
            if shell.is_layout_invalid() {
                node = content
                    .as_widget_mut()
                    .layout(&mut tree, &renderer, &limits);
            }
            let mut shell = Shell::new(&mut messages);
            content.as_widget_mut().update(
                &mut tree,
                &Event::Window(window::Event::RedrawRequested(start + elapsed)),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &viewport,
            );
            assert!(
                !shell.is_layout_invalid(),
                "repeated redraw must converge after relayout"
            );
            let row = node.children()[0].bounds();
            if redraw {
                assert!(
                    row.y > 0. && row.y < 160.,
                    "row moves toward its final position"
                );
            } else {
                assert_eq!(row.y, 0., "row reaches its final position");
            }
            assert!(
                messages.is_empty(),
                "animation frames must not publish app messages"
            );

            if redraw {
                // Input follows the row's current position, not its old/final layout.
                let cursor = mouse::Cursor::Available(row.center());
                for event in [
                    mouse::Event::ButtonPressed(mouse::Button::Left),
                    mouse::Event::ButtonReleased(mouse::Button::Left),
                ] {
                    content.as_widget_mut().update(
                        &mut tree,
                        &Event::Mouse(event),
                        Layout::new(&node),
                        cursor,
                        &renderer,
                        &mut clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &viewport,
                    );
                }
                assert_eq!(messages, [3]);
                // Exercise the same transformed layout through the drawing path.
                content.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &iced::Theme::Dark,
                    &renderer::Style {
                        text_color: iced::Color::WHITE,
                    },
                    Layout::new(&node),
                    cursor,
                    &viewport,
                );
            }
        }
    }
    #[test]
    fn crossing_rows_select_and_open_the_menu_of_the_topmost_session() {
        let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ));
        let limits = layout::Limits::new(Size::ZERO, Size::new(200., 400.));
        let viewport = Rectangle::with_size(Size::new(200., 400.));
        let mut content = buttons(&[(1, 40.), (2, 40.)]);
        let mut tree = Tree::new(&content);
        let start = tree.state.downcast_ref::<State<usize>>().now;
        content
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        content = buttons(&[(2, 40.), (1, 40.)]);
        content.as_widget().diff(&mut tree);
        tree.state.downcast_mut::<State<usize>>().now = start;
        content
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        tree.state.downcast_mut::<State<usize>>().now = start + DURATION / 2;
        let node = content
            .as_widget_mut()
            .layout(&mut tree, &renderer, &limits);
        let overlap = node.children()[0]
            .bounds()
            .intersection(&node.children()[1].bounds())
            .expect("crossing rows overlap");
        let cursor = mouse::Cursor::Available(overlap.center());
        let mut messages = Vec::new();
        for event in [
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::ButtonReleased(mouse::Button::Left),
            mouse::Event::ButtonPressed(mouse::Button::Right),
        ] {
            content.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(event),
                Layout::new(&node),
                cursor,
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &viewport,
            );
        }
        assert_eq!(
            messages,
            [1, 101],
            "last drawn session must win overlapping input"
        );
    }
}
