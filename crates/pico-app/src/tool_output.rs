//! Fixed-height visual rows: only the visible slice is handed to text shaping.
//! The complete source remains available to scrolling and the chat's Copy action.
use crate::{
    chat_selection::{self, Position},
    ui::Message,
};
use iced::{
    Element, Fill,
    widget::{column, scrollable, sensor, space},
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
        let mut rows = column![space().height(visible.start as f32 * LINE_HEIGHT)]
            .spacing(0)
            .width(Fill);
        for range in &self.rows[visible.clone()] {
            rows = rows.push(chat_selection::tool_row(
                &self.display[range.clone()],
                selection.clone(),
                Position {
                    entry,
                    block: 0,
                    byte: range.start,
                },
                LINE_HEIGHT,
            ));
        }
        rows = rows.push(space().height((self.rows.len() - visible.end) as f32 * LINE_HEIGHT));
        let scroll_id = id.to_owned();
        let resize_id = id.to_owned();
        sensor(
            scrollable(rows)
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
