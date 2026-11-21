//! Images decode off the UI thread. Chat uses static thumbnails; only the modal animates.
use crate::icons::{Icon, tip as icon_tip};
use crate::{recent_previews::RecentPreviews, theme};
#[cfg(test)]
use iced::Color;
use iced::{
    Element, Length, Rectangle, Size, Subscription, Task,
    advanced::{Layout, Widget, image as image_primitive, layout, renderer, widget::Tree},
    mouse,
    widget::{column, container, image, mouse_area, text},
};
use orbit_core::media::{Frame, Media};
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
    Tick(Instant),
}
#[derive(Clone, Debug)]
pub struct DisplayFrame {
    handle: image::Handle,
    delay: Duration,
    // Pin the first frame while the viewer owns it: CPU decode alone does not
    // guarantee a large image can be drawn by WGPU in the next frame.
    allocation: Option<image_primitive::Allocation>,
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
    transition: Option<Transition>,
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
                allocation: None,
            })
            .collect(),
    )
}
const ZOOM_DURATION: Duration = Duration::from_millis(250);
const GIF_CONTROLS_HEIGHT: f32 = 40.;

struct Motion {
    started: Instant,
    from: f32,
    to: f32,
}
struct Transition {
    source: crate::image_transition::Source,
    natural: Size,
    controls: f32,
    controls_motion: Option<Motion>,
    return_to_source: bool,
    close_from: f32,
    amount: f32,
    motion: Option<Motion>,
    closing: bool,
}
impl Transition {
    fn advance(&mut self, now: Instant) {
        for (value, motion) in [
            (&mut self.amount, &mut self.motion),
            (&mut self.controls, &mut self.controls_motion),
        ] {
            if let Some(current) = motion {
                let duration = ZOOM_DURATION.as_secs_f32() * (current.to - current.from).abs();
                let t = (now.saturating_duration_since(current.started).as_secs_f32() / duration)
                    .min(1.);
                let eased = 1. - (1. - t).powi(3);
                *value = current.from + (current.to - current.from) * eased;
                if t >= 1. {
                    *motion = None;
                }
            }
        }
    }
    fn animating(&self) -> bool {
        self.motion.is_some() || self.controls_motion.is_some()
    }
}

fn modal_bounds(natural: Size, window: Size, controls: f32) -> Rectangle {
    let scale = ((window.width - 2. * MODAL_MARGIN).max(1.) / natural.width)
        .min((window.height - 2. * MODAL_MARGIN - controls).max(1.) / natural.height)
        .min(1.);
    let size = natural * scale;
    Rectangle::new(
        iced::Point::new(
            (window.width - size.width) / 2.,
            (window.height - size.height - controls) / 2.,
        ),
        size,
    )
}

const MODAL_MARGIN: f32 = 24.;
const INFO_PADDING: f32 = 8.;
const INFO_TITLE_SIZE: f32 = 14.;
const INFO_DETAIL_SIZE: f32 = 12.;
const INFO_LINE_SPACING: f32 = 2.;
const ATTACHMENT_SIDE: f32 = 72.;

pub enum PendingAttachment {
    Adding,
    Queued,
}
impl PendingAttachment {
    pub fn view<'a>(self) -> Element<'a, crate::ui::Message> {
        let content = match self {
            Self::Adding => Icon::LoaderCircle.spinning(18., true),
            Self::Queued => column![Icon::Clock.view(18.), text("Queued").size(10)]
                .spacing(4)
                .align_x(iced::Center)
                .into(),
        };
        container(content)
            .center_x(ATTACHMENT_SIDE)
            .center_y(ATTACHMENT_SIDE)
            .style(theme::card)
            .into()
    }
}

const THUMBNAIL_MAX_WIDTH: f32 = 320.;
const THUMBNAIL_MAX_HEIGHT: f32 = 220.;

impl Viewer {
    pub fn prepare(&mut self, recent: &mut RecentPreviews, media: &[Media]) -> Task<Message> {
        let wanted: HashSet<_> = media.iter().filter_map(|m| m.thumbnail.clone()).collect();
        for (path, result) in self.thumbnails.extract_if(|path, _| !wanted.contains(path)) {
            if let Ok(handle) = result {
                recent.store_saved(path, handle);
            }
        }
        *self.wanted.lock().expect("wanted previews") = wanted.clone();
        let mut jobs = Vec::new();
        for path in wanted {
            if self.thumbnails.contains_key(&path) {
                continue;
            }
            if let Some(handle) = recent.take_saved(&path) {
                self.thumbnails.insert(path, Ok(handle));
                continue;
            }
            if !self.pending.insert(path.clone()) {
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
                                orbit_core::media::frames(&input).map_err(|e| format!("{e:#}"))?;
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
        if jobs.is_empty() {
            Task::none()
        } else {
            Task::batch(jobs)
        }
    }
    pub fn thumbnail(&self, media: &Media) -> Element<'_, crate::ui::Message> {
        match media
            .thumbnail
            .as_ref()
            .and_then(|p| self.thumbnails.get(p))
        {
            Some(Ok(handle)) => crate::image_transition::preview(
                theme::button(
                    container(image(handle.clone()).content_fit(iced::ContentFit::Contain))
                        .max_width(THUMBNAIL_MAX_WIDTH)
                        .max_height(THUMBNAIL_MAX_HEIGHT),
                )
                .on_press(crate::ui::Message::OpenImage(media.clone()))
                .style(theme::text_button),
                handle.clone(),
            ),
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
            .width(ATTACHMENT_SIDE)
            .height(ATTACHMENT_SIDE)
            .style(theme::attachment_button)
            .on_press(crate::ui::Message::OpenImage(media.clone()));
        let base: Element<'_, crate::ui::Message> = match media
            .thumbnail
            .as_ref()
            .and_then(|p| self.thumbnails.get(p))
        {
            Some(Ok(handle)) => crate::image_transition::preview(base, handle.clone()),
            _ => base.into(),
        };
        let remove = container(icon_tip(
            theme::button(container(Icon::X.view(14.)).center_y(20))
                .padding([0, 5])
                .style(theme::attachment_remove)
                .on_press(crate::ui::Message::RemoveAttachment(index)),
            "Remove attachment",
        ))
        .width(ATTACHMENT_SIDE)
        .height(ATTACHMENT_SIDE)
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
        let upload_cancel = cancel.clone();
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
                        Some(bytes) => orbit_core::media::frames_from_bytes(&bytes, &cancel),
                        None => orbit_core::media::frames_cancellable(&input, &cancel),
                    }
                    .map(handles)
                    .map_err(|e| format!("{e:#}"))
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
                (path, result)
            },
            |decoded| decoded,
        )
        .then(move |(path, result)| {
            if upload_cancel.load(Ordering::Relaxed) {
                return Task::none();
            }
            match result {
                Ok(mut frames) => {
                    iced_runtime::image::allocate(frames[0].handle.clone()).map(move |result| {
                        let ready = result
                            .map(|allocation| {
                                Arc::make_mut(&mut frames)[0].allocation = Some(allocation);
                                frames.clone()
                            })
                            .map_err(|error| {
                                format!("Could not prepare image for display: {error}. Close and reopen the image to retry.")
                            });
                        Message::Loaded(generation, path.clone(), ready)
                    })
                }
                Err(error) => Task::done(Message::Loaded(generation, path, Err(error))),
            }
        })
    }
    pub fn animate_from(
        &mut self,
        source: crate::image_transition::Source,
        natural: Size,
        gif: bool,
    ) {
        source.hide(true);
        self.transition = Some(Transition {
            source,
            natural,
            controls: if gif { 1. } else { 0. },
            controls_motion: None,
            return_to_source: true,
            close_from: 1.,
            amount: 0.,
            motion: Some(Motion {
                started: Instant::now(),
                from: 0.,
                to: 1.,
            }),
            closing: false,
        });
    }
    pub fn request_close(&mut self, now: Instant) -> bool {
        let Some(transition) = &mut self.transition else {
            return false;
        };
        if !transition.closing {
            transition.closing = true;
            transition.close_from = transition.amount;
            transition.controls_motion = None;
            transition.motion = (transition.amount > 0.).then_some(Motion {
                started: now,
                from: transition.amount,
                to: 0.,
            });
            self.playing = false;
            self.cancel.store(true, Ordering::Relaxed);
        }
        // An immediate dismissal before the first animation frame needs no timer.
        transition.motion.is_some()
    }
    pub fn invalidate_return(&mut self) {
        if let Some(transition) = &mut self.transition
            && transition.return_to_source
        {
            if transition.closing {
                // A live transcript can move even during the return flight.
                // Freeze this frame, not the geometry from the start of closing.
                transition.close_from = transition.amount;
            }
            transition.return_to_source = false;
        }
    }
    pub fn finished_closing(&self) -> bool {
        self.transition
            .as_ref()
            .is_some_and(|t| t.closing && t.motion.is_none())
    }
    pub fn backdrop_opacity(&self) -> f32 {
        self.transition.as_ref().map_or(1., |t| t.amount)
    }
    pub fn close(&mut self) {
        if let Some(transition) = self.transition.take() {
            transition.source.hide(false);
        }
        self.cancel.store(true, Ordering::Relaxed);
        self.current = None;
        self.frames = None;
        self.playing = false;
        self.deadline = None;
        self.index = 0;
    }
    pub fn update(&mut self, message: Message) {
        if let Message::Tick(now) = message
            && let Some(transition) = &mut self.transition
        {
            transition.advance(now);
        }
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
                if self.generation == generation
                    && self.current.as_ref() == Some(&path)
                    && !self.transition.as_ref().is_some_and(|t| t.closing) =>
            {
                self.playing = result.as_ref().is_ok_and(|f| f.len() > 1);
                if let Some(transition) = &mut self.transition {
                    let controls = if self.playing { 1. } else { 0. };
                    // GIF headers do not say whether there are multiple frames.
                    // Retarget continuously if decoding changes the control space.
                    if transition.controls != controls {
                        transition.controls_motion = Some(Motion {
                            started: Instant::now(),
                            from: transition.controls,
                            to: controls,
                        });
                    }
                }
                self.deadline = result.as_ref().ok().map(|f| Instant::now() + f[0].delay);
                if result.is_err()
                    && let Some(transition) = self.transition.take()
                {
                    transition.source.hide(false);
                }
                self.frames = Some(result);
            }
            Message::Toggle => {
                self.playing = !self.playing;
                if let Some(Ok(frames)) = &self.frames {
                    self.deadline = Some(Instant::now() + frames[self.index].delay);
                }
            }
            Message::Tick(now) if self.playing => {
                if let Some(Ok(frames)) = &self.frames
                    && self.deadline.is_some_and(|t| now >= t)
                {
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
        if (self.playing && self.current.is_some())
            || self.transition.as_ref().is_some_and(Transition::animating)
        {
            iced::time::every(Duration::from_millis(16)).map(Message::Tick)
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
        if let Some(transition) = &self.transition
            && (transition.animating() || self.frames.is_none() || transition.closing)
        {
            let handle = match &self.frames {
                Some(Ok(frames)) => frames[self.index].handle.clone(),
                _ => transition.source.handle.clone(),
            };
            let window = Size::new(width, height);
            let destination = modal_bounds(
                transition.natural,
                window,
                transition.controls * GIF_CONTROLS_HEIGHT,
            );
            // A resized/reflowed chat no longer has the captured source geometry.
            // Fade out in place rather than fly to an unrelated position.
            let fade = transition.closing && !transition.return_to_source;
            let position = if fade {
                transition.close_from
            } else {
                transition.amount
            };
            let bounds = crate::image_transition::interpolate(
                transition.source.bounds,
                destination,
                position,
            );
            let clip = crate::image_transition::interpolate(
                transition.source.clip,
                Rectangle::with_size(window),
                position,
            );
            return mouse_area(Element::new(crate::image_transition::Flight {
                handle,
                bounds,
                clip,
                opacity: if fade {
                    transition.amount / transition.close_from
                } else {
                    1.
                },
            }))
            .on_press(crate::ui::Message::Dismiss)
            .into();
        }
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
                    .max_height(
                        (available_height - if animated { GIF_CONTROLS_HEIGHT } else { 0. })
                            .max(1.),
                    );
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
                    body = body.push(icon_tip(
                        theme::button(
                            container(
                                (if self.playing {
                                    Icon::Pause
                                } else {
                                    Icon::Play
                                })
                                .view(16.),
                            )
                            .center_x(20)
                            .center_y(20),
                        )
                        .padding(6)
                        .on_press(crate::ui::Message::Image(Message::Toggle)),
                        if self.playing { "Pause" } else { "Play" },
                    ));
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
                renderer.draw_image(
                    image_primitive::Image::new(BACKGROUND.clone()).snap(true),
                    info.bounds(),
                    picture.bounds(),
                );
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
    fn zoom_can_reverse_during_loading_and_ignores_late_decode() {
        let handle = image::Handle::from_rgba(120, 40, vec![255; 120 * 40 * 4]);
        let message = crate::image_transition::tests::activate(
            crate::image_transition::preview(
                theme::button(image(handle.clone()))
                    .padding(0)
                    .on_press(crate::ui::Message::OpenMarkdownImage("fixture.png".into())),
                handle,
            ),
            iced::Point::new(30., 20.),
        );
        let crate::ui::Message::OpenImageFrom(_, source) = message else {
            panic!("expected image activation")
        };
        let mut viewer = Viewer {
            current: Some("fixture.png".into()),
            ..Default::default()
        };
        viewer.animate_from(source, Size::new(1200., 400.), false);
        let started = viewer
            .transition
            .as_ref()
            .unwrap()
            .motion
            .as_ref()
            .unwrap()
            .started;
        let reversal = started + ZOOM_DURATION / 4;
        viewer.update(Message::Tick(reversal));
        let amount = viewer.backdrop_opacity();
        assert!(amount > 0. && amount < 1.);
        assert!(
            !iced_runtime::futures::subscription::into_recipes(viewer.subscription()).is_empty()
        );
        assert!(viewer.request_close(reversal));
        assert_eq!(viewer.backdrop_opacity(), amount, "reversing does not jump");
        viewer.update(Message::Loaded(
            viewer.generation,
            "fixture.png".into(),
            Ok(handles(vec![Frame {
                width: 1,
                height: 1,
                rgba: vec![255; 4],
                delay: Duration::from_secs(1),
            }])),
        ));
        assert!(
            viewer.frames.is_none(),
            "cancelled loading must not restart playback or replace the preview"
        );
        viewer.update(Message::Tick(reversal + ZOOM_DURATION / 4));
        assert!(viewer.backdrop_opacity() < amount && viewer.backdrop_opacity() > 0.);
        assert!(viewer.request_close(reversal + ZOOM_DURATION / 4));
        viewer.update(Message::Tick(reversal + ZOOM_DURATION));
        assert!(viewer.finished_closing());
        assert_eq!(viewer.backdrop_opacity(), 0.);
        assert!(
            iced_runtime::futures::subscription::into_recipes(viewer.subscription()).is_empty(),
            "settled static viewer must not keep ticking"
        );
        viewer.close();
        assert!(viewer.transition.is_none());
    }

    #[test]
    fn close_fades_in_place_after_reflow_and_single_frame_gif_retargets_smoothly() {
        use iced::advanced::Renderer as _;
        let handle = image::Handle::from_rgba(120, 40, vec![255; 120 * 40 * 4]);
        let message = crate::image_transition::tests::activate(
            crate::image_transition::preview(
                theme::button(image(handle.clone()))
                    .padding(0)
                    .on_press(crate::ui::Message::OpenMarkdownImage("fixture.gif".into())),
                handle,
            ),
            iced::Point::new(30., 20.),
        );
        let crate::ui::Message::OpenImageFrom(_, source) = message else {
            panic!("expected image activation")
        };
        let window = Size::new(800., 600.);
        let natural = Size::new(600., 200.);
        let mut renderer = crate::test_renderer::new(16.);
        let mut bounds = |viewer: &Viewer| {
            let mut ui = iced_runtime::UserInterface::build(
                container(viewer.view(String::new(), String::new(), window.width, window.height))
                    .center_x(iced::Fill)
                    .center_y(iced::Fill),
                window,
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            renderer.reset(Rectangle::with_size(window));
            ui.draw(
                &mut renderer,
                &theme::app(),
                &renderer::Style {
                    text_color: Color::WHITE,
                },
                mouse::Cursor::Unavailable,
            );
            let pixels = crate::test_renderer::screenshot(
                &mut renderer,
                Size::new(800, 600),
                1.,
                Color::BLACK,
            );
            let mut bounds = (800, 600, 0, 0);
            for (i, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
                if pixel[..3].iter().any(|c| *c > 0) {
                    let (x, y) = (i % 800, i / 800);
                    bounds = (
                        bounds.0.min(x),
                        bounds.1.min(y),
                        bounds.2.max(x),
                        bounds.3.max(y),
                    );
                }
            }
            bounds
        };
        for reflow in [false, true] {
            let mut viewer = Viewer::default();
            viewer.animate_from(source.clone(), natural, false);
            let started = viewer
                .transition
                .as_ref()
                .unwrap()
                .motion
                .as_ref()
                .unwrap()
                .started;
            viewer.update(Message::Tick(started + ZOOM_DURATION));
            let expanded = bounds(&viewer);
            if reflow {
                viewer.invalidate_return();
            }
            viewer.request_close(started + ZOOM_DURATION);
            viewer.update(Message::Tick(started + ZOOM_DURATION + ZOOM_DURATION / 2));
            let closing = bounds(&viewer);
            if reflow {
                assert_eq!(
                    closing, expanded,
                    "reflow fallback fades without moving to stale coordinates"
                );
            } else {
                assert!(closing.0 < expanded.0 && closing.1 < expanded.1);
                assert!(closing.2 - closing.0 < expanded.2 - expanded.0);
                viewer.invalidate_return();
                assert_eq!(
                    bounds(&viewer),
                    closing,
                    "invalidation mid-flight must not jump back to the expanded image"
                );
                viewer.update(Message::Tick(
                    started + ZOOM_DURATION + ZOOM_DURATION * 3 / 4,
                ));
                viewer.invalidate_return();
                assert_eq!(
                    bounds(&viewer),
                    closing,
                    "repeated transcript updates must keep fading in place"
                );
            }
            viewer.close();
        }
        let mut viewer = Viewer {
            current: Some("fixture.gif".into()),
            ..Default::default()
        };
        viewer.animate_from(source, natural, true);
        let started = viewer
            .transition
            .as_ref()
            .unwrap()
            .motion
            .as_ref()
            .unwrap()
            .started;
        viewer.update(Message::Tick(started + ZOOM_DURATION));
        let before_decode = bounds(&viewer);
        viewer.update(Message::Loaded(
            viewer.generation,
            "fixture.gif".into(),
            Ok(handles(vec![Frame {
                width: 600,
                height: 200,
                rgba: vec![255; 600 * 200 * 4],
                delay: Duration::from_secs(1),
            }])),
        ));
        assert_eq!(
            bounds(&viewer),
            before_decode,
            "discovering a single-frame GIF must not snap its destination"
        );
        let retargeted = viewer
            .transition
            .as_ref()
            .unwrap()
            .controls_motion
            .as_ref()
            .unwrap()
            .started;
        viewer.update(Message::Tick(retargeted + ZOOM_DURATION / 4));
        let midway = bounds(&viewer);
        viewer.update(Message::Tick(retargeted + ZOOM_DURATION));
        let settled = bounds(&viewer);
        assert!(before_decode.1 < midway.1 && midway.1 < settled.1);
        assert_eq!(
            settled,
            (100, 200, 699, 399),
            "natural-size image lands at the window center"
        );
        assert!(
            iced_runtime::futures::subscription::into_recipes(viewer.subscription()).is_empty()
        );
    }

    #[tokio::test]
    async fn large_image_keeps_preview_until_gpu_ready_and_draws_on_first_handoff_frame() {
        use iced::{
            advanced::Renderer as _,
            futures::{FutureExt, StreamExt},
        };
        let bytes = Arc::new(include_bytes!("fixtures/large-transparent.png").to_vec());
        let dimensions = orbit_core::media::image_dimensions(&bytes).unwrap();
        let natural = Size::new(dimensions.0 as f32, dimensions.1 as f32);
        let preview = image::Handle::from_rgba(120, 80, [0, 0, 255, 255].repeat(120 * 80));
        let message = crate::image_transition::tests::activate(
            crate::image_transition::preview(
                theme::button(image(preview.clone()))
                    .padding(0)
                    .on_press(crate::ui::Message::OpenMarkdownImage("large.png".into())),
                preview,
            ),
            iced::Point::new(30., 20.),
        );
        let crate::ui::Message::OpenImageFrom(_, source) = message else {
            panic!("image activation")
        };
        let mut viewer = Viewer::default();
        let task = viewer.open_bytes("large.png", bytes);
        viewer.animate_from(source, natural, false);
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Image(iced_runtime::image::Action::Allocate(
            handle,
            sender,
        ))) = stream.next().await
        else {
            panic!("decoded images must request a GPU allocation before presentation")
        };
        // Hold back the renderer completion past the entire zoom. No timer or
        // decode completion is a substitute for actual GPU readiness.
        viewer.update(Message::Tick(Instant::now() + ZOOM_DURATION));
        assert!(stream.next().now_or_never().is_none());
        assert!(viewer.frames.is_none());
        let mut renderer = crate::test_renderer::new(16.);
        let draw = |viewer: &Viewer, renderer: &mut iced::Renderer| {
            let window = Size::new(800., 600.);
            let mut ui = iced_runtime::UserInterface::build(
                container(viewer.view(String::new(), String::new(), window.width, window.height))
                    .center_x(iced::Fill)
                    .center_y(iced::Fill),
                window,
                iced_runtime::user_interface::Cache::new(),
                renderer,
            );
            renderer.reset(Rectangle::with_size(window));
            ui.draw(
                renderer,
                &theme::app(),
                &renderer::Style {
                    text_color: Color::WHITE,
                },
                mouse::Cursor::Unavailable,
            );
            crate::test_renderer::screenshot(renderer, Size::new(800, 600), 1., Color::BLACK)
        };
        let before = draw(&viewer, &mut renderer);
        let pixel = |pixels: &[u8], x: usize| -> [u8; 3] {
            pixels[(300 * 800 + x) * 4..][..3].try_into().unwrap()
        };
        assert_eq!(pixel(&before, 200), [0, 0, 255]);
        assert_eq!(pixel(&before, 600), [0, 0, 255]);
        // Use the same asynchronous allocation API as iced's window shell.
        renderer.allocate_image(&handle, move |result| sender.send(result).unwrap());
        let message = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match stream.next().now_or_never() {
                    Some(Some(iced_runtime::Action::Output(message))) => break message,
                    Some(_) => panic!("expected GPU-ready completion"),
                    None => {
                        // Headless tests drive redraws explicitly; rendering collects
                        // the GPU worker's completion just as the window shell does.
                        let pending = draw(&viewer, &mut renderer);
                        assert_eq!(pixel(&pending, 200), [0, 0, 255]);
                        assert_eq!(pixel(&pending, 600), [0, 0, 255]);
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                }
            }
        })
        .await
        .expect("asynchronous GPU upload completed");
        viewer.update(message);
        assert!(stream.next().await.is_none());
        let after = draw(&viewer, &mut renderer);
        assert_eq!(
            pixel(&after, 200),
            [255, 0, 0],
            "full resolution must be visible in the very first handoff frame"
        );
        assert_eq!(
            pixel(&after, 600),
            [0, 0, 0],
            "transparent pixels must not expose a retained thumbnail underneath"
        );
        let pin = viewer.frames.as_ref().unwrap().as_ref().unwrap()[0]
            .allocation
            .as_ref()
            .unwrap()
            .downgrade();
        assert!(pin.upgrade().is_some());
        viewer.close();
        assert!(
            pin.upgrade().is_none(),
            "closing must release the GPU allocation"
        );
    }

    #[tokio::test]
    async fn gpu_upload_failure_is_actionable_and_late_success_cannot_reopen_viewer() {
        use iced::{advanced::image::Renderer as _, futures::StreamExt};
        for close_before_ready in [false, true] {
            let mut viewer = Viewer::default();
            let task = viewer.open_bytes(
                "fixture.png",
                Arc::new(include_bytes!("fixtures/media.png").to_vec()),
            );
            let mut stream = iced_runtime::task::into_stream(task).unwrap();
            let Some(iced_runtime::Action::Image(iced_runtime::image::Action::Allocate(
                handle,
                sender,
            ))) = stream.next().await
            else {
                panic!("allocation request")
            };
            let renderer = crate::test_renderer::new(16.);
            if close_before_ready {
                viewer.close();
                sender.send(renderer.load_image(&handle)).unwrap();
            } else {
                sender
                    .send(Err(image_primitive::Error::OutOfMemory))
                    .unwrap();
            }
            let Some(iced_runtime::Action::Output(message)) = stream.next().await else {
                panic!("allocation completion")
            };
            viewer.update(message);
            if close_before_ready {
                assert!(viewer.frames.is_none());
                assert!(viewer.current.is_none());
            } else {
                let error = viewer.frames.as_ref().unwrap().as_ref().unwrap_err();
                assert!(error.contains("not enough memory"));
                assert!(error.contains("reopen"));
            }
            assert!(
                iced_runtime::futures::subscription::into_recipes(viewer.subscription()).is_empty()
            );
        }
    }

    #[test]
    fn thumbnails_shrink_wrap_within_maximum_and_available_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let media = orbit_core::media::MediaStore::new(dir.path().into())
            .image(include_bytes!("fixtures/media.png"))
            .unwrap();
        let renderer = crate::test_renderer::new(16.);
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
        use iced::{Point, Size, advanced::Renderer as _};
        let dir = tempfile::tempdir().unwrap();
        let media = orbit_core::media::MediaStore::new(dir.path().into())
            .image(include_bytes!("fixtures/media.png"))
            .unwrap();
        let mut viewer = Viewer::default();
        viewer.thumbnails.insert(
            media.thumbnail.clone().unwrap(),
            Ok(image::Handle::from_rgba(48, 16, vec![255; 48 * 16 * 4])),
        );
        let mut renderer = crate::test_renderer::new(16.);
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
            crate::pointer_input::pointer_input(
                viewer.attachment(&media, 3),
                false,
                Default::default(),
            ),
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
            let pixels = crate::test_renderer::screenshot(
                &mut renderer,
                Size::new(size.width as u32, size.height as u32),
                1.,
                Color::BLACK,
            );
            let foreground = theme::default_colors().text.into_rgba8();
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
                let [crate::ui::Message::OpenImageFrom(message, source)] = messages.as_slice()
                else {
                    panic!("{messages:?}")
                };
                assert!(
                    matches!(message.as_ref(), crate::ui::Message::OpenImage(m) if m == &media)
                );
                assert!((source.bounds.width / source.bounds.height - 3.).abs() < 0.001);
                assert_eq!(
                    source.bounds.center(),
                    Point::new(size.width / 2., size.height / 2.)
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
        let renderer = crate::test_renderer::new(16.);
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
            let destination = modal_bounds(
                Size::new(width as f32, height as f32),
                Size::new(800., 600.),
                if animated { GIF_CONTROLS_HEIGHT } else { 0. },
            );
            assert_eq!(destination.size(), picture);
            assert_eq!(destination.x, (800. - node.size().width) / 2.);
            assert_eq!(
                destination.y,
                (600. - node.size().height) / 2.,
                "animation must land exactly on the settled image, including GIF controls"
            );
        }
    }

    #[test]
    fn long_paths_leave_room_for_image_details() {
        use iced::{
            Size,
            advanced::{layout, widget::Tree},
        };
        let renderer = crate::test_renderer::new(16.);
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
        let renderer = crate::test_renderer::new(16.);
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

    #[test]
    fn metadata_covers_fractional_edges() {
        use iced::advanced::Renderer as _;
        let mut renderer = crate::test_renderer::new(16.);
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
                screenshots.push(crate::test_renderer::screenshot(
                    &mut renderer,
                    physical,
                    scale,
                    Color::BLACK,
                ));
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
                    "dimmed metadata text on wgpu at scale {scale}: color distance {closest}"
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
                    "uncovered image edge on wgpu at scale {scale}: {}",
                    hovered[offset]
                );
            }
        }
    }

    #[test]
    fn metadata_is_hover_only_and_playback_click_does_not_dismiss() {
        use iced::{Point, Size, advanced::Renderer as _, mouse};
        let viewer = Viewer {
            frames: Some(Ok(handles(
                orbit_core::media::frames_from_bytes(
                    include_bytes!("fixtures/media.gif"),
                    &AtomicBool::new(false),
                )
                .unwrap(),
            ))),
            playing: true,
            ..Default::default()
        };
        let mut renderer = crate::test_renderer::new(16.);
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
            let pixels = crate::test_renderer::screenshot(
                &mut renderer,
                Size::new(800, 600),
                1.,
                Color::BLACK,
            );
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
        let media = orbit_core::media::MediaStore::new(dir.path().into())
            .image(include_bytes!("fixtures/media.gif"))
            .unwrap();
        let frames = handles(orbit_core::media::frames(&media.path).unwrap());
        assert_eq!(frames.len(), 2);
        let mut viewer = Viewer::default();
        let mut recent = RecentPreviews::default();
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
        let _task = viewer.prepare(&mut recent, std::slice::from_ref(&media));
        let _task = viewer.prepare(&mut recent, &[]);
        viewer.update(Message::Thumbnail(
            media.thumbnail.unwrap(),
            Some(Ok(frames[0].handle.clone())),
        ));
        assert!(viewer.thumbnails.is_empty());
        // A skipped decode that arrives after reopening must not poison the cache.
        let dir = tempfile::tempdir().unwrap();
        let media = orbit_core::media::MediaStore::new(dir.path().into())
            .image(include_bytes!("fixtures/media.png"))
            .unwrap();
        let _task = viewer.prepare(&mut recent, std::slice::from_ref(&media));
        let _task = viewer.prepare(&mut recent, &[]);
        let _task = viewer.prepare(&mut recent, std::slice::from_ref(&media));
        let thumbnail = media.thumbnail.clone().unwrap();
        viewer.update(Message::Thumbnail(thumbnail.clone(), None));
        assert!(!viewer.thumbnails.contains_key(&thumbnail));
        assert!(!viewer.pending.contains(&thumbnail));
        let _retry = viewer.prepare(&mut recent, std::slice::from_ref(&media));
        assert!(viewer.pending.contains(&thumbnail));
        let handle = frames[0].handle.clone();
        viewer.update(Message::Thumbnail(
            thumbnail.clone(),
            Some(Ok(handle.clone())),
        ));
        let _ = viewer.prepare(&mut recent, &[]);
        let hit = viewer.prepare(&mut recent, &[media]);
        assert!(iced_runtime::task::into_stream(hit).is_none());
        assert_eq!(
            viewer.thumbnails[&thumbnail].as_ref().unwrap().id(),
            handle.id()
        );
    }
    #[tokio::test]
    async fn ephemeral_gif_bytes_use_the_existing_modal_playback_and_error_states() {
        use iced::futures::StreamExt;
        let mut viewer = Viewer::default();
        let renderer = crate::test_renderer::new(16.);
        let bytes = Arc::new(include_bytes!("fixtures/media.gif").to_vec());
        let task = viewer.open_bytes("https://example.com/animation.gif", bytes);
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        while let Some(action) = stream.next().await {
            match action {
                iced_runtime::Action::Output(message) => viewer.update(message),
                iced_runtime::Action::Image(iced_runtime::image::Action::Allocate(
                    handle,
                    sender,
                )) => {
                    use image_primitive::Renderer as _;
                    sender.send(renderer.load_image(&handle)).unwrap();
                }
                _ => {}
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
            match action {
                iced_runtime::Action::Output(message) => viewer.update(message),
                iced_runtime::Action::Image(iced_runtime::image::Action::Allocate(
                    handle,
                    sender,
                )) => {
                    use image_primitive::Renderer as _;
                    sender.send(renderer.load_image(&handle)).unwrap();
                }
                _ => {}
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
        viewer.update(Message::Tick(now));
        assert_eq!(viewer.index, 1);
        viewer.update(Message::Tick(now + Duration::from_millis(499)));
        assert_eq!(viewer.index, 1);
        viewer.update(Message::Tick(now + Duration::from_millis(500)));
        assert_eq!(viewer.index, 0);
    }
}
