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
            fit(state, &self.value, width, |paragraph, value| {
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
    mut measure: impl FnMut(&mut Plain, &str) -> f32,
) {
    state.fitted_width = measure(&mut state.paragraph, value);
    if state.fitted_width <= width {
        return;
    }
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
    let mut advances = vec![0.; value.len() + 1];
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            advances[offsets[run.line_i] + glyph.start + 1] += glyph.w;
        }
    }
    for i in 1..advances.len() {
        advances[i] += advances[i - 1];
    }
    let prefixes: Vec<_> = value
        .grapheme_indices(true)
        .map(|(byte, _)| (byte, advances[byte]))
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
        iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            Font::DEFAULT,
            iced::Pixels(16.),
        ))
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
    fn fits_real_glyphs_without_shaping_a_binary_search_of_prefixes() {
        let renderer = renderer();
        let value = "nix develop --command cargo test --locked -p pico-core";
        let mut state = State::default();
        let mut calls = 0;
        fit(&mut state, value, 180., |paragraph, text| {
            calls += 1;
            measure(&renderer, paragraph, text)
        });
        assert_eq!(calls, 3, "full label, ellipsis, and one verified candidate");
        assert!(state.paragraph.content().starts_with("nix develop"));
        assert!(state.paragraph.content().ends_with('…'));
        assert!(state.fitted_width <= 180.);

        let complete_width = measure(&renderer, &mut Plain::default(), value);
        for width in [complete_width, complete_width + 100.] {
            let mut calls = 0;
            fit(&mut state, value, width, |paragraph, text| {
                calls += 1;
                measure(&renderer, paragraph, text)
            });
            assert_eq!(calls, 1, "a fitting label needs no candidate shaping");
            assert_eq!(state.paragraph.content(), value);
        }
    }

    #[test]
    fn final_candidate_overflow_backs_off_and_draws_the_verified_result() {
        let renderer = renderer();
        let value = "nix develop --command cargo test --locked -p pico-core";
        let mut state = State::default();
        let mut calls = 0;
        let mut first_candidate = None;
        fit(&mut state, value, 180., |paragraph, text| {
            calls += 1;
            let width = measure(&renderer, paragraph, text);
            if text.ends_with('…') && text != "…" && first_candidate.is_none() {
                first_candidate = Some(text.to_owned());
                // Model an increase from contextual reshaping without depending
                // on a particular font's kerning/Arabic joining at the cutoff.
                210.
            } else {
                width
            }
        });
        assert_eq!(
            calls, 4,
            "verify the first candidate and the corrected prefix"
        );
        let first = first_candidate.unwrap();
        let final_text = state.paragraph.content();
        assert!(final_text.len() < first.len());
        assert!(final_text.ends_with('…'));
        assert!(state.fitted_width <= 180.);
        assert_eq!(
            state.fitted_width,
            measure(&renderer, &mut Plain::default(), final_text)
        );
    }

    #[test]
    fn narrow_unicode_bidi_and_contextual_labels_remain_whole_and_fit() {
        let renderer = renderer();
        let ellipsis_width = measure(&renderer, &mut Plain::default(), "…");
        let mut state = State::default();
        fit(&mut state, "界long", ellipsis_width - 0.5, |p, s| {
            measure(&renderer, p, s)
        });
        assert_eq!(state.paragraph.content(), "");
        fit(&mut state, "WWWW", ellipsis_width, |p, s| {
            measure(&renderer, p, s)
        });
        assert_eq!(state.paragraph.content(), "…");
        fit(&mut state, "", 0., |p, s| measure(&renderer, p, s));
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
                fit(&mut state, value, width, |p, s| measure(&renderer, p, s));
                let fitted = state.paragraph.content();
                assert!(
                    state.fitted_width <= width,
                    "{value:?} -> {fitted:?} at {width}"
                );
                if fitted == value || fitted.is_empty() {
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
    fn retained_layout_reuses_paragraph_and_reacts_to_content_size_and_width() {
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
        let original = tree.state.downcast_ref::<State>().paragraph.raw().clone();
        assert!(layout.size().width <= 120.);
        <Label as Widget<(), iced::Theme, iced::Renderer>>::layout(
            &mut label, &mut tree, &renderer, &limits,
        );
        assert!(std::ptr::eq(
            original.buffer(),
            tree.state.downcast_ref::<State>().paragraph.raw().buffer()
        ));
        let wide = layout::Limits::new(Size::ZERO, Size::new(900., 40.));
        <Label as Widget<(), iced::Theme, iced::Renderer>>::layout(
            &mut label, &mut tree, &renderer, &wide,
        );
        assert_eq!(
            tree.state.downcast_ref::<State>().paragraph.content(),
            label.value
        );
        label.value = "new λ".into();
        label.size = 18.;
        <Label as Widget<(), iced::Theme, iced::Renderer>>::layout(
            &mut label, &mut tree, &renderer, &limits,
        );
        assert_eq!(
            tree.state.downcast_ref::<State>().paragraph.content(),
            "new λ"
        );
        assert_eq!(
            tree.state.downcast_ref::<State>().key.as_ref().unwrap().2,
            18.
        );
    }
}
