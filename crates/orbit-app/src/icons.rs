//! Lucide icons. Foreground is inherited from the entire enclosing control,
//! not just the icon's own hover bounds. Asset provenance and license: icons/README.md.
use iced::{
    Color, Element, Event, Length, Rectangle, Size,
    advanced::{
        Clipboard, Layout, Shell, Widget,
        graphics::{
            color,
            mesh::{self, Renderer as _},
        },
        layout, renderer,
        svg::{self, Renderer as _},
        widget::{Tree, tree},
    },
    mouse,
    time::Instant,
    widget::{row, text, tooltip},
    window,
};

#[derive(Clone, Copy)]
pub enum Icon {
    Pencil,
    GitFork,
    Copy,
    Trash2,
    Archive,
    ArchiveRestore,
    Search,
    X,
    Plus,
    Bot,
    BookOpen,
    Map,
    Settings,
    Star,
    StarFilled,
    ChevronRight,
    ChevronDown,
    ChevronUp,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowUpRight,
    Square,
    Play,
    Pause,
    Clock,
    Check,
    Zap,
    Undo2,
    FolderPlus,
    FolderOpen,
    User,
    LogIn,
    LogOut,
    RotateCcw,
    RefreshCw,
    LoaderCircle,
    Info,
    CircleAlert,
}

impl Icon {
    fn glyph(self) -> Glyph {
        let bytes: &'static [u8] = match self {
            Self::GitFork => include_bytes!("icons/git-fork.svg"),
            Self::Pencil => include_bytes!("icons/pencil.svg"),
            Self::Copy => include_bytes!("icons/copy.svg"),
            Self::Trash2 => include_bytes!("icons/trash-2.svg"),
            Self::Archive => include_bytes!("icons/archive.svg"),
            Self::ArchiveRestore => include_bytes!("icons/archive-restore.svg"),
            Self::Search => include_bytes!("icons/search.svg"),
            Self::X => include_bytes!("icons/x.svg"),
            Self::Plus => include_bytes!("icons/plus.svg"),
            Self::BookOpen => include_bytes!("icons/book-open.svg"),
            Self::Map => include_bytes!("icons/map.svg"),
            Self::Bot => include_bytes!("icons/bot.svg"),
            Self::Settings => include_bytes!("icons/settings.svg"),
            Self::Star => include_bytes!("icons/star.svg"),
            Self::StarFilled => include_bytes!("icons/star-filled.svg"),
            Self::ChevronRight => include_bytes!("icons/chevron-right.svg"),
            Self::ChevronDown => include_bytes!("icons/chevron-down.svg"),
            Self::ChevronUp => include_bytes!("icons/chevron-up.svg"),
            Self::ArrowLeft => include_bytes!("icons/arrow-left.svg"),
            Self::ArrowRight => include_bytes!("icons/arrow-right.svg"),
            Self::ArrowUp => include_bytes!("icons/arrow-up.svg"),
            Self::ArrowUpRight => include_bytes!("icons/arrow-up-right.svg"),
            Self::Square => include_bytes!("icons/square.svg"),
            Self::Play => include_bytes!("icons/play.svg"),
            Self::Pause => include_bytes!("icons/pause.svg"),
            Self::Zap => include_bytes!("icons/zap.svg"),
            Self::Clock => include_bytes!("icons/clock.svg"),
            Self::Check => include_bytes!("icons/check.svg"),
            Self::Undo2 => include_bytes!("icons/undo-2.svg"),
            Self::FolderPlus => include_bytes!("icons/folder-plus.svg"),
            Self::User => include_bytes!("icons/user.svg"),
            Self::FolderOpen => include_bytes!("icons/folder-open.svg"),
            Self::LogIn => include_bytes!("icons/log-in.svg"),
            Self::LogOut => include_bytes!("icons/log-out.svg"),
            Self::RotateCcw => include_bytes!("icons/rotate-ccw.svg"),
            Self::RefreshCw => include_bytes!("icons/refresh-cw.svg"),
            Self::LoaderCircle => return Glyph::LoaderCircle,
            Self::Info => include_bytes!("icons/info.svg"),
            Self::CircleAlert => include_bytes!("icons/circle-alert.svg"),
        };
        Glyph::Svg(svg::Handle::from_memory(bytes))
    }

    pub fn view<'a, Message: 'a>(self, size: f32) -> Element<'a, Message> {
        self.spinning(size, false)
    }

    pub fn spinning<'a, Message: 'a>(self, size: f32, spinning: bool) -> Element<'a, Message> {
        Element::new(IconWidget {
            glyph: self.glyph(),
            size,
            color: None,
            spinning,
        })
    }

    pub fn colored<'a, Message: 'a>(self, size: f32, color: Color) -> Element<'a, Message> {
        Element::new(IconWidget {
            glyph: self.glyph(),
            size,
            color: Some(color),
            spinning: false,
        })
    }

    pub fn label<'a, Message: 'a>(
        self,
        label: impl Into<iced::widget::Text<'a>>,
        size: f32,
    ) -> Element<'a, Message> {
        row![self.view(16.), label.into().size(size)]
            .spacing(8)
            .align_y(iced::Center)
            .into()
    }
}

/// Text descriptions for compact icon-only controls.
pub fn tip<'a, Message: 'a>(
    control: impl Into<Element<'a, Message>>,
    label: &'a str,
) -> Element<'a, Message> {
    tooltip(control, text(label).size(12), tooltip::Position::Bottom)
        .style(crate::theme::footer_surface)
        .into()
}

enum Glyph {
    Svg(svg::Handle),
    LoaderCircle,
}

struct IconWidget {
    glyph: Glyph,
    size: f32,
    color: Option<Color>,
    spinning: bool,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for IconWidget {
    fn tag(&self) -> tree::Tag {
        if self.spinning {
            tree::Tag::of::<Instant>()
        } else {
            tree::Tag::stateless()
        }
    }

    fn state(&self) -> tree::State {
        if self.spinning {
            tree::State::new(Instant::now())
        } else {
            tree::State::None
        }
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if self.spinning
            && matches!(event, Event::Window(window::Event::RedrawRequested(_)))
            && layout.bounds().intersects(viewport)
        {
            shell.request_redraw();
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.size), Length::Fixed(self.size))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(self.size, self.size, Size::ZERO))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let color = self.color.unwrap_or(style.text_color);
        let angle = if self.spinning {
            tree.state
                .downcast_ref::<Instant>()
                .elapsed()
                .as_secs_f32()
                .fract()
                * std::f32::consts::TAU
        } else {
            0.
        };
        self.glyph
            .draw(renderer, layout.bounds(), *viewport, color, angle);
    }
}

impl Glyph {
    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        bounds: Rectangle,
        viewport: Rectangle,
        tint: Color,
        angle: f32,
    ) {
        match self {
            Self::Svg(handle) => {
                // The SVG tint filter only applies RGB; pass alpha as draw opacity.
                let svg = svg::Svg::new(handle.clone())
                    .color(Color { a: 1., ..tint })
                    .opacity(tint.a)
                    .rotation(angle);
                renderer.draw_svg(svg, bounds, viewport);
            }
            Self::LoaderCircle => {
                if let Some(clip) = bounds.intersection(&viewport) {
                    draw_loader_circle(renderer, bounds, clip, tint, angle);
                }
            }
        }
    }
}

/// Tessellate Lucide's round-capped loader-circle directly at its current angle.
/// The renderer multisamples the stroke edges instead of rotating cached texels.
fn draw_loader_circle(
    renderer: &mut iced::Renderer,
    bounds: Rectangle,
    clip: Rectangle,
    tint: Color,
    angle: f32,
) {
    use std::f32::consts::{FRAC_PI_2, TAU};
    const ARC_SEGMENTS: u32 = 40;
    const CAP_SEGMENTS: u32 = 4;
    const SWEEP: f32 = TAU * 0.8;
    // Radius 9 and stroke width 2 in the original 24-unit viewBox.
    let half_stroke = bounds.width.min(bounds.height) / 24.;
    let radius = 9. * half_stroke;
    let center = bounds.center();
    let color = color::pack(tint);
    let sections = (ARC_SEGMENTS + 1 + 2 * CAP_SEGMENTS) as usize;
    let mut vertices = Vec::with_capacity(2 * sections);
    let mut indices = Vec::with_capacity(6 * (sections - 1));

    // Each cross-section spans the stroke radially. At either end, slide
    // the section along the tangent and narrow it to trace a semicircle.
    let mut section = |theta: f32, cap: f32| {
        let (sin, cos) = theta.sin_cos();
        let (along, across) = cap.sin_cos();
        let i = vertices.len() as u32;
        for r in [radius - across * half_stroke, radius + across * half_stroke] {
            vertices.push(mesh::SolidVertex2D {
                position: [
                    center.x + r * cos - along * half_stroke * sin,
                    center.y + r * sin + along * half_stroke * cos,
                ],
                color,
            });
        }
        if i > 0 {
            indices.extend_from_slice(&[i - 2, i - 1, i, i, i - 1, i + 1]);
        }
    };
    for i in (1..=CAP_SEGMENTS).rev() {
        section(angle, -FRAC_PI_2 * i as f32 / CAP_SEGMENTS as f32);
    }
    for i in 0..=ARC_SEGMENTS {
        section(angle + SWEEP * i as f32 / ARC_SEGMENTS as f32, 0.);
    }
    for i in 1..=CAP_SEGMENTS {
        section(angle + SWEEP, FRAC_PI_2 * i as f32 / CAP_SEGMENTS as f32);
    }
    renderer.draw_mesh(mesh::Mesh::Solid {
        buffers: mesh::Indexed { vertices, indices },
        transformation: iced::Transformation::IDENTITY,
        clip_bounds: clip,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{Point, advanced::Renderer as _, widget::button};
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn loader_rotation_moves_the_gap_without_shimmering_along_the_rest_of_the_ring() {
        let mut renderer = crate::test_renderer::new(16.);
        // Exercise a small, single-scale icon as in the reported screenshot.
        let size = Size::new(18, 18);
        let bounds = Rectangle::with_size(Size::new(size.width as f32, size.height as f32));
        let loader = Icon::LoaderCircle.glyph();
        let mut render = |angle| {
            renderer.reset(bounds);
            loader.draw(&mut renderer, bounds, bounds, Color::WHITE, angle);
            crate::test_renderer::screenshot(&mut renderer, size, 1., Color::TRANSPARENT)
        };
        let first = render(0.);
        let turned = render(std::f32::consts::FRAC_PI_4);
        let (first, _) = first.as_chunks::<4>();
        let (turned, _) = turned.as_chunks::<4>();
        assert!(
            turned.iter().any(|p| (1..255).contains(&p[3])),
            "stroke edges should have partial pixel coverage"
        );

        // The gap moves within the right half. On the left the same circular
        // stroke covers the same pixels, even at a non-axis-aligned angle.
        let mut left = 0u32;
        let mut right = 0u32;
        for (i, (a, b)) in first.iter().zip(turned).enumerate() {
            let delta = u32::from(a[3].abs_diff(b[3]));
            if (i as u32 % size.width) < size.width / 2 {
                left += delta;
            } else {
                right += delta;
            }
        }
        assert!(
            left * 4 < right,
            "rotation should change the gap, not resample the entire ring: left={left} right={right}"
        );
    }

    #[test]
    fn svg_foreground_follows_button_color_and_opacity() {
        let mut renderer = crate::test_renderer::new(16.);
        let size = Size::new(180., 48.);
        let active = Color::from_rgb8(220, 65, 90);
        let hovered = Color::from_rgb8(60, 210, 130);
        let disabled = Color::from_rgb8(75, 110, 230);
        let mut render = |enabled: bool, hover_label: bool, disabled_alpha: f32| {
            let disabled = disabled.scale_alpha(disabled_alpha);
            let expected = if !enabled {
                disabled
            } else if hover_label {
                hovered
            } else {
                active
            };
            let control = || {
                button(Icon::Pencil.label("Rename", 16.))
                    .padding(8)
                    .width(180)
                    .on_press_maybe(enabled.then_some(()))
                    .style(move |_, status| button::Style {
                        text_color: match status {
                            button::Status::Hovered => hovered,
                            button::Status::Disabled => disabled,
                            _ => active,
                        },
                        ..Default::default()
                    })
            };
            let mut view: Element<'_, ()> = control().into();
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            let layout = Layout::new(&node);
            let row = layout.children().next().unwrap();
            let icon = row.children().next().unwrap().bounds();
            let label = row.children().nth(1).unwrap().bounds();
            let cursor = if hover_label {
                mouse::Cursor::Available(label.center())
            } else {
                mouse::Cursor::Unavailable
            };
            assert!(!icon.contains(cursor.position().unwrap_or(Point::ORIGIN)));
            let mut ui = UserInterface::build(control(), size, Cache::new(), &mut renderer);
            ui.update(
                &[iced::Event::Window(iced::window::Event::RedrawRequested(
                    iced::time::Instant::now(),
                ))],
                cursor,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut Vec::new(),
            );
            renderer.reset(Rectangle::with_size(size));
            ui.draw(
                &mut renderer,
                &crate::theme::app(),
                &renderer::Style {
                    text_color: Color::WHITE,
                },
                cursor,
            );
            let width = size.width as u32;
            let pixels = crate::test_renderer::screenshot(
                &mut renderer,
                Size::new(width, size.height as u32),
                1.,
                Color::TRANSPARENT,
            );
            let foreground = expected.into_rgba8();
            let strokes =
                (icon.y.ceil() as u32..(icon.y + icon.height).floor() as u32).flat_map(|y| {
                    let pixels = &pixels;
                    (icon.x.ceil() as u32..(icon.x + icon.width).floor() as u32)
                        .map(move |x| &pixels[((y * width + x) * 4) as usize..][..4])
                });
            if expected.a == 1. {
                // Even when only the label is hovered, the SVG inherits the button's tint.
                assert!(
                    strokes
                        .clone()
                        .any(|pixel| { (0..3).all(|c| pixel[c].abs_diff(foreground[c]) < 30) }),
                    "missing inherited SVG foreground: enabled={enabled}, hover={hover_label}"
                );
            }
            strokes.map(|pixel| pixel[3]).max().unwrap()
        };

        render(true, false, 1.);
        render(true, true, 1.);
        let opaque_peak = render(false, true, 1.);
        assert!(opaque_peak > 0, "the reference icon must be visible");
        let opacity = 0.4;
        let faded_peak = render(false, true, opacity);
        let expected_peak = (f32::from(opaque_peak) * opacity).round() as u8;
        assert!(
            faded_peak.abs_diff(expected_peak) <= 1,
            "SVG stroke opacity: expected {expected_peak} from opaque peak {opaque_peak}, got {faded_peak}"
        );
    }
}
