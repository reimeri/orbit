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
use std::{ops::Range, sync::Arc};
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
    name: String,
    raw: bool,
    headings: Vec<Range<usize>>,
    input: String,
    output: String,
    display: String,
    units: Arc<chat_selection::TextUnits>,
    rows: Vec<Range<usize>>,
    columns: usize,
    offset: f32,
}
/// One logical selection document, with semantic heading ranges independent of wrapping.
/// This same projection is used by clipboard reconstruction after viewport eviction.
fn document(name: &str, input: &str, output: &str, raw: bool) -> (String, Vec<Range<usize>>) {
    let mut text = String::new();
    let mut headings = Vec::new();
    let mut section = |title: &str, body: &str| {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        let start = text.len();
        text.push_str(title);
        headings.push(start..text.len());
        text.push('\n');
        text.push_str(body);
    };
    // Keep raw first when disclosed, so even a huge command cannot hide it.
    if raw {
        section("Raw input", input);
    }
    let parsed = serde_json::from_str::<serde_json::Value>(input).ok();
    if name == "exec_command"
        && let Some(serde_json::Value::Object(args)) = &parsed
        && let Some(serde_json::Value::String(command)) = args.get("cmd")
    {
        section("Command", command);
        let mut arguments = String::new();
        for (key, value) in args.iter().filter(|(key, _)| key.as_str() != "cmd") {
            if !arguments.is_empty() {
                arguments.push('\n');
            }
            arguments.push_str(key);
            arguments.push_str(": ");
            match value {
                serde_json::Value::String(value) => arguments.push_str(value),
                value => {
                    arguments.push_str(&serde_json::to_string_pretty(value).expect("JSON value"))
                }
            }
        }
        if !arguments.is_empty() {
            section("Arguments", &arguments);
        }
    } else {
        let formatted = parsed
            .as_ref()
            .map(|value| serde_json::to_string_pretty(value).expect("JSON value"));
        section("Input", formatted.as_deref().unwrap_or(input));
    }
    section(
        "Output",
        if output.is_empty() {
            "Waiting for output…"
        } else {
            output
        },
    );
    (text, headings)
}

pub fn display(name: &str, input: &str, output: &str, raw: bool) -> String {
    document(name, input, output, raw).0
}

impl Output {
    pub fn new(name: &str, input: &str, output: &str, raw: bool) -> Self {
        let mut view = Self {
            name: name.into(),
            raw,
            headings: vec![],
            input: input.into(),
            output: output.into(),
            display: String::new(),
            units: Arc::new(chat_selection::TextUnits::lines("")),
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
    pub fn set_raw(&mut self, raw: bool) {
        self.raw = raw;
        self.offset = 0.;
        self.rebuild();
    }
    fn rebuild(&mut self) {
        (self.display, self.headings) = document(&self.name, &self.input, &self.output, self.raw);
        self.units = Arc::new(chat_selection::TextUnits::lines(&self.display));
        self.reflow();
    }
    fn reflow(&mut self) {
        self.rows.clear();
        let mut start = 0;
        let mut width = 0;
        let mut chars = self.display.char_indices().peekable();
        while let Some((offset, ch)) = chars.next() {
            // Paragraph separators must not enter a shaped row: Cosmic Text strips
            // them, which would desynchronize selection offsets. Keep their exact
            // bytes between row ranges in the logical clipboard document instead.
            if matches!(
                ch,
                '\n' | '\r' | '\u{1c}'..='\u{1e}' | '\u{85}' | '\u{2029}'
            ) {
                self.rows.push(start..offset);
                start = offset + ch.len_utf8();
                if ch == '\r' && chars.peek().is_some_and(|(_, ch)| *ch == '\n') {
                    chars.next();
                    start += 1;
                }
                width = 0;
                continue;
            }
            // Cosmic text uses eight-cell tab stops; budget the widest advance.
            let cells = if ch == '\t' {
                8
            } else {
                ch.width().unwrap_or(0)
            };
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
                let columns = ((width - crate::scrollbar::GUTTER - 4.).max(8.) / 8.) as usize;
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
                    self.headings
                        .iter()
                        .any(|heading| heading.contains(&range.start)),
                    self.units.clone(),
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
    fn details_decode_commands_format_other_tools_and_preserve_raw_bytes() {
        let input = r#"{ "cmd": "python3 - <<'PY'\n\tprint(\"λ\")\nPY", "workdir": "/tmp/a b", "max_output_tokens": 1500 }"#;
        let output = "  indented\tresult λ\r\nOutput\n";
        let formatted = "Command\npython3 - <<'PY'\n\tprint(\"λ\")\nPY\n\nArguments\nmax_output_tokens: 1500\nworkdir: /tmp/a b\n\nOutput\n  indented\tresult λ\r\nOutput\n";
        assert_eq!(display("exec_command", input, output, false), formatted);
        assert_eq!(
            display("exec_command", input, output, true),
            format!("Raw input\n{input}\n\n{formatted}"),
        );
        assert_eq!(
            display(
                "other_tool",
                r#"{"query":"λ","options":{"limit":3}}"#,
                "done",
                false
            ),
            "Input\n{\n  \"options\": {\n    \"limit\": 3\n  },\n  \"query\": \"λ\"\n}\n\nOutput\ndone",
        );
        // Streaming/incomplete JSON is readable as-is, never dropped or replaced.
        assert_eq!(
            display("exec_command", "{\"cmd\":", "", false),
            "Input\n{\"cmd\":\n\nOutput\nWaiting for output…"
        );
    }

    #[test]
    fn scrolling_reuses_overlapping_paragraphs_and_reflows_changed_rows() {
        use crate::chat_selection::tests::shaped_paragraphs;
        use iced::{
            Size,
            advanced::{Renderer as _, widget::Operation},
        };
        use iced_runtime::{UserInterface, user_interface::Cache};

        #[derive(Default)]
        struct Texts(Vec<(String, iced::Rectangle)>);
        impl Operation for Texts {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
                visit(self);
            }
            fn text(
                &mut self,
                _: Option<&iced::advanced::widget::Id>,
                bounds: iced::Rectangle,
                text: &str,
            ) {
                self.0.push((text.to_owned(), bounds));
            }
        }
        let source: String = (0..200)
            .map(|i| format!("row {i:03} with distinct Unicode λ界e\u{301} and a long suffix\n"))
            .collect();
        let mut output = Output::new("test", "cmd", &source, false);
        output.update(Event::Resize(700.));
        let selection = chat_selection::Shared::default();
        let mut renderer = crate::test_renderer::new(16.);
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
                texts
                    .0
                    .iter()
                    .map(|(text, _)| text.as_str())
                    .collect::<Vec<_>>(),
                expected,
                "retained widget state must show current source rows"
            );
            ui.operate(
                &renderer,
                &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                    iced::advanced::widget::Id::new("tool-output-tool"),
                    iced::widget::scrollable::AbsoluteOffset {
                        x: None,
                        y: Some(output.offset),
                    },
                ),
            );
            renderer.reset(iced::Rectangle::with_size(Size::new(width, MAX_HEIGHT)));
            ui.draw(
                &mut renderer,
                &crate::theme::app(),
                &iced::advanced::renderer::Style {
                    text_color: crate::theme::default_colors().text,
                },
                iced::mouse::Cursor::Unavailable,
            );
            let shaped = shaped_paragraphs() - before;
            let text_region = texts
                .0
                .iter()
                .zip(&output.rows[output.visible()])
                .filter(|((text, _), range)| {
                    !text.trim().is_empty()
                        && !output
                            .headings
                            .iter()
                            .any(|heading| heading.contains(&range.start))
                })
                .find_map(|((_, bounds), _)| {
                    let visible = (*bounds - iced::Vector::new(0., output.offset))
                        .intersection(&iced::Rectangle::with_size(Size::new(width, MAX_HEIGHT)))?;
                    (visible.height == bounds.height).then_some(visible)
                })
                .expect("a complete source row must be visible");
            crate::test_renderer::assert_matches_fresh(
                &mut renderer,
                output.view("tool", selection.clone(), 0),
                Size::new(width, MAX_HEIGHT),
                "tool-output-tool",
                output.offset,
                text_region,
            );
            (ui.into_cache(), shaped)
        };
        let (mut cache, count) = build(&output, Cache::new(), 700.);
        assert!(count <= output.visible().len());
        let (next, count) = build(&output, cache, 700.);
        cache = next;
        assert_eq!(count, 0);
        // Same-sized windows are the case iced's keyed column does not reconcile.
        // Movement may shape entering rows, but must reuse unchanged survivors.
        for row in [1, 2, 3, 2, 1, 0] {
            output.update(Event::Scroll(row as f32 * LINE_HEIGHT));
            let (next, count) = build(&output, cache, 700.);
            cache = next;
            assert!(count <= 1, "at most the entering row should be shaped");
        }
        output.set("cmd", &source.replacen("row 000", "ROW 000", 1));
        let (next, count) = build(&output, cache, 700.);
        cache = next;
        assert!(count <= 1, "changed spans must not reshape unrelated rows");
        output.update(Event::Resize(180.));
        let (next, count) = build(&output, cache, 180.);
        assert!(
            count <= output.visible().len(),
            "wrapping must update visible text without shaping the whole output"
        );
        cache = next;
        output.update(Event::Scroll(1000.));
        let (next, count) = build(&output, cache, 180.);
        cache = next;
        assert!(count <= output.visible().len());
        output.set("cmd", "shortened 界");
        let (_, count) = build(&output, cache, 180.);
        assert!(count <= output.visible().len());
        assert_eq!(output.offset, 0., "shorter output clamps the visible slice");
    }

    #[test]
    fn huge_single_line_and_unicode_keep_all_text_but_bound_layout_work() {
        let source = format!("{}\n\nlast λ界e\u{301}", "x".repeat(1_000_000));
        let mut output = Output::new("test", "cmd", &source, false);
        output.update(Event::Resize(360.));
        assert!(output.visible().len() as f32 * LINE_HEIGHT <= MAX_HEIGHT + 3. * LINE_HEIGHT);
        assert!(output.visible().len() < output.rows.len());
        let expected = format!("Input\ncmd\n\nOutput\n{source}");
        assert_eq!(output.display, expected);
        let mounted_bytes: usize = output.rows[output.visible()].iter().map(Range::len).sum();
        assert!(mounted_bytes < 4096);
        let rebuilt = output
            .rows
            .iter()
            .map(|range| &output.display[range.clone()])
            .collect::<String>();
        assert_eq!(rebuilt, expected.replace('\n', ""));
        output.update(Event::Scroll(
            output.rows.len() as f32 * LINE_HEIGHT - MAX_HEIGHT,
        ));
        assert_eq!(output.visible().end, output.rows.len());
        assert!(output.display[output.rows.last().unwrap().clone()].contains("last λ界e\u{301}"));
    }
}
