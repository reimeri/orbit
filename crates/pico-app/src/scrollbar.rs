//! Shared, quiet scrollbars with a wider hit area than their visible thumb.
use crate::theme;
use iced::{
    Element, Padding, Theme, border,
    widget::{Scrollable, container, scrollable},
};

/// Always reserve the rail and its gap, even when content fits. Iced's embedded
/// scrollbar spacing only reserves space while overflowing, which causes reflow.
pub const GUTTER: f32 = theme::SCROLLBAR_WIDTH + theme::SCROLLBAR_GAP;

pub fn scroll<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Scrollable<'a, Message> {
    scrollable(container(content).padding(Padding::ZERO.right(GUTTER)))
        .direction(scrollable::Direction::Vertical(rail()))
        .style(style)
}

pub fn horizontal<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Scrollable<'a, Message> {
    scrollable(content)
        .direction(scrollable::Direction::Horizontal(rail()))
        .style(style)
}

pub fn rail() -> scrollable::Scrollbar {
    scrollable::Scrollbar::new()
        .width(theme::SCROLLBAR_WIDTH)
        .scroller_width(theme::SCROLLBAR_THUMB_WIDTH)
}

pub fn thumb(theme: &Theme, active: bool) -> scrollable::Scroller {
    scrollable::Scroller {
        background: theme
            .palette()
            .text
            .scale_alpha(if active {
                theme::SCROLLBAR_ACTIVE_ALPHA
            } else {
                theme::SCROLLBAR_ALPHA
            })
            .into(),
        border: border::rounded(theme::SCROLLBAR_THUMB_WIDTH / 2.),
    }
}

fn style(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let (horizontal, vertical) = match status {
        scrollable::Status::Active { .. } => (false, false),
        scrollable::Status::Hovered {
            is_horizontal_scrollbar_hovered,
            is_vertical_scrollbar_hovered,
            ..
        } => (
            is_horizontal_scrollbar_hovered,
            is_vertical_scrollbar_hovered,
        ),
        scrollable::Status::Dragged {
            is_horizontal_scrollbar_dragged,
            is_vertical_scrollbar_dragged,
            ..
        } => (
            is_horizontal_scrollbar_dragged,
            is_vertical_scrollbar_dragged,
        ),
    };
    let rail = |active| scrollable::Rail {
        background: None,
        border: border::Border::default(),
        scroller: thumb(theme, active),
    };
    scrollable::Style {
        vertical_rail: rail(vertical),
        horizontal_rail: rail(horizontal),
        ..scrollable::default(theme, status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{
        Event, Font,
        Length::Fill,
        Pixels, Point, Rectangle, Size,
        advanced::{
            Renderer as _, clipboard, renderer,
            renderer::Headless,
            widget::{Id, Operation},
        },
        mouse,
        widget::{button, column, space},
    };
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[derive(Default)]
    struct Bounds(Option<Rectangle>, Option<Rectangle>);

    impl Operation for Bounds {
        fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
            visit(self);
        }
        fn scrollable(
            &mut self,
            _: Option<&Id>,
            bounds: Rectangle,
            _: Rectangle,
            _: iced::Vector,
            _: &mut dyn iced::advanced::widget::operation::Scrollable,
        ) {
            self.1 = Some(bounds);
        }
        fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
            if id == Some(&Id::new("content")) {
                self.0 = Some(bounds);
            }
        }
    }

    #[test]
    fn vertical_gutter_is_stable_and_does_not_activate_content() {
        let mut renderer =
            iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(Font::DEFAULT, Pixels(16.)));
        let mut cache = Cache::new();
        // Reuse widget state through overflow, shrinkage, and panel resizing.
        for width in [240., 137.] {
            for height in [30., 400., 30.] {
                let content = container(column![
                    button("Session").width(Fill).height(30).on_press(()),
                    space().height(height - 30.),
                ])
                .id("content")
                .width(Fill);
                let mut ui = UserInterface::build(
                    scroll(content).height(100),
                    Size::new(width, 100.),
                    cache,
                    &mut renderer,
                );
                let mut bounds = Bounds::default();
                ui.operate(&renderer, &mut bounds);
                let content = bounds.0.unwrap();
                assert_eq!(content.x, 0.);
                assert_eq!(content.width, width - GUTTER);

                let theme = theme::app();
                renderer.reset(Rectangle::with_size(Size::new(width, 100.)));
                ui.draw(
                    &mut renderer,
                    &theme,
                    &renderer::Style {
                        text_color: theme::TEXT,
                    },
                    mouse::Cursor::Unavailable,
                );
                let pixels =
                    renderer.screenshot(Size::new(width as u32, 100), 1., iced::Color::BLACK);
                let mut painted_x = Vec::new();
                for y in 0..100 {
                    for x in (content.x + content.width).ceil() as usize..width as usize {
                        let pixel = &pixels[(y * width as usize + x) * 4..][..3];
                        if pixel.iter().any(|&channel| channel != 0) {
                            painted_x.push(x as f32);
                        }
                    }
                }
                assert_eq!(
                    !painted_x.is_empty(),
                    height > 100.,
                    "thumb appears only for overflow"
                );
                if !painted_x.is_empty() {
                    let left = painted_x.iter().copied().fold(f32::INFINITY, f32::min);
                    let right = painted_x.iter().copied().fold(0., f32::max) + 1.;
                    let expected_center = width - theme::SCROLLBAR_WIDTH / 2.;
                    assert!(((left + right) / 2. - expected_center).abs() <= 1.);
                    assert!((right - left - theme::SCROLLBAR_THUMB_WIDTH).abs() <= 1.);
                    assert!(left >= content.x + content.width + theme::SCROLLBAR_GAP);
                }

                // Both the gap and scrollbar hit area stay outside the row;
                // contrast with an actual click on its rightmost pixel.
                let content_right = content.x + content.width;
                for (x, expected) in [
                    (content_right + theme::SCROLLBAR_GAP / 2., 0),
                    (width - theme::SCROLLBAR_WIDTH / 2., 0),
                    (content_right - 1., 1),
                ] {
                    let mut messages = Vec::new();
                    ui.update(
                        &[
                            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                        ],
                        mouse::Cursor::Available(Point::new(x, 15.)),
                        &mut renderer,
                        &mut clipboard::Null,
                        &mut messages,
                    );
                    assert_eq!(messages.len(), expected);
                }
                cache = ui.into_cache();
            }
        }
    }

    #[test]
    fn builtin_pick_list_menus_reserve_a_gutter_even_when_they_fit() {
        let mut renderer =
            iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(Font::DEFAULT, Pixels(16.)));
        let theme = theme::app();
        let style = theme::pick_list_menu(&theme);
        for count in [2, 40] {
            let mut ui = UserInterface::build(
                container(
                    iced::widget::pick_list((0..count).collect::<Vec<_>>(), Some(0), |_| ())
                        .width(180)
                        .style(theme::pick_list)
                        .menu_style(theme::pick_list_menu),
                )
                .id("content"),
                Size::new(240., 200.),
                Cache::new(),
                &mut renderer,
            );
            ui.update(
                &[
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(Point::new(30., 15.)),
                &mut renderer,
                &mut clipboard::Null,
                &mut Vec::new(),
            );
            let mut bounds = Bounds::default();
            ui.operate(&renderer, &mut bounds);
            let control = bounds.0.unwrap();
            let point = Point::new(control.x + 10., control.y + control.height + 10.);
            ui.update(
                &[Event::Mouse(mouse::Event::CursorMoved { position: point })],
                mouse::Cursor::Available(point),
                &mut renderer,
                &mut clipboard::Null,
                &mut Vec::new(),
            );
            ui.draw(
                &mut renderer,
                &theme,
                &renderer::Style {
                    text_color: theme::TEXT,
                },
                mouse::Cursor::Unavailable,
            );
            let pixels = renderer.screenshot(Size::new(240, 200), 1., iced::Color::BLACK);
            let iced::Background::Color(selected) = style.selected_background else {
                panic!("fixture theme must have a solid selection background")
            };
            let selected = selected.into_rgba8();
            let row = &pixels[point.y.floor() as usize * 240 * 4..][..240 * 4];
            let right = row
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .filter(|(_, pixel)| (0..3).all(|c| pixel[c].abs_diff(selected[c]) <= 1))
                .map(|(x, _)| x + 1)
                .next_back()
                .expect("menu must paint the selected row") as f32;
            let content_right = control.x + control.width - GUTTER - style.border.width;
            assert!(
                (right - content_right).abs() <= 1.,
                "selection must stop before the gutter"
            );
        }
    }

    #[test]
    fn horizontal_scroll_does_not_reserve_a_vertical_gutter() {
        let mut renderer =
            iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(Font::DEFAULT, Pixels(16.)));
        let mut ui = UserInterface::<(), _, _>::build(
            horizontal(container(space().width(83).height(20)).id("content")),
            Size::new(240., 100.),
            Cache::new(),
            &mut renderer,
        );
        let mut bounds = Bounds::default();
        ui.operate(&renderer, &mut bounds);
        assert_eq!(bounds.1.unwrap().width, 83.);
        assert_eq!(bounds.0.unwrap().width, 83.);
    }
}
