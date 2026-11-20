//! Account connections, session preferences, and image generation settings.
use super::{App, Message, SettingsTab};
use crate::{ellipsized_text::ellipsized_text, icons::Icon, theme};
use iced::theme::Base;
use iced::{
    Center, Element, Fill, Right,
    widget::{Column, column, container, row, rule, slider, space, svg, text, text_input},
};
use pico_core::{
    image_tools::{Backend, DEFAULT_API_MODEL, QUALITIES},
    models::{MODELS, model},
    web_search::Mode,
};

#[derive(Clone, Copy, PartialEq)]
struct DictionaryChoice<'a> {
    language: &'a str,
    missing: bool,
}

impl std::fmt::Display for DictionaryChoice<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.language)?;
        if self.missing {
            f.write_str(" (not found)")?;
        }
        Ok(())
    }
}

pub(super) fn caption(label: &'static str) -> iced::widget::Text<'static> {
    text(label)
        .size(10)
        .style(theme::muted)
        .font(iced::Font::MONOSPACE)
}

fn section(label: &'static str) -> Element<'static, Message> {
    section_bar(caption(label))
}

pub(super) const SECTION_INSET: u16 = 10;

pub(super) fn section_bar<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .width(Fill)
        .padding([6, SECTION_INSET])
        .style(|t| {
            container::background(theme::colors(t).panel)
                .border(iced::border::rounded(theme::RADIUS_SMALL))
        })
        .into()
}

fn status(label: &'static str, active: bool) -> Element<'static, Message> {
    container(text(label).size(11))
        .padding([3, 8])
        .style(move |t| {
            let c = theme::colors(t);
            let color = if active { c.success } else { c.muted };
            container::background(color.scale_alpha(theme::BADGE_ALPHA))
                .color(color)
                .border(iced::border::rounded(12))
        })
        .into()
}

impl App {
    pub(super) fn account_settings(&self) -> Column<'_, Message> {
        let subscription: Element<'_, Message> = if let Some(account) = &self.account {
            row![
                column![
                    row![text("ChatGPT").size(16), status("Connected", true)]
                        .spacing(6)
                        .align_y(Center),
                    ellipsized_text(account, 13., Some(self.preset().colors.muted))
                ]
                .spacing(4)
                .width(Fill),
                theme::button(Icon::LogOut.label(text("Sign out").size(13), 14.))
                    .padding([7, 9])
                    .style(theme::secondary)
                    .on_press(Message::Logout),
            ]
            .spacing(8)
            .align_y(Center)
            .into()
        } else if self.login_cancel.is_some() {
            let copy = self.login_url.as_ref().map(|url| {
                theme::action("Copy login URL")
                    .on_press(Message::Copy(url.clone()))
                    .style(theme::secondary)
            });
            column![
                text("Finish sign-in in your browser.").size(14),
                row![]
                    .push(copy)
                    .push(theme::cancel("Cancel login").on_press(Message::CancelLogin))
                    .spacing(6)
                    .wrap()
            ]
            .spacing(8)
            .into()
        } else {
            theme::button(
                Icon::LogIn.label(theme::primary_label("Sign in with ChatGPT").size(14), 15.),
            )
            .padding([8, 12])
            .on_press_maybe(self.harness.as_ref().map(|_| Message::Login))
            .into()
        };
        let configured = self.api_key_configured;
        let key = text_input(
            if configured {
                "Enter a replacement key"
            } else {
                "Enter API key"
            },
            &self.api_key_draft,
        )
        .secure(true)
        .on_input(Message::ApiKey)
        .style(theme::input)
        .size(13)
        .padding(8);
        column![
            section("CHATGPT SUBSCRIPTION"),
            subscription,
            rule::horizontal(1),
            section("OPENAI API KEY / IMAGES ONLY"),
            row![
                text("Private key").size(16),
                status(if configured { "Saved" } else { "Not saved" }, configured)
            ]
            .spacing(8)
            .align_y(Center),
            row![
                key,
                theme::primary_action("Save key").on_press_maybe(
                    (!self.api_key_draft.trim().is_empty()).then_some(Message::SaveApiKey)
                )
            ]
            .spacing(8)
            .align_y(Center)
        ]
        .push(configured.then(|| {
            theme::action("Remove key")
                .style(theme::danger_text)
                .on_press(Message::RemoveApiKey)
        }))
        .spacing(14)
    }

    fn preference_row<'a>(
        &self,
        label: &'static str,
        control: impl Into<Element<'a, Message>>,
    ) -> iced::widget::Row<'a, Message> {
        let control_width = (self.panel_width() * 0.36).clamp(84., 176.);
        row![
            text(label).size(14).width(Fill),
            container(control).align_right(control_width)
        ]
        .padding([7, 4])
        .spacing(8)
        .align_y(Center)
    }

    pub(super) fn general_settings<'a>(&'a self) -> Column<'a, Message> {
        let number = |placeholder, value, message: fn(String) -> Message| {
            text_input(placeholder, value)
                .on_input(message)
                .size(13)
                .padding(8)
                .style(theme::input)
        };
        let appearance = &self.catalog.settings;
        let measure = |label, control: Element<'a, Message>, value| {
            row![
                text(label)
                    .size(14)
                    .width((self.panel_width() * 0.32).clamp(76., 180.)),
                control,
                text(format!("{value:.0} px"))
                    .size(12)
                    .style(theme::muted)
                    .width(49)
                    .align_x(Right)
            ]
            .padding([8, 4])
            .spacing(8)
            .align_y(Center)
        };
        let web_help = match self.settings.web_search {
            Mode::Live => "Live uses the web through your ChatGPT subscription.",
            Mode::Cached => "Cached requests stored results; some operations may be limited.",
            Mode::Disabled => "Disabled prevents new web calls.",
        };
        column![
            section("SESSION DEFAULTS"),
            self.preference_row(
                "Default model",
                theme::dropdown(MODELS, model(&self.settings.model), |m| Message::Model(
                    m.id.into()
                ))
            ),
            rule::horizontal(1),
            self.preference_row(
                "Context limit (tokens)",
                number("272000", &self.context_text, Message::Context)
            ),
            rule::horizontal(1),
            self.preference_row(
                "Auto-compact at (%)",
                number("80", &self.threshold_text, Message::Threshold)
            ),
            rule::horizontal(1),
            self.preference_row(
                "Restore last session",
                theme::switch(appearance.resume_last).on_toggle(Message::Resume)
            ),
            space().height(8),
            section("WEB ACCESS"),
            self.preference_row(
                "Search mode",
                theme::dropdown(
                    [Mode::Live, Mode::Cached, Mode::Disabled],
                    Some(self.settings.web_search),
                    Message::WebMode
                )
            ),
            container(text(web_help).size(12).style(theme::muted)).padding([3, 4]),
            space().height(8),
            section("APPEARANCE"),
            self.preference_row(
                "Theme",
                theme::dropdown(
                    theme::PRESETS
                        .iter()
                        .map(|p| p.theme.name())
                        .collect::<Vec<_>>(),
                    Some(self.preset().theme.name()),
                    Message::Theme
                )
            ),
            rule::horizontal(1),
            measure(
                "Chat text",
                slider(12.0..=24.0, appearance.font_size, Message::Font)
                    .step(1_f32)
                    .into(),
                appearance.font_size
            ),
            rule::horizontal(1),
            measure(
                "Chat width",
                slider(480.0..=1600.0, appearance.chat_width, Message::ChatWidth)
                    .step(20_f32)
                    .into(),
                appearance.chat_width
            ),
            space().height(8),
            section("SPELL CHECK"),
            self.preference_row(
                "Check composer",
                theme::switch(self.settings.spellcheck.enabled)
                    .on_toggle(Message::SpellcheckEnabled)
            ),
            self.preference_row("Dictionary", self.dictionary_picker())
        ]
        .spacing(3)
    }

    fn dictionary_picker(&self) -> Element<'_, Message> {
        let inventory = self.spellcheck.languages.as_deref();
        let language = &self.settings.spellcheck.language;
        let selected = DictionaryChoice {
            language,
            missing: inventory.is_some_and(|known| !known.contains(language)),
        };
        let options: Vec<_> = inventory
            .into_iter()
            .flatten()
            .map(|language| DictionaryChoice {
                language,
                missing: false,
            })
            .chain(selected.missing.then_some(selected))
            .collect();
        let picker = theme::dropdown(options, Some(selected), |choice| {
            Message::SpellcheckLanguage(choice.language.to_owned())
        });
        if self.settings.spellcheck.enabled
            && *language == self.catalog.settings.spellcheck.language
            && let Some(error) = self.spellcheck.unavailable.as_deref()
        {
            crate::icons::tip(picker, error)
        } else {
            picker.into()
        }
    }

    pub(super) fn image_settings(&self) -> Column<'_, Message> {
        let images = &self.settings.images;
        let colors = &self.preset().colors;
        // Leave room for the longer subscription label beside the circle in each half.
        let inline = self.panel_width() >= 400.;
        let choice = |backend, label, detail| {
            let selected = images.backend == backend;
            let color = if selected {
                colors.accent
            } else {
                colors.muted
            };
            // Rasterize the ring and dot together so pixel snapping cannot offset their centers.
            let mark: &[u8] = if selected {
                br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16"><circle cx="8" cy="8" r="7" fill="none" stroke="black" stroke-width="1.5"/><circle cx="8" cy="8" r="4"/></svg>"#
            } else {
                br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16"><circle cx="8" cy="8" r="7" fill="none" stroke="black" stroke-width="1.5"/></svg>"#
            };
            let indicator = svg(svg::Handle::from_memory(mark))
                .width(16)
                .height(16)
                .style(move |_, _| svg::Style { color: Some(color) });
            let labels = column![
                theme::primary_label(label).size(16),
                text(detail).size(12).style(theme::muted)
            ]
            .spacing(5);
            let content: Element<'_, Message> = if inline {
                row![container(indicator).center_y(21), labels]
                    .spacing(8)
                    .into()
            } else {
                column![indicator, labels].spacing(5).into()
            };
            theme::button(content)
                .width(Fill)
                .height(if inline {
                    68
                } else if self.panel_width() < 360. {
                    108
                } else {
                    90
                })
                .padding(12)
                .style(move |t, s| {
                    let mut style = theme::card_button(t, s);
                    if selected {
                        style.border.color = colors.accent;
                        style.background = Some(colors.accent.scale_alpha(0.13).into());
                    }
                    style
                })
                .on_press(Message::ImageBackend(backend))
        };
        let mut settings = column![
            row![
                choice(Backend::Codex, "Codex", "ChatGPT subscription"),
                choice(Backend::Api, "OpenAI API", "Billed separately")
            ]
            .spacing(10)
        ]
        .spacing(14);
        if images.backend == Backend::Api && !self.api_key_configured {
            settings = settings.push(
                row![
                    container(space().width(3).height(20))
                        .style(move |_| container::background(colors.warning)),
                    column![
                        text("An API key is required before you can generate.")
                            .size(13)
                            .style(theme::muted),
                        theme::button(
                            row![
                                text("Add key in Account").size(13).color(colors.accent),
                                Icon::ArrowRight.colored(13., colors.accent)
                            ]
                            .align_y(Center)
                            .spacing(4)
                        )
                        .padding([4, 0])
                        .style(theme::text_button)
                        .on_press(Message::SettingsTab(SettingsTab::Account))
                    ]
                    .spacing(4)
                ]
                .spacing(10),
            );
        }
        let mut model_settings = column![
            section("GENERATION & EDITING"),
            self.preference_row(
                "API model",
                text_input(DEFAULT_API_MODEL, &images.model)
                    .style(theme::input)
                    .size(13)
                    .on_input(Message::ImageModel)
                    .padding(8),
            )
        ]
        .spacing(3);
        if images.model != DEFAULT_API_MODEL {
            let reset = theme::button(
                Icon::RotateCcw.label(
                    text("Reset to current default")
                        .size(13)
                        .color(colors.accent),
                    13.,
                ),
            )
            .style(theme::text_button)
            .padding([4, 0])
            .on_press(Message::ResetImageModel);
            model_settings =
                model_settings.push(container(reset).padding([0, 4]).align_right(Fill));
        }
        let output = column![
            section("OUTPUT / BOTH BACKENDS"),
            self.preference_row(
                "Quality",
                theme::dropdown(QUALITIES, Some(images.quality.as_str()), |q| {
                    Message::ImageQuality(q.into())
                }),
            ),
            rule::horizontal(1),
            self.preference_row("Size", text("Automatic").size(13).style(theme::muted))
        ]
        .spacing(3);
        settings.push(model_settings).push(output)
    }
}
