//! A single-line label fitted with real font measurements, not character counts.
use iced::{
    Color, Element, Font, Length, Rectangle, Size,
    advanced::{
        Layout, Widget, layout, renderer,
        text::{self, Renderer as _, paragraph},
        widget::{Operation, Tree, tree},
    },
    mouse, widget,
};
use unicode_segmentation::UnicodeSegmentation;

pub fn ellipsized_text<'a, Message: 'a>(
    value: &str,
    size: f32,
    color: Option<Color>,
) -> Element<'a, Message> {
    Element::new(Label {
        value: value.replace(['\n', '\r'], " "),
        size,
        color,
        width: Length::Fill,
    })
}

pub fn shrinking_ellipsized_text<'a, Message: 'a>(
    value: &str,
    size: f32,
    color: Option<Color>,
) -> Element<'a, Message> {
    Element::new(Label {
        value: value.replace(['\n', '\r'], " "),
        size,
        color,
        width: Length::Shrink,
    })
}

struct Label {
    value: String,
    size: f32,
    color: Option<Color>,
    width: Length,
}

#[derive(Default)]
struct State {
    key: Option<(String, f32, f32, Font)>,
    fitted_width: f32,
    paragraph: paragraph::Plain<<iced::Renderer as text::Renderer>::Paragraph>,
    ellipsis: paragraph::Plain<<iced::Renderer as text::Renderer>::Paragraph>,
}

impl<Message> Widget<Message, iced::Theme, iced::Renderer> for Label {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        // Whole-pixel line heights keep repeated sidebar rows on the same pixel grid.
        Size::new(self.width, Length::Fixed((self.size * 1.3).round()))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let width = limits.max().width;
        let font = renderer.default_font();
        let key = (self.value.clone(), width, self.size, font);
        if state.key.as_ref() != Some(&key) {
            let format = widget::text::Format {
                size: Some(self.size.into()),
                font: Some(font),
                line_height: text::LineHeight::Absolute(((self.size * 1.3).round()).into()),
                wrapping: text::Wrapping::None,
                ..Default::default()
            };
            let unlimited = layout::Limits::new(Size::ZERO, Size::INFINITE);
            fit(state, &self.value, width, self.size, |paragraph, value| {
                widget::text::layout::<iced::Renderer>(
                    paragraph, renderer, &unlimited, value, format,
                )
                .size()
                .width
            });
            state.key = Some(key);
        }
        layout::Node::new(limits.resolve(
            self.width,
            (self.size * 1.3).round(),
            Size::new(state.fitted_width, 0.),
        ))
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
        let state = tree.state.downcast_ref::<State>();
        if let Some(clip) = layout.bounds().intersection(viewport) {
            widget::text::draw(
                renderer,
                style,
                layout.bounds(),
                state.paragraph.raw(),
                widget::text::Style { color: self.color },
                &clip,
            );
        }
    }

    fn operate(
        &mut self,
        _tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.text(None, layout.bounds(), &self.value);
    }
}

type Plain = paragraph::Plain<<iced::Renderer as text::Renderer>::Paragraph>;

fn fit(
    state: &mut State,
    value: &str,
    width: f32,
    size: f32,
    mut measure: impl FnMut(&mut Plain, &str) -> f32,
) {
    if width == 0. {
        state.fitted_width = measure(&mut state.paragraph, "");
        return;
    }

    // Start with roughly three graphemes per em plus a little headroom. This is only
    // a work estimate: actual glyph measurements decide whether more is needed.
    // Double the prefix until it overflows or reaches the original end, rather
    // than shaping a long suffix that cannot appear in this single-line label.
    let mut count = ((width / size).ceil() as usize)
        .saturating_mul(3)
        .saturating_add(8)
        .max(32);
    let probe = loop {
        let end = value
            .grapheme_indices(true)
            .nth(count)
            .map_or(value.len(), |(byte, _)| byte);
        let probe = &value[..end];
        state.fitted_width = measure(&mut state.paragraph, probe);
        if state.fitted_width > width {
            break probe;
        }
        if end == value.len() {
            return;
        }
        count = count.saturating_mul(2);
    };
    let ellipsis_width = measure(&mut state.ellipsis, "…");
    if ellipsis_width > width {
        state.fitted_width = measure(&mut state.paragraph, "");
        return;
    }

    // Glyphs are in visual (bidi) order, but truncation removes a logical suffix.
    // Charge each cluster's advance at its start, then accumulate in byte order.
    // A ligature spanning several graphemes is charged in full at the first one;
    // this may select a slightly shorter prefix, but never cuts a grapheme.
    let buffer = state.paragraph.raw().buffer();
    let mut offset = 0;
    let offsets: Vec<_> = buffer
        .lines
        .iter()
        .map(|line| {
            let start = offset;
            offset += line.text().len() + line.ending().as_str().len();
            start
        })
        .collect();
    let mut advances = vec![0.; probe.len() + 1];
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            advances[offsets[run.line_i] + glyph.start + 1] += glyph.w;
        }
    }
    for i in 1..advances.len() {
        advances[i] += advances[i - 1];
    }
    let prefixes: Vec<_> = probe
        .grapheme_indices(true)
        .map(|(byte, _)| byte)
        .chain((probe.len() < value.len()).then_some(probe.len()))
        .map(|byte| (byte, advances[byte]))
        .collect();
    let mut index = prefixes
        .partition_point(|(_, advance)| *advance <= width - ellipsis_width)
        .saturating_sub(1);
    loop {
        if index == 0 {
            state.paragraph = state.ellipsis.clone();
            state.fitted_width = ellipsis_width;
            return;
        }
        let (byte, advance) = prefixes[index];
        let fitted = format!("{}…", &value[..byte]);
        state.fitted_width = measure(&mut state.paragraph, &fitted);
        if state.fitted_width <= width {
            return;
        }
        // Cutting a shaping run can change ligatures, joining and bidi layout.
        // Verify the final text, backing off by the actual excess only if needed.
        // The ordinary path shapes one candidate, not every binary-search probe.
        let budget = advance - (state.fitted_width - width);
        index = prefixes[..index]
            .partition_point(|(_, advance)| *advance <= budget)
            .saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer() -> iced::Renderer {
        crate::test_renderer::new(16.)
    }

    fn measure(renderer: &iced::Renderer, paragraph: &mut Plain, value: &str) -> f32 {
        widget::text::layout::<iced::Renderer>(
            paragraph,
            renderer,
            &layout::Limits::new(Size::ZERO, Size::INFINITE),
            value,
            widget::text::Format {
                size: Some(12.into()),
                wrapping: text::Wrapping::None,
                ..Default::default()
            },
        )
        .size()
        .width
    }

    #[test]
    fn long_labels_only_shape_a_bounded_grapheme_safe_prefix() {
        let renderer = renderer();
        for unit in [
            "cargo test --locked ",
            "ae\u{301} 👩‍💻 路径/λ.rs ",
            "abc שלום مرحبا ",
        ] {
            let value = unit.repeat(1000);
            let mut state = State::default();
            let mut shaped_bytes = 0;
            fit(&mut state, &value, 180., 12., |p, text| {
                shaped_bytes += text.len();
                measure(&renderer, p, text)
            });
            assert!(
                shaped_bytes < 4096,
                "a narrow label must not shape its enormous suffix"
            );
            let fitted = state.paragraph.content();
            let prefix = fitted
                .strip_suffix('…')
                .expect("unshown suffix needs an ellipsis");
            assert!(value.starts_with(prefix) && !prefix.is_empty());
            assert!(value.grapheme_indices(true).any(|(i, _)| i == prefix.len()));
            assert!(state.fitted_width <= 180.);
        }
    }

    #[test]
    fn a_long_label_that_fits_is_not_truncated() {
        let renderer = renderer();
        let value = "i".repeat(512);
        let width = measure(&renderer, &mut Plain::default(), &value);
        let mut state = State::default();
        for available in [width, f32::INFINITY] {
            fit(&mut state, &value, available, 12., |p, text| {
                measure(&renderer, p, text)
            });
            assert_eq!(state.paragraph.content(), value);
        }
        fit(&mut state, &value, 0., 12., |p, text| {
            measure(&renderer, p, text)
        });
        assert_eq!(state.paragraph.content(), "");
    }

    #[test]
    fn final_candidate_overflow_backs_off_and_draws_the_verified_result() {
        let renderer = renderer();
        let value = "nix develop --command cargo test --locked -p pico-core";
        let mut state = State::default();
        // Model contextual shaping: adding an ellipsis to a run can make it
        // wider than the sum of its separately measured pieces.
        let shaped_width = |paragraph: &mut Plain, text: &str| {
            measure(&renderer, paragraph, text)
                + if text != "…" && text.ends_with('…') {
                    30.
                } else {
                    0.
                }
        };
        fit(&mut state, value, 180., 12., shaped_width);
        let fitted = state.paragraph.content();
        let prefix = fitted.strip_suffix('…').unwrap();
        assert!(!prefix.is_empty() && value.starts_with(prefix));
        assert!(shaped_width(&mut Plain::default(), fitted) <= 180.);
    }

    #[test]
    fn narrow_unicode_bidi_and_contextual_labels_remain_whole_and_fit() {
        let renderer = renderer();
        let ellipsis_width = measure(&renderer, &mut Plain::default(), "…");
        let mut state = State::default();
        fit(&mut state, "界long", ellipsis_width - 0.5, 12., |p, s| {
            measure(&renderer, p, s)
        });
        assert_eq!(state.paragraph.content(), "");
        fit(&mut state, "WWWW", ellipsis_width, 12., |p, s| {
            measure(&renderer, p, s)
        });
        assert_eq!(state.paragraph.content(), "…");
        fit(&mut state, "", 0., 12., |p, s| measure(&renderer, p, s));
        assert_eq!(state.paragraph.content(), "");

        for value in [
            "ae\u{301}z 👩‍💻 file 路径/λ.rs",
            "office affinity ffi official tail",
            "مرحبا بالعالم اختبار المسار الطويل",
            "abc שלום 123 مرحبا xyz/file.rs",
        ] {
            let full_width = measure(&renderer, &mut Plain::default(), value);
            let boundaries: Vec<_> = value.grapheme_indices(true).map(|(i, _)| i).collect();
            for width in [
                0.,
                ellipsis_width,
                full_width * 0.37,
                full_width - 0.5,
                full_width,
            ] {
                fit(&mut state, value, width, 12., |p, s| {
                    measure(&renderer, p, s)
                });
                let fitted = state.paragraph.content();
                assert!(
                    state.fitted_width <= width,
                    "{value:?} -> {fitted:?} at {width}"
                );
                if width >= full_width {
                    assert_eq!(fitted, value);
                    continue;
                }
                if width < ellipsis_width {
                    assert_eq!(fitted, "");
                    continue;
                }
                let prefix = fitted.strip_suffix('…').expect("truncation indicator");
                assert!(
                    value.starts_with(prefix),
                    "must truncate in logical, not visual bidi order"
                );
                assert!(
                    boundaries.contains(&prefix.len()),
                    "must preserve entire graphemes"
                );
                assert_eq!(
                    state.fitted_width,
                    measure(&renderer, &mut Plain::default(), fitted),
                    "the measured paragraph must be the one we draw"
                );
            }
        }
    }

    #[test]
    fn retained_layout_reacts_to_content_size_and_width() {
        let renderer = renderer();
        let mut label = Label {
            value: "nix develop --command cargo test --locked".into(),
            size: 12.,
            color: None,
            width: Length::Shrink,
        };
        let mut tree = Tree::empty();
        tree.state = tree::State::new(State::default());
        let limits = layout::Limits::new(Size::ZERO, Size::new(120., 30.));
        let layout = <Label as Widget<(), iced::Theme, iced::Renderer>>::layout(
            &mut label, &mut tree, &renderer, &limits,
        );
        assert!(layout.size().width <= 120.);
        <Label as Widget<(), iced::Theme, iced::Renderer>>::layout(
            &mut label, &mut tree, &renderer, &limits,
        );
        assert!(tree.state.downcast_ref::<State>().fitted_width <= 120.);
        let wide = layout::Limits::new(Size::ZERO, Size::new(900., 40.));
        <Label as Widget<(), iced::Theme, iced::Renderer>>::layout(
            &mut label, &mut tree, &renderer, &wide,
        );
        assert_eq!(
            tree.state.downcast_ref::<State>().paragraph.content(),
            label.value
        );
        // Change only size: changing the text too would hide a stale-size cache.
        let before = tree.state.downcast_ref::<State>().fitted_width;
        label.size = 18.;
        <Label as Widget<(), iced::Theme, iced::Renderer>>::layout(
            &mut label, &mut tree, &renderer, &wide,
        );
        let expected = widget::text::layout::<iced::Renderer>(
            &mut Plain::default(),
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::INFINITE),
            &label.value,
            widget::text::Format {
                size: Some(label.size.into()),
                wrapping: text::Wrapping::None,
                ..Default::default()
            },
        )
        .size()
        .width;
        assert!(
            expected > before,
            "fixture must distinguish the two font sizes"
        );
        assert_eq!(tree.state.downcast_ref::<State>().fitted_width, expected);
        label.value = "new λ".into();
        <Label as Widget<(), iced::Theme, iced::Renderer>>::layout(
            &mut label, &mut tree, &renderer, &limits,
        );
        assert_eq!(
            tree.state.downcast_ref::<State>().paragraph.content(),
            "new λ"
        );
    }
}
