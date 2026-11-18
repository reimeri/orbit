//! Shared visual language for the native UI. Keep colors and widget styling here.
//!
//! Controls/rows use 8 px corners; cards, editors and floating surfaces use 12 px.
//! Tiny decorations use 4 px. Edge-to-edge panels, dividers, selection rectangles,
//! and icon geometry intentionally do not use the surface radius.
use iced::{
    Color, Element, Theme,
    widget::{
        self, button as button_widget, container, markdown, overlay::menu,
        pick_list as pick_list_widget, text_editor, text_input, toggler,
    },
};

pub const BACKGROUND: Color = Color::from_rgb8(24, 26, 30);
pub const TEXT: Color = Color::from_rgb8(225, 228, 234);
pub const SUCCESS: Color = Color::from_rgb8(125, 192, 154);
pub const DANGER: Color = Color::from_rgb8(224, 130, 130);
pub const WARNING: Color = Color::from_rgb8(224, 186, 120);
pub const MUTED: Color = Color::from_rgb8(141, 149, 164);
pub const PANEL: Color = Color::from_rgb8(29, 32, 37);
pub const BORDER: Color = Color::from_rgb8(48, 53, 61);
pub const ACCENT: Color = Color::from_rgb8(137, 175, 210);
const RIGHT_PANEL: Color = Color::from_rgb8(24, 27, 31);
const SECONDARY: Color = Color::from_rgb8(43, 49, 58);
const SECONDARY_HOVER: Color = Color::from_rgb8(55, 62, 74);
const POPOVER: Color = Color::from_rgb8(30, 30, 36);
const SEARCH: Color = Color::from_rgb8(39, 42, 48);
pub const ERROR_SURFACE: Color = Color::from_rgb8(66, 39, 43);
pub const SCRIM: Color = Color::from_rgba(0., 0., 0., 0.65);
pub const SELECTION: Color = Color::from_rgba8(100, 160, 230, 0.35);
pub const IMAGE_TEXT: Color = Color::WHITE;
const CODE: Color = Color::from_rgb8(17, 17, 17);

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

pub fn palette() -> iced::theme::Palette {
    iced::theme::Palette {
        background: BACKGROUND,
        text: TEXT,
        primary: ACCENT,
        success: SUCCESS,
        danger: DANGER,
        warning: WARNING,
    }
}

pub fn app() -> Theme {
    Theme::custom("Pico", palette())
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
    if !matches!(status, button_widget::Status::Disabled) {
        style.background = Some(
            match status {
                button_widget::Status::Hovered => Color::from_rgb8(183, 211, 235),
                button_widget::Status::Pressed => ACCENT,
                _ => Color::from_rgb8(167, 201, 232),
            }
            .into(),
        );
        style.text_color = BACKGROUND;
    }
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
    let mut style = text_button(theme, status);
    style.text_color = if selected {
        ACCENT
    } else if matches!(
        status,
        button_widget::Status::Hovered | button_widget::Status::Pressed
    ) {
        ACCENT.scale_alpha(0.85)
    } else {
        MUTED
    };
    style
}

/// Destructive menu action without a filled button background.
pub fn danger_text(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let mut style = text_button(theme, status);
    if !matches!(status, button_widget::Status::Disabled) {
        style.text_color = theme.palette().danger.scale_alpha(
            if matches!(status, button_widget::Status::Hovered) {
                1.
            } else {
                0.8
            },
        );
    }
    style
}

pub fn menu_button(
    theme: &Theme,
    status: button_widget::Status,
    selected: bool,
) -> button_widget::Style {
    let mut style = text_button(theme, status);
    if matches!(
        status,
        button_widget::Status::Hovered | button_widget::Status::Pressed
    ) {
        style.background = Some(SECONDARY_HOVER.into());
    } else if selected {
        style.background = Some(SECONDARY.into());
    }
    style
}

pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut style = text_input::default(theme, status);
    style.border.radius = RADIUS_CONTROL.into();
    style
}

pub fn search_input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut style = input(theme, status);
    style.background = SEARCH.into();
    style.placeholder = MUTED;
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

fn pick_list(_: &Theme, status: pick_list_widget::Status) -> pick_list_widget::Style {
    pick_list_widget::Style {
        text_color: TEXT,
        placeholder_color: MUTED,
        handle_color: MUTED,
        background: SECONDARY.into(),
        border: iced::border::rounded(RADIUS_CONTROL)
            .width(BORDER_WIDTH)
            .color(if matches!(status, pick_list_widget::Status::Active) {
                SECONDARY_HOVER
            } else {
                ACCENT
            }),
    }
}

pub fn pick_list_menu(theme: &Theme) -> menu::Style {
    let mut style = menu::default(theme);
    style.background = POPOVER.into();
    style.selected_background = Color::from_rgb8(52, 73, 91).into();
    style.selected_text_color = TEXT;
    style.border = iced::border::rounded(RADIUS_CONTROL)
        .color(SECONDARY_HOVER)
        .width(BORDER_WIDTH);
    style
}

pub fn switch<'a, Message>(on: bool) -> widget::Toggler<'a, Message> {
    widget::toggler(on)
        .size(18)
        .text_size(14)
        .style(|theme, status| {
            let mut style = toggler::default(theme, status);
            match status {
                toggler::Status::Active { is_toggled }
                | toggler::Status::Hovered { is_toggled } => {
                    style.background = if is_toggled { ACCENT } else { SECONDARY_HOVER }.into();
                    style.foreground = if is_toggled { BACKGROUND } else { TEXT }.into();
                    if matches!(status, toggler::Status::Hovered { .. }) {
                        style.background_border_width = BORDER_WIDTH;
                        style.background_border_color = MUTED;
                    }
                }
                toggler::Status::Disabled { .. } => {}
            }
            style.padding_ratio = 0.14;
            style
        })
}

pub fn markdown_style() -> markdown::Style {
    let mut style = markdown::Style::from_palette(palette());
    style.inline_code_highlight.background = CODE.into();
    style.inline_code_highlight.border.radius = RADIUS_SMALL.into();
    style.inline_code_color = TEXT;
    style
}

pub fn code_block(theme: &Theme) -> container::Style {
    let mut style = container::dark(theme);
    style.border.radius = RADIUS_SURFACE.into();
    style
}

pub fn questionnaire(_: &Theme) -> container::Style {
    container::Style {
        background: Some(PANEL.into()),
        border: iced::border::rounded(RADIUS_SURFACE),
        ..Default::default()
    }
}

pub fn questionnaire_handle(_: &Theme) -> container::Style {
    container::Style {
        background: Some(MUTED.into()),
        border: iced::border::rounded(RADIUS_SMALL),
        ..Default::default()
    }
}

pub fn option_button(
    theme: &Theme,
    status: button_widget::Status,
    selected: bool,
) -> button_widget::Style {
    let mut style = text_button(theme, status);
    style.background = Some(
        if matches!(
            status,
            button_widget::Status::Hovered | button_widget::Status::Pressed
        ) {
            SECONDARY_HOVER
        } else if selected {
            SECONDARY
        } else {
            BACKGROUND
        }
        .into(),
    );
    style
}

pub fn footer_surface(_: &Theme) -> container::Style {
    container::Style {
        background: Some(POPOVER.into()),
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
        MUTED
    };
    style
}

pub fn secondary(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    let mut style = button_widget::secondary(theme, status);
    style.border.radius = RADIUS_CONTROL.into();
    style.background = Some(
        if matches!(status, button_widget::Status::Hovered) {
            SECONDARY_HOVER
        } else {
            SECONDARY
        }
        .into(),
    );
    style.text_color = if matches!(status, button_widget::Status::Disabled) {
        MUTED
    } else {
        theme.palette().text
    };
    style
}

pub fn right_panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(RIGHT_PANEL.into()),
        ..Default::default()
    }
}
pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(PANEL.into()),
        ..Default::default()
    }
}
pub fn attachment_button(theme: &Theme, status: button_widget::Status) -> button_widget::Style {
    button_widget::Style {
        background: Some(PANEL.into()),
        text_color: theme.palette().text,
        border: iced::Border {
            color: if matches!(
                status,
                button_widget::Status::Hovered | button_widget::Status::Pressed
            ) {
                ACCENT
            } else {
                BORDER
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

pub fn card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(PANEL.into()),
        border: iced::Border {
            color: BORDER,
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
            Some(SECONDARY.into())
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
pub fn tool_code(_: &Theme) -> container::Style {
    container::Style {
        background: Some(BACKGROUND.into()),
        border: iced::border::rounded(RADIUS_SMALL),
        ..Default::default()
    }
}
