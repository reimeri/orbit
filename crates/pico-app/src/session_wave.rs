//! A low-contrast row background. Widget-local motion advances on existing app redraws.
use std::cell::Cell;

use iced::{
    Color, Element, Event, Length, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Renderer as _, Shell, Widget,
        graphics::{
            color,
            mesh::{self, Renderer as _},
        },
        layout, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
    mouse,
    time::Instant,
    window,
};

const PERIOD: f32 = 12.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Working,
    Question,
    Error,
    Unchecked,
    Idle,
}

impl Status {
    pub fn section(self) -> usize {
        match self {
            Self::Question | Self::Error => 0,
            Self::Working => 1,
            Self::Unchecked | Self::Idle => 2,
        }
    }

    fn color(self, theme: &iced::Theme) -> Option<Color> {
        match self {
            Self::Working => Some(crate::theme::colors(theme).accent),
            Self::Question => Some(crate::theme::colors(theme).warning),
            Self::Error => Some(crate::theme::colors(theme).danger),
            Self::Unchecked | Self::Idle => None,
        }
    }
}

pub fn wave<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    status: Status,
) -> Element<'a, Message> {
    Element::new(Wave {
        content: content.into(),
        status,
    })
}

struct Wave<'a, Message> {
    content: Element<'a, Message>,
    status: Status,
}

struct Motion {
    working: bool,
    last_frame: Option<Instant>,
    elapsed: f32,
    displayed: Cell<f32>,
}

impl Motion {
    fn transition(&mut self, status: Status) {
        let working = status == Status::Working;
        if working != self.working {
            self.elapsed = self.displayed.get();
            self.last_frame = None;
        }
        if matches!(status, Status::Unchecked | Status::Idle) {
            self.elapsed = 0.;
            self.displayed.set(0.);
        }
        self.working = working;
    }

    fn frame(&mut self, now: Instant, visible: bool) {
        if !self.working || !visible {
            self.last_frame = None;
            return;
        }
        if let Some(last) = self.last_frame.replace(now) {
            self.elapsed = (self.elapsed + (now - last).as_secs_f32()) % PERIOD;
        }
    }
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Wave<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Motion>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Motion {
            working: self.status == Status::Working,
            last_frame: None,
            elapsed: 0.,
            displayed: Cell::new(0.),
        })
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
        tree.state.downcast_mut::<Motion>().transition(self.status);
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
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
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
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            // The existing activity Tick already redraws working sessions. Do not
            // turn this subtle background into a display-rate redraw loop.
            tree.state
                .downcast_mut::<Motion>()
                .frame(*now, layout.bounds().intersects(viewport));
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
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
        let bounds = layout.bounds();
        let draw_content = |renderer: &mut iced::Renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        };
        if let Some(color) = self.status.color(theme)
            && let Some(clip) = bounds.intersection(viewport)
        {
            let motion = tree.state.downcast_ref::<Motion>();
            renderer.with_layer(clip, |renderer| {
                draw_wave(renderer, bounds, color, motion.elapsed);
                // Keep all foreground primitives above the mesh, including badges.
                renderer.with_layer(clip, draw_content);
            });
            motion.displayed.set(motion.elapsed);
        } else {
            draw_content(renderer);
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
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

fn draw_wave(renderer: &mut iced::Renderer, bounds: Rectangle, color: Color, elapsed: f32) {
    // V1's small untextured grid: each horizontal slice follows the rounded
    // outline; vertex alpha gives two broad, counter-moving Gaussian crests.
    const COLUMNS: usize = 24;
    const ROWS: usize = 16;
    let radius = crate::theme::RADIUS_CONTROL
        .min(bounds.width / 2.)
        .min(bounds.height / 2.);
    let slow = elapsed * std::f32::consts::TAU / PERIOD;
    let mut vertices = Vec::with_capacity((ROWS + 1) * (COLUMNS + 1));
    let mut indices = Vec::with_capacity(ROWS * COLUMNS * 6);
    for row in 0..=ROWS {
        let y = bounds.height * row as f32 / ROWS as f32;
        let corner_y = ((y - bounds.height / 2.).abs() - (bounds.height / 2. - radius)).max(0.);
        let inset = radius - (radius * radius - corner_y * corner_y).max(0.).sqrt();
        for col in 0..=COLUMNS {
            let x = inset + (bounds.width - 2. * inset) * col as f32 / COLUMNS as f32;
            let u = x / bounds.width;
            let v = y / bounds.height;
            let crest_a = 0.45 + 0.38 * (u * 5.2 - slow).sin();
            let crest_b = 0.55 + 0.42 * (u * 4.0 + slow * 2.).sin();
            let a = (v - crest_a) / 0.38;
            let b = (v - crest_b) / 0.45;
            let envelope = 0.65 + 0.35 * (u * 4.5 - slow * 2.).sin();
            let alpha = crate::theme::SESSION_WAVE_ALPHA
                * (0.1 + 0.6 * (-a * a).exp() * envelope + 0.3 * (-b * b).exp());
            vertices.push(mesh::SolidVertex2D {
                position: [bounds.x + x, bounds.y + y],
                color: color::pack(Color { a: alpha, ..color }),
            });
            if row > 0 && col > 0 {
                let d = (row * (COLUMNS + 1) + col) as u32;
                let a = d - (COLUMNS + 1) as u32 - 1;
                let b = a + 1;
                let c = d - 1;
                indices.extend_from_slice(&[a, b, d, a, d, c]);
            }
        }
    }
    renderer.draw_mesh(mesh::Mesh::Solid {
        buffers: mesh::Indexed { vertices, indices },
        transformation: iced::Transformation::IDENTITY,
        clip_bounds: bounds,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{Point, advanced::clipboard, widget::space};
    use std::time::Duration;

    #[test]
    fn rendered_wave_moves_freezes_and_resumes_without_requesting_frames() {
        let mut renderer = crate::test_renderer::new(16.);
        let size = Size::new(320., 80.);
        let viewport = Rectangle::with_size(size);
        let start = Instant::now();
        let make = |status| wave::<()>(space().width(270).height(48), status);
        let mut content = make(Status::Working);
        let mut tree = Tree::new(&content);
        let mut frames = Vec::new();
        for (status, seconds, visible) in [
            (Status::Working, 0, true),
            (Status::Working, 2, true),
            (Status::Question, 2, true),
            (Status::Question, 9, true),
            (Status::Error, 9, true),
            (Status::Error, 15, true),
            (Status::Working, 15, true),
            (Status::Working, 17, true),
            (Status::Working, 20, false),
            (Status::Working, 40, true),
            (Status::Unchecked, 40, true),
            (Status::Idle, 50, true),
        ] {
            content = make(status);
            content.as_widget().diff(&mut tree);
            let node = content
                .as_widget_mut()
                .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size))
                .move_to(Point::new(12., 8.));
            let clip = if visible {
                viewport
            } else {
                Rectangle::new(Point::new(400., 0.), size)
            };
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            content.as_widget_mut().update(
                &mut tree,
                &Event::Window(window::Event::RedrawRequested(
                    start + Duration::from_secs(seconds),
                )),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &clip,
            );
            assert_eq!(
                shell.redraw_request(),
                window::RedrawRequest::Wait,
                "the wave must reuse existing redraws, never schedule its own"
            );
            assert!(
                !shell.is_layout_invalid(),
                "animation must not re-layout text"
            );
            assert!(messages.is_empty(), "animation stays widget-local");
            renderer.reset(viewport);
            content.as_widget().draw(
                &tree,
                &mut renderer,
                &crate::theme::app(),
                &renderer::Style {
                    text_color: crate::theme::PICO.text,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &clip,
            );
            frames.push(crate::test_renderer::screenshot(
                &mut renderer,
                Size::new(320, 80),
                1.,
                crate::theme::PICO.background,
            ));
        }
        assert_ne!(frames[0], frames[1], "working wave moves");
        assert_eq!(frames[2], frames[3], "questionnaire wave is frozen");
        assert_eq!(frames[4], frames[5], "error wave is frozen");
        assert_ne!(frames[2], frames[4], "question and error colors differ");
        assert_eq!(
            frames[1], frames[6],
            "resume preserves the last displayed shape"
        );
        assert_ne!(frames[6], frames[7], "resumed work moves again");
        assert_eq!(
            frames[7], frames[9],
            "offscreen time does not advance the wave"
        );
        assert_eq!(
            frames[8], frames[10],
            "offscreen and completed rows have no wave"
        );
        assert_eq!(frames[10], frames[11], "idle rows have no wave");
        for (index, (wave, idle)) in frames[1]
            .as_chunks::<4>()
            .0
            .iter()
            .zip(frames[11].as_chunks::<4>().0.iter())
            .enumerate()
        {
            let (x, y) = (index % 320, index / 320);
            if !(12..282).contains(&x) || !(8..56).contains(&y) || (x == 12 && y == 8) {
                assert_eq!(wave, idle, "wave stays within rounded row bounds");
            }
        }
    }
}
