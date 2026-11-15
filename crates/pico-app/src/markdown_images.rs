//! Visible-only, ephemeral Markdown previews. Never writes to session media storage.
use crate::theme;
use iced::{
    Element, Length, Size, Task,
    widget::{container, image, responsive, space, text, tooltip},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::{Arc, LazyLock},
};

const MAX_PREVIEW_WIDTH: f32 = 640.;
const MAX_PREVIEW_HEIGHT: f32 = 440.;

static LOADERS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

#[derive(Debug)]
pub struct Loaded {
    pub url: String,
    pub bytes: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    preview: image::Handle,
    preview_bytes: usize,
}
#[derive(Clone, Debug)]
pub enum Message {
    Dimensions {
        url: String,
        generation: u64,
        dimensions: (u32, u32),
    },
    Loaded {
        url: String,
        generation: u64,
        result: Result<Arc<Loaded>, String>,
    },
}
struct Slot {
    generation: u64,
    source: Option<pico_core::markdown_image::Source>,
    dimensions: Option<(u32, u32)>,
    result: Option<Result<Arc<Loaded>, String>>,
    _task: iced::task::Handle,
}
#[derive(Default)]
pub struct Cache {
    session: Option<String>,
    generation: u64,
    slots: HashMap<String, Slot>,
    // Only small geometry records survive viewport/session eviction, never pixels.
    dimensions: VecDeque<(pico_core::markdown_image::Source, (u32, u32))>,
}
impl Cache {
    pub fn prepare(
        &mut self,
        session: Option<(&str, &Path)>,
        urls: HashSet<String>,
    ) -> Task<Message> {
        let id = session.map(|(id, _)| id);
        if self.session.as_deref() != id {
            self.slots.clear();
            self.session = id.map(str::to_owned);
        }
        self.slots.retain(|url, _| urls.contains(url));
        let Some((_, workspace)) = session else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        for url in urls {
            if self.slots.contains_key(&url) {
                continue;
            }
            self.generation = self.generation.wrapping_add(1);
            let generation = self.generation;
            let source = pico_core::markdown_image::Source::resolve(workspace, &url)
                .map_err(|e| format!("{e:#}"));
            let dimensions = source.as_ref().ok().and_then(|key| {
                let index = self
                    .dimensions
                    .iter()
                    .position(|(source, _)| source == key)?;
                let entry = self.dimensions.remove(index).expect("cached dimensions");
                let dimensions = entry.1;
                self.dimensions.push_back(entry);
                Some(dimensions)
            });
            let key = source.as_ref().ok().cloned();
            let destination = url.clone();
            let (sender, receiver) = iced::futures::channel::mpsc::unbounded();
            let notify_url = url.clone();
            let loading = Task::perform(
                async move {
                    let result = load(destination.clone(), source, move |dimensions| {
                        // A dropped receiver means the preview left the viewport.
                        let _ = sender.unbounded_send(Message::Dimensions {
                            url: notify_url.clone(),
                            generation,
                            dimensions,
                        });
                    })
                    .await;
                    Message::Loaded {
                        url: destination,
                        generation,
                        result,
                    }
                },
                |message| message,
            );
            let (task, handle) =
                Task::batch([Task::run(receiver, |message| message), loading]).abortable();
            self.slots.insert(
                url,
                Slot {
                    generation,
                    source: key,
                    dimensions,
                    result: None,
                    _task: handle.abort_on_drop(),
                },
            );
            tasks.push(task);
        }
        Task::batch(tasks)
    }
    pub fn update(&mut self, message: Message) {
        let (url, generation) = match &message {
            Message::Dimensions {
                url, generation, ..
            }
            | Message::Loaded {
                url, generation, ..
            } => (url, *generation),
        };
        if !self
            .slots
            .get(url)
            .is_some_and(|slot| slot.generation == generation)
        {
            return;
        }
        let dimensions = match &message {
            Message::Dimensions { dimensions, .. } => Some(*dimensions),
            Message::Loaded { result, .. } => result
                .as_ref()
                .ok()
                .map(|loaded| (loaded.width, loaded.height)),
        };
        if let Some(dimensions) = dimensions {
            let slot = self.slots.get_mut(url).expect("current slot");
            slot.dimensions = Some(dimensions);
            if let Some(source) = &slot.source {
                self.dimensions.retain(|(key, _)| key != source);
                self.dimensions.push_back((source.clone(), dimensions));
                if self.dimensions.len() > 256 {
                    self.dimensions.pop_front();
                }
            }
        }
        if let Message::Loaded {
            url, mut result, ..
        } = message
        {
            let retained: usize = self
                .slots
                .values()
                .filter_map(|slot| slot.result.as_ref()?.as_ref().ok())
                .map(|loaded| loaded.bytes.len() + loaded.preview_bytes)
                .sum();
            if let Ok(loaded) = &result
                && retained + loaded.bytes.len() + loaded.preview_bytes
                    > pico_core::media::MAX_ANIMATION_BYTES
            {
                result = Err("Visible Markdown images exceed the 256 MiB cache limit. Use fewer/smaller images, or scroll other image messages out of view and retry.".into());
            }
            self.slots.get_mut(&url).expect("current slot").result = Some(result);
        }
    }
    pub fn loaded(&self, url: &str) -> Option<Arc<Loaded>> {
        self.slots.get(url)?.result.as_ref()?.as_ref().ok().cloned()
    }
    pub fn retry(&mut self, url: &str) {
        self.slots.remove(url);
    }
    pub fn view(&self, url: &str) -> Element<'static, crate::ui::Message> {
        let slot = self.slots.get(url);
        let dimensions = slot.and_then(|s| s.dimensions).unwrap_or((640, 440));
        let result = slot.and_then(|s| s.result.clone());
        let url = url.to_owned();
        // Resolve the same box in every state and at every chat width. Pixel
        // upload, decoding, errors and retry never control the widget's size.
        responsive(move |available| {
            let (width, height) = (dimensions.0 as f32, dimensions.1 as f32);
            let scale =
                (available.width.min(MAX_PREVIEW_WIDTH) / width).min(MAX_PREVIEW_HEIGHT / height);
            let size = Size::new(width * scale, height * scale);
            let content: Element<'_, crate::ui::Message> = match &result {
                Some(Ok(loaded)) => theme::button(
                    image(loaded.preview.clone())
                        .width(size.width)
                        .height(size.height)
                        .content_fit(iced::ContentFit::Contain),
                )
                .padding(0)
                .on_press(crate::ui::Message::OpenMarkdownImage(url.clone()))
                .style(theme::text_button)
                .into(),
                // The whole rectangle retries, even if the aspect ratio leaves
                // too little room for a separate padded button. Details live in
                // an overlay, not in the chat flow.
                Some(Err(error)) => tooltip(
                    theme::button(text("Image unavailable · Retry image").size(12))
                        .padding(0)
                        .width(size.width)
                        .height(size.height)
                        .on_press(crate::ui::Message::RetryMarkdownImage(url.clone()))
                        .style(theme::danger),
                    container(
                        text(format!(
                            "Image unavailable: {error}\nClick the rectangle to retry."
                        ))
                        .size(12),
                    )
                    .max_width(360)
                    .padding(8),
                    tooltip::Position::FollowCursor,
                )
                .style(theme::footer_surface)
                .into(),
                None => container(space())
                    .width(size.width)
                    .height(size.height)
                    .style(theme::card)
                    .into(),
            };
            content
        })
        .height(Length::Shrink)
        .into()
    }
}

async fn load(
    url: String,
    source: Result<pico_core::markdown_image::Source, String>,
    dimensions: impl Fn((u32, u32)) + Send + Sync + 'static,
) -> Result<Arc<Loaded>, String> {
    let source = source?;
    // Slow HTTP requests must not occupy the modal/tool decoder workers.
    let loading = LOADERS
        .clone()
        .acquire_owned()
        .await
        .expect("image loader semaphore");
    let bytes = source
        .read_with_dimensions(dimensions)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let permit = crate::image_view::DECODERS
        .clone()
        .acquire_owned()
        .await
        .expect("decoder semaphore");
    tokio::task::spawn_blocking(move || {
        let _loading = loading;
        let _permit = permit;
        let (decoded, _) = pico_core::media::decode_image(&bytes).map_err(|e| format!("{e:#}"))?;
        let (width, height) = (decoded.width(), decoded.height());
        let preview = decoded.thumbnail(640, 440).into_rgba8();
        Ok(Arc::new(Loaded {
            url,
            bytes: Arc::new(bytes),
            width,
            height,
            preview_bytes: preview.len(),
            preview: image::Handle::from_rgba(
                preview.width(),
                preview.height(),
                preview.into_raw(),
            ),
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Traverse the rendered items, not Content::images (which retains stale streaming URLs).
pub fn urls(content: &iced::widget::markdown::Content) -> HashSet<String> {
    use iced::widget::markdown;
    #[derive(Default)]
    struct Collector(std::cell::RefCell<HashSet<String>>);
    impl<'a> markdown::Viewer<'a, ()> for Collector {
        fn on_link_click(_: String) {}
        fn image(
            &self,
            _: markdown::Settings,
            url: &'a String,
            _: &'a str,
            _: &markdown::Text,
        ) -> Element<'a, ()> {
            self.0.borrow_mut().insert(url.clone());
            iced::widget::space().into()
        }
    }
    let collector = Collector::default();
    let _ = markdown::view_with(
        content.items(),
        markdown::Settings::from(iced::Theme::Dark),
        &collector,
    );
    collector.0.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{futures::StreamExt, widget::markdown};

    async fn finish(cache: &mut Cache, task: Task<Message>) {
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            while let Some(action) = stream.next().await {
                if let iced_runtime::Action::Output(message) = action {
                    cache.update(message);
                }
            }
        }
    }
    fn one_url() -> HashSet<String> {
        HashSet::from(["image.gif".into()])
    }

    #[tokio::test]
    async fn previews_reload_sources_retry_failures_and_reject_stale_session_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.gif");
        std::fs::write(&path, include_bytes!("fixtures/media.gif")).unwrap();
        let mut cache = Cache::default();
        let decoders = crate::image_view::DECODERS
            .clone()
            .acquire_many_owned(2)
            .await
            .unwrap();
        let task = cache.prepare(Some(("first", dir.path())), one_url());
        let stale_generation = cache.slots["image.gif"].generation;
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let first = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap();
        let iced_runtime::Action::Output(message) = first else {
            panic!("expected metadata output")
        };
        assert!(matches!(
            &message,
            Message::Dimensions {
                dimensions: (96, 64),
                ..
            }
        ));
        cache.update(message);
        assert!(cache.loaded("image.gif").is_none());
        assert_eq!(cache.slots["image.gif"].dimensions, Some((96, 64)));
        drop(decoders);
        while let Some(action) = stream.next().await {
            if let iced_runtime::Action::Output(message) = action {
                cache.update(message);
            }
        }
        let loaded = cache.slots["image.gif"]
            .result
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(&*loaded.bytes, include_bytes!("fixtures/media.gif"));
        // GIF logical-screen header: width 0x0060, height 0x0040.
        assert_eq!((loaded.width, loaded.height), (96, 64));
        let task = cache.prepare(Some(("first", dir.path())), one_url());
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            assert!(
                stream.next().await.is_none(),
                "visible images must not refetch on every layout"
            );
        }
        // A second session with the same relative URL must not inherit the image.
        std::fs::remove_file(&path).unwrap();
        let task = cache.prepare(Some(("second", dir.path())), one_url());
        cache.update(Message::Loaded {
            url: "image.gif".into(),
            generation: stale_generation,
            result: Ok(loaded),
        });
        assert!(cache.slots["image.gif"].result.is_none());
        finish(&mut cache, task).await;
        assert!(cache.slots["image.gif"].result.as_ref().unwrap().is_err());
        std::fs::write(&path, include_bytes!("fixtures/media.gif")).unwrap();
        cache.retry("image.gif");
        let task = cache.prepare(Some(("second", dir.path())), one_url());
        finish(&mut cache, task).await;
        assert!(cache.slots["image.gif"].result.as_ref().unwrap().is_ok());
        let _ = cache.prepare(None, HashSet::new());
        assert!(cache.slots.is_empty());
        // Evict an in-flight request as well; it must never restore its slot.
        let task = cache.prepare(Some(("second", dir.path())), one_url());
        let _ = cache.prepare(None, HashSet::new());
        finish(&mut cache, task).await;
        assert!(cache.slots.is_empty());
    }

    fn layout(cache: &Cache, width: f32) -> (iced::Rectangle, f32) {
        use iced::advanced::{layout, widget::Tree};
        let renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.),
        ));
        let mut element: Element<'_, crate::ui::Message> =
            iced::widget::column![cache.view("image.gif"), space().height(13)]
                .spacing(7)
                .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, f32::INFINITY)),
        );
        // The responsive wrapper fills the row; its child is the actual image rectangle.
        (
            node.children()[0].children()[0].bounds(),
            node.children()[1].bounds().y,
        )
    }

    #[test]
    fn metadata_placeholder_loaded_error_and_retry_have_identical_layout_at_each_width() {
        for dimensions in [(1200, 600), (300, 1200), (6000, 100)] {
            let mut cache = Cache::default();
            let _task = cache.prepare(Some(("session", Path::new("/workspace"))), one_url());
            let generation = cache.slots["image.gif"].generation;
            cache.update(Message::Dimensions {
                url: "image.gif".into(),
                generation,
                dimensions,
            });
            let wide = layout(&cache, 700.);
            let narrow_width = wide.0.width / 2.;
            let loading = [wide, layout(&cache, narrow_width)];
            for (actual, available) in loading.iter().zip([700., narrow_width]) {
                let width_limit = available.min(MAX_PREVIEW_WIDTH);
                assert!(actual.0.width > 0. && actual.0.width <= width_limit + 0.001);
                assert!(actual.0.height > 0. && actual.0.height <= MAX_PREVIEW_HEIGHT + 0.001);
                assert!(
                    (actual.0.width - width_limit).abs() < 0.001
                        || (actual.0.height - MAX_PREVIEW_HEIGHT).abs() < 0.001,
                    "the preview fills its constraining axis"
                );
                assert!(
                    (actual.0.width / actual.0.height - dimensions.0 as f32 / dimensions.1 as f32)
                        .abs()
                        < 0.001
                );
                // Seven pixels is the explicit spacing in this test's surrounding column.
                assert!((actual.1 - actual.0.height - 7.).abs() < 0.001);
            }
            assert!(loading[1].0.width < loading[0].0.width);
            assert!(loading[1].0.height < loading[0].0.height);
            cache.update(Message::Loaded {
                url: "image.gif".into(),
                generation,
                result: Ok(Arc::new(Loaded {
                    url: "image.gif".into(),
                    bytes: Arc::new(vec![]),
                    width: dimensions.0,
                    height: dimensions.1,
                    preview: image::Handle::from_rgba(1, 1, vec![255; 4]),
                    preview_bytes: 4,
                })),
            });
            assert_eq!(
                [layout(&cache, 700.), layout(&cache, narrow_width)],
                loading
            );
            cache.update(Message::Loaded {
                url: "image.gif".into(),
                generation,
                result: Err("Source disappeared".into()),
            });
            assert_eq!(
                [layout(&cache, 700.), layout(&cache, narrow_width)],
                loading
            );
            // Even a very short failed panorama remains clickable;
            // error details must not force a taller in-flow layout.
            let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
                iced::Font::DEFAULT,
                iced::Pixels(16.),
            ));
            let mut ui = iced_runtime::UserInterface::build(
                cache.view("image.gif"),
                Size::new(narrow_width, 800.),
                iced_runtime::user_interface::Cache::default(),
                &mut renderer,
            );
            let mut messages = Vec::new();
            ui.update(
                &[
                    iced::Event::Mouse(iced::mouse::Event::ButtonPressed(
                        iced::mouse::Button::Left,
                    )),
                    iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                        iced::mouse::Button::Left,
                    )),
                ],
                iced::mouse::Cursor::Available(iced::Point::new(
                    loading[1].0.center_x(),
                    loading[1].0.center_y(),
                )),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            assert!(
                matches!(messages.as_slice(), [crate::ui::Message::RetryMarkdownImage(url)] if url == "image.gif")
            );
            cache.retry("image.gif");
            let _task = cache.prepare(Some(("session", Path::new("/workspace"))), one_url());
            assert_eq!(
                [layout(&cache, 700.), layout(&cache, narrow_width)],
                loading
            );
        }
    }

    #[test]
    fn geometry_survives_session_eviction_without_reusing_pixels_or_other_workspaces() {
        let mut cache = Cache::default();
        let _task = cache.prepare(Some(("first", Path::new("/workspace/a"))), one_url());
        let old = cache.slots["image.gif"].generation;
        cache.update(Message::Dimensions {
            url: "image.gif".into(),
            generation: old,
            dimensions: (900, 300),
        });
        let expected = layout(&cache, 500.);
        let _task = cache.prepare(None, HashSet::new());
        let _task = cache.prepare(Some(("second", Path::new("/workspace/a"))), one_url());
        assert_eq!(layout(&cache, 500.), expected);
        assert!(
            cache.loaded("image.gif").is_none(),
            "source must still be reloaded"
        );
        let _task = cache.prepare(Some(("third", Path::new("/workspace/b"))), one_url());
        assert!(cache.slots["image.gif"].dimensions.is_none());
        cache.update(Message::Dimensions {
            url: "image.gif".into(),
            generation: old,
            dimensions: (300, 900),
        });
        assert!(
            cache.slots["image.gif"].dimensions.is_none(),
            "stale metadata cannot cross sessions"
        );
        let generation = cache.slots["image.gif"].generation;
        cache.update(Message::Dimensions {
            url: "image.gif".into(),
            generation,
            dimensions: (50, 150),
        });
        let _task = cache.prepare(None, HashSet::new());
        let _task = cache.prepare(Some(("first", Path::new("/workspace/a"))), one_url());
        assert_eq!(layout(&cache, 500.), expected);
    }

    #[test]
    fn retained_originals_are_bounded_across_images_and_eviction_allows_retry() {
        let mut cache = Cache::default();
        let urls: HashSet<_> = ["a", "b", "c", "d"].map(str::to_owned).into();
        let _task = cache.prepare(Some(("session", Path::new("/workspace"))), urls.clone());
        // Each input is below the per-image limit. Three fit the total budget,
        // but four do not; sharing the fixture allocation keeps the test cheap.
        let mut bytes = include_bytes!("fixtures/media.png").to_vec();
        bytes.resize(pico_core::media::MAX_BYTES, 0);
        let bytes = Arc::new(bytes);
        let loaded = |url: &str| {
            Arc::new(Loaded {
                url: url.into(),
                bytes: bytes.clone(),
                width: 1,
                height: 1,
                preview: image::Handle::from_rgba(1, 1, vec![255; 4]),
                preview_bytes: 4,
            })
        };
        for url in ["a", "b", "c", "d"] {
            cache.update(Message::Loaded {
                url: url.into(),
                generation: cache.slots[url].generation,
                result: Ok(loaded(url)),
            });
        }
        for url in ["a", "b", "c"] {
            assert!(cache.loaded(url).is_some());
        }
        assert!(cache.loaded("d").is_none());
        assert!(
            cache.slots["d"]
                .result
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap_err()
                .contains("256 MiB")
        );
        let _task = cache.prepare(
            Some(("session", Path::new("/workspace"))),
            urls.into_iter().filter(|url| url != "a").collect(),
        );
        cache.retry("d");
        let _task = cache.prepare(
            Some(("session", Path::new("/workspace"))),
            ["b", "c", "d"].map(str::to_owned).into(),
        );
        cache.update(Message::Loaded {
            url: "d".into(),
            generation: cache.slots["d"].generation,
            result: Ok(loaded("d")),
        });
        assert!(cache.loaded("d").is_some());
    }

    #[test]
    fn discovers_nested_and_reference_images_but_not_links_or_code() {
        let content = markdown::Content::parse(
            "![first](one.png)\n\n> ![quote](two.png)\n\n- ![list][ref]\n\n[ref]: three.png\n\n| image | other |\n| --- | --- |\n| ![table](four.png) | [link](not-image.png) |\n\n`![code](ignored.png)`\n",
        );
        assert_eq!(
            urls(&content),
            HashSet::from(["one.png", "two.png", "three.png", "four.png"].map(str::to_owned))
        );
        let mut streaming = markdown::Content::parse("![x](old.png)");
        streaming.push_str(" extra\n\n![new](new.png)");
        assert_eq!(
            urls(&streaming),
            HashSet::from(["old.png", "new.png"].map(str::to_owned))
        );
    }
}
