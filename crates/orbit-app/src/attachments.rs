//! Native Wayland image clipboard input. Snapshot the offer on paste; wait and decode off the UI and storage threads.
use orbit_core::media::{MAX_BYTES, Media, MediaStore};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug)]
pub enum Input {
    Image(Media),
    Text(String),
    Empty,
}

pub enum Source {
    File(PathBuf),
    Clipboard {
        offer: Result<std::sync::mpsc::Receiver<std::io::Result<Option<Vec<u8>>>>, String>,
        text: tokio::sync::oneshot::Receiver<Option<String>>,
    },
}

impl Source {
    pub fn clipboard(text: tokio::sync::oneshot::Receiver<Option<String>>) -> Self {
        Self::Clipboard {
            offer: winit::platform::wayland::request_image_clipboard(MAX_BYTES)
                .map_err(|e| e.to_string()),
            text,
        }
    }

    pub fn prepare(self, root: &Path) -> Result<Input, String> {
        match self {
            Self::File(path) => {
                let bytes = orbit_core::media::read(&path).map_err(|e| format!("{e:#}"))?;
                image(root, &bytes)
            }
            Self::Clipboard { offer, text } => {
                let bytes = offer?.recv_timeout(Duration::from_secs(12))
                    .map_err(|e| format!("Could not read clipboard: {e}. Copy the image again or drop its file into the chat."))?
                    .map_err(|e| e.to_string())?;
                match bytes {
                    Some(bytes) => image(root, &bytes),
                    None => Ok(text
                        .blocking_recv()
                        .map_err(|e| {
                            format!("Could not read clipboard text: {e}. Copy it again and retry.")
                        })?
                        .map_or(Input::Empty, Input::Text)),
                }
            }
        }
    }
}

fn image(root: &Path, bytes: &[u8]) -> Result<Input, String> {
    MediaStore::new(root.to_owned())
        .image(bytes)
        .map(Input::Image)
        .map_err(|e| format!("{e:#}"))
}
// Exercise the exact backend URI parser in the normal workspace test suite.
#[cfg(test)]
mod uri_list {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.build-deps/winit/src/platform_impl/linux/wayland/data_device/uri_list.rs"
    ));
}
