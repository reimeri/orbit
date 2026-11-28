//! Ephemeral Markdown image sources, resolved against the conversation workspace.
use anyhow::{Context, Result, bail};
use reqwest::Url;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Source {
    File(PathBuf),
    Remote(Url),
}
impl Source {
    pub fn resolve(workspace: &Path, uri: &str) -> Result<Self> {
        if uri.is_empty() {
            bail!("Image destination is empty");
        }
        let base = Url::from_directory_path(workspace)
            .map_err(|()| anyhow::anyhow!("Image workspace must be an absolute path"))?;
        let url = base.join(uri).context("Invalid image URL")?;
        match url.scheme() {
            "file" => {
                Ok(Self::File(url.to_file_path().map_err(|()| {
                    anyhow::anyhow!("Expected a local file:// URL")
                })?))
            }
            "http" | "https" => {
                if !url.username().is_empty() || url.password().is_some() {
                    bail!("Image URLs with embedded credentials are not supported");
                }
                Ok(Self::Remote(url))
            }
            _ => bail!(
                "Unsupported image source; use a local path, file://, http:// or https:// URL"
            ),
        }
    }
    /// Notify as soon as a bounded header probe finds display dimensions. The
    /// source is read only once; no extra HTTP requests or full pixel decoding.
    pub async fn read_with_dimensions(
        self,
        dimensions: impl Fn((u32, u32)) + Send + Sync + 'static,
    ) -> Result<Vec<u8>> {
        let dimensions = Arc::new(dimensions);
        match self {
            Self::File(path) => {
                tokio::task::spawn_blocking(move || {
                    let mut file = crate::media::open(&path)?;
                    let mut bytes = Vec::new();
                    let mut chunk = [0; 8192];
                    let mut probe = Probe::default();
                    loop {
                        let count = file.read(&mut chunk)?;
                        if count == 0 {
                            break;
                        }
                        if bytes.len() + count > crate::media::MAX_BYTES {
                            bail!("Image exceeds the {} byte limit", crate::media::MAX_BYTES);
                        }
                        bytes.extend_from_slice(&chunk[..count]);
                        if probe.ready(bytes.len()) {
                            probe.inspect(&bytes, &*dimensions);
                        }
                    }
                    if !probe.found {
                        probe.inspect(&bytes, &*dimensions);
                    }
                    Ok(bytes)
                })
                .await?
            }
            Self::Remote(url) => {
                static CLIENT: tokio::sync::OnceCell<reqwest::Client> =
                    tokio::sync::OnceCell::const_new();
                let client = CLIENT
                    .get_or_try_init(|| async {
                        reqwest::Client::builder()
                            .timeout(Duration::from_secs(30))
                            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                                let url = attempt.url();
                                if attempt.previous().len() >= 5 {
                                    attempt.error("Too many image redirects")
                                } else if !matches!(url.scheme(), "http" | "https")
                                    || !url.username().is_empty()
                                    || url.password().is_some()
                                {
                                    attempt.error("Unsupported image redirect")
                                } else {
                                    attempt.follow()
                                }
                            }))
                            .build()
                    })
                    .await?;
                let mut response = client.get(url).send().await?.error_for_status()?;
                if !response.status().is_success() {
                    bail!(
                        "Image request returned HTTP {} without a usable redirect",
                        response.status()
                    );
                }
                if response
                    .content_length()
                    .is_some_and(|n| n > crate::media::MAX_BYTES as u64)
                {
                    bail!("Image exceeds the {} byte limit", crate::media::MAX_BYTES);
                }
                let mut bytes = Vec::new();
                let mut probe = Probe::default();
                while let Some(chunk) = response.chunk().await? {
                    if bytes.len() + chunk.len() > crate::media::MAX_BYTES {
                        bail!("Image exceeds the {} byte limit", crate::media::MAX_BYTES);
                    }
                    bytes.extend_from_slice(&chunk);
                    if probe.ready(bytes.len()) {
                        let prefix = bytes[..bytes.len().min(PROBE_LIMIT)].to_vec();
                        let notify = dimensions.clone();
                        probe = tokio::task::spawn_blocking(move || {
                            probe.inspect(&prefix, &*notify);
                            probe
                        })
                        .await?;
                    }
                }
                // Some formats put orientation metadata at the end. Wait for it
                // rather than reserving a rectangle with the wrong orientation.
                tokio::task::spawn_blocking(move || {
                    if !probe.found {
                        probe.inspect(&bytes, &*dimensions);
                    }
                    bytes
                })
                .await
                .map_err(Into::into)
            }
        }
    }
}

const PROBE_LIMIT: usize = 1024 * 1024;
struct Probe {
    next: usize,
    found: bool,
}
impl Default for Probe {
    fn default() -> Self {
        Self {
            next: 1024,
            found: false,
        }
    }
}
impl Probe {
    fn ready(&self, length: usize) -> bool {
        !self.found && length >= self.next && self.next <= PROBE_LIMIT
    }
    fn inspect(&mut self, bytes: &[u8], notify: &impl Fn((u32, u32))) {
        if let Ok(dimensions) = crate::media::image_dimensions(bytes) {
            self.found = true;
            notify(dimensions);
        }
        // Failed prefix probes are expected while metadata is incomplete.
        // Exponential backoff bounds repeated parsing and prefix copies.
        self.next = bytes.len().max(self.next).saturating_mul(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn local_sources_resolve_against_the_conversation_not_process_directory() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let path = root.path().join("image 界 #.png");
        std::fs::write(&path, b"local image bytes").unwrap();
        for uri in [
            "../image%20%E7%95%8C%20%23.png".to_owned(),
            Url::from_file_path(&path).unwrap().to_string(),
        ] {
            let source = Source::resolve(&workspace, &uri).unwrap();
            assert_eq!(source, Source::File(path.clone()));
            assert_eq!(
                source.read_with_dimensions(|_| {}).await.unwrap(),
                b"local image bytes"
            );
        }
        assert_eq!(
            Source::resolve(&workspace, "/tmp/image.png").unwrap(),
            Source::File("/tmp/image.png".into())
        );
        assert!(matches!(
            Source::resolve(&workspace, "https://example.com/image.png"),
            Ok(Source::Remote(_))
        ));
        for uri in [
            "",
            "data:image/png;base64,AA==",
            "ftp://example.com/a.png",
            "file://remote/a.png",
            "https://user:secret@example.com/a.png",
        ] {
            assert!(Source::resolve(&workspace, uri).is_err(), "{uri}");
        }
        std::fs::remove_file(&path).unwrap();
        assert!(
            Source::File(path)
                .read_with_dimensions(|_| {})
                .await
                .is_err()
        );
        assert!(
            Source::File(workspace)
                .read_with_dimensions(|_| {})
                .await
                .is_err()
        );
    }

    async fn serve(headers: String, body: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/image", listener.local_addr().unwrap());
        let job = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket.write_all(headers.as_bytes()).await.unwrap();
            // An over-limit client closes its connection without consuming the body.
            let _ = socket.write_all(&body).await;
        });
        (url, job)
    }
    async fn fetch(url: &str) -> Result<Vec<u8>> {
        Source::resolve(Path::new("/workspace"), url)?
            .read_with_dimensions(|_| {})
            .await
    }
    #[tokio::test]
    async fn remote_images_follow_redirects_preserve_bytes_and_report_http_errors() {
        let bytes = b"\x89PNG\r\n\x1a\n\0\xff".to_vec();
        let (target, target_job) = serve(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            ),
            bytes.clone(),
        )
        .await;
        let (url, job) = serve(
            format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n"),
            vec![],
        )
        .await;
        assert_eq!(fetch(&url).await.unwrap(), bytes);
        job.await.unwrap();
        target_job.await.unwrap();
        let (url, job) = serve(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into(),
            vec![],
        )
        .await;
        assert!(fetch(&url).await.unwrap_err().to_string().contains("404"));
        job.await.unwrap();
        let (url, job) = serve(
            "HTTP/1.1 302 Found\r\nLocation: file:///etc/passwd\r\nContent-Length: 0\r\n\r\n"
                .into(),
            vec![],
        )
        .await;
        assert!(fetch(&url).await.is_err());
        job.await.unwrap();
    }
    #[tokio::test]
    async fn remote_dimensions_arrive_while_pixel_download_is_still_blocked() {
        let pixels = image::RgbImage::from_fn(257, 113, |x, y| {
            image::Rgb([(x * 31 + y * 17) as u8, (x * y) as u8, (x ^ y) as u8])
        });
        let mut encoded = std::io::Cursor::new(Vec::new());
        pixels
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let png = encoded.into_inner();
        assert!(png.len() > 1024);
        assert!(crate::media::decode_image(&png[..1024]).is_err());
        let expected = png.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/image", listener.local_addr().unwrap());
        let (release, blocked) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", png.len()).as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(&png[..1024]).await.unwrap();
            blocked.await.unwrap();
            socket.write_all(&png[1024..]).await.unwrap();
        });
        let (notify, mut dimensions) = tokio::sync::mpsc::unbounded_channel();
        let reading = tokio::spawn(async move {
            Source::resolve(Path::new("/workspace"), &url)
                .unwrap()
                .read_with_dimensions(move |size| {
                    notify.send(size).unwrap();
                })
                .await
        });
        let size = tokio::time::timeout(Duration::from_secs(5), dimensions.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(size, (257, 113));
        assert!(
            !reading.is_finished(),
            "metadata must not wait for the whole download"
        );
        release.send(()).unwrap();
        assert_eq!(reading.await.unwrap().unwrap(), expected);
        assert!(
            dimensions.recv().await.is_none(),
            "one metadata event per source load"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn remote_byte_limit_applies_with_and_without_content_length() {
        let (url, job) = serve(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                crate::media::MAX_BYTES + 1
            ),
            vec![],
        )
        .await;
        assert!(
            fetch(&url)
                .await
                .unwrap_err()
                .to_string()
                .contains("byte limit")
        );
        job.await.unwrap();
        let (url, job) = serve(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".into(),
            vec![0; crate::media::MAX_BYTES + 1],
        )
        .await;
        assert!(
            fetch(&url)
                .await
                .unwrap_err()
                .to_string()
                .contains("byte limit")
        );
        job.await.unwrap();
    }
}
