//! A continuation belongs to a live socket, not to a persisted conversation ID.
use super::*;
use crate::history::History;
use futures_util::{SinkExt, StreamExt};
use reqwest::{StatusCode, header::HeaderMap};
use std::{collections::HashMap, sync::Mutex};
use tokio::time::timeout;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Error, Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

type Socket = WebSocketStream<reqwest::Upgraded>;

#[derive(Debug)]
pub(crate) struct StreamInterrupted;
impl std::fmt::Display for StreamInterrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChatGPT connection interrupted without response.completed")
    }
}
impl std::error::Error for StreamInterrupted {}

struct Checkpoint {
    socket: Socket,
    properties: Value,
    input_and_output: History<Value>,
    response_id: String,
    account: Option<String>,
    access_token: String,
}

// Idle sockets must keep answering pings between turns. A checkout transfers
// exclusive ownership back to the request; dropping that request drops its chain.
type Checkout = tokio::sync::oneshot::Sender<Checkpoint>;
#[derive(Default)]
pub(super) struct Sockets(Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Checkout>>>>);
impl Sockets {
    async fn take(&self, id: &str) -> Option<Checkpoint> {
        let checkout = self.0.lock().expect("socket pool").remove(id)?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        checkout.send(tx).ok()?;
        rx.await.ok()
    }
    fn park(&self, session_id: &str, mut checkpoint: Checkpoint) {
        let id = session_id.to_owned();
        let (tx, mut checkout) = tokio::sync::oneshot::channel::<Checkout>();
        self.0.lock().expect("socket pool").insert(id.clone(), tx);
        let pool = Arc::downgrade(&self.0);
        tokio::spawn(async move {
            let expiry = tokio::time::sleep(Duration::from_secs(300));
            tokio::pin!(expiry);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut expiry => break,
                    event = checkpoint.socket.next() => {
                        if !matches!(event, Some(Ok(Message::Ping(_) | Message::Pong(_)))) { break; }
                    }
                    request = &mut checkout => {
                        if let Ok(tx) = request { let _ = tx.send(checkpoint); }
                        return;
                    }
                }
            }
            checkout.close();
            if let Some(pool) = pool.upgrade() {
                let mut pool = pool.lock().expect("socket pool");
                if pool.get(&id).is_some_and(|tx| tx.is_closed()) {
                    pool.remove(&id);
                }
            }
        });
    }
}

#[derive(Default)]
struct Attempts {
    transient: u8,
    refreshed: bool,
}
impl Attempts {
    // Both a refused upgrade and an in-band error can carry an HTTP status.
    async fn rejected(
        &mut self,
        status: StatusCode,
        retry_after: Option<u64>,
        auth: &Arc<Auth>,
        credentials: &mut crate::auth::Credentials,
        emit: &(dyn Fn(Delta) + Send + Sync),
    ) -> Result<bool> {
        if status == StatusCode::UNAUTHORIZED {
            if self.refreshed {
                bail!(
                    "ChatGPT rejected your login ({status}). Stop running turns, then sign out and sign in again in Settings."
                );
            }
            *credentials = auth.token(Some(&credentials.access_token)).await?;
            self.refreshed = true;
            return Ok(true);
        }
        if (status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
            && self.transient < MAX_TRANSIENT_RETRIES
        {
            let delay_secs = retry_after.unwrap_or(1 << self.transient).min(30);
            self.transient += 1;
            emit(Delta::Retrying(RetryStatus::ServiceBusy {
                status,
                delay_secs,
            }));
            tokio::time::sleep(Duration::from_secs(delay_secs)).await;
            return Ok(true);
        }
        Ok(false)
    }
}
impl Provider {
    pub(crate) fn discard_continuation(&self, session_id: &str) {
        self.sockets
            .0
            .lock()
            .expect("socket pool")
            .remove(session_id);
    }

    /// Use the normal TLS/proxy client for the *upgrade only*. Never POST a
    /// Responses payload. A rejected upgrade may carry a diagnostic larger than
    /// the initial TCP read; consume it before applying the inline error budget.
    async fn connect(
        &self,
        session_id: &str,
        hint: &str,
        credentials: &mut crate::auth::Credentials,
        attempts: &mut Attempts,
        emit: &(dyn Fn(Delta) + Send + Sync),
    ) -> Result<Socket> {
        use tokio_tungstenite::tungstenite::{handshake::derive_accept_key, protocol::Role};
        let mut url = reqwest::Url::parse(&self.endpoint)?;
        let scheme = match url.scheme() {
            "wss" => "https",
            "ws" => "http",
            _ => bail!("Invalid WebSocket endpoint"),
        };
        url.set_scheme(scheme).expect("WebSocket upgrade scheme");
        loop {
            let (parts, _) = self.endpoint.as_str().into_client_request()?.into_parts();
            let accept = derive_accept_key(parts.headers["sec-websocket-key"].as_bytes());
            let mut upgrade = self
                .upgrade_client
                .get(url.clone())
                .headers(parts.headers)
                .bearer_auth(&credentials.access_token)
                .header("originator", "pico-evo")
                .header("openai-beta", "responses_websockets=2026-02-06")
                .header("session-id", session_id)
                .header("x-codex-routing-hint", hint)
                .timeout(Duration::from_secs(90));
            if let Some(account) = &credentials.account_id {
                upgrade = upgrade.header("chatgpt-account-id", account);
            }
            let response = match upgrade.send().await {
                Ok(r) => r,
                Err(e)
                    if (e.is_connect() || e.is_timeout())
                        && attempts.transient < MAX_TRANSIENT_RETRIES =>
                {
                    let delay = Duration::from_secs(1 << attempts.transient);
                    attempts.transient += 1;
                    emit(Delta::Retrying(RetryStatus::Reconnecting {
                        retry: attempts.transient,
                    }));
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(e) => {
                    return Err(e).context(
                        "Could not open ChatGPT WebSocket. Check your connection or proxy",
                    );
                }
            };
            let status = response.status();
            if status != StatusCode::SWITCHING_PROTOCOLS {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok());
                if attempts
                    .rejected(status, retry_after, &self.auth, credentials, emit)
                    .await?
                {
                    continue;
                }
                let bytes = response.bytes().await?;
                let error = match serde_json::from_slice(&bytes) {
                    Ok(v) => safe_error(&v),
                    Err(_) => crate::output::preserve_bytes(&bytes, 500),
                };
                bail!(
                    "ChatGPT WebSocket upgrade failed ({status}): {error}. Check your connection or proxy."
                );
            }
            let h = response.headers();
            if !header_token(h, "connection", "upgrade")
                || !header_token(h, "upgrade", "websocket")
                || !h.get("sec-websocket-accept").is_some_and(|v| v == &accept)
                || h.contains_key("sec-websocket-protocol")
                || h.contains_key("sec-websocket-extensions")
            {
                bail!("Invalid ChatGPT WebSocket handshake. Check your network or proxy.");
            }
            let config = WebSocketConfig::default().max_message_size(Some(16 * 1024 * 1024));
            return Ok(WebSocketStream::from_raw_socket(
                response.upgrade().await?,
                Role::Client,
                Some(config),
            )
            .await);
        }
    }

    pub(super) async fn websocket_request(
        &self,
        session_id: &str,
        properties: Value,
        history: Option<&History<Value>>,
        emit: &(dyn Fn(Delta) + Send + Sync),
    ) -> Result<Response> {
        let mut credentials = self.auth.token(None).await?;
        let mut cached = if history.is_some() {
            self.sockets.take(session_id).await
        } else {
            None
        }
        .filter(|c| {
            c.account == credentials.account_id
                && c.access_token == credentials.access_token
                && properties == c.properties
        });
        let model = properties["model"].as_str().expect("request model");
        let audio = crate::models::model(model).is_some_and(|m| m.audio_input);
        let mut hint = format!("model={model}");
        if let Some(tier) = properties["service_tier"].as_str() {
            hint.push_str(&format!(";tier={tier}"));
        }
        let mut attempts = Attempts::default();
        'attempt: loop {
            let mut frame = properties.clone();
            frame["type"] = json!("response.create");
            let checkpoint = cached.take();
            if let Some(history) = history {
                // The base request carries the stable developer prefix. It participates
                // in the properties comparison above, but is inherited on a continuation.
                let mut start = 0;
                if let Some(c) = &checkpoint
                    && history.len() >= c.input_and_output.len()
                    && c.input_and_output
                        .iter()
                        .eq(history.iter().take(c.input_and_output.len()))
                {
                    frame["previous_response_id"] = json!(c.response_id);
                    start = c.input_and_output.len();
                    frame["input"] = json!([]);
                }
                frame["input"]
                    .as_array_mut()
                    .expect("request input")
                    .extend(history.range(start).map(crate::project_guidance::wire_item));
            }
            let chained = frame.get("previous_response_id").is_some();
            let frame = tokio::task::spawn_blocking(move || {
                crate::media::project(&mut frame, audio)?;
                Ok::<_, anyhow::Error>(serde_json::to_string(&frame)?)
            })
            .await??;
            let mut socket = if let Some(c) = checkpoint {
                c.socket
            } else {
                self.connect(session_id, &hint, &mut credentials, &mut attempts, emit)
                    .await?
            };
            timeout(Duration::from_secs(90), send_create(&mut socket, frame))
                .await
                .map_err(|_| StreamInterrupted)?
                .map_err(transport_error)?;
            let mut stream = ResponseStream {
                previous_calls: history
                    .into_iter()
                    .flat_map(|h| h.iter())
                    .filter(|item| is_tool_call(item))
                    .filter_map(|item| item["call_id"].as_str())
                    .collect(),
                ..Default::default()
            };
            loop {
                let msg = timeout(Duration::from_secs(120), socket.next())
                    .await
                    .map_err(|_| StreamInterrupted)?
                    .ok_or(StreamInterrupted)?
                    .map_err(transport_error)?;
                let event: Value = match msg {
                    Message::Text(text) => {
                        serde_json::from_str(&text).context("Invalid JSON in WebSocket event")?
                    }
                    Message::Close(_) => return Err(StreamInterrupted.into()),
                    Message::Ping(_) | Message::Pong(_) => continue,
                    _ => bail!("Unexpected non-text ChatGPT WebSocket message"),
                };
                let before_output =
                    stream.first_token.is_none() && stream.completed_items.is_empty();
                if chained
                    && before_output
                    && event["type"] == "error"
                    && event["error"]["code"] == "previous_response_not_found"
                {
                    continue 'attempt;
                }
                if event["type"] == "error" {
                    if event["error"]["code"] == "websocket_connection_limit_reached" {
                        return Err(StreamInterrupted.into());
                    }
                    if before_output
                        && !is_context_limit(&event)
                        && let Some(status) = event
                            .get("status")
                            .or_else(|| event.get("status_code"))
                            .and_then(Value::as_u64)
                            .and_then(|n| u16::try_from(n).ok())
                            .and_then(|n| StatusCode::from_u16(n).ok())
                    {
                        let retry_after = [&event["error"]["headers"], &event["headers"]]
                            .into_iter()
                            .filter_map(Value::as_object)
                            .flat_map(|h| h.iter())
                            .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
                            .and_then(|(_, v)| v.as_u64().or_else(|| v.as_str()?.parse().ok()));
                        if attempts
                            .rejected(status, retry_after, &self.auth, &mut credentials, emit)
                            .await?
                        {
                            continue 'attempt;
                        }
                    }
                }
                let id = event["response"]["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned);
                match stream.event(event, emit, Instant::now()) {
                    Ok(Some(response)) => {
                        if let (Some(input), Some(response_id)) = (history, id) {
                            let mut input_and_output = input.clone();
                            input_and_output.extend(response.output.iter().cloned());
                            self.sockets.park(
                                session_id,
                                Checkpoint {
                                    socket,
                                    properties,
                                    input_and_output,
                                    response_id,
                                    account: credentials.account_id,
                                    access_token: credentials.access_token,
                                },
                            );
                        }
                        return Ok(response);
                    }
                    Ok(None) => {}
                    Err(error)
                        if error.is::<Overload>()
                            && before_output
                            && attempts.transient < MAX_TRANSIENT_RETRIES =>
                    {
                        let delay_secs = 1 << attempts.transient;
                        attempts.transient += 1;
                        emit(Delta::Retrying(RetryStatus::Overloaded { delay_secs }));
                        tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                        continue 'attempt;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
}

// A single text message can contain many images. Fragment it with backpressure
// so the first full upload is not limited by a gateway's per-frame limit.
async fn send_create(socket: &mut Socket, frame: String) -> Result<(), Error> {
    use tokio_tungstenite::tungstenite::{
        Bytes,
        protocol::frame::{
            Frame,
            coding::{Data, OpCode},
        },
    };
    const CHUNK: usize = 1024 * 1024;
    if frame.len() <= CHUNK {
        return socket.send(Message::text(frame)).await;
    }
    let mut data = Bytes::from(frame);
    let mut kind = Data::Text;
    while !data.is_empty() {
        let chunk = data.split_to(data.len().min(CHUNK));
        socket
            .send(Message::Frame(Frame::message(
                chunk,
                OpCode::Data(kind),
                data.is_empty(),
            )))
            .await?;
        kind = Data::Continue;
    }
    Ok(())
}

fn header_token(headers: &HeaderMap, name: &str, token: &str) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .any(|s| s.trim().eq_ignore_ascii_case(token))
}

fn transport_error(error: Error) -> anyhow::Error {
    match error {
        Error::Io(_)
        | Error::ConnectionClosed
        | Error::AlreadyClosed
        | Error::Protocol(
            tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
        ) => StreamInterrupted.into(),
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{authenticated_store, gated_server, header};

    #[tokio::test]
    async fn image_chain_projects_only_new_items_and_recovers_a_rejected_id_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = authenticated_store(dir.path().join("state"));
        let (url, mut requests, server) = gated_server().await;
        let provider = Provider::fixture(store, url);
        let media = crate::media::MediaStore::new(dir.path().join("media"))
            .image(&crate::media::tests::png())
            .unwrap();
        let mut session = Session::new(dir.path().into(), &crate::store::Settings::default());
        session
            .input
            .push(json!({"role":"user", "content":[{"type":"pico_media", "media":media}]}));
        let image_path = media.prepared.unwrap();
        let parked = image_path.with_extension("parked");
        let call =
            json!({"type":"custom_tool_call","call_id":"inspect","name":"exec","input":"text(1)"});
        let result = json!({"type":"custom_tool_call_output","call_id":"inspect","output":"1"});
        let question = json!({"role":"user","content":"What colour was it?"});

        let fixture = async {
            let completion = |id, output: Value| {
                (
                    200,
                    json!({"type":"response.completed","response":{
                        "id":id,"status":"completed","output":output
                    }})
                    .to_string(),
                )
            };
            let (first, reply) = requests.recv().await.unwrap();
            assert_eq!(
                header(&first, "openai-beta"),
                Some("responses_websockets=2026-02-06")
            );
            let body = |r: &str| {
                serde_json::from_str::<Value>(r.split_once("\r\n\r\n").unwrap().1).unwrap()
            };
            let full = body(&first);
            assert!(full.get("previous_response_id").is_none());
            assert!(full.get("instructions").is_none());
            assert_eq!(full["input"][0]["role"], "developer");
            assert_eq!(full["input"][0]["content"][0]["text"], "fixture");
            assert_eq!(full["input"][1]["content"][0]["type"], "input_image");
            reply.send(completion("R1", json!([call]))).unwrap();

            let (second, reply) = requests.recv().await.unwrap();
            assert_eq!(
                header(&first, "sec-websocket-key"),
                header(&second, "sec-websocket-key")
            );
            assert_eq!(body(&second)["previous_response_id"], "R1");
            assert_eq!(body(&second)["input"], json!([result]));
            reply.send(completion("R2", json!([]))).unwrap();

            let (third, reply) = requests.recv().await.unwrap();
            assert_eq!(body(&third)["previous_response_id"], "R2");
            assert_eq!(body(&third)["input"], json!([question]));
            reply.send((200,json!({"type":"error","status":400,"error":{"code":"previous_response_not_found"}}).to_string())).unwrap();
            let (reset, reply) = requests.recv().await.unwrap();
            assert!(body(&reset).get("previous_response_id").is_none());
            assert_eq!(
                body(&reset)["input"],
                json!([full["input"][0], full["input"][1], call, result, question])
            );
            reply.send(completion("R3", json!([]))).unwrap();
        };
        let client = async {
            let cancel = CancellationToken::new();
            let response = provider
                .respond(&session, "fixture", &cancel, &|_| {})
                .await
                .unwrap();
            session.input.extend(response.output);
            session.input.push(result.clone());
            // If the client projects before slicing history, this fails locally.
            std::fs::rename(&image_path, &parked).unwrap();
            provider
                .respond(&session, "fixture", &cancel, &|_| {})
                .await
                .unwrap();
            std::fs::rename(&parked, &image_path).unwrap();
            session.input.push(question.clone());
            provider
                .respond(&session, "fixture", &cancel, &|_| {})
                .await
                .unwrap();
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(client, fixture);
        })
        .await
        .unwrap();
        server.abort();
    }
}

#[cfg(test)]
mod effort_acceptance {
    use super::*;

    /// Exercise the actual upgrade, on one socket, without fallback or replay.
    #[tokio::test]
    #[ignore = "uses ChatGPT subscription; requires PICO_EFFORT_ACCEPTANCE_AUTH"]
    async fn live_configuration_update() -> Result<()> {
        let path = std::env::var_os("PICO_EFFORT_ACCEPTANCE_AUTH")
            .context("Supply the path to an authorized Pico login")?;
        let mut credentials: crate::auth::Credentials =
            serde_json::from_slice(&std::fs::read(path)?)?;
        if credentials.expires_at < crate::store::now() + 300 {
            bail!("Refresh the login in Pico before running this check");
        }
        credentials.refresh_token.clear();
        let root = tempfile::tempdir()?;
        let provider = Provider::new(Auth::new(crate::store::Store::open(root.path().into())?)?)?;
        let mut failures = Vec::new();
        for model in crate::models::MODELS {
            let result = async {
                let id = crate::store::id();
                let mut socket = provider.connect(&id, &format!("model={}", model.id), &mut credentials, &mut Attempts::default(), &|_| {}).await?;
                let base = json!({"type":"response.create","model":model.id,"reasoning":{"effort":"low","summary":"auto"},"tools":[],"stream":true,"store":false,"include":["reasoning.encrypted_content"],"prompt_cache_key":id});
                let mut first = base.clone();
                first["input"] = json!([developer_message("Answer briefly."),{"role":"user","content":"Reply with just the number 3."}]);
                let first = completion(&mut socket, first).await.context("baseline")?;
                let mut next = base;
                next["previous_response_id"] = json!(first["id"].as_str().context("missing baseline response ID")?);
                next["input"] = json!([{"type":"configuration_update","reasoning":{"effort":"high"}},{"role":"user","content":"Add one to that number. Reply with just the number."}]);
                let response = completion(&mut socket, next).await.context("chained update")?;
                let text = response["output"].as_array().context("missing output")?.iter()
                    .filter_map(|item| item["content"].as_array()).flatten().filter_map(|part| part["text"].as_str()).collect::<String>();
                anyhow::ensure!(text.trim()=="4", "lost prior context");
                println!("{}: chained update completed; response.reasoning.effort={}",model.id,response["reasoning"]["effort"]);
                Ok::<_, anyhow::Error>(())
            }.await;
            if let Err(e) = result {
                println!("{}: {e:#}", model.id);
                failures.push(model.id);
            }
        }
        anyhow::ensure!(failures.is_empty(), "failed for: {}", failures.join(", "));
        Ok(())
    }

    async fn completion(socket: &mut Socket, frame: Value) -> Result<Value> {
        send_create(socket, serde_json::to_string(&frame)?).await?;
        timeout(Duration::from_secs(120), async {
            loop {
                match socket.next().await.context("socket ended")?? {
                    Message::Text(raw) => {
                        let event: Value = serde_json::from_str(&raw)?;
                        match event["type"].as_str() {
                            Some("response.completed")
                                if event["response"]["status"] == "completed" =>
                            {
                                return Ok(event["response"].clone());
                            }
                            Some("error" | "response.failed" | "response.incomplete") => {
                                let error =
                                    event.get("error").unwrap_or(&event["response"]["error"]);
                                bail!("{}: {}", error["code"], error["message"]);
                            }
                            _ => {}
                        }
                    }
                    Message::Ping(_) | Message::Pong(_) => (),
                    _ => bail!("socket closed or unexpected non-text message"),
                }
            }
        })
        .await?
    }
}
