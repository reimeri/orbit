//! Native Wayland image clipboard input. All reads/decodes run on the I/O worker.
use orbit_core::media::{MAX_BYTES, Media, MediaStore};
use std::{path::Path, time::Duration};

#[derive(Clone, Debug)]
pub enum Input {
    Image(Media),
    Text(String),
    Empty,
}

pub fn file(root: &Path, path: &Path) -> Result<Input, String> {
    let bytes = orbit_core::media::read(path).map_err(|e| format!("{e:#}"))?;
    image(root, &bytes)
}
fn image(root: &Path, bytes: &[u8]) -> Result<Input, String> {
    MediaStore::new(root.to_owned())
        .image(bytes)
        .map(Input::Image)
        .map_err(|e| format!("{e:#}"))
}
pub fn clipboard(root: &Path, text: Option<String>) -> Result<Input, String> {
    let reply =
        winit::platform::wayland::request_image_clipboard(MAX_BYTES).map_err(|e| e.to_string())?;
    match reply.recv_timeout(Duration::from_secs(12)).map_err(|e| format!("Could not read clipboard: {e}. Copy the image again or drop its file into the chat."))?.map_err(|e| e.to_string())? {
        Some(bytes) => image(root, &bytes),
        None => Ok(text.map_or(Input::Empty, Input::Text)),
    }
}

// Exercise the exact backend URI parser in the normal workspace test suite.
#[cfg(test)]
mod uri_list {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.build-deps/winit/src/platform_impl/linux/wayland/data_device/uri_list.rs"
    ));
}
