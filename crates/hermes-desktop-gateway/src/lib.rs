//! Preview backend that Hermes Desktop attaches to by URL and token.
//!
//! Hermes Desktop's remote mode probes `GET /api/health` (or `/api/status`), then opens a WebSocket to
//! `/api/ws?token=...` and speaks JSON-RPC over it; the first frame from the backend is a
//! `gateway.ready` event. This crate serves that handshake, the chat methods in [`chat`] and
//! a session's saved messages at `GET /api/sessions/{id}/messages`. Other methods get
//! JSON-RPC 'method not found'.
//!
//! The listener is loopback only. Every request must carry a `Host` naming a loopback
//! address and the bound port (DNS rebinding). The WebSocket upgrade needs the token in its
//! query and the messages route needs it in `X-Hermes-Session-Token`, as the desktop sends
//! them; both need an `Origin` that is absent (no browser), exactly `file://` (the packaged
//! desktop) or an http(s) origin naming this listener. Each refusal happens before any
//! session is touched.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;

pub mod chat;

pub use chat::Chat;
use chat::{Outgoing, RpcError};

/// The environment variable Hermes Desktop and `hermes serve` use for the session token.
pub const TOKEN_ENV: &str = "HERMES_DASHBOARD_SESSION_TOKEN";

/// Shortest token accepted from the environment.
pub const MIN_TOKEN_LEN: usize = 32;

/// Largest inbound WebSocket message (and frame); a larger one closes the connection. This
/// is the only memory bound on a message: it is parsed whole before anything else is checked.
// shortcut: 16 MiB, uvicorn's default. Parsing a message can cost about 100 times its size
// (a 16 MiB array of tiny objects, estimated at 1.5 GB; not measured), and connections are
// not limited; only a token holder, who controls the agent anyway, can send one. Upstream
// allows 384 MiB (`_DESKTOP_ATTACHMENT_WS_MAX_BYTES` in hermes_cli/web_server_boundary.py)
// because `file.attach` sends a whole base64 file in one frame. Upgrade path: when this
// crate serves `file.attach`, raise the cap with a streaming reader instead of a whole parse.
pub const MAX_MESSAGE_BYTES: usize = 16 << 20;

/// Most requests one JSON-RPC batch may hold; a longer batch gets a single -32600. It bounds
/// the reply, not memory: the batch is already parsed when it is counted. Upstream's gateway
/// takes no batches at all, so no desktop frame comes near it.
pub const MAX_BATCH: usize = 100;

const LOOPBACK_NAMES: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// Header that carries the token on HTTP routes other than the public probes.
const TOKEN_HEADER: &str = "x-hermes-session-token";

/// Most messages one history request returns.
const MAX_HISTORY: usize = 1000;

struct Gateway {
    token: String,
    port: u16,
    replay_epoch: String,
    chat: Arc<Chat>,
}

/// A new random token: 64 hexadecimal characters from 2 version 4 UUIDs (244 random bits).
pub fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Serves the desktop gateway on `listener`, which the caller has bound to a loopback address.
pub async fn serve(
    listener: tokio::net::TcpListener,
    token: String,
    chat: Chat,
) -> std::io::Result<()> {
    let port = listener.local_addr()?.port();
    axum::serve(listener, router(token, port, chat)).await
}

fn router(token: String, port: u16, chat: Chat) -> Router {
    let gateway = Arc::new(Gateway {
        token,
        port,
        replay_epoch: uuid::Uuid::new_v4().simple().to_string(),
        chat: Arc::new(chat),
    });
    let private = Router::new()
        .route("/api/sessions/{session_id}/messages", get(messages))
        .layer(middleware::from_fn_with_state(gateway.clone(), token_guard));
    Router::new()
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/ws", get(upgrade))
        .merge(private)
        .layer(middleware::from_fn_with_state(gateway.clone(), host_guard))
        .with_state(gateway)
}

/// True when `authority` (a `Host` value or an origin's host and port) names a loopback
/// address and `port`. A missing port means 80.
fn is_loopback_authority(authority: &str, port: u16) -> bool {
    let (name, given_port) = match authority.rsplit_once(':') {
        Some((name, p)) if !p.contains(']') => (name, p.parse::<u16>().ok()),
        _ => (authority, Some(80)),
    };
    given_port == Some(port)
        && LOOPBACK_NAMES
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed))
}

async fn host_guard(State(gw): State<Arc<Gateway>>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    match host {
        Some(host) if is_loopback_authority(host, gw.port) => next.run(request).await,
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

/// An absent `Origin` is a client without a browser, such as the desktop's main-process
/// probe. A web page (`http` or `https`) must be served from this listener. The packaged
/// desktop loads its renderer from a file, so its socket sends exactly `file://` (upstream's
/// tests/hermes_cli/test_dashboard_auth_ws_auth.py, and seen in a live trace), which no page
/// served over the web can send. A local HTML file opened in a Chromium browser likely sends
/// `file://` too (inferred from Chromium's source, not tested), so the token alone stops it.
/// Every other origin is refused, `null` included: sandboxed frames and `data:` pages send
/// it. Upstream also admits `null` and other schemes and relies on the token for them; if
/// its desktop moves to one of those, this check refuses it (fails closed).
fn origin_allowed(headers: &HeaderMap, port: u16) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    match origin.split_once("://") {
        Some((scheme, authority))
            if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") =>
        {
            is_loopback_authority(authority, port)
        }
        _ => origin == "file://",
    }
}

// Public probes say only that the process is up and its version. `auth_required: false` is
// how the desktop tells a token gateway from an OAuth-gated one (`authModeFromStatus` in
// upstream's apps/desktop/electron/connection-config.ts); the socket still needs the token.

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "version": env!("CARGO_PKG_VERSION"), "auth_required": false}))
}

async fn status() -> Json<Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "auth_required": false,
        "gateway_running": true,
    }))
}

fn token_matches(given: Option<&str>, token: &str) -> bool {
    bool::from(given.unwrap_or_default().as_bytes().ct_eq(token.as_bytes()))
}

/// The token check, then the Origin check, as for the WebSocket upgrade.
async fn token_guard(State(gw): State<Arc<Gateway>>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let given = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok());
    if !token_matches(given, &gw.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !origin_allowed(headers, gw.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

/// A session's saved user and assistant messages, oldest first: the first `limit` with
/// `order=oldest` (the default), the last `limit` with `order=latest`. Unknown query keys,
/// such as the desktop's `profile`, are ignored.
async fn messages(
    State(gw): State<Arc<Gateway>>,
    Path(session_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let limit = query
        .get("limit")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(MAX_HISTORY)
        .clamp(1, MAX_HISTORY);
    let mut rows = match gw.chat.history(&session_id) {
        None => return StatusCode::NOT_FOUND.into_response(),
        Some(Err(err)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"detail": err})),
            )
                .into_response()
        }
        Some(Ok(rows)) => rows,
    };
    if query.get("order").map(String::as_str) == Some("latest") {
        rows.drain(..rows.len().saturating_sub(limit));
    } else {
        rows.truncate(limit);
    }
    Json(json!({"session_id": session_id, "messages": rows})).into_response()
}

async fn upgrade(
    State(gw): State<Arc<Gateway>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !token_matches(query.get("token").map(String::as_str), &gw.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !origin_allowed(&headers, gw.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let epoch = gw.replay_epoch.clone();
    let chat = Arc::clone(&gw.chat);
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| session(socket, epoch, chat))
}

async fn session(mut socket: WebSocket, replay_epoch: String, chat: Arc<Chat>) {
    let ready = json!({
        "jsonrpc": "2.0",
        "method": "event",
        "params": {"type": "gateway.ready", "payload": {
            // An empty skin is the contract's 'no skin engine' value; change_events false keeps
            // the desktop on its own polling until this backend broadcasts changes.
            "skin": {},
            "change_events": false,
            "replay_epoch": replay_epoch,
        }},
    });
    // Turns stream their events through this connection's outbox, after `gateway.ready`;
    // replies go out first, as each is written before the next outbox frame is read. Queued
    // events are written before the next request is read, so a client that reads slowly cannot
    // start more turns while finished ones wait. A turn keeps its slot until its last event is
    // written, so the turn cap bounds the replies waiting across all connections. A frame not
    // written within the write timeout of being queued closes the connection, which drops its
    // queued events and frees their slots.
    let (outbox, mut events) = mpsc::unbounded_channel::<Outgoing>();
    // Cannot fail: `events` lives until the session ends.
    let _ = outbox.send(Outgoing::from(ready));
    let call = |method: &str, params: &Value| chat.call(method, params, &outbox);
    loop {
        let frame = tokio::select! {
            biased;
            Some(event) = events.recv() => Some(event),
            inbound = socket.recv() => match inbound {
                Some(Ok(Message::Text(text))) => reply_to(&text, &call).map(Outgoing::from),
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => None,
            },
        };
        if let Some(outgoing) = frame {
            let deadline = outgoing.queued + chat.write_timeout();
            // `timeout_at` would still take a write that is ready at once after the deadline.
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            let write = socket.send(Message::Text(outgoing.frame.to_string().into()));
            if !matches!(tokio::time::timeout_at(deadline, write).await, Ok(Ok(()))) {
                break;
            }
        }
    }
}

fn rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Runs one request's method; the error is a JSON-RPC code and message.
type Call<'a> = dyn Fn(&str, &Value) -> Result<Value, RpcError> + 'a;

/// The reply to one inbound frame: one request or a JSON-RPC batch of them.
fn reply_to(text: &str, call: &Call<'_>) -> Option<Value> {
    let Ok(frame) = serde_json::from_str::<Value>(text) else {
        return Some(rpc_error(Value::Null, -32700, "parse error".into()));
    };
    match frame {
        Value::Array(items) if items.is_empty() => Some(rpc_error(
            Value::Null,
            -32600,
            "invalid request: empty batch".into(),
        )),
        Value::Array(items) if items.len() > MAX_BATCH => Some(rpc_error(
            Value::Null,
            -32600,
            format!("invalid request: a batch holds at most {MAX_BATCH} requests"),
        )),
        Value::Array(items) => {
            let replies: Vec<Value> = items
                .iter()
                .filter_map(|item| reply_to_one(item, call))
                .collect();
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        single => reply_to_one(&single, call),
    }
}

/// A request carries "jsonrpc": "2.0", a non-empty string method, an id that is a string, a
/// number or null when present, and params that are an array or an object when present. A
/// request without an id is a notification: it is not run and gets no reply, as does a
/// response to our own requests (an id with a result or an error). Anything else gets
/// -32600, with its id only when that id has a valid type.
///
/// One departure from JSON-RPC 2.0 is deliberate: a notification with an empty method gets
/// -32600, where the specification allows no reply to any notification. Upstream's gateway
/// (`_normalize_request` in tui_gateway/server.py) answers an empty method that way with or
/// without an id, and Hermes Desktop never sends one.
fn reply_to_one(frame: &Value, call: &Call<'_>) -> Option<Value> {
    let invalid = |id: Value| Some(rpc_error(id, -32600, "invalid request".into()));
    let Some(request) = frame.as_object() else {
        return invalid(Value::Null);
    };
    let id = request.get("id");
    let id_valid = matches!(
        id,
        None | Some(Value::String(_) | Value::Number(_) | Value::Null)
    );
    let reply_id = match id {
        Some(id) if id_valid => id.clone(),
        _ => Value::Null,
    };
    let params_valid = matches!(
        request.get("params"),
        None | Some(Value::Array(_) | Value::Object(_))
    );
    let version_2 = request.get("jsonrpc").and_then(Value::as_str) == Some("2.0");
    match request.get("method") {
        Some(Value::String(method))
            if version_2 && !method.is_empty() && id_valid && params_valid =>
        {
            id?;
            let params = request.get("params").unwrap_or(&Value::Null);
            Some(match call(method, params) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": reply_id, "result": result}),
                Err((code, message)) => rpc_error(reply_id, code, message),
            })
        }
        Some(_) => invalid(reply_id),
        None if id.is_some()
            && (request.contains_key("result") || request.contains_key("error")) =>
        {
            None
        }
        None => invalid(reply_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_authorities() {
        assert!(is_loopback_authority("127.0.0.1:9119", 9119));
        assert!(is_loopback_authority("LOCALHOST:9119", 9119));
        assert!(is_loopback_authority("[::1]:9119", 9119));
        assert!(is_loopback_authority("localhost", 80));
        assert!(!is_loopback_authority("localhost", 9119));
        assert!(!is_loopback_authority("127.0.0.1:9120", 9119));
        assert!(!is_loopback_authority("127.0.0.2:9119", 9119));
        assert!(!is_loopback_authority(
            "localhost.attacker.example:9119",
            9119
        ));
        assert!(!is_loopback_authority("[::1]", 9119));
        assert!(!is_loopback_authority("127.0.0.1:", 9119));
    }

    #[test]
    fn origins() {
        let allowed = |origin: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::ORIGIN, origin.parse().unwrap());
            origin_allowed(&headers, 9119)
        };
        assert!(origin_allowed(&HeaderMap::new(), 9119));
        assert!(allowed("http://localhost:9119"));
        assert!(allowed("https://127.0.0.1:9119"));
        assert!(allowed("file://"));
        assert!(!allowed("null"));
        assert!(!allowed("app://hermes"));
        assert!(!allowed("file://attacker.example"));
        assert!(!allowed("HTTP://attacker.example:9119"));
        assert!(!allowed("https://localhost:9120"));
        assert!(!allowed("attacker.example"));
    }

    #[test]
    fn tokens_are_long_and_distinct() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn notifications_and_responses_get_no_reply_and_run_nothing() {
        let call: &Call<'_> = &|method, _| panic!("{method} was run");
        assert_eq!(reply_to(r#"{"jsonrpc":"2.0","method":"x"}"#, call), None);
        assert_eq!(
            reply_to(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, call),
            None
        );
        assert_eq!(
            reply_to(
                r#"[{"jsonrpc":"2.0","method":"x"},{"id":2,"result":{}}]"#,
                call
            ),
            None
        );
    }
}
