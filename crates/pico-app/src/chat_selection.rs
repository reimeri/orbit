//! Logical transcript positions survive virtualized widgets being unmounted.
//! Only mounted text is shaped; copy reconstructs the selected entries on demand.
use crate::scrollbar::horizontal;
use crate::ui::Message;
use iced::{
    Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Renderer as _, Shell, Widget,
        graphics::text::cosmic_text,
        layout, mouse, renderer,
        text::{self, Paragraph as _, Span},
        widget::{Operation, Tree, tree},
    },
    widget::{column, container, markdown},
};
use std::{
    cell::{Cell, RefCell},
    ops::Range,
    rc::Rc,
    sync::Arc,
};
use unicode_segmentation::UnicodeSegmentation;

type Paragraph = <iced::Renderer as text::Renderer>::Paragraph;
pub type Shared = Rc<RefCell<Selection>>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Position {
    pub entry: usize,
    pub block: usize,
    pub byte: usize,
}

#[derive(Default)]
pub struct Selection {
    pub anchor: Option<Position>,
    pub focus: Option<Position>,
    pub dragging: bool,
    moved: bool,
    candidate: Option<MotionCandidate>,
    last_click: Option<(mouse::Click, Position)>,
    granularity: Granularity,
    origin: Option<(Position, Position)>,
    pressed_at: Option<Position>,
}
struct MotionCandidate {
    at: Position,
    unit: (Position, Position),
    distance: f32,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Granularity {
    #[default]
    Character,
    Word,
    Paragraph,
}

/// Logical boundaries, independent of visual wrapping. Tool rows share one index
/// for the complete document, so even unmounted portions of a unit are selected.
#[derive(Default)]
pub struct TextUnits {
    words: Vec<Range<usize>>,
    paragraphs: Vec<Range<usize>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Paragraphs {
    Block,
    BlankLines,
    Lines,
}

impl TextUnits {
    fn new(text: &str, offset: usize, paragraphs: Paragraphs) -> Self {
        #[cfg(test)]
        tests::UNITS.with(|count| count.set(count.get() + 1));
        let words = text
            .split_word_bound_indices()
            .map(|(start, word)| offset + start..offset + start + word.len())
            .collect();
        let mut lines = Vec::new();
        let mut start = 0;
        let mut chars = text.char_indices().peekable();
        while let Some((byte, ch)) = chars.next() {
            if matches!(
                ch,
                '\n' | '\r' | '\u{1c}'..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
            ) {
                lines.push(start..byte);
                start = byte + ch.len_utf8();
                if ch == '\r' && chars.peek().is_some_and(|(_, ch)| *ch == '\n') {
                    chars.next();
                    start += 1;
                }
            }
        }
        lines.push(start..text.len());
        let paragraphs = match paragraphs {
            Paragraphs::Block => std::iter::once(0..text.len()).collect(),
            Paragraphs::Lines => lines,
            Paragraphs::BlankLines => {
                let mut result: Vec<Range<usize>> = Vec::new();
                let mut previous_blank = true;
                for line in lines {
                    let blank = text[line.clone()].trim().is_empty();
                    if !blank && !previous_blank {
                        result.last_mut().expect("preceding paragraph").end = line.end;
                    } else {
                        result.push(line);
                    }
                    previous_blank = blank;
                }
                result
            }
        }
        .into_iter()
        .map(|range| offset + range.start..offset + range.end)
        .collect();
        Self { words, paragraphs }
    }

    pub fn lines(text: &str) -> Self {
        Self::new(text, 0, Paragraphs::Lines)
    }

    fn range(&self, byte: usize, granularity: Granularity) -> Range<usize> {
        let ranges = match granularity {
            Granularity::Character => return byte..byte,
            Granularity::Word => &self.words,
            Granularity::Paragraph => &self.paragraphs,
        };
        // A caret at the end of a line belongs to that line, not the next one.
        let index = ranges.partition_point(|range| range.start <= byte);
        index
            .checked_sub(1)
            .map_or(byte..byte, |i| ranges[i].clone())
    }
}

impl Selection {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    /// A new press clears the highlight, but only a text hit may continue the
    /// previous click sequence. Real drags break the sequence as well.
    pub fn begin_press(&mut self) {
        let previous = self.last_click;
        self.clear();
        self.last_click = previous;
    }
    pub fn finish_press(&mut self) {
        if !self.dragging {
            self.clear();
        }
    }
    pub fn range(&self) -> Option<(Position, Position)> {
        let (a, b) = (self.anchor?, self.focus?);
        (a != b).then_some((a.min(b), a.max(b)))
    }
    pub fn invalidate_entry(&mut self, entry: usize) {
        if let (Some(a), Some(b)) = (self.anchor, self.focus)
            && (a.entry.min(b.entry)..=a.entry.max(b.entry)).contains(&entry)
        {
            self.clear();
        }
    }
    fn start(&mut self, at: Position) {
        self.anchor = Some(at);
        self.focus = Some(at);
        self.dragging = true;
        self.moved = false;
        self.pressed_at = Some(at);
        self.granularity = Granularity::Character;
        self.origin = None;
    }
    fn press(&mut self, at: Position, unit_byte: usize, point: Point, units: &TextUnits) {
        let previous = self
            .last_click
            .filter(|(_, previous)| previous.entry == at.entry && previous.block == at.block);
        let click = mouse::Click::new(point, mouse::Button::Left, previous.map(|(click, _)| click));
        self.start(at);
        self.last_click = Some((click, at));
        self.granularity = match click.kind() {
            mouse::click::Kind::Single => Granularity::Character,
            mouse::click::Kind::Double => Granularity::Word,
            mouse::click::Kind::Triple => Granularity::Paragraph,
        };
        if self.granularity != Granularity::Character {
            let range = units.range(unit_byte, self.granularity);
            let start = Position {
                byte: range.start,
                ..at
            };
            let end = Position {
                byte: range.end,
                ..at
            };
            self.anchor = Some(start);
            self.focus = Some(end);
            self.origin = Some((start, end));
        }
    }
    pub fn begin_motion(&mut self) {
        self.candidate = None;
    }
    pub fn finish_motion(&mut self) -> bool {
        if let Some(MotionCandidate {
            at,
            unit: (start, end),
            ..
        }) = self.candidate.take()
        {
            let previous = (self.anchor, self.focus);
            self.extend(at);
            if let Some((origin_start, origin_end)) = self.origin {
                if start < origin_start {
                    self.anchor = Some(origin_end);
                    self.focus = Some(start);
                } else {
                    self.anchor = Some(origin_start);
                    self.focus = Some(end.max(origin_end));
                }
            }
            let changed = previous != (self.anchor, self.focus);
            if changed {
                // Extending the selected range ends a click sequence. Caret jitter
                // inside the initial word/paragraph does not, though `moved` still
                // suppresses link activation just as it does for character drags.
                self.last_click = None;
            }
            changed
        } else {
            false
        }
    }
    fn extend(&mut self, at: Position) {
        self.focus = Some(at);
        self.moved |= self.pressed_at != Some(at);
    }
    fn local_range(&self, entry: usize, block: usize, bytes: Range<usize>) -> Range<usize> {
        let Some((a, b)) = self.range() else {
            return 0..0;
        };
        let start = Position {
            entry,
            block,
            byte: bytes.start,
        }
        .max(a);
        let end = Position {
            entry,
            block,
            byte: bytes.end,
        }
        .min(b);
        if start >= end {
            return 0..0;
        }
        start.byte - bytes.start..end.byte - bytes.start
    }
}

pub fn selection_motion(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(
            mouse::Event::CursorMoved { .. } | mouse::Event::ButtonReleased(mouse::Button::Left)
        ) | Event::Window(iced::window::Event::RedrawRequested(_))
    )
}

pub struct Block {
    pub text: String,
    pub separator: &'static str,
}

/// Extract using the very same viewer traversal as rendering (including table cells).
pub fn copy_blocks(
    selection: &Selection,
    entries: impl IntoIterator<Item = (usize, Vec<Block>)>,
) -> String {
    let Some((start, end)) = selection.range() else {
        return String::new();
    };
    let mut result = String::new();
    for (entry, blocks) in entries {
        for (block, content) in blocks.into_iter().enumerate() {
            let beginning = Position {
                entry,
                block,
                byte: 0,
            };
            if beginning > start && beginning <= end {
                result.push_str(content.separator);
            }
            let range = selection.local_range(entry, block, 0..content.text.len());
            result.push_str(&content.text[range]);
        }
    }
    result
}

pub struct Viewer<'a> {
    images: Option<&'a crate::markdown_images::Cache>,
    selection: Shared,
    entry: usize,
    next: Cell<usize>,
    pub blocks: RefCell<Vec<Block>>,
}
impl<'a> Viewer<'a> {
    pub fn new(selection: Shared, entry: usize) -> Self {
        Self {
            images: None,
            selection,
            entry,
            next: Cell::new(0),
            blocks: RefCell::new(Vec::new()),
        }
    }
    pub fn with_images(mut self, images: &'a crate::markdown_images::Cache) -> Self {
        self.images = Some(images);
        self
    }
    fn block(
        &self,
        spans: Arc<[Span<'static, String>]>,
        size: Pixels,
        font: Font,
        separator: &'static str,
    ) -> Element<'static, Message> {
        let block = self.next.get();
        self.next.set(block + 1);
        self.blocks.borrow_mut().push(Block {
            text: spans.iter().map(|span| span.text.as_ref()).collect(),
            separator,
        });
        selectable(
            spans,
            size,
            font,
            self.selection.clone(),
            Position {
                entry: self.entry,
                block,
                byte: 0,
            },
            SelectableText::Block,
        )
    }
}
impl<'a> markdown::Viewer<'a, Message> for Viewer<'a> {
    fn on_link_click(url: String) -> Message {
        Message::Link(url)
    }
    fn paragraph(
        &self,
        settings: markdown::Settings,
        text: &markdown::Text,
    ) -> Element<'a, Message> {
        self.block(
            text.spans(settings.style),
            settings.text_size,
            Font::DEFAULT,
            "\n\n",
        )
    }
    fn heading(
        &self,
        settings: markdown::Settings,
        level: &'a markdown::HeadingLevel,
        text: &'a markdown::Text,
        index: usize,
    ) -> Element<'a, Message> {
        use markdown::HeadingLevel::*;
        let size = match level {
            H1 => settings.h1_size,
            H2 => settings.h2_size,
            H3 => settings.h3_size,
            H4 => settings.h4_size,
            H5 => settings.h5_size,
            H6 => settings.h6_size,
        };
        container(self.block(text.spans(settings.style), size, Font::DEFAULT, "\n\n"))
            .padding(iced::Padding::default().top(if index > 0 {
                settings.text_size.0 / 2.
            } else {
                0.
            }))
            .into()
    }
    fn code_block(
        &self,
        settings: markdown::Settings,
        _language: Option<&'a str>,
        _code: &'a str,
        lines: &'a [markdown::Text],
    ) -> Element<'a, Message> {
        container(horizontal(
            container(column(lines.iter().enumerate().map(|(i, line)| {
                self.block(
                    line.spans(settings.style),
                    settings.code_size,
                    settings.style.code_block_font,
                    if i == 0 { "\n\n" } else { "\n" },
                )
            })))
            .padding(settings.code_size),
        ))
        .width(Length::Fill)
        .padding(settings.code_size / 4)
        .style(crate::theme::code_block)
        .into()
    }
    fn image(
        &self,
        settings: markdown::Settings,
        url: &'a String,
        title: &'a str,
        alt: &markdown::Text,
    ) -> Element<'a, Message> {
        // Keep the alt-text block in the same logical position for selection/copy,
        // regardless of whether the image is loading, failed, or ready.
        let caption = self.block(
            alt.spans(settings.style),
            settings.text_size,
            Font::DEFAULT,
            "\n\n",
        );
        let mut body = column![].spacing(4);
        if let Some(images) = self.images {
            body = body.push(images.view(url));
        }
        body = body.push(caption);
        let content = container(body).max_width(640).padding(settings.spacing.0);
        if title.is_empty() {
            content.into()
        } else {
            iced::widget::tooltip(
                content,
                iced::widget::text(title.to_owned()),
                iced::widget::tooltip::Position::FollowCursor,
            )
            .style(crate::theme::footer_surface)
            .into()
        }
    }
}

pub fn plain(
    value: &str,
    size: f32,
    color: Option<Color>,
    selection: Shared,
    at: Position,
) -> Element<'static, Message> {
    let mut span = Span::new(value.to_owned());
    span.color = color;
    selectable(
        Arc::from([span]),
        size.into(),
        Font::DEFAULT,
        selection,
        at,
        SelectableText::Plain,
    )
}

pub fn tool_row(
    value: &str,
    selection: Shared,
    at: Position,
    height: f32,
    heading: bool,
    units: Arc<TextUnits>,
) -> Element<'static, Message> {
    let mut span = Span::new(value.to_owned());
    span.color = heading.then_some(crate::theme::MUTED);
    selectable(
        Arc::from([span]),
        12.into(),
        if heading {
            Font {
                weight: iced::font::Weight::Semibold,
                ..Font::DEFAULT
            }
        } else {
            Font::MONOSPACE
        },
        selection,
        at,
        SelectableText::Row { height, units },
    )
}

enum SelectableText {
    Block,
    Plain,
    Row { height: f32, units: Arc<TextUnits> },
}

fn selectable(
    spans: Arc<[Span<'static, String>]>,
    size: Pixels,
    font: Font,
    selection: Shared,
    at: Position,
    kind: SelectableText,
) -> Element<'static, Message> {
    let row_height = match &kind {
        SelectableText::Row { height, .. } => Some(*height),
        SelectableText::Block | SelectableText::Plain => None,
    };
    let line_height = row_height
        .map(|h| text::LineHeight::Absolute(h.into()))
        .unwrap_or_default();
    let wrapping = if row_height.is_some() {
        text::Wrapping::None
    } else {
        text::Wrapping::default()
    };
    Element::new(Selectable {
        height: row_height.map_or(Length::Shrink, Length::Fixed),
        spans,
        kind,
        size,
        font,
        line_height,
        wrapping,
        selection,
        at,
    })
}

struct Selectable {
    height: Length,
    spans: Arc<[Span<'static, String>]>,
    kind: SelectableText,
    size: Pixels,
    font: Font,
    line_height: text::LineHeight,
    wrapping: text::Wrapping,
    selection: Shared,
    at: Position,
}
#[derive(Default)]
struct State {
    paragraph: Paragraph,
    spans: Arc<[Span<'static, String>]>,
    index: TextIndex,
    // Paragraph-local, unpadded bounds; translation and styling stay draw-time inputs.
    decorations: Vec<(usize, Vec<Rectangle>)>,
    content: String,
    units: Arc<TextUnits>,
    units_key: Option<(usize, Paragraphs)>,
    pressed_link: Option<String>,
}
/// Byte indexes belong to the shaped paragraph, not to a transient widget/view.
/// Build from the renderer's logical lines; visual wrapping does not change them.
#[derive(Default)]
struct TextIndex {
    graphemes: Vec<usize>,
    lines: Vec<Range<usize>>,
}
impl TextIndex {
    fn new(content: &str, buffer: &cosmic_text::Buffer) -> Self {
        let graphemes = content
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .chain([content.len()])
            .collect();
        let lines = buffer
            .lines
            .iter()
            .scan(0, |offset, line| {
                let start = *offset;
                let end = start + line.text().len();
                *offset = end + line.ending().as_str().len();
                Some(start..end)
            })
            .collect();
        Self { graphemes, lines }
    }
    fn nearest(&self, byte: usize) -> usize {
        let mut index = self
            .graphemes
            .partition_point(|&boundary| boundary < byte)
            .min(self.graphemes.len() - 1);
        // Match the original scan: the earlier boundary wins an equal-distance tie.
        if index > 0
            && byte.abs_diff(self.graphemes[index - 1]) <= byte.abs_diff(self.graphemes[index])
        {
            index -= 1;
        }
        index
    }
    fn cursor(&self, byte: usize) -> cosmic_text::Cursor {
        let line = self.lines.partition_point(|range| range.end < byte);
        cosmic_text::Cursor::new(line, byte - self.lines[line].start)
    }
}
struct Hit {
    byte: usize,
    unit_byte: usize,
}
impl Selectable {
    fn hit(&self, state: &State, point: Point) -> Option<Hit> {
        // Iced's generic hit_test drops the hard-line index. Use the renderer's
        // shaped buffer so hard breaks, wrapping, bidi and Unicode agree with paint.
        #[cfg(test)]
        tests::HITS.set(tests::HITS.get() + 1);
        let buffer = state.paragraph.buffer();
        let hit = buffer.hit(point.x, point.y)?;
        let offset = state.index.lines[hit.line].start;
        let boundary = state.index.nearest(offset + hit.index);
        let byte = state.index.graphemes[boundary];
        // The trailing half of a glyph hits the caret *after* it. Unit selection
        // must still select that glyph's word (also for RTL text and ligatures).
        let unit_byte = if hit.affinity == cosmic_text::Affinity::Before && byte > offset {
            state.index.graphemes[boundary - 1]
        } else {
            byte
        };
        Some(Hit { byte, unit_byte })
    }
    fn link(&self, state: &State, point: Point) -> Option<String> {
        state
            .paragraph
            .hit_span(point)
            .and_then(|i| self.spans[i].link.clone())
    }
}
impl Widget<Message, iced::Theme, iced::Renderer> for Selectable {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.text(
            None,
            layout.bounds(),
            &tree.state.downcast_ref::<State>().content,
        );
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, self.height)
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        // Match iced Rich's sizing, but own the only shaped paragraph. Paint,
        // hits, links and selection must all use the same bounds and glyphs.
        layout::sized(limits, Length::Shrink, self.height, |limits| {
            let state = tree.state.downcast_mut::<State>();
            let spans_replaced = !Arc::ptr_eq(&state.spans, &self.spans);
            let spans_changed = state.spans != self.spans;
            if spans_changed {
                let content: String = self.spans.iter().map(|span| span.text.as_ref()).collect();
                if content != state.content {
                    state.content = content;
                    state.units_key = None;
                }
            }
            // Selection boundaries follow logical text, not styling, wrapping or
            // viewport position. Retain them with the mounted paragraph instead
            // of segmenting unchanged text during every view reconstruction.
            match &self.kind {
                SelectableText::Block | SelectableText::Plain => {
                    let paragraphs = match self.kind {
                        SelectableText::Block => Paragraphs::Block,
                        _ => Paragraphs::BlankLines,
                    };
                    let key = (self.at.byte, paragraphs);
                    if state.units_key != Some(key) {
                        state.units =
                            Arc::new(TextUnits::new(&state.content, self.at.byte, paragraphs));
                        state.units_key = Some(key);
                    }
                }
                SelectableText::Row { units, .. } => {
                    // Tool rows share boundaries for the entire output; a word or
                    // logical line may extend beyond this mounted visual row.
                    state.units = units.clone();
                    state.units_key = None;
                }
            }
            let text = iced::advanced::Text {
                content: self.spans.as_ref(),
                bounds: limits.max(),
                size: self.size,
                font: self.font,
                line_height: self.line_height,
                align_x: text::Alignment::Default,
                align_y: iced::alignment::Vertical::Top,
                shaping: text::Shaping::Advanced,
                wrapping: self.wrapping,
            };
            let geometry_changed = if spans_changed
                || state.paragraph.compare(text.with_content(())) == text::Difference::Shape
            {
                state.paragraph = Paragraph::with_spans(text);
                state.index = TextIndex::new(&state.content, state.paragraph.buffer());
                #[cfg(test)]
                tests::SHAPED.with(|count| count.set(count.get() + 1));
                true
            } else if state.paragraph.bounds() != text.bounds {
                state.paragraph.resize(text.bounds);
                true
            } else {
                false
            };
            let decorated_spans = || {
                self.spans.iter().enumerate().filter_map(|(index, span)| {
                    (span.highlight.is_some() || span.underline || span.strikethrough)
                        .then_some(index)
                })
            };
            // Iced's Span equality compares shaping attributes, not decoration
            // flags. Changing which spans are decorated needs geometry even if
            // the shaped paragraph is unchanged. Paint-only changes to an already
            // decorated span (padding/colors/underline style) reuse the bounds.
            if geometry_changed
                || (spans_replaced
                    && !decorated_spans().eq(state.decorations.iter().map(|(index, _)| *index)))
            {
                // span_bounds scans glyphs from the start for each span. Pay that
                // cost only when its inputs change, never on scroll-only redraws.
                state.decorations = decorated_spans()
                    .map(|index| {
                        #[cfg(test)]
                        tests::DECORATION_QUERIES.with(|count| count.set(count.get() + 1));
                        (index, state.paragraph.span_bounds(index))
                    })
                    .collect();
            }
            if spans_replaced {
                state.spans = self.spans.clone();
            }
            state.paragraph.min_bounds()
        })
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();
        let position = cursor
            .position()
            .filter(|p| viewport.contains(*p) && bounds.contains(*p))
            .map(|p| p - bounds.position());
        // Re-hit after a wheel scroll/reflow even if the pointer is stationary,
        // and once more on release before the root ends the drag.
        if self.selection.borrow().dragging
            && selection_motion(event)
            && let Some(p) = cursor.position().filter(|p| viewport.contains(*p))
        {
            // Nearest mounted text wins, including whitespace between
            // paragraphs and the empty area after a short line.
            let x = p.x.clamp(bounds.x, bounds.x + bounds.width);
            let y = p.y.clamp(bounds.y, bounds.y + bounds.height);
            let distance = (p.x - x).powi(2) + (p.y - y).powi(2);
            let mut selection = self.selection.borrow_mut();
            // Equal-distance candidates cannot replace the first winner either.
            // Reject before consulting the shaped text, including offscreen blocks.
            if selection
                .candidate
                .as_ref()
                .is_none_or(|best| distance < best.distance)
                && let Some(hit) = self.hit(state, Point::new(x - bounds.x, y - bounds.y))
            {
                let at = Position {
                    byte: self.at.byte + hit.byte,
                    ..self.at
                };
                let range = state
                    .units
                    .range(self.at.byte + hit.unit_byte, selection.granularity);
                selection.candidate = Some(MotionCandidate {
                    at,
                    unit: (
                        Position {
                            byte: range.start,
                            ..at
                        },
                        Position {
                            byte: range.end,
                            ..at
                        },
                    ),
                    distance,
                });
            }
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                state.pressed_link = None;
                if let Some(point) = position
                    && let Some(hit) = self.hit(state, Point::new(point.x, point.y))
                {
                    self.selection.borrow_mut().press(
                        Position {
                            byte: self.at.byte + hit.byte,
                            ..self.at
                        },
                        self.at.byte + hit.unit_byte,
                        cursor.position().expect("hit has a cursor"),
                        &state.units,
                    );
                    state.pressed_link = self.link(state, Point::new(point.x, point.y));
                    shell.capture_event();
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some(link) = state.pressed_link.take()
                    && !self.selection.borrow().moved
                    && position.is_some_and(|p| {
                        let point = Point::new(p.x, p.y);
                        self.link(state, point).as_ref() == Some(&link)
                            && self.hit(state, point).is_some_and(|hit| {
                                self.selection.borrow().pressed_at
                                    == Some(Position {
                                        byte: self.at.byte + hit.byte,
                                        ..self.at
                                    })
                            })
                    })
                {
                    shell.publish(Message::Link(link));
                }
            }
            _ => {}
        }
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
        let Some(clip) = layout.bounds().intersection(viewport) else {
            return;
        };
        let state = tree.state.downcast_ref::<State>();
        let paragraph = &state.paragraph;
        // Preserve Rich's span decorations (not just the glyphs). Default/top
        // alignment anchors both the paragraph and these regions at layout.position().
        let translation = layout.position() - Point::ORIGIN;
        for (index, regions) in &state.decorations {
            let span = &self.spans[*index];
            if let Some(highlight) = span.highlight {
                for bounds in regions {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: bounds.expand(span.padding) + translation,
                            border: highlight.border,
                            ..Default::default()
                        },
                        highlight.background,
                    );
                }
            }
            if span.underline || span.strikethrough {
                let size = span.size.unwrap_or(self.size);
                let line_height = span
                    .line_height
                    .unwrap_or(self.line_height)
                    .to_absolute(size);
                let color = span.color.unwrap_or(style.text_color);
                let baseline =
                    translation + Vector::new(0., size.0 + (line_height.0 - size.0) / 2.);
                for bounds in regions {
                    if span.underline {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle::new(
                                    bounds.position() + baseline - Vector::new(0., size.0 * 0.08),
                                    Size::new(bounds.width, 1.),
                                ),
                                ..Default::default()
                            },
                            color,
                        );
                    }
                    if span.strikethrough {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle::new(
                                    bounds.position() + baseline - Vector::new(0., size.0 / 2.),
                                    Size::new(bounds.width, 1.),
                                ),
                                ..Default::default()
                            },
                            color,
                        );
                    }
                }
            }
        }
        iced::widget::text::draw(
            renderer,
            style,
            layout.bounds(),
            paragraph,
            iced::widget::text::Style::default(),
            viewport,
        );
        let range = self.selection.borrow().local_range(
            self.at.entry,
            self.at.block,
            self.at.byte..self.at.byte + state.content.len(),
        );
        if range.is_empty() {
            return;
        }
        let buffer = paragraph.buffer();
        let (start, end) = (
            state.index.cursor(range.start),
            state.index.cursor(range.end),
        );
        renderer.with_layer(clip, |renderer| {
            for run in buffer.layout_runs() {
                if let Some((x, width)) = run.highlight(start, end) {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(
                                layout.position() + iced::Vector::new(x, run.line_top),
                                Size::new(width, run.line_height),
                            ),
                            ..Default::default()
                        },
                        crate::theme::SELECTION,
                    );
                }
            }
        });
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if let Some(p) = cursor
            .position()
            .filter(|p| viewport.contains(*p) && layout.bounds().contains(*p))
        {
            let point = p - layout.position();
            if self
                .link(
                    tree.state.downcast_ref::<State>(),
                    Point::new(point.x, point.y),
                )
                .is_some()
            {
                mouse::Interaction::Pointer
            } else {
                mouse::Interaction::Text
            }
        } else {
            mouse::Interaction::None
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    thread_local! {
        pub(super) static HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        pub(super) static DECORATION_QUERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        pub(super) static UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        pub(super) static SHAPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    pub(crate) fn shaped_paragraphs() -> usize {
        SHAPED.with(std::cell::Cell::get)
    }

    use super::*;
    use crate::pointer_input::pointer_input;
    use iced::{
        advanced::clipboard,
        keyboard,
        widget::{column, scrollable, text_input},
    };
    use iced_runtime::{UserInterface, user_interface};

    fn renderer() -> iced::Renderer {
        crate::test_renderer::new(16.)
    }
    fn send(
        ui: &mut UserInterface<'_, Message, iced::Theme, iced::Renderer>,
        renderer: &mut iced::Renderer,
        event: Event,
        at: Point,
    ) -> Vec<Message> {
        let mut messages = vec![];
        ui.update(
            &[event],
            mouse::Cursor::Available(at),
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        messages
    }
    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }
    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }
    fn motion(at: Point) -> Event {
        Event::Mouse(mouse::Event::CursorMoved { position: at })
    }
    fn key(key: keyboard::Key, code: keyboard::key::Code) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: keyboard::key::Physical::Code(code),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::CTRL,
            text: None,
            repeat: false,
        })
    }

    #[derive(Default)]
    struct TextBounds(Vec<(String, Rectangle)>);
    impl Operation for TextBounds {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(
            &mut self,
            _id: Option<&iced::advanced::widget::Id>,
            bounds: Rectangle,
            text: &str,
        ) {
            self.0.push((text.to_owned(), bounds));
        }
    }

    fn clicks(
        ui: &mut UserInterface<'_, Message, iced::Theme, iced::Renderer>,
        renderer: &mut iced::Renderer,
        at: Point,
        count: usize,
        hold: bool,
    ) {
        let events: Vec<_> = (0..count).flat_map(|_| [press(), release()]).collect();
        let mut messages = Vec::new();
        ui.update(
            &events[..events.len() - usize::from(hold)],
            mouse::Cursor::Available(at),
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
    }

    fn copied(selection: &Shared, viewer: &Viewer<'_>) -> String {
        copy_blocks(
            &selection.borrow(),
            [(
                viewer.entry,
                viewer
                    .blocks
                    .borrow()
                    .iter()
                    .map(|b| Block {
                        text: b.text.clone(),
                        separator: b.separator,
                    })
                    .collect(),
            )],
        )
    }

    #[test]
    fn indexed_boundaries_follow_unicode_and_hard_lines_after_reflow_and_edits() {
        let mut renderer = renderer();
        let selection = Shared::default();
        let mut tree = Tree::empty();
        let original = "a\u{301}👩‍💻z\nsecond line with enough words to wrap\nlast";
        let mut runs = Vec::new();
        for (value, width) in [
            (original, 500.),
            (original, 90.),
            ("אבג\ncafé\nend", 300.),
            ("", 90.),
        ] {
            let mut view = plain(
                value,
                16.,
                None,
                selection.clone(),
                Position {
                    entry: 0,
                    block: 0,
                    byte: 0,
                },
            );
            tree.diff(view.as_widget());
            view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(width, 1000.)),
            );
            let state = tree.state.downcast_ref::<State>();
            runs.push(state.paragraph.buffer().layout_runs().count());
            // Independent linear reference, including bytes inside UTF-8/emoji clusters
            // and equal-distance ties. It also catches stale indexes after an edit.
            for byte in 0..=value.len() {
                let expected = value
                    .grapheme_indices(true)
                    .map(|(i, _)| i)
                    .chain([value.len()])
                    .min_by_key(|i| i.abs_diff(byte))
                    .unwrap();
                assert_eq!(state.index.graphemes[state.index.nearest(byte)], expected);
            }
            // Explicit LF fixtures: visual wraps must never become logical line offsets.
            let mut offset = 0;
            for (line, text) in value.split('\n').enumerate() {
                for byte in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
                    let cursor = state.index.cursor(offset + byte);
                    assert_eq!((cursor.line, cursor.index), (line, byte));
                }
                offset += text.len() + 1;
            }
            // Exercise the cached endpoint mapping in drawing as well as in lookup.
            selection.borrow_mut().start(Position {
                entry: 0,
                block: 0,
                byte: 0,
            });
            selection.borrow_mut().extend(Position {
                entry: 0,
                block: 0,
                byte: value.len(),
            });
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(width, 1000.)),
            );
            renderer.reset(Rectangle::with_size(Size::new(width, 1000.)));
            view.as_widget().draw(
                &tree,
                &mut renderer,
                &iced::Theme::Dark,
                &renderer::Style {
                    text_color: Color::WHITE,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &Rectangle::with_size(Size::new(width, 1000.)),
            );
        }
        assert!(
            runs[1] > runs[0],
            "narrowing must actually exercise visual reflow"
        );
    }

    #[test]
    fn selection_units_reuse_unchanged_text_and_refresh_text_offset_and_mode() {
        let mut renderer = renderer();
        let selection = Shared::default();
        let mut cache = user_interface::Cache::new();
        // Same-length edits change word boundaries. Width/style changes must not
        // segment text again, while offsets and paragraph semantics must refresh.
        let original = "café cat\n\nlast";
        let edited = "ab cd cat\n\nlast";
        let block = Paragraphs::Block;
        let plain = Paragraphs::BlankLines;
        let white = Some(Color::WHITE);
        for (value, paragraphs, offset, width, color, reuse, word_end, paragraph_end) in [
            (original, block, 0, 500., None, false, 5, 15),
            (original, block, 0, 45., None, true, 5, 15),
            (original, block, 0, 45., white, true, 5, 15),
            (edited, block, 0, 500., None, false, 2, 15),
            (edited, block, 37, 500., None, false, 2, 15),
            (edited, plain, 37, 500., None, false, 2, 9),
            (edited, plain, 37, 90., None, true, 2, 9),
        ] {
            let before = UNITS.get();
            let mut span = Span::new(value.to_owned());
            span.color = color;
            let view = selectable(
                Arc::from([span]),
                16.into(),
                Font::DEFAULT,
                selection.clone(),
                Position {
                    entry: 3,
                    block: 2,
                    byte: offset,
                },
                match paragraphs {
                    Paragraphs::Block => SelectableText::Block,
                    _ => SelectableText::Plain,
                },
            );
            let mut ui = UserInterface::build(
                pointer_input(view, false, selection.clone()),
                Size::new(width, 1000.),
                cache,
                &mut renderer,
            );
            if reuse {
                assert_eq!(
                    UNITS.get(),
                    before,
                    "unchanged logical text must reuse its index"
                );
            } else {
                assert!(
                    UNITS.get() > before,
                    "changed selection semantics need a new index"
                );
            }
            let mut bounds = TextBounds::default();
            ui.operate(&renderer, &mut bounds);
            assert_eq!(
                bounds.0[0].0, value,
                "text operations must see the current retained text"
            );
            let point = bounds.0[0].1.position() + Vector::new(1., 8.);
            for (count, end) in [(2, word_end), (3, paragraph_end)] {
                selection.borrow_mut().clear();
                clicks(&mut ui, &mut renderer, point, count, false);
                let (start, finish) = selection
                    .borrow()
                    .range()
                    .expect("multi-click selects text");
                assert_eq!(
                    start,
                    Position {
                        entry: 3,
                        block: 2,
                        byte: offset
                    }
                );
                assert_eq!(
                    finish,
                    Position {
                        byte: offset + end,
                        ..start
                    }
                );
            }
            cache = ui.into_cache();
        }
    }

    #[test]
    fn retained_tool_rows_replace_shared_units_even_when_visible_text_is_unchanged() {
        let mut renderer = renderer();
        let selection = Shared::default();
        let mut cache = user_interface::Cache::new();
        // Identical visible text, different unseen suffix and logical line extent.
        // Row-local indexes or reuse based only on the visible spans are incorrect.
        for (source, end) in [
            ("prefix café tail", 17),
            ("prefix café much longer tail", 29),
        ] {
            let units = Arc::new(TextUnits::lines(source));
            let before = UNITS.get();
            let mut ui = UserInterface::build(
                pointer_input(
                    tool_row(
                        "café",
                        selection.clone(),
                        Position {
                            entry: 0,
                            block: 0,
                            byte: 7,
                        },
                        20.,
                        false,
                        units,
                    ),
                    false,
                    selection.clone(),
                ),
                Size::new(300., 100.),
                cache,
                &mut renderer,
            );
            assert_eq!(
                UNITS.get(),
                before,
                "tool rows must use their shared document index"
            );
            selection.borrow_mut().clear();
            clicks(&mut ui, &mut renderer, Point::new(1., 8.), 3, false);
            let (start, finish) = selection.borrow().range().unwrap();
            assert_eq!(start.byte, 0);
            assert_eq!(finish.byte, end);
            cache = ui.into_cache();
        }
    }

    #[test]
    fn candidate_rejection_bounds_hits_and_preserves_closer_and_tied_targets() {
        let mut renderer = renderer();
        let mut hit_counts = Vec::new();
        for count in [2, 80] {
            let selection = Shared::default();
            let rows = (0..count).map(|entry| {
                tool_row(
                    "alpha omega",
                    selection.clone(),
                    Position {
                        entry,
                        block: 0,
                        byte: 0,
                    },
                    20.,
                    false,
                    Arc::new(TextUnits::lines("alpha omega")),
                )
            });
            let mut ui = UserInterface::build(
                pointer_input(column(rows).spacing(12).into(), false, selection.clone()),
                Size::new(300., 2000.),
                user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = TextBounds::default();
            ui.operate(&renderer, &mut bounds);
            let first = bounds.0[0].1;
            let second = bounds.0[1].1;
            let first_point = Point::new(first.x + 1., first.center_y());
            send(&mut ui, &mut renderer, press(), first_point);
            HITS.set(0);
            send(&mut ui, &mut renderer, motion(first_point), first_point);
            hit_counts.push(HITS.get());
            // A nearer block encountered later must replace the first candidate;
            // changing direction must not retain the previous event's winner.
            for (point, entry) in [
                (Point::new(second.x + 1., second.center_y()), 1),
                (first_point, 0),
                (
                    Point::new(first.x + 1., (first.y + first.height + second.y) / 2.),
                    0,
                ),
            ] {
                send(&mut ui, &mut renderer, motion(point), point);
                assert_eq!(selection.borrow().focus.unwrap().entry, entry);
            }
        }
        assert!(
            hit_counts[0] > 0,
            "the fixture must exercise text hit testing"
        );
        assert_eq!(
            hit_counts[0], hit_counts[1],
            "adding noncompetitive blocks must not add text hits"
        );
    }

    #[test]
    fn units_follow_unicode_words_and_logical_not_visual_lines() {
        let text = "can't foo_bar café e\u{301} 👩‍💻  ...";
        let offset = 19;
        let units = TextUnits::new(text, offset, Paragraphs::Block);
        for expected in ["can't", "foo_bar", "café", "e\u{301}", "👩‍💻", "  ", "."] {
            let start = text.find(expected).unwrap();
            let range = units.range(offset + start, Granularity::Word);
            assert_eq!(&text[range.start - offset..range.end - offset], expected);
        }
        let text = "first\r\nstill first\r\n \t\r\nsecond\nline";
        let units = TextUnits::new(text, 0, Paragraphs::BlankLines);
        for (needle, expected) in [("still", "first\r\nstill first"), ("line", "second\nline")] {
            let range = units.range(text.find(needle).unwrap(), Granularity::Paragraph);
            assert_eq!(&text[range], expected);
        }
        let lines = TextUnits::lines(text);
        assert_eq!(
            &text[lines.range(text.find("still").unwrap(), Granularity::Paragraph)],
            "still first"
        );
    }

    #[test]
    fn double_click_uses_the_glyph_under_pointer_not_the_nearest_caret() {
        let selection = Shared::default();
        let mut renderer = renderer();
        let mut ui = UserInterface::build(
            pointer_input(
                selectable(
                    Arc::from([Span::new("a bc.".to_owned())]),
                    20.into(),
                    Font::MONOSPACE,
                    selection.clone(),
                    Position {
                        entry: 0,
                        block: 0,
                        byte: 0,
                    },
                    SelectableText::Block,
                ),
                false,
                selection.clone(),
            ),
            Size::new(300., 100.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = TextBounds::default();
        ui.operate(&renderer, &mut bounds);
        let bounds = bounds.0[0].1;
        let cell = bounds.width / 5.;
        for (x, expected) in [(0.8, "a"), (1.2, " "), (3.8, "bc"), (4.2, ".")] {
            selection.borrow_mut().clear();
            let at = Point::new(bounds.x + x * cell, bounds.center_y());
            clicks(&mut ui, &mut renderer, at, 2, false);
            assert_eq!(
                copy_blocks(
                    &selection.borrow(),
                    [(
                        0,
                        vec![Block {
                            text: "a bc.".into(),
                            separator: ""
                        }]
                    )]
                ),
                expected
            );
        }
        // Cross a glyph midpoint while holding the second press: its nearest
        // caret changes, but the word does not. The next click is still a triple.
        selection.borrow_mut().clear();
        let at = Point::new(bounds.x + 2.49 * cell, bounds.center_y());
        let jitter = Point::new(bounds.x + 2.51 * cell, bounds.center_y());
        clicks(&mut ui, &mut renderer, at, 2, true);
        send(&mut ui, &mut renderer, motion(jitter), jitter);
        send(&mut ui, &mut renderer, release(), jitter);
        clicks(&mut ui, &mut renderer, jitter, 1, false);
        assert_eq!(
            copy_blocks(
                &selection.borrow(),
                [(
                    0,
                    vec![Block {
                        text: "a bc.".into(),
                        separator: ""
                    }]
                )]
            ),
            "a bc."
        );
        drop(ui);
        let text = "אבג דהו";
        let mut ui = UserInterface::build(
            pointer_input(
                plain(
                    text,
                    20.,
                    None,
                    selection.clone(),
                    Position {
                        entry: 0,
                        block: 0,
                        byte: 0,
                    },
                ),
                false,
                selection.clone(),
            ),
            Size::new(300., 100.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = TextBounds::default();
        ui.operate(&renderer, &mut bounds);
        let bounds = bounds.0[0].1;
        for (x, expected) in [
            (bounds.x + 0.1, "דהו"),
            (bounds.x + bounds.width - 0.1, "אבג"),
        ] {
            selection.borrow_mut().clear();
            clicks(
                &mut ui,
                &mut renderer,
                Point::new(x, bounds.center_y()),
                2,
                false,
            );
            assert_eq!(
                copy_blocks(
                    &selection.borrow(),
                    [(
                        0,
                        vec![Block {
                            text: text.into(),
                            separator: ""
                        }]
                    )]
                ),
                expected
            );
        }
    }

    #[test]
    fn multiclick_markdown_drag_reverses_by_units_and_resets_after_other_interactions() {
        let selection = Shared::default();
        let content = markdown::Content::parse(
            "Before\n\nca**fé** with words that wrap onto another row.  \nHard break.\n\nAfter\n\n```text\ncode first\ncode second\n```\n",
        );
        let viewer = Viewer::new(selection.clone(), 0);
        let mut renderer = renderer();
        let mut ui = UserInterface::build(
            pointer_input(
                markdown::view_with(
                    content.items(),
                    markdown::Settings::from(iced::Theme::Dark),
                    &viewer,
                ),
                false,
                selection.clone(),
            ),
            Size::new(210., 600.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = TextBounds::default();
        ui.operate(&renderer, &mut bounds);
        let point = |prefix: &str| {
            let bounds = bounds
                .0
                .iter()
                .find(|(text, _)| text.starts_with(prefix))
                .unwrap()
                .1;
            Point::new(bounds.x + 1., bounds.y + 5.)
        };
        let at = point("café");
        let paragraph = "café with words that wrap onto another row.\nHard break.";
        clicks(&mut ui, &mut renderer, at, 2, true);
        assert_eq!(copied(&selection, &viewer), "café");
        for (target, expected) in [
            ("Before", "Before\n\ncafé"),
            (
                "After",
                "café with words that wrap onto another row.\nHard break.\n\nAfter",
            ),
            ("café", "café"),
        ] {
            let end = point(target);
            send(&mut ui, &mut renderer, motion(end), end);
            assert_eq!(copied(&selection, &viewer), expected);
        }
        send(&mut ui, &mut renderer, release(), at);
        // A completed drag must not count as the first click of a new sequence.
        clicks(&mut ui, &mut renderer, at, 1, false);
        assert!(selection.borrow().range().is_none());
        selection.borrow_mut().clear();
        clicks(&mut ui, &mut renderer, at, 3, true);
        assert_eq!(copied(&selection, &viewer), paragraph);
        for (target, expected) in [
            ("Before", format!("Before\n\n{paragraph}")),
            ("After", format!("{paragraph}\n\nAfter")),
            ("café", paragraph.to_owned()),
        ] {
            let end = point(target);
            send(&mut ui, &mut renderer, motion(end), end);
            assert_eq!(copied(&selection, &viewer), expected);
        }
        send(&mut ui, &mut renderer, release(), at);
        assert_eq!(copied(&selection, &viewer), paragraph);
        let messages = send(
            &mut ui,
            &mut renderer,
            key(
                keyboard::Key::Character("c".into()),
                keyboard::key::Code::KeyC,
            ),
            at,
        );
        assert!(matches!(messages.as_slice(), [Message::CopySelection]));
        // Clicking outside text breaks the sequence, even if the next text click
        // returns immediately to the same coordinates.
        clicks(&mut ui, &mut renderer, Point::new(205., 590.), 1, false);
        clicks(&mut ui, &mut renderer, at, 1, false);
        assert!(selection.borrow().range().is_none());
        selection.borrow_mut().invalidate_entry(0);
        clicks(&mut ui, &mut renderer, at, 1, false);
        assert!(selection.borrow().range().is_none());
        selection.borrow_mut().clear();
        clicks(&mut ui, &mut renderer, point("code second"), 3, false);
        assert_eq!(copied(&selection, &viewer), "code second");
    }

    #[test]
    fn triple_click_plain_text_selects_blank_line_delimited_paragraph() {
        let selection = Shared::default();
        let text = "First\ncontinued\n\nSecond";
        let mut renderer = renderer();
        let mut ui = UserInterface::build(
            pointer_input(
                plain(
                    text,
                    20.,
                    None,
                    selection.clone(),
                    Position {
                        entry: 0,
                        block: 0,
                        byte: 0,
                    },
                ),
                false,
                selection.clone(),
            ),
            Size::new(400., 200.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = TextBounds::default();
        ui.operate(&renderer, &mut bounds);
        let bounds = bounds.0[0].1;
        clicks(
            &mut ui,
            &mut renderer,
            Point::new(bounds.x + 1., bounds.y + bounds.height * 1.5 / 4.),
            3,
            false,
        );
        assert_eq!(
            copy_blocks(
                &selection.borrow(),
                [(
                    0,
                    vec![Block {
                        text: text.into(),
                        separator: ""
                    }]
                )]
            ),
            "First\ncontinued"
        );
    }

    #[test]
    fn tool_multiclick_selects_unmounted_word_and_logical_line_across_reflow() {
        let selection = Shared::default();
        let word = "éclair".repeat(500);
        let line = format!("prefix {word} suffix");
        let source = format!("{line}\r\nshort\nlast");
        let mut output = crate::tool_output::Output::new("test", "cmd", &source, false);
        let display = crate::tool_output::display("test", "cmd", &source, false);
        let mut renderer = renderer();
        output.update(crate::tool_output::Event::Resize(320.));
        let mut ui = UserInterface::build(
            pointer_input(
                output.view("tool", selection.clone(), 0),
                false,
                selection.clone(),
            ),
            Size::new(320., 340.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = TextBounds::default();
        ui.operate(&renderer, &mut bounds);
        let (row, bounds) = bounds
            .0
            .iter()
            .find(|(text, _)| text.starts_with("prefix "))
            .unwrap();
        assert!(row.len() < word.len());
        let at = Point::new(
            bounds.x + bounds.width * 9. / row.chars().count() as f32,
            bounds.center_y(),
        );
        let copy = || {
            copy_blocks(
                &selection.borrow(),
                [(
                    0,
                    vec![Block {
                        text: display.clone(),
                        separator: "",
                    }],
                )],
            )
        };
        clicks(&mut ui, &mut renderer, at, 2, false);
        assert_eq!(copy(), word);
        selection.borrow_mut().clear();
        clicks(&mut ui, &mut renderer, at, 3, false);
        assert_eq!(copy(), line);
        let cache = ui.into_cache();
        output.update(crate::tool_output::Event::Resize(150.));
        output.update(crate::tool_output::Event::Scroll(680.));
        let mut ui = UserInterface::build(
            pointer_input(
                output.view("tool", selection.clone(), 0),
                false,
                selection.clone(),
            ),
            Size::new(150., 340.),
            cache,
            &mut renderer,
        );
        ui.draw(
            &mut renderer,
            &crate::theme::app(),
            &renderer::Style {
                text_color: Color::WHITE,
            },
            mouse::Cursor::Unavailable,
        );
        assert_eq!(copy(), line);
    }

    #[test]
    fn shared_paragraph_matches_rich_text_pixels() {
        use iced::widget::rich_text;
        let mut renderer = crate::test_renderer::new(16.);
        let spans: Arc<[Span<'static, String>]> = Arc::from([
            Span::new("Bold café ").font(Font {
                weight: iced::font::Weight::Bold,
                ..Font::DEFAULT
            }),
            Span::new("code 界")
                .font(Font::MONOSPACE)
                .color(Color::from_rgb8(240, 200, 80))
                .background(Color::from_rgb8(60, 40, 20))
                .border(iced::Border {
                    color: Color::from_rgb8(180, 100, 50),
                    width: 1.,
                    radius: 3.into(),
                })
                .padding([2, 4]),
            Span::new(" struck ").strikethrough(true),
            Span::new("link")
                .underline(true)
                .link("https://example.com".to_owned())
                .color(Color::from_rgb8(110, 180, 240))
                .size(21),
            Span::new("\nHard break e\u{301} 👩‍💻 אבג mixed and a wrapping suffix."),
        ]);
        let mut paint_only = spans.to_vec();
        paint_only[1] = paint_only[1]
            .clone()
            .padding([5, 9])
            .background(Color::from_rgb8(30, 60, 90));
        paint_only[2] = paint_only[2].clone().underline(true);
        let paint_only: Arc<[_]> = paint_only.into();
        let mut restyled = paint_only.to_vec();
        // Decoration-only additions/removals must invalidate even when text/font metrics match.
        restyled[0] = restyled[0].clone().background(Color::from_rgb8(30, 60, 90));
        let restyled: Arc<[_]> = restyled.into();
        let mut edited = restyled.to_vec();
        edited[0] = Span::new("Changed prefix with more wrapping words. ");
        let edited: Arc<[_]> = edited.into();
        let undecorated: Arc<[_]> = Arc::from([Span::new("Plain text replaces all decorations.")]);
        for scale in [1., 1.25] {
            let mut cache = user_interface::Cache::new();
            for (width, scroll, text_size, font, spans, recompute) in [
                (420., 0., 18., Font::DEFAULT, &spans, true),
                (420., 15., 18., Font::DEFAULT, &spans, false),
                (160., 0., 18., Font::DEFAULT, &spans, true),
                (160., 15., 18., Font::DEFAULT, &spans, false),
                (160., 15., 22., Font::DEFAULT, &spans, true),
                (160., 15., 22., Font::MONOSPACE, &spans, true),
                (160., 15., 22., Font::MONOSPACE, &paint_only, false),
                (160., 15., 22., Font::MONOSPACE, &restyled, true),
                (160., 15., 22., Font::MONOSPACE, &spans, true),
                (160., 15., 22., Font::MONOSPACE, &edited, true),
                (160., 0., 22., Font::MONOSPACE, &undecorated, false),
            ] {
                let size = Size::new(width, 240.);
                let mut frames = Vec::new();
                for reference in [true, false] {
                    let queries = DECORATION_QUERIES.get();
                    let text: Element<'_, Message> = if reference {
                        rich_text(spans.clone()).size(text_size).font(font).into()
                    } else {
                        selectable(
                            spans.clone(),
                            Pixels(text_size),
                            font,
                            Shared::default(),
                            Position {
                                entry: 0,
                                block: 0,
                                byte: 0,
                            },
                            SelectableText::Block,
                        )
                    };
                    let mut ui = UserInterface::build(
                        scrollable(container(text).padding(8))
                            .id("decoration-test")
                            .height(100),
                        size,
                        if reference {
                            user_interface::Cache::new()
                        } else {
                            std::mem::take(&mut cache)
                        },
                        &mut renderer,
                    );
                    // Absolute positioning keeps retained and fresh scrollables
                    // at the same offset, including after content shrink/reflow.
                    ui.operate(
                        &renderer,
                        &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                            iced::advanced::widget::Id::new("decoration-test"),
                            iced::widget::scrollable::AbsoluteOffset {
                                x: None,
                                y: Some(scroll),
                            },
                        ),
                    );
                    renderer.reset(Rectangle::with_size(size));
                    ui.draw(
                        &mut renderer,
                        &iced::Theme::Dark,
                        &renderer::Style {
                            text_color: Color::WHITE,
                        },
                        mouse::Cursor::Unavailable,
                    );
                    if !reference {
                        if recompute {
                            assert!(
                                DECORATION_QUERIES.get() > queries,
                                "changed layout must refresh decorated bounds"
                            );
                        } else {
                            assert_eq!(
                                DECORATION_QUERIES.get(),
                                queries,
                                "scroll-only redraws and undecorated text need no geometry queries"
                            );
                        }
                        cache = ui.into_cache();
                    }
                    frames.push(crate::test_renderer::screenshot(
                        &mut renderer,
                        Size::new((width * scale) as u32, (240. * scale) as u32),
                        scale,
                        Color::BLACK,
                    ));
                }
                assert!(
                    frames[0]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|p| p[0] > 200 && p[1] > 200 && p[2] > 200),
                    "the reference must actually render text"
                );
                assert!(
                    frames[0] == frames[1],
                    "glyphs, inline-code backgrounds/borders, decorations and wrapping differ at width {width}, scale {scale}, scroll {scroll}"
                );
            }
        }
    }

    #[test]
    fn right_to_left_hits_follow_the_painted_shrink_width() {
        let selection = Shared::default();
        let mut renderer = renderer();
        let mut ui = UserInterface::build(
            pointer_input(
                plain(
                    "אבג",
                    20.,
                    None,
                    selection.clone(),
                    Position {
                        entry: 0,
                        block: 0,
                        byte: 0,
                    },
                ),
                false,
                selection.clone(),
            ),
            Size::new(300., 80.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = TextBounds::default();
        ui.operate(&renderer, &mut bounds);
        let bounds = bounds.0[0].1;
        send(
            &mut ui,
            &mut renderer,
            press(),
            Point::new(bounds.x + 0.1, bounds.y + 10.),
        );
        assert_eq!(selection.borrow().anchor.unwrap().byte, "אבג".len());
        send(
            &mut ui,
            &mut renderer,
            press(),
            Point::new(bounds.x + bounds.width - 0.1, bounds.y + 10.),
        );
        assert_eq!(selection.borrow().anchor.unwrap().byte, 0);
    }

    #[test]
    fn markdown_image_previews_preserve_alt_text_selection_positions() {
        let selection = Shared::default();
        let content = markdown::Content::parse("Before\n\n![Caption](image.png)\n\nAfter");
        let images = crate::markdown_images::Cache::default();
        let visible = Viewer::new(selection.clone(), 0).with_images(&images);
        let copy = Viewer::new(selection.clone(), 0);
        let settings = markdown::Settings::from(iced::Theme::Dark);
        let _ = markdown::view_with(content.items(), settings, &visible);
        let _ = markdown::view_with(content.items(), settings, &copy);
        for viewer in [visible, copy] {
            selection.borrow_mut().start(Position {
                entry: 0,
                block: 0,
                byte: 0,
            });
            selection.borrow_mut().extend(Position {
                entry: 0,
                block: 2,
                byte: 5,
            });
            assert_eq!(
                copy_blocks(&selection.borrow(), [(0, viewer.blocks.into_inner())]),
                "Before\n\nCaption\n\nAfter"
            );
        }
    }
    #[test]
    fn drag_across_markdown_and_messages_copies_visible_text_in_both_directions() {
        let selection = Shared::default();
        let content = markdown::Content::parse(
            "# Heading\n\nA **bold** café界.  \nHard break.\n\n```rust\nfirst\n\nlast\n```\n\n> Quoted\n\n| Left | Right |\n| --- | --- |\n| uno | dos |\n",
        );
        let viewer = Viewer::new(selection.clone(), 0);
        let md = markdown::view_with(
            content.items(),
            markdown::Settings::from(iced::Theme::Dark),
            &viewer,
        );
        let notice = "Final e\u{301}👩‍💻";
        let view = pointer_input(
            column![
                md,
                plain(
                    notice,
                    16.,
                    None,
                    selection.clone(),
                    Position {
                        entry: 1,
                        block: 0,
                        byte: 0
                    }
                ),
                text_input("", "composer").on_input(Message::Search)
            ]
            .spacing(12)
            .into(),
            false,
            selection.clone(),
        );
        let mut renderer = renderer();
        let mut ui = UserInterface::build(
            view,
            Size::new(400., 900.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = TextBounds::default();
        ui.operate(&renderer, &mut bounds);
        let heading = bounds
            .0
            .iter()
            .find(|(text, _)| text == "Heading")
            .unwrap()
            .1;
        let last = bounds.0.iter().find(|(text, _)| text == notice).unwrap().1;
        let first_point = Point::new(heading.x, heading.center_y());
        let last_point = Point::new(last.x + last.width - 0.1, last.center_y());
        for (start, end) in [(first_point, last_point), (last_point, first_point)] {
            send(&mut ui, &mut renderer, press(), start);
            send(&mut ui, &mut renderer, motion(end), end);
            send(&mut ui, &mut renderer, release(), end);
            assert!(!selection.borrow().dragging);
            ui.draw(
                &mut renderer,
                &iced::Theme::Dark,
                &renderer::Style {
                    text_color: Color::WHITE,
                },
                mouse::Cursor::Available(end),
            );
            // Holding Control must not clear the selection before C arrives.
            send(
                &mut ui,
                &mut renderer,
                key(
                    keyboard::Key::Named(keyboard::key::Named::Control),
                    keyboard::key::Code::ControlLeft,
                ),
                end,
            );
            let messages = send(
                &mut ui,
                &mut renderer,
                key(
                    keyboard::Key::Character("c".into()),
                    keyboard::key::Code::KeyC,
                ),
                end,
            );
            assert!(
                matches!(messages.as_slice(), [Message::CopySelection]),
                "{messages:?}; range {:?}; start {start:?} end {end:?}",
                selection.borrow().range()
            );
            let blocks = viewer
                .blocks
                .borrow()
                .iter()
                .map(|b| Block {
                    text: b.text.clone(),
                    separator: b.separator,
                })
                .collect();
            assert_eq!(
                copy_blocks(
                    &selection.borrow(),
                    [
                        (0, blocks),
                        (
                            1,
                            vec![Block {
                                text: notice.into(),
                                separator: "\n\n"
                            }]
                        )
                    ]
                ),
                "Heading\n\nA bold café界.\nHard break.\n\nfirst\n\nlast\n\nQuoted\n\nLeft\n\nRight\n\nuno\n\ndos\n\nFinal e\u{301}👩‍💻"
            );
        }
        // Clicking the composer returns clipboard ownership to the editor.
        send(
            &mut ui,
            &mut renderer,
            press(),
            Point::new(10., last.y + last.height + 20.),
        );
        assert!(selection.borrow().range().is_none());
    }

    #[test]
    fn link_clicks_work_but_link_drags_select_and_clip_to_the_viewport() {
        let selection = Shared::default();
        let content =
            markdown::Content::parse("[link words](https://example.com)\n\nsecond\n\nthird");
        let viewer = Viewer::new(selection.clone(), 0);
        let view = pointer_input(
            scrollable(markdown::view_with(
                content.items(),
                markdown::Settings::from(iced::Theme::Dark),
                &viewer,
            ))
            .height(45)
            .into(),
            false,
            selection.clone(),
        );
        let mut renderer = renderer();
        let mut ui = UserInterface::build(
            view,
            Size::new(300., 200.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let at = Point::new(4., 10.);
        send(&mut ui, &mut renderer, press(), at);
        assert!(
            matches!(send(&mut ui, &mut renderer, release(), at).as_slice(), [Message::Link(url)] if url == "https://example.com")
        );
        // Multi-clicks select text but do not defer or suppress link activation.
        for expected in ["link", "link words"] {
            send(&mut ui, &mut renderer, press(), at);
            assert!(
                matches!(send(&mut ui, &mut renderer, release(), at).as_slice(), [Message::Link(url)] if url == "https://example.com")
            );
            assert_eq!(copied(&selection, &viewer), expected);
        }
        selection.borrow_mut().clear();
        send(&mut ui, &mut renderer, press(), at);
        let end = Point::new(60., 10.);
        send(&mut ui, &mut renderer, motion(end), end);
        assert!(send(&mut ui, &mut renderer, release(), end).is_empty());
        assert!(selection.borrow().range().is_some());
        // Text below the scrollable viewport is not an interactive hit target.
        send(&mut ui, &mut renderer, press(), Point::new(4., 80.));
        assert!(selection.borrow().range().is_none());
    }

    #[test]
    fn hard_line_hits_and_virtualized_rows_keep_source_offsets() {
        let selection = Shared::default();
        let mut renderer = renderer();
        let value = "αβ\nsecond 界\nthird";
        let view = pointer_input(
            plain(
                value,
                16.,
                None,
                selection.clone(),
                Position {
                    entry: 2,
                    block: 0,
                    byte: 0,
                },
            ),
            false,
            selection.clone(),
        );
        let mut ui = UserInterface::build(
            view,
            Size::new(200., 100.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        // Start on the second hard line, not offset zero in the first line.
        send(&mut ui, &mut renderer, press(), Point::new(0., 30.));
        assert_eq!(selection.borrow().anchor.unwrap().byte, "αβ\n".len());
        drop(ui);
        // Rebuild with only a later virtual row mounted. The earlier endpoint and
        // the intervening unmounted text still participate in Copy.
        let source = "Input\ncmd\n\nOutput\nrow one\nrow two 界\nlast";
        let offset = source.find("last").unwrap();
        let view = pointer_input(
            tool_row(
                "last",
                selection.clone(),
                Position {
                    entry: 5,
                    block: 0,
                    byte: offset,
                },
                17.,
                false,
                Arc::new(TextUnits::lines(source)),
            ),
            false,
            selection.clone(),
        );
        let mut ui = UserInterface::build(
            view,
            Size::new(200., 100.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        send(
            &mut ui,
            &mut renderer,
            motion(Point::new(150., 8.)),
            Point::new(150., 8.),
        );
        send(&mut ui, &mut renderer, release(), Point::new(150., 8.));
        assert_eq!(
            copy_blocks(
                &selection.borrow(),
                [
                    (
                        2,
                        vec![Block {
                            text: value.into(),
                            separator: "\n\n"
                        }]
                    ),
                    (
                        3,
                        vec![Block {
                            text: "offscreen message".into(),
                            separator: "\n\n"
                        }]
                    ),
                    (
                        5,
                        vec![Block {
                            text: source.into(),
                            separator: "\n\n"
                        }]
                    )
                ]
            ),
            "second 界\nthird\n\noffscreen message\n\nInput\ncmd\n\nOutput\nrow one\nrow two 界\nlast"
        );
    }
    #[test]
    fn tool_whitespace_selection_draws_and_copies_original_line_endings_and_tabs() {
        let selection = Shared::default();
        let source = "α\r\nβ\t\t\tEND";
        let expected = "Input\ncmd\n\nOutput\nα\r\nβ\t\t\tEND";
        let mut output = crate::tool_output::Output::new("test", "cmd", source, false);
        output.update(crate::tool_output::Event::Resize(160.));
        let mut renderer = renderer();
        let mut ui = UserInterface::build(
            pointer_input(
                output.view("tool", selection.clone(), 0),
                false,
                selection.clone(),
            ),
            Size::new(160., 340.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = TextBounds::default();
        ui.operate(&renderer, &mut bounds);
        let end = bounds
            .0
            .iter()
            .find(|(text, _)| text.ends_with("END"))
            .unwrap()
            .1;
        // A tab-heavy row's final glyph must actually fit and be pointer-selectable.
        send(
            &mut ui,
            &mut renderer,
            press(),
            Point::new(end.x + end.width - 0.1, end.center_y()),
        );
        assert_eq!(selection.borrow().anchor.unwrap().byte, expected.len());
        send(&mut ui, &mut renderer, release(), end.center());
        {
            let mut selection = selection.borrow_mut();
            selection.anchor = Some(Position {
                entry: 0,
                block: 0,
                byte: 0,
            });
            selection.focus = Some(Position {
                entry: 0,
                block: 0,
                byte: expected.len(),
            });
        }
        // Previously the trailing CR was stripped by shaping but included in the
        // highlight range, causing drawing a cross-line selection to panic.
        ui.draw(
            &mut renderer,
            &crate::theme::app(),
            &renderer::Style {
                text_color: Color::WHITE,
            },
            mouse::Cursor::Unavailable,
        );
        assert_eq!(
            copy_blocks(
                &selection.borrow(),
                [(
                    0,
                    vec![Block {
                        text: crate::tool_output::display("test", "cmd", source, false),
                        separator: "\n\n",
                    }]
                )]
            ),
            expected
        );
    }

    #[test]
    fn wheel_drag_rehits_virtual_tool_rows_without_pointer_motion() {
        for click_count in 1..=3 {
            let selection = Shared::default();
            let source: String = (0..120).map(|i| format!("line {i:03}\n")).collect();
            let mut output = crate::tool_output::Output::new("test", "cmd", &source, false);
            let mut renderer = renderer();
            let view = pointer_input(
                output.view("tool", selection.clone(), 0),
                false,
                selection.clone(),
            );
            let mut ui = UserInterface::build(
                view,
                Size::new(320., 340.),
                user_interface::Cache::new(),
                &mut renderer,
            );
            let at = Point::new(0., 8.);
            clicks(&mut ui, &mut renderer, at, click_count, true);
            let messages = send(
                &mut ui,
                &mut renderer,
                Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Pixels { x: 0., y: -680. },
                }),
                at,
            );
            let cache = ui.into_cache();
            for message in messages {
                if let Message::ToolOutput(_, event) = message {
                    output.update(event);
                }
            }
            let view = pointer_input(
                output.view("tool", selection.clone(), 0),
                false,
                selection.clone(),
            );
            let mut ui = UserInterface::build(view, Size::new(320., 340.), cache, &mut renderer);
            send(
                &mut ui,
                &mut renderer,
                Event::Window(iced::window::Event::RedrawRequested(
                    iced::time::Instant::now(),
                )),
                at,
            );
            let display = crate::tool_output::display("test", "cmd", &source, false);
            #[derive(Default)]
            struct ScrolledText {
                translation: iced::Vector,
                rows: Vec<(String, Rectangle)>,
            }
            impl Operation for ScrolledText {
                fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
                    visit(self);
                }
                fn scrollable(
                    &mut self,
                    _: Option<&iced::advanced::widget::Id>,
                    _: Rectangle,
                    _: Rectangle,
                    translation: iced::Vector,
                    _: &mut dyn iced::advanced::widget::operation::Scrollable,
                ) {
                    self.translation = translation;
                }
                fn text(
                    &mut self,
                    _: Option<&iced::advanced::widget::Id>,
                    mut bounds: Rectangle,
                    text: &str,
                ) {
                    bounds.x -= self.translation.x;
                    bounds.y -= self.translation.y;
                    self.rows.push((text.to_owned(), bounds));
                }
            }
            let mut painted = ScrolledText::default();
            ui.operate(&renderer, &mut painted);
            let (row, _) = painted
                .rows
                .iter()
                .find(|(text, bounds)| text.starts_with("line ") && bounds.contains(at))
                .expect("scroll must expose a numbered source row under the stationary pointer");
            let expected_end = display.find(row.trim_end()).unwrap()
                + match click_count {
                    1 => 0,
                    2 => "line".len(),
                    3 => row.trim_end().len(),
                    _ => unreachable!(),
                };
            assert!(
                expected_end > selection.borrow().anchor.unwrap().byte,
                "scroll must advance the selection"
            );
            assert_eq!(selection.borrow().focus.unwrap().byte, expected_end);
            send(&mut ui, &mut renderer, release(), at);
            assert_eq!(
                copy_blocks(
                    &selection.borrow(),
                    [(
                        0,
                        vec![Block {
                            text: display.clone(),
                            separator: "\n\n"
                        }]
                    )]
                ),
                &display[..expected_end]
            );
        }
    }
}

#[cfg(test)]
#[path = "chat_selection_benchmark.rs"]
mod benchmark;
