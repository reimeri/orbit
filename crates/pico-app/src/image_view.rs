//! Images decode off the UI thread. Chat uses static thumbnails; only the modal animates.
use crate::theme;
#[cfg(test)]
use iced::Color;
use iced::{
    Element, Length, Rectangle, Size, Subscription, Task,
    advanced::{Layout, Widget, image as image_primitive, layout, renderer, widget::Tree},
    mouse,
    widget::{column, container, image, mouse_area, text},
};
use pico_core::media::{Frame, Media};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub enum Message {
    Thumbnail(PathBuf, Option<Result<image::Handle, String>>),
    Loaded(u64, PathBuf, Result<Arc<Vec<DisplayFrame>>, String>),
    Toggle,
    Tick,
}
#[derive(Debug)]
pub struct DisplayFrame {
    handle: image::Handle,
    delay: Duration,
}
#[derive(Default)]
pub struct Viewer {
    thumbnails: HashMap<PathBuf, Result<image::Handle, String>>,
    pending: HashSet<PathBuf>,
    current: Option<PathBuf>,
    frames: Option<Result<Arc<Vec<DisplayFrame>>, String>>,
    index: usize,
    playing: bool,
    deadline: Option<Instant>,
    wanted: Arc<Mutex<HashSet<PathBuf>>>,
    generation: u64,
    cancel: Arc<AtomicBool>,
}
pub(crate) static DECODERS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));
fn handles(frames: Vec<Frame>) -> Arc<Vec<DisplayFrame>> {
    Arc::new(
        frames
            .into_iter()
            .map(|f| DisplayFrame {
                handle: image::Handle::from_rgba(f.width, f.height, f.rgba),
                delay: f.delay,
            })
            .collect(),
    )
}
const MODAL_MARGIN: f32 = 24.;
const INFO_PADDING: f32 = 8.;
const INFO_TITLE_SIZE: f32 = 14.;
const INFO_DETAIL_SIZE: f32 = 12.;
const INFO_LINE_SPACING: f32 = 2.;
const THUMBNAIL_MAX_WIDTH: f32 = 320.;
const THUMBNAIL_MAX_HEIGHT: f32 = 220.;

impl Viewer {
    pub fn prepare(&mut self, media: &[Media]) -> Task<Message> {
        let wanted: HashSet<_> = media.iter().filter_map(|m| m.thumbnail.clone()).collect();
        self.thumbnails.retain(|p, _| wanted.contains(p));
        *self.wanted.lock().expect("wanted previews") = wanted.clone();
        let mut jobs = Vec::new();
        for path in wanted {
            if self.thumbnails.contains_key(&path) || !self.pending.insert(path.clone()) {
                continue;
            }
            let wanted = self.wanted.clone();
            jobs.push(Task::perform(
                async move {
                    let input = path.clone();
                    let permit = DECODERS
                        .clone()
                        .acquire_owned()
                        .await
                        .expect("decoder semaphore");
                    let result = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        if !wanted.lock().expect("wanted previews").contains(&input) {
                            return None;
                        }
                        Some((|| {
                            let mut frames =
                                pico_core::media::frames(&input).map_err(|e| format!("{e:#}"))?;
                            let frame = frames.remove(0);
                            Ok(image::Handle::from_rgba(
                                frame.width,
                                frame.height,
                                frame.rgba,
                            ))
                        })())
                    })
                    .await
                    .unwrap_or_else(|e| Some(Err(e.to_string())));
                    (path, result)
                },
                |(p, r)| Message::Thumbnail(p, r),
            ));
        }
        Task::batch(jobs)
    }
    pub fn thumbnail(&self, media: &Media) -> Element<'_, crate::ui::Message> {
        match media
            .thumbnail
            .as_ref()
            .and_then(|p| self.thumbnails.get(p))
        {
            Some(Ok(handle)) => theme::button(
                container(image(handle.clone()).content_fit(iced::ContentFit::Contain))
                    .max_width(THUMBNAIL_MAX_WIDTH)
                    .max_height(THUMBNAIL_MAX_HEIGHT),
            )
            .on_press(crate::ui::Message::OpenImage(media.clone()))
            .style(theme::text_button)
            .into(),
            Some(Err(error)) => text(format!("Preview unavailable: {error}"))
                .size(12)
                .into(),
            None => text("Loading image preview…").size(12).into(),
        }
    }
    pub fn attachment(&self, media: &Media, index: usize) -> Element<'_, crate::ui::Message> {
        let preview: Element<'_, crate::ui::Message> = match media
            .thumbnail
            .as_ref()
            .and_then(|p| self.thumbnails.get(p))
        {
            Some(Ok(handle)) => image(handle.clone())
                .width(56)
                .height(56)
                .content_fit(iced::ContentFit::Contain)
                .into(),
            Some(Err(_)) => text("Preview unavailable").size(10).into(),
            None => text("Loading…").size(10).into(),
        };
        let base = theme::button(container(preview).center_x(56).center_y(56))
            .padding(8)
            .width(72)
            .height(72)
            .style(theme::attachment_button)
            .on_press(crate::ui::Message::OpenImage(media.clone()));
        let remove = container(
            theme::button(text("×").size(14))
                .padding([0, 5])
                .style(theme::attachment_remove)
                .on_press(crate::ui::Message::RemoveAttachment(index)),
        )
        .width(72)
        .height(72)
        .align_x(iced::Right)
        .padding(3);
        iced::widget::hover(base, remove)
    }
    pub fn open(&mut self, media: &Media) -> Task<Message> {
        self.open_source(media.path.clone(), None)
    }
    pub fn open_bytes(&mut self, title: &str, bytes: Arc<Vec<u8>>) -> Task<Message> {
        self.open_source(PathBuf::from(title), Some(bytes))
    }
    fn open_source(&mut self, path: PathBuf, bytes: Option<Arc<Vec<u8>>>) -> Task<Message> {
        self.close();
        self.current = Some(path.clone());
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        Task::perform(
            async move {
                let input = path.clone();
                let permit = DECODERS
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("decoder semaphore");
                let result = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    match bytes {
                        Some(bytes) => pico_core::media::frames_from_bytes(&bytes, &cancel),
                        None => pico_core::media::frames_cancellable(&input, &cancel),
                    }
                    .map(handles)
                    .map_err(|e| format!("{e:#}"))
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
                (path, result)
            },
            move |(p, r)| Message::Loaded(generation, p, r),
        )
    }
    pub fn close(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.current = None;
        self.frames = None;
        self.playing = false;
        self.deadline = None;
        self.index = 0;
    }
    pub fn update(&mut self, message: Message) {
        match message {
            Message::Thumbnail(path, result) => {
                self.pending.remove(&path);
                if let Some(result) = result
                    && self.wanted.lock().expect("wanted previews").contains(&path)
                {
                    self.thumbnails.insert(path, result);
                }
            }
            Message::Loaded(generation, path, result)
                if self.generation == generation && self.current.as_ref() == Some(&path) =>
            {
                self.playing = result.as_ref().is_ok_and(|f| f.len() > 1);
                self.deadline = result.as_ref().ok().map(|f| Instant::now() + f[0].delay);
                self.frames = Some(result);
            }
            Message::Toggle => {
                self.playing = !self.playing;
                if let Some(Ok(frames)) = &self.frames {
                    self.deadline = Some(Instant::now() + frames[self.index].delay);
                }
            }
            Message::Tick if self.playing => {
                if let Some(Ok(frames)) = &self.frames
                    && self.deadline.is_some_and(|t| Instant::now() >= t)
                {
                    let now = Instant::now();
                    let cycle: Duration = frames.iter().map(|f| f.delay).sum();
                    let mut deadline = self.deadline.expect("playing deadline");
                    // Skip complete cycles after suspension, then preserve sub-frame lateness.
                    let lag = now.saturating_duration_since(deadline);
                    if lag >= cycle {
                        deadline +=
                            cycle.mul_f64((lag.as_secs_f64() / cycle.as_secs_f64()).floor());
                    }
                    while deadline <= now {
                        self.index = (self.index + 1) % frames.len();
                        deadline += frames[self.index].delay;
                    }
                    self.deadline = Some(deadline);
                }
            }
            _ => {}
        }
    }
    pub fn subscription(&self) -> Subscription<Message> {
        if self.playing && self.current.is_some() {
            iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }
    pub fn view(
        &self,
        title: String,
        description: String,
        width: f32,
        height: f32,
    ) -> Element<'_, crate::ui::Message> {
        let available_width = (width - 2. * MODAL_MARGIN).max(1.);
        let available_height = (height - 2. * MODAL_MARGIN).max(1.);
        let body: Element<'_, crate::ui::Message> = match &self.frames {
            None => text("Loading image…").into(),
            Some(Err(error)) => text(format!("Could not open image: {error}")).into(),
            Some(Ok(frames)) => {
                let animated = frames.len() > 1;
                // Shrink sizing preserves natural dimensions, with no empty hit area
                // around images that need to scale down to the available space.
                let picture = container(image(frames[self.index].handle.clone()))
                    .max_width(available_width)
                    .max_height((available_height - if animated { 40. } else { 0. }).max(1.));
                let info = container(
                    column![
                        crate::ellipsized_text::shrinking_ellipsized_text(
                            &title,
                            INFO_TITLE_SIZE,
                            Some(theme::IMAGE_TEXT)
                        ),
                        text(description).size(INFO_DETAIL_SIZE),
                    ]
                    .spacing(INFO_LINE_SPACING),
                )
                .max_width(600)
                .padding(INFO_PADDING)
                .style(|_| container::Style {
                    text_color: Some(theme::IMAGE_TEXT),
                    ..Default::default()
                });
                let mut body = column![Element::new(ImageInfo {
                    picture: picture.into(),
                    info: info.into(),
                    hovered: false,
                })]
                .spacing(8)
                .align_x(iced::Center);
                if animated {
                    body = body.push(
                        theme::button(
                            container(text(if self.playing { "❚❚" } else { "▶" }).size(16))
                                .center_x(20)
                                .center_y(20),
                        )
                        .padding(6)
                        .on_press(crate::ui::Message::Image(Message::Toggle)),
                    );
                }
                body.into()
            }
        };
        // The play/pause button captures its own click; all other modal content
        // dismisses just like the surrounding dark overlay.
        mouse_area(body)
            .on_press(crate::ui::Message::Dismiss)
            .into()
    }
}

/// A passive image/info pair. Unlike iced's general-purpose `hover`, this does
/// not introduce a separately rounded scissor at the image's top/left edges.
struct ImageInfo<'a> {
    picture: Element<'a, crate::ui::Message>,
    info: Element<'a, crate::ui::Message>,
    hovered: bool,
}

impl Widget<crate::ui::Message, iced::Theme, iced::Renderer> for ImageInfo<'_> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.picture), Tree::new(&self.info)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.picture, &self.info]);
    }

    fn size(&self) -> Size<Length> {
        self.picture.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let picture = self
            .picture
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let info = self.info.as_widget_mut().layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(Size::ZERO, picture.size()),
        );
        layout::Node::with_children(picture.size(), vec![picture, info])
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        _event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, crate::ui::Message>,
        _viewport: &Rectangle,
    ) {
        let hovered = cursor.is_over(layout.bounds());
        if self.hovered != hovered {
            self.hovered = hovered;
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        use image_primitive::Renderer as _;
        let mut children = layout.children();
        let picture = children.next().expect("picture layout");
        self.picture.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            picture,
            cursor,
            viewport,
        );
        if cursor.is_over(picture.bounds())
            && let Some(visible) = picture.bounds().intersection(viewport)
        {
            let info = children.next().expect("info layout");
            // Use the same image primitive/snap rules as the picture. A container
            // quad rounds half-pixels differently on WGPU and can leave a seam.
            static BACKGROUND: LazyLock<image::Handle> = LazyLock::new(|| {
                image::Handle::from_rgba(1, 1, theme::SCRIM.into_rgba8().to_vec())
            });
            // Keep text in the same foreground layer as its background;
            // returning to the parent layer would draw it underneath the panel.
            renderer.with_layer(*viewport, |renderer| {
                match renderer {
                    iced::Renderer::Primary(_) => renderer.draw_image(
                        image_primitive::Image::new(BACKGROUND.clone()).snap(true),
                        info.bounds(),
                        picture.bounds(),
                    ),
                    // tiny-skia truncates raster origins and cannot accurately
                    // position a stretched one-pixel texture. Match its origin
                    // with a solid quad instead.
                    iced::Renderer::Secondary(_) => renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle {
                                x: info.bounds().x.floor(),
                                y: info.bounds().y.floor(),
                                ..info.bounds()
                            },
                            snap: false,
                            ..Default::default()
                        },
                        theme::SCRIM,
                    ),
                }
                self.info.as_widget().draw(
                    &tree.children[1],
                    renderer,
                    theme,
                    style,
                    info,
                    cursor,
                    &visible,
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnails_shrink_wrap_within_maximum_and_available_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let media = pico_core::media::MediaStore::new(dir.path().into())
            .image(include_bytes!("fixtures/media.png"))
            .unwrap();
        let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ));
        // Measure the normal button inset instead of pinning iced's padding default.
        let mut button = theme::button::<crate::ui::Message>(iced::widget::space());
        let mut tree = Tree::new(&button as &dyn Widget<_, _, _>);
        let padding = button
            .layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(800., 1000.)),
            )
            .size();
        for (width, height, available_width) in [
            (640, 60, 800.),
            (100, 400, 800.),
            (320, 30, 180.),
            (48, 32, 800.),
        ] {
            let mut viewer = Viewer::default();
            viewer.thumbnails.insert(
                media.thumbnail.clone().unwrap(),
                Ok(image::Handle::from_rgba(
                    width,
                    height,
                    vec![255; (width * height * 4) as usize],
                )),
            );
            let mut view = viewer.thumbnail(&media);
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(available_width, 1000.)),
            );
            let fitted = Size::new(
                node.size().width - padding.width,
                node.size().height - padding.height,
            );
            let available = Size::new(
                (available_width - padding.width).min(THUMBNAIL_MAX_WIDTH),
                THUMBNAIL_MAX_HEIGHT,
            );
            let natural = Size::new(width as f32, height as f32);
            assert!(fitted.width > 0. && fitted.height > 0.);
            assert!(
                fitted.width <= available.width + 0.001
                    && fitted.height <= available.height + 0.001
            );
            assert!((fitted.width / fitted.height - natural.width / natural.height).abs() < 0.001);
            if natural.width <= available.width && natural.height <= available.height {
                assert_eq!(fitted, natural, "small images are not enlarged");
            } else {
                assert!(
                    (fitted.width - available.width).abs() < 0.001
                        || (fitted.height - available.height).abs() < 0.001,
                    "shrink-wrap to the constraining axis rather than leaving blank space"
                );
            }
        }
    }

    #[test]
    fn attachment_remove_is_hover_only_and_does_not_open_modal() {
        use iced::{
            Point, Size,
            advanced::{Renderer as _, renderer::Headless},
        };
        let dir = tempfile::tempdir().unwrap();
        let media = pico_core::media::MediaStore::new(dir.path().into())
            .image(include_bytes!("fixtures/media.png"))
            .unwrap();
        let viewer = Viewer::default();
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ));
        let mut view = viewer.attachment(&media, 3);
        let mut tree = Tree::new(view.as_widget());
        let size = view
            .as_widget_mut()
            .layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(400., 400.)),
            )
            .size();
        let mut ui = iced_runtime::UserInterface::build(
            viewer.attachment(&media, 3),
            size,
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        // The remove control occupies the upper-right corner. Its appearance
        // may be text or vector geometry; neither is part of the interaction contract.
        let corner = Rectangle::new(
            Point::new(size.width * 2. / 3., 0.),
            Size::new(size.width / 3., size.height / 3.),
        );
        let remove = corner.center();
        let base = Point::new(size.width / 4., size.height * 0.75);
        for (point, hovering) in [
            (Point::new(size.width + 1., size.height + 1.), false),
            (base, true),
        ] {
            let cursor = mouse::Cursor::Available(point);
            ui.update(
                &[iced::Event::Mouse(mouse::Event::CursorMoved {
                    position: point,
                })],
                cursor,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut vec![],
            );
            renderer.reset(iced::Rectangle::with_size(size));
            ui.draw(
                &mut renderer,
                &theme::app(),
                &renderer::Style {
                    text_color: Color::WHITE,
                },
                cursor,
            );
            let pixels = renderer.screenshot(
                Size::new(size.width as u32, size.height as u32),
                1.,
                Color::BLACK,
            );
            let foreground = theme::TEXT.into_rgba8();
            let remove_painted =
                (corner.y as usize..(corner.y + corner.height) as usize).any(|y| {
                    (corner.x as usize..(corner.x + corner.width) as usize).any(|x| {
                        let pixel = &pixels[(y * size.width as usize + x) * 4..][..4];
                        (0..3).all(|c| pixel[c].abs_diff(foreground[c]) < 30)
                    })
                });
            assert_eq!(remove_painted, hovering, "remove is visible only on hover");
        }
        for (point, remove) in [(remove, true), (base, false)] {
            let mut messages = vec![];
            ui.update(
                &[
                    iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(point),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            if remove {
                assert!(matches!(
                    messages.as_slice(),
                    [crate::ui::Message::RemoveAttachment(3)]
                ));
            } else {
                assert!(
                    matches!(messages.as_slice(), [crate::ui::Message::OpenImage(m)] if m == &media)
                );
            }
        }
    }

    #[test]
    fn modal_keeps_natural_size_and_fits_both_window_axes() {
        use iced::{
            Size,
            advanced::{layout, widget::Tree},
        };
        let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ));
        for (width, height, animated) in [
            (96, 64, false),
            (96, 64, true),
            (1600, 400, false),
            (400, 1600, false),
            (400, 1600, true),
        ] {
            let frames = (0..if animated { 2 } else { 1 })
                .map(|_| Frame {
                    width,
                    height,
                    rgba: vec![255; (width * height * 4) as usize],
                    delay: Duration::from_millis(100),
                })
                .collect();
            let viewer = Viewer {
                frames: Some(Ok(handles(frames))),
                ..Default::default()
            };
            let mut view = viewer.view(
                "/long/path/to/image.png".into(),
                "resolution · bytes".into(),
                800.,
                600.,
            );
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(800., 600.)),
            );
            let picture = node.children()[0].size();
            let controls_height = node.size().height - picture.height;
            assert_eq!(controls_height > 0., animated);
            let available = Size::new(800. - 2. * MODAL_MARGIN, 600. - 2. * MODAL_MARGIN);
            let scale = (available.width / width as f32)
                .min((available.height - controls_height) / height as f32)
                .min(1.);
            assert_eq!(
                picture,
                Size::new(width as f32 * scale, height as f32 * scale)
            );
            assert!(node.size().width <= available.width && node.size().height <= available.height);
        }
    }

    #[test]
    fn long_paths_leave_room_for_image_details() {
        use iced::{
            Size,
            advanced::{layout, widget::Tree},
        };
        let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ));
        let viewer = Viewer {
            frames: Some(Ok(handles(vec![Frame {
                width: 320,
                height: 120,
                rgba: vec![255; 320 * 120 * 4],
                delay: Duration::from_millis(100),
            }]))),
            ..Default::default()
        };
        let mut sizes = Vec::new();
        for title in [
            "image.png".to_owned(),
            format!("/{}image.png", "long-directory/".repeat(20)),
        ] {
            let mut view = viewer.view(title, "320 × 120 · 2588760 bytes".into(), 800., 600.);
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(800., 600.)),
            );
            // Column → image/info pair → metadata. Long paths must not wrap
            // into a paragraph that crowds out the dimensions and byte count.
            let info = &node.children()[0].children()[1];
            assert!(info.size().width <= node.size().width);
            assert!(info.size().height <= node.size().height);
            sizes.push(info.size());
        }
        assert_eq!(sizes[0].height, sizes[1].height);
        assert!(sizes[1].width >= sizes[0].width);
    }

    #[test]
    fn metadata_shrinks_to_the_wider_text_line() {
        let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ));
        let viewer = Viewer {
            frames: Some(Ok(handles(vec![Frame {
                width: 800,
                height: 400,
                rgba: vec![255; 800 * 400 * 4],
                delay: Duration::from_millis(100),
            }]))),
            ..Default::default()
        };
        for (title, description) in [
            ("/tmp/car-generated.png", "1672 × 941 · 2588760 bytes"),
            ("a.png", "800 × 400 · 2588760 bytes"),
        ] {
            let mut natural: Element<'_, crate::ui::Message> = container(
                iced::widget::column![
                    text(title).size(INFO_TITLE_SIZE),
                    text(description).size(INFO_DETAIL_SIZE)
                ]
                .spacing(INFO_LINE_SPACING),
            )
            .padding(INFO_PADDING)
            .into();
            let mut tree = Tree::new(natural.as_widget());
            let expected = natural
                .as_widget_mut()
                .layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::INFINITE),
                )
                .size()
                .width;
            let mut view = viewer.view(title.into(), description.into(), 1000., 800.);
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(1000., 800.)),
            );
            let info = &node.children()[0].children()[1];
            assert!(
                (info.size().width - expected).abs() < 0.01,
                "actual {} expected {expected}",
                info.size().width
            );
        }
    }

    async fn metadata_edges_match_image_pixels(backend: &str) {
        use iced::advanced::{Renderer as _, renderer::Headless};
        let mut renderer =
            iced::Renderer::new(iced::Font::DEFAULT, iced::Pixels(16.), Some(backend))
                .await
                .expect("requested renderer is available");
        let viewer = Viewer {
            frames: Some(Ok(handles(vec![Frame {
                width: 320,
                height: 120,
                rgba: vec![255; 320 * 120 * 4],
                delay: Duration::from_millis(100),
            }]))),
            ..Default::default()
        };
        let size = Size::new(721., 321.); // Centering puts both edges at .5 logical pixels.
        let mut view = viewer.view(
            "a.png".into(),
            "320 × 120 · 1234 bytes".into(),
            size.width,
            size.height,
        );
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let pair = Layout::new(&node).children().next().unwrap();
        let info = pair.children().nth(1).unwrap();
        let lines: Vec<_> = info
            .children()
            .next()
            .unwrap()
            .children()
            .map(|line| line.bounds())
            .collect();
        assert_eq!(lines.len(), 2);
        let mut ui = iced_runtime::UserInterface::build(
            container(viewer.view(
                "a.png".into(),
                "320 × 120 · 1234 bytes".into(),
                size.width,
                size.height,
            ))
            .center_x(iced::Fill)
            .center_y(iced::Fill),
            size,
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        for scale in [1., 1.25, 2.] {
            let physical = Size::new((size.width * scale) as u32, (size.height * scale) as u32);
            let mut screenshots = Vec::new();
            for cursor in [
                mouse::Cursor::Unavailable,
                mouse::Cursor::Available(iced::Point::new(250., 120.)),
            ] {
                renderer.reset(Rectangle::with_size(size));
                ui.draw(
                    &mut renderer,
                    &iced::Theme::Dark,
                    &renderer::Style {
                        text_color: Color::WHITE,
                    },
                    cursor,
                );
                screenshots.push(renderer.screenshot(physical, scale, Color::BLACK));
            }
            let plain = &screenshots[0];
            let hovered = &screenshots[1];
            let first = plain
                .as_chunks::<4>()
                .0
                .iter()
                .position(|pixel| pixel[0] > 200)
                .unwrap();
            let x = first % physical.width as usize;
            let y = first / physical.width as usize;
            // Both text lines must remain bright above the translucent panel,
            // not be dimmed by a background submitted in a later render layer.
            let foreground = theme::IMAGE_TEXT.into_rgba8();
            for line in &lines {
                let left = x + (line.x * scale).floor() as usize;
                let right = x + ((line.x + line.width) * scale).ceil() as usize;
                let top = y + (line.y * scale).floor() as usize;
                let bottom = y + ((line.y + line.height) * scale).ceil() as usize;
                let closest = (top..bottom)
                    .flat_map(|y| (left..right).map(move |x| (y * physical.width as usize + x) * 4))
                    .map(|offset| {
                        (0..3)
                            .map(|c| hovered[offset + c].abs_diff(foreground[c]))
                            .max()
                            .unwrap()
                    })
                    .min()
                    .unwrap();
                assert!(
                    closest < 30,
                    "dimmed metadata text on {backend} at scale {scale}: color distance {closest}"
                );
            }
            // Check the first painted top row and left column, away from text.
            // Quad/scissor rounding previously left these bright on WGPU.
            let interior = hovered[((y + 1) * physical.width as usize + x + 1) * 4];
            assert!(
                interior < plain[first * 4],
                "hover must paint the metadata backdrop"
            );
            for (x, y) in [(x + 4, y), (x, y + 4)] {
                let offset = (y * physical.width as usize + x) * 4;
                assert!(plain[offset] > 200);
                assert!(
                    hovered[offset].abs_diff(interior) <= 3,
                    "uncovered image edge on {backend} at scale {scale}: {}",
                    hovered[offset]
                );
            }
        }
    }

    #[tokio::test]
    async fn metadata_covers_fractional_edges_software() {
        metadata_edges_match_image_pixels("tiny-skia").await;
    }

    #[tokio::test]
    #[ignore = "requires a working GPU; run alongside smoke-gpu"]
    async fn metadata_covers_fractional_edges_gpu() {
        metadata_edges_match_image_pixels("wgpu").await;
    }

    #[test]
    fn metadata_is_hover_only_and_playback_click_does_not_dismiss() {
        use iced::{
            Point, Size,
            advanced::{Renderer as _, renderer::Headless},
            mouse,
        };
        let viewer = Viewer {
            frames: Some(Ok(handles(
                pico_core::media::frames_from_bytes(
                    include_bytes!("fixtures/media.gif"),
                    &AtomicBool::new(false),
                )
                .unwrap(),
            ))),
            playing: true,
            ..Default::default()
        };
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ));
        let mut view = viewer.view("image.gif".into(), "96 × 64 · 123 bytes".into(), 800., 600.);
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(800., 600.)),
        );
        let picture = node.children()[0].bounds();
        let playback = node.children()[1].bounds();
        let mut ui = iced_runtime::UserInterface::build(
            viewer.view("image.gif".into(), "96 × 64 · 123 bytes".into(), 800., 600.),
            Size::new(800., 600.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        // The 96×64 fixture is not enlarged. The button is centered below it.
        let mut baseline = None;
        for (cursor, metadata_visible) in [
            (mouse::Cursor::Unavailable, false),
            (mouse::Cursor::Available(picture.center()), true),
            (mouse::Cursor::Available(playback.center()), false),
            (mouse::Cursor::Available(Point::new(200., 200.)), false),
        ] {
            renderer.reset(iced::Rectangle::with_size(Size::new(800., 600.)));
            ui.draw(
                &mut renderer,
                &iced::Theme::Dark,
                &iced::advanced::renderer::Style {
                    text_color: Color::WHITE,
                },
                cursor,
            );
            let pixels = renderer.screenshot(Size::new(800, 600), 1., Color::BLACK);
            let image_pixels: Vec<_> = (picture.y as usize..(picture.y + picture.height) as usize)
                .flat_map(|y| {
                    let start = (y * 800 + picture.x as usize) * 4;
                    pixels[start..start + picture.width as usize * 4]
                        .iter()
                        .copied()
                })
                .collect();
            if let Some(baseline) = &baseline {
                assert_eq!(
                    &image_pixels != baseline,
                    metadata_visible,
                    "metadata changes the image only while the picture is hovered"
                );
            } else {
                baseline = Some(image_pixels);
            }
        }
        for (point, toggle) in [
            (playback.center(), true),
            (Point::new(4., 4.), false), // Includes clicks over the metadata.
            (
                iced::Point::new(picture.center_x(), picture.y + picture.height - 1.),
                false,
            ),
        ] {
            let mut messages = Vec::new();
            ui.update(
                &[
                    iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(point),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            if toggle {
                assert!(matches!(
                    messages.as_slice(),
                    [crate::ui::Message::Image(Message::Toggle)]
                ));
            } else {
                assert!(matches!(messages.as_slice(), [crate::ui::Message::Dismiss]));
            }
        }
    }

    #[test]
    fn stale_decodes_do_not_restore_closed_images_or_invisible_thumbnails() {
        let dir = tempfile::tempdir().unwrap();
        let media = pico_core::media::MediaStore::new(dir.path().into())
            .image(include_bytes!("fixtures/media.gif"))
            .unwrap();
        let frames = handles(pico_core::media::frames(&media.path).unwrap());
        assert_eq!(frames.len(), 2);
        let mut viewer = Viewer::default();
        let _task = viewer.open(&media);
        let generation = viewer.generation;
        viewer.close();
        viewer.update(Message::Loaded(
            generation,
            media.path.clone(),
            Ok(frames.clone()),
        ));
        assert!(viewer.frames.is_none());
        assert!(!viewer.playing);
        let _task = viewer.open(&media);
        viewer.update(Message::Loaded(
            generation,
            media.path.clone(),
            Ok(frames.clone()),
        ));
        assert!(viewer.frames.is_none());
        viewer.update(Message::Loaded(
            viewer.generation,
            media.path.clone(),
            Ok(frames.clone()),
        ));
        assert!(viewer.playing);
        viewer.update(Message::Toggle);
        assert!(!viewer.playing);
        let _task = viewer.prepare(std::slice::from_ref(&media));
        let _task = viewer.prepare(&[]);
        viewer.update(Message::Thumbnail(
            media.thumbnail.unwrap(),
            Some(Ok(frames[0].handle.clone())),
        ));
        assert!(viewer.thumbnails.is_empty());
        // A skipped decode that arrives after reopening must not poison the cache.
        let dir = tempfile::tempdir().unwrap();
        let media = pico_core::media::MediaStore::new(dir.path().into())
            .image(include_bytes!("fixtures/media.png"))
            .unwrap();
        let _task = viewer.prepare(std::slice::from_ref(&media));
        let _task = viewer.prepare(&[]);
        let _task = viewer.prepare(std::slice::from_ref(&media));
        let thumbnail = media.thumbnail.clone().unwrap();
        viewer.update(Message::Thumbnail(thumbnail.clone(), None));
        assert!(!viewer.thumbnails.contains_key(&thumbnail));
        assert!(!viewer.pending.contains(&thumbnail));
        let _retry = viewer.prepare(&[media]);
        assert!(viewer.pending.contains(&thumbnail));
    }
    #[tokio::test]
    async fn ephemeral_gif_bytes_use_the_existing_modal_playback_and_error_states() {
        use iced::futures::StreamExt;
        let mut viewer = Viewer::default();
        let bytes = Arc::new(include_bytes!("fixtures/media.gif").to_vec());
        let task = viewer.open_bytes("https://example.com/animation.gif", bytes);
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        while let Some(action) = stream.next().await {
            if let iced_runtime::Action::Output(message) = action {
                viewer.update(message);
            }
        }
        assert_eq!(viewer.frames.as_ref().unwrap().as_ref().unwrap().len(), 2);
        assert!(viewer.playing);
        viewer.update(Message::Toggle);
        assert!(!viewer.playing);
        viewer.close();
        let task = viewer.open_bytes("broken.png", Arc::new(b"not an image".to_vec()));
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        while let Some(action) = stream.next().await {
            if let iced_runtime::Action::Output(message) = action {
                viewer.update(message);
            }
        }
        assert!(viewer.frames.as_ref().unwrap().is_err());
        assert!(!viewer.playing);
    }
    #[test]
    fn playback_preserves_lateness_instead_of_slowing_each_frame() {
        let frames = handles(vec![
            Frame {
                width: 1,
                height: 1,
                rgba: vec![255; 4],
                delay: Duration::from_secs(1),
            },
            Frame {
                width: 1,
                height: 1,
                rgba: vec![0; 4],
                delay: Duration::from_secs(2),
            },
        ]);
        let now = Instant::now();
        let mut viewer = Viewer {
            frames: Some(Ok(frames)),
            playing: true,
            deadline: Some(now - Duration::from_millis(1500)),
            ..Default::default()
        };
        viewer.update(Message::Tick);
        assert_eq!(viewer.index, 1);
        assert_eq!(viewer.deadline, Some(now + Duration::from_millis(500)));
        viewer.close();
        assert!(!viewer.playing);
        assert!(viewer.deadline.is_none());
    }
}
