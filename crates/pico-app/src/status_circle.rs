//! Shared status circle, animated by the foreground activity clock.
use iced::{
    Element,
    widget::{container, text},
};

pub fn status_circle<'a, Message: 'a>(running: bool, tick: u64) -> Element<'a, Message> {
    let dot = if running {
        ["◦", "○", "●", "○"][(tick as usize / 2) % 4]
    } else {
        "●"
    };
    container(text(dot).size(11).color(if running {
        crate::theme::ACCENT
    } else {
        crate::theme::MUTED
    }))
    .width(12)
    .into()
}
