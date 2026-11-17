//! Embedded Lucide SVGs. Foreground is inherited from the entire enclosing control,
//! not just the icon's own hover bounds. Asset provenance and license: icons/README.md.
use iced::{
    Color, Element, Length, Rectangle, Size,
    advanced::{
        Layout, Widget, layout, renderer,
        svg::{self, Renderer as _},
        widget::Tree,
    },
    mouse,
    widget::{row, text, tooltip},
};

#[derive(Clone, Copy)]
pub enum Icon {
    Pencil,
    Copy,
    Trash2,
    Archive,
    ArchiveRestore,
    Search,
    X,
    Plus,
    Bot,
    Settings,
    Star,
    StarFilled,
    ChevronRight,
    ChevronDown,
    ArrowLeft,
    ArrowRight,
    ArrowUpRight,
    Square,
    Play,
    Pause,
    Check,
    Zap,
    Undo2,
    FolderPlus,
    FolderOpen,
    LogIn,
    LogOut,
    RotateCcw,
    RefreshCw,
    Info,
    CircleAlert,
}

impl Icon {
    fn handle(self) -> svg::Handle {
        let bytes: &'static [u8] = match self {
            Self::Pencil => include_bytes!("icons/pencil.svg"),
            Self::Copy => include_bytes!("icons/copy.svg"),
            Self::Trash2 => include_bytes!("icons/trash-2.svg"),
            Self::Archive => include_bytes!("icons/archive.svg"),
            Self::ArchiveRestore => include_bytes!("icons/archive-restore.svg"),
            Self::Search => include_bytes!("icons/search.svg"),
            Self::X => include_bytes!("icons/x.svg"),
            Self::Plus => include_bytes!("icons/plus.svg"),
            Self::Bot => include_bytes!("icons/bot.svg"),
            Self::Settings => include_bytes!("icons/settings.svg"),
            Self::Star => include_bytes!("icons/star.svg"),
            Self::StarFilled => include_bytes!("icons/star-filled.svg"),
            Self::ChevronRight => include_bytes!("icons/chevron-right.svg"),
            Self::ChevronDown => include_bytes!("icons/chevron-down.svg"),
            Self::ArrowLeft => include_bytes!("icons/arrow-left.svg"),
            Self::ArrowRight => include_bytes!("icons/arrow-right.svg"),
            Self::ArrowUpRight => include_bytes!("icons/arrow-up-right.svg"),
            Self::Square => include_bytes!("icons/square.svg"),
            Self::Play => include_bytes!("icons/play.svg"),
            Self::Pause => include_bytes!("icons/pause.svg"),
            Self::Zap => include_bytes!("icons/zap.svg"),
            Self::Check => include_bytes!("icons/check.svg"),
            Self::Undo2 => include_bytes!("icons/undo-2.svg"),
            Self::FolderPlus => include_bytes!("icons/folder-plus.svg"),
            Self::FolderOpen => include_bytes!("icons/folder-open.svg"),
            Self::LogIn => include_bytes!("icons/log-in.svg"),
            Self::LogOut => include_bytes!("icons/log-out.svg"),
            Self::RotateCcw => include_bytes!("icons/rotate-ccw.svg"),
            Self::RefreshCw => include_bytes!("icons/refresh-cw.svg"),
            Self::Info => include_bytes!("icons/info.svg"),
            Self::CircleAlert => include_bytes!("icons/circle-alert.svg"),
        };
        svg::Handle::from_memory(bytes)
    }

    pub fn view<'a, Message: 'a>(self, size: f32) -> Element<'a, Message> {
        Element::new(SvgIcon {
            handle: self.handle(),
            size,
            color: None,
        })
    }

    pub fn colored<'a, Message: 'a>(self, size: f32, color: Color) -> Element<'a, Message> {
        Element::new(SvgIcon {
            handle: self.handle(),
            size,
            color: Some(color),
        })
    }

    pub fn label<'a, Message: 'a>(self, label: &'a str, size: f32) -> Element<'a, Message> {
        row![self.view(16.), text(label).size(size)]
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

struct SvgIcon {
    handle: svg::Handle,
    size: f32,
    color: Option<Color>,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for SvgIcon {
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
        _tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        renderer.draw_svg(
            svg::Svg::new(self.handle.clone()).color(self.color.unwrap_or(style.text_color)),
            layout.bounds(),
            layout.bounds(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{Point, advanced::Renderer as _, widget::button};
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn svg_foreground_follows_button_hover_and_disabled_state() {
        let mut renderer = crate::test_renderer::new(16.);
        let size = Size::new(180., 48.);
        let active = Color::from_rgb8(220, 65, 90);
        let hovered = Color::from_rgb8(60, 210, 130);
        let disabled = Color::from_rgb8(75, 110, 230);
        for (enabled, hover_label, expected) in [
            (true, false, active),
            (true, true, hovered),
            (false, true, disabled),
        ] {
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
                Color::BLACK,
            );
            let foreground = expected.into_rgba8();
            // There must be painted SVG strokes in the control's foreground color,
            // even when only the label (not the icon) is hovered.
            assert!(
                (icon.y.ceil() as u32..(icon.y + icon.height).floor() as u32).any(|y| {
                    (icon.x.ceil() as u32..(icon.x + icon.width).floor() as u32).any(|x| {
                        let pixel = &pixels[((y * width + x) * 4) as usize..][..4];
                        (0..3).all(|c| pixel[c].abs_diff(foreground[c]) < 30)
                    })
                }),
                "missing inherited SVG foreground: enabled={enabled}, hover={hover_label}"
            );
        }
    }
}
