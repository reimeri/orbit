//! Visible-only, ephemeral Markdown previews. Never writes to session media storage.
use iced::{
    Element, Task,
    widget::{button, column, container, image, text},
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, LazyLock},
};

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
pub struct Message {
    url: String,
    generation: u64,
    result: Result<Arc<Loaded>, String>,
}
struct Slot {
    generation: u64,
    result: Option<Result<Arc<Loaded>, String>>,
    _task: iced::task::Handle,
}
#[derive(Default)]
pub struct Cache {
    session: Option<String>,
    generation: u64,
    slots: HashMap<String, Slot>,
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
            let destination = url.clone();
            let (task, handle) = Task::perform(
                async move {
                    let result = load(destination.clone(), source).await;
                    Message {
                        url: destination,
                        generation,
                        result,
                    }
                },
                |message| message,
            )
            .abortable();
            self.slots.insert(
                url,
                Slot {
                    generation,
                    result: None,
                    _task: handle.abort_on_drop(),
                },
            );
            tasks.push(task);
        }
        Task::batch(tasks)
    }
    pub fn update(&mut self, mut message: Message) {
        let retained: usize = self
            .slots
            .values()
            .filter_map(|slot| slot.result.as_ref()?.as_ref().ok())
            .map(|loaded| loaded.bytes.len() + loaded.preview_bytes)
            .sum();
        if let Ok(loaded) = &message.result
            && retained + loaded.bytes.len() + loaded.preview_bytes
                > pico_core::media::MAX_ANIMATION_BYTES
        {
            message.result = Err("Visible Markdown images exceed the 256 MiB cache limit. Use fewer/smaller images, or scroll other image messages out of view and retry.".into());
        }
        if let Some(slot) = self.slots.get_mut(&message.url)
            && slot.generation == message.generation
        {
            slot.result = Some(message.result);
        }
    }
    pub fn loaded(&self, url: &str) -> Option<Arc<Loaded>> {
        self.slots.get(url)?.result.as_ref()?.as_ref().ok().cloned()
    }
    pub fn retry(&mut self, url: &str) {
        self.slots.remove(url);
    }
    pub fn view(&self, url: &str) -> Element<'static, crate::ui::Message> {
        match self.slots.get(url).and_then(|s| s.result.as_ref()) {
            Some(Ok(loaded)) => button(
                container(
                    image(loaded.preview.clone())
                        .width(iced::Length::Fill)
                        .height(iced::Length::Shrink)
                        .content_fit(iced::ContentFit::Contain),
                )
                .max_height(440),
            )
            .on_press(crate::ui::Message::OpenMarkdownImage(url.to_owned()))
            .style(button::text)
            .into(),
            Some(Err(error)) => column![
                text(format!("Image unavailable: {error}")).size(12),
                button("Retry image")
                    .on_press(crate::ui::Message::RetryMarkdownImage(url.to_owned()))
                    .style(button::text),
            ]
            .spacing(4)
            .into(),
            None => text("Loading image…").size(12).into(),
        }
    }
}
async fn load(
    url: String,
    source: Result<pico_core::markdown_image::Source, String>,
) -> Result<Arc<Loaded>, String> {
    let source = source?;
    // Slow HTTP requests must not occupy the modal/tool decoder workers.
    let loading = LOADERS
        .clone()
        .acquire_owned()
        .await
        .expect("image loader semaphore");
    let bytes = source.read().await.map_err(|e| format!("{e:#}"))?;
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
        let task = cache.prepare(Some(("first", dir.path())), one_url());
        let stale_generation = cache.slots["image.gif"].generation;
        finish(&mut cache, task).await;
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
        cache.update(Message {
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
            cache.update(Message {
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
        cache.update(Message {
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
