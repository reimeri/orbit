//! Built-in image tools. Paid requests are never replayed after an uncertain outcome.
use crate::{
    auth::Auth,
    media::{self, Media, MediaStore},
    store::{Catalog, Store},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_API_MODEL: &str = "gpt-image-2.6-sunburst";
// Pinned public Codex image-generation backend default, not the API default.
const CODEX_MODEL: &str = "gpt-image-2";
pub const QUALITIES: &[&str] = &["auto", "low", "medium", "high", "xhigh", "max"];
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    #[default]
    Codex,
    Api,
}
impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Codex => "Codex subscription",
            Self::Api => "OpenAI API",
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub backend: Backend,
    pub model: String,
    pub quality: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            backend: Backend::Codex,
            model: DEFAULT_API_MODEL.into(),
            quality: "auto".into(),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.model.trim().is_empty() || self.model.chars().any(char::is_whitespace) {
            bail!("Enter an API image model ID without whitespace");
        }
        if !QUALITIES.contains(&self.quality.as_str()) {
            bail!("Choose an image quality from the selector");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewArgs {
    path: PathBuf,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    prompt: String,
    output_path: PathBuf,
    #[serde(default)]
    transparent_background: bool,
    referenced_image_paths: Option<Vec<PathBuf>>,
    num_last_images_to_include: Option<usize>,
    gif: Option<Gif>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Gif {
    frame_count: Option<u32>,
    frame_duration_ms: Option<u32>,
}
impl Args {
    fn validate(&self) -> Result<()> {
        if self.prompt.trim().is_empty() {
            bail!("prompt must not be empty");
        }
        let extension = if self.gif.is_some() { "gif" } else { "png" };
        if self.output_path.extension().and_then(|v| v.to_str()) != Some(extension) {
            bail!(
                "output_path must end in .{extension} {}. Change the extension or change the gif argument. No generation request was sent.",
                if self.gif.is_some() {
                    "when gif is supplied"
                } else {
                    "when gif is omitted"
                }
            );
        }
        let paths = self.referenced_image_paths.as_deref().unwrap_or(&[]);
        if paths.len() > 5 {
            bail!("referenced_image_paths accepts at most 5 images");
        }
        if paths.iter().any(|p| !p.is_absolute()) {
            bail!(
                "referenced_image_paths must contain absolute local paths; resolve them against the calling workspace"
            );
        }
        if let Some(count) = self.num_last_images_to_include {
            if !(1..=5).contains(&count) {
                bail!("num_last_images_to_include must be between 1 and 5");
            }
            if !paths.is_empty() {
                bail!(
                    "Provide only one of referenced_image_paths or num_last_images_to_include, not both"
                );
            }
        }
        if let Some(gif) = &self.gif {
            if gif.frame_count.is_some_and(|v| !(2..=16).contains(&v)) {
                bail!("gif.frame_count must be 2–16 (default 9)");
            }
            if gif
                .frame_duration_ms
                .is_some_and(|v| !(20..=2000).contains(&v))
            {
                bail!("gif.frame_duration_ms must be 20–2000 (default 200)");
            }
        }
        Ok(())
    }
}

pub struct ToolResult {
    pub value: Value,
    pub text: String,
    pub media: Media,
}
pub type History = Arc<dyn Fn() -> Result<crate::store::Session> + Send + Sync>;
static IMAGE_CALLS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
pub struct ImageTools {
    pub media: Arc<MediaStore>,
    pub generation: bool,
    store: Store,
    auth: Arc<Auth>,
    workspace: PathBuf,
    history: History,
    client: reqwest::Client,
    codex_endpoint: String,
    api_endpoint: String,
}
impl ImageTools {
    pub fn new(
        store: Store,
        auth: Arc<Auth>,
        workspace: PathBuf,
        session: &str,
        generation: bool,
        history: History,
        client: reqwest::Client,
    ) -> Self {
        Self {
            media: Arc::new(MediaStore::new(store.root().join("media").join(session))),
            generation,
            store,
            auth,
            workspace,
            history,
            client,
            codex_endpoint: "https://chatgpt.com/backend-api/codex/images".into(),
            api_endpoint: "https://api.openai.com/v1/images".into(),
        }
    }
    pub fn client() -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(600))
            .build()?)
    }
    pub fn audio_supported(&self) -> Result<bool> {
        Ok(crate::models::model(&(self.history)()?.model).is_some_and(|m| m.audio_input))
    }
    pub async fn call(
        &self,
        name: &str,
        args: &str,
        request: &str,
        cancel: &CancellationToken,
    ) -> Result<ToolResult> {
        let _permit = tokio::select! {biased; _=cancel.cancelled()=>bail!("Image tool cancelled before dispatch"), permit=IMAGE_CALLS.acquire()=>permit.expect("image semaphore")};
        match name {
            "view_image" => {
                let args: ViewArgs = serde_json::from_str(args)
                    .context("view_image expects {path: string}; there is no detail argument")?;
                let path = self.workspace.join(args.path);
                let media = self.media.image(&media::read(&path)?)?;
                let text = format!(
                    "Viewed {}. {}{}",
                    path.display(),
                    media.description(),
                    animation_notice(&media)
                );
                Ok(ToolResult {
                    value: json!({"image_url":media.data_url()?}),
                    text,
                    media,
                })
            }
            "image_gen__imagegen" if self.generation => self.generate(args, request, cancel).await,
            _ => bail!("Image generation is only available to the main agent"),
        }
    }
    async fn generate(
        &self,
        args: &str,
        request: &str,
        cancel: &CancellationToken,
    ) -> Result<ToolResult> {
        let args:Args=serde_json::from_str(args).context("Invalid image_gen__imagegen arguments; prompt and output_path are required. No generation request was sent")?;
        args.validate()?;
        let destination =
            Destination::new(&self.workspace.join(&args.output_path), self.store.root())?;
        let catalog: Catalog = self
            .store
            .read_optional("settings.json")?
            .unwrap_or_default();
        let settings = catalog.settings.images;
        settings.validate()?;
        let mut images = Vec::new();
        if let Some(count) = args.num_last_images_to_include {
            for m in media::recent(
                (self.history)()?
                    .input
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter(),
                count,
            )? {
                if m.mime == "image/gif" {
                    bail!(
                        "Recent image is a GIF. Create and reference a regular-grid spritesheet for GIF editing, not the animation itself. No request was sent"
                    );
                }
                images.push(json!({"image_url":m.data_url()?}));
            }
        } else {
            for path in args.referenced_image_paths.as_deref().unwrap_or(&[]) {
                let bytes = media::read(path)?;
                if image::guess_format(&bytes)? == image::ImageFormat::Gif {
                    bail!(
                        "Do not pass a GIF directly as an edit reference. Inspect its frames and create a regular-grid spritesheet, then reference the spritesheet and set gif."
                    );
                }
                let m = self.media.image(&bytes)?;
                images.push(json!({"image_url":m.data_url()?}));
            }
        }
        let edit = !images.is_empty();
        let model = match settings.backend {
            Backend::Codex => CODEX_MODEL,
            Backend::Api => &settings.model,
        };
        let mut body = json!({"prompt":args.prompt,"model":model,"quality":settings.quality,"size":"auto","background":if args.transparent_background {"transparent"}else{"opaque"},"n":1,"output_format":if args.gif.is_some(){"gif"}else{"png"}});
        if edit {
            body["images"] = json!(images);
        }
        if let Some(gif) = args.gif {
            body["frame_count"] = json!(gif.frame_count.unwrap_or(9));
            body["frame_duration_ms"] = json!(gif.frame_duration_ms.unwrap_or(200));
        }
        let endpoint = match settings.backend {
            Backend::Codex => &self.codex_endpoint,
            Backend::Api => &self.api_endpoint,
        };
        let url = format!("{endpoint}/{}", if edit { "edits" } else { "generations" });
        let mut credentials = if settings.backend == Backend::Codex {
            Some(self.auth.token(None).await?)
        } else {
            None
        };
        let key = if settings.backend == Backend::Api {
            Some(self.store.api_key()?.context("Set an OpenAI API key in Settings → Account before using the API image backend. No request was sent")?)
        } else {
            None
        };
        // Only a definite 401 can be retried, once, using synchronized OAuth refresh.
        let mut refreshed = false;
        let response = loop {
            if cancel.is_cancelled() {
                bail!("Generation cancelled before dispatch; no new request was sent");
            }
            let secret = credentials
                .as_ref()
                .map(|c| c.access_token.as_str())
                .or(key.as_deref())
                .expect("selected credentials");
            let mut post = self
                .client
                .post(&url)
                .bearer_auth(secret)
                .header("originator", "pico-evo")
                .header("x-codex-image-turn-id", request)
                .json(&body);
            if let Some(account) = credentials.as_ref().and_then(|c| c.account_id.as_deref()) {
                post = post.header("chatgpt-account-id", account);
            }
            let response = tokio::select! { biased; _=cancel.cancelled()=>bail!("Generation cancelled after dispatch. Remote work/charges may continue; no automatic retry was made. Check {} before retrying.",destination.path.display()), result=post.send()=>result.context("Image request outcome is unknown; no automatic retry was made. Check the output path and account usage before retrying")? };
            if response.status() == reqwest::StatusCode::UNAUTHORIZED
                && credentials.is_some()
                && !refreshed
            {
                credentials = Some(
                    self.auth
                        .token(credentials.as_ref().map(|c| c.access_token.as_str()))
                        .await?,
                );
                refreshed = true;
                continue;
            }
            break response;
        };
        let status = response.status();
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        loop {
            let next = tokio::select! { biased; _=cancel.cancelled()=>bail!("Generation cancelled while receiving the result. Remote usage may have been consumed; do not blindly repeat the request"), next=stream.next()=>next };
            let Some(chunk) = next else { break };
            bytes.extend_from_slice(&chunk.context(
                "Image response interrupted; outcome uncertain. No automatic retry was made",
            )?);
            if bytes.len() > media::MAX_BYTES.div_ceil(3) * 4 + 1024 * 1024 {
                bail!(
                    "Image response exceeds the supported limit; remote usage may have been consumed. No automatic retry was made"
                );
            }
        }
        let mut response: Value = serde_json::from_slice(&bytes)
            .context("Image backend returned invalid JSON. No automatic retry was made")?;
        if !status.is_success() {
            // Redact our secrets even if a service echoes request data in an error.
            let mut error = response.to_string();
            for secret in key.iter().map(String::as_str).chain(
                credentials
                    .iter()
                    .flat_map(|c| [c.access_token.as_str(), c.refresh_token.as_str()]),
            ) {
                if !secret.is_empty() {
                    error = error.replace(secret, "[redacted]");
                }
            }
            bail!(
                "Image backend rejected the request ({status}). Check credentials, account access, model, quality and GIF support. No fallback or automatic retry was made. {}",
                crate::output::preserve_text(&error, 2000)
            );
        }
        let encoded = response["data"]
            .as_array_mut()
            .and_then(|a| a.get_mut(0))
            .and_then(|v| v["b64_json"].take().as_str().map(str::to_owned))
            .context("Image backend returned no image data; do not blindly regenerate")?;
        let bytes = STANDARD
            .decode(encoded)
            .context("Image backend returned invalid base64; do not blindly regenerate")?;
        let format = image::guess_format(&bytes).map_err(|e|anyhow::anyhow!("Generation returned invalid image data: {e}. Complete returned bytes retained; do not regenerate blindly:\n{}",crate::output::preserve_bytes(&bytes,0)))?;
        let expected = if body["output_format"] == "gif" {
            image::ImageFormat::Gif
        } else {
            image::ImageFormat::Png
        };
        if format != expected {
            bail!(
                "Backend returned {format:?}, but {expected:?} was requested. No format fallback or file publication was performed. Recover the returned bytes without regenerating:\n{}",
                crate::output::preserve_bytes(&bytes, 0)
            );
        }
        // Keep an independent recoverable result before publishing caller-owned bytes.
        let media=self.media.image(&bytes).map_err(|error| {
            let notice=crate::output::preserve_bytes(&bytes,0);
            anyhow::anyhow!("Generation completed but private media storage failed: {error:#}. Recover the generated bytes without regenerating:\n{notice}")
        })?;
        self.publish_result(
            &destination,
            media,
            &bytes,
            model,
            &settings.quality,
            cancel,
        )
    }
    fn publish_result(
        &self,
        destination: &Destination,
        media: Media,
        bytes: &[u8],
        model: &str,
        quality: &str,
        cancel: &CancellationToken,
    ) -> Result<ToolResult> {
        self.media.while_live(|| {
            if cancel.is_cancelled() {bail!("Generation completed, but publication was cancelled");}
            destination.publish(bytes)
        }).map_err(|error| {
            // This artifact must outlive session deletion, including a deletion that wins
            // immediately after the private snapshot was prepared.
            let recovery=crate::output::preserve_bytes(bytes,0);
            anyhow::anyhow!("Generation completed but output publication did not complete: {error:#}. Check {} (publication may precede a sync error). Recover the generated bytes below; do not regenerate:\n{recovery}",destination.path.display())
        })?;
        let hint = format!(
            "Saved {} (requested model {model}, quality {}).{}",
            destination.path.display(),
            quality,
            animation_notice(&media)
        );
        Ok(ToolResult {
            value: json!({"image_url":format!("data:{};base64,{}",media.mime,STANDARD.encode(bytes)),"output_hint":hint}),
            text: hint,
            media,
        })
    }
}
pub fn animation_notice(media: &Media) -> &'static str {
    if media.mime == "image/gif" {
        " The saved GIF retains its animation; model image input is a static first-frame representation. Inspect extracted frames to assess motion; use a spritesheet for GIF edits."
    } else {
        ""
    }
}

/// Hold the resolved directory open, then publish with renameat2(NOREPLACE).
struct Destination {
    path: PathBuf,
    parent: File,
    name: std::ffi::OsString,
}
impl Destination {
    fn new(path: &Path, private: &Path) -> Result<Self> {
        let parent_path=path.parent().context("output_path needs a parent directory")?.canonicalize().context("The output parent directory must already exist. Create it before generation; no request was sent")?;
        let parent = File::open(&parent_path)?;
        let resolved = fs::read_link(format!("/proc/self/fd/{}", parent.as_raw_fd()))?;
        if !parent.metadata()?.is_dir() {
            bail!("Output parent is not a directory");
        }
        if resolved.starts_with(private.canonicalize()?) {
            bail!(
                "output_path cannot point into Pico-owned storage, including symlink aliases. Choose a caller-owned destination; no request was sent"
            );
        }
        let name = path
            .file_name()
            .context("output_path must name a new file")?
            .to_owned();
        let destination = Self {
            path: resolved.join(&name),
            parent,
            name,
        };
        match fs::symlink_metadata(destination.fd_path().join(&destination.name)) {
            Ok(_) => bail!(
                "output_path already exists (files and symlinks are never overwritten). Choose a new path; no request was sent"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Detect an unwritable directory before consuming image usage. The held
        // descriptor prevents a changed parent symlink from redirecting this probe.
        let probe = destination
            .fd_path()
            .join(format!(".pico-image-{}.tmp", crate::store::id()));
        let file=OpenOptions::new().write(true).create_new(true).mode(0o600).open(&probe).context("Output directory is not writable; choose another destination. No generation request was sent")?;
        drop(file);
        fs::remove_file(&probe).context("Could not remove output preflight file; fix directory permissions before generating. No request was sent")?;
        Ok(destination)
    }
    fn fd_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.parent.as_raw_fd()))
    }
    fn publish(&self, bytes: &[u8]) -> Result<()> {
        if fs::read_link(self.fd_path())?.as_path() != self.path.parent().expect("resolved parent")
        {
            bail!(
                "Output parent moved during generation; choose a stable destination and recover the saved result"
            );
        }
        let name = format!(".pico-image-{}.tmp", crate::store::id());
        let temp = self.fd_path().join(&name);
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            nix::fcntl::renameat2(
                &self.parent,
                name.as_str(),
                &self.parent,
                Path::new(&self.name),
                nix::fcntl::RenameFlags::RENAME_NOREPLACE,
            )?;
            self.parent.sync_all()?;
            Ok(())
        })();
        if result.is_err()
            && let Err(e) = fs::remove_file(&temp)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(anyhow::anyhow!(
                "Publication failed: {result:?}; staging file cleanup failed at {}: {e}",
                temp.display()
            ));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        media::tests::{gif, png},
        store::{Session, Settings as AppSettings},
        test_support::server,
    };
    fn fixture(dir: &Path, generation: bool) -> ImageTools {
        let store = Store::open(dir.join("state")).unwrap();
        let session = Session::new(dir.into(), &AppSettings::default());
        let id = session.id.clone();
        ImageTools::new(
            store.clone(),
            Auth::new(store).unwrap(),
            dir.into(),
            &id,
            generation,
            Arc::new(move || Ok(session.clone())),
            ImageTools::client().unwrap(),
        )
    }
    fn response(bytes: &[u8]) -> String {
        json!({"created":123,"data":[{"b64_json":STANDARD.encode(bytes)}]}).to_string()
    }
    fn api(tools: &ImageTools) {
        tools.store.save_api_key("test-api-secret").unwrap();
        let mut c = Catalog::default();
        c.settings.images.backend = Backend::Api;
        c.settings.images.model = "custom-image-model".into();
        c.settings.images.quality = "max".into();
        tools.store.save_catalog(&c).unwrap();
    }
    #[tokio::test]
    async fn api_generation_and_gif_edit_use_explicit_billing_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = fixture(dir.path(), true);
        api(&tools);
        let (url, mut requests, task) =
            server(vec![(200, response(&png())), (200, response(&gif()))]).await;
        tools.api_endpoint = url;
        let result = tools
            .call(
                "image_gen__imagegen",
                r#"{"prompt":"small icon","output_path":"icon.png"}"#,
                "req1",
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(fs::read(dir.path().join("icon.png")).unwrap(), png());
        assert!(result.text.contains("custom-image-model"));
        assert!(!result.text.contains("base64"));
        assert!(
            result.value["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        let request = requests.recv().await.unwrap();
        assert!(request.starts_with("POST /generations"));
        assert!(request.contains("Bearer test-api-secret"));
        assert!(!request.contains("chatgpt-account-id"));
        let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["model"], "custom-image-model");
        assert_eq!(body["quality"], "max");
        assert_eq!(body["output_format"], "png");
        let result=tools.call("image_gen__imagegen",&json!({"prompt":"animate it","output_path":"icon.gif","referenced_image_paths":[dir.path().join("icon.png")],"gif":{"frame_count":2,"frame_duration_ms":230},"transparent_background":true}).to_string(),"req2",&CancellationToken::new()).await.unwrap();
        assert_eq!(fs::read(dir.path().join("icon.gif")).unwrap(), gif());
        assert!(result.text.contains("static first-frame"));
        let request = requests.recv().await.unwrap();
        assert!(request.starts_with("POST /edits"));
        let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["frame_count"], 2);
        assert_eq!(body["frame_duration_ms"], 230);
        assert_eq!(body["background"], "transparent");
        assert!(
            body["images"][0]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        task.await.unwrap();
    }
    #[tokio::test]
    async fn codex_uses_oauth_and_never_api_key_and_http_failure_is_not_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = fixture(dir.path(), true);
        tools
            .store
            .save_api_key("must-not-use-this-api-key")
            .unwrap();
        tools
            .store
            .write(
                "auth.json",
                &crate::auth::Credentials {
                    access_token: "subscription-token".into(),
                    refresh_token: "refresh".into(),
                    expires_at: crate::store::now() + 3600,
                    account_id: Some("account-id".into()),
                    email: None,
                },
            )
            .unwrap();
        tools.auth = Auth::new(tools.store.clone()).unwrap();
        let (url, mut requests, task) = server(vec![(
            503,
            json!({"error":{"message":"subscription-token unavailable"}}).to_string(),
        )])
        .await;
        tools.codex_endpoint = url;
        let error = tools
            .call(
                "image_gen__imagegen",
                r#"{"prompt":"icon","output_path":"icon.png"}"#,
                "r",
                &CancellationToken::new(),
            )
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("503"));
        assert!(error.contains("No fallback"));
        assert!(!error.contains("subscription-token"));
        let request = requests.recv().await.unwrap();
        assert!(request.contains("Bearer subscription-token"));
        assert_eq!(
            crate::test_support::header(&request, "chatgpt-account-id"),
            Some("account-id")
        );
        assert!(!request.contains("must-not-use"));
        task.await.unwrap();
        assert!(requests.recv().await.is_none());
        assert!(!dir.path().join("icon.png").exists());
    }
    #[tokio::test]
    async fn validation_rejects_mistakes_before_network_and_preserves_wrong_format_result() {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = fixture(dir.path(), true);
        api(&tools);
        for (args, expected) in [
            (
                json!({"prompt":"x","output_path":"bad.png","gif":{}}),
                ".gif",
            ),
            (
                json!({"prompt":"x","output_path":"x.png","referenced_image_paths":["relative.png"]}),
                "absolute",
            ),
            (
                json!({"prompt":"x","output_path":"x.gif","gif":{"frame_count":17}}),
                "2–16",
            ),
            (
                json!({"prompt":"x","output_path":"x.png","num_last_images_to_include":0}),
                "1 and 5",
            ),
        ] {
            let error = tools
                .call(
                    "image_gen__imagegen",
                    &args.to_string(),
                    "r",
                    &CancellationToken::new(),
                )
                .await
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains(expected), "{error}");
        }
        let (url, _requests, task) = server(vec![(200, response(&png()))]).await;
        tools.api_endpoint = url;
        let error = tools
            .call(
                "image_gen__imagegen",
                r#"{"prompt":"x","output_path":"animation.gif","gif":{}}"#,
                "r",
                &CancellationToken::new(),
            )
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("Recover the returned bytes"));
        assert_eq!(crate::test_support::saved_output(&error), png());
        assert!(!dir.path().join("animation.gif").exists());
        task.await.unwrap();
    }
    async fn gated_server() -> (
        String,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<String>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (accepted, arrival) = tokio::sync::oneshot::channel();
        let (reply, response) = tokio::sync::oneshot::channel::<String>();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
                if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let length: usize = std::str::from_utf8(&request[..end])
                        .unwrap()
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            accepted.send(()).unwrap();
            if let Ok(body) = response.await {
                let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
            }
        });
        (url, arrival, reply, task)
    }
    #[tokio::test]
    async fn dispatched_cancellation_is_uncertain_and_publication_failure_keeps_paid_result() {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = fixture(dir.path(), true);
        api(&tools);
        let (url, arrival, reply, server) = gated_server().await;
        tools.api_endpoint = url;
        let tools = Arc::new(tools);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let worker = tools.clone();
        let call = tokio::spawn(async move {
            worker
                .call(
                    "image_gen__imagegen",
                    r#"{"prompt":"x","output_path":"cancelled.png"}"#,
                    "r",
                    &token,
                )
                .await
        });
        arrival.await.unwrap();
        cancel.cancel();
        let error = call.await.unwrap().err().unwrap().to_string();
        assert!(
            error.contains("may continue") || error.contains("usage may have been consumed"),
            "{error}"
        );
        assert!(!dir.path().join("cancelled.png").exists());
        drop(reply);
        server.await.unwrap();
        drop(tools);
        // Reopen the same state after the cancelled call, then lose a publication race.
        let mut tools = fixture(dir.path(), true);
        api(&tools);
        let (url, arrival, reply, server) = gated_server().await;
        tools.api_endpoint = url;
        let tools = Arc::new(tools);
        let worker = tools.clone();
        let call = tokio::spawn(async move {
            worker
                .call(
                    "image_gen__imagegen",
                    r#"{"prompt":"x","output_path":"race.png"}"#,
                    "r",
                    &CancellationToken::new(),
                )
                .await
        });
        arrival.await.unwrap();
        fs::write(dir.path().join("race.png"), b"competitor").unwrap();
        reply.send(response(&png())).unwrap();
        let error = format!("{:#}", call.await.unwrap().err().unwrap());
        assert!(error.contains("do not regenerate"), "{error}");
        assert_eq!(
            fs::read(dir.path().join("race.png")).unwrap(),
            b"competitor"
        );
        let recovered = tools.media.image(&png()).unwrap();
        assert_eq!(fs::read(recovered.path).unwrap(), png());
        server.await.unwrap();
    }
    #[test]
    fn output_publication_is_no_replace_and_resists_symlink_aliases_and_parent_moves() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("state");
        fs::create_dir(&private).unwrap();
        let path = dir.path().join("new.png");
        let target = Destination::new(&path, &private).unwrap();
        fs::write(&path, b"competitor").unwrap();
        assert!(target.publish(&png()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"competitor");
        fs::remove_file(&path).unwrap();
        symlink("missing-target", &path).unwrap();
        assert!(Destination::new(&path, &private).is_err());
        assert!(Destination::new(&private.join("new.png"), &private).is_err());
        let protected = private.join("media");
        fs::create_dir(&protected).unwrap();
        let alias = dir.path().join("alias");
        symlink(&protected, &alias).unwrap();
        assert!(Destination::new(&alias.join("new.png"), &private).is_err());
        let parent = dir.path().join("destination");
        fs::create_dir(&parent).unwrap();
        let target = Destination::new(&parent.join("new.png"), &private).unwrap();
        fs::rename(&parent, dir.path().join("moved")).unwrap();
        symlink(&private, &parent).unwrap();
        assert!(target.publish(&png()).is_err());
        assert!(!private.join("new.png").exists());
    }
    #[test]
    fn deletion_after_preparation_preserves_paid_bytes_outside_session_and_published_result_is_self_contained()
     {
        let dir = tempfile::tempdir().unwrap();
        let tools = fixture(dir.path(), true);
        let media = tools.media.image(&png()).unwrap();
        let destination =
            Destination::new(&dir.path().join("deleted.png"), tools.store.root()).unwrap();
        tools.media.remove().unwrap();
        let error = tools
            .publish_result(
                &destination,
                media,
                &png(),
                "fixture",
                "auto",
                &CancellationToken::new(),
            )
            .err()
            .unwrap()
            .to_string();
        assert!(!destination.path.exists());
        assert_eq!(crate::test_support::saved_output(&error), png());
        assert!(!tools.media.root().exists());
        drop(tools);
        let tools = fixture(dir.path(), true);
        let media = tools.media.image(&png()).unwrap();
        let destination =
            Destination::new(&dir.path().join("published.png"), tools.store.root()).unwrap();
        let result = tools
            .publish_result(
                &destination,
                media,
                &png(),
                "fixture",
                "auto",
                &CancellationToken::new(),
            )
            .unwrap();
        tools.media.remove().unwrap();
        assert_eq!(fs::read(destination.path).unwrap(), png());
        assert_eq!(
            media::parse_data_url(result.value["image_url"].as_str().unwrap())
                .unwrap()
                .1,
            png()
        );
        assert!(result.text.contains("Saved"));
    }
    #[test]
    fn settings_preserve_custom_model_and_private_key_is_independent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tools = fixture(dir.path(), true);
        api(&tools);
        assert_eq!(
            tools.store.load().unwrap().0.settings.images.model,
            "custom-image-model"
        );
        assert_eq!(
            fs::metadata(tools.store.root().join("image-api-key.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            !fs::read_to_string(tools.store.root().join("settings.json"))
                .unwrap()
                .contains("test-api-secret")
        );
        tools.store.save_api_key("").unwrap();
        assert!(tools.store.api_key().unwrap().is_none());
    }
}
