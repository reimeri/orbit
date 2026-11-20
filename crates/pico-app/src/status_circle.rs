//! Shared status circle, animated by the foreground activity clock.
use iced::{
    Element,
    widget::{container, text},
};

pub const WIDTH: f32 = 12.;

pub fn status_circle<'a, Message: 'a>(running: bool, tick: u64) -> Element<'a, Message> {
    let dot = if running {
        ["◦", "○", "●", "○"][(tick as usize / 2) % 4]
    } else {
        "●"
    };
    container(
        text(dot)
            .size(11)
            .style(move |t| iced::widget::text::Style {
                color: Some(if running {
                    crate::theme::colors(t).accent
                } else {
                    crate::theme::colors(t).muted
                }),
            }),
    )
    .width(WIDTH)
    .into()
}
