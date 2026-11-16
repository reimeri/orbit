//! A growing editor with native viewport measurements and estimated offscreen heights.
use crate::{scrollbar, theme};
use iced::{
    Element, Event, Length, Rectangle, Size, Theme,
    advanced::{
        Clipboard, Layout, Renderer as _, Shell, Widget, layout, renderer,
        text::highlighter::PlainText,
        widget::{Operation, Tree, tree},
    },
    mouse,
    widget::{TextEditor, text_editor},
};

pub(crate) const PADDING: f32 = 12.;

pub fn growing_editor<'a, Message: Clone + 'a>(
    editor: TextEditor<'a, PlainText, Message>,
    content: &'a text_editor::Content,
    max_height: f32,
    on_scroll: impl Fn(ScrollTo) -> Message + 'a,
    scroll_lines: fn(&Message) -> Option<i32>,
) -> Element<'a, Message> {
    Element::new(GrowingEditor {
        editor: editor
            .padding(
                iced::Padding::new(PADDING).right(scrollbar::GUTTER + theme::SCROLLBAR_EDGE_INSET),
            )
            .height(Length::Shrink)
            .max_height(max_height),
        content,
        max_height,
        on_scroll: Box::new(on_scroll),
        scroll_lines,
    })
}

struct GrowingEditor<'a, Message> {
    editor: TextEditor<'a, PlainText, Message>,
    content: &'a text_editor::Content,
    max_height: f32,
    on_scroll: Box<dyn Fn(ScrollTo) -> Message + 'a>,
    scroll_lines: fn(&Message) -> Option<i32>,
}

/// An absolute native scroll request, applied in message order alongside edits
/// and wheel actions. Seeking never moves the caret or selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollTo {
    line: usize,
    fraction: f32,
}

impl ScrollTo {
    pub fn apply(self, content: &mut text_editor::Content) {
        content.with_editor_mut(|editor| editor.scroll_to(self.line, self.fraction));
    }
}

#[derive(Default)]
struct State {
    // Estimated cumulative row counts, not cloned/shaped offscreen text.
    row_ends: Vec<f32>,
    line_height: f32,
    content_height: f32,
    offset: f32,
    drag: Option<f32>,
    hovered: bool,
}

struct Scrollbar {
    track: Rectangle,
    thumb: Rectangle,
    max_offset: f32,
}

impl State {
    fn measure(&mut self, content: &text_editor::Content) {
        content.with_editor(|editor| {
            let buffer = editor.buffer();
            let metrics = buffer.metrics();
            let width = buffer.size().0.expect("laid-out editor width");
            self.row_ends.clear();
            let mut rows = 0.;
            let mut offset_rows = 0.;
            for (i, line) in buffer.lines.iter().enumerate() {
                if i == buffer.scroll().line {
                    offset_rows = rows;
                }
                // Reading an existing layout is cheap. Never shape text just
                // for a thumb: long offscreen lines use a rough width estimate.
                // Native layout invalidates these measurements on reflow/edit.
                let count = line.layout_opt().as_ref().map_or_else(
                    || {
                        (line.text().len() as f32 * metrics.font_size * 0.5
                            / width.max(metrics.font_size))
                        .ceil()
                        .max(1.)
                    },
                    |layout| layout.len() as f32,
                );
                rows += count;
                self.row_ends.push(rows);
            }
            self.line_height = metrics.line_height;
            self.content_height = rows * metrics.line_height;
            self.offset = offset_rows * metrics.line_height + buffer.scroll().vertical;
        });
    }

    fn scrollbar(&self, bounds: Rectangle) -> Option<Scrollbar> {
        let track = Rectangle {
            x: bounds.x + bounds.width - theme::SCROLLBAR_EDGE_INSET - theme::SCROLLBAR_WIDTH,
            y: bounds.y + PADDING,
            width: theme::SCROLLBAR_WIDTH,
            height: (bounds.height - 2. * PADDING).max(0.),
        };
        let max_offset = self.content_height - track.height;
        // Ignore subpixel rounding at the exact ten-row boundary.
        if max_offset < 0.5 || track.height == 0. {
            return None;
        }
        let height = (track.height * track.height / self.content_height)
            .max(20.)
            .min(track.height);
        Some(Scrollbar {
            track,
            thumb: Rectangle {
                x: track.x + (theme::SCROLLBAR_WIDTH - theme::SCROLLBAR_THUMB_WIDTH) / 2.,
                y: track.y + (track.height - height) * (self.offset / max_offset).clamp(0., 1.),
                width: theme::SCROLLBAR_THUMB_WIDTH,
                height,
            },
            max_offset,
        })
    }
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for GrowingEditor<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(
            &self.editor as &dyn Widget<Message, Theme, iced::Renderer>,
        )]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.children[0].diff(&self.editor as &dyn Widget<Message, Theme, iced::Renderer>);
    }

    fn size(&self) -> Size<Length> {
        Widget::size(&self.editor)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = self.editor.layout(&mut tree.children[0], renderer, limits);
        tree.state.downcast_mut::<State>().measure(self.content);
        // iced 0.14 measures partially visible rows, clamps the content height,
        // then adds padding again. Its viewport is correct, but its returned
        // node can exceed the maximum. The editor is a leaf: constrain that
        // node without changing its shaping, scrolling or input handling.
        layout::Node::new(Size::new(
            node.size().width,
            node.size()
                .height
                .min(self.max_height)
                .min(limits.max().height),
        ))
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.editor
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let scrollbar = state.scrollbar(layout.bounds());
        let hovered = scrollbar
            .as_ref()
            .is_some_and(|bar| cursor.is_over(bar.track));
        if state.hovered != hovered {
            state.hovered = hovered;
            shell.request_redraw();
        }
        if let Some(bar) = scrollbar {
            let seek = match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if hovered => {
                    let y = cursor.position().expect("hovered scrollbar").y;
                    let grab = if cursor.is_over(bar.thumb) {
                        y - bar.thumb.y
                    } else {
                        bar.thumb.height / 2.
                    };
                    state.drag = Some(grab);
                    Some(y - grab)
                }
                Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    state.drag.map(|grab| position.y - grab)
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                    if state.drag.take().is_some() =>
                {
                    shell.capture_event();
                    shell.request_redraw();
                    return;
                }
                _ => None,
            };
            if let Some(top) = seek {
                let travel = bar.track.height - bar.thumb.height;
                let target = if travel > 0. {
                    ((top - bar.track.y) / travel).clamp(0., 1.) * bar.max_offset
                } else {
                    0.
                };
                if target != state.offset {
                    // A relative pixel scroll would shape all intervening
                    // lines. Seek by logical line instead; native layout then
                    // measures/clamps only the destination viewport. Endpoints
                    // remain exact even when intermediate heights are estimates.
                    let request = if top <= bar.track.y {
                        ScrollTo {
                            line: 0,
                            fraction: 0.,
                        }
                    } else if top >= bar.track.y + travel {
                        ScrollTo {
                            line: state.row_ends.len() - 1,
                            fraction: 1.,
                        }
                    } else {
                        let row = target / state.line_height;
                        let line = state.row_ends.partition_point(|&end| end <= row);
                        let start = if line == 0 {
                            0.
                        } else {
                            state.row_ends[line - 1]
                        };
                        ScrollTo {
                            line,
                            fraction: (row - start) / (state.row_ends[line] - start),
                        }
                    };
                    shell.publish((self.on_scroll)(request));
                    // Suppress duplicate batched motion. The next layout uses
                    // the actual native position, including any end clamping.
                    state.offset = target;
                }
                shell.capture_event();
                shell.request_redraw();
                return;
            }
        } else {
            state.drag = None;
        }
        if matches!(event, Event::Window(iced::window::Event::Unfocused)) {
            state.drag = None;
        }
        let mut messages = Vec::new();
        let mut child_shell = Shell::new(&mut messages);
        self.editor.update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            &mut child_shell,
            viewport,
        );
        let pending_lines = std::cell::Cell::new(0);
        shell.merge(child_shell, |message| {
            // Native wheel actions may precede a track click in this same
            // event batch, before Content has applied the queued messages.
            if let Some(lines) = (self.scroll_lines)(&message) {
                pending_lines.set(pending_lines.get() + lines);
            }
            message
        });
        state.offset += pending_lines.get() as f32 * state.line_height;
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        if state.drag.is_some()
            || state
                .scrollbar(layout.bounds())
                .is_some_and(|bar| cursor.is_over(bar.track))
        {
            mouse::Interaction::None
        } else {
            self.editor
                .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.editor.draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        let state = tree.state.downcast_ref::<State>();
        if let Some(bar) = state.scrollbar(layout.bounds())
            && let Some(clip) = layout.bounds().intersection(viewport)
        {
            let thumb = crate::scrollbar::thumb(theme, state.hovered || state.drag.is_some());
            renderer.with_layer(clip, |renderer| {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: bar.thumb,
                        border: thumb.border,
                        ..Default::default()
                    },
                    thumb.background,
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::graphics::text as graphics_text;
    use iced::{Point, advanced::clipboard, keyboard};
    use text_editor::{Action, Content, Edit, Motion};

    const LINE_HEIGHT: f32 = 19.5;
    const VISIBLE_ROWS: usize = 10;

    #[derive(Debug, Clone, PartialEq)]
    enum Message {
        Editor(Action),
        Scroll(ScrollTo),
    }

    fn view(content: &Content) -> Element<'_, Message> {
        growing_editor(
            text_editor(content)
                .size(15)
                .line_height(iced::widget::text::LineHeight::Absolute(LINE_HEIGHT.into()))
                .min_height(2. * LINE_HEIGHT)
                .on_action(Message::Editor),
            content,
            VISIBLE_ROWS as f32 * LINE_HEIGHT + 2. * PADDING,
            Message::Scroll,
            |action| match action {
                Message::Editor(Action::Scroll { lines }) => Some(*lines),
                _ => None,
            },
        )
    }

    struct Editor {
        content: Content,
        tree: Tree,
        renderer: iced::Renderer,
    }

    impl Editor {
        fn new(value: &str) -> Self {
            let content = Content::with_text(value);
            let tree = Tree::new(view(&content).as_widget());
            Self {
                content,
                tree,
                renderer: crate::test_renderer::new(16.),
            }
        }

        fn layout(&mut self, width: f32) -> layout::Node {
            self.layout_in(width, 600.)
        }

        fn layout_in(&mut self, width: f32, height: f32) -> layout::Node {
            let mut view = view(&self.content);
            self.tree.diff(view.as_widget());
            view.as_widget_mut().layout(
                &mut self.tree,
                &self.renderer,
                &layout::Limits::new(Size::ZERO, Size::new(width, height)),
            )
        }

        fn bar(&self, node: &layout::Node) -> Option<Scrollbar> {
            self.tree
                .state
                .downcast_ref::<State>()
                .scrollbar(node.bounds())
        }

        fn interaction(&self, node: &layout::Node, point: Point) -> mouse::Interaction {
            view(&self.content).as_widget().mouse_interaction(
                &self.tree,
                Layout::new(node),
                mouse::Cursor::Available(point),
                &node.bounds(),
                &self.renderer,
            )
        }

        fn events(&mut self, node: &layout::Node, events: &[(Event, Point)]) -> Vec<Message> {
            let mut messages = Vec::new();
            {
                let mut view = view(&self.content);
                for (event, point) in events {
                    view.as_widget_mut().update(
                        &mut self.tree,
                        event,
                        Layout::new(node),
                        mouse::Cursor::Available(*point),
                        &self.renderer,
                        &mut clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &node.bounds(),
                    );
                }
            }
            for message in &messages {
                match message {
                    Message::Editor(action) => self.content.perform(action.clone()),
                    Message::Scroll(scroll) => scroll.apply(&mut self.content),
                }
            }
            messages
        }
    }

    fn press() -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
    }
    fn release() -> Event {
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
    }

    fn shaped_editor(value: &str, width: f32, height: f32) -> graphics_text::Editor {
        use iced::advanced::text::Editor as _;
        let mut editor = graphics_text::Editor::with_text(value);
        layout_shaped_editor(&mut editor, width, height);
        editor
    }

    fn layout_shaped_editor(editor: &mut graphics_text::Editor, width: f32, height: f32) {
        use iced::advanced::text::Editor as _;
        editor.update(
            Size::new(width, height),
            iced::Font::with_name("DejaVu Sans"),
            iced::Pixels(15.),
            iced::widget::text::LineHeight::Absolute(LINE_HEIGHT.into()),
            iced::advanced::text::Wrapping::default(),
            &mut PlainText,
        );
    }

    fn caret(editor: &graphics_text::Editor) -> Point {
        use iced::advanced::text::{Editor as _, editor::Selection};
        let Selection::Caret(point) = editor.selection() else {
            panic!("expected an unselected caret");
        };
        point
    }

    #[test]
    fn ligature_carets_follow_graphemes_in_both_directions() {
        use iced::advanced::text::Editor as _;
        // Explicit byte positions distinguish graphemes from bytes/code points.
        // Lam-alef is an RTL ligature; the accented a is one grapheme.
        for (value, start, middle, end, forward, backward) in [
            (
                "Let's implement the fix",
                20,
                21,
                22,
                Motion::Right,
                Motion::Left,
            ),
            ("a\u{301}fix", 3, 4, 5, Motion::Right, Motion::Left),
            ("لا", 0, 2, 4, Motion::Left, Motion::Right),
        ] {
            let mut editor = shaped_editor(value, 400., 100.);
            let (x, width, rtl) = {
                let glyph = editor
                    .buffer()
                    .layout_runs()
                    .flat_map(|run| run.glyphs)
                    .find(|glyph| glyph.start == start && glyph.end == end)
                    .expect("fixture must shape into a two-grapheme ligature");
                (glyph.x, glyph.w, glyph.level.is_rtl())
            };
            editor.perform(Action::Move(Motion::DocumentStart));
            while editor.cursor().position.column < start {
                editor.perform(Action::Move(forward));
            }
            let mut positions = Vec::new();
            for (column, fraction) in [(start, 0.), (middle, 0.5), (end, 1.)] {
                assert_eq!(editor.cursor().position.column, column, "{value:?}");
                let point = caret(&editor);
                let expected_x = x + width * if rtl { 1. - fraction } else { fraction };
                assert!(
                    (point.x - expected_x).abs() < 0.01,
                    "{value:?}: {point:?}, expected x={expected_x}"
                );
                positions.push(point);
                if column != end {
                    editor.perform(Action::Move(forward));
                }
            }
            for (column, point) in [(middle, positions[1]), (start, positions[0])] {
                editor.perform(Action::Move(backward));
                assert_eq!(editor.cursor().position.column, column);
                assert_eq!(caret(&editor), point);
            }
            if start == 3 {
                editor.perform(Action::Move(backward));
                assert_eq!(
                    editor.cursor().position.column,
                    0,
                    "do not split combining marks"
                );
            }
        }
    }

    #[test]
    fn ligature_carets_preserve_wrapping_affinity_and_scroll_coordinates() {
        use iced::advanced::text::Editor as _;
        let value = "prefix words repeated\nLet's implement the fix";
        let mut editor = shaped_editor(value, 105., 39.);
        editor.perform(Action::Move(Motion::DocumentEnd));
        layout_shaped_editor(&mut editor, 105., 39.);
        editor.perform(Action::Move(Motion::Left)); // after i
        let after = caret(&editor);
        editor.perform(Action::Move(Motion::Left)); // between f and i
        let middle = caret(&editor);
        editor.perform(Action::Move(Motion::Left)); // before f
        let before = caret(&editor);
        assert!(before.x < middle.x && middle.x < after.x);
        assert_eq!(before.y, middle.y);
        assert_eq!(middle.y, after.y);
        assert!(before.y >= 0. && before.y < 39.);
        assert!(editor.buffer().scroll().line > 0 || editor.buffer().scroll().vertical > 0.);
        let run_top = editor
            .buffer()
            .layout_runs()
            .find(|run| run.line_i == 1 && run.glyphs.iter().any(|glyph| glyph.start == 20))
            .expect("ligature should be visible")
            .line_top;
        assert!((middle.y - run_top).abs() < 0.01);

        // Wheel scrolling can leave the caret outside the visible runs.
        layout_shaped_editor(&mut editor, 105., 39.);
        editor.perform(Action::Scroll { lines: -100 });
        layout_shaped_editor(&mut editor, 105., 39.);
        assert_eq!(caret(&editor).x, before.x);
        assert!(caret(&editor).y >= 39.);

        // A forced wrap has two visual positions at the same logical boundary.
        let mut editor = shaped_editor("fifi", 12., 100.);
        editor.update(
            Size::new(12., 100.),
            iced::Font::with_name("DejaVu Sans"),
            iced::Pixels(15.),
            iced::widget::text::LineHeight::Absolute(LINE_HEIGHT.into()),
            iced::advanced::text::Wrapping::Glyph,
            &mut PlainText,
        );
        assert_eq!(editor.buffer().layout_runs().count(), 2);
        editor.perform(Action::Move(Motion::Right));
        editor.perform(Action::Move(Motion::Right));
        assert_eq!(editor.cursor().position.column, 2);
        let previous_row_end = caret(&editor);
        editor.perform(Action::Move(Motion::Right));
        editor.perform(Action::Move(Motion::Left));
        assert_eq!(editor.cursor().position.column, 2);
        let next_row_start = caret(&editor);
        assert!(previous_row_end.x > next_row_start.x);
        assert!((next_row_start.y - previous_row_end.y - LINE_HEIGHT).abs() < 0.01);
    }

    #[test]
    fn caret_affinity_handles_direction_changes_and_wrapped_spaces() {
        use iced::advanced::text::Editor as _;
        let mut editor = shaped_editor("fi אב fi", 100., 100.);
        let (after_space, before_alef) = {
            let glyphs = editor.buffer().layout_runs().next().unwrap().glyphs;
            let space = glyphs.iter().find(|glyph| glyph.start == 2).unwrap();
            let alef = glyphs.iter().find(|glyph| glyph.start == 3).unwrap();
            assert!(alef.level.is_rtl());
            (space.x + space.w, alef.x + alef.w)
        };
        assert!(after_space < before_alef);
        for _ in 0..3 {
            editor.perform(Action::Move(Motion::Right));
        }
        assert_eq!(editor.cursor().position.column, 3);
        assert_eq!(caret(&editor).x, after_space);
        editor.perform(Action::Move(Motion::Right));
        editor.perform(Action::Move(Motion::Left));
        assert_eq!(editor.cursor().position.column, 3);
        assert_eq!(caret(&editor).x, before_alef);

        // An LTR prefix makes the zero-advance mark precede the advancing
        // lam-alef glyph. Using only the first glyph collapses every caret.
        let mut editor = shaped_editor("x لَا", 100., 100.);
        let (left, right) = {
            let cluster: Vec<_> = editor
                .buffer()
                .layout_runs()
                .flat_map(|run| run.glyphs)
                .filter(|glyph| glyph.start == 2 && glyph.end == 8)
                .collect();
            assert_eq!(cluster.len(), 2);
            assert_eq!(cluster[0].w, 0.);
            assert!(cluster[1].w > 0.);
            (cluster[1].x, cluster[1].x + cluster[1].w)
        };
        editor.perform(Action::Move(Motion::DocumentEnd));
        for (motion, column, expected_x) in [
            (Motion::Left, 6, (left + right) / 2.),
            (Motion::Left, 2, right),
            (Motion::Right, 6, (left + right) / 2.),
            (Motion::Right, 8, left),
        ] {
            editor.perform(Action::Move(motion));
            assert_eq!(editor.cursor().position.column, column);
            assert!((caret(&editor).x - expected_x).abs() < 0.01);
        }

        let mut editor = shaped_editor("fi   fi", 15., 100.);
        assert_eq!(editor.buffer().layout_runs().count(), 2);
        // The first space fits on row one; the remaining spaces are omitted.
        let first_row_end = editor
            .buffer()
            .layout_runs()
            .next()
            .unwrap()
            .glyphs
            .iter()
            .map(|glyph| glyph.end)
            .max()
            .unwrap();
        assert_eq!(first_row_end, 3);
        for _ in 0..3 {
            editor.perform(Action::Move(Motion::Right));
        }
        let row_end = caret(&editor);
        editor.perform(Action::Move(Motion::Right));
        assert_eq!(editor.cursor().position.column, 4);
        assert_eq!(
            caret(&editor),
            row_end,
            "wrapped-away space stays at the row end"
        );
        editor.perform(Action::Move(Motion::Right));
        let row_start = caret(&editor);
        assert_eq!(row_start.x, 0.);
        assert!((row_start.y - row_end.y - LINE_HEIGHT).abs() < 0.01);
        for column in (3..=4).rev() {
            editor.perform(Action::Move(Motion::Left));
            assert_eq!(editor.cursor().position.column, column);
            assert_eq!(caret(&editor), row_end);
        }
    }

    fn selection_rectangles(editor: &graphics_text::Editor) -> Vec<Rectangle> {
        use iced::advanced::text::{Editor as _, editor::Selection};
        let Selection::Range(rectangles) = editor.selection() else {
            panic!("expected a selection range");
        };
        rectangles
    }

    #[test]
    fn ligature_selection_matches_individual_graphemes_and_copied_text() {
        use iced::advanced::text::{
            Editor as _,
            editor::{Cursor, Position},
        };
        for (value, start, middle, end, forward, backward) in [
            (
                "Let's implement the fix",
                20,
                21,
                22,
                Motion::Right,
                Motion::Left,
            ),
            ("لا", 0, 2, 4, Motion::Left, Motion::Right),
            ("x لَا", 2, 6, 8, Motion::Right, Motion::Left),
        ] {
            let mut editor = shaped_editor(value, 400., 100.);
            let cluster: Vec<_> = editor
                .buffer()
                .layout_runs()
                .flat_map(|run| run.glyphs)
                .filter(|glyph| glyph.start == start && glyph.end == end)
                .collect();
            assert!(!cluster.is_empty(), "fixture must form a ligature");
            let left = cluster
                .iter()
                .map(|glyph| glyph.x)
                .reduce(f32::min)
                .unwrap();
            let right = cluster
                .iter()
                .map(|glyph| glyph.x + glyph.w)
                .reduce(f32::max)
                .unwrap();
            let half = (right - left) / 2.;
            let rtl = cluster[0].level.is_rtl();
            for (anchor, motion, from, to) in [
                (start, forward, start, middle),
                (middle, forward, middle, end),
                (end, backward, middle, end),
                (middle, backward, start, middle),
            ] {
                editor.perform(Action::Move(Motion::DocumentStart));
                editor.move_to(Cursor {
                    position: Position {
                        line: 0,
                        column: anchor,
                    },
                    selection: None,
                });
                editor.perform(Action::Select(motion));
                assert_eq!(editor.copy().as_deref(), Some(&value[from..to]));
                let rectangles = selection_rectangles(&editor);
                assert_eq!(rectangles.len(), 1, "{value:?} {from}..{to}");
                let expected_x = if (from == start) != rtl {
                    left
                } else {
                    left + half
                };
                assert!(
                    (rectangles[0].x - expected_x).abs() < 0.01,
                    "{value:?}: {rectangles:?}"
                );
                assert!(
                    (rectangles[0].width - half).abs() < 0.01,
                    "{value:?}: {rectangles:?}"
                );
                assert_eq!(rectangles[0].y, 0.);
                assert_eq!(rectangles[0].height, LINE_HEIGHT);
            }
            // The ordinary click/drag path must select and highlight f or i,
            // not just keyboard-created logical ranges.
            if start == 20 {
                for (a, b, expected) in [
                    (left, left + half, "f"),
                    (left + half, right, "i"),
                    (left + half, left, "f"),
                    (right, left + half, "i"),
                ] {
                    editor.perform(Action::Click(Point::new(a, 9.)));
                    editor.perform(Action::Drag(Point::new(b, 9.)));
                    assert_eq!(editor.copy().as_deref(), Some(expected));
                    let rectangles = selection_rectangles(&editor);
                    assert_eq!(rectangles.len(), 1);
                    assert!((rectangles[0].x - a.min(b)).abs() < 0.01);
                    assert!((rectangles[0].width - half).abs() < 0.01);
                }
            }
        }
    }

    #[test]
    fn ligature_selection_keeps_disjoint_bidi_regions_and_visual_row_offsets() {
        use iced::advanced::text::{
            Editor as _,
            editor::{Cursor, Position},
        };
        let mut editor = shaped_editor("fi אב fi", 100., 100.);
        let glyphs = editor.buffer().layout_runs().next().unwrap().glyphs;
        let fi = glyphs.iter().find(|glyph| glyph.start == 0).unwrap();
        let space = glyphs.iter().find(|glyph| glyph.start == 2).unwrap();
        let alef = glyphs.iter().find(|glyph| glyph.start == 3).unwrap();
        let expected = [
            (fi.x + fi.w / 2., space.x + space.w),
            (alef.x, alef.x + alef.w),
        ];
        editor.move_to(Cursor {
            position: Position { line: 0, column: 5 },
            selection: Some(Position { line: 0, column: 1 }),
        });
        assert_eq!(editor.copy().as_deref(), Some("i א"));
        let rectangles = selection_rectangles(&editor);
        assert_eq!(
            rectangles.len(),
            2,
            "do not highlight the unselected bet between the spans"
        );
        for (rectangle, (left, right)) in rectangles.iter().zip(expected) {
            assert!((rectangle.x - left).abs() < 0.01);
            assert!((rectangle.width - (right - left)).abs() < 0.01);
            assert_eq!(rectangle.y, 0.);
        }

        let mut editor = shaped_editor("prefix\nfi fi\n\nfi", 20., 100.);
        assert_eq!(
            editor
                .buffer()
                .layout_runs()
                .map(|run| run.line_i)
                .collect::<Vec<_>>(),
            [0, 1, 1, 2, 3]
        );
        let half = editor.buffer().layout_runs().last().unwrap().glyphs[0].w / 2.;
        editor.move_to(Cursor {
            position: Position { line: 3, column: 1 },
            selection: Some(Position { line: 1, column: 4 }),
        });
        assert_eq!(editor.copy().as_deref(), Some("i\n\nf"));
        let rectangles = selection_rectangles(&editor);
        assert_eq!(rectangles.len(), 2);
        for (rectangle, (x, y)) in rectangles
            .iter()
            .zip([(half, 2. * LINE_HEIGHT), (0., 4. * LINE_HEIGHT)])
        {
            assert!((rectangle.x - x).abs() < 0.01);
            assert!((rectangle.width - half).abs() < 0.01);
            assert_eq!(rectangle.y, y);
        }
        layout_shaped_editor(&mut editor, 20., 39.);
        let scrolled = selection_rectangles(&editor);
        assert_eq!(scrolled.len(), 2);
        for (before, after) in rectangles.iter().zip(scrolled) {
            assert_eq!(after.x, before.x);
            assert_eq!(after.width, before.width);
            assert_eq!(after.y, before.y - 3. * LINE_HEIGHT);
        }
    }

    #[test]
    fn large_paste_and_seeks_shape_only_the_destination_viewport() {
        let value = (0..4000)
            .map(|i| {
                if i % 19 == 0 {
                    format!("Wrapped line {i}: {}", "asymmetric λ words ".repeat(40))
                } else {
                    format!("Short line {i}: λ")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        for mounted in [false, true] {
            let mut editor = Editor::new("");
            if mounted {
                editor.layout(400.);
            }
            editor
                .content
                .perform(Action::Edit(Edit::Paste(std::sync::Arc::new(
                    value.clone(),
                ))));
            let mut node = editor.layout(400.);
            let cached = |editor: &Editor| {
                editor.content.with_editor(|editor| {
                    editor
                        .buffer()
                        .lines
                        .iter()
                        .filter(|line| line.layout_opt().is_some())
                        .count()
                })
            };
            assert!(
                cached(&editor) < 64,
                "paste must not shape the offscreen document"
            );
            assert!(
                editor
                    .content
                    .with_editor(|editor| editor.buffer().scroll().line)
                    > 3900
            );
            let cursor = editor.content.cursor();
            for fraction in [0., 0.5, 1.] {
                let before = cached(&editor);
                let bar = editor.bar(&node).unwrap();
                let point = Point::new(
                    bar.track.center_x(),
                    bar.track.y + (fraction * bar.track.height).min(bar.track.height - 0.5),
                );
                editor.events(&node, &[(press(), point), (release(), point)]);
                node = editor.layout(400.);
                assert!(
                    cached(&editor) <= before + 64,
                    "seek must not shape intervening lines"
                );
                assert_eq!(editor.content.cursor(), cursor);
                let bar = editor.bar(&node).unwrap();
                if fraction == 0. {
                    assert_eq!(bar.thumb.y, bar.track.y);
                } else if fraction == 1. {
                    assert!(
                        (bar.thumb.y + bar.thumb.height - bar.track.y - bar.track.height).abs()
                            < 0.01
                    );
                } else {
                    let line = editor
                        .content
                        .with_editor(|editor| editor.buffer().scroll().line);
                    assert!(
                        (800..3200).contains(&line),
                        "middle seek must reach the document interior"
                    );
                }
            }
            assert_eq!(editor.content.text(), value);
        }

        // Opening a persisted draft must also defer shaping until layout.
        let content: Content = Content::with_text(&value);
        content.with_editor(|editor| {
            assert!(
                editor
                    .buffer()
                    .lines
                    .iter()
                    .all(|line| line.layout_opt().is_none())
            )
        });
    }

    #[test]
    fn focused_draw_after_seek_and_reflow_does_not_shape_or_move_offscreen_caret() {
        use iced::advanced::graphics::core::{InputMethod, input_method};
        use iced::advanced::{Renderer as _, text::Editor as _};
        let mut editor = Editor::new(&"A wrapped line with asymmetric words λ.\n".repeat(1000));
        let node = editor.layout(400.);
        editor.events(
            &node,
            &[
                (press(), Point::new(35., 20.)),
                (release(), Point::new(35., 20.)),
            ],
        );
        // Editing invalidates the caret line. A following seek in the same
        // batch cancels cursor reveal before the next layout.
        editor.content.perform(Action::Edit(Edit::Insert('x')));
        let cursor = editor.content.cursor();
        ScrollTo {
            line: 1000,
            fraction: 1.,
        }
        .apply(&mut editor.content);
        editor.layout(400.);
        let node = editor.layout(180.);
        editor.content.with_editor(|native| {
            assert!(
                native.buffer().lines[cursor.position.line]
                    .layout_opt()
                    .is_none()
            );
        });
        let bounds = node.bounds();
        editor.renderer.reset(bounds);
        view(&editor.content).as_widget().draw(
            &editor.tree,
            &mut editor.renderer,
            &theme::app(),
            &renderer::Style {
                text_color: theme::TEXT,
            },
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &bounds,
        );
        editor.content.with_editor(|native| {
            assert!(native.buffer().lines[cursor.position.line].layout_opt().is_none(),
                "drawing must not shape the offscreen caret line");
            assert!(matches!(native.selection(), iced::advanced::text::editor::Selection::Range(regions) if regions.is_empty()),
                "an offscreen caret has no visible paint geometry");
        });
        // Offscreen paint geometry must not disable input-method composition.
        {
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            view(&editor.content).as_widget_mut().update(
                &mut editor.tree,
                &Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                )),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &editor.renderer,
                &mut clipboard::Null,
                &mut shell,
                &bounds,
            );
            assert!(
                matches!(shell.input_method(), InputMethod::Enabled { cursor, .. }
                if bounds.contains(cursor.position())),
                "IME needs a bounded enabled anchor"
            );
        }
        assert_eq!(editor.content.cursor(), cursor);
        let pixels = crate::test_renderer::screenshot(
            &mut editor.renderer,
            Size::new(bounds.width.ceil() as u32, bounds.height.ceil() as u32),
            1.,
            theme::BACKGROUND,
        );
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel != &pixels[..4])
        );
        let mut expected = editor.content.text();
        assert_eq!(cursor.position.line, 0);
        expected.insert(cursor.position.column, '界');
        editor.events(
            &node,
            &[
                (
                    Event::InputMethod(input_method::Event::Opened),
                    Point::ORIGIN,
                ),
                (
                    Event::InputMethod(input_method::Event::Preedit("界".into(), None)),
                    Point::ORIGIN,
                ),
                (
                    Event::InputMethod(input_method::Event::Commit("界".into())),
                    Point::ORIGIN,
                ),
            ],
        );
        editor.layout(180.);
        assert_eq!(editor.content.text(), expected);
        editor.content.with_editor(|native| {
            assert_eq!(
                native.buffer().scroll().line,
                0,
                "IME commit must reveal the caret"
            );
            assert!(
                native
                    .buffer()
                    .lines
                    .iter()
                    .filter(|line| line.layout_opt().is_some())
                    .count()
                    < 64
            );
        });
    }

    #[test]
    fn bulk_paste_preserves_middle_lines_suffix_endings_and_selection_replacement() {
        for inserted in ["first\r\nλ界\r\nlast", "first\nλ界\nlast\n"] {
            let mut editor = Editor::new("prefix-REPLACE-tail\nuntouched λ");
            editor.content.move_to(text_editor::Cursor {
                position: text_editor::Position {
                    line: 0,
                    column: 14,
                },
                selection: Some(text_editor::Position { line: 0, column: 7 }),
            });
            editor
                .content
                .perform(Action::Edit(Edit::Paste(std::sync::Arc::new(
                    inserted.into(),
                ))));
            editor.layout(400.);
            // The original first line's LF ending still terminates the suffix;
            // inserted interior lines retain their CRLF/LF endings.
            let expected = format!("prefix-{inserted}-tail\nuntouched λ");
            assert_eq!(editor.content.text(), expected);
            assert!(editor.content.cursor().selection.is_none());
            editor.content.perform(Action::Edit(Edit::Insert('!')));
            assert_eq!(
                editor.content.text(),
                format!("prefix-{inserted}!-tail\nuntouched λ")
            );
        }
    }

    #[test]
    fn editor_reserves_the_same_gutter_before_and_after_overflow() {
        let mut editor = Editor::new("Short draft");
        for value in [
            "Short draft".to_owned(),
            "Wrapped draft words. ".repeat(200),
            "Short again".to_owned(),
        ] {
            editor.content = Content::with_text(&value);
            let node = editor.layout(300.);
            let text_width = editor
                .content
                .with_editor(|editor| editor.buffer().size().0.unwrap());
            assert_eq!(
                text_width,
                node.size().width - PADDING - scrollbar::GUTTER - theme::SCROLLBAR_EDGE_INSET
            );
            if let Some(bar) = editor.bar(&node) {
                assert_eq!(bar.track.x - (PADDING + text_width), theme::SCROLLBAR_GAP);
                assert_eq!(
                    node.size().width - (bar.track.x + bar.track.width),
                    theme::SCROLLBAR_EDGE_INSET
                );
            }
        }
    }

    #[test]
    fn scrollbar_keeps_default_cursor_without_changing_text_cursor() {
        let mut editor = Editor::new(&"A line of text\n".repeat(30));
        let node = editor.layout(400.);
        let bar = editor.bar(&node).unwrap();
        let text = Point::new(35., 20.);
        for point in [bar.thumb.center(), bar.track.center()] {
            assert_eq!(editor.interaction(&node, point), mouse::Interaction::None);
        }
        assert_eq!(editor.interaction(&node, text), mouse::Interaction::Text);

        editor.events(&node, &[(press(), bar.thumb.center())]);
        // Keep the arrow even when a drag leaves the scrollbar's hit area.
        assert_eq!(editor.interaction(&node, text), mouse::Interaction::None);
        editor.events(&node, &[(release(), text)]);
        assert_eq!(editor.interaction(&node, text), mouse::Interaction::Text);
    }

    #[test]
    fn scrollbar_tracks_drag_wheel_keyboard_and_shrinking_without_moving_selection() {
        let value = (0..30)
            .map(|i| format!("Line {i}: λ"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = Editor::new(&value);
        let node = editor.layout(400.);
        let bar = editor.bar(&node).unwrap();
        assert_eq!(
            bar.thumb.height / bar.track.height,
            VISIBLE_ROWS as f32 / editor.content.line_count() as f32
        );
        assert_eq!(bar.thumb.y, bar.track.y);

        // Focus the real editor before grabbing its scrollbar.
        editor.events(
            &node,
            &[
                (press(), Point::new(35., 20.)),
                (release(), Point::new(35., 20.)),
            ],
        );
        editor.content.perform(Action::Select(Motion::End));
        let node = editor.layout(400.);
        let cursor = editor.content.cursor();
        assert!(cursor.selection.is_some());
        let end = Point::new(bar.thumb.center_x(), bar.track.y + bar.track.height);
        let messages = editor.events(
            &node,
            &[
                (press(), bar.thumb.center()),
                (
                    Event::Mouse(mouse::Event::CursorMoved { position: end }),
                    end,
                ),
                // Repeated motion in one event batch must not compound relative scroll.
                (
                    Event::Mouse(mouse::Event::CursorMoved { position: end }),
                    end,
                ),
                (release(), end),
            ],
        );
        assert_eq!(
            messages,
            [Message::Scroll(ScrollTo {
                line: editor.content.line_count() - 1,
                fraction: 1.,
            })]
        );
        assert_eq!(editor.content.cursor(), cursor);
        let node = editor.layout(400.);
        let bar = editor.bar(&node).unwrap();
        assert_eq!(
            bar.thumb.y + bar.thumb.height,
            bar.track.y + bar.track.height
        );

        editor.events(
            &node,
            &[(
                Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Lines { x: 0., y: 3. },
                }),
                Point::new(35., 20.),
            )],
        );
        let node = editor.layout(400.);
        let moved = editor.bar(&node).unwrap();
        assert!(moved.thumb.y < bar.thumb.y && moved.thumb.y > moved.track.y);

        let messages = editor.events(
            &node,
            &[(
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::End),
                    modified_key: keyboard::Key::Named(keyboard::key::Named::End),
                    physical_key: keyboard::key::Physical::Code(keyboard::key::Code::End),
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::CTRL,
                    text: None,
                    repeat: false,
                }),
                Point::new(35., 20.),
            )],
        );
        assert_eq!(
            messages,
            [Message::Editor(Action::Move(Motion::DocumentEnd))]
        );
        let node = editor.layout(400.);
        let bar = editor.bar(&node).unwrap();
        assert_eq!(
            bar.thumb.y + bar.thumb.height,
            bar.track.y + bar.track.height
        );

        let top = Point::new(bar.thumb.center_x(), bar.track.y);
        editor.events(&node, &[(press(), top), (release(), top)]);
        let node = editor.layout(400.);
        let bar = editor.bar(&node).unwrap();
        assert_eq!(bar.thumb.y, bar.track.y);
        assert_eq!(editor.content.text(), value);

        editor.content.perform(Action::SelectAll);
        editor.content.perform(Action::Edit(Edit::Backspace));
        let node = editor.layout(400.);
        assert_eq!(node.size().height, 2. * LINE_HEIGHT + 2. * PADDING);
        assert!(editor.bar(&node).is_none());
    }

    #[test]
    fn mixed_wheel_seek_batches_and_fractional_viewports_reach_the_requested_position() {
        let mut editor = Editor::new(&["line"; 30].join("\n"));
        let node = editor.layout(400.);
        let bar = editor.bar(&node).unwrap();
        let center = bar.track.center();
        editor.events(
            &node,
            &[
                (
                    Event::Mouse(mouse::Event::WheelScrolled {
                        delta: mouse::ScrollDelta::Lines { x: 0., y: -1. },
                    }),
                    Point::new(30., 30.),
                ),
                (press(), center),
                (release(), center),
            ],
        );
        let node = editor.layout(400.);
        let bar = editor.bar(&node).unwrap();
        assert_eq!(bar.thumb.center_y(), bar.track.center_y());

        let node = editor.layout_in(400., 212.);
        let bar = editor.bar(&node).unwrap();
        let bottom = Point::new(bar.track.center_x(), bar.track.y + bar.track.height - 1.);
        editor.events(&node, &[(press(), bottom), (release(), bottom)]);
        let node = editor.layout_in(400., 212.);
        let bar = editor.bar(&node).unwrap();
        assert_eq!(
            bar.thumb.y + bar.thumb.height,
            bar.track.y + bar.track.height
        );
        let top = Point::new(bar.track.center_x(), bar.track.y);
        editor.events(&node, &[(press(), top), (release(), top)]);
        let node = editor.layout_in(400., 212.);
        let bar = editor.bar(&node).unwrap();
        assert_eq!(bar.thumb.y, bar.track.y);
    }

    #[test]
    fn scrollbar_uses_wrapped_rows_and_disappears_at_the_limit_or_after_reflow() {
        let mut editor = Editor::new(&["line"; VISIBLE_ROWS].join("\n"));
        let node = editor.layout(400.);
        assert!(editor.bar(&node).is_none());
        editor.content.perform(Action::Move(Motion::DocumentEnd));
        editor.content.perform(Action::Edit(Edit::Enter));
        let node = editor.layout(400.);
        assert!(editor.bar(&node).is_some());

        let value = "A longer sentence with Unicode λ and words. ".repeat(8);
        editor.content = Content::with_text(&value);
        let node = editor.layout(110.);
        assert_eq!(editor.content.line_count(), 1);
        let bar = editor.bar(&node).unwrap();
        assert!(bar.thumb.height < bar.track.height / 2.);
        let bottom = Point::new(bar.thumb.center_x(), bar.track.y + bar.track.height - 1.);
        editor.events(&node, &[(press(), bottom), (release(), bottom)]);
        let node = editor.layout(110.);
        let bar = editor.bar(&node).unwrap();
        assert_eq!(
            bar.thumb.y + bar.thumb.height,
            bar.track.y + bar.track.height
        );

        let node = editor.layout(2400.);
        assert!(editor.bar(&node).is_none());
        assert_eq!(editor.content.text(), value);
    }
}

#[cfg(test)]
#[path = "growing_editor_benchmark.rs"]
mod benchmark;

#[cfg(test)]
#[path = "editor_shaping_tests.rs"]
mod shaping_tests;
