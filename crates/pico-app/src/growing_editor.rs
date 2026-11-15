//! A bounded, growing editor with a scrollbar driven by its actual shaped text.
use iced::{
    Element, Event, Length, Rectangle, Size, Theme,
    advanced::{
        Clipboard, Layout, Renderer as _, Shell, Widget,
        graphics::text as graphics_text,
        layout, renderer,
        text::highlighter::PlainText,
        widget::{Operation, Tree, tree},
    },
    mouse,
    widget::{TextEditor, text_editor},
};

pub fn growing_editor<'a, Message: Clone + 'a>(
    editor: TextEditor<'a, PlainText, Message>,
    content: &'a text_editor::Content,
    max_height: f32,
    on_action: impl Fn(text_editor::Action) -> Message + 'a,
    scroll_lines: fn(&Message) -> Option<i32>,
) -> Element<'a, Message> {
    Element::new(GrowingEditor {
        editor: editor.height(Length::Shrink).max_height(max_height),
        content,
        max_height,
        on_action: Box::new(on_action),
        scroll_lines,
    })
}

struct GrowingEditor<'a, Message> {
    editor: TextEditor<'a, PlainText, Message>,
    content: &'a text_editor::Content,
    max_height: f32,
    on_action: Box<dyn Fn(text_editor::Action) -> Message + 'a>,
    scroll_lines: fn(&Message) -> Option<i32>,
}

#[derive(Default)]
struct State {
    // Cache offscreen line layouts too: a logical line may wrap into many rows.
    lines: Vec<graphics_text::cosmic_text::BufferLine>,
    key: Option<(
        Option<f32>,
        graphics_text::cosmic_text::Metrics,
        graphics_text::Version,
    )>,
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
            let mut fonts = graphics_text::font_system().write().expect("font system");
            let key = (buffer.size().0, metrics, fonts.version());
            if self.key != Some(key) {
                self.lines.clear();
                self.key = Some(key);
            }
            self.lines.truncate(buffer.lines.len());
            let mut rows = 0;
            let mut offset_rows = 0;
            for (i, line) in buffer.lines.iter().enumerate() {
                if i == self.lines.len() {
                    self.lines.push(line.clone());
                } else if self.lines[i].text() != line.text()
                    || self.lines[i].attrs_list() != line.attrs_list()
                {
                    self.lines[i] = line.clone();
                }
                let count = self.lines[i]
                    .layout(
                        fonts.raw(),
                        metrics.font_size,
                        buffer.size().0,
                        buffer.wrap(),
                        buffer.monospace_width(),
                        buffer.tab_width(),
                    )
                    .len();
                rows += count;
                if i < buffer.scroll().line {
                    offset_rows += count;
                }
            }
            self.line_height = metrics.line_height;
            self.content_height = rows as f32 * metrics.line_height;
            self.offset = offset_rows as f32 * metrics.line_height + buffer.scroll().vertical;
        });
    }

    fn scrollbar(&self, bounds: Rectangle) -> Option<Scrollbar> {
        let track = Rectangle {
            x: bounds.x + bounds.width - 12.,
            y: bounds.y + 12.,
            width: 12.,
            height: (bounds.height - 24.).max(0.),
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
                x: track.x + 3.,
                y: track.y + (track.height - height) * (self.offset / max_offset).clamp(0., 1.),
                width: 6.,
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
                let delta = (target - state.offset) / state.line_height;
                // Endpoints must reach the native clamp even when a short
                // window leaves a fractional number of visible text rows.
                let lines = if top <= bar.track.y {
                    delta.floor()
                } else if top >= bar.track.y + travel {
                    delta.ceil()
                } else {
                    delta.round()
                } as i32;
                if lines != 0 {
                    shell.publish((self.on_action)(text_editor::Action::Scroll { lines }));
                    // Events may be batched before the next layout; don't apply
                    // the same relative scroll repeatedly in that batch.
                    state.offset += lines as f32 * state.line_height;
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
    use iced::{Point, advanced::clipboard, keyboard};
    use text_editor::{Action, Content, Edit, Motion};

    fn view(content: &Content) -> Element<'_, Action> {
        growing_editor(
            text_editor(content)
                .size(15)
                .line_height(iced::widget::text::LineHeight::Absolute(19.5.into()))
                .min_height(39)
                .padding(12)
                .on_action(|action| action),
            content,
            219.,
            |action| action,
            |action| match action {
                Action::Scroll { lines } => Some(*lines),
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
                renderer: iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                    iced::Font::DEFAULT,
                    iced::Pixels(16.),
                )),
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

        fn events(&mut self, node: &layout::Node, events: &[(Event, Point)]) -> Vec<Action> {
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
                self.content.perform(message.clone());
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
        assert_eq!(bar.thumb.height, 65.);
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
        assert_eq!(messages, [Action::Scroll { lines: 20 }]);
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
        assert_eq!(messages, [Action::Move(Motion::DocumentEnd)]);
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
        assert_eq!(node.size().height, 63.);
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
        let mut editor = Editor::new(&["line"; 10].join("\n"));
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
