//! Opt-in CPU-side editor benchmark. No drawing/readback is included in timings.
//! Run through scripts/test-renderer.sh with --ignored --nocapture --test-threads=1.
use super::*;
use std::{sync::Arc, time::Instant};
use text_editor::{Action, Content, Edit, Motion};

// Explicit fixture geometry, not assertions about the application's defaults.
const HEIGHT: f32 = 160.;
const WIDTH: f32 = 900.;

fn native_view(content: &Content) -> Element<'_, Action> {
    text_editor(content)
        .size(15)
        .min_height(HEIGHT - 2. * PADDING)
        .padding(iced::Padding::new(PADDING).right(scrollbar::GUTTER + theme::SCROLLBAR_EDGE_INSET))
        .height(Length::Shrink)
        .max_height(HEIGHT)
        .on_action(|action| action)
        .into()
}

struct Fixture {
    content: Content,
    tree: Tree,
    renderer: iced::Renderer,
    scrollbar: State,
}

impl Fixture {
    fn new() -> Self {
        let content = Content::new();
        let tree = Tree::new(native_view(&content).as_widget());
        Self {
            content,
            tree,
            renderer: crate::test_renderer::new(16.),
            scrollbar: State::default(),
        }
    }

    fn layout(&mut self, width: f32) -> (f64, f64, usize) {
        let start = Instant::now();
        let mut view = native_view(&self.content);
        self.tree.diff(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut self.tree,
            &self.renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, 600.)),
        );
        let native_ms = start.elapsed().as_secs_f64() * 1000.;
        assert!(node.size().width <= width);
        drop(view);
        // Inspect outside timed regions: how many logical lines did native
        // layout cache, before our separate full-document measurement?
        let native_cached = self.content.with_editor(|editor| {
            editor
                .buffer()
                .lines
                .iter()
                .filter(|line| line.layout_opt().is_some())
                .count()
        });
        let start = Instant::now();
        self.scrollbar.measure(&self.content);
        let measure_ms = start.elapsed().as_secs_f64() * 1000.;
        (native_ms, measure_ms, native_cached)
    }
}

#[test]
#[ignore = "manual timing benchmark; run alone in debug and release"]
fn editor_stage_benchmark() {
    let samples = std::env::var("PICO_EDITOR_BENCH_SAMPLES")
        .map(|value| value.parse::<usize>().expect("positive sample count"))
        .unwrap_or(3);
    assert!(samples > 0);
    let filter = std::env::var("PICO_EDITOR_BENCH_CASE").ok();
    let unicode = "Long answer with Unicode λ界 and preserved newlines.\n";
    let ascii = "Long answer with Unicode ab and preserved newlines.\n";
    let paragraph = "A paragraph with Unicode λ界 and wrapped words. ";
    let ascii_paragraph = paragraph.replace("λ界", "ab");
    let cases = [
        ("ascii-700", ascii.repeat(700)),
        ("greek-700", ascii.replace("ab", "λb").repeat(700)),
        ("cjk-700", ascii.replace("ab", "a界").repeat(700)),
        ("unicode-700", unicode.repeat(700)),
        ("ascii-7000", ascii.repeat(7000)),
        ("unicode-7000", unicode.repeat(7000)),
        ("paragraph-ascii-700", ascii_paragraph.repeat(700)),
        ("paragraph-unicode-700", paragraph.repeat(700)),
        ("paragraph-ascii-7000", ascii_paragraph.repeat(7000)),
        ("paragraph-unicode-7000", paragraph.repeat(7000)),
        // Avoid measuring only repeated words that fit in the shaping cache.
        (
            "paragraph-distinct-7000",
            (0..7000_u64)
                .map(|i| {
                    format!(
                        "record_{i:04x} value_{:016x} λ界; ",
                        i.wrapping_mul(0x9e3779b97f4a7c15)
                    )
                })
                .collect::<String>(),
        ),
    ];
    if let Some(filter) = &filter {
        assert!(
            cases.iter().any(|(name, _)| name == filter),
            "unknown benchmark case: {filter}"
        );
    }
    // Exclude device creation and initial font discovery. Each sample still has
    // a fresh document, widget tree and scrollbar measurement cache.
    let mut warmup = Fixture::new();
    warmup.layout(WIDTH);
    warmup
        .content
        .perform(Action::Edit(Edit::Paste(Arc::new(unicode.into()))));
    warmup.layout(WIDTH);
    println!(
        "EDITOR_BENCH profile={} samples={samples}; times in milliseconds; font/device warm, document cold",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    println!(
        "case,initial_state,sample,bytes,lines,phase,edit_ms,native_ms,scrollbar_ms,native_cached_lines"
    );
    for (name, value) in cases {
        if filter.as_ref().is_some_and(|filter| name != filter) {
            continue;
        }
        let payload = Arc::new(value);
        for mounted in [false, true] {
            for sample in 1..=samples {
                let initial = if mounted { "mounted" } else { "unlaid" };
                eprintln!("EDITOR_BENCH starting {name}/{initial}/{sample}");
                let mut fixture = Fixture::new();
                if mounted {
                    fixture.layout(WIDTH);
                }
                for phase in ["paste", "unchanged", "type", "resize", "type-start"] {
                    let start = Instant::now();
                    match phase {
                        "paste" => fixture
                            .content
                            .perform(Action::Edit(Edit::Paste(payload.clone()))),
                        "type" => fixture.content.perform(Action::Edit(Edit::Insert('x'))),
                        // Includes navigation to the start as well as insertion.
                        "type-start" => {
                            fixture.content.perform(Action::Move(Motion::DocumentStart));
                            fixture.content.perform(Action::Edit(Edit::Insert('y')));
                        }
                        _ => {}
                    }
                    let edit_ms = start.elapsed().as_secs_f64() * 1000.;
                    let (native_ms, measure_ms, native_cached) =
                        fixture.layout(if matches!(phase, "resize" | "type-start") {
                            420.
                        } else {
                            WIDTH
                        });
                    println!(
                        "{name},{initial},{sample},{},{},{phase},{edit_ms:.3},{native_ms:.3},{measure_ms:.3},{native_cached}",
                        payload.len(),
                        fixture.content.line_count()
                    );
                    if phase == "paste" {
                        assert!(
                            fixture.scrollbar.offset > 0.,
                            "paste caret should reveal the tail"
                        );
                    }
                }
                // Lossless paste/edit and real overflow, rather than timing an
                // accidentally empty or disconnected editor.
                assert_eq!(fixture.content.text(), format!("y{payload}x"));
                assert!(fixture.scrollbar.content_height > HEIGHT);
            }
        }
    }
}
