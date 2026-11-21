//! Local-only protocol fixture; no test reads user credentials or calls OpenAI.
pub mod sqlite;

use crate::store::Store;
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};

/// Wall-clock stages for the ignored local-turn probe, collected outside the DB lock.
#[derive(Debug)]
pub enum SaveTiming {
    WorkerQueue(Duration),
    Database {
        wait: Duration,
        body: Duration,
        commit: Duration,
    },
}

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
            let (status, body) = reply;
            tx.send(request).unwrap();
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (url, rx, task)
}
pub fn completed(output: serde_json::Value) -> String {
    format!(
        "{}\n\n",
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
    assert!(path.starts_with("/tmp/orbit-output-"), "{path}");
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

pub async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
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

mod websocket;
pub use websocket::{StreamingRequest, gated_server, server_with, streaming_server, ws_server};
