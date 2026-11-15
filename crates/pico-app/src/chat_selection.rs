//! Logical transcript positions survive virtualized widgets being unmounted.
//! Only mounted text is shaped; copy reconstructs the selected entries on demand.
use crate::scrollbar::horizontal;
use crate::ui::Message;
use iced::{
    Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Vector,
    advanced::{
        Clipboard, Layout, Renderer as _, Shell, Widget,
        graphics::text::cosmic_text,
        layout, renderer,
        text::{self, Paragraph as _, Span},
        widget::{Operation, Tree, tree},
    },
    mouse,
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
    candidate: Option<(Position, f32)>,
}
impl Selection {
    pub fn clear(&mut self) {
        *self = Self::default();
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
    }
    pub fn begin_motion(&mut self) {
        self.candidate = None;
    }
    pub fn finish_motion(&mut self) -> bool {
        if let Some((at, _)) = self.candidate.take() {
            let changed = self.focus != Some(at);
            self.extend(at);
            changed
        } else {
            false
        }
    }
    fn offer(&mut self, at: Position, distance: f32) {
        if self.candidate.is_none_or(|(_, best)| distance < best) {
            self.candidate = Some((at, distance));
        }
    }
    fn extend(&mut self, at: Position) {
        self.focus = Some(at);
        self.moved |= self.anchor != self.focus;
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
            None,
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
        None,
    )
}

pub fn tool_row(
    value: &str,
    selection: Shared,
    at: Position,
    height: f32,
    heading: bool,
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
        Some(height),
    )
}

fn selectable(
    spans: Arc<[Span<'static, String>]>,
    size: Pixels,
    font: Font,
    selection: Shared,
    at: Position,
    row_height: Option<f32>,
) -> Element<'static, Message> {
    let content: String = spans.iter().map(|s| s.text.as_ref()).collect();
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
        content,
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
    content: String,
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
    pressed_link: Option<String>,
}
impl Selectable {
    fn hit(&self, state: &State, point: Point) -> Option<usize> {
        // Iced's generic hit_test drops the hard-line index. Use the renderer's
        // shaped buffer so hard breaks, wrapping, bidi and Unicode agree with paint.
        let buffer = state.paragraph.buffer();
        let hit = buffer.hit(point.x, point.y)?;
        let offset: usize = buffer.lines[..hit.line]
            .iter()
            .map(|l| l.text().len() + l.ending().as_str().len())
            .sum();
        let byte = offset + hit.index;
        Some(
            self.content
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain([self.content.len()])
                .min_by_key(|i| i.abs_diff(byte))
                .expect("end boundary"),
        )
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
        _tree: &mut Tree,
        layout: Layout<'_>,
        _renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.text(None, layout.bounds(), &self.content);
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
            if state.spans != self.spans
                || state.paragraph.compare(text.with_content(())) == text::Difference::Shape
            {
                state.paragraph = Paragraph::with_spans(text);
                state.spans = self.spans.clone();
                #[cfg(test)]
                tests::SHAPED.with(|count| count.set(count.get() + 1));
            } else if state.paragraph.bounds() != text.bounds {
                state.paragraph.resize(text.bounds);
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
            if let Some(byte) = self.hit(state, Point::new(x - bounds.x, y - bounds.y)) {
                self.selection.borrow_mut().offer(
                    Position {
                        byte: self.at.byte + byte,
                        ..self.at
                    },
                    distance,
                );
            }
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                state.pressed_link = None;
                if let Some(point) = position
                    && let Some(byte) = self.hit(state, Point::new(point.x, point.y))
                {
                    self.selection.borrow_mut().start(Position {
                        byte: self.at.byte + byte,
                        ..self.at
                    });
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
                            && self.hit(state, point).is_some_and(|byte| {
                                self.selection.borrow().anchor
                                    == Some(Position {
                                        byte: self.at.byte + byte,
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
        let paragraph = &tree.state.downcast_ref::<State>().paragraph;
        // Preserve Rich's span decorations (not just the glyphs). Default/top
        // alignment anchors both the paragraph and these regions at layout.position().
        let translation = layout.position() - Point::ORIGIN;
        for (index, span) in self.spans.iter().enumerate() {
            if span.highlight.is_none() && !span.underline && !span.strikethrough {
                continue;
            }
            let regions = paragraph.span_bounds(index);
            if let Some(highlight) = span.highlight {
                for bounds in &regions {
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
                for bounds in &regions {
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
            self.at.byte..self.at.byte + self.content.len(),
        );
        if range.is_empty() {
            return;
        }
        let buffer = paragraph.buffer();
        let to_cursor = |byte: usize| {
            let mut offset = 0;
            for (line, text) in buffer.lines.iter().enumerate() {
                if byte <= offset + text.text().len() {
                    return cosmic_text::Cursor::new(line, byte - offset);
                }
                offset += text.text().len() + text.ending().as_str().len();
            }
            unreachable!("selection lies within paragraph")
        };
        let (start, end) = (to_cursor(range.start), to_cursor(range.end));
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
        for (width, scroll) in [(420., 0.), (160., 0.), (160., 15.)] {
            let size = Size::new(width, 240.);
            for scale in [1., 1.25] {
                let mut frames = Vec::new();
                for reference in [true, false] {
                    let text: Element<'_, Message> = if reference {
                        rich_text(spans.clone()).size(18).font(Font::DEFAULT).into()
                    } else {
                        selectable(
                            spans.clone(),
                            Pixels(18.),
                            Font::DEFAULT,
                            Shared::default(),
                            Position {
                                entry: 0,
                                block: 0,
                                byte: 0,
                            },
                            None,
                        )
                    };
                    let mut ui = UserInterface::build(
                        scrollable(container(text).padding(8)).height(100),
                        size,
                        user_interface::Cache::new(),
                        &mut renderer,
                    );
                    if scroll > 0. {
                        send(
                            &mut ui,
                            &mut renderer,
                            Event::Mouse(mouse::Event::WheelScrolled {
                                delta: mouse::ScrollDelta::Pixels { x: 0., y: -scroll },
                            }),
                            Point::new(20., 30.),
                        );
                    }
                    renderer.reset(Rectangle::with_size(size));
                    ui.draw(
                        &mut renderer,
                        &iced::Theme::Dark,
                        &renderer::Style {
                            text_color: Color::WHITE,
                        },
                        mouse::Cursor::Unavailable,
                    );
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
        send(&mut ui, &mut renderer, press(), at);
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
        let expected_end = display.find(row.trim_end()).unwrap();
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
