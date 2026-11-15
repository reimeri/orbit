//! Local-only protocol fixture; no test reads user credentials or calls OpenAI.
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};

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
        for (status, body) in replies {
            let (mut socket, _) =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
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
            tx.send(String::from_utf8(bytes).unwrap()).unwrap();
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
