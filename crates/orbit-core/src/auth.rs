use crate::store::{Store, now};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const ISSUER: &str = "https://auth.openai.com";

#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub account_id: Option<String>,
    pub email: Option<String>,
}
#[derive(Default)]
struct LoginState {
    current: Option<Credentials>,
    needs_save: bool,
}

pub struct Auth {
    store: Store,
    credentials: Arc<Mutex<LoginState>>,
    client: Client,
    issuer: String,
}
impl Auth {
    pub fn new(store: Store) -> Result<Arc<Self>> {
        let credentials = store.read_optional("auth.json")?;
        Ok(Arc::new(Self {
            store,
            issuer: ISSUER.into(),
            credentials: Arc::new(Mutex::new(LoginState {
                current: credentials,
                needs_save: false,
            })),
            client: Client::builder().timeout(Duration::from_secs(60)).build()?,
        }))
    }
    #[cfg(test)]
    pub(crate) fn fixture(store: Store, issuer: String) -> Arc<Self> {
        let mut auth = Self::new(store).unwrap();
        Arc::get_mut(&mut auth).unwrap().issuer = issuer;
        auth
    }
    pub async fn status(&self) -> Option<String> {
        self.credentials
            .lock()
            .await
            .current
            .as_ref()
            .map(|c| c.email.clone().unwrap_or_else(|| "ChatGPT account".into()))
    }
    pub async fn token(self: &Arc<Self>, rejected: Option<&str>) -> Result<Credentials> {
        // A caller cancelling its request must not abandon a rotating refresh token.
        let auth = self.clone();
        let rejected = rejected.map(str::to_owned);
        tokio::spawn(async move { auth.token_inner(rejected.as_deref()).await }).await?
    }
    async fn token_inner(&self, rejected: Option<&str>) -> Result<Credentials> {
        // One refresh per account, including concurrent conversations receiving a 401.
        let mut guard = self.credentials.clone().lock_owned().await;
        let current = guard
            .current
            .as_ref()
            .context("Sign in to ChatGPT in Settings before sending a message")?;
        if current.expires_at > now() + 90 && rejected != Some(current.access_token.as_str()) {
            let current = current.clone();
            if guard.needs_save {
                self.save(guard).await?;
            }
            return Ok(current);
        }
        let response = self
            .client
            .post(format!("{}/oauth/token", self.issuer))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_ID),
                ("refresh_token", current.refresh_token.as_str()),
            ])
            .send()
            .await
            .context("Could not refresh login; check your connection and retry")?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            let secrets = [
                current.access_token.as_bytes(),
                current.refresh_token.as_bytes(),
            ];
            let mut redacted = Vec::new();
            crate::output::redact_prefix(&mut bytes.to_vec(), &secrets, true, |part| {
                redacted.extend_from_slice(part)
            });
            bail!(
                "ChatGPT login refresh failed ({status}). Sign in again in Settings. {}",
                service_error(&redacted)
            );
        }
        let credentials = parse_tokens(serde_json::from_slice(&bytes)?, Some(current))?;
        guard.current = Some(credentials.clone());
        guard.needs_save = true;
        self.save(guard).await?;
        Ok(credentials)
    }
    async fn save(&self, mut state: tokio::sync::OwnedMutexGuard<LoginState>) -> Result<()> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            store.write("auth.json", state.current.as_ref().expect("unsaved credentials"))
                .context("Login is retained in memory but could not be saved. Fix disk space/permissions and retry before quitting")?;
            state.needs_save = false;
            Ok(())
        }).await?
    }
    pub async fn flush(&self) -> Result<()> {
        let state = self.credentials.clone().lock_owned().await;
        if state.needs_save {
            self.save(state).await?;
        }
        Ok(())
    }
    pub async fn logout(&self) -> Result<()> {
        let mut state = self.credentials.clone().lock_owned().await;
        let path = self.store.root().join("auth.json");
        tokio::task::spawn_blocking(move || {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            *state = LoginState::default();
            Ok(())
        })
        .await?
    }
    pub async fn begin(&self) -> Result<Login> {
        let listener = match TcpListener::bind("127.0.0.1:1455").await {
            Ok(l) => l,
            Err(_) => TcpListener::bind("127.0.0.1:1457").await.context(
                "OAuth needs localhost port 1455 or 1457. Close other login flows, then retry",
            )?,
        };
        let redirect = format!(
            "http://localhost:{}/auth/callback",
            listener.local_addr()?.port()
        );
        let verifier = random();
        let state = random();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = Url::parse(&format!("{}/oauth/authorize", self.issuer))?;
        url.query_pairs_mut().extend_pairs(&[
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", &redirect),
            ("scope", "openid profile email offline_access"),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "orbit"),
        ]);
        Ok(Login {
            url: url.to_string(),
            listener,
            redirect,
            verifier,
            state,
        })
    }
    pub async fn finish(&self, login: Login, cancel: CancellationToken) -> Result<String> {
        let future = async {
            loop {
                let (mut socket, _) = login.listener.accept().await?;
                let mut request = Vec::new();
                let received = tokio::time::timeout(Duration::from_secs(5), async {
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = socket.read(&mut buffer).await?;
                        if n == 0 {
                            bail!("Empty OAuth callback");
                        }
                        request.extend_from_slice(&buffer[..n]);
                        if request.len() > 16_384 {
                            bail!("OAuth callback too large");
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                })
                .await;
                if !matches!(received, Ok(Ok(()))) {
                    continue;
                }
                let Ok(request) = String::from_utf8(request) else {
                    continue;
                };
                let Some(path) = request
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("GET "))
                    .and_then(|l| l.split_whitespace().next())
                else {
                    continue;
                };
                let Ok(url) = Url::parse(&format!("http://localhost{path}")) else {
                    continue;
                };
                let query: std::collections::HashMap<_, _> =
                    url.query_pairs().into_owned().collect();
                if url.path() != "/auth/callback" || query.get("state") != Some(&login.state) {
                    socket.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 22\r\n\r\nInvalid login callback").await?;
                    continue;
                }
                if let Some(error) = query.get("error") {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nLogin declined. Return to Orbit and retry.").await?;
                    bail!("Login declined: {error}");
                }
                let code = query
                    .get("code")
                    .context("OAuth callback missing authorization code")?;
                let response = self
                    .client
                    .post(format!("{}/oauth/token", self.issuer))
                    .form(&[
                        ("grant_type", "authorization_code"),
                        ("client_id", CLIENT_ID),
                        ("code", code.as_str()),
                        ("redirect_uri", &login.redirect),
                        ("code_verifier", &login.verifier),
                    ])
                    .send()
                    .await?;
                let status = response.status();
                let bytes = response.bytes().await?;
                if !status.is_success() {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nLogin failed. Return to Orbit for details and retry.").await?;
                    bail!(
                        "Login token exchange failed ({status}): {}",
                        service_error(&bytes)
                    );
                }
                let credentials = parse_tokens(serde_json::from_slice(&bytes)?, None)?;
                let label = credentials
                    .email
                    .clone()
                    .unwrap_or_else(|| "ChatGPT account".into());
                let mut guard = self.credentials.clone().lock_owned().await;
                guard.current = Some(credentials);
                guard.needs_save = true;
                self.save(guard).await?;
                // Login remains successful even if the browser closes the callback socket.
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<h1>Signed in to Orbit</h1><p>You can close this tab and return to Orbit.</p>").await;
                return Ok(label);
            }
        };
        tokio::select! { _ = cancel.cancelled() => bail!("Login cancelled"), result = tokio::time::timeout(Duration::from_secs(300), future) => result.context("Login timed out. Click Sign in to try again")? }
    }
}
#[derive(Debug)]
pub struct Login {
    pub url: String,
    listener: TcpListener,
    redirect: String,
    verifier: String,
    state: String,
}
fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
fn claims(token: &str) -> Option<Value> {
    let part = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).ok()?).ok()
}
fn parse_tokens(body: Value, old: Option<&Credentials>) -> Result<Credentials> {
    let access_token = body["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Login response did not contain an access token")?
        .to_owned();
    let refresh_token = body["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| old.map(|c| c.refresh_token.as_str()))
        .context("Login response did not contain a refresh token")?
        .to_owned();
    let expires = body["expires_in"]
        .as_u64()
        .filter(|n| *n > 0 && *n < 365 * 86400)
        .context("Login response has invalid expiry")?;
    if let Some(kind) = body["token_type"].as_str()
        && !kind.eq_ignore_ascii_case("bearer")
    {
        bail!("Unsupported login token type");
    }
    // JWT claims are display/routing hints, never treated as verification of identity.
    let id_claims = body["id_token"].as_str().and_then(claims);
    let access_claims = claims(&access_token);
    let account_id = id_claims
        .iter()
        .chain(access_claims.iter())
        .find_map(|c| c["https://api.openai.com/auth"]["chatgpt_account_id"].as_str())
        .map(str::to_owned)
        .or_else(|| old.and_then(|c| c.account_id.clone()));
    let email = id_claims
        .iter()
        .chain(access_claims.iter())
        .find_map(|c| c["email"].as_str())
        .map(str::to_owned)
        .or_else(|| old.and_then(|c| c.email.clone()));
    Ok(Credentials {
        access_token,
        refresh_token,
        expires_at: now() + expires,
        account_id,
        email,
    })
}
pub(crate) fn safe_error(body: &Value) -> String {
    let text = body["error"]["message"]
        .as_str()
        .or_else(|| body["error_description"].as_str())
        .or_else(|| body["message"].as_str());
    match text {
        Some(text) => crate::output::preserve_text(text, 500),
        None => crate::output::preserve_text(&body.to_string(), 500),
    }
}
fn service_error(bytes: &[u8]) -> String {
    match serde_json::from_slice(bytes) {
        Ok(body) => safe_error(&body),
        Err(_) => crate::output::preserve_bytes(bytes, 500),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_service_error_preserves_details_after_old_cutoff() {
        let message = format!("{} crucial diagnostic at the end", "λ".repeat(501));
        let result = safe_error(&serde_json::json!({"error": {"message": message}}));
        assert_eq!(
            crate::test_support::saved_output(&result),
            message.as_bytes()
        );
        assert_eq!(
            safe_error(&serde_json::json!({"message": "small error"})),
            "small error"
        );
        assert_eq!(
            crate::test_support::saved_output(&service_error(b"proxy\xfferror")),
            b"proxy\xfferror"
        );
        let unknown = serde_json::json!({"unrecognized_error": "x".repeat(501)});
        assert_eq!(
            crate::test_support::saved_output(&safe_error(&unknown)),
            unknown.to_string().as_bytes()
        );
    }

    #[tokio::test]
    async fn rotated_token_is_retained_and_resaved_after_storage_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        store
            .write(
                "auth.json",
                &Credentials {
                    access_token: "old".into(),
                    refresh_token: "old-refresh".into(),
                    expires_at: 0,
                    account_id: None,
                    email: None,
                },
            )
            .unwrap();
        let mut auth = Auth::new(store.clone()).unwrap();
        let (url, _requests, server) = crate::test_support::server(vec![(
            200,
            r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#.into(),
        )])
        .await;
        Arc::get_mut(&mut auth).unwrap().issuer = url;
        std::fs::remove_file(store.root().join("auth.json")).unwrap();
        std::fs::create_dir(store.root().join("auth.json")).unwrap();
        assert!(auth.token(None).await.is_err());
        server.await.unwrap();
        std::fs::remove_dir(store.root().join("auth.json")).unwrap();
        auth.token(None).await.unwrap();
        let credentials: Credentials = store.read_optional("auth.json").unwrap().unwrap();
        assert_eq!(credentials.refresh_token, "rotated");
        assert_eq!(auth.token(None).await.unwrap().access_token, "new");
    }
    #[tokio::test]
    async fn cancelled_request_does_not_abandon_rotating_refresh_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        store
            .write(
                "auth.json",
                &Credentials {
                    access_token: "old-access".into(),
                    refresh_token: "old-refresh".into(),
                    expires_at: 0,
                    account_id: Some("account".into()),
                    email: None,
                },
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut auth = Auth::new(store.clone()).unwrap();
        Arc::get_mut(&mut auth).unwrap().issuer =
            format!("http://{}", listener.local_addr().unwrap());
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = crate::test_support::read_request(&mut socket).await;
            assert!(request.starts_with("POST /oauth/token "));
            seen_tx.send(()).unwrap();
            release_rx.await.unwrap();
            let body = r#"{"access_token":"new-access","refresh_token":"rotated-refresh","expires_in":3600}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        });
        let requester = auth.clone();
        let task = tokio::spawn(async move { requester.token(None).await });
        seen_rx.await.unwrap();
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        release_tx.send(()).unwrap();
        server.await.unwrap();
        let credentials = auth.token(None).await.unwrap();
        assert_eq!(credentials.refresh_token, "rotated-refresh");
        let saved: Credentials = store.read_optional("auth.json").unwrap().unwrap();
        assert_eq!(saved.access_token, "new-access");
        assert_eq!(saved.refresh_token, "rotated-refresh");
    }
    #[tokio::test]
    async fn oauth_rejects_wrong_state_then_accepts_valid_callback_and_persists_login() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let mut auth = Auth::new(store.clone()).unwrap();
        let (url,mut requests,server)=crate::test_support::server(vec![(200,r#"{"access_token":"access","refresh_token":"refresh","expires_in":3600,"token_type":"Bearer"}"#.into())]).await;
        Arc::get_mut(&mut auth).unwrap().issuer = url;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let login = Login {
            url: String::new(),
            listener,
            redirect: format!("http://localhost:{port}/auth/callback"),
            verifier: "test-verifier".into(),
            state: "expected-state".into(),
        };
        let flow = auth.clone();
        let task = tokio::spawn(async move { flow.finish(login, CancellationToken::new()).await });
        let bad = reqwest::get(format!(
            "http://127.0.0.1:{port}/auth/callback?state=wrong&code=no"
        ))
        .await
        .unwrap();
        assert_eq!(bad.status().as_u16(), 400);
        let good = reqwest::get(format!(
            "http://127.0.0.1:{port}/auth/callback?state=expected-state&code=authorized"
        ))
        .await
        .unwrap();
        assert!(good.status().is_success());
        assert!(task.await.unwrap().is_ok());
        server.await.unwrap();
        let request = requests.recv().await.unwrap();
        assert!(request.contains("code_verifier=test-verifier"));
        assert!(request.contains("code=authorized"));
        let persisted: Credentials = store.read_optional("auth.json").unwrap().unwrap();
        assert_eq!(persisted.refresh_token, "refresh");
    }
    #[test]
    fn refresh_preserves_rotatable_credentials_and_checks_expiry() {
        let old = Credentials {
            access_token: "old".into(),
            refresh_token: "refresh".into(),
            expires_at: 0,
            account_id: Some("account".into()),
            email: None,
        };
        let new = parse_tokens(
            serde_json::json!({"access_token":"new", "expires_in":3600}),
            Some(&old),
        )
        .unwrap();
        assert_eq!(new.refresh_token, "refresh");
        assert_eq!(new.account_id.as_deref(), Some("account"));
        assert!(
            parse_tokens(
                serde_json::json!({"access_token":"new","expires_in":0}),
                Some(&old)
            )
            .is_err()
        );
    }
}
