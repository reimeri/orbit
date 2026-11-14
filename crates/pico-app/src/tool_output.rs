//! Fixed-height visual rows: only the visible slice is handed to text shaping.
//! The complete source remains available to scrolling and selection-copy.
use crate::scrollbar::scroll;
use crate::{
    chat_selection::{self, Position},
    ui::Message,
};
use iced::{
    Element,
    widget::{sensor, space},
};
use std::ops::Range;
use unicode_width::UnicodeWidthChar;

const LINE_HEIGHT: f32 = 17.;
const MAX_HEIGHT: f32 = 340.;
#[derive(Clone, Debug)]
pub enum Event {
    Resize(f32),
    Scroll(f32),
}
#[derive(Clone, PartialEq, Eq, Hash)]
enum Row {
    Top,
    Text(usize),
    Bottom,
}

pub struct Output {
    input: String,
    output: String,
    display: String,
    rows: Vec<Range<usize>>,
    columns: usize,
    offset: f32,
}
pub fn display(input: &str, output: &str) -> String {
    format!(
        "Input\n{}\n\nOutput\n{}",
        input,
        if output.is_empty() {
            "Waiting for output…"
        } else {
            output
        }
    )
    .replace('\t', "    ")
    .replace("\r\n", "\n")
}
impl Output {
    pub fn new(input: &str, output: &str) -> Self {
        let mut view = Self {
            input: input.into(),
            output: output.into(),
            display: String::new(),
            rows: vec![],
            columns: 80,
            offset: 0.,
        };
        view.rebuild();
        view
    }
    pub fn set(&mut self, input: &str, output: &str) {
        if self.input != input || self.output != output {
            self.input = input.into();
            self.output = output.into();
            self.rebuild();
        }
    }
    fn rebuild(&mut self) {
        self.display = display(&self.input, &self.output);
        self.reflow();
    }
    fn reflow(&mut self) {
        self.rows.clear();
        let mut start = 0;
        let mut width = 0;
        for (offset, ch) in self.display.char_indices() {
            if ch == '\n' {
                self.rows.push(start..offset);
                start = offset + 1;
                width = 0;
                continue;
            }
            let cells = ch.width().unwrap_or(0);
            if width + cells > self.columns && offset > start {
                self.rows.push(start..offset);
                start = offset;
                width = 0;
            }
            width += cells;
        }
        self.rows.push(start..self.display.len());
        self.offset = self
            .offset
            .min((self.rows.len() as f32 * LINE_HEIGHT - MAX_HEIGHT).max(0.));
    }
    pub fn update(&mut self, event: Event) {
        match event {
            Event::Resize(width) => {
                // Conservative cell width for the 12px monospace font, including wide Unicode glyphs.
                let columns = ((width - 18.).max(8.) / 8.) as usize;
                if columns != self.columns {
                    self.columns = columns;
                    self.reflow();
                }
            }
            Event::Scroll(offset) => self.offset = offset,
        }
    }
    fn visible(&self) -> Range<usize> {
        let start = (self.offset / LINE_HEIGHT).floor() as usize;
        start.min(self.rows.len())
            ..(start + (MAX_HEIGHT / LINE_HEIGHT).ceil() as usize + 2).min(self.rows.len())
    }
    pub fn view(
        &self,
        id: &str,
        selection: chat_selection::Shared,
        entry: usize,
    ) -> Element<'_, Message> {
        let visible = self.visible();
        let mut rows: Vec<(Row, Element<'_, Message>)> = vec![(
            Row::Top,
            space().height(visible.start as f32 * LINE_HEIGHT).into(),
        )];
        for range in &self.rows[visible.clone()] {
            // Byte starts survive scrolling. Changed content/wrap bounds are
            // compared by the text widget, not hidden behind a generation key.
            rows.push((
                Row::Text(range.start),
                chat_selection::tool_row(
                    &self.display[range.clone()],
                    selection.clone(),
                    Position {
                        entry,
                        block: 0,
                        byte: range.start,
                    },
                    LINE_HEIGHT,
                ),
            ));
        }
        rows.push((
            Row::Bottom,
            space()
                .height((self.rows.len() - visible.end) as f32 * LINE_HEIGHT)
                .into(),
        ));
        let scroll_id = id.to_owned();
        let resize_id = id.to_owned();
        sensor(
            scroll(crate::keyed_column::keyed_column(rows))
                .id(format!("tool-output-{id}"))
                .height((self.rows.len() as f32 * LINE_HEIGHT).min(MAX_HEIGHT))
                .on_scroll(move |v| {
                    Message::ToolOutput(scroll_id.clone(), Event::Scroll(v.absolute_offset().y))
                }),
        )
        .on_resize(move |size| Message::ToolOutput(resize_id.clone(), Event::Resize(size.width)))
        .into()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_reuses_overlapping_paragraphs_and_reflows_changed_rows() {
        use crate::chat_selection::tests::shaped_paragraphs;
        use iced::{Font, Pixels, Size, advanced::widget::Operation};
        use iced_runtime::{UserInterface, user_interface::Cache};

        #[derive(Default)]
        struct Texts(Vec<String>);
        impl Operation for Texts {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
                visit(self);
            }
            fn text(
                &mut self,
                _: Option<&iced::advanced::widget::Id>,
                _: iced::Rectangle,
                text: &str,
            ) {
                self.0.push(text.to_owned());
            }
        }
        let source: String = (0..200)
            .map(|i| format!("row {i:03} with distinct Unicode λ界e\u{301} and a long suffix\n"))
            .collect();
        let mut output = Output::new("cmd", &source);
        output.update(Event::Resize(700.));
        let selection = chat_selection::Shared::default();
        let mut renderer =
            iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(Font::DEFAULT, Pixels(16.)));
        let mut build = |output: &Output, cache, width| {
            let before = shaped_paragraphs();
            let mut ui = UserInterface::build(
                output.view("tool", selection.clone(), 0),
                Size::new(width, MAX_HEIGHT),
                cache,
                &mut renderer,
            );
            let mut texts = Texts::default();
            ui.operate(&renderer, &mut texts);
            let expected: Vec<_> = output.rows[output.visible()]
                .iter()
                .map(|range| &output.display[range.clone()])
                .collect();
            assert_eq!(
                texts.0, expected,
                "retained widget state must show current source rows"
            );
            (ui.into_cache(), shaped_paragraphs() - before)
        };
        let (mut cache, count) = build(&output, Cache::new(), 700.);
        assert_eq!(count, 22);
        let (next, count) = build(&output, cache, 700.);
        cache = next;
        assert_eq!(count, 0);
        // Same-sized windows are the case iced's keyed column does not reconcile.
        // Reverse movement must recreate only the row that was actually evicted.
        for row in [1, 2, 3, 2, 1, 0] {
            output.update(Event::Scroll(row as f32 * LINE_HEIGHT));
            let (next, count) = build(&output, cache, 700.);
            cache = next;
            assert_eq!(count, 1, "only the entering row should be shaped");
        }
        output.set("cmd", &source.replacen("row 000", "ROW 000", 1));
        let (next, count) = build(&output, cache, 700.);
        cache = next;
        assert_eq!(count, 1, "a retained row must invalidate changed spans");
        output.update(Event::Resize(180.));
        let (next, count) = build(&output, cache, 180.);
        assert!(
            (1..=22).contains(&count),
            "wrapping must update visible text without shaping the whole output"
        );
        cache = next;
        output.update(Event::Scroll(1000.));
        let (next, count) = build(&output, cache, 180.);
        cache = next;
        assert!(count <= 22);
        output.set("cmd", "shortened 界");
        let (_, count) = build(&output, cache, 180.);
        assert!(count <= output.visible().len());
        assert_eq!(output.offset, 0., "shorter output clamps the visible slice");
    }

    #[test]
    fn huge_single_line_and_unicode_keep_all_text_but_bound_layout_work() {
        let source = format!("{}\n\nlast λ界e\u{301}", "x".repeat(1_000_000));
        let mut output = Output::new("cmd", &source);
        output.update(Event::Resize(360.));
        assert!(output.visible().len() <= 22);
        assert!(output.rows.iter().all(|range| range.len() < 200));
        let rebuilt = output
            .rows
            .iter()
            .map(|range| &output.display[range.clone()])
            .collect::<String>();
        assert_eq!(rebuilt, output.display.replace('\n', ""));
        output.update(Event::Scroll(
            output.rows.len() as f32 * LINE_HEIGHT - MAX_HEIGHT,
        ));
        assert_eq!(output.visible().end, output.rows.len());
        assert!(output.display[output.rows.last().unwrap().clone()].contains("last λ界e\u{301}"));
    }
}
