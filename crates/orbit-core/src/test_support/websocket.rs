//! Local Responses fixture. Script strings contain one JSON event per line;
//! each event is sent as its own text message over a real WebSocket.
use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};

type Socket = WebSocketStream<TcpStream>;
#[allow(clippy::result_large_err)] // Callback error type is fixed by tungstenite.
async fn upgrade(stream: TcpStream) -> (Socket, String) {
    let mut headers = String::new();
    let socket = accept_hdr_async(stream, |req: &Request, resp: Response| {
        headers = format!("GET {} HTTP/1.1\r\n", req.uri());
        for (name, value) in req.headers() {
            headers.push_str(&format!("{name}: {}\r\n", value.to_str().unwrap()));
        }
        headers.push_str("\r\n");
        Ok(resp)
    })
    .await
    .unwrap();
    (socket, headers)
}

async fn request(socket: &mut Socket, headers: &str) -> Option<String> {
    let Message::Text(frame) = socket.next().await?.ok()? else {
        return None;
    };
    let value: serde_json::Value = serde_json::from_str(frame.as_str()).unwrap();
    assert_eq!(value["type"], "response.create");
    Some(format!("{headers}{frame}"))
}

async fn send(socket: &mut Socket, status: u16, body: &str) -> bool {
    if status != 200 {
        let event = serde_json::json!({"type":"error","status":status,"error":{"message":body}});
        return socket.send(Message::text(event.to_string())).await.is_ok();
    }
    for event in body.lines().filter(|s| !s.is_empty()) {
        if socket.send(Message::text(event.to_owned())).await.is_err() {
            return false;
        }
    }
    true
}

type Script = (
    String,
    mpsc::UnboundedReceiver<String>,
    tokio::task::JoinHandle<()>,
);

pub async fn ws_server(
    replies: impl IntoIterator<Item = (u16, String), IntoIter: Send + 'static>,
) -> Script {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    let replies = replies.into_iter();
    let task = tokio::spawn(async move {
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().await.unwrap();
            if status != 200 {
                tx.send(super::read_request(&mut stream).await).unwrap();
                let msg = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(msg.as_bytes()).await.unwrap();
            } else {
                let (mut ws, headers) = upgrade(stream).await;
                tx.send(request(&mut ws, &headers).await.unwrap()).unwrap();
                send(&mut ws, status, &body).await;
            }
        }
    });
    (url, rx, task)
}

pub async fn server_with<F: FnOnce(&str) -> (u16, String) + Send + 'static>(
    replies: impl IntoIterator<Item = F, IntoIter: Send + 'static>,
) -> Script {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    let replies = replies.into_iter();
    let task = tokio::spawn(async move {
        for reply in replies {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut ws, headers) = upgrade(stream).await;
            let req = request(&mut ws, &headers).await.unwrap();
            let (status, body) = reply(&req);
            tx.send(req).unwrap();
            send(&mut ws, status, &body).await;
        }
    });
    (url, rx, task)
}

pub async fn gated_server() -> (
    String,
    mpsc::UnboundedReceiver<(String, tokio::sync::oneshot::Sender<(u16, String)>)>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let tx = tx.clone();
            tokio::spawn(async move {
                let (mut ws, headers) = upgrade(stream).await;
                while let Some(req) = request(&mut ws, &headers).await {
                    let (reply, result) = tokio::sync::oneshot::channel::<(u16, String)>();
                    if tx.send((req, reply)).is_err() {
                        break;
                    }
                    let Ok((status, body)) = result.await else {
                        break;
                    };
                    if !send(&mut ws, status, &body).await {
                        break;
                    }
                }
            });
        }
    });
    (url, rx, task)
}

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
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let tx = tx.clone();
            tokio::spawn(async move {
                let (mut ws, headers) = upgrade(stream).await;
                while let Some(req) = request(&mut ws, &headers).await {
                    let (reply, mut events) = mpsc::unbounded_channel::<String>();
                    let body = serde_json::from_str(req.split_once("\r\n\r\n").unwrap().1).unwrap();
                    if tx.send(StreamingRequest { body, reply }).is_err() {
                        return;
                    }
                    loop {
                        let Some(batch) = events.recv().await else {
                            return;
                        };
                        if !send(&mut ws, 200, &batch).await {
                            return;
                        }
                        if batch
                            .lines()
                            .filter_map(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                            .any(|e| e["type"] == "response.completed")
                        {
                            break;
                        }
                    }
                }
            });
        }
    });
    (url, rx, task)
}
