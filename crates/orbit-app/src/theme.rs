//! Shared visual language for the native UI. Keep colors and widget styling here.
//!
//! Controls/rows use 8 px corners; cards, editors and floating surfaces use 12 px.
//! Tiny decorations use 4 px. Edge-to-edge panels, dividers, selection rectangles,
//! and icon geometry intentionally do not use the surface radius.
use iced::theme::{Base, palette::mix};
use iced::{
    Color, Element, Theme,
    widget::{
        self, button as button_widget, container, markdown, overlay::menu,
        pick_list as pick_list_widget, text_editor, text_input, toggler,
    },
};
use orbit_core::store::DEFAULT_THEME;
use std::sync::LazyLock;

pub const SCRIM: Color = Color::from_rgba(0., 0., 0., 0.65);
pub const IMAGE_TEXT: Color = Color::WHITE;

pub const RADIUS_SMALL: f32 = 4.;
pub const RADIUS_CONTROL: f32 = 8.;
pub const RADIUS_SURFACE: f32 = 12.;
pub const BORDER_WIDTH: f32 = 1.;
pub const SESSION_WAVE_ALPHA: f32 = 0.18;
pub const BADGE_ALPHA: f32 = 0.18;
pub const SCROLLBAR_WIDTH: f32 = 5.;
pub const SCROLLBAR_GAP: f32 = 2.;
pub const SCROLLBAR_EDGE_INSET: f32 = 2.;
pub const SCROLLBAR_THUMB_WIDTH: f32 = 3.;
pub const SCROLLBAR_ALPHA: f32 = 0.25;
pub const SCROLLBAR_ACTIVE_ALPHA: f32 = 0.4;

/// Derived once per preset; view construction and draw callbacks share these roles.
pub struct Colors {
    pub background: Color,
    pub text: Color,
    pub success: Color,
    pub danger: Color,
    pub warning: Color,
    pub muted: Color,
    pub panel: Color,
    pub border: Color,
    pub accent: Color,
    pub right_panel: Color,
    pub secondary: Color,
    pub secondary_hover: Color,
    pub popover: Color,
    pub search: Color,
    pub error_surface: Color,
    pub selection: Color,
    pub code: Color,
}

/// Keep the hue of a foreground (notably project badges and status text) as
/// close to its original color as the surface allows.
pub fn contrasting_ink(ink: Color, surface: Color) -> Color {
    let target =
        if surface.relative_contrast(Color::BLACK) > surface.relative_contrast(Color::WHITE) {
            Color::BLACK
        } else {
            Color::WHITE
        };
    if ink.relative_contrast(surface) >= 4.5 {
        return ink;
    }
    for step in 1..=20 {
        let candidate = mix(ink, target, step as f32 / 20.);
        if candidate.relative_contrast(surface) >= 4.5 {
            return candidate;
        }
    }
    target
}

impl Colors {
    fn from_iced(theme: &Theme) -> Self {
        let p = theme.palette();
        let bg = theme.extended_palette().background;
        // Semantic text is also used on cards and controls, not just the window.
        let ink = |color| contrasting_ink(color, bg.weak.color);
        Self {
            background: p.background,
            text: bg.base.text,
            success: ink(p.success),
            danger: ink(p.danger),
            warning: ink(p.warning),
            accent: ink(p.primary),
            muted: ink(mix(p.text, p.background, 0.26)),
            panel: bg.weaker.color,
            right_panel: bg.weakest.color,
            border: bg.strong.color,
            secondary: bg.weak.color,
            secondary_hover: bg.strong.color,
            popover: bg.weaker.color,
            search: bg.weak.color,
            error_surface: mix(p.background, p.danger, 0.12),
            selection: p.primary.scale_alpha(0.35),
            code: bg.weak.color,
        }
    }
}

pub struct Preset {
    pub theme: Theme,
    pub colors: Colors,
}
pub static PRESETS: LazyLock<Vec<Preset>> = LazyLock::new(|| {
    Theme::ALL
        .iter()
        .map(|theme| Preset {
            theme: theme.clone(),
            colors: Colors::from_iced(theme),
        })
        .collect()
});

pub fn preset(name: &str) -> &'static Preset {
    let find = |name| PRESETS.iter().find(|p| p.theme.name() == name);
    find(name).unwrap_or_else(|| find(DEFAULT_THEME).expect("default theme must be shipped"))
}

pub fn colors(theme: &Theme) -> &'static Colors {
    &preset(theme.name()).colors
}
pub fn muted(theme: &Theme) -> widget::text::Style {
    widget::text::Style {
        color: Some(colors(theme).muted),
    }
}

#[cfg(test)]
pub fn app() -> Theme {
    preset(DEFAULT_THEME).theme.clone()
}

#[cfg(test)]
pub fn default_colors() -> &'static Colors {
    &preset(DEFAULT_THEME).colors
}

/// Use this constructor so buttons without an explicit variant get our radius too.
pub fn button<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> widget::Button<'a, Message> {
    widget::button(content).style(primary)
}

/// Shared sizing for labeled form and dialog actions (not icon buttons or menu rows).
pub fn action<'a, Message: 'a>(label: impl Into<widget::Text<'a>>) -> widget::Button<'a, Message> {
    button(label.into().size(14)).padding([8, 16])
}

pub fn primary_label(label: &str) -> widget::Text<'_> {
    widget::text(label).font(iced::Font {
        weight: iced::font::Weight::Bold,
        ..iced::Font::DEFAULT
    })
}

pub fn primary_action<'a, Message: 'a>(label: &'a str) -> widget::Button<'a, Message> {
    action(primary_label(label))
}

pub fn cancel<'a, Message: 'a>(label: &'a str) -> widget::Button<'a, Message> {
    action(label).style(text_button)
}

pub fn primary(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let mut style = button_widget::primary(theme, status);
    style.border.radius = RADIUS_CONTROL.into();
    style
}

pub fn danger(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let mut style = button_widget::danger(theme, status);
    // Iced's strong accent is darker: use it at rest, not on hover.
    let palette = theme.extended_palette().danger;
    match status {
        button_widget::Status::Active | button_widget::Status::Pressed => {
            style.background = Some(palette.strong.color.into());
            style.text_color = palette.strong.text;
        }
        button_widget::Status::Hovered => {
            style.background = Some(palette.base.color.into());
            style.text_color = palette.base.text;
        }
        button_widget::Status::Disabled => {}
    }
    style.border.radius = RADIUS_CONTROL.into();
    style
}

pub fn text_button(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let mut style = button_widget::text(theme, status);
    style.text_color = match status {
        button_widget::Status::Active | button_widget::Status::Pressed => {
            theme.palette().text.scale_alpha(0.8)
        }
        button_widget::Status::Hovered => theme.palette().text,
        button_widget::Status::Disabled => style.text_color,
    };
    style.border.radius = RADIUS_CONTROL.into();
    style
}

pub fn completion_item(
    theme: &Theme,
    status: button_widget::Status,
    selected: bool,
) -> button_widget::Style {
    let c = colors(theme);
    let mut style = text_button(theme, status);
    style.text_color = if selected {
        c.accent
    } else if matches!(
        status,
        button_widget::Status::Hovered | button_widget::Status::Pressed
    ) {
        c.accent.scale_alpha(0.85)
    } else {
        c.muted
    };
    style
}

/// Destructive menu action without a filled button background.
pub fn danger_text(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let mut style = text_button(theme, status);
    if !matches!(status, button_widget::Status::Disabled) {
        style.text_color =
            colors(theme)
                .danger
                .scale_alpha(if matches!(status, button_widget::Status::Hovered) {
                    1.
                } else {
                    0.8
                });
    }
    style
}

pub fn menu_button(
    theme: &Theme,
    status: button_widget::Status,
    selected: bool,
) -> button_widget::Style {
    let c = colors(theme);
    let mut style = text_button(theme, status);
    if matches!(
        status,
        button_widget::Status::Hovered | button_widget::Status::Pressed
    ) {
        style.background = Some(c.secondary_hover.into());
    } else if selected {
        style.background = Some(c.secondary.into());
    }
    style
}

pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut style = text_input::default(theme, status);
    style.border.radius = RADIUS_CONTROL.into();
    style
}

pub fn search_input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let c = colors(theme);
    let mut style = input(theme, status);
    style.background = c.search.into();
    style.placeholder = c.muted;
    style
}

pub fn editor(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let mut style = text_editor::default(theme, status);
    style.border.radius = RADIUS_SURFACE.into();
    style
}

pub fn dropdown<'a, T, L, V, Message>(
    options: L,
    selected: Option<V>,
    on_selected: impl Fn(T) -> Message + 'a,
) -> widget::PickList<'a, T, L, V, Message>
where
    T: ToString + PartialEq + Clone + 'a,
    L: std::borrow::Borrow<[T]> + 'a,
    V: std::borrow::Borrow<T> + 'a,
    Message: Clone,
{
    widget::pick_list(options, selected, on_selected)
        .text_size(13)
        .padding([7, 9])
        .handle(pick_list_widget::Handle::Arrow {
            size: Some(9.into()),
        })
        .style(pick_list)
        .menu_style(pick_list_menu)
        .width(iced::Fill)
}

fn pick_list(theme: &Theme, status: pick_list_widget::Status) -> pick_list_widget::Style {
    let c = colors(theme);
    pick_list_widget::Style {
        text_color: c.text,
        placeholder_color: c.muted,
        handle_color: c.muted,
        background: c.secondary.into(),
        border: iced::border::rounded(RADIUS_CONTROL)
            .width(BORDER_WIDTH)
            .color(if matches!(status, pick_list_widget::Status::Active) {
                c.secondary_hover
            } else {
                c.accent
            }),
    }
}

pub fn pick_list_menu(theme: &Theme) -> menu::Style {
    let c = colors(theme);
    let mut style = menu::default(theme);
    style.background = c.popover.into();
    style.border = iced::border::rounded(RADIUS_CONTROL)
        .color(c.secondary_hover)
        .width(BORDER_WIDTH);
    style
}

pub fn switch<'a, Message>(on: bool) -> widget::Toggler<'a, Message> {
    widget::toggler(on)
        .size(18)
        .text_size(14)
        .style(|theme, status| {
            let c = colors(theme);
            let mut style = toggler::default(theme, status);
            match status {
                toggler::Status::Active { is_toggled }
                | toggler::Status::Hovered { is_toggled } => {
                    style.background = if is_toggled {
                        c.accent
                    } else {
                        c.secondary_hover
                    }
                    .into();
                    style.foreground = if is_toggled { c.background } else { c.text }.into();
                    if matches!(status, toggler::Status::Hovered { .. }) {
                        style.background_border_width = BORDER_WIDTH;
                        style.background_border_color = c.muted;
                    }
                }
                toggler::Status::Disabled { .. } => {}
            }
            style.padding_ratio = 0.14;
            style
        })
}

pub fn markdown_style(theme: &Theme) -> markdown::Style {
    let c = colors(theme);
    let mut style = markdown::Style::from_palette(theme.palette());
    style.inline_code_highlight.background = c.code.into();
    style.inline_code_highlight.border.radius = RADIUS_SMALL.into();
    style.inline_code_color = c.text;
    style.link_color = c.accent;
    style
}

pub fn code_block(theme: &Theme) -> container::Style {
    let c = colors(theme);
    let mut style = container::background(c.code).color(c.text);
    style.border.radius = RADIUS_SURFACE.into();
    style
}

pub fn questionnaire(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(colors(theme).panel.into()),
        border: iced::border::rounded(RADIUS_SURFACE),
        ..Default::default()
    }
}

pub fn inline_answer_editor(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: Color::TRANSPARENT.into(),
        border: Default::default(),
        ..editor(theme, status)
    }
}

pub fn answer_row(theme: &Theme, focused: bool) -> container::Style {
    container::Style {
        border: if focused {
            editor(theme, text_editor::Status::Focused { is_hovered: false }).border
        } else {
            Default::default()
        },
        ..Default::default()
    }
}

pub fn option_button(
    theme: &Theme,
    status: button_widget::Status,
    selected: bool,
) -> button_widget::Style {
    let c = colors(theme);
    let mut style = text_button(theme, status);
    style.background = Some(
        if matches!(
            status,
            button_widget::Status::Hovered | button_widget::Status::Pressed
        ) {
            c.secondary_hover
        } else if selected {
            c.secondary
        } else {
            c.background
        }
        .into(),
    );
    style
}

pub fn footer_surface(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(colors(theme).popover.into()),
        border: iced::Border {
            radius: RADIUS_SURFACE.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn sidebar_control(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let mut style = text_button(theme, status);
    style.text_color = if matches!(
        status,
        button_widget::Status::Hovered | button_widget::Status::Pressed
    ) {
        theme.palette().text
    } else {
        colors(theme).muted
    };
    style
}

pub fn secondary(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let c = colors(theme);
    let mut style = button_widget::secondary(theme, status);
    style.border.radius = RADIUS_CONTROL.into();
    style.background = Some(
        if matches!(status, button_widget::Status::Hovered) {
            c.secondary_hover
        } else {
            c.secondary
        }
        .into(),
    );
    style.text_color = if matches!(status, button_widget::Status::Disabled) {
        c.muted
    } else {
        theme.palette().text
    };
    style
}

pub fn right_panel(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(colors(theme).right_panel.into()),
        ..Default::default()
    }
}
pub fn panel(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(colors(theme).panel.into()),
        ..Default::default()
    }
}
pub fn attachment_button(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let c = colors(theme);
    button_widget::Style {
        background: Some(c.panel.into()),
        text_color: theme.palette().text,
        border: iced::Border {
            color: if matches!(
                status,
                button_widget::Status::Hovered | button_widget::Status::Pressed
            ) {
                c.accent
            } else {
                c.border
            },
            width: BORDER_WIDTH,
            radius: RADIUS_SURFACE.into(),
        },
        ..Default::default()
    }
}
pub fn attachment_remove(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let mut style = attachment_button(theme, status);
    style.border.radius = RADIUS_CONTROL.into();
    style
}

pub fn card(theme: &Theme) -> container::Style {
    let c = colors(theme);
    container::Style {
        background: Some(c.panel.into()),
        border: iced::Border {
            color: c.border,
            width: BORDER_WIDTH,
            radius: RADIUS_SURFACE.into(),
        },
        ..Default::default()
    }
}

/// Clickable cards retain the surface border and gain a quiet hover highlight.
pub fn card_button(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let surface = card(theme);
    button_widget::Style {
        background: if matches!(
            status,
            button_widget::Status::Hovered | button_widget::Status::Pressed
        ) {
            Some(colors(theme).secondary.into())
        } else {
            surface.background
        },
        border: surface.border,
        text_color: theme.palette().text,
        ..Default::default()
    }
}

pub const PROJECT_COLORS: [Color; 8] = [
    Color::from_rgb8(197, 193, 112),
    Color::from_rgb8(137, 190, 146),
    Color::from_rgb8(118, 188, 194),
    Color::from_rgb8(139, 170, 221),
    Color::from_rgb8(184, 151, 217),
    Color::from_rgb8(216, 145, 177),
    Color::from_rgb8(221, 158, 122),
    Color::from_rgb8(199, 181, 137),
];

/// Quiet inset for tool code; the enclosing detail panel owns the only border.
pub fn tool_code(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(colors(theme).background.into()),
        border: iced::border::rounded(RADIUS_SMALL),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_shipped_palettes_have_readable_labels_on_window_and_card_surfaces() {
        for shipped in Theme::ALL {
            let preset = preset(shipped.name());
            assert_eq!(&preset.theme, shipped);
            let c = &preset.colors;
            for surface in [c.background, c.panel, c.code] {
                for ink in [c.text, c.muted, c.accent, c.success, c.warning, c.danger] {
                    assert!(
                        ink.relative_contrast(surface) >= 4.5,
                        "{shipped}: {ink:?} on {surface:?}"
                    );
                }
            }
        }
        assert_eq!(preset("unknown").theme.name(), DEFAULT_THEME);
    }
}
