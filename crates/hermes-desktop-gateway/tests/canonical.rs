//! The canonical wire, `hermes-gateway-v1`, as upstream Hermes Desktop v0.23.2 speaks it on a
//! native dial (apps/desktop/src/api/canonical-protocol.ts `openGatewaySocket`): the one-use
//! ticket rides as the `hermes-gateway-ticket.<ticket>` subprotocol beside `hermes-gateway-v1`,
//! and the session verbs answer as upstream's session authority does (frames captured from
//! upstream's own gateway running the `local-gateway-attach` spec).
#![cfg(unix)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use hermes_desktop_gateway::local::{self, LocalRuntime};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const REPLY: [&str; 3] = ["A1 ", "served by ", "the gateway"];

/// A scripted OpenAI-compatible model that streams [`REPLY`].
async fn complete(Json(_body): Json<Value>) -> axum::response::Response {
    let mut sse = String::new();
    for piece in REPLY {
        let chunk = json!({"id": "c", "object": "chat.completion.chunk", "model": "mock-model",
            "choices": [{"index": 0, "delta": {"content": piece}, "finish_reason": null}]});
        sse.push_str(&format!("data: {chunk}\n\n"));
    }
    let last = json!({"id": "c", "object": "chat.completion.chunk", "model": "mock-model",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
    sse.push_str(&format!("data: {last}\n\ndata: [DONE]\n\n"));
    ([("content-type", "text/event-stream")], sse).into_response()
}

/// A private home under /tmp (short enough for the control socket) whose config names the
/// scripted model, and the local runtime serving it.
async fn start() -> (tempfile::TempDir, PathBuf, LocalRuntime) {
    let model = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model.local_addr().unwrap();
    let app = Router::new().route("/v1/chat/completions", post(complete));
    tokio::spawn(async move { axum::serve(model, app).await });

    let dir = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        home.join("config.yaml"),
        format!(
            "model:\n  default: mock-model\n  provider: mock\nproviders:\n  mock:\n    \
             api: http://{model_addr}/v1\n    name: Mock\n    api_mode: chat_completions\n    \
             key_env: DESKTOP_CANONICAL_TEST_KEY\n    models:\n      mock-model: {{}}\n"
        ),
    )
    .unwrap();
    let config = hermes_config::load_config(home.to_str()).unwrap();
    let runtime = local::start(&home, hermes_desktop_gateway::Chat::new(config)).unwrap();
    (dir, home, runtime)
}

/// An `interactive` ticket from the control socket, as the desktop mints one. It blocks, so the
/// tests run on a multi-threaded runtime.
fn mint(home: &Path, runtime: &LocalRuntime) -> String {
    let mut stream = UnixStream::connect(home.join("gateway.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = json!({"protocol": 1, "id": 1, "verb": "session-ticket", "params": {
        "profile_id": home.to_str().unwrap(), "instance_id": runtime.instance_id(),
        "purpose": "interactive"}});
    stream.write_all(format!("{request}\n").as_bytes()).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap();
    reply["result"]["ticket"].as_str().unwrap().to_string()
}

/// The desktop's native dial with `protocols` as its `Sec-WebSocket-Protocol`.
fn dial(runtime: &LocalRuntime, protocols: &str) -> tungstenite::handshake::client::Request {
    let url = format!(
        "{}/api/ws?native_dial=n1",
        runtime.api_origin().replacen("http", "ws", 1)
    );
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", protocols.parse().unwrap());
    request
}

fn offered(ticket: &str) -> String {
    format!("hermes-gateway-v1, hermes-gateway-ticket.{ticket}")
}

async fn refused_status(request: tungstenite::handshake::client::Request) -> u16 {
    match tokio_tungstenite::connect_async(request).await {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(other) => panic!("expected an HTTP refusal, got {other}"),
        Ok(_) => panic!("the upgrade was accepted"),
    }
}

/// A desktop connection: replies are matched by id; events that arrive meanwhile are kept.
struct Client {
    socket: Socket,
    events: VecDeque<Value>,
    next_id: u64,
}

impl Client {
    async fn next(&mut self) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), self.socket.next())
                .await
                .expect("a frame within 10 s")
                .expect("the socket is open")
                .unwrap();
            if let Message::Text(text) = frame {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
        loop {
            let frame = self.next().await;
            if frame["id"] == id {
                return frame;
            }
            self.events.push_back(frame);
        }
    }

    /// Events until the one of type `last`, inclusive, in arrival order.
    async fn events_until(&mut self, last: &str) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let event = match self.events.pop_front() {
                Some(event) => event,
                None => self.next().await,
            };
            let done = event["params"]["type"] == last;
            seen.push(event);
            if done {
                return seen;
            }
        }
    }
}

/// A canonical connection, after its `gateway.ready`.
async fn connect(home: &Path, runtime: &LocalRuntime) -> (Client, Value) {
    let ticket = mint(home, runtime);
    let (socket, response) = tokio_tungstenite::connect_async(dial(runtime, &offered(&ticket)))
        .await
        .unwrap();
    let selected = response.headers().get("sec-websocket-protocol").unwrap();
    assert_eq!(selected, "hermes-gateway-v1");
    let mut client = Client {
        socket,
        events: VecDeque::new(),
        next_id: 0,
    };
    let ready = client.next().await;
    assert_eq!(ready["params"]["type"], "gateway.ready", "{ready}");
    (client, ready)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ticket_offered_as_the_gateway_subprotocol_opens_the_canonical_wire_once() {
    let (_dir, home, runtime) = start().await;
    let ticket = mint(&home, &runtime);
    let (mut socket, response) = tokio_tungstenite::connect_async(dial(&runtime, &offered(&ticket)))
        .await
        .unwrap();
    // Only the public protocol is selected; the ticket-bearing one is never echoed.
    let selected: Vec<_> = response
        .headers()
        .get_all("sec-websocket-protocol")
        .iter()
        .collect();
    assert_eq!(selected, ["hermes-gateway-v1"]);
    let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
        panic!("no text frame")
    };
    let ready: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(ready["params"]["type"], "gateway.ready");
    assert_eq!(ready["params"]["payload"]["session_authority"], true);
    // Used once.
    assert_eq!(refused_status(dial(&runtime, &offered(&ticket))).await, 401);

    // The `?ticket=` dial keeps the legacy wire.
    let ticket = mint(&home, &runtime);
    let url = format!(
        "{}/api/ws?ticket={ticket}",
        runtime.api_origin().replacen("http", "ws", 1)
    );
    let (mut socket, response) = tokio_tungstenite::connect_async(url).await.unwrap();
    assert!(response.headers().get("sec-websocket-protocol").is_none());
    let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
        panic!("no text frame")
    };
    let ready: Value = serde_json::from_str(&text).unwrap();
    assert!(ready["params"]["payload"].get("session_authority").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ambiguous_ticket_protocol_set_is_refused_without_spending_the_ticket() {
    let (_dir, home, runtime) = start().await;
    let ticket = mint(&home, &runtime);
    let other = mint(&home, &runtime);
    for protocols in [
        format!("hermes-gateway-ticket.{ticket}"),
        format!("hermes-gateway-v1, hermes-gateway-ticket.{ticket}, hermes-gateway-ticket.{other}"),
        "hermes-gateway-v1, hermes-gateway-ticket.".to_string(),
        "hermes-gateway-v1".to_string(),
    ] {
        assert_eq!(
            refused_status(dial(&runtime, &protocols)).await,
            401,
            "{protocols}"
        );
    }
    // As upstream, a set refused before the ticket is read leaves the ticket unspent.
    assert!(
        tokio_tungstenite::connect_async(dial(&runtime, &offered(&ticket)))
            .await
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_canonical_turn_streams_with_its_admission_and_the_session_resumes() {
    let (_dir, home, runtime) = start().await;
    let (mut client, _) = connect(&home, &runtime).await;

    // The desktop's create: `source`, `request_id`, its speed pick and the profile.
    let created = client
        .request(
            "session.create",
            json!({"profile": "default", "request_id": "r-1", "service_tier": "normal",
                   "source": "gui"}),
        )
        .await;
    let snapshot = &created["result"];
    let session = snapshot["session_id"].as_str().expect("a session id");
    assert_eq!(snapshot["stored_session_id"], session);
    assert_eq!(snapshot["messages"], json!([]));
    assert_eq!(snapshot["message_count"], 0);
    assert_eq!(snapshot["running"], false);
    assert_eq!(snapshot["execution_generation"], 0);
    assert_eq!(snapshot["revision"], 0);
    assert_eq!(snapshot["last_sequence"], 0);
    assert_eq!(snapshot["pending"], json!([]));
    assert_eq!(snapshot["prompts"], json!([]));
    assert_eq!(snapshot["info"]["desktop_protocol"], "hermes-gateway-v1");
    let replay_epoch = snapshot["replay_epoch"].as_str().unwrap().to_string();

    // The desktop's prompt carries its submission id.
    let submitted = client
        .request(
            "prompt.submit",
            json!({"session_id": session, "text": "U1 hello", "submission_id": "s-1",
                   "clarify_batch": true}),
        )
        .await;
    let receipt = &submitted["result"];
    let admission = receipt["admission_id"].as_str().expect("an admission id");
    assert_eq!(receipt["ref"]["session_id"], session);
    assert_eq!(receipt["ref"]["profile_id"], home.to_str().unwrap());
    assert_eq!(receipt["execution_generation"], 1);

    let events = client.events_until("message.complete").await;
    let types: Vec<&str> = events
        .iter()
        .map(|e| e["params"]["type"].as_str().unwrap())
        .collect();
    assert_eq!(types[..2], ["message.start", "session.info"]);
    for event in &events {
        let params = &event["params"];
        assert_eq!(params["session_id"], session, "{event}");
        assert_eq!(params["admission_id"], admission, "{event}");
        assert_eq!(params["execution_generation"], 1, "{event}");
        assert_eq!(params["replay_epoch"], replay_epoch.as_str(), "{event}");
    }
    let running = &events[1]["params"]["payload"];
    assert_eq!(running["running"], true);
    assert_eq!(running["pending"][0]["input_id"], "s-1");
    assert_eq!(running["pending"][0]["status"], "started");
    let done = &events.last().unwrap()["params"]["payload"];
    let reply = REPLY.concat();
    assert_eq!(done["text"], reply.as_str());
    assert_eq!(done["content"], reply.as_str());
    assert_eq!(done["admission_id"], admission);
    assert_eq!(done["status"], "complete");
    let idle = client.events_until("session.info").await;
    assert_eq!(idle[0]["params"]["payload"]["running"], false);
    assert_eq!(idle[0]["params"]["payload"]["pending"], json!([]));

    // A relaunched desktop resumes the session on a new connection, without its rows ...
    let (mut relaunched, _) = connect(&home, &runtime).await;
    let resumed = relaunched
        .request(
            "session.resume",
            json!({"session_id": session, "cols": 96, "source": "desktop",
                   "defer_history": true, "omit_messages": true, "profile": "default"}),
        )
        .await;
    let snapshot = &resumed["result"];
    assert_eq!(snapshot["session_id"], session, "{resumed}");
    assert_eq!(snapshot["messages"], json!([]));
    assert_eq!(snapshot["messages_omitted"], true);
    assert_eq!(snapshot["message_count"], 2);
    assert_eq!(snapshot["execution_generation"], 1);
    assert_eq!(snapshot["running"], false);
    // ... or with them.
    let resumed = relaunched
        .request("session.resume", json!({"session_id": session}))
        .await;
    let rows = resumed["result"]["messages"].as_array().unwrap();
    let rows: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r["role"].as_str().unwrap(), r["content"].as_str().unwrap()))
        .collect();
    assert_eq!(rows, [("user", "U1 hello"), ("assistant", reply.as_str())]);
    // A second turn there is generation 2.
    let submitted = relaunched
        .request(
            "prompt.submit",
            json!({"session_id": session, "text": "U2 again", "submission_id": "s-2"}),
        )
        .await;
    assert_eq!(submitted["result"]["execution_generation"], 2, "{submitted}");
    relaunched.events_until("message.complete").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn canonical_requests_refuse_another_profile_unknown_sessions_and_launch_options() {
    let (_dir, home, runtime) = start().await;
    let (mut client, _) = connect(&home, &runtime).await;
    let refused = client
        .request(
            "session.create",
            json!({"profile": "work", "request_id": "r-1", "source": "gui"}),
        )
        .await;
    assert_eq!(refused["error"]["code"], 4001, "{refused}");
    // A launch option this gateway would not honour is refused, not dropped.
    let refused = client
        .request(
            "session.create",
            json!({"request_id": "r-2", "source": "gui", "model": "other-model"}),
        )
        .await;
    assert_eq!(refused["error"]["code"], 4000, "{refused}");
    let refused = client
        .request("session.resume", json!({"session_id": "no-such-session"}))
        .await;
    assert_eq!(refused["error"]["code"], 4001, "{refused}");
    // Methods the canonical wire does not serve get JSON-RPC 'method not found'.
    let unknown = client.request("profiles.list", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");
}
