//! Images decode off the UI thread. Chat uses static thumbnails; only the modal animates.
use iced::{
    Element, Fill, Subscription, Task,
    widget::{button, column, container, image, row, scrollable, text},
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
    ActualSize,
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
    actual_size: bool,
    wanted: Arc<Mutex<HashSet<PathBuf>>>,
    generation: u64,
    cancel: Arc<AtomicBool>,
}
static DECODERS: LazyLock<Arc<tokio::sync::Semaphore>> =
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
            Some(Ok(handle)) => button(
                image(handle.clone())
                    .width(320)
                    .height(220)
                    .content_fit(iced::ContentFit::Contain),
            )
            .on_press(crate::ui::Message::OpenImage(media.clone()))
            .style(button::text)
            .into(),
            Some(Err(error)) => text(format!("Preview unavailable: {error}"))
                .size(12)
                .into(),
            None => text("Loading image preview…").size(12).into(),
        }
    }
    pub fn open(&mut self, media: &Media) -> Task<Message> {
        self.close();
        self.current = Some(media.path.clone());
        let path = media.path.clone();
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
                    pico_core::media::frames_cancellable(&input, &cancel)
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
        self.actual_size = false;
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
            Message::ActualSize => self.actual_size = !self.actual_size,
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
    pub fn view(&self, media: &Media, width: f32, height: f32) -> Element<'_, crate::ui::Message> {
        let mut body = column![
            row![
                text(media.path.display().to_string()).size(14).width(Fill),
                button("Close").on_press(crate::ui::Message::Dismiss)
            ]
            .spacing(12),
            text(format!(
                "{} × {} · {} bytes",
                media.width, media.height, media.bytes
            ))
            .size(12)
        ]
        .spacing(12);
        match &self.frames {
            None => body = body.push(text("Loading image…")),
            Some(Err(error)) => body = body.push(text(format!("Could not open image: {error}"))),
            Some(Ok(frames)) => {
                let handle = frames[self.index].handle.clone();
                let picture: Element<'_, crate::ui::Message> = if self.actual_size {
                    scrollable(image(handle))
                        .direction(scrollable::Direction::Both {
                            vertical: scrollable::Scrollbar::new(),
                            horizontal: scrollable::Scrollbar::new(),
                        })
                        .width(Fill)
                        .height(Fill)
                        .into()
                } else {
                    image(handle)
                        .width(Fill)
                        .height(Fill)
                        .content_fit(iced::ContentFit::Contain)
                        .into()
                };
                body = body.push(picture);
                let mut controls = row![
                    button(if self.actual_size {
                        "Fit to window"
                    } else {
                        "Actual size"
                    })
                    .on_press(crate::ui::Message::Image(Message::ActualSize))
                ]
                .spacing(12);
                if frames.len() > 1 {
                    controls = controls
                        .push(
                            button(if self.playing { "Pause" } else { "Play" })
                                .on_press(crate::ui::Message::Image(Message::Toggle)),
                        )
                        .push(text(format!("Frame {} / {}", self.index + 1, frames.len())));
                }
                body = body.push(controls);
            }
        }
        container(body)
            .padding(20)
            .width((width - 48.).max(240.))
            .height((height - 48.).max(240.))
            .style(container::rounded_box)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
