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
    })
}

struct Label {
    value: String,
    size: f32,
    color: Option<Color>,
}

#[derive(Default)]
struct State {
    key: Option<(String, f32, f32, Font)>,
    paragraph: paragraph::Plain<<iced::Renderer as text::Renderer>::Paragraph>,
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
        Size::new(Length::Fill, Length::Fixed((self.size * 1.3).round()))
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
            let mut measure = |value: &str| {
                widget::text::layout::<iced::Renderer>(
                    &mut state.paragraph,
                    renderer,
                    &unlimited,
                    value,
                    format,
                )
                .size()
                .width
            };
            let fitted = fit(&self.value, width, &mut measure);
            // The last binary-search probe need not be the final fitted string.
            measure(&fitted);
            state.key = Some(key);
        }
        layout::Node::new(limits.resolve(Length::Fill, (self.size * 1.3).round(), Size::ZERO))
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

fn fit(value: &str, width: f32, mut measure: impl FnMut(&str) -> f32) -> String {
    if measure(value) <= width {
        return value.into();
    }
    if measure("…") > width {
        return String::new();
    }
    let boundaries: Vec<_> = value
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .collect();
    let mut low = 0;
    let mut high = boundaries.len() - 1;
    while low < high {
        let mid = (low + high).div_ceil(2);
        let candidate = format!("{}…", &value[..boundaries[mid]]);
        if measure(&candidate) <= width {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    format!("{}…", &value[..boundaries[low]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_using_glyph_widths_and_preserves_unicode_boundaries() {
        // Deliberately unequal widths distinguish measurement from character limits.
        let measure = |s: &str| {
            s.chars()
                .map(|ch| match ch {
                    'W' | '界' => 12.,
                    '…' => 8.,
                    _ => 4.,
                })
                .sum()
        };
        assert_eq!(fit("Wiii", 24., measure), "Wiii");
        assert_eq!(fit("Wiii", 20., measure), "W…");
        assert_eq!(fit("ii界λtail", 28., measure), "ii界…");
        assert_eq!(fit("界long", 8., measure), "…");
        assert_eq!(fit("界long", 7., measure), "");
        assert_eq!(fit("", 0., measure), "");
        assert_eq!(fit("界", 8., measure), "…");

        // Neither a combining accent nor a joined emoji may be partially retained.
        let measure = |s: &str| s.chars().count() as f32;
        assert_eq!(fit("ae\u{301}long", 3., measure), "a…");
        assert_eq!(fit("a👩‍💻long", 4., measure), "a…");
    }
}
