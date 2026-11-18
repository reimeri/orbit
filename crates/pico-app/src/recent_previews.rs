//! Shared offscreen cache. An entry lives here or in its visible cache, never both.
use crate::markdown_images::Loaded;
use iced::widget::image::Handle;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Arc,
};

enum Preview {
    Markdown(Arc<Loaded>),
    Saved(PathBuf, Handle),
}

impl Preview {
    fn bytes(&self) -> usize {
        match self {
            Self::Markdown(image) => image.retained_bytes(),
            Self::Saved(_, Handle::Rgba { pixels, .. }) => pixels.len(),
            Self::Saved(_, _) => unreachable!("thumbnail decoder produces RGBA handles"),
        }
    }
}

#[derive(Default)]
pub struct RecentPreviews {
    // Bound metadata and the linear lookup cost even for tiny images.
    entries: VecDeque<Preview>,
    bytes: usize,
}

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;

impl RecentPreviews {
    fn push(&mut self, preview: Preview) {
        let size = preview.bytes();
        if size > MAX_BYTES {
            return;
        }
        while self.bytes + size > MAX_BYTES || self.entries.len() >= MAX_ENTRIES {
            self.bytes -= self.entries.pop_front().expect("oldest preview").bytes();
        }
        self.entries.push_back(preview);
        self.bytes += size;
    }

    fn take(&mut self, matches: impl Fn(&Preview) -> bool) -> Option<Preview> {
        let i = self.entries.iter().position(matches)?;
        let preview = self.entries.remove(i).expect("matched preview");
        self.bytes -= preview.bytes();
        Some(preview)
    }

    pub fn store_markdown(&mut self, image: Arc<Loaded>) {
        self.push(Preview::Markdown(image));
    }

    pub fn take_markdown(&mut self, url: &str) -> Option<Arc<Loaded>> {
        match self.take(|entry| matches!(entry, Preview::Markdown(image) if image.url == url))? {
            Preview::Markdown(image) => Some(image),
            _ => unreachable!(),
        }
    }

    pub fn clear_markdown(&mut self) {
        self.entries
            .retain(|entry| matches!(entry, Preview::Saved(..)));
        self.bytes = self.entries.iter().map(Preview::bytes).sum();
    }

    pub fn store_saved(&mut self, path: PathBuf, handle: Handle) {
        self.push(Preview::Saved(path, handle));
    }

    pub fn take_saved(&mut self, path: &Path) -> Option<Handle> {
        match self.take(|entry| matches!(entry, Preview::Saved(key, _) if key == path))? {
            Preview::Saved(_, handle) => Some(handle),
            _ => unreachable!(),
        }
    }
}
