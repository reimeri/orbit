//! Shared, quiet scrollbars with a wider hit area than their visible thumb.
use crate::theme;
use iced::{
    Element, Theme, border,
    widget::{Scrollable, scrollable},
};

pub fn scroll<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> Scrollable<'a, Message> {
    scrollable(content)
        .direction(scrollable::Direction::Vertical(rail()))
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
