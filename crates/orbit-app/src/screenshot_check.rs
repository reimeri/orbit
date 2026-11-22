//! Native screenshot-ordering regression, shown only at the start of --smoke-test.
//! Explicit fixture geometry/colors are independent of the application's styling.
use iced::{
    Color, Element, Fill, Rectangle,
    widget::{column, container, row, space, text},
    window::Screenshot,
};

const PADDING: f32 = 20.;
const TEXT_WIDTH: f32 = 280.;
const TEXT_HEIGHT: f32 = 64.;
const GAP: f32 = 12.;
const MARKER_WIDTH: f32 = 80.;
const MARKER_HEIGHT: f32 = 32.;
const SHIFT: f32 = 100.;
const INITIAL_COLOR: Color = Color::from_rgb8(224, 48, 96);
const UPDATED_COLOR: Color = Color::from_rgb8(32, 176, 208);

#[derive(Default)]
pub enum Check {
    #[default]
    Initial,
    Updated {
        first: Option<Screenshot>,
    },
}

pub enum Progress {
    CaptureUpdatedPair,
    AwaitSecond,
    Complete,
}

impl Check {
    pub fn view<Message: 'static>(&self) -> Element<'static, Message> {
        let (label, offset, color) = match self {
            Self::Initial => ("Before capture", 0., INITIAL_COLOR),
            Self::Updated { .. } => ("Updated capture", SHIFT, UPDATED_COLOR),
        };
        container(
            column![
                container(text(label).size(28).color(Color::WHITE))
                    .width(TEXT_WIDTH)
                    .height(TEXT_HEIGHT),
                row![
                    space().width(offset),
                    container(space())
                        .width(MARKER_WIDTH)
                        .height(MARKER_HEIGHT)
                        .style(move |_| container::Style {
                            background: Some(color.into()),
                            ..Default::default()
                        }),
                ],
            ]
            .spacing(GAP),
        )
        .padding(PADDING)
        .width(Fill)
        .height(Fill)
        .style(|_| container::Style {
            background: Some(Color::BLACK.into()),
            ..Default::default()
        })
        .into()
    }

    pub fn receive(&mut self, screenshot: Screenshot) -> Result<Progress, String> {
        let crop = |bounds: Rectangle| {
            let scale = screenshot.scale_factor;
            let x = (bounds.x * scale).floor() as u32;
            let y = (bounds.y * scale).floor() as u32;
            screenshot
                .crop(Rectangle {
                    x,
                    y,
                    width: ((bounds.x + bounds.width) * scale).ceil() as u32 - x,
                    height: ((bounds.y + bounds.height) * scale).ceil() as u32 - y,
                })
                .map_err(|error| {
                    format!("screenshot fixture is outside the captured frame: {error}")
                })
        };
        let text = crop(Rectangle {
            x: PADDING,
            y: PADDING,
            width: TEXT_WIDTH,
            height: TEXT_HEIGHT,
        })?;
        if !text
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[..3].iter().all(|&channel| channel > 180))
        {
            return Err("screenshot omitted the fixture's text".into());
        }
        let updated = matches!(self, Self::Updated { .. });
        // Inspect both positions: the new marker must appear and the old one
        // must disappear. Nonblank pixels alone would accept a stale scene.
        for (position, color) in [(false, INITIAL_COLOR), (true, UPDATED_COLOR)] {
            let sample = crop(Rectangle {
                x: PADDING + if position { SHIFT } else { 0. } + MARKER_WIDTH / 2.,
                y: PADDING + TEXT_HEIGHT + GAP + MARKER_HEIGHT / 2.,
                width: 1.,
                height: 1.,
            })?;
            let expected = if position == updated {
                color
            } else {
                Color::BLACK
            }
            .into_rgba8();
            if !sample
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| (0..3).all(|channel| pixel[channel].abs_diff(expected[channel]) <= 2))
            {
                return Err("screenshot captured stale fixture content or placement".into());
            }
        }
        match self {
            Self::Initial => {
                *self = Self::Updated { first: None };
                Ok(Progress::CaptureUpdatedPair)
            }
            Self::Updated { first } => match first {
                None => {
                    *first = Some(screenshot);
                    Ok(Progress::AwaitSecond)
                }
                Some(first) => {
                    if first.size != screenshot.size || first.rgba != screenshot.rgba {
                        return Err("queued captures did not return the same updated frame".into());
                    }
                    Ok(Progress::Complete)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{Size, advanced::renderer, mouse};
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn validator_rejects_missing_text_and_stale_content_at_fractional_scale() {
        let size = Size::new(360., 180.);
        let mut renderer = crate::test_renderer::new(16.);
        for scale in [1., 1.25, 2.] {
            let mut check = Check::default();
            let mut capture = |check: &Check| {
                let mut ui =
                    UserInterface::build(check.view::<()>(), size, Cache::new(), &mut renderer);
                ui.draw(
                    &mut renderer,
                    &crate::theme::app(),
                    &renderer::Style {
                        text_color: Color::WHITE,
                    },
                    mouse::Cursor::Unavailable,
                );
                let physical = Size::new((size.width * scale) as u32, (size.height * scale) as u32);
                Screenshot::new(
                    crate::test_renderer::screenshot(&mut renderer, physical, scale, Color::BLACK),
                    physical,
                    scale,
                )
            };
            let initial = capture(&check);
            assert!(matches!(
                check.receive(initial.clone()),
                Ok(Progress::CaptureUpdatedPair)
            ));
            assert!(
                check.receive(initial).is_err(),
                "old text/marker scene must not pass as updated"
            );
            let updated = capture(&check);
            let mut missing_text = updated.rgba.to_vec();
            for y in (PADDING * scale) as u32..((PADDING + TEXT_HEIGHT) * scale) as u32 {
                for x in (PADDING * scale) as u32..((PADDING + TEXT_WIDTH) * scale) as u32 {
                    let offset = ((y * updated.size.width + x) * 4) as usize;
                    missing_text[offset..offset + 3].fill(0);
                }
            }
            assert!(
                check
                    .receive(Screenshot::new(missing_text, updated.size, scale))
                    .is_err(),
                "nonblank marker pixels must not hide missing text"
            );
            assert!(matches!(
                check.receive(updated.clone()),
                Ok(Progress::AwaitSecond)
            ));
            assert!(matches!(check.receive(updated), Ok(Progress::Complete)));
        }
    }
}
