//! Opt-in CPU-side selection benchmark; parsing, layout and GPU rendering are untimed.
//! PICO_SELECTION_BENCH_TEXT optionally supplies a long local Markdown file (never logged).
use super::*;
use iced::advanced::clipboard;
use iced_runtime::{UserInterface, user_interface};
use std::{hint::black_box, time::Instant};

#[test]
#[ignore = "manual timing benchmark; run alone in debug and release"]
fn selection_motion_benchmark() {
    let paragraph = "The quick brown fox moves left and right. café e\u{301} 👩‍💻\n";
    let mut cases = vec![
        ("paragraph-6400".to_owned(), paragraph.repeat(100)),
        ("paragraph-64000".to_owned(), paragraph.repeat(1000)),
        (
            "many-blocks".to_owned(),
            format!("{paragraph}\n").repeat(200),
        ),
    ];
    if let Some(path) = std::env::var_os("PICO_SELECTION_BENCH_TEXT") {
        cases.push((
            "local-message".into(),
            std::fs::read_to_string(path).unwrap(),
        ));
    }
    let mut renderer = crate::test_renderer::new(16.);
    for (name, source) in cases {
        let selection = Shared::default();
        let content = markdown::Content::parse(&source);
        let viewer = Viewer::new(selection.clone(), 0);
        let element = markdown::view_with(
            content.items(),
            markdown::Settings::with_text_size(16., crate::theme::markdown_style()),
            &viewer,
        );
        let blocks = viewer.blocks.borrow().len();
        let mut ui = UserInterface::build(
            crate::pointer_input::pointer_input(
                iced::widget::scrollable(container(element).width(Length::Fill))
                    .width(Length::Fill)
                    .id("selection-bench")
                    .into(),
                false,
                selection.clone(),
            ),
            Size::new(760., 600.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut top_focus = None;
        for (location, offset) in [("top", 0.), ("bottom", f32::MAX)] {
            ui.operate(
                &renderer,
                &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                    iced::advanced::widget::Id::new("selection-bench"),
                    iced::widget::scrollable::AbsoluteOffset {
                        x: None,
                        y: Some(offset),
                    },
                ),
            );
            selection.borrow_mut().start(Position {
                entry: 0,
                block: 0,
                byte: 0,
            });
            let shaped = tests::shaped_paragraphs();
            let mut messages = Vec::new();
            let mut samples = Vec::new();
            let events = 200;
            // Both pointer positions must really change the selection, rather than
            // accidentally benchmarking motion outside a shrink-width viewport.
            let mut previous = None;
            for x in [35., 650.] {
                let p = Point::new(x, 30.);
                let _ = ui.update(
                    &[Event::Mouse(mouse::Event::CursorMoved { position: p })],
                    mouse::Cursor::Available(p),
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
                let focus = selection.borrow().focus.unwrap();
                if let Some(previous) = previous {
                    assert_ne!(focus, previous);
                }
                previous = Some(focus);
            }
            tests::HITS.set(0);
            // Warm up once, then report the median of three batches.
            for sample in 0..4 {
                let start = Instant::now();
                for i in 0..events {
                    let p = Point::new(if i % 2 == 0 { 35. } else { 650. }, 30.);
                    black_box(ui.update(
                        &[Event::Mouse(mouse::Event::CursorMoved { position: p })],
                        mouse::Cursor::Available(p),
                        &mut renderer,
                        &mut clipboard::Null,
                        &mut messages,
                    ));
                }
                if sample > 0 {
                    samples.push(start.elapsed().as_secs_f64() * 1000. / events as f64);
                }
            }
            samples.sort_by(f64::total_cmp);
            assert!(selection.borrow().range().is_some());
            let focus = selection.borrow().focus.unwrap();
            if let Some(top) = top_focus {
                assert!(
                    focus > top,
                    "bottom must reach later text; use a long local fixture"
                );
            } else {
                top_focus = Some(focus);
            }
            assert!(messages.is_empty());
            assert_eq!(tests::shaped_paragraphs(), shaped);
            println!(
                "SELECTION case={name} location={location} bytes={} blocks={blocks} ms/event={:.6} hits/event={:.1} focus={:?}",
                source.len(),
                samples[1],
                tests::HITS.get() as f64 / (4 * events) as f64,
                selection.borrow().focus.unwrap(),
            );
        }
    }
}
