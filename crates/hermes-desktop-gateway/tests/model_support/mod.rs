//! The harness the model and config tests share: a gateway whose home configures a scripted
//! provider, and a client for it. Each test binary uses part of it.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

pub const TOKEN: &str = "0123456789abcdef0123456789abcdef-model-config-token";
/// The scripted provider's key variable. `load_config` sets it in the test process from each
/// home's `.env`, so a test that changes the key uses a variable of its own.
pub const KEY_ENV: &str = "DESKTOP_MODEL_CONFIG_TEST_KEY";
pub const RELOAD_KEY_ENV: &str = "DESKTOP_MODEL_CONFIG_RELOAD_TEST_KEY";
pub const REFERENCE_KEY_ENV: &str = "DESKTOP_MODEL_CONFIG_REFERENCE_TEST_KEY";
/// A known provider whose key these tests save and remove (`XIAOMI_API_KEY`).
pub const SAVED: &str = "xiaomi";

pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// What the scripted model saw: each request's `(model, Authorization)`.
pub type Seen = Arc<Mutex<Vec<(String, String)>>>;

pub async fn complete(
    State(seen): State<Seen>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let model = body["model"].as_str().unwrap_or_default().to_string();
    seen.lock().unwrap().push((model, auth));
    let chunk = json!({"id": "c", "object": "chat.completion.chunk", "model": "m",
        "choices": [{"index": 0, "delta": {"content": "ok"}, "finish_reason": null}]});
    let last = json!({"id": "c", "object": "chat.completion.chunk", "model": "m",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
    let sse = format!("data: {chunk}\n\ndata: {last}\n\ndata: [DONE]\n\n");
    ([("content-type", "text/event-stream")], sse)
}

/// A scripted model answering chat completions at `http://<address>/v1`, and what it saw.
pub async fn model_server() -> (SocketAddr, Seen) {
    let seen: Seen = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(complete))
        .with_state(Arc::clone(&seen));
    tokio::spawn(async move { axum::serve(listener, app).await });
    (addr, seen)
}

pub struct Started {
    pub addr: SocketAddr,
    pub home: tempfile::TempDir,
    pub seen: Seen,
}

pub async fn start() -> Started {
    start_with(KEY_ENV).await
}

/// A gateway whose home configures the scripted provider `mock` with 2 models and its key,
/// read from `key_env`, in `.env`.
pub async fn start_with(key_env: &str) -> Started {
    start_from(key_env, "").await
}

/// As [`start_with`], with `extra` (indented lines, such as an `api_key`) after `key_env`.
pub async fn start_from(key_env: &str, extra: &str) -> Started {
    let (model_addr, seen) = model_server().await;

    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        format!(
            "model:\n  default: mock-model\n  provider: mock\nproviders:\n  mock:\n    \
             api: http://{model_addr}/v1\n    name: Mock\n    api_mode: chat_completions\n    \
             key_env: {key_env}\n{extra}    models:\n      mock-model: {{}}\n      other-model: {{}}\n"
        ),
    )
    .unwrap();
    std::fs::write(home.path().join(".env"), format!("{key_env}=first-key\n")).unwrap();
    let config = hermes_config::load_config(home.path().to_str()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        hermes_desktop_gateway::Chat::new(config),
    ));
    Started { addr, home, seen }
}

pub struct Client {
    socket: Socket,
}

pub async fn connect(addr: SocketAddr) -> Client {
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws?token={TOKEN}"))
        .await
        .unwrap();
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

    /// The reply to one request; events that arrive meanwhile are dropped.
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
        let created = self
            .call(1, "session.create", json!({"source": "desktop"}))
            .await;
        created["result"]["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Runs one turn in `session` to its `message.complete`.
    pub async fn turn(&mut self, session: &str) {
        let params = json!({"session_id": session, "text": "hello"});
        let request =
            json!({"jsonrpc": "2.0", "id": 2, "method": "prompt.submit", "params": params});
        self.socket
            .send(Message::text(request.to_string()))
            .await
            .unwrap();
        loop {
            let frame = self.next_json().await;
            if frame["id"] == 2 {
                assert!(frame.get("error").is_none(), "{frame}");
            }
            if frame["params"]["type"] == "message.complete" {
                assert_eq!(frame["params"]["session_id"], session);
                return;
            }
        }
    }
}

pub fn code(reply: &Value) -> i64 {
    reply["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("not refused: {reply}"))
}

pub fn env_lines(home: &Path) -> String {
    std::fs::read_to_string(home.join(".env")).unwrap_or_default()
}

/// An in-process gateway whose `llm_providers` are `entries` (`(name, api_key_env)`), each
/// pointed at one scripted model, with default `model`; returns its home, a connected client
/// and what the model saw.
pub async fn start_providers(
    model: &str,
    entries: &[(&str, Option<&str>)],
) -> (tempfile::TempDir, Client, Seen) {
    let (model_addr, seen) = model_server().await;
    let home = tempfile::tempdir().unwrap();
    let mut config = hermes_config::GatewayConfig {
        model: Some(model.into()),
        home_dir: Some(home.path().to_string_lossy().into_owned()),
        ..hermes_config::GatewayConfig::default()
    };
    for (name, var) in entries {
        let entry = json!({"base_url": format!("http://{model_addr}/v1"), "api_key_env": var});
        config
            .llm_providers
            .insert((*name).into(), serde_json::from_value(entry).unwrap());
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        hermes_desktop_gateway::Chat::new(config),
    ));
    (home, connect(addr).await, seen)
}

pub fn auth_sent(seen: &Seen) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|(_, auth)| auth.clone())
        .collect()
}
