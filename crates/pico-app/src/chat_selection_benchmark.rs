//! Opt-in CPU selection benchmarks: pointer motion and scroll-triggered view rebuilds.
//! GPU preparation, submission and presentation are excluded.
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
            markdown::Settings::with_text_size(
                16.,
                crate::theme::markdown_style(&crate::theme::app()),
            ),
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

#[test]
#[ignore = "manual scrolling CPU benchmark; run alone in release"]
fn selection_scroll_benchmark() {
    use iced::advanced::Renderer as _;
    let prose = "Scrolling **formatted text** with `inline code`, a [link](https://example.com), and Unicode café λ界. This paragraph describes an ordinary document with several words.\n\n";
    let cases = vec![
        ("20-blocks", prose.repeat(20)),
        ("200-blocks", prose.repeat(200)),
        ("2000-blocks", prose.repeat(2000)),
        ("formatted-single-paragraph-200", prose.trim().repeat(200)),
        ("formatted-single-paragraph-1000", prose.trim().repeat(1000)),
        (
            "64k-paragraph",
            "The quick brown fox moves left and right. café e\u{301} 👩‍💻\n".repeat(1000),
        ),
        (
            "2000-code-lines",
            format!(
                "```rust\n{}\n```",
                "let result = compute_value(input); // an ordinary code line\n".repeat(2000)
            ),
        ),
    ];
    let mut renderer = crate::test_renderer::new(16.);
    let size = Size::new(760., 600.);
    for (name, source) in cases {
        let start = Instant::now();
        let content = markdown::Content::parse(&source);
        let parse_ms = start.elapsed().as_secs_f64() * 1000.;
        let selection = Shared::default();
        let mut cache = user_interface::Cache::new();
        let mut samples = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let mut blocks = 0;
        let mut messages = Vec::new();
        let mut cold_layout_ms = 0.;
        let mut cold_draw_ms = 0.;
        let mut reflow_ms = 0.;
        for iteration in 0..121 {
            let units = tests::UNITS.get();
            let decoration_passes = tests::DECORATION_PASSES.get();
            let start = Instant::now();
            let viewer = Viewer::new(selection.clone(), 0);
            let element = markdown::view_with(
                content.items(),
                markdown::Settings::with_text_size(
                    16.,
                    crate::theme::markdown_style(&crate::theme::app()),
                ),
                &viewer,
            );
            blocks = viewer.blocks.borrow().len();
            let view = crate::pointer_input::pointer_input(
                crate::scrollbar::scroll(crate::transcript_viewport::observe(
                    container(element).width(Length::Fill),
                    0,
                    |offset, height, _| Message::Scroll(0, offset, height, false),
                ))
                .width(Length::Fill)
                .id("scroll-probe")
                .into(),
                false,
                selection.clone(),
            );
            let view_ms = start.elapsed().as_secs_f64() * 1000.;
            let shaped = tests::shaped_paragraphs();
            let start = Instant::now();
            let mut ui = UserInterface::build(view, size, cache, &mut renderer);
            let layout_ms = start.elapsed().as_secs_f64() * 1000.;
            if iteration == 0 {
                cold_layout_ms = layout_ms;
                ui.operate(
                    &renderer,
                    &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                        iced::advanced::widget::Id::new("scroll-probe"),
                        iced::widget::scrollable::AbsoluteOffset {
                            x: None,
                            y: Some(2000.),
                        },
                    ),
                );
            } else {
                assert_eq!(
                    tests::shaped_paragraphs(),
                    shaped,
                    "unchanged content must reuse paragraphs"
                );
                assert_eq!(
                    tests::UNITS.get(),
                    units,
                    "unchanged content must reuse selection indexes"
                );
            }
            messages.clear();
            let start = Instant::now();
            black_box(ui.update(
                &[
                    Event::Mouse(mouse::Event::WheelScrolled {
                        delta: mouse::ScrollDelta::Pixels {
                            x: 0.,
                            y: if iteration % 2 == 0 { -120. } else { 120. },
                        },
                    }),
                    Event::Window(iced::window::Event::RedrawRequested(
                        iced::time::Instant::now(),
                    )),
                ],
                mouse::Cursor::Available(Point::new(400., 300.)),
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            ));
            let update_ms = start.elapsed().as_secs_f64() * 1000.;
            assert!(messages.iter().any(|m| matches!(m, Message::Scroll(..))));
            let start = Instant::now();
            renderer.reset(Rectangle::with_size(size));
            ui.draw(
                &mut renderer,
                &crate::theme::app(),
                &renderer::Style {
                    text_color: crate::theme::default_colors().text,
                },
                mouse::Cursor::Unavailable,
            );
            let draw_ms = start.elapsed().as_secs_f64() * 1000.;
            if iteration == 0 {
                cold_draw_ms = draw_ms;
            } else {
                assert_eq!(
                    tests::DECORATION_PASSES.get(),
                    decoration_passes,
                    "unchanged scroll frames must reuse decoration geometry"
                );
            }
            if iteration == 120 {
                let start = Instant::now();
                ui = ui.relayout(Size::new(size.width * 0.75, size.height), &mut renderer);
                reflow_ms = start.elapsed().as_secs_f64() * 1000.;
            }
            cache = ui.into_cache();
            // Independent cost of rebuilding the same word indexes (the old path).
            // This diagnostic is not added to the measured frame stages.
            let start = Instant::now();
            for block in viewer.blocks.borrow().iter() {
                black_box(TextUnits::new(&block.text, 0, Paragraphs::Block));
            }
            let units_ms = start.elapsed().as_secs_f64() * 1000.;
            if iteration > 0 {
                for (values, value) in samples
                    .iter_mut()
                    .zip([view_ms, layout_ms, update_ms, draw_ms, units_ms])
                {
                    values.push(value);
                }
            }
        }
        println!(
            "SCROLL case={name} bytes={} blocks={blocks} parse_ms={parse_ms:.3} cold_layout_ms={cold_layout_ms:.3} cold_draw_ms={cold_draw_ms:.3} reflow_ms={reflow_ms:.3}",
            source.len()
        );
        for (stage, mut values) in [
            "view",
            "layout",
            "wheel_redraw",
            "draw_recording",
            "word_indexes_separate",
        ]
        .into_iter()
        .zip(samples)
        {
            values.sort_by(f64::total_cmp);
            println!(
                "SCROLL stage={stage} median_ms={:.6} p95_ms={:.6}",
                values[values.len() / 2],
                values[values.len() * 95 / 100]
            );
        }
    }
}
