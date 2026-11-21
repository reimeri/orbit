//! Lucide robot with a separate running-subagent indicator.
use iced::{
    Element,
    widget::{container, space, stack},
};

pub fn agent_icon<'a, Message: 'a>(running: bool) -> Element<'a, Message> {
    let bot = container(crate::icons::Icon::Bot.view(24.))
        .center_x(26)
        .center_y(26);
    if !running {
        return bot.into();
    }
    let dot = container(space().width(6).height(6)).style(|theme: &iced::Theme| container::Style {
        background: Some(theme.palette().primary.into()),
        border: iced::Border {
            radius: 3.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    stack![bot, container(dot).align_right(26)].into()
}
