//! Durable media is separate from text budgets, JavaScript values and wire payloads.
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageDecoder, ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const MAX_BYTES: usize = 79 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 10_240_000;
pub const MAX_ANIMATION_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub path: PathBuf,
    pub mime: String,
    pub bytes: usize,
    pub width: u32,
    pub height: u32,
    pub prepared: Option<PathBuf>,
    pub thumbnail: Option<PathBuf>,
}
impl Media {
    pub fn data_url(&self) -> Result<String> {
        data_url(&self.path, &self.mime)
    }
    pub fn description(&self) -> String {
        format!(
            "{} · {} bytes{}",
            self.path.display(),
            self.bytes,
            if self.width == 0 {
                String::new()
            } else {
                format!(" · {} × {}", self.width, self.height)
            }
        )
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    InputText { text: String },
    PicoMedia { media: Media },
}
#[derive(Clone, Debug, Default)]
pub struct Output {
    /// Execution outcome for the transcript, never sent as model output.
    pub failed: bool,
    pub cell_id: Option<String>,
    pub parts: Vec<Part>,
}
impl Output {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            parts: vec![Part::InputText { text: text.into() }],
            failed: false,
            cell_id: None,
        }
    }
    pub fn summary(&self) -> String {
        self.parts
            .iter()
            .map(|p| match p {
                Part::InputText { text } => text.clone(),
                Part::PicoMedia { media } => media.description(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn media(&self) -> Vec<Media> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::PicoMedia { media } => Some(media.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn history(&self) -> Value {
        if self
            .parts
            .iter()
            .all(|p| matches!(p, Part::InputText { .. }))
        {
            json!(self.summary())
        } else {
            json!(self.parts)
        }
    }
}

pub struct MediaStore {
    root: PathBuf,
    live: Mutex<bool>,
}
impl MediaStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            live: Mutex::new(true),
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Family deletion holds this lease across its SQL commit; publication uses the same lock.
    pub(crate) fn lifecycle(&self) -> std::sync::MutexGuard<'_, bool> {
        self.live.lock().expect("media lifecycle")
    }
    pub(crate) fn while_live<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let live = self.lifecycle();
        if !*live {
            bail!("Session deleted before publication");
        }
        operation()
    }
    pub fn remove(&self) -> Result<()> {
        let mut live = self.live.lock().expect("media lock");
        *live = false;
        remove(&self.root)
    }
    fn save(&self, bytes: &[u8], extension: &str) -> Result<PathBuf> {
        let live = self.live.lock().expect("media lock");
        if !*live {
            bail!("Session deleted; media will not be recreated");
        }
        fs::create_dir_all(&self.root)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        let path = self
            .root
            .join(format!("{:x}.{extension}", Sha256::digest(bytes)));
        if !path.exists() {
            crate::store::atomic_write(&path, bytes)?;
        }
        Ok(path)
    }
    pub fn image(&self, bytes: &[u8]) -> Result<Media> {
        static DECODER: Mutex<()> = Mutex::new(());
        let _decoder = DECODER.lock().expect("image decoder lock");
        let (mut decoded, format) = decode_image(bytes)?;
        let (width, height) = (decoded.width(), decoded.height());
        if format == ImageFormat::Gif {
            validate_gif(bytes)?;
        }
        let path = self.save(bytes, extension(format))?;
        // Codex unified budget: original detail, at most 6000px/10000 32px patches.
        let scale = (6000. / width.max(height) as f64)
            .min((MAX_PIXELS as f64 / (width as f64 * height as f64)).sqrt())
            .min(1.);
        let (mut w, mut h) = (
            (width as f64 * scale).floor().max(1.) as u32,
            (height as f64 * scale).floor().max(1.) as u32,
        );
        while u64::from(w.div_ceil(32)) * u64::from(h.div_ceil(32)) > 10_000 {
            if w >= h { w -= 1 } else { h -= 1 }
        }
        if (w, h) != (width, height) {
            decoded = decoded.resize_exact(w, h, image::imageops::FilterType::Triangle);
        }
        let mut png = Cursor::new(Vec::new());
        decoded.write_to(&mut png, ImageFormat::Png)?;
        let prepared = self.save(png.get_ref(), "png")?;
        let mut thumb = Cursor::new(Vec::new());
        decoded
            .thumbnail(320, 220)
            .write_to(&mut thumb, ImageFormat::Png)?;
        let thumbnail = self.save(thumb.get_ref(), "png")?;
        Ok(Media {
            path,
            mime: format.to_mime_type().into(),
            bytes: bytes.len(),
            width,
            height,
            prepared: Some(prepared),
            thumbnail: Some(thumbnail),
        })
    }
    pub fn from_url(&self, url: &str, audio: bool) -> Result<Media> {
        let (mime, bytes) = parse_data_url(url)?;
        if audio {
            if !mime.starts_with("audio/") && mime != "application/octet-stream" {
                bail!("audio() requires an audio/* base64 data URL");
            }
            let path = self.save(&bytes, "audio")?;
            Ok(Media {
                path,
                mime,
                bytes: bytes.len(),
                width: 0,
                height: 0,
                prepared: None,
                thumbnail: None,
            })
        } else {
            self.image(&bytes)
        }
    }
}
pub fn remove(root: &Path) -> Result<()> {
    match fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub(crate) fn open(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("Cannot open {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(file)
}
pub fn read(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open(path)?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        bail!("Media exceeds the {MAX_BYTES} byte limit; use a smaller file");
    }
    Ok(bytes)
}
pub fn data_url(path: &Path, mime: &str) -> Result<String> {
    Ok(format!(
        "data:{mime};base64,{}",
        STANDARD.encode(read(path)?)
    ))
}
pub fn parse_data_url(url: &str) -> Result<(String, Vec<u8>)> {
    let (header, payload) = url
        .split_once(',')
        .context("Expected a base64 data URL, not a local path or HTTP(S) URL")?;
    let mime = header
        .strip_prefix("data:")
        .and_then(|s| s.strip_suffix(";base64"))
        .context("Expected data:<mime>;base64,...; remote URLs are not supported")?;
    if payload.len() > MAX_BYTES.div_ceil(3) * 4 {
        bail!("Media data URL exceeds the {MAX_BYTES} byte limit");
    }
    let bytes = STANDARD
        .decode(payload)
        .context("Invalid base64 media data")?;
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        bail!("Media must contain 1–{MAX_BYTES} bytes");
    }
    Ok((mime.into(), bytes))
}
fn image_decoder(bytes: &[u8]) -> Result<(impl ImageDecoder + '_, ImageFormat)> {
    if bytes.len() > MAX_BYTES {
        bail!("Image exceeds {MAX_BYTES} bytes");
    }
    let format =
        image::guess_format(bytes).context("Invalid image: expected PNG, JPEG, WebP or GIF")?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP | ImageFormat::Gif
    ) {
        bail!("Unsupported image format; use PNG, JPEG, WebP or GIF");
    }
    let bytes = if format == ImageFormat::WebP {
        // The upstream WebP EXIF reader allocates the declared chunk length
        // before checking for EOF, without honoring image::Limits. Validate
        // RIFF ranges first, also when probing an incomplete download.
        let size = u32::from_le_bytes(bytes[4..8].try_into().expect("WebP signature"));
        let end = u64::from(size) + 8;
        if end > bytes.len() as u64 {
            bail!("Incomplete WebP RIFF data");
        }
        let input = &bytes[..end as usize];
        let mut chunks = input.get(12..).context("Invalid WebP RIFF size")?;
        while !chunks.is_empty() {
            let header = chunks.get(..8).context("Incomplete WebP chunk header")?;
            let size = u64::from(u32::from_le_bytes(
                header[4..8].try_into().expect("chunk header"),
            ));
            let end = 8 + size + size % 2;
            if end > chunks.len() as u64 {
                bail!("WebP chunk exceeds available RIFF data");
            }
            chunks = &chunks[end as usize..];
        }
        input
    } else {
        bytes
    };
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_ANIMATION_BYTES as u64);
    reader.limits(limits);
    let decoder = reader.into_decoder().context("Unable to decode image")?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 {
        bail!("Image dimensions must be nonzero");
    }
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        bail!("Image exceeds {MAX_PIXELS} decoded pixels; resize it first");
    }
    Ok((decoder, format))
}
/// Read display dimensions, including EXIF rotation, without decoding pixels.
/// Incomplete input may fail; callers probing a download can retry with more bytes.
pub fn image_dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    use image::metadata::Orientation;
    let (mut decoder, _) = image_decoder(bytes)?;
    let (width, height) = decoder.dimensions();
    Ok(match decoder.orientation()? {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    })
}
pub fn decode_image(bytes: &[u8]) -> Result<(image::DynamicImage, ImageFormat)> {
    let (mut decoder, format) = image_decoder(bytes)?;
    let orientation = decoder.orientation()?;
    let mut img = image::DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok((img, format))
}
fn extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::WebP => "webp",
        ImageFormat::Gif => "gif",
        _ => unreachable!("validated format"),
    }
}

/// Only the provider boundary materializes media bytes, never SQLite or text budgeting.
pub fn project(value: &mut Value, audio_supported: bool) -> Result<()> {
    match value {
        Value::Object(map) if map.get("type").and_then(Value::as_str) == Some("pico_skill") => {
            map.insert("type".into(), Value::String("input_text".into()));
        }
        Value::Object(map) if map.get("type").and_then(Value::as_str) == Some("pico_file") => {
            let file: crate::file_tags::Attachment = serde_json::from_value(map["file"].clone())?;
            *value = json!({"type":"input_text", "text":file.project()?});
        }
        Value::Object(map) if map.get("type").and_then(Value::as_str) == Some("pico_media") => {
            let media: Media = serde_json::from_value(map["media"].clone())?;
            *value = if let Some(prepared) = &media.prepared {
                json!({"type":"input_image", "image_url":data_url(prepared, "image/png")?, "detail":"original"})
            } else {
                if !audio_supported {
                    bail!(
                        "This chat model does not support audio input. Choose an audio-capable model; the audio artifact remains saved at {}",
                        media.path.display()
                    );
                }
                json!({"type":"input_audio", "audio_url":media.data_url()?})
            };
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                project(v, audio_supported)?;
            }
        }
        Value::Array(items) => {
            for v in items {
                project(v, audio_supported)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Select only images that were actually emitted into this conversation's current context.
pub fn recent(history: impl DoubleEndedIterator<Item = Value>, count: usize) -> Result<Vec<Media>> {
    let mut images = Vec::new();
    for item in history.rev() {
        let content = match item["type"].as_str() {
            Some("custom_tool_call_output" | "function_call_output") => &item["output"],
            Some("message") => &item["content"],
            _ => continue,
        };
        if let Some(parts) = content.as_array() {
            for part in parts.iter().rev() {
                if part["type"] == "pico_media" {
                    let m: Media = serde_json::from_value(part["media"].clone())?;
                    if m.prepared.is_some() {
                        images.push(m);
                        if images.len() == count {
                            images.reverse();
                            return Ok(images);
                        }
                    }
                }
            }
        }
    }
    bail!(
        "Requested the last {count} conversation images, but only {} are available. Complete or yield the cell that emits the images first, or supply absolute referenced_image_paths instead.",
        images.len()
    )
}

#[derive(Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub delay: std::time::Duration,
}
/// Decoding is called on worker threads. Bound animation allocations cumulatively.
pub fn frames(path: &Path) -> Result<Vec<Frame>> {
    frames_cancellable(path, &std::sync::atomic::AtomicBool::new(false))
}
pub fn frames_cancellable(
    path: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<Frame>> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        bail!("Image decoding cancelled");
    }
    frames_from_bytes(&read(path)?, cancel)
}
/// Decode an ephemeral image without creating a durable media artifact.
pub fn frames_from_bytes(
    bytes: &[u8],
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<Frame>> {
    use image::AnimationDecoder;
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        bail!("Image decoding cancelled");
    }
    let (first, format) = decode_image(bytes)?;
    if format != ImageFormat::Gif {
        let rgba = first.to_rgba8();
        return Ok(vec![Frame {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
            delay: std::time::Duration::ZERO,
        }]);
    }
    drop(first);
    let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_ANIMATION_BYTES as u64);
    decoder.set_limits(limits)?;
    let mut frames = Vec::new();
    let mut allocated = 0;
    for frame in decoder.into_frames() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("Image decoding cancelled");
        }
        let frame = frame?;
        let (n, d) = frame.delay().numer_denom_ms();
        let delay =
            std::time::Duration::from_secs_f64((f64::from(n) / f64::from(d) / 1000.).max(0.02));
        let rgba = frame.into_buffer();
        allocated += rgba.len();
        if allocated > MAX_ANIMATION_BYTES || frames.len() >= 512 {
            bail!(
                "Animation exceeds 512 frames or 256 MiB decoded memory; use a smaller animation"
            );
        }
        frames.push(Frame {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
            delay,
        });
    }
    if frames.is_empty() {
        bail!("GIF contains no frames");
    }
    Ok(frames)
}

// Validate every frame before accepting an animation, without retaining all its pixels.
fn validate_gif(bytes: &[u8]) -> Result<()> {
    use image::AnimationDecoder;
    let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_ANIMATION_BYTES as u64);
    decoder.set_limits(limits)?;
    let mut total = 0usize;
    for (index, frame) in decoder.into_frames().enumerate() {
        total += frame?.buffer().len();
        if index >= 512 || total > MAX_ANIMATION_BYTES {
            bail!("Animation exceeds 512 frames or 256 MiB decoded memory");
        }
    }
    Ok(())
}

/// Codex omits PCM/float WAV clips shorter than 25ms, using actual available samples.
pub(crate) fn short_wav(path: &Path) -> Result<bool> {
    fn duration(bytes: &[u8]) -> Option<f64> {
        if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
            return None;
        }
        let mut offset = 12usize;
        let mut format = None;
        while let Some(header) = bytes.get(offset..offset.checked_add(8)?) {
            let length = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
            let rest = bytes.get(offset + 8..)?;
            let chunk = &rest[..length.min(rest.len())];
            if &header[..4] == b"fmt " {
                let mut encoding = u16::from_le_bytes(chunk.get(..2)?.try_into().ok()?);
                if encoding == 0xfffe {
                    if chunk.get(26..40)?
                        != [0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]
                    {
                        return None;
                    }
                    encoding = u16::from_le_bytes(chunk.get(24..26)?.try_into().ok()?);
                }
                if encoding != 1 && encoding != 3 {
                    return None;
                }
                let rate = u32::from_le_bytes(chunk.get(4..8)?.try_into().ok()?);
                let align = u16::from_le_bytes(chunk.get(12..14)?.try_into().ok()?);
                if rate == 0 || align == 0 {
                    return None;
                }
                format = Some((rate, align));
            } else if &header[..4] == b"data" {
                let (rate, align) = format?;
                return Some((chunk.len() / usize::from(align)) as f64 / f64::from(rate));
            }
            offset = offset
                .checked_add(8)?
                .checked_add(length)?
                .checked_add(length % 2)?;
        }
        None
    }
    Ok(duration(&read(path)?).is_some_and(|s| s < 0.025))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn png() -> Vec<u8> {
        let image = image::RgbaImage::from_fn(7, 3, |x, y| {
            image::Rgba([
                (x * 31) as u8,
                (y * 83) as u8,
                47,
                if x == 2 { 0 } else { 255 },
            ])
        });
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }
    pub(crate) fn gif() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .unwrap();
            for (rgba, delay) in [([180, 40, 15, 255], 70), ([10, 120, 220, 255], 230)] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(7, 3, image::Rgba(rgba)),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(delay, 1),
                    ))
                    .unwrap();
            }
        }
        bytes
    }
    #[test]
    fn metadata_dimensions_do_not_require_pixels_and_respect_exif_rotation() {
        let png = png();
        let header_end = png.windows(4).position(|bytes| bytes == b"IDAT").unwrap() + 4;
        assert_eq!(image_dimensions(&png[..header_end]).unwrap(), (7, 3));
        assert!(
            decode_image(&png[..header_end]).is_err(),
            "header contains no complete pixel data"
        );
        assert_eq!(image_dimensions(&gif()).unwrap(), (7, 3));
        let rgb = image::RgbImage::from_pixel(11, 5, image::Rgb([30, 70, 110]));
        let mut webp = Cursor::new(Vec::new());
        rgb.write_to(&mut webp, ImageFormat::WebP).unwrap();
        assert_eq!(image_dimensions(webp.get_ref()).unwrap(), (11, 5));
        let mut jpeg = Cursor::new(Vec::new());
        rgb.write_to(&mut jpeg, ImageFormat::Jpeg).unwrap();
        // APP1: Exif + little-endian TIFF, one SHORT Orientation=6 (90° CW).
        let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let mut oriented = vec![0xff, 0xd8, 0xff, 0xe1];
        oriented.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        oriented.extend_from_slice(exif);
        oriented.extend_from_slice(&jpeg.get_ref()[2..]);
        assert_eq!(image_dimensions(&oriented).unwrap(), (5, 11));
        let (decoded, _) = decode_image(&oriented).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (5, 11));
    }

    #[test]
    fn webp_metadata_rejects_declared_chunk_sizes_before_decoder_allocation() {
        // A 1×1 extended WebP with a minimal VP8L header and a malicious EXIF
        // chunk declaring 2 GiB, despite the entire RIFF being only 52 bytes.
        let mut body =
            b"WEBPVP8X\x0a\0\0\0\x08\0\0\0\0\0\0\0\0\0VP8L\x05\0\0\0\x2f\0\0\0\0\0EXIF".to_vec();
        body.extend_from_slice(&(1u32 << 31).to_le_bytes());
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        for error in [
            image_dimensions(&bytes).unwrap_err(),
            decode_image(&bytes).unwrap_err(),
        ] {
            assert!(
                error
                    .to_string()
                    .contains("WebP chunk exceeds available RIFF data")
            );
        }
    }

    #[test]
    fn snapshots_survive_source_changes_and_project_without_private_markers() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.png");
        fs::write(&source, png()).unwrap();
        let store = MediaStore::new(dir.path().join("media"));
        let m = store.image(&read(&source).unwrap()).unwrap();
        fs::remove_file(source).unwrap();
        let serialized = serde_json::to_string(&m).unwrap();
        drop(store);
        let m: Media = serde_json::from_str(&serialized).unwrap();
        assert_eq!(fs::read(&m.path).unwrap(), png());
        assert_eq!((m.width, m.height), (7, 3));
        let mut wire = json!({"type":"custom_tool_call_output","call_id":"test","output":[{"type":"input_text","text":"before"},{"type":"pico_media","media":m}]});
        project(&mut wire, false).unwrap();
        assert_eq!(wire["output"][1]["type"], "input_image");
        assert_eq!(wire["output"][1]["detail"], "original");
        let (_, bytes) = parse_data_url(wire["output"][1]["image_url"].as_str().unwrap()).unwrap();
        assert_eq!(
            decode_image(&bytes).unwrap().0.to_rgba8().get_pixel(2, 1).0,
            [62, 83, 47, 0]
        );
        assert!(!wire.to_string().contains("pico_media"));
    }
    #[test]
    fn audio_projection_capability_and_recent_images_are_separate_from_previews() {
        let dir = tempfile::tempdir().unwrap();
        let store = MediaStore::new(dir.path().into());
        let audio = store.from_url("data:audio/wav;base64,AQID", true).unwrap();
        let mut wire = json!({"type":"pico_media","media":audio});
        assert!(
            project(&mut wire, false)
                .unwrap_err()
                .to_string()
                .contains("does not support audio")
        );
        project(&mut wire, true).unwrap();
        assert_eq!(wire["audio_url"], "data:audio/wav;base64,AQID");
        let first = store.image(&png()).unwrap();
        let last = store.image(&gif()).unwrap();
        let history = vec![
            json!({"type":"custom_tool_call_output","output":[{"type":"pico_media","media":first}]}),
            json!({"type":"notice","output":[{"type":"pico_media","media":audio}]}),
            json!({"type":"custom_tool_call_output","output":[{"type":"pico_media","media":last}]}),
        ];
        assert_eq!(
            recent(history.clone().into_iter(), 2).unwrap(),
            [first, last]
        );
        assert!(
            recent(history.into_iter(), 3)
                .unwrap_err()
                .to_string()
                .contains("only 2")
        );
    }
    #[test]
    fn gif_frames_retain_timing_and_cancellation_and_fifo_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.gif");
        fs::write(&path, gif()).unwrap();
        let frames = frames(&path).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].delay, std::time::Duration::from_millis(70));
        assert_eq!(frames[1].delay, std::time::Duration::from_millis(230));
        assert_ne!(frames[0].rgba, frames[1].rgba);
        assert!(frames_cancellable(&path, &std::sync::atomic::AtomicBool::new(true)).is_err());
        let fifo = dir.path().join("fifo");
        nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
        assert!(
            read(&fifo)
                .unwrap_err()
                .to_string()
                .contains("not a regular file")
        );
        let store = MediaStore::new(dir.path().join("private"));
        store.image(&png()).unwrap();
        store.remove().unwrap();
        assert!(store.image(&png()).is_err());
        assert!(!store.root().exists());
    }
}
