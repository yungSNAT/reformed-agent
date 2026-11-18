//! Chat as Hermes Desktop drives it: `session.create`, `prompt.submit`, the streamed reply as
//! `message.*` events, and the saved transcript at `GET /api/sessions/{id}/messages`. The
//! model is a scripted OpenAI-compatible endpoint configured the way upstream's desktop tests
//! write `config.yaml`.

use std::collections::VecDeque;
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

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A scripted model: every completion streams `pieces`, after `delay`, and fails with 400 when
/// the last message contains `FAIL`; each request body is kept.
#[derive(Clone)]
struct Model {
    pieces: Vec<&'static str>,
    delay: Duration,
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn complete(State(model): State<Model>, Json(body): Json<Value>) -> Response {
    let stream = body["stream"] == true;
    let fail = body["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .is_some_and(|text| text.contains("FAIL"));
    model.requests.lock().unwrap().push(body);
    tokio::time::sleep(model.delay).await;
    if fail {
        let error =
            json!({"error": {"message": "scripted failure", "type": "invalid_request_error"}});
        return (axum::http::StatusCode::BAD_REQUEST, Json(error)).into_response();
    }
    if !stream {
        let text: String = model.pieces.concat();
        return Json(json!({
            "id": "c", "object": "chat.completion", "model": "mock-model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": text},
                         "finish_reason": "stop"}],
        }))
        .into_response();
    }
    let mut sse = String::new();
    for piece in &model.pieces {
        let chunk = json!({"id": "c", "object": "chat.completion.chunk", "model": "mock-model",
            "choices": [{"index": 0, "delta": {"content": piece}, "finish_reason": null}]});
        sse.push_str(&format!("data: {chunk}\n\n"));
    }
    let last = json!({"id": "c", "object": "chat.completion.chunk", "model": "mock-model",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
    sse.push_str(&format!("data: {last}\n\ndata: [DONE]\n\n"));
    ([("content-type", "text/event-stream")], sse).into_response()
}

struct Started {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Value>>>,
    home: tempfile::TempDir,
}

/// Starts the scripted model and a gateway whose Hermes home names it as upstream's tests do
/// (`providers.<name>.api` and `key_env`).
async fn start(pieces: Vec<&'static str>, delay: Duration) -> Started {
    start_with(pieces, delay, None, "mock-model").await
}

/// As [`start`], with the gateway's socket write timeout when given and `model_name` as the
/// configured model.
async fn start_with(
    pieces: Vec<&'static str>,
    delay: Duration,
    write_timeout: Option<Duration>,
    model_name: &str,
) -> Started {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = Model {
        pieces,
        delay,
        requests: Arc::clone(&requests),
    };
    let model_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model_listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(complete))
        .with_state(model);
    tokio::spawn(async move { axum::serve(model_listener, app).await });

    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        format!(
            "model:\n  default: {model_name}\n  provider: mock\nproviders:\n  mock:\n    \
             api: http://{model_addr}/v1\n    name: Mock\n    api_mode: chat_completions\n    \
             key_env: DESKTOP_CHAT_TEST_KEY\n    models:\n      {model_name}: {{}}\n"
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
    Started {
        addr,
        requests,
        home,
    }
}

/// A desktop connection: replies are matched by id, and events that arrive meanwhile are
/// kept for [`Client::turn_events`].
struct Client {
    socket: Socket,
    events: VecDeque<Value>,
}

async fn connect(addr: SocketAddr) -> Client {
    let request = format!("ws://{addr}/api/ws?token={TOKEN}")
        .into_client_request()
        .unwrap();
    let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let mut client = Client {
        socket,
        events: VecDeque::new(),
    };
    assert_eq!(client.next_json().await["params"]["type"], "gateway.ready");
    client
}

impl Client {
    async fn next_json(&mut self) -> Value {
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

    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        // Null params are sent as absent ones, which a request may omit.
        let mut request = json!({"jsonrpc": "2.0", "id": id, "method": method});
        if !params.is_null() {
            request["params"] = params;
        }
        self.send(id, request).await
    }

    /// Sends `request` as it is and returns the reply with `id`.
    async fn send(&mut self, id: u64, request: Value) -> Value {
        self.socket
            .send(Message::text(request.to_string()))
            .await
            .unwrap();
        loop {
            let frame = self.next_json().await;
            if frame["method"] == "event" {
                self.events.push_back(frame);
                continue;
            }
            assert_eq!(frame["id"], id, "{frame}");
            return frame;
        }
    }

    async fn create(&mut self) -> String {
        let reply = self
            .call(
                1,
                "session.create",
                json!({"source": "desktop", "cols": 96}),
            )
            .await;
        let result = &reply["result"];
        let id = result["session_id"]
            .as_str()
            .expect("a session id")
            .to_string();
        assert_eq!(result["stored_session_id"], id);
        assert_eq!(result["messages"], json!([]));
        id
    }

    async fn submit(&mut self, id: u64, session: &str, text: &str) -> Value {
        self.call(
            id,
            "prompt.submit",
            json!({"session_id": session, "text": text}),
        )
        .await
    }

    /// One turn's events up to `message.complete`: (type, payload, seq) each.
    async fn turn_events(&mut self, session: &str) -> Vec<(String, Value, u64)> {
        let mut events = Vec::new();
        loop {
            let frame = match self.events.pop_front() {
                Some(frame) => frame,
                None => self.next_json().await,
            };
            assert_eq!(frame["method"], "event", "{frame}");
            let params = &frame["params"];
            assert_eq!(params["session_id"], session, "{frame}");
            let kind = params["type"].as_str().unwrap().to_string();
            let seq = params["seq"].as_u64().expect("every event has a seq");
            let done = kind == "message.complete";
            events.push((kind, params["payload"].clone(), seq));
            if done {
                return events;
            }
        }
    }
}

async fn history(addr: SocketAddr, session: &str) -> Vec<(String, String)> {
    let response = reqwest::Client::new()
        .get(format!(
            "http://{addr}/api/sessions/{session}/messages?order=oldest&limit=500"
        ))
        .header("x-hermes-session-token", TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["role"].as_str().unwrap().to_string(),
                m["content"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_turn_streams_its_reply_and_is_saved() {
    // A character outside the BMP is 2 UTF-16 units, so the offsets differ from char counts.
    let started = start(vec!["Hello ", "wörld 😀", "!"], Duration::ZERO).await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;

    // Like upstream's `hermes serve`, this gateway refuses a submission id before running
    // anything; the desktop then sends the prompt again without one.
    let params = json!({"session_id": session, "text": "U1-x7 hi", "submission_id": "s-1"});
    let reply = client.call(2, "prompt.submit", params).await;
    assert_eq!(reply["error"]["code"], 4000, "{reply}");
    assert!(reply["error"]["message"]
        .as_str()
        .is_some_and(|m| m.contains("submission_id")));
    assert!(started.requests.lock().unwrap().is_empty());
    let reply = client.submit(3, &session, "U1-x7 hi").await;
    assert_eq!(reply["result"]["status"], "streaming", "{reply}");
    // The reply was written before any of the turn's events.
    assert!(client.events.is_empty(), "{:?}", client.events);

    let events = client.turn_events(&session).await;
    let kinds: Vec<&str> = events.iter().map(|(k, _, _)| k.as_str()).collect();
    assert_eq!(kinds.first(), Some(&"message.start"));
    assert_eq!(kinds.last(), Some(&"message.complete"));
    let seqs: Vec<u64> = events.iter().map(|(_, _, s)| *s).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    let turn_id = events[0].1["turn_id"].as_str().unwrap().to_string();

    // Each delta carries the turn, segment 0 and its UTF-16 offset in the reply.
    let mut streamed = String::new();
    for (_, payload, _) in events.iter().filter(|(k, _, _)| k == "message.delta") {
        assert_eq!(payload["turn_id"], turn_id.as_str());
        assert_eq!(payload["segment"], 0);
        assert_eq!(payload["offset"], streamed.encode_utf16().count());
        streamed.push_str(payload["text"].as_str().unwrap());
    }
    assert_eq!(streamed, "Hello wörld 😀!");
    let complete = &events.last().unwrap().1;
    assert_eq!(complete["status"], "complete");
    assert_eq!(complete["text"], "Hello wörld 😀!");

    // The model was asked once, saw the user's words and was offered no tools. The reply
    // lacks the prompt's `U1-x7`, which the agent re-asks for only when tools are offered.
    let requests = started.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let first = &requests[0];
    let messages = first["messages"].as_array().unwrap();
    assert_eq!(messages.last().unwrap()["content"], "U1-x7 hi");
    assert!(
        first["tools"].as_array().is_none_or(Vec::is_empty),
        "{first}"
    );

    assert_eq!(
        history(started.addr, &session).await,
        vec![
            ("user".to_string(), "U1-x7 hi".to_string()),
            ("assistant".to_string(), "Hello wörld 😀!".to_string()),
        ]
    );
}

#[tokio::test]
async fn a_second_turn_carries_the_first() {
    let started = start(vec!["A"], Duration::ZERO).await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    for (n, text) in [(2, "first"), (3, "second")] {
        let reply = client.submit(n, &session, text).await;
        assert_eq!(reply["result"]["status"], "streaming", "{reply}");
        client.turn_events(&session).await;
    }
    let second = started.requests.lock().unwrap().last().unwrap().clone();
    let said: Vec<(&str, &str)> = second["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] != "system")
        .map(|m| {
            (
                m["role"].as_str().unwrap(),
                m["content"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        said,
        vec![("user", "first"), ("assistant", "A"), ("user", "second")]
    );
    assert_eq!(history(started.addr, &session).await.len(), 4);
}

#[tokio::test]
async fn prompt_submit_refuses_bad_requests() {
    let started = start(vec!["slow"], Duration::from_millis(1500)).await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let cases = [
        (json!({"text": "hi"}), 4000),
        (json!({"session_id": 7, "text": "hi"}), 4000),
        (json!({"session_id": "no-such-session", "text": "hi"}), 4001),
        (json!({"session_id": session, "text": ["hi"]}), 4000),
        (json!({"session_id": session}), 4012),
        (json!({"session_id": session, "text": "  "}), 4012),
        (json!(["hi"]), 4000),
    ];
    for (n, (params, code)) in cases.into_iter().enumerate() {
        let reply = client
            .call(10 + n as u64, "prompt.submit", params.clone())
            .await;
        assert_eq!(reply["error"]["code"], code, "{params}: {reply}");
    }

    // Params that are not an object are refused at the wire, before any method runs.
    for (n, params) in [Value::Null, json!(5)].into_iter().enumerate() {
        let id = 20 + n as u64;
        let request =
            json!({"jsonrpc": "2.0", "id": id, "method": "prompt.submit", "params": params});
        let reply = client.send(id, request).await;
        assert_eq!(reply["error"]["code"], -32600, "{reply}");
    }
    assert!(started.requests.lock().unwrap().is_empty());

    // One turn at a time per session: a second submit while the first runs is refused.
    let reply = client.submit(30, &session, "one").await;
    assert_eq!(reply["result"]["status"], "streaming");
    let reply = client.submit(31, &session, "two").await;
    assert_eq!(reply["error"]["code"], 4009, "{reply}");
    client.turn_events(&session).await;
    let reply = client.submit(32, &session, "three").await;
    assert_eq!(reply["result"]["status"], "streaming", "{reply}");
}

/// `file.attach` (staging is tested in the crate, as it writes under the working folder):
/// each refusal comes before anything is written, and a host path is never read.
#[tokio::test]
async fn file_attach_refuses_bad_requests() {
    let started = start(vec!["x"], Duration::ZERO).await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let data_url = "data:text/plain;base64,aGk=";
    let cases = [
        (json!({"data_url": data_url}), 4000),
        (
            json!({"session_id": "no-such-session", "data_url": data_url}),
            4001,
        ),
        (json!({"session_id": session, "path": "Cargo.toml"}), 4015),
        (json!({"session_id": session, "data_url": 5}), 4000),
        (
            json!({"session_id": session, "data_url": data_url, "name": 5}),
            4000,
        ),
        // Not base64: refused before the attachments folder is made.
        (
            json!({"session_id": session, "data_url": "data:text/plain,hi"}),
            5028,
        ),
    ];
    for (n, (params, code)) in cases.into_iter().enumerate() {
        let reply = client
            .call(10 + n as u64, "file.attach", params.clone())
            .await;
        assert_eq!(reply["error"]["code"], code, "{params}: {reply}");
    }
}

#[tokio::test]
async fn the_runtime_check_reports_the_configured_model() {
    let started = start(vec!["x"], Duration::ZERO).await;
    let mut client = connect(started.addr).await;
    let reply = client.call(1, "setup.runtime_check", json!({})).await;
    assert_eq!(reply["result"]["ok"], true, "{reply}");
    assert_eq!(reply["result"]["model"], "mock-model");
}

#[tokio::test]
async fn without_a_model_the_runtime_check_fails_and_a_turn_reports_an_error() {
    let home = tempfile::tempdir().unwrap();
    let config = hermes_config::GatewayConfig {
        home_dir: home.path().to_str().map(str::to_string),
        ..Default::default()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let chat = hermes_desktop_gateway::Chat::new(config);
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        chat,
    ));
    let mut client = connect(addr).await;

    let reply = client.call(1, "setup.runtime_check", Value::Null).await;
    assert_eq!(reply["result"]["ok"], false, "{reply}");

    let session = client.create().await;
    let reply = client.submit(2, &session, "hi").await;
    assert_eq!(reply["result"]["status"], "streaming");
    let events = client.turn_events(&session).await;
    let complete = &events.last().unwrap().1;
    assert_eq!(complete["status"], "error", "{complete}");
    assert!(complete["error"].as_str().is_some_and(|e| !e.is_empty()));
}

/// The transcript route needs the token in `X-Hermes-Session-Token` and the WebSocket's
/// Origin rule; an id that cannot name a session is not looked up.
#[tokio::test]
async fn the_messages_route_needs_the_token() {
    let started = start(vec!["x"], Duration::ZERO).await;
    let url = |id: &str| format!("http://{}/api/sessions/{id}/messages", started.addr);
    let client = reqwest::Client::new();
    let status = |request: reqwest::RequestBuilder| async move {
        request.send().await.unwrap().status().as_u16()
    };

    assert_eq!(status(client.get(url("abc"))).await, 401);
    assert_eq!(
        status(
            client
                .get(url("abc"))
                .header("x-hermes-session-token", "wrong")
        )
        .await,
        401
    );
    // The query token is the WebSocket's; it does not open HTTP routes.
    assert_eq!(
        status(client.get(format!("{}?token={TOKEN}", url("abc")))).await,
        401
    );
    assert_eq!(
        status(
            client
                .get(url("abc"))
                .header("x-hermes-session-token", TOKEN)
                .header("origin", "https://attacker.example")
        )
        .await,
        403
    );
    for id in ["a%20b", "%2E%2E", "a%2Fb"] {
        assert_eq!(
            status(client.get(url(id)).header("x-hermes-session-token", TOKEN)).await,
            404,
            "{id}"
        );
    }
    assert_eq!(history(started.addr, "no-such-session").await, vec![]);
}

/// `order=latest` keeps the newest `limit` messages, still oldest first.
#[tokio::test]
async fn history_order_and_limit() {
    let started = start(vec!["A"], Duration::ZERO).await;
    let addr = started.addr;
    let mut client = connect(addr).await;
    let session = client.create().await;
    for (n, text) in [(2, "first"), (3, "second")] {
        client.submit(n, &session, text).await;
        client.turn_events(&session).await;
    }
    let page = |query: &'static str| {
        let url = format!("http://{addr}/api/sessions/{session}/messages?{query}");
        async move {
            let body: Value = reqwest::Client::new()
                .get(url)
                .header("x-hermes-session-token", TOKEN)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["content"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(page("order=latest&limit=2").await, ["second", "A"]);
    assert_eq!(page("order=oldest&limit=1").await, ["first"]);
    assert_eq!(page("limit=nonsense").await.len(), 4);
}

/// At most 8 turns run at once across the gateway; a ninth is refused until one ends.
#[tokio::test]
async fn turns_beyond_the_cap_are_refused() {
    let started = start(vec!["slow"], Duration::from_millis(1500)).await;
    let mut client = connect(started.addr).await;
    let mut sessions = Vec::new();
    for _ in 0..9 {
        sessions.push(client.create().await);
    }
    for (n, session) in sessions[..8].iter().enumerate() {
        let reply = client.submit(10 + n as u64, session, "hi").await;
        assert_eq!(reply["result"]["status"], "streaming", "{reply}");
    }
    let reply = client.submit(30, &sessions[8], "hi").await;
    assert_eq!(reply["error"]["code"], 4090, "{reply}");
    // A slot frees once one of the 8 turns ends (each takes about 1.5 s).
    let mut reply = client.submit(31, &sessions[8], "hi").await;
    for id in 32..60 {
        if reply["result"]["status"] == "streaming" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        reply = client.submit(id, &sessions[8], "hi").await;
    }
    assert_eq!(reply["result"]["status"], "streaming", "{reply}");
}

/// A failed turn reports the error and keeps the user's prompt, which the next turn sends.
#[tokio::test]
async fn a_failed_turn_keeps_the_prompt() {
    let started = start(vec!["B"], Duration::ZERO).await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;

    client.submit(2, &session, "FAIL please").await;
    let events = client.turn_events(&session).await;
    let complete = &events.last().unwrap().1;
    assert_eq!(complete["status"], "error", "{complete}");
    assert_eq!(
        history(started.addr, &session).await,
        vec![("user".to_string(), "FAIL please".to_string())]
    );

    client.submit(3, &session, "again").await;
    let events = client.turn_events(&session).await;
    assert_eq!(events.last().unwrap().1["status"], "complete");
    let last = started.requests.lock().unwrap().last().unwrap().clone();
    let said: Vec<&str> = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert_eq!(said, ["FAIL please", "again"]);
}

/// A failure after a saved turn rewrites the session's rows as the stored ones, the system
/// prompt included, each once, then the failed prompt.
#[tokio::test]
async fn a_failed_turn_keeps_the_saved_rows_once() {
    let started = start(vec!["B"], Duration::ZERO).await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    client.submit(2, &session, "first").await;
    client.turn_events(&session).await;
    let store = hermes_agent::SessionPersistence::new(started.home.path());
    let roles = |rows: Vec<hermes_core::Message>| {
        rows.into_iter()
            .map(|m| (format!("{:?}", m.role), m.content.unwrap_or_default()))
            .collect::<Vec<_>>()
    };
    let saved = roles(store.load_session(&session).unwrap());
    assert_eq!(
        saved.first().map(|r| r.0.as_str()),
        Some("System"),
        "{saved:?}"
    );

    client.submit(3, &session, "FAIL now").await;
    let events = client.turn_events(&session).await;
    assert_eq!(events.last().unwrap().1["status"], "error");
    let mut expected = saved.clone();
    expected.push(("User".to_string(), "FAIL now".to_string()));
    assert_eq!(roles(store.load_session(&session).unwrap()), expected);
}

type PlainSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

async fn next_text(ws: &mut PlainSocket) -> Value {
    loop {
        if let Message::Text(text) = ws.next().await.unwrap().unwrap() {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

/// A connection that sends a prompt and then reads nothing, with a small receive buffer so
/// its turn's frames stop at the socket.
async fn stalled_turn(addr: SocketAddr) -> (PlainSocket, String) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let stream = socket.connect(addr).await.unwrap();
    let request = format!("ws://{addr}/api/ws?token={TOKEN}")
        .into_client_request()
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap();
    assert_eq!(next_text(&mut ws).await["params"]["type"], "gateway.ready");
    let create = json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}});
    ws.send(Message::text(create.to_string())).await.unwrap();
    let session = next_text(&mut ws).await["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let submit = json!({"jsonrpc": "2.0", "id": 2, "method": "prompt.submit",
        "params": {"session_id": session, "text": "hi"}});
    ws.send(Message::text(submit.to_string())).await.unwrap();
    (ws, session)
}

/// A finished turn keeps its slot until its events are written, so readers that stop reading
/// cannot pile up finished replies past the turn cap; a write that waits past the timeout
/// closes the connection and frees the slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalled_reader_holds_its_turn_slot_until_it_is_closed() {
    // 4 MiB streamed and the same again in message.complete: more than a loopback socket's
    // buffers hold, so each stalled connection's writes stop. The reply streams as 4 frames
    // of 1 MiB because Windows takes a whole send while less than its send buffer (64 KiB) is
    // queued, so 2 large frames would both be taken and the slot freed.
    let piece: &'static str = Box::leak("x".repeat(1 << 20).into_boxed_str());
    // The timeout runs from when each frame is queued, so all 8 stalled turns and the check
    // below must fit inside it: 10 seconds was too short on a loaded runner.
    let timeout = Duration::from_secs(30);
    let started = start_with(vec![piece; 4], Duration::ZERO, Some(timeout), "mock-model").await;
    let mut stalled = Vec::new();
    for _ in 0..8 {
        stalled.push(stalled_turn(started.addr).await);
    }
    // Each turn has finished once its reply is saved.
    for (_, session) in &stalled {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while history(started.addr, session).await.len() < 2 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "turn {session} did not finish"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let reply = client.submit(2, &session, "hi").await;
    assert_eq!(reply["error"]["code"], 4090, "{reply}");

    // Once the stalled writes time out, their connections close and the slots free.
    let deadline = tokio::time::Instant::now() + timeout * 4;
    let mut id = 3;
    let mut reply = client.submit(id, &session, "hi").await;
    while reply["result"]["status"] != "streaming" {
        assert!(tokio::time::Instant::now() < deadline, "{reply}");
        tokio::time::sleep(Duration::from_millis(250)).await;
        id += 1;
        reply = client.submit(id, &session, "hi").await;
    }
    drop(stalled);
}

/// A reader that pauses 4 seconds, takes one frame and pauses 4 more never makes a single
/// write wait the 6 second write timeout, yet the turn's last event, which carries its slot,
/// waits 8 seconds. A frame not written within the timeout of being queued closes the
/// connection, so that event is never delivered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_reader_is_closed_once_a_frame_has_waited_the_write_timeout() {
    // 8 MiB streamed as 8 frames of 1 MiB, and the same again in message.complete: more than
    // a loopback socket's buffers hold, so later frames wait for the reader.
    let piece: &'static str = Box::leak("x".repeat(1 << 20).into_boxed_str());
    let timeout = Duration::from_secs(6);
    let pause = Duration::from_secs(4);
    let started = start_with(vec![piece; 8], Duration::ZERO, Some(timeout), "mock-model").await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let reply = client.submit(2, &session, "hi").await;
    assert_eq!(reply["result"]["status"], "streaming", "{reply}");
    // Every frame of the turn is queued once its reply is saved.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while history(started.addr, &session).await.len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the turn did not finish"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    tokio::time::sleep(pause).await;
    while client.next_json().await["params"]["type"] != "message.delta" {}
    tokio::time::sleep(pause).await;
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(20), client.socket.next())
            .await
            .expect("no frame and no close within 20 seconds");
        match frame {
            Some(Ok(Message::Text(text))) => {
                let frame: Value = serde_json::from_str(&text).unwrap();
                assert_ne!(
                    frame["params"]["type"], "message.complete",
                    "the turn's last event reached a reader that paused twice for {pause:?}"
                );
            }
            Some(Ok(Message::Close(_)) | Err(_)) | None => break,
            Some(Ok(_)) => {}
        }
    }
}

/// A frame already past its deadline closes the connection even when the socket could take it
/// at once: with no time allowed, not even `gateway.ready` is written.
#[tokio::test]
async fn a_frame_past_its_deadline_is_not_written_even_to_an_idle_socket() {
    let started = start_with(vec!["hi"], Duration::ZERO, Some(Duration::ZERO), "mock-model").await;
    let request = format!("ws://{}/api/ws?token={TOKEN}", started.addr)
        .into_client_request()
        .unwrap();
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(20), socket.next())
        .await
        .expect("no frame and no close within 20 seconds");
    assert!(
        !matches!(frame, Some(Ok(Message::Text(_)))),
        "a frame past its deadline was written: {frame:?}"
    );
}

/// The deadline is per frame, not per turn: a turn that runs longer than the write timeout
/// still reaches a client that keeps reading.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_longer_than_the_write_timeout_reaches_a_reader_that_keeps_up() {
    let timeout = Duration::from_secs(1);
    let started = start_with(vec!["slow ", "reply"], timeout * 3, Some(timeout), "mock-model").await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let reply = client.submit(2, &session, "hi").await;
    assert_eq!(reply["result"]["status"], "streaming", "{reply}");
    let events = client.turn_events(&session).await;
    let (kind, payload, _) = events.last().unwrap();
    assert_eq!(kind, "message.complete");
    assert_eq!(payload["text"], "slow reply", "{payload}");
}

/// A turn reads its session while its first event waits to be written; that store work must
/// not hold the event, or a reader that keeps up is closed for a slow disk. Another connection
/// holds the sessions database for a second, so the turn's read waits in SQLite's busy handler.
#[tokio::test]
async fn a_slow_session_read_does_not_hold_the_turns_first_event() {
    let timeout = Duration::from_millis(200);
    let started = start_with(vec!["ok"], Duration::ZERO, Some(timeout), "mock-model").await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let store = hermes_agent::SessionPersistence::new(started.home.path());
    store.load_session(&session).unwrap();
    let db = store.db_path().to_path_buf();
    let (locked, wait_for_lock) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(1));
        conn.execute_batch("COMMIT").unwrap();
    });
    wait_for_lock.recv().unwrap();
    let probe = rusqlite::Connection::open(store.db_path()).unwrap();
    probe.busy_timeout(Duration::ZERO).unwrap();
    assert!(
        probe
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row
                .get::<_, i64>(0))
            .is_err(),
        "the database is not locked against readers"
    );

    let reply = client.submit(2, &session, "hi").await;
    assert_eq!(reply["result"]["status"], "streaming", "{reply}");
    let events = client.turn_events(&session).await;
    assert_eq!(events.first().unwrap().0, "message.start");
    let (kind, payload, _) = events.last().unwrap();
    assert_eq!(kind, "message.complete");
    assert_eq!(payload["text"], "ok", "{payload}");
    holder.join().unwrap();
}

/// A small payload that starts as a PNG does: the gateway checks only the signature.
fn png(tag: &str) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(tag.as_bytes());
    bytes
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The files under `<home>/images`.
fn staged_images(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    match std::fs::read_dir(home.join("images")) {
        Ok(entries) => entries.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

/// `image.attach_bytes` stages the bytes under the gateway's home, and the next prompt, only,
/// sends the model the image and the staged path, as upstream's remote-topology spec expects.
#[tokio::test]
async fn an_attached_image_goes_to_the_model_with_the_next_prompt() {
    // gpt-4o takes images, so the provider sends image parts as they are.
    let started = start_with(vec!["saw it"], Duration::ZERO, None, "gpt-4o").await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let image = png("client-only");

    let params = json!({"session_id": session, "content_base64": b64(&image),
                        "filename": "../../shot.png", "ext": "exe"});
    let reply = client.call(2, "image.attach_bytes", params).await;
    let result = &reply["result"];
    assert_eq!(result["attached"], true, "{reply}");
    assert_eq!(result["count"], 1);
    assert_eq!(result["bytes"], image.len());
    // The client's name and extension are not used: the bytes name the file.
    let staged = std::path::PathBuf::from(result["path"].as_str().unwrap());
    assert_eq!(staged_images(started.home.path()), vec![staged.clone()]);
    assert_eq!(staged.extension().unwrap(), "png");
    assert_eq!(std::fs::read(&staged).unwrap(), image);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(started.home.path().join("images"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    let reply = client.submit(3, &session, "U2 what is this").await;
    assert_eq!(reply["result"]["status"], "streaming", "{reply}");
    let complete = client.turn_events(&session).await.pop().unwrap().1;
    assert_eq!(complete["status"], "complete", "{complete}");

    let user = |body: &Value| body["messages"].as_array().unwrap().last().unwrap().clone();
    let first = user(&started.requests.lock().unwrap()[0]);
    let parts = first["content"].as_array().expect("text and image parts");
    let text = parts[0]["text"].as_str().unwrap();
    assert!(text.starts_with("U2 what is this"), "{text}");
    assert!(text.contains(&staged.display().to_string()), "{text}");
    let url = parts[1]["image_url"]["url"].as_str().unwrap();
    assert_eq!(url, format!("data:image/png;base64,{}", b64(&image)));
    assert_eq!(parts.len(), 2);

    // The transcript shows the prompt's text, not its parts.
    let shown = history(started.addr, &session).await;
    assert_eq!(shown[0].0, "user");
    assert!(shown[0].1.starts_with("U2 what is this"), "{shown:?}");
    assert!(!shown[0].1.contains("data:image"), "{shown:?}");

    // The image went with that prompt only.
    client.submit(4, &session, "U3 and now").await;
    client.turn_events(&session).await;
    let second = user(&started.requests.lock().unwrap()[1]);
    assert_eq!(second["content"], "U3 and now");
}

/// Each refusal comes before anything is written under the home.
#[tokio::test]
async fn image_attach_bytes_refuses_bad_requests() {
    let started = start(vec!["x"], Duration::ZERO).await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let image = b64(&png("x"));
    let cases = [
        (json!({"content_base64": image}), 4000),
        (
            json!({"session_id": "no-such-session", "content_base64": image}),
            4001,
        ),
        (json!({"session_id": session}), 4015),
        (json!({"session_id": session, "content_base64": "  "}), 4015),
        (json!({"session_id": session, "content_base64": 5}), 4000),
        (json!({"session_id": session, "content_base64": "not base64!"}), 4017),
        // Text, not an image, under the `data` alias.
        (json!({"session_id": session, "data": b64(b"hello")}), 4016),
    ];
    for (n, (params, code)) in cases.into_iter().enumerate() {
        let reply = client
            .call(10 + n as u64, "image.attach_bytes", params.clone())
            .await;
        assert_eq!(reply["error"]["code"], code, "{params}: {reply}");
    }
    assert!(staged_images(started.home.path()).is_empty());
}

/// An image whose request is over the 16 MiB message limit is still read, as a `file.attach`
/// request is, rather than closing the connection.
#[tokio::test]
async fn a_large_image_is_attached() {
    let started = start(vec!["x"], Duration::ZERO).await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    let mut image = png("large");
    image.resize(13 << 20, 7);
    let params = json!({"session_id": session, "content_base64": b64(&image)});
    assert!(params.to_string().len() > hermes_desktop_gateway::MAX_MESSAGE_BYTES);
    let reply = client.call(2, "image.attach_bytes", params).await;
    assert_eq!(reply["result"]["bytes"], image.len(), "{}", reply["error"]);
}
