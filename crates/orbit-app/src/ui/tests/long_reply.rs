//! Drive the real transcript, including resize observations and the scroll
//! corrections they return. No GPU submission/readback in the timing probe.
use super::*;
use iced::advanced::{
    clipboard,
    widget::{Operation, operation::scrollable::snap_to},
};
use iced::futures::StreamExt;
use iced_runtime::{UserInterface, user_interface::Cache};

fn fixture(source: &str) -> App {
    let mut app = app();
    let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
    session
        .entries
        .push(Entry::new(EntryKind::Assistant, source));
    app.selected = Some(session.id.clone());
    app.current_session = Some(session);
    app.rebuild_transcript();
    app.auto_scroll = false;
    app
}

#[derive(Default)]
struct Viewport {
    bounds: iced::Rectangle,
    dy: f32,
}
impl Operation for Viewport {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
        visit(self);
    }
    fn scrollable(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        bounds: iced::Rectangle,
        _: iced::Rectangle,
        translation: iced::Vector,
        _: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if id == Some(&"chat".into()) {
            self.bounds = bounds;
            self.dy = translation.y;
        }
    }
}

async fn settle(
    app: &mut App,
    renderer: &mut iced::Renderer,
    cache: &mut Cache,
    size: Size,
    jump: Option<f32>,
) {
    let mut ops: Vec<Box<dyn Operation>> = Vec::new();
    if let Some(y) = jump {
        ops.push(Box::new(snap_to(
            "chat".into(),
            scrollable::RelativeOffset {
                x: None,
                y: Some(y),
            },
        )));
    }
    for _ in 0..20 {
        let mut ui = UserInterface::build(app.center(), size, std::mem::take(cache), renderer);
        for mut op in ops.drain(..) {
            ui.operate(renderer, op.as_mut());
        }
        let mut messages = vec![];
        ui.update(
            &[iced::Event::Window(window::Event::RedrawRequested(
                iced::time::Instant::now(),
            ))],
            mouse::Cursor::Unavailable,
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        *cache = ui.into_cache();
        let mut geometry_changed = false;
        for message in messages {
            if !matches!(
                message,
                Message::MarkdownGeometry(..)
                    | Message::RowSize(..)
                    | Message::Scroll(..)
                    | Message::Find(find::Action::Located { .. })
            ) {
                continue;
            }
            geometry_changed = true;
            if let Some(mut stream) = iced_runtime::task::into_stream(app.update_inner(message)) {
                while let Some(action) = stream.next().await {
                    if let iced_runtime::Action::Widget(op) = action {
                        ops.push(op);
                    }
                }
            }
        }
        if !geometry_changed {
            return;
        }
    }
    panic!("transcript geometry did not settle");
}

#[tokio::test]
async fn large_reply_reaches_tail_and_copies_across_unmounted_mixed_blocks() {
    let mut source = String::from(
        "First\n\n> quoted\n\n- café\n- two\n\n---\n\n```rs\nline1\nline2\n```\n\n| H | I |\n|---|---|\n| C | D |\n\n",
    );
    let mut expected =
        String::from("First\n\nquoted\n\ncafé\n\ntwo\n\nline1\nline2\n\nH\n\nI\n\nC\n\nD");
    let count = 300;
    for i in 0..count {
        let wrap = if i >= count - 32 {
            " wraps at the narrow width".repeat(18)
        } else {
            String::new()
        };
        source.push_str(&format!("**Item {i}**{wrap}\n\n"));
        expected.push_str(&format!("\n\nItem {i}{wrap}"));
    }
    source.push_str("# Tail marker");
    let mut app = fixture(&source);
    app.prepare_visible();
    let mut renderer = header_renderer();
    let mut cache = Cache::new();
    let mut top = None;
    for (location, width) in [(0., 720.), (0.52, 720.), (0.52, 530.), (1., 530.)] {
        let size = Size::new(width, 520.);
        let shaped = crate::chat_selection::tests::shaped_paragraphs();
        settle(&mut app, &mut renderer, &mut cache, size, Some(location)).await;
        let mut ui = UserInterface::build(
            app.center(),
            size,
            std::mem::take(&mut cache),
            &mut renderer,
        );
        let mut labels = UiBounds::default();
        let mut viewport = Viewport::default();
        ui.operate(&renderer, &mut labels);
        ui.operate(&renderer, &mut viewport);
        let entered = crate::chat_selection::tests::shaped_paragraphs() - shaped;
        assert!(
            entered < count / 2,
            "shaped {entered} of {count} paragraphs"
        );
        let visible: Vec<_> = labels
            .labels
            .iter()
            .filter_map(|(label, b)| {
                let point = iced::Point::new(b.x + 0.2, b.center().y - viewport.dy);
                viewport
                    .bounds
                    .contains(point)
                    .then_some((label.as_str(), point))
            })
            .collect();
        if location == 0. || location == 1. {
            let label = if location == 0. {
                "First"
            } else {
                "Tail marker"
            };
            let (_, point) = visible
                .iter()
                .find(|(s, _)| *s == label)
                .unwrap_or_else(|| panic!("{label} not visible: {visible:?}"));
            click_at(&mut ui, &mut renderer, *point);
            let at = app.chat_selection.borrow().anchor.expect("text hit");
            assert_eq!(at.byte, 0);
            if location == 0. {
                top = Some(at);
            } else {
                let mut selection = app.chat_selection.borrow_mut();
                selection.anchor = top;
                selection.focus = Some(at);
            }
        } else {
            assert!(
                visible.iter().any(|(s, _)| s.starts_with("Item ")),
                "blank middle"
            );
        }
        cache = ui.into_cache();
    }
    assert_eq!(
        copied_text(app.update_inner(Message::CopySelection)).await,
        expected + "\n\n"
    );
}

#[tokio::test]
#[ignore = "manual release CPU benchmark; run alone"]
async fn long_reply_layout_benchmark() {
    let source: String = (0..2000).map(|i| format!("Section {i}: scrolling **formatted text** with `inline code`, a [link](https://example.com), and Unicode café λ界. This paragraph describes an ordinary document with several words.\n\n")).collect();
    let mut renderer = header_renderer();
    for sample in 0..4 {
        for eager in [sample % 2 == 0, sample % 2 != 0] {
            let mut app = fixture(&source);
            let mut cache = Cache::new();
            let start = std::time::Instant::now();
            app.prepare_visible();
            if eager {
                app.markdown.values_mut().next().unwrap().window = None;
            }
            let prepare = start.elapsed();
            for (phase, width, jump) in [
                ("mount", 760., Some(0.)),
                ("middle", 760., Some(0.5)),
                ("reflow", 510., None),
                ("end", 510., Some(1.)),
            ] {
                let start = std::time::Instant::now();
                settle(
                    &mut app,
                    &mut renderer,
                    &mut cache,
                    Size::new(width, 600.),
                    jump,
                )
                .await;
                println!(
                    "LONG_REPLY eager={eager} sample={sample} phase={phase} elapsed_ms={:.3} prepare_ms={:.3}",
                    start.elapsed().as_secs_f64() * 1000.,
                    prepare.as_secs_f64() * 1000.
                );
            }
        }
    }
}

#[tokio::test]
async fn returning_to_measured_reply_preserves_visible_text_and_scroll_anchor() {
    let source: String = (0..160)
        .map(|i| {
            format!(
                "Paragraph {i}: {}\n\n",
                "Unicode λ and wrapping text. ".repeat(1 + i % 24)
            )
        })
        .collect();
    // A measured prefix that will revert to an estimate on a session switch.
    let mut app = fixture(&"Earlier line.  \n".repeat(24));
    let session = app.current_session.as_mut().unwrap();
    let reply = Entry::new(EntryKind::Assistant, source);
    let id = reply.id.clone();
    session.entries.push(reply);
    for i in 0..90 {
        session
            .entries
            .push(Entry::new(EntryKind::Assistant, format!("Following {i}")));
    }
    let session_id = session.id.clone();
    let h = app.harness.as_ref().unwrap().clone();
    h.store.save_session(session).unwrap();
    h.insert(session.clone());
    app.rebuild_transcript();
    app.prepare_visible();
    let mut renderer = header_renderer();
    let size = Size::new(525., 540.);
    let mut cache = Cache::new();
    // Read several non-uniform sections so the row has real measurements on
    // both sides of the eventual anchor. Use absolute wheel-like offsets.
    for offset in [0., 950., 2400., 4100.] {
        move_to(&app, &mut renderer, &mut cache, size, offset);
        settle(&mut app, &mut renderer, &mut cache, size, None).await;
    }
    let (offset, (label, y)) = visible_text(&app, &mut renderer, &mut cache, size);
    settle(&mut app, &mut renderer, &mut cache, size, Some(1.)).await;
    assert!(
        !app.markdown.contains_key(&id),
        "reply must really be unmounted"
    );
    move_to(&app, &mut renderer, &mut cache, size, offset);
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    let (_, (restored, restored_y)) = visible_text(&app, &mut renderer, &mut cache, size);
    assert_eq!(restored, label);
    assert!(
        (restored_y - y).abs() < 1.,
        "same text moved from {y} to {restored_y}"
    );

    // Unlike scrolling away, switching sessions tears down the height indexes.
    let other = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
    let other_id = other.id.clone();
    h.store.save_session(&other).unwrap();
    for id in [other_id, session_id] {
        drive_messages(
            &mut app,
            &mut renderer,
            &mut cache,
            size,
            vec![Message::Select(id)],
        )
        .await;
        settle(&mut app, &mut renderer, &mut cache, size, None).await;
    }
    assert!(!app.auto_scroll);
    let (_, (restored, restored_y)) = visible_text(&app, &mut renderer, &mut cache, size);
    assert_eq!(restored, label);
    assert!((restored_y - y).abs() < 1.);
}

async fn drive_messages(
    app: &mut App,
    renderer: &mut iced::Renderer,
    cache: &mut Cache,
    size: Size,
    messages: Vec<Message>,
) {
    drive_ui_messages(App::center, app, renderer, cache, size, messages).await;
}

fn move_to(app: &App, renderer: &mut iced::Renderer, cache: &mut Cache, size: Size, y: f32) {
    let mut ui = UserInterface::build(app.center(), size, std::mem::take(cache), renderer);
    ui.operate(
        renderer,
        &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
            "chat".into(),
            scrollable::AbsoluteOffset {
                x: None,
                y: Some(y),
            },
        ),
    );
    *cache = ui.into_cache();
}
fn visible_text(
    app: &App,
    renderer: &mut iced::Renderer,
    cache: &mut Cache,
    size: Size,
) -> (f32, (String, f32)) {
    let mut ui = UserInterface::build(app.center(), size, std::mem::take(cache), renderer);
    let mut viewport = Viewport::default();
    let mut labels = UiBounds::default();
    ui.operate(renderer, &mut labels);
    ui.operate(renderer, &mut viewport);
    let first = labels
        .labels
        .into_iter()
        .find_map(|(label, b)| {
            let y = b.y - viewport.dy;
            (label.starts_with("Paragraph ")
                && y < viewport.bounds.y + viewport.bounds.height
                && y + b.height > viewport.bounds.y)
                .then_some((label, y))
        })
        .unwrap();
    *cache = ui.into_cache();
    (viewport.dy, first)
}

fn streaming_fixture(source: &str) -> App {
    let mut app = fixture(source);
    let session = app.current_session.take().unwrap();
    app.harness.as_ref().unwrap().insert(session);
    app.refresh();
    app
}

async fn append_text(
    app: &mut App,
    renderer: &mut iced::Renderer,
    cache: &mut Cache,
    size: Size,
    text: &str,
) {
    app.harness
        .as_ref()
        .unwrap()
        .edit(app.selected.as_ref().unwrap(), |s| {
            s.entries[0].text.push_str(text)
        })
        .unwrap();
    drive_messages(app, renderer, cache, size, vec![Message::Changed]).await;
    settle(app, renderer, cache, size, None).await;
}

#[tokio::test]
async fn image_paste_preserves_tail_following_and_scrolled_up_position() {
    let source: String = (0..60).map(|i| format!("Paragraph {i}\n\n")).collect();
    let mut app = streaming_fixture(&source);
    let image = orbit_core::media::MediaStore::new(app.smoke_dir.as_ref().unwrap().join("images"))
        .image(include_bytes!("../../fixtures/media.png"))
        .unwrap();
    let size = Size::new(620., 520.);
    let mut renderer = header_renderer();
    let mut cache = Cache::new();
    settle(&mut app, &mut renderer, &mut cache, size, Some(1.)).await;
    let (end, _) = visible_text(&app, &mut renderer, &mut cache, size);
    let full_height = app.transcript.viewport;

    for (offset, following) in [(end, true), (end / 2., false)] {
        // Wheel scrolling leaves an absolute offset, even at the bottom.
        move_to(&app, &mut renderer, &mut cache, size, offset);
        settle(&mut app, &mut renderer, &mut cache, size, None).await;
        assert_eq!(app.auto_scroll, following);

        let _ = app.update(Message::Paste);
        dispatch(
            &mut app,
            Message::AttachmentLoaded(Ok(crate::attachments::Input::Image(image.clone()))),
        )
        .await;
        settle(&mut app, &mut renderer, &mut cache, size, None).await;
        let (after, _) = visible_text(&app, &mut renderer, &mut cache, size);
        let shrink = full_height - app.transcript.viewport;
        assert!(shrink > 0., "the image strip must shrink the chat viewport");
        assert_eq!(app.auto_scroll, following);
        let expected = if following { end + shrink } else { offset };
        assert!((after - expected).abs() <= 1.);

        dispatch(&mut app, Message::RemoveAttachment(0)).await;
        settle(&mut app, &mut renderer, &mut cache, size, None).await;
    }
}

#[tokio::test]
async fn streaming_pauses_on_scroll_and_resumes_on_jump() {
    let source: String = (0..12)
        .map(|i| {
            format!(
                "Paragraph {i}: {}\n\n",
                "Long wrapped lines λ. ".repeat(240)
            )
        })
        .collect();
    let mut app = streaming_fixture(&source);
    let mut cache = Cache::new();
    let mut renderer = header_renderer();
    let size = Size::new(525., 540.);
    settle(&mut app, &mut renderer, &mut cache, size, Some(1.)).await;
    let (bottom, _) = visible_text(&app, &mut renderer, &mut cache, size);
    move_to(&app, &mut renderer, &mut cache, size, bottom / 2.);
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    let (_, (label, y)) = visible_text(&app, &mut renderer, &mut cache, size);
    append_text(
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        &"Next block.\n\n".repeat(160),
    )
    .await;
    let (_, (now, screen_y)) = visible_text(&app, &mut renderer, &mut cache, size);
    assert_eq!(label, now);
    assert!((screen_y - y).abs() < 1.);
    assert!(
        !app.auto_scroll,
        "layout changes must not resume following the tail"
    );

    let mut ui = UserInterface::build(
        app.center(),
        size,
        std::mem::take(&mut cache),
        &mut renderer,
    );
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let chat = bounds.scroll("chat");
    let jump = bounds
        .containers
        .iter()
        .find(|b| {
            b.width == b.height
                && b.width > 0.
                && b.is_within(&chat)
                && (b.center_x() - chat.center_x()).abs() < 1.
                && b.center_y() > chat.center_y()
        })
        .expect("round control centered above composer")
        .center();
    let messages = click_at(&mut ui, &mut renderer, jump);
    cache = ui.into_cache();
    drive_messages(&mut app, &mut renderer, &mut cache, size, messages).await;
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    assert!(app.auto_scroll);

    append_text(
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        &format!("{}Stream tail.", "New tokens.\n\n".repeat(80)),
    )
    .await;
    let mut ui = UserInterface::build(app.center(), size, cache, &mut renderer);
    let mut bounds = UiBounds::default();
    let mut viewport = Viewport::default();
    ui.operate(&renderer, &mut bounds);
    ui.operate(&renderer, &mut viewport);
    let tail = bounds.label("Stream tail.").center() - iced::Vector::new(0., viewport.dy);
    assert!(chat.contains(tail), "newly streamed text must be visible");
    assert!(
        !click_at(&mut ui, &mut renderer, jump)
            .iter()
            .any(|m| matches!(m, Message::JumpToLatest))
    );
}

#[tokio::test]
async fn find_refresh_keeps_the_selection_without_scrolling_back() {
    let mut app = streaming_fixture(&format!(
        "Older match.\n\n{}",
        "Conversation.\n\n".repeat(150)
    ));
    let size = Size::new(620., 470.);
    let mut renderer = header_renderer();
    let mut cache = Cache::new();
    settle(&mut app, &mut renderer, &mut cache, size, Some(1.)).await;
    dispatch(&mut app, Message::Find(find::Action::Open)).await;
    assert!(
        app.auto_scroll,
        "opening Find should not detach from the stream"
    );
    dispatch(&mut app, Message::Find(find::Action::Query("match".into()))).await;
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    assert!(
        !app.auto_scroll,
        "revealing the older match should pause following"
    );

    // Return to the stream without changing Find's selection.
    settle(&mut app, &mut renderer, &mut cache, size, Some(1.)).await;
    append_text(
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        &format!("{}New match.", "New tokens.\n\n".repeat(80)),
    )
    .await;
    let mut ui = UserInterface::build(app.center(), size, cache, &mut renderer);
    let mut bounds = UiBounds::default();
    let mut viewport = Viewport::default();
    ui.operate(&renderer, &mut bounds);
    ui.operate(&renderer, &mut viewport);
    assert!(
        bounds.labels.iter().any(|(s, _)| s == "1 of 2"),
        "Find should count the new match but keep the old selection"
    );
    let tail = bounds.label("New match.").center() - iced::Vector::new(0., viewport.dy);
    assert!(
        viewport.bounds.contains(tail),
        "updating matches must not pull the reader back"
    );
}

#[tokio::test]
async fn find_reveals_offscreen_occurrence_inside_a_virtual_reply_and_wide_code() {
    let filler = "ordinary paragraph with **formatting**.\n\n".repeat(300);
    let wide = format!("{}second NEEDLE", "abcdef".repeat(80));
    let source = format!("first **needle**\n\n{filler}```txt\n{wide}\n```\n\n{filler}");
    let mut app = fixture("a different message");
    let session = app.current_session.as_mut().unwrap();
    for i in 0..250 {
        session
            .entries
            .push(Entry::new(EntryKind::Assistant, format!("unrelated {i}")));
    }
    session
        .entries
        .push(Entry::new(EntryKind::Assistant, source));
    app.rebuild_transcript();
    app.prepare_visible();
    let size = Size::new(680., 460.);
    let mut renderer = header_renderer();
    let mut cache = Cache::new();
    // Drive both native widget handling and the window keyboard subscription.
    async fn key(
        app: &mut App,
        renderer: &mut iced::Renderer,
        cache: &mut Cache,
        size: Size,
        key: keyboard::Key,
        modifiers: keyboard::Modifiers,
    ) {
        let event = iced::Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            modifiers,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            text: None,
            repeat: false,
        });
        let mut ui = UserInterface::build(app.center(), size, std::mem::take(cache), renderer);
        let mut messages = vec![];
        ui.update(
            std::slice::from_ref(&event),
            mouse::Cursor::Unavailable,
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        *cache = ui.into_cache();
        messages.push(Message::Event(event));
        drive_messages(app, renderer, cache, size, messages).await;
    }
    key(
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        keyboard::Key::Character("f".into()),
        keyboard::Modifiers::CTRL,
    )
    .await;
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    dispatch(
        &mut app,
        Message::Find(find::Action::Query("needle".into())),
    )
    .await;
    let shaped = crate::chat_selection::tests::shaped_paragraphs();
    for (step, label, count) in [
        (None, "first needle", "1 of 2"),
        (Some(false), wide.as_str(), "2 of 2"),
        (Some(false), "first needle", "1 of 2"),
        (Some(true), wide.as_str(), "2 of 2"),
    ] {
        if let Some(back) = step {
            key(
                &mut app,
                &mut renderer,
                &mut cache,
                size,
                keyboard::Key::Named(keyboard::key::Named::Enter),
                if back {
                    keyboard::Modifiers::SHIFT
                } else {
                    keyboard::Modifiers::NONE
                },
            )
            .await;
        }
        settle(&mut app, &mut renderer, &mut cache, size, None).await;
        let mut ui = UserInterface::build(
            app.center(),
            size,
            std::mem::take(&mut cache),
            &mut renderer,
        );
        let mut labels = UiBounds::default();
        let mut viewport = Viewport::default();
        ui.operate(&renderer, &mut labels);
        ui.operate(&renderer, &mut viewport);
        let target = labels.label(label);
        assert!(labels.labels.iter().any(|(s, _)| s == count));
        assert!(
            (viewport.bounds.y..viewport.bounds.y + viewport.bounds.height)
                .contains(&(target.center_y() - viewport.dy)),
            "target {label} outside chat: {target:?}, offset {}",
            viewport.dy
        );
        if label == wide {
            // The occurrence ends at the last glyph of a line far wider than the viewport.
            #[derive(Default)]
            struct Horizontal(Vec<(iced::Rectangle, f32)>);
            impl Operation for Horizontal {
                fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
                    visit(self)
                }
                fn scrollable(
                    &mut self,
                    _: Option<&iced::advanced::widget::Id>,
                    bounds: iced::Rectangle,
                    content: iced::Rectangle,
                    t: iced::Vector,
                    _: &mut dyn iced::advanced::widget::operation::Scrollable,
                ) {
                    if content.width > bounds.width {
                        self.0.push((bounds, t.x))
                    }
                }
            }
            let mut horizontal = Horizontal::default();
            ui.operate(&renderer, &mut horizontal);
            assert!(
                horizontal.0.iter().any(|(b, dx)| *dx > 0.
                    && (b.x..=b.x + b.width).contains(&(target.x + target.width - dx))),
                "end of code line not visible"
            );
        }
        cache = ui.into_cache();
        if step.is_none() {
            // Final response reconciliation can replace the Arc without changing
            // the text. Subsequent inner-window measurements must still apply.
            let history = &mut app.current_session.as_mut().unwrap().entries;
            let last = history.len() - 1;
            history[last] = history[last].clone();
            app.prepare_visible();
            app.find_sync();
            let task = Task::batch(app.tasks.drain(..));
            drain(&mut app, task).await;
        }
    }
    assert!(
        crate::chat_selection::tests::shaped_paragraphs() - shaped < 300,
        "jumping should not shape the whole reply"
    );
}

#[tokio::test]
async fn find_open_and_close_preserve_scrolled_transcript_and_typing_does_not_wrap() {
    let source: String = (0..150)
        .map(|i| {
            format!(
                "Paragraph {i}{}\n\n",
                if matches!(i, 2 | 70) {
                    " contains a needle"
                } else {
                    ""
                }
            )
        })
        .collect();
    let mut app = fixture(&source);
    app.prepare_visible();
    let size = Size::new(620., 470.);
    let mut cache = Cache::new();
    let mut renderer = header_renderer();
    settle(&mut app, &mut renderer, &mut cache, size, Some(0.85)).await;
    let (offset, (before, _)) = visible_text(&app, &mut renderer, &mut cache, size);
    // Use an absolute scroll (as after wheel/drag), not a relative snap that
    // intentionally moves as the viewport height changes.
    move_to(&app, &mut renderer, &mut cache, size, offset);
    assert!(
        before
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<usize>()
            .unwrap()
            > 70
    );
    dispatch(&mut app, Message::Find(find::Action::Open)).await;
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    let (opened, (first, _)) = visible_text(&app, &mut renderer, &mut cache, size);
    assert_eq!(
        first, before,
        "opening Find must not replace the scrollable"
    );
    assert!((opened - offset).abs() < 1.);
    dispatch(
        &mut app,
        Message::Find(find::Action::Query("needle".into())),
    )
    .await;
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    let mut ui = UserInterface::build(
        app.center(),
        size,
        std::mem::take(&mut cache),
        &mut renderer,
    );
    let mut labels = UiBounds::default();
    let mut viewport = Viewport::default();
    ui.operate(&renderer, &mut labels);
    ui.operate(&renderer, &mut viewport);
    assert!(labels.labels.iter().any(|(s, _)| s == "2 of 2"));
    let y = labels.label("Paragraph 70 contains a needle").center_y() - viewport.dy;
    assert!((viewport.bounds.y..viewport.bounds.y + viewport.bounds.height).contains(&y));
    cache = ui.into_cache();
    let (_, (before_close, _)) = visible_text(&app, &mut renderer, &mut cache, size);
    dispatch(&mut app, Message::Find(find::Action::Close)).await;
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    assert_eq!(
        visible_text(&app, &mut renderer, &mut cache, size).1.0,
        before_close
    );
}

#[tokio::test]
async fn find_popup_overlays_the_upper_right_and_fits_narrow_transcripts() {
    let mut app = fixture("An existing reply");
    app.prepare_visible();
    let mut renderer = header_renderer();
    let mut layout = |app: &App, width| {
        let mut ui = UserInterface::build(
            app.center(),
            Size::new(width, 520.),
            Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        (bounds.scroll("chat"), bounds.find_input)
    };
    let widths = [980., 790., 320.];
    let closed = widths.map(|width| layout(&app, width).0);
    dispatch(&mut app, Message::Find(find::Action::Open)).await;
    let fields: [iced::Rectangle; 3] = std::array::from_fn(|i| {
        let (chat, field) = layout(&app, widths[i]);
        let field = field.unwrap();
        assert_eq!(
            chat, closed[i],
            "opening Find must not take space from the transcript"
        );
        assert!(field.is_within(&chat) && field.center_y() < chat.center_y());
        field
    });
    assert!(
        fields[0].x > closed[0].center_x(),
        "popup should not occupy the left half of a wide transcript"
    );
    assert_eq!(fields[0].width, fields[1].width);
    assert_eq!(
        widths[0] - fields[0].x,
        widths[1] - fields[1].x,
        "popup should follow the right edge"
    );
    assert!(
        fields[2].width > 0. && fields[2].width < fields[1].width,
        "input should shrink in a narrow pane"
    );
}

#[tokio::test]
async fn prompt_rail_browses_independently_and_jumps_across_virtualized_history() {
    let mut app = app();
    let mut session = Session::new("/chat".into(), &app.catalog.settings);
    for i in 0..10_000 {
        session.accept(format!("Question {i}").into());
        session
            .entries
            .push(Entry::new(EntryKind::Assistant, "Answer"));
    }
    app.selected = Some(session.id.clone());
    app.current_session = Some(session);
    app.rebuild_transcript();
    app.prepare_visible();
    let mut renderer = header_renderer();
    let mut cache = Cache::new();
    // Leave half a panel of spare room. Opening the panel consumes that margin,
    // without any change to the width of the window itself.
    app.window_width +=
        app.catalog.settings.chat_width - app.center_dimensions().1 + app.panel_width() / 2.;
    let wide = app.window_width;
    let mut target = None;
    for (width, panel, shown) in [
        (wide, false, true),
        (wide, true, false),
        (wide - app.panel_width(), false, false),
        (wide, false, true),
    ] {
        app.window_width = width;
        app.agents_open = panel;
        let size = Size::new(width, 700.);
        let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
        let mut before = Viewport::default();
        ui.operate(&renderer, &mut before);
        let point = iced::Point::new(before.bounds.x + 4., before.bounds.center_y());
        let click = |ui: &mut UserInterface<'_, Message, Theme, iced::Renderer>,
                     renderer: &mut iced::Renderer| {
            click_at(ui, renderer, point)
                .into_iter()
                .find_map(|m| match m {
                    Message::JumpToPrompt(id) => Some(id),
                    _ => None,
                })
        };
        let hit = click(&mut ui, &mut renderer);
        assert_eq!(hit.is_some(), shown, "panel={panel}, width={width}");
        if shown && target.is_none() {
            let mut messages = vec![];
            ui.update(
                &[iced::Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Lines { x: 0., y: -150. },
                })],
                mouse::Cursor::Available(point),
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            assert!(messages.is_empty(), "wheel input stays local to the rail");
            let mut after = Viewport::default();
            ui.operate(&renderer, &mut after);
            assert_eq!(before.dy, after.dy);
            target = click(&mut ui, &mut renderer);
            assert_ne!(hit, target);
        } else if shown {
            assert_eq!(
                hit, target,
                "temporarily hiding the rail should not reset its position"
            );
        }
        cache = ui.into_cache();
    }
    let id = target.unwrap();
    let s = app.current().unwrap();
    let i = s.entries.index_of(&id).unwrap();
    let label = s.entries[i].text.clone();
    assert!(matches!(s.entries[i].kind, EntryKind::User { .. }));
    assert!(!app.transcript.order[app.chat_range()].contains(&i));
    // Drive the actual jump and settle the newly mounted row measurements.
    let size = Size::new(app.center_dimensions().1, 700.);
    cache = Cache::new();
    drive_messages(
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        vec![Message::JumpToPrompt(id)],
    )
    .await;
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    let mut ui = UserInterface::build(app.center(), size, cache, &mut renderer);
    let mut viewport = Viewport::default();
    ui.operate(&renderer, &mut viewport);
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let prompt = bounds.label(&label) - iced::Vector::new(0., viewport.dy);
    assert!(
        viewport.bounds.contains(prompt.center()),
        "jump reveals the clicked prompt: {prompt:?}, view={:?}, dy={}, indexed y={}",
        viewport.bounds,
        viewport.dy,
        app.transcript.offset
    );
    assert!(!app.auto_scroll);
}

#[tokio::test]
async fn prompt_rail_jump_clamped_to_bottom_stays_put_as_reply_grows() {
    let mut app = app();
    let mut session = Session::new("/chat".into(), &app.catalog.settings);
    session.accept("Explain the result".into());
    let prompt = session.entries[0].id.clone();
    session
        .entries
        .push(Entry::new(EntryKind::Assistant, "Working…"));
    app.selected = Some(session.id.clone());
    let h = app.harness.as_ref().unwrap().clone();
    h.insert(session);
    app.refresh();
    let mut renderer = header_renderer();
    let mut cache = Cache::new();
    let size = Size::new(app.catalog.settings.chat_width + 200., 700.);
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    drive_messages(
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        vec![Message::JumpToPrompt(prompt)],
    )
    .await;
    // Resize while the entire conversation still fits: another at-bottom
    // observation must not override the explicit jump.
    let size = Size::new(size.width, size.height - 50.);
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    h.edit(app.selected.as_ref().unwrap(), |s| {
        s.entries[1].text.push_str(
            &"

More detail."
                .repeat(150),
        )
    })
    .unwrap();
    drive_messages(
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        vec![Message::Changed],
    )
    .await;
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    let mut ui = UserInterface::build(app.center(), size, cache, &mut renderer);
    let mut viewport = Viewport::default();
    ui.operate(&renderer, &mut viewport);
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    assert!(viewport.bounds.contains(
        bounds.label("Explain the result").center() - iced::Vector::new(0., viewport.dy)
    ));
    assert!(!app.auto_scroll);
}

#[tokio::test]
async fn last_prompt_jump_keeps_the_whole_bottom_viewport_rendered() {
    let mut app = app();
    let mut session = Session::new("/chat".into(), &app.catalog.settings);
    for i in 0..30 {
        session.accept(format!("Question {i}").into());
    }
    let last = session.entries[29].id.clone();
    session
        .entries
        .push(Entry::new(EntryKind::Assistant, "Short final reply."));
    app.selected = Some(session.id.clone());
    app.current_session = Some(session);
    app.rebuild_transcript();
    app.prepare_visible();
    let size = Size::new(app.catalog.settings.chat_width + 200., 1000.);
    let mut renderer = header_renderer();
    let mut cache = Cache::new();
    settle(&mut app, &mut renderer, &mut cache, size, Some(1.)).await;

    let visible = |app: &App, renderer: &mut iced::Renderer, cache: &mut Cache| {
        let mut ui = UserInterface::build(app.center(), size, std::mem::take(cache), renderer);
        let mut viewport = Viewport::default();
        let mut labels = UiBounds::default();
        ui.operate(renderer, &mut viewport);
        ui.operate(renderer, &mut labels);
        *cache = ui.into_cache();
        assert!(viewport.dy > 0., "the transcript overflows");
        labels
            .labels
            .into_iter()
            .filter_map(|(label, bounds)| {
                (label.starts_with("Question ")
                    && viewport
                        .bounds
                        .contains(bounds.center() - iced::Vector::new(0., viewport.dy)))
                .then_some(label)
            })
            .collect::<Vec<_>>()
    };
    let before = visible(&app, &mut renderer, &mut cache);
    assert!(before.len() > 3 && before.last().unwrap() == "Question 29");
    drive_messages(
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        vec![Message::JumpToPrompt(last)],
    )
    .await;
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    assert_eq!(
        visible(&app, &mut renderer, &mut cache),
        before,
        "the clamped jump does not scroll, so earlier visible prompts must not become blank space"
    );
}
