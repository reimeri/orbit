//! Ephemeral Markdown image sources, resolved against the conversation workspace.
use anyhow::{Context, Result, bail};
use reqwest::Url;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, PartialEq)]
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
    pub async fn read(self) -> Result<Vec<u8>> {
        match self {
            Self::File(path) => {
                tokio::task::spawn_blocking(move || crate::media::read(&path)).await?
            }
            Self::Remote(url) => {
                let client = reqwest::Client::builder()
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
                    .build()?;
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
                while let Some(chunk) = response.chunk().await? {
                    if bytes.len() + chunk.len() > crate::media::MAX_BYTES {
                        bail!("Image exceeds the {} byte limit", crate::media::MAX_BYTES);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(bytes)
            }
        }
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
            assert_eq!(source.read().await.unwrap(), b"local image bytes");
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
        assert!(Source::File(path).read().await.is_err());
        assert!(Source::File(workspace).read().await.is_err());
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
        Source::resolve(Path::new("/workspace"), url)?.read().await
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
