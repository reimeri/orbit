//! One graphics device per test process; each fixture owns its recorded renderer state.
//! Engine clones still share MSAA targets, so capture preparation/render/readback is serialized.
//! Automated checks select Lavapipe in scripts/test-renderer.sh, never in the app.
use std::sync::{LazyLock, Mutex};

use iced::advanced::graphics::{Antialiasing, Shell, color};
use iced_wgpu::{Engine, wgpu};

static CAPTURE: Mutex<()> = Mutex::new(());

static GPU: LazyLock<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> = LazyLock::new(|| {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            flags: wgpu::InstanceFlags::empty(),
            ..Default::default()
        });
        let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: None,
            ..Default::default()
        }).await.expect("Vulkan test adapter unavailable; run `nix develop --command just test` to use the pinned CPU Vulkan driver");
        let info = adapter.get_info();
        eprintln!(
            "Rendering tests: {} ({:?}) via Vulkan",
            info.name, info.device_type
        );
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Orbit rendering tests"),
                required_limits: wgpu::Limits {
                    max_bind_groups: 2,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .expect("Create Vulkan test device");
        (adapter, device, queue)
    })
});

fn engine() -> Engine {
    let (adapter, device, queue) = &*GPU;
    Engine::new(
        adapter,
        device.clone(),
        queue.clone(),
        if color::GAMMA_CORRECTION {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        },
        Some(Antialiasing::MSAAx4),
        Shell::headless(),
    )
}

static ENGINE: LazyLock<Engine> = LazyLock::new(engine);

pub fn new(text_size: f32) -> iced::Renderer {
    iced::Renderer::new(ENGINE.clone(), iced::Font::DEFAULT, iced::Pixels(text_size))
}

pub fn screenshot(
    renderer: &mut iced::Renderer,
    size: iced::Size<u32>,
    scale: f32,
    background: iced::Color,
) -> Vec<u8> {
    // iced_wgpu's cloned triangle pipelines share resizeable MSAA targets. The
    // internal per-operation locks do not protect a whole prepare/render pair:
    // another fixture can resize the target between them (or overwrite pixels).
    let _capture = CAPTURE.lock().expect("render-test capture lock");
    iced::advanced::renderer::Headless::screenshot(renderer, size, scale, background)
}

/// A cold render is the reference for retained-widget tests: stale paragraphs
/// can have correct operation labels while still painting the previous text.
pub fn assert_matches_fresh<Message>(
    renderer: &mut iced::Renderer,
    fresh: iced::Element<'_, Message>,
    size: iced::Size,
    scroll_id: &'static str,
    offset: f32,
    text_region: iced::Rectangle,
) {
    use iced::advanced::Renderer as _;
    let colors = crate::theme::default_colors();
    let physical = iced::Size::new(size.width as u32, size.height as u32);
    let actual = screenshot(renderer, physical, 1., colors.background);
    let mut reference = iced_runtime::UserInterface::build(
        fresh,
        size,
        iced_runtime::user_interface::Cache::new(),
        renderer,
    );
    reference.operate(
        renderer,
        &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
            iced::advanced::widget::Id::new(scroll_id),
            iced::widget::scrollable::AbsoluteOffset {
                x: None,
                y: Some(offset),
            },
        ),
    );
    renderer.reset(iced::Rectangle::with_size(size));
    reference.draw(
        renderer,
        &crate::theme::app(),
        &iced::advanced::renderer::Style {
            text_color: colors.text,
        },
        iced::mouse::Cursor::Unavailable,
    );
    let expected = screenshot(renderer, physical, 1., colors.background);
    // Inspect an actual visible text run, not arbitrary bright controls elsewhere
    // in the view. Allow antialiased edges around the shared foreground color.
    let foreground = colors.text.into_rgba8();
    let text_region = text_region
        .intersection(&iced::Rectangle::with_size(size))
        .expect("reference text must intersect the viewport");
    assert!(
        (text_region.y.ceil() as u32..(text_region.y + text_region.height).floor() as u32).any(
            |y| {
                (text_region.x.ceil() as u32..(text_region.x + text_region.width).floor() as u32)
                    .any(|x| {
                        let pixel = &expected[((y * physical.width + x) * 4) as usize..][..4];
                        (0..3).all(|c| pixel[c].abs_diff(foreground[c]) < 30)
                    })
            }
        ),
        "reference must paint foreground glyphs inside the visible text bounds"
    );
    assert!(
        actual == expected,
        "retained widgets must paint the same current content as a fresh view"
    );
}

#[test]
fn growing_the_glyph_atlas_keeps_earlier_and_later_uploads() {
    use iced::advanced::{
        Renderer as _,
        text::{self, Renderer as _},
    };
    use iced::{Color, Font, Point, Rectangle, Size};
    let glyphs: Vec<char> = ('A'..='Z').chain('a'..='z').chain('0'..='9').collect();
    let columns = 8;
    let cell = 112;
    let render = |renderer: &mut iced::Renderer, glyphs: &[char]| {
        let size = Size::new(
            columns as u32 * cell,
            glyphs.len().div_ceil(columns) as u32 * cell,
        );
        let clip = Rectangle::with_size(Size::new(size.width as f32, size.height as f32));
        renderer.reset(clip);
        for (i, ch) in glyphs.iter().enumerate() {
            renderer.fill_text(
                text::Text {
                    content: ch.to_string(),
                    bounds: Size::new(cell as f32, cell as f32),
                    size: 88.into(),
                    line_height: text::LineHeight::Absolute(100.into()),
                    font: Font::DEFAULT,
                    align_x: text::Alignment::Left,
                    align_y: iced::alignment::Vertical::Top,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::None,
                },
                Point::new(
                    (i % columns) as f32 * cell as f32,
                    (i / columns) as f32 * cell as f32,
                ),
                Color::WHITE,
                clip,
            );
        }
        screenshot(renderer, size, 1., Color::BLACK)
    };
    // A row at a time fits the initial mask texture. The same distinct large
    // glyphs in ONE frame force growth while earlier uploads are still pending.
    let mut reference = new(16.);
    let expected: Vec<_> = glyphs
        .chunks(columns)
        .flat_map(|row| render(&mut reference, row))
        .collect();
    assert!(expected.as_chunks::<4>().0.iter().any(|px| px[0] > 128));
    // Same device, independent empty atlas regardless of test order.
    let mut cold = iced::Renderer::new(engine(), Font::DEFAULT, 16.into());
    let actual = render(&mut cold, &glyphs);
    assert!(actual == expected, "atlas growth changed rendered glyphs");
}
