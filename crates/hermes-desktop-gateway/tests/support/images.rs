//! The `image.attach` tests' gateway, desktop client and scripted model, which streams `ok`
//! and keeps each request body.

#![allow(dead_code, reason = "each test file uses part of it")]

#[path = "gateway.rs"]
mod gateway;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

pub const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";

pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub type Requests = Arc<Mutex<Vec<Value>>>;

/// Streams `ok` and keeps the request body.
pub async fn complete(
    State(requests): State<Requests>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    requests.lock().unwrap().push(body);
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "c", "object": "chat.completion.chunk", "model": "mock-model",
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let sse = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        chunk(json!({"content": "ok"}), Value::Null),
        chunk(json!({}), json!("stop"))
    );
    ([("content-type", "text/event-stream")], sse)
}

pub struct Started {
    pub addr: SocketAddr,
    pub requests: Requests,
    pub home: tempfile::TempDir,
}

/// A gateway whose model takes image parts (`image_input_mode: native`).
pub async fn start() -> Started {
    let requests = Requests::default();
    let model_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model_listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(complete))
        .with_state(Arc::clone(&requests));
    tokio::spawn(async move { axum::serve(model_listener, app).await });

    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        format!(
            "model:\n  default: mock-model\n  provider: mock\nproviders:\n  mock:\n    \
             api: http://{model_addr}/v1\n    name: Mock\n    api_mode: chat_completions\n    \
             key_env: DESKTOP_IMAGES_TEST_KEY\n    models:\n      mock-model: {{}}\n\
             agent:\n  image_input_mode: native\n"
        ),
    )
    .unwrap();
    let addr = gateway::serve(home.path(), TOKEN, None).await;
    Started {
        addr,
        requests,
        home,
    }
}

pub struct Client {
    pub socket: Socket,
}

pub async fn connect(addr: SocketAddr) -> Client {
    let request = format!("ws://{addr}/api/ws?token={TOKEN}")
        .into_client_request()
        .unwrap();
    let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let mut client = Client { socket };
    assert_eq!(client.next_json().await["params"]["type"], "gateway.ready");
    client
}

impl Client {
    pub async fn next_json(&mut self) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(20), self.socket.next())
                .await
                .expect("no frame within 20 seconds")
                .expect("socket closed");
            match frame.unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }

    /// The reply to `method`; events that arrive meanwhile are dropped.
    pub async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.socket
            .send(Message::text(request.to_string()))
            .await
            .unwrap();
        loop {
            let frame = self.next_json().await;
            if frame["method"] != "event" {
                assert_eq!(frame["id"], id, "{frame}");
                return frame;
            }
        }
    }

    pub async fn create(&mut self) -> String {
        let reply = self.call(1, "session.create", json!({"cols": 96})).await;
        reply["result"]["session_id"].as_str().unwrap().to_string()
    }

    /// Submits `text` and waits for the turn's `message.complete`.
    pub async fn turn(&mut self, id: u64, session: &str, text: &str) {
        let reply = self
            .call(
                id,
                "prompt.submit",
                json!({"session_id": session, "text": text}),
            )
            .await;
        assert!(reply["result"].is_object(), "{reply}");
        loop {
            let frame = self.next_json().await;
            if frame["params"]["type"] == "message.complete" {
                assert_eq!(frame["params"]["payload"]["status"], "complete", "{frame}");
                return;
            }
        }
    }
}

/// A small payload that starts as a PNG does: the gateway checks only the signature.
pub fn png(tag: &str) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(tag.as_bytes());
    bytes
}

pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn data_url(bytes: &[u8]) -> String {
    format!("data:image/png;base64,{}", b64(bytes))
}

/// The files under `<home>/images`.
pub fn staged_images(home: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(home.join("images")) {
        Ok(entries) => entries.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

/// The image data URLs the model was sent with request `n`'s last message.
pub fn sent_images(requests: &Requests, n: usize) -> Vec<String> {
    let requests = requests.lock().unwrap();
    let last = requests[n]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    match last["content"].as_array() {
        Some(parts) => parts
            .iter()
            .filter_map(|part| part["image_url"]["url"].as_str().map(str::to_string))
            .collect(),
        None => Vec::new(),
    }
}
