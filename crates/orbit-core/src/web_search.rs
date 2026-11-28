//! Built-in Codex standalone web access. Only result text enters code mode.
mod commands;
mod schema;
use crate::{auth::Auth, output, store::now};
use anyhow::{Context, Result, bail};
pub(crate) use schema::{DESCRIPTION, input_schema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Display-only links. Complete backend metadata is retained in `Metadata::details`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub title: String,
    pub url: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub searched_at: u64,
    pub sources: Vec<Source>,
    /// Complete result metadata or response diagnostics, persisted in SQLite; never model input.
    pub details: String,
}
impl Metadata {
    fn new(searched_at: u64, results: Vec<Value>) -> Result<Self> {
        let sources = results
            .iter()
            .filter_map(|result| {
                let url = result.get("url")?.as_str()?;
                let parsed = reqwest::Url::parse(url).ok()?;
                if !matches!(parsed.scheme(), "http" | "https")
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                {
                    return None;
                }
                Some(Source {
                    title: result
                        .get("title")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or(url)
                        .into(),
                    url: url.into(),
                })
            })
            .take(20)
            .collect();
        Ok(Self {
            searched_at,
            sources,
            details: serde_json::to_string_pretty(&results)?,
        })
    }
}
#[derive(Deserialize)]
struct Response {
    output: String,
    results: Option<Vec<Value>>,
}
pub(crate) struct ToolResult {
    pub text: String,
    pub metadata: Metadata,
}
/// Remote failures retain complete UI-only diagnostics without leaking them through JS errors.
#[derive(Debug)]
pub(crate) struct Failure {
    message: String,
    pub metadata: Metadata,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Failure {}
impl Failure {
    fn response(message: String, searched_at: u64, bytes: &[u8]) -> Self {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let diagnostic = match std::str::from_utf8(bytes) {
            Ok(body) => json!({"response_body":body}),
            Err(_) => json!({"response_bytes_base64":STANDARD.encode(bytes)}),
        };
        Self {
            message,
            metadata: Metadata {
                searched_at,
                sources: vec![],
                details: diagnostic.to_string(),
            },
        }
    }
}
pub(crate) type Model = Arc<dyn Fn() -> Result<String> + Send + Sync>;
pub struct WebSearch {
    auth: Arc<Auth>,
    session: String,
    model: Model,
    client: reqwest::Client,
    endpoint: String,
}
impl WebSearch {
    pub fn client() -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .user_agent(concat!("orbit/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(120))
            .build()?)
    }
    pub(crate) fn new(
        auth: Arc<Auth>,
        session: String,
        model: Model,
        client: reqwest::Client,
    ) -> Self {
        Self {
            auth,
            session,
            model,
            client,
            endpoint: "https://chatgpt.com/backend-api/codex/alpha/search".into(),
        }
    }
    pub(crate) async fn call(
        &self,
        arguments: &str,
        cancel: &CancellationToken,
    ) -> Result<ToolResult> {
        let commands = commands::SearchCommands::parse(arguments)?;
        let model = (self.model)()?;
        let body = json!({
            "id": self.session, "model": model, "commands": commands,
            "settings": {"external_web_access": true, "allowed_callers": ["direct"]},
            "max_output_tokens": 10000
        });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("Web request cancelled; remote work may already have consumed subscription usage. No automatic retry was made"),
            result = self.request(body) => result,
        }
    }
    async fn request(&self, body: Value) -> Result<ToolResult> {
        let mut credentials = self.auth.token(None).await?;
        let searched_at = now();
        for attempt in 0..2 {
            let mut request = self
                .client
                .post(&self.endpoint)
                .bearer_auth(&credentials.access_token)
                .header("originator", "orbit")
                .json(&body);
            if let Some(account) = &credentials.account_id {
                request = request.header("chatgpt-account-id", account);
            }
            let mut response = request.send().await.context("Could not complete the web request. Check your connection; remote work may have consumed subscription usage. The request was not automatically retried")?;
            let status = response.status();
            if status.as_u16() == 401 && attempt == 0 {
                credentials = self.auth.token(Some(&credentials.access_token)).await?;
                continue;
            }
            const MAX_RESPONSE: usize = 8 * 1024 * 1024;
            let secrets = [
                credentials.access_token.as_bytes(),
                credentials.refresh_token.as_bytes(),
            ];
            let mut tail = Vec::new();
            let mut large = None;
            let failure = loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        tail.extend_from_slice(&chunk);
                        if large.is_some() || tail.len() > MAX_RESPONSE {
                            let spool = large.get_or_insert_with(output::OutputBuffer::default);
                            output::redact_prefix(&mut tail, &secrets, false, |part| {
                                spool.append(part, 0)
                            });
                        }
                    }
                    Ok(None) => break None,
                    Err(error) => {
                        break Some(format!(
                            "Web response was interrupted: {error}. No automatic replay was made. Received bytes are preserved in the tool's transcript details."
                        ));
                    }
                }
            };
            if let Some(mut spool) = large {
                output::redact_prefix(&mut tail, &secrets, true, |part| spool.append(part, 0));
                let notice = spool.finish_diagnostic();
                return Err(Failure::response(failure.unwrap_or_else(|| format!(
                    "Web response ({status}) exceeds the supported size. Complete redacted response preserved in transcript details; do not repeat the request.")),
                    searched_at, notice.as_bytes()).into());
            }
            let failure = if let Some(error) = failure {
                error
            } else if !status.is_success() {
                format!(
                    "Web request failed ({status}). Check ChatGPT sign-in and account access, or retry later for rate limits/service failures. No backend fallback or retry of uncertain work was used. Complete diagnostics are in the tool's transcript details."
                )
            } else if let Ok(response) = serde_json::from_slice::<Response>(&tail) {
                return Ok(ToolResult {
                    text: output::preserve_default(&response.output),
                    metadata: Metadata::new(searched_at, response.results.unwrap_or_default())?,
                });
            } else {
                "Invalid web response: expected output text and optional result metadata. No automatic replay was made. Complete diagnostics are in the tool's transcript details.".into()
            };
            let mut redacted = Vec::new();
            output::redact_prefix(&mut tail, &secrets, true, |part| {
                redacted.extend_from_slice(part)
            });
            return Err(Failure::response(failure, searched_at, &redacted).into());
        }
        unreachable!("second authentication rejection returns an HTTP error")
    }
}

#[cfg(test)]
mod tests;
