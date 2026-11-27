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

/// A scripted OpenAI-compatible model that streams [`REPLY`], a second later when the prompt
/// holds `SLOW`.
async fn complete(Json(body): Json<Value>) -> axum::response::Response {
    if body.to_string().contains("SLOW") {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
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
    let (mut socket, response) =
        tokio_tungstenite::connect_async(dial(&runtime, &offered(&ticket)))
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
    assert!(ready["params"]["payload"]
        .get("session_authority")
        .is_none());
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
    assert_eq!(
        submitted["result"]["execution_generation"], 2,
        "{submitted}"
    );
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
    assert_eq!(refused["error"]["code"], 4001, "{refused}");
    let refused = client
        .request("session.resume", json!({"session_id": "no-such-session"}))
        .await;
    assert_eq!(refused["error"]["code"], 4001, "{refused}");
    // A request id longer than any the desktop makes is refused, not remembered.
    let refused = client
        .request("session.create", json!({"request_id": "r".repeat(129)}))
        .await;
    assert_eq!(refused["error"]["code"], 4001, "{refused}");
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(message.starts_with("invalid_params"), "{refused}");
    // A submission id longer than any the desktop makes is refused before it is queued.
    let made = client
        .request("session.create", json!({"request_id": "r-3"}))
        .await;
    let session = made["result"]["session_id"].as_str().unwrap().to_string();
    let long = "s".repeat(129);
    let refused = client
        .request(
            "prompt.submit",
            json!({"session_id": session, "text": "U1", "submission_id": long}),
        )
        .await;
    assert_eq!(refused["error"]["code"], 4001, "{refused}");
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(message.starts_with("invalid_params"), "{refused}");
    // Methods the canonical wire does not serve get JSON-RPC 'method not found', in
    // upstream's words.
    let unknown = client.request("no.such.method", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");
    assert_eq!(
        unknown["error"]["message"],
        "unknown method: no.such.method \u{2014} not available on this Hermes backend"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_request_leaves_the_connection_and_session_usable() {
    let (_dir, home, runtime) = start().await;
    let (mut client, _) = connect(&home, &runtime).await;
    let created = client
        .request(
            "session.create",
            json!({"request_id": "r-1", "source": "gui", "profile": "default"}),
        )
        .await;
    let session = created["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Refusals for another profile and an unknown session ...
    let refused = client
        .request(
            "prompt.submit",
            json!({"session_id": session, "text": "U0", "profile": "work"}),
        )
        .await;
    assert_eq!(refused["error"]["code"], 4001, "{refused}");
    let refused = client
        .request(
            "prompt.submit",
            json!({"session_id": "no-such-session", "text": "U0"}),
        )
        .await;
    assert_eq!(refused["error"]["code"], 4001, "{refused}");

    // ... then a slow turn, and a prompt sent whilst it runs is queued behind it.
    let first = client
        .request(
            "prompt.submit",
            json!({"session_id": session, "text": "U1 SLOW", "submission_id": "s-1"}),
        )
        .await;
    assert_eq!(first["result"]["execution_generation"], 1, "{first}");
    assert_eq!(first["result"]["sequence"], 1, "{first}");
    let queued = client
        .request(
            "prompt.submit",
            json!({"session_id": session, "text": "U2", "submission_id": "s-2"}),
        )
        .await;
    let receipt = &queued["result"];
    assert_eq!(receipt["status"], "queued", "{queued}");
    assert_eq!(receipt["sequence"], 2, "{queued}");
    assert_eq!(receipt["execution_generation"], Value::Null, "{queued}");
    // Whilst the first runs, a snapshot lists it as started and the second as queued.
    let snapshot = client
        .request(
            "session.resume",
            json!({"session_id": session, "omit_messages": true}),
        )
        .await;
    let rows: Vec<(Value, Value, Value)> = snapshot["result"]["pending"]
        .as_array()
        .unwrap_or_else(|| panic!("{snapshot}"))
        .iter()
        .map(|row| {
            (
                row["sequence"].clone(),
                row["status"].clone(),
                row["execution_generation"].clone(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (json!(1), json!("started"), json!(1)),
            (json!(2), json!("queued"), Value::Null)
        ],
        "{snapshot}"
    );
    let events = client.events_until("message.complete").await;
    assert!(events
        .iter()
        .all(|e| e["params"]["execution_generation"] == 1));
    // The first turn's end reports the session idle with the second still queued.
    let ended = client.events_until("session.info").await;
    let info = &ended.last().unwrap()["params"]["payload"];
    assert_eq!(info["running"], false, "{info}");
    let waiting = &info["pending"];
    assert_eq!(waiting.as_array().map(Vec::len), Some(1), "{info}");
    assert_eq!(
        waiting[0]["admission_id"], receipt["admission_id"],
        "{info}"
    );
    assert_eq!(waiting[0]["status"], "queued", "{info}");
    assert_eq!(waiting[0]["execution_generation"], Value::Null, "{info}");

    // The queued prompt then runs as generation 2 and streams in full.
    let events = client.events_until("message.complete").await;
    assert_eq!(events[0]["params"]["admission_id"], receipt["admission_id"]);
    let running = &events[1]["params"]["payload"]["pending"][0];
    assert_eq!(running["sequence"], 2, "{running}");
    assert_eq!(running["input_id"], "s-2", "{running}");
    let done = &events.last().unwrap()["params"];
    assert_eq!(done["execution_generation"], 2);
    assert_eq!(done["payload"]["text"], REPLY.concat().as_str());
    let resumed = client
        .request(
            "session.resume",
            json!({"session_id": session, "omit_messages": true}),
        )
        .await;
    assert_eq!(resumed["result"]["message_count"], 4, "{resumed}");
}

/// A create retried after a lost reply, on a new connection, gets the same session, and a turn
/// started on either connection streams on both in one order.
#[tokio::test(flavor = "multi_thread")]
async fn a_retried_create_gets_the_same_session_and_both_connections_stream_it() {
    let (_dir, home, runtime) = start().await;
    let (mut first, _) = connect(&home, &runtime).await;
    let create = json!({"request_id": "5b0e7a52-0f7c-4d1b-9c3e-2a6f4d8e1b90", "source": "gui"});
    let made = first.request("session.create", create.clone()).await;
    let session = made["result"]["session_id"].as_str().unwrap().to_string();
    let (mut second, _) = connect(&home, &runtime).await;
    let again = second.request("session.create", create).await;
    assert_eq!(again["result"]["session_id"], session.as_str(), "{again}");
    let other = second
        .request("session.create", json!({"request_id": "r-other"}))
        .await;
    assert_ne!(other["result"]["session_id"], session.as_str(), "{other}");
    // A request id shorter than 16 characters is not remembered, so it cannot be guessed.
    let short = json!({"request_id": "r-other"});
    let repeated = second.request("session.create", short).await;
    assert_ne!(
        repeated["result"]["session_id"], other["result"]["session_id"],
        "{repeated}"
    );

    first
        .request(
            "prompt.submit",
            json!({"session_id": session, "text": "U1 hello"}),
        )
        .await;
    let on_first = first.events_until("message.complete").await;
    let on_second = second.events_until("message.complete").await;
    let seqs = |events: &[Value]| -> Vec<u64> {
        events
            .iter()
            .map(|e| e["params"]["seq"].as_u64().unwrap())
            .collect()
    };
    assert_eq!(seqs(&on_first), seqs(&on_second));
    let done = &on_second.last().unwrap()["params"]["payload"];
    assert_eq!(done["text"], REPLY.concat().as_str());
}

/// A session resumed on another connection gets the turns started on the first.
#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_session_streams_turns_started_on_another_connection() {
    let (_dir, home, runtime) = start().await;
    let (mut owner, _) = connect(&home, &runtime).await;
    let made = owner
        .request("session.create", json!({"request_id": "r-1"}))
        .await;
    let session = made["result"]["session_id"].as_str().unwrap().to_string();
    let (mut viewer, _) = connect(&home, &runtime).await;
    let resumed = viewer
        .request("session.resume", json!({"session_id": session}))
        .await;
    assert_eq!(
        resumed["result"]["session_id"],
        session.as_str(),
        "{resumed}"
    );

    owner
        .request(
            "prompt.submit",
            json!({"session_id": session, "text": "U1 from the owner"}),
        )
        .await;
    let seen = viewer.events_until("message.complete").await;
    assert_eq!(seen[0]["params"]["type"], "message.start", "{:?}", seen[0]);
    assert_eq!(
        seen.last().unwrap()["params"]["payload"]["text"],
        REPLY.concat().as_str()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn canonical_session_validation_preserves_saved_messages_and_generation() {
    let (_dir, home, runtime) = start().await;
    let saved_id = "fixture-saved-session";
    let persistence = hermes_agent::SessionPersistence::new(&home);
    persistence
        .persist_session(
            saved_id,
            &[
                hermes_core::Message::user("Saved question"),
                hermes_core::Message::assistant("Saved answer"),
            ],
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let (mut client, _) = connect(&home, &runtime).await;
    for (method, params, code, reason) in [
        (
            "session.create",
            json!([]),
            4000,
            "params must be an object",
        ),
        (
            "session.create",
            json!({"request_id": ""}),
            4001,
            "request_id must be a non-empty string",
        ),
        (
            "session.create",
            json!({"request_id": "r-1", "source": true}),
            4001,
            "source must be a string",
        ),
        (
            "session.create",
            json!({"request_id": "r-2", "service_tier": "fast"}),
            4001,
            "only the normal service tier",
        ),
        (
            "session.resume",
            json!({"session_id": 2}),
            4001,
            "session_id must be a string",
        ),
        (
            "prompt.submit",
            json!({"session_id": saved_id, "text": "Fixture follow-up", "submission_id": 3}),
            4001,
            "submission_id must be a string",
        ),
    ] {
        let reply = client.request(method, params).await;
        assert_eq!(reply["error"]["code"], code, "{reply}");
        assert!(
            reply["error"]["message"].as_str().unwrap().contains(reason),
            "{reply}"
        );
    }
    assert!(client.events.is_empty());

    let missing = client
        .request("session.resume", json!({"session_id": "fixture-session"}))
        .await;
    assert_eq!(missing["error"]["code"], 4001, "{missing}");
    assert_eq!(
        missing["error"]["message"], "session_not_found",
        "{missing}"
    );

    let resumed = client
        .request("session.resume", json!({"session_id": saved_id}))
        .await;
    let session = &resumed["result"];
    assert_eq!(session["stored_session_id"], saved_id, "{resumed}");
    assert_eq!(session["message_count"], 2);
    assert_eq!(session["execution_generation"], 0);
    assert_eq!(session["messages"][0]["content"], "Saved question");
    assert_eq!(session["messages"][1]["content"], "Saved answer");
}

#[tokio::test(flavor = "multi_thread")]
async fn canonical_resume_reports_storage_failure_without_treating_it_as_missing() {
    let (_dir, home, runtime) = start().await;
    let persistence = hermes_agent::SessionPersistence::new(&home);
    std::fs::create_dir(persistence.db_path()).unwrap();
    let (mut client, _) = connect(&home, &runtime).await;

    let unavailable = client
        .request("session.resume", json!({"session_id": "fixture-session"}))
        .await;
    assert_eq!(unavailable["error"]["code"], 5001, "{unavailable}");
    assert!(
        unavailable["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("storage_unavailable:"),
        "{unavailable}"
    );
    assert!(persistence.db_path().is_dir());

    let created = client
        .request(
            "session.create",
            json!({"request_id": "fixture-create", "service_tier": "normal"}),
        )
        .await;
    assert_eq!(created["result"]["message_count"], 0, "{created}");
}
