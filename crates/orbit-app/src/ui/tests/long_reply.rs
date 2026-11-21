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
                Message::MarkdownGeometry(..) | Message::RowSize(..) | Message::Scroll(..)
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
    let mut app = fixture(&source);
    let session = app.current_session.as_mut().unwrap();
    let id = session.entries[0].id.clone();
    for i in 0..90 {
        session
            .entries
            .push(Entry::new(EntryKind::Assistant, format!("Following {i}")));
    }
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

#[tokio::test]
async fn streaming_into_windowed_layout_keeps_the_reader_at_the_same_text() {
    let source: String = (0..12)
        .map(|i| {
            format!(
                "Paragraph {i}: {}\n\n",
                "Long wrapped lines λ. ".repeat(240)
            )
        })
        .collect();
    let mut app = fixture(&source);
    let h = app.harness.as_ref().unwrap().clone();
    let session = app.current_session.take().unwrap();
    let id = session.id.clone();
    h.insert(session);
    app.refresh();
    let mut cache = Cache::new();
    let mut renderer = header_renderer();
    let size = Size::new(525., 540.);
    settle(&mut app, &mut renderer, &mut cache, size, Some(0.65)).await;
    let (offset, (label, y)) = visible_text(&app, &mut renderer, &mut cache, size);
    move_to(&app, &mut renderer, &mut cache, size, offset);
    h.edit(&id, |s| {
        s.entries[0].text.push_str(&"Next block.\n\n".repeat(160))
    })
    .unwrap();
    app.refresh();
    settle(&mut app, &mut renderer, &mut cache, size, None).await;
    let (_, (now, screen_y)) = visible_text(&app, &mut renderer, &mut cache, size);
    assert_eq!(label, now);
    assert!((screen_y - y).abs() < 1.);
    assert!(
        !app.auto_scroll,
        "layout changes must not resume following the tail"
    );
}
