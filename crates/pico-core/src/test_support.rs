//! Local-only protocol fixture; no test reads user credentials or calls OpenAI.
pub mod sqlite;

use crate::store::Store;
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};

pub fn authenticated_store(root: PathBuf) -> Store {
    let store = Store::open(root).unwrap();
    store
        .write(
            "auth.json",
            &crate::auth::Credentials {
                access_token: "fixture".into(),
                refresh_token: "fixture".into(),
                expires_at: crate::store::now() + 3600,
                account_id: Some("fixture".into()),
                email: None,
            },
        )
        .unwrap();
    store
}

pub async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}

pub async fn server(
    replies: impl IntoIterator<Item = (u16, String), IntoIter: Send + 'static>,
) -> (
    String,
    mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<()>,
) {
    server_with(replies.into_iter().map(|reply| move |_: &str| reply)).await
}

/// Scripted responses that can inspect the request (for generated tool/cell IDs).
pub async fn server_with<F: FnOnce(&str) -> (u16, String) + Send + 'static>(
    replies: impl IntoIterator<Item = F, IntoIter: Send + 'static>,
) -> (
    String,
    mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    let replies = replies.into_iter();
    let task = tokio::spawn(async move {
        for reply in replies {
            let (mut socket, _) =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let request = read_request(&mut socket).await;
            let (status, body) = reply(&request);
            tx.send(request).unwrap();
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (url, rx, task)
}
pub fn completed(output: serde_json::Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({"type":"response.completed","response":{"status":"completed","output":output,"usage":{"input_tokens":37,"output_tokens":11}}})
    )
}

/// Assert the public spill notice identifies a private file with the exact size.
pub fn saved_output(notice: &str) -> Vec<u8> {
    use std::os::unix::fs::PermissionsExt;
    let path = notice
        .lines()
        .find_map(|line| line.strip_prefix("Path: "))
        .expect("complete output file path");
    assert!(path.starts_with("/tmp/pico-evo-output-"), "{path}");
    let metadata = std::fs::metadata(path).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert!(
        notice.contains(&format!("({} bytes)", metadata.len())),
        "{notice}"
    );
    std::fs::read(path).unwrap()
}

/// HTTP field names are case-insensitive; field values retain their original case.
pub fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request
        .split_once("\r\n\r\n")
        .unwrap()
        .0
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .find(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

#[test]
fn request_headers_ignore_name_case_but_preserve_values_and_exclude_body() {
    let request =
        "POST / HTTP/1.1\r\nAuThOrIzAtIoN:\tBearer CaseSensitive \r\n\r\nMissing: not-a-header";
    assert_eq!(
        header(request, "authorization"),
        Some("Bearer CaseSensitive")
    );
    assert_eq!(header(request, "missing"), None);
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = vec![];
    let header_end = loop {
        let mut buffer = [0; 4096];
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length: ")
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "0".into())
        .parse()
        .unwrap();
    while bytes.len() < header_end + length {
        let mut buffer = [0; 4096];
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
    }
    String::from_utf8(bytes).unwrap()
}

/// Concurrent requests whose replies are independently released by the test.
pub async fn gated_server() -> (
    String,
    mpsc::UnboundedReceiver<(String, tokio::sync::oneshot::Sender<(u16, String)>)>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let tx = tx.clone();
            tokio::spawn(async move {
                let request = read_request(&mut socket).await;
                let (reply, response) = tokio::sync::oneshot::channel::<(u16, String)>();
                if tx.send((request, reply)).is_err() {
                    return;
                }
                let Ok((status, body)) = response.await else {
                    return;
                };
                let response = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                // Cancellation/deletion can close the client before the gated reply.
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (url, rx, task)
}

/// A response stream whose individual SSE chunks are released by the test.
pub struct StreamingRequest {
    pub body: serde_json::Value,
    pub reply: mpsc::UnboundedSender<String>,
}

pub async fn streaming_server() -> (
    String,
    mpsc::UnboundedReceiver<StreamingRequest>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (tx, requests) = mpsc::unbounded_channel();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let tx = tx.clone();
            tokio::spawn(async move {
                let request = read_request(&mut socket).await;
                let (reply, mut rx) = mpsc::unbounded_channel::<String>();
                if tx
                    .send(StreamingRequest {
                        body: serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1)
                            .unwrap(),
                        reply,
                    })
                    .is_err()
                {
                    return;
                }
                if socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.is_err() {
                    return;
                }
                while let Some(body) = rx.recv().await {
                    let chunk = format!("{:x}\r\n{body}\r\n", body.len());
                    if socket.write_all(chunk.as_bytes()).await.is_err() {
                        return;
                    }
                }
                // Stop/deletion may have closed the client already.
                let _ = socket.write_all(b"0\r\n\r\n").await;
            });
        }
    });
    (endpoint, requests, server)
}
