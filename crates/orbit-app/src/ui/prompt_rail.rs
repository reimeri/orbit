//! A virtual prompt navigator. Hover requests a redraw, not an application
//! update: no transcript layout or per-prompt widgets are needed.
use super::Message;
use crate::theme;
use iced::{
    Element, Event, Length, Point, Rectangle, Size,
    advanced::{
        Clipboard, Layout, Renderer as _, Shell, Widget, layout, renderer, text,
        widget::{Tree, tree},
    },
    border, mouse, widget,
};
use orbit_core::store::{Entry, Session};
use std::{cell::RefCell, sync::Arc};

const PITCH: f32 = 12.;
const INSET: f32 = 8.;
const MIN_TICK: f32 = 7.;
const MAX_TICK: f32 = 30.;
const WIDTH: f32 = INSET + MAX_TICK + INSET;

pub(super) fn rail<'a>(
    session: &'a Session,
    prompts: &'a [usize],
    chat_width: f32,
) -> Element<'a, Message> {
    Element::new(Rail {
        session,
        prompts,
        chat_width,
    })
}

struct Rail<'a> {
    session: &'a Session,
    prompts: &'a [usize],
    chat_width: f32,
}

struct State {
    session: String,
    // Fractional slots accumulate high-resolution trackpad input.
    offset: f64,
    slots: usize,
    pane_height: f32,
    hover_y: Option<f32>,
    pressed: Option<String>,
    preview: RefCell<Option<Preview>>,
}

struct Preview {
    source: Arc<Entry>,
    paragraph: text::paragraph::Plain<<iced::Renderer as text::Renderer>::Paragraph>,
    size: Size,
}

impl Preview {
    fn new(source: Arc<Entry>, renderer: &iced::Renderer) -> Self {
        // Bound both copying and shaping, even for a very large single-line prompt.
        let mut chars = source.text.chars();
        let mut label: String = chars
            .by_ref()
            .take(90)
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect();
        if chars.next().is_some() {
            label.push('…');
        }
        if label.trim().is_empty() {
            label = "Attachment message".into();
        }
        let mut paragraph = Default::default();
        let size = widget::text::layout::<iced::Renderer>(
            &mut paragraph,
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(270., f32::INFINITY)),
            &label,
            widget::text::Format {
                size: Some(13.into()),
                wrapping: text::Wrapping::WordOrGlyph,
                ..Default::default()
            },
        )
        .size();
        Self {
            source,
            paragraph,
            size,
        }
    }
}

fn tick_length(distance: f32) -> f32 {
    let t = (1. - distance / (3. * PITCH)).clamp(0., 1.);
    MIN_TICK + (MAX_TICK - MIN_TICK) * t * t * (3. - 2. * t)
}

impl Widget<Message, iced::Theme, iced::Renderer> for Rail<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            session: self.session.id.clone(),
            offset: 0.,
            slots: 0,
            pane_height: 0.,
            hover_y: None,
            pressed: None,
            preview: RefCell::default(),
        })
    }

    fn diff(&self, tree: &mut Tree) {
        if tree.state.downcast_ref::<State>().session != self.session.id {
            tree.state = self.state();
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // The stack sizes itself from the scrollable, then offers this layer the
        // actual pane. Never change transcript padding to make room for the rail.
        let pane = limits.max();
        let state = tree.state.downcast_mut::<State>();
        state.pane_height = pane.height;
        state.slots = if (pane.width - self.chat_width) * 0.5 < WIDTH {
            0
        } else {
            self.prompts
                .len()
                .min(((pane.height - 2. * INSET) / PITCH) as usize)
        };
        state.offset = state.offset.min((self.prompts.len() - state.slots) as f64);
        let height = state.slots as f32 * PITCH;
        layout::Node::new(Size::new(WIDTH, height))
            .move_to(Point::new(0., (pane.height - height) * 0.5))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &iced::Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let y = cursor.position_in(layout.bounds()).map(|p| p.y);
        if state.hover_y != y {
            state.hover_y = y;
            shell.request_redraw();
        }
        let target = || {
            y.map(|y| {
                &self.session.entries[self.prompts[state.offset as usize + (y / PITCH) as usize]].id
            })
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if y.is_some() => {
                state.pressed = target().cloned();
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some(pressed) = state.pressed.take() {
                    if Some(&pressed) == target() {
                        shell.publish(Message::JumpToPrompt(pressed));
                    }
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if y.is_some() => {
                let max = self.prompts.len() - state.slots;
                if max > 0 {
                    let rows = match delta {
                        mouse::ScrollDelta::Lines { y, .. } => *y * 3.,
                        mouse::ScrollDelta::Pixels { y, .. } => *y / PITCH,
                    };
                    let next = (state.offset - f64::from(rows)).clamp(0., max as f64);
                    if next as usize != state.offset as usize {
                        shell.request_redraw();
                    }
                    state.offset = next;
                    shell.capture_event();
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Rectangle,
        _: &iced::Renderer,
    ) -> mouse::Interaction {
        if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
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
        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<State>();
        let colors = theme::colors(theme);
        let hover = cursor.position_in(bounds).map(|p| p.y);
        let selected = hover.map(|y| (y / PITCH) as usize);
        for slot in 0..state.slots {
            let center = (slot as f32 + 0.5) * PITCH;
            let width = hover.map_or(MIN_TICK, |y| tick_length((center - y).abs()));
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: bounds.x + INSET,
                        y: bounds.y + center - 1.,
                        width,
                        height: 2.,
                    },
                    ..Default::default()
                },
                if Some(slot) == selected {
                    colors.text
                } else {
                    colors.muted.scale_alpha(0.58)
                },
            );
        }
        if let Some(slot) = selected {
            let index = self.prompts[state.offset as usize + slot];
            let mut cached = state.preview.borrow_mut();
            let entry = &self.session.entries[index];
            if !cached
                .as_ref()
                .is_some_and(|p| std::ptr::eq(&*p.source, entry))
            {
                *cached = Some(Preview::new(self.session.entries.shared(index), renderer));
            }
            let preview = cached.as_ref().unwrap();
            let height = (preview.size.height + 2. * INSET).min(state.pane_height);
            let center = bounds.y + (slot as f32 + 0.5) * PITCH;
            let room = (state.pane_height - height) * 0.5;
            let card = Rectangle {
                x: bounds.x + WIDTH,
                y: center.clamp(bounds.center_y() - room, bounds.center_y() + room) - height * 0.5,
                width: preview.size.width + 2. * INSET,
                height,
            };
            renderer.fill_quad(
                renderer::Quad {
                    bounds: card,
                    border: border::rounded(theme::RADIUS_CONTROL)
                        .width(theme::BORDER_WIDTH)
                        .color(colors.border),
                    ..Default::default()
                },
                colors.popover,
            );
            if let Some(clip) = card.intersection(viewport) {
                widget::text::draw(
                    renderer,
                    style,
                    Rectangle::new(Point::new(card.x + INSET, card.y + INSET), preview.size),
                    preview.paragraph.raw(),
                    widget::text::Style {
                        color: Some(colors.text),
                    },
                    &clip,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn prompt_rail_wave_grows_toward_pointer_without_moving_targets() {
        let mut session = Session::new("/chat".into(), &Default::default());
        for n in 0..7 {
            session.accept(format!("Explain step {n}").into());
        }
        let prompts = [0, 1, 2, 3, 4, 5, 6];
        let size = Size::new(680., 200.);
        let mut renderer = crate::test_renderer::new(16.);
        let mut ui = UserInterface::build(
            rail(&session, &prompts, 400.),
            size,
            Cache::new(),
            &mut renderer,
        );
        let mut capture = |cursor| {
            renderer.reset(Rectangle::with_size(size));
            ui.draw(
                &mut renderer,
                &theme::app(),
                &renderer::Style {
                    text_color: theme::default_colors().text,
                },
                cursor,
            );
            crate::test_renderer::screenshot(
                &mut renderer,
                Size::new(size.width as u32, size.height as u32),
                1.,
                theme::default_colors().background,
            )
        };
        let idle = capture(mouse::Cursor::Unavailable);
        let hot = capture(mouse::Cursor::Available(Point::new(
            INSET,
            size.height / 2.,
        )));
        // Find the centers of the painted ticks in the idle image, independently
        // of the pitch. Measure length at those same positions after hovering.
        let stride = size.width as usize * 4;
        let row_length = |row: &[u8]| {
            row[..WIDTH as usize * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| pixel.as_slice() != &row[..4])
                .count()
        };
        let lengths: Vec<_> = idle.chunks_exact(stride).map(row_length).collect();
        let mut rows = 0;
        let widths: Vec<_> = lengths
            .chunk_by(|a, b| (*a == 0) == (*b == 0))
            .filter_map(|group| {
                let center = rows + group.len() / 2;
                rows += group.len();
                (group[0] > 0).then(|| row_length(&hot[center * stride..(center + 1) * stride]))
            })
            .collect();
        assert_eq!(widths.len(), prompts.len());
        assert!(widths[3] > widths[2] && widths[2] > widths[1]);
        assert!(widths[3] > widths[4] && widths[4] > widths[5]);
        assert!(
            idle.chunks_exact(stride)
                .zip(hot.chunks_exact(stride))
                .any(|(a, b)| a[WIDTH as usize * 4..] != b[WIDTH as usize * 4..]),
            "a preview appears beside the ticks"
        );
    }

    #[test]
    #[ignore = "manual rail CPU comparison; run alone"]
    fn prompt_rail_hover_and_wheel_cost() {
        use iced::advanced::clipboard;
        let size = Size::new(900., 480.);
        let mut renderer = crate::test_renderer::new(16.);
        let theme = theme::app();
        for count in [200, 20_000] {
            let mut session = Session::new("/chat".into(), &Default::default());
            for i in 0..count / 2 {
                session.accept(format!("Question {i:05}").into());
                session.entries.push(Entry::new(
                    orbit_core::store::EntryKind::Assistant,
                    "Answer",
                ));
            }
            let mut projection = crate::activity::Projection::default();
            projection.sync(&session.entries);
            let mut ui = UserInterface::build(
                rail(&session, &projection.prompts, 600.),
                size,
                Cache::new(),
                &mut renderer,
            );
            // Warm fonts/device once, then time input through CPU draw recording,
            // excluding fixture construction, GPU submission and readback.
            let mut messages = Vec::new();
            for pass in 0..2 {
                let start = std::time::Instant::now();
                for i in 0..2048 {
                    let position = Point::new(INSET, size.height * 0.25 + (i % 160) as f32);
                    let cursor = mouse::Cursor::Available(position);
                    let event = if i % 8 == 0 {
                        mouse::Event::WheelScrolled {
                            delta: mouse::ScrollDelta::Lines {
                                x: 0.,
                                y: if i % 256 < 128 { -1. } else { 1. },
                            },
                        }
                    } else {
                        mouse::Event::CursorMoved { position }
                    };
                    ui.update(
                        &[Event::Mouse(event)],
                        cursor,
                        &mut renderer,
                        &mut clipboard::Null,
                        &mut messages,
                    );
                    renderer.reset(Rectangle::with_size(size));
                    ui.draw(
                        &mut renderer,
                        &theme,
                        &renderer::Style {
                            text_color: theme::default_colors().text,
                        },
                        cursor,
                    );
                }
                if pass == 1 {
                    eprintln!(
                        "{count} entries: {:?} for 2048 rail frames",
                        start.elapsed()
                    );
                }
                assert!(messages.is_empty());
            }
        }
    }
}
