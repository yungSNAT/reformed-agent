//! The `llm.oneshot` tests' gateway and scripted OpenAI-compatible model, which keeps each
//! request body and holds a prompt containing `SLOW` until the test lets it through.

#![allow(dead_code, reason = "each test file uses part of it")]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

pub const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";

/// What the scripted model answers.
pub const REPLY: &str = "```\nfeat(gateway): serve one-shot requests\n```";

/// What it answers a prompt containing `THINK`: its reasoning inline, as a local reasoning
/// model sends it.
pub const THINKING_REPLY: &str =
    "<think>look at the diff</think>\n<Thinking>\nstill\n</Thinking>\nfeat: x\n<thought>a</thought>";

pub type Requests = Arc<Mutex<Vec<Value>>>;

/// One permit per `SLOW` request the test lets through ([`release_slow`]), so a test orders
/// events by what it lets through rather than by how long anything takes.
static SLOW_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(0);

/// How many `SLOW` requests have reached the model.
static SLOW_ARRIVED: AtomicUsize = AtomicUsize::new(0);

/// Lets one held `SLOW` request be answered.
pub fn release_slow() {
    SLOW_GATE.add_permits(1);
}

/// Waits until `count` `SLOW` requests have reached the model.
pub async fn slow_arrived(count: usize) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while SLOW_ARRIVED.load(Ordering::SeqCst) < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{count} SLOW requests did not reach the model within 20 seconds"));
}

async fn complete(State(requests): State<Requests>, Json(body): Json<Value>) -> Response {
    let stream = body["stream"] == true;
    let text = body.to_string();
    if text.contains("SLOW") {
        SLOW_ARRIVED.fetch_add(1, Ordering::SeqCst);
        // Never closed, so it cannot fail.
        if let Ok(permit) = SLOW_GATE.acquire().await {
            permit.forget();
        }
    }
    let reply = if text.contains("THINK") {
        THINKING_REPLY
    } else {
        REPLY
    };
    requests.lock().unwrap().push(body);
    if !stream {
        return Json(json!({
            "id": "c", "object": "chat.completion", "model": "mock-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": reply},
                         "finish_reason": "stop"}],
        }))
        .into_response();
    }
    let chunk = |delta: Value, finish: Value| {
        json!({"id": "c", "object": "chat.completion.chunk", "model": "mock-model",
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    let sse = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        chunk(json!({"content": reply}), Value::Null),
        chunk(json!({}), json!("stop"))
    );
    ([("content-type", "text/event-stream")], sse).into_response()
}

/// A gateway with the scripted model, or with no model when `with_model` is false.
pub async fn start(with_model: bool) -> (SocketAddr, Requests, tempfile::TempDir) {
    start_with(with_model, None).await
}

/// As [`start`], with the gateway's write timeout set to `write_timeout`.
pub async fn start_with(
    with_model: bool,
    write_timeout: Option<Duration>,
) -> (SocketAddr, Requests, tempfile::TempDir) {
    let requests = Requests::default();
    let model_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model_listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(complete))
        .with_state(Arc::clone(&requests));
    tokio::spawn(async move { axum::serve(model_listener, app).await });

    let home = tempfile::tempdir().unwrap();
    let model = if with_model {
        "model:\n  default: mock-model\n  provider: mock\n"
    } else {
        ""
    };
    std::fs::write(
        home.path().join("config.yaml"),
        format!(
            "{model}providers:\n  mock:\n    api: http://{model_addr}/v1\n    name: Mock\n    \
             api_mode: chat_completions\n    key_env: DESKTOP_ONESHOT_TEST_KEY\n    \
             models:\n      mock-model: {{}}\n"
        ),
    )
    .unwrap();
    let config = hermes_config::load_config(home.path().to_str()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut chat = hermes_desktop_gateway::Chat::new(config);
    if let Some(timeout) = write_timeout {
        chat = chat.with_write_timeout(timeout);
    }
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        chat,
    ));
    (addr, requests, home)
}

pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub async fn connect(addr: SocketAddr) -> Socket {
    let request = format!("ws://{addr}/api/ws?token={TOKEN}")
        .into_client_request()
        .unwrap();
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(
        next_json(&mut socket).await["params"]["type"],
        "gateway.ready"
    );
    socket
}

pub async fn next_json(socket: &mut Socket) -> Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(20), socket.next())
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

pub async fn call(socket: &mut Socket, id: u64, params: Value) -> Value {
    rpc(socket, id, "llm.oneshot", params).await
}

pub async fn send(socket: &mut Socket, id: u64, method: &str, params: Value) {
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    socket
        .send(Message::text(request.to_string()))
        .await
        .unwrap();
}

/// The reply to `method`; events that arrive meanwhile are dropped.
pub async fn rpc(socket: &mut Socket, id: u64, method: &str, params: Value) -> Value {
    send(socket, id, method, params).await;
    loop {
        let frame = next_json(socket).await;
        if frame["method"] != "event" {
            assert_eq!(frame["id"], id, "{frame}");
            return frame;
        }
    }
}
