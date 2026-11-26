//! `llm.oneshot` as the desktop's review pane and new-project dialog call it: one model call
//! outside any conversation. The model is a scripted OpenAI-compatible endpoint that keeps
//! each request body.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";

/// What the scripted model answers.
const REPLY: &str = "```\nfeat(gateway): serve one-shot requests\n```";

type Requests = Arc<Mutex<Vec<Value>>>;

async fn complete(State(requests): State<Requests>, Json(body): Json<Value>) -> Response {
    let stream = body["stream"] == true;
    requests.lock().unwrap().push(body);
    if !stream {
        return Json(json!({
            "id": "c", "object": "chat.completion", "model": "mock-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": REPLY},
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
        chunk(json!({"content": REPLY}), Value::Null),
        chunk(json!({}), json!("stop"))
    );
    ([("content-type", "text/event-stream")], sse).into_response()
}

/// A gateway with the scripted model, or with no model when `with_model` is false.
async fn start(with_model: bool) -> (SocketAddr, Requests, tempfile::TempDir) {
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
    let chat = hermes_desktop_gateway::Chat::new(config);
    tokio::spawn(hermes_desktop_gateway::serve(listener, TOKEN.to_string(), chat));
    (addr, requests, home)
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: SocketAddr) -> Socket {
    let request = format!("ws://{addr}/api/ws?token={TOKEN}")
        .into_client_request()
        .unwrap();
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(next_json(&mut socket).await["params"]["type"], "gateway.ready");
    socket
}

async fn next_json(socket: &mut Socket) -> Value {
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

async fn call(socket: &mut Socket, id: u64, params: Value) -> Value {
    let request = json!({"jsonrpc": "2.0", "id": id, "method": "llm.oneshot", "params": params});
    socket
        .send(Message::text(request.to_string()))
        .await
        .unwrap();
    loop {
        let frame = next_json(socket).await;
        if frame["method"] != "event" {
            assert_eq!(frame["id"], id, "{frame}");
            return frame;
        }
    }
}

/// The review pane's request: the `commit_message` template with the diff, recent subjects and
/// the message to avoid, at the temperature it asks for; the fence the model added is removed.
#[tokio::test]
async fn the_commit_message_template_goes_to_the_model() {
    let (addr, requests, _home) = start(true).await;
    let mut socket = connect(addr).await;
    let params = json!({
        "template": "commit_message",
        "temperature": 0.8,
        "variables": {"diff": "+fn served() {}", "recent_commits": "fix: an earlier one",
                      "avoid": "feat: the last proposal"},
    });
    let reply = call(&mut socket, 2, params).await;
    assert_eq!(
        reply["result"],
        json!({"text": "feat(gateway): serve one-shot requests"}),
        "{reply}"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let body = &requests[0];
    assert_eq!(body["temperature"], 0.8);
    assert_eq!(body["max_tokens"], 1024);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "{body}");
    assert_eq!(messages[0]["role"], "system");
    let system = messages[0]["content"].as_str().unwrap();
    assert!(system.starts_with("You write git commit messages."), "{system}");
    let user = messages[1]["content"].as_str().unwrap();
    for part in ["+fn served() {}", "fix: an earlier one", "feat: the last proposal"] {
        assert!(user.contains(part), "{part} missing from {user}");
    }
}

/// Raw instructions and input, as the new-project dialog sends them; an unknown `session_id`
/// is not an error, as upstream.
#[tokio::test]
async fn instructions_and_input_go_as_they_are() {
    let (addr, requests, _home) = start(true).await;
    let mut socket = connect(addr).await;
    let params = json!({"instructions": "Write an idea.", "input": "Project name: x",
                        "max_tokens": 50, "session_id": "not-a-session"});
    let reply = call(&mut socket, 2, params).await;
    assert!(reply["result"]["text"].is_string(), "{reply}");
    let body = requests.lock().unwrap()[0].clone();
    assert_eq!(body["max_tokens"], 50);
    assert_eq!(body["temperature"], 0.3);
    let sent: Vec<(&str, &str)> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["role"].as_str().unwrap(), m["content"].as_str().unwrap()))
        .collect();
    assert_eq!(
        sent,
        [("system", "Write an idea."), ("user", "Project name: x")]
    );
}

/// Upstream's refusals, none of which reaches the model.
#[tokio::test]
async fn llm_oneshot_refuses_bad_requests() {
    let (addr, requests, _home) = start(true).await;
    let mut socket = connect(addr).await;
    let cases = [
        (json!({}), 4030),
        (json!({"instructions": "  ", "input": ""}), 4030),
        (json!({"template": "  "}), 4030),
        (json!({"template": "limerick", "input": "x"}), 4031),
    ];
    for (n, (params, code)) in cases.iter().enumerate() {
        let reply = call(&mut socket, 10 + n as u64, params.clone()).await;
        assert_eq!(reply["error"]["code"], *code, "{params}: {reply}");
    }
    assert!(requests.lock().unwrap().is_empty());

    let (addr, _, _home) = start(false).await;
    let mut socket = connect(addr).await;
    let reply = call(&mut socket, 2, json!({"input": "hello"})).await;
    assert_eq!(reply["error"]["code"], 5030, "{reply}");
}
