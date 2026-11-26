//! Preview backend that Hermes Desktop attaches to by URL and token.
//!
//! Hermes Desktop's remote mode probes `GET /api/health` (or `/api/status`), then opens a WebSocket to
//! `/api/ws?token=...` and speaks JSON-RPC over it; the first frame from the backend is a
//! `gateway.ready` event. This crate serves that handshake, the chat methods in [`chat`] and
//! a session's saved messages at `GET /api/sessions/{id}/messages`, and reports the wake word and
//! self-update as off. Other methods get JSON-RPC 'method not found' (-32601).
//!
//! The listener is loopback only. Every request must carry a `Host` naming a loopback
//! address and the bound port (DNS rebinding). The WebSocket upgrade needs the token in its
//! query and the messages route needs it in `X-Hermes-Session-Token`, as the desktop sends
//! them; both need an `Origin` that is absent (no browser), exactly `file://` (the packaged
//! desktop) or an http(s) origin naming this listener. Each refusal happens before any
//! session is touched.
//!
//! Local mode ([`local`], Unix) serves the same routes to the desktop on this machine. There the
//! token path is off: the WebSocket upgrade needs a one-use `interactive` ticket in its `ticket`
//! query and the messages route a one-use `native-http` ticket in `X-Hermes-Gateway-Ticket`, both
//! minted on the runtime's private control socket, and any `Origin` refuses the request. The
//! desktop's native dial offers its ticket as the `hermes-gateway-ticket.` subprotocol beside
//! `hermes-gateway-v1` instead, and that connection speaks the canonical wire ([`chat`]'s
//! `canonical` module).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::Poll;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures::stream::{FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;

mod attach;
pub mod chat;
#[cfg(unix)]
pub mod local;
mod tickets;

pub use chat::Chat;
use chat::{Conn, Origin, Outgoing, Reply};
use tickets::{Purpose, Tickets};

/// The environment variable Hermes Desktop and `hermes serve` use for the session token.
pub const TOKEN_ENV: &str = "HERMES_DASHBOARD_SESSION_TOKEN";

/// Shortest token accepted from the environment.
pub const MIN_TOKEN_LEN: usize = 32;

/// Largest inbound WebSocket message other than an attach request; a larger one closes
/// the connection. This is the only memory bound on such a message: it is parsed whole before
/// anything else is checked.
// shortcut: 16 MiB, uvicorn's default. Parsing a message can cost about 100 times its size
// (a 16 MiB array of tiny objects, estimated at 1.5 GB; not measured), and connections are
// not limited; only a token holder, who controls the agent anyway, can send one.
pub const MAX_MESSAGE_BYTES: usize = 16 << 20;

/// Largest inbound WebSocket message (and frame) at all, as upstream allows
/// (`_DESKTOP_ATTACHMENT_WS_MAX_BYTES` in hermes_cli/web_server_boundary.py), because
/// `file.attach` sends a whole base64 file in one message. Over [`MAX_MESSAGE_BYTES`], only a
/// single `file.attach` or `image.attach_bytes` request with a string or number id and its
/// method's data is read, into fixed fields, so its parse costs about its size; anything else
/// closes the connection.
// shortcut: such a message is held 2 or 3 times while parsed (the text, its data and, when the
// data holds an escape, the parser's scratch copy), up to about 1.1 GiB a connection, and
// connections are not limited. Upgrade path: a streaming reader that decodes the data as it
// arrives.
pub const ATTACH_MAX_MESSAGE_BYTES: usize = 384 << 20;

/// Most requests one JSON-RPC batch may hold; a longer batch gets a single -32600. It bounds
/// the reply, not memory: the batch is already parsed when it is counted. Upstream's gateway
/// takes no batches at all, so no desktop frame comes near it.
pub const MAX_BATCH: usize = 100;

const LOOPBACK_NAMES: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// Header that carries the token on HTTP routes other than the public probes.
const TOKEN_HEADER: &str = "x-hermes-session-token";

/// The canonical session-authority protocol a native dial selects (upstream
/// gateway/session_contract.py `CANONICAL_GATEWAY_PROTOCOL`).
pub const CANONICAL_PROTOCOL: &str = "hermes-gateway-v1";

/// Prefix of the subprotocol that carries a native dial's one-use ticket.
const TICKET_PROTOCOL_PREFIX: &str = "hermes-gateway-ticket.";

/// Header that carries a local-mode `native-http` ticket (upstream
/// hermes_cli/dashboard_auth/native_http.py `HEADER`).
const TICKET_HEADER: &str = "x-hermes-gateway-ticket";

/// Most messages one history request returns.
const MAX_HISTORY: usize = 1000;

/// How a client proves it may use this gateway.
enum Auth {
    /// Remote mode: the session token.
    Token(String),
    /// Local mode: one-use tickets from the control socket, and no browser origin.
    #[cfg_attr(not(unix), allow(dead_code))]
    Native(Arc<Tickets>),
}

struct Gateway {
    auth: Auth,
    port: u16,
    chat: Arc<Chat>,
}

/// Numbers each connection, so a session knows which ones it streams to.
static CONNECTIONS: AtomicU64 = AtomicU64::new(0);

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
    axum::serve(listener, router(Auth::Token(token), port, chat)).await
}

fn router(auth: Auth, port: u16, chat: Chat) -> Router {
    let gateway = Arc::new(Gateway {
        auth,
        port,
        chat: Arc::new(chat),
    });
    let private = Router::new()
        .route("/api/sessions/{session_id}/messages", get(messages))
        .route("/api/hermes/update/check", get(update_check))
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

// Self-update is not served: this answers in the shape of upstream's
// hermes_cli/web_routers/actions.py `check_hermes_update` with nothing to apply. Voice chat is not
// served either, and `/api/audio/voice-live/status` is left unrouted on purpose: every 200 reply in
// upstream's contract shows the desktop's voice-engine picker, whilst a 404 is read as a backend
// that predates voice modes and hides it (apps/desktop/src/lib/voice-live.ts).

async fn update_check() -> Json<Value> {
    Json(json!({
        "install_method": "unknown",
        "current_version": env!("CARGO_PKG_VERSION"),
        "behind": null,
        "update_available": false,
        "can_apply": false,
        "update_command": "",
        "message": "This gateway does not check for updates; update it the way it was installed.",
    }))
}

fn token_matches(given: Option<&str>, token: &str) -> bool {
    bool::from(given.unwrap_or_default().as_bytes().ct_eq(token.as_bytes()))
}

/// The credential check, then the Origin check, as for the WebSocket upgrade.
async fn token_guard(State(gw): State<Arc<Gateway>>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    match admit(
        &gw,
        header(TOKEN_HEADER),
        header(TICKET_HEADER),
        Purpose::NativeHttp,
        headers,
    ) {
        Ok(()) => next.run(request).await,
        Err(status) => status.into_response(),
    }
}

/// 401 without the mode's credential, then 403 for an Origin the mode refuses. A local-mode
/// ticket is spent when presented, so one refused for its Origin cannot be presented again.
fn admit(
    gw: &Gateway,
    token: Option<&str>,
    ticket: Option<&str>,
    purpose: Purpose,
    headers: &HeaderMap,
) -> Result<(), StatusCode> {
    let origin_ok = match &gw.auth {
        Auth::Token(expected) => {
            if !token_matches(token, expected) {
                return Err(StatusCode::UNAUTHORIZED);
            }
            origin_allowed(headers, gw.port)
        }
        Auth::Native(tickets) => {
            if !ticket.is_some_and(|t| tickets.consume(t, purpose)) {
                return Err(StatusCode::UNAUTHORIZED);
            }
            !headers.contains_key(header::ORIGIN)
        }
    };
    if origin_ok {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
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
    let credential = |name: &str| query.get(name).map(String::as_str);
    // An ambiguous offer is refused before any ticket is read, so its ticket stays unspent. The
    // token mode has no tickets, so a ticket protocol there is refused too.
    let offered = match protocol_ticket(&headers) {
        Ok(Some(_)) if matches!(gw.auth, Auth::Token(_)) => Err(()),
        offered => offered,
    };
    let Ok(offered) = offered else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if let Err(status) = admit(
        &gw,
        credential("token"),
        offered.or(credential("ticket")),
        Purpose::Interactive,
        &headers,
    ) {
        return status.into_response();
    }
    let chat = Arc::clone(&gw.chat);
    let canonical = offered.is_some();
    // Both listeners take loopback connections only: the token one checks `Host` and `Origin`,
    // and the ticket one is local mode's. A new kind of listener must choose here.
    let origin = match gw.auth {
        Auth::Token(_) | Auth::Native(_) => Origin::Local,
    };
    // The ticket-bearing protocol is a credential: only the public one is ever selected.
    let ws = if canonical {
        ws.protocols([CANONICAL_PROTOCOL])
    } else {
        ws
    };
    ws.max_message_size(ATTACH_MAX_MESSAGE_BYTES)
        .max_frame_size(ATTACH_MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| session(socket, chat, canonical, origin))
}

/// The ticket a native dial offers as its `hermes-gateway-ticket.` subprotocol: `None` when it
/// offers none, an error when the offer is ambiguous (no `hermes-gateway-v1` beside it, more than
/// one ticket protocol, an empty ticket or a header that is not text), as upstream's
/// `_gateway_ws_ticket_from_subprotocol` (hermes_cli/web_server_boundary.py) decides.
fn protocol_ticket(headers: &HeaderMap) -> Result<Option<&str>, ()> {
    let mut offered = Vec::new();
    for value in headers.get_all(header::SEC_WEBSOCKET_PROTOCOL) {
        let value = value.to_str().map_err(|_| ())?;
        offered.extend(value.split(',').map(str::trim).filter(|p| !p.is_empty()));
    }
    let tickets: Vec<&str> = offered
        .iter()
        .filter_map(|p| p.strip_prefix(TICKET_PROTOCOL_PREFIX))
        .collect();
    match tickets[..] {
        [] => Ok(None),
        [ticket] if !ticket.is_empty() && offered.contains(&CANONICAL_PROTOCOL) => Ok(Some(ticket)),
        _ => Err(()),
    }
}

/// One connection. A `canonical` one ([`CANONICAL_PROTOCOL`]) announces the session authority in
/// its `gateway.ready` and answers the session verbs as [`chat`]'s `canonical` module does.
async fn session(mut socket: WebSocket, chat: Arc<Chat>, canonical: bool, origin: Origin) {
    let mut ready = json!({
        "jsonrpc": "2.0",
        "method": "event",
        "params": {"type": "gateway.ready", "payload": {
            // An empty skin is the contract's 'no skin engine' value; change_events false keeps
            // the desktop on its own polling until this backend broadcasts changes.
            "skin": {},
            "change_events": false,
            "replay_epoch": chat.replay_epoch(),
        }},
    });
    if canonical {
        ready["params"]["payload"]["session_authority"] = true.into();
    }
    // Turns stream their events through the outbox of each connection on their session,
    // after `gateway.ready` (see `serve_socket` for the order).
    let (outbox, mut events) = mpsc::unbounded_channel::<Outgoing>();
    // Cannot fail: `events` lives until the session ends.
    let _ = outbox.send(Outgoing::from(ready));
    let mut conn = Conn::new(
        CONNECTIONS.fetch_add(1, Ordering::Relaxed),
        outbox,
        canonical,
    );
    conn.origin = origin;
    let call = |method: &str, params: Value| chat.call(&conn, method, params);
    serve_socket(&mut socket, &mut events, &call, chat.write_timeout()).await;
    chat.disconnect(conn.id);
}

/// Most requests one connection has waiting on their handlers; past it, the connection's
/// next frame is read once one is answered.
const MAX_IN_FLIGHT: usize = 16;

/// A frame's reply, once its handlers are done.
type InFlight<'a> = Pin<Box<dyn Future<Output = Option<Value>> + Send + 'a>>;

/// Answers `socket`'s requests with `call` and writes `events`, each frame within
/// `write_timeout` of being queued, until either side closes.
///
/// A frame's handlers run whilst the connection goes on: a handler that waits holds up
/// neither the connection's events nor its other requests, and its reply goes out when it is
/// done. Each pass writes a finished reply first, then a queued event, and only then reads
/// another frame, so a handler that answers at once is answered before the next frame is read,
/// and a client that reads slowly cannot start more turns whilst finished ones wait. A turn
/// keeps its slot until its last event is written to the connection that prompted it, so the
/// turn cap bounds the replies waiting on those connections. A frame not written within the
/// write timeout of being queued closes the connection, which drops its queued events, frees
/// their slots and drops the handlers still waiting.
async fn serve_socket(
    socket: &mut WebSocket,
    events: &mut mpsc::UnboundedReceiver<Outgoing>,
    call: &Call<'_>,
    write_timeout: std::time::Duration,
) {
    let mut in_flight: FuturesUnordered<InFlight<'_>> = FuturesUnordered::new();
    loop {
        let frame = tokio::select! {
            biased;
            Some(reply) = in_flight.next(), if !in_flight.is_empty() => reply.map(Outgoing::from),
            Some(event) = events.recv() => Some(event),
            inbound = socket.recv(), if in_flight.len() < MAX_IN_FLIGHT => {
                match inbound {
                    Some(Ok(Message::Text(text))) if text.len() <= MAX_MESSAGE_BYTES => {
                        in_flight.push(Box::pin(async move { reply_to(&text, call).await }));
                    }
                    Some(Ok(Message::Text(text))) => match attach_request(&text) {
                        Some(request) => in_flight.push(Box::pin(reply_to_one(request, call))),
                        None => break,
                    },
                    Some(Ok(Message::Binary(data))) if data.len() > MAX_MESSAGE_BYTES => break,
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    Some(Ok(_)) => {}
                }
                None
            }
        };
        if let Some(outgoing) = frame {
            let deadline = outgoing.queued + write_timeout;
            let text = axum::extract::ws::Utf8Bytes::from(&*outgoing.text);
            if !write_by(deadline, socket.send(Message::Text(text))).await {
                break;
            }
        }
    }
}

/// Runs `write` until it finishes or `deadline` passes: true if it finished first. `write` is
/// never polled at or after the deadline, where `timeout_at` would still poll it first and take
/// a write that is ready by then.
async fn write_by<E>(
    deadline: tokio::time::Instant,
    write: impl Future<Output = Result<(), E>>,
) -> bool {
    let mut write = std::pin::pin!(write);
    let mut expiry = std::pin::pin!(tokio::time::sleep_until(deadline));
    std::future::poll_fn(|cx| {
        if tokio::time::Instant::now() >= deadline {
            return Poll::Ready(false);
        }
        if let Poll::Ready(result) = write.as_mut().poll(cx) {
            return Poll::Ready(result.is_ok());
        }
        // Wakes this task at the deadline, so a stalled write ends then.
        expiry.as_mut().poll(cx).map(|()| false)
    })
    .await
}

fn rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Runs one request's method; the error is a JSON-RPC code and message.
type Call<'a> = dyn Fn(&str, Value) -> Reply + Sync + 'a;

/// The code for a batch member that was not run because the batch's reply had passed
/// [`MAX_MESSAGE_BYTES`] (JSON-RPC's range for server errors).
const BATCH_FULL: i64 = -32000;

/// The length of `value` as JSON text, without building the text.
fn json_len(value: &Value) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // Writing to a counter cannot fail.
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}

/// The reply to one inbound frame: one request or a JSON-RPC batch of them, run in order. Once
/// a batch's replies pass [`MAX_MESSAGE_BYTES`], its later requests are not run and get
/// [`BATCH_FULL`], so one small frame cannot build an unbounded reply.
async fn reply_to(text: &str, call: &Call<'_>) -> Option<Value> {
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
            let full: &Call<'_> = &|_, _| {
                chat::ready(Err((
                    BATCH_FULL,
                    format!(
                        "not run: this batch's reply passed {} MiB; send the request again on its own",
                        MAX_MESSAGE_BYTES >> 20
                    ),
                )))
            };
            let mut replies = Vec::new();
            let mut bytes = 0;
            for item in items {
                let run = if bytes > MAX_MESSAGE_BYTES {
                    full
                } else {
                    call
                };
                if let Some(reply) = reply_to_one(item, run).await {
                    bytes += json_len(&reply);
                    replies.push(reply);
                }
            }
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        single => reply_to_one(single, call).await,
    }
}

/// A message over [`MAX_MESSAGE_BYTES`] as a request [`reply_to_one`] can check, if it is a
/// single `file.attach` or `image.attach_bytes` request or notification with a string, number
/// or null id and string params that hold its method's data. Every other member is skipped
/// unparsed, so no part of the message is built into a tree.
fn attach_request(text: &str) -> Option<Value> {
    #[derive(serde::Deserialize)]
    struct Request {
        jsonrpc: String,
        // Absent for a notification; `Some(Id(Null))` for an explicit null.
        #[serde(default, deserialize_with = "present")]
        id: Option<Id>,
        method: String,
        params: Object<Params>,
    }
    /// `T` read from a JSON object only: a derived struct also takes an array of its fields
    /// in order, which is no JSON-RPC request.
    struct Object<T>(T);
    impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Object<T> {
        fn deserialize<D: serde::Deserializer<'de>>(from: D) -> Result<Self, D::Error> {
            struct Fields<T>(std::marker::PhantomData<T>);
            impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Fields<T> {
                type Value = T;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("an object")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
                    T::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                }
            }
            from.deserialize_map(Fields(std::marker::PhantomData))
                .map(Object)
        }
    }
    fn present<'de, D: serde::Deserializer<'de>>(from: D) -> Result<Option<Id>, D::Error> {
        serde::Deserialize::deserialize(from).map(Some)
    }
    /// `file.attach`'s `data_url` and `name`, or `image.attach_bytes`'s `content_base64` (or
    /// its `data` alias).
    #[derive(serde::Deserialize)]
    struct Params {
        session_id: String,
        data_url: Option<String>,
        name: Option<String>,
        content_base64: Option<String>,
        data: Option<String>,
    }
    /// A string, number or null id. Not an untagged enum, which would buffer an array or
    /// object id whole before refusing it.
    struct Id(Value);
    impl<'de> serde::Deserialize<'de> for Id {
        fn deserialize<D: serde::Deserializer<'de>>(from: D) -> Result<Self, D::Error> {
            struct Scalar;
            impl serde::de::Visitor<'_> for Scalar {
                type Value = Value;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("a string or number id")
                }
                fn visit_unit<E>(self) -> Result<Value, E> {
                    Ok(Value::Null)
                }
                fn visit_str<E>(self, id: &str) -> Result<Value, E> {
                    Ok(Value::String(id.to_owned()))
                }
                fn visit_u64<E>(self, id: u64) -> Result<Value, E> {
                    Ok(id.into())
                }
                fn visit_i64<E>(self, id: i64) -> Result<Value, E> {
                    Ok(id.into())
                }
                fn visit_f64<E: serde::de::Error>(self, id: f64) -> Result<Value, E> {
                    serde_json::Number::from_f64(id)
                        .map(Value::Number)
                        .ok_or_else(|| E::custom("id is not finite"))
                }
            }
            from.deserialize_any(Scalar).map(Id)
        }
    }
    let Object(request): Object<Request> = serde_json::from_str(text).ok()?;
    let Object(Params {
        session_id,
        data_url,
        name,
        content_base64,
        data,
    }) = request.params;
    let fields = match request.method.as_str() {
        "file.attach" if data_url.is_some() => [("data_url", data_url), ("name", name)],
        "image.attach_bytes" if content_base64.is_some() || data.is_some() => {
            [("content_base64", content_base64), ("data", data)]
        }
        _ => return None,
    };
    let mut params = serde_json::Map::new();
    params.insert("session_id".into(), Value::String(session_id));
    for (key, value) in fields {
        if let Some(value) = value {
            params.insert(key.into(), Value::String(value));
        }
    }
    let mut frame = serde_json::Map::new();
    frame.insert("jsonrpc".into(), Value::String(request.jsonrpc));
    if let Some(Id(id)) = request.id {
        frame.insert("id".into(), id);
    }
    frame.insert("method".into(), Value::String(request.method));
    frame.insert("params".into(), Value::Object(params));
    Some(Value::Object(frame))
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
async fn reply_to_one(frame: Value, call: &Call<'_>) -> Option<Value> {
    let invalid = |id: Value| Some(rpc_error(id, -32600, "invalid request".into()));
    let Value::Object(mut request) = frame else {
        return invalid(Value::Null);
    };
    let params = request.remove("params");
    let id = request.get("id");
    let id_valid = matches!(
        id,
        None | Some(Value::String(_) | Value::Number(_) | Value::Null)
    );
    let reply_id = match id {
        Some(id) if id_valid => id.clone(),
        _ => Value::Null,
    };
    let params_valid = matches!(params, None | Some(Value::Array(_) | Value::Object(_)));
    let params = params.unwrap_or(Value::Null);
    let version_2 = request.get("jsonrpc").and_then(Value::as_str) == Some("2.0");
    match request.get("method") {
        Some(Value::String(method))
            if version_2 && !method.is_empty() && id_valid && params_valid =>
        {
            id?;
            Some(match call(method, params).await {
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
    use std::cell::Cell;
    use tokio::time::{Duration, Instant};

    #[tokio::test]
    async fn a_write_is_not_started_once_its_deadline_has_passed() {
        let polls = Cell::new(0);
        let write = std::future::poll_fn(|_| {
            polls.set(polls.get() + 1);
            Poll::Ready(Ok::<(), ()>(()))
        });
        assert!(!write_by(Instant::now(), write).await);
        assert_eq!(polls.get(), 0);
    }

    #[tokio::test]
    async fn a_pending_write_is_not_polled_again_once_its_deadline_has_passed() {
        // Pending at first and ready on the next poll, which only the deadline's timer causes.
        let polls = Cell::new(0);
        let write = std::future::poll_fn(|_| {
            polls.set(polls.get() + 1);
            if polls.get() == 1 {
                Poll::Pending
            } else {
                Poll::Ready(Ok::<(), ()>(()))
            }
        });
        assert!(!write_by(Instant::now() + Duration::from_millis(50), write).await);
        assert_eq!(polls.get(), 1);
    }

    #[tokio::test]
    async fn a_write_that_finishes_before_its_deadline_counts() {
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(write_by(deadline, async { Ok::<(), ()>(()) }).await);
        assert!(!write_by(deadline, async { Err(()) }).await);
    }

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

    #[tokio::test]
    async fn notifications_and_responses_get_no_reply_and_run_nothing() {
        let call: &Call<'_> = &|method, _| panic!("{method} was run");
        assert_eq!(
            reply_to(r#"{"jsonrpc":"2.0","method":"x"}"#, call).await,
            None
        );
        assert_eq!(
            reply_to(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, call).await,
            None
        );
        assert_eq!(
            reply_to(
                r#"[{"jsonrpc":"2.0","method":"x"},{"id":2,"result":{}}]"#,
                call
            )
            .await,
            None
        );
    }

    /// A batch whose replies pass [`MAX_MESSAGE_BYTES`] runs no more of its requests; each
    /// is still answered, with an error that says to send it again on its own.
    #[tokio::test]
    async fn a_batch_stops_running_requests_once_its_reply_is_large() {
        let runs = std::sync::atomic::AtomicUsize::new(0);
        let big = "x".repeat(1 << 20);
        let call: &Call<'_> = &|_, _| {
            runs.fetch_add(1, Ordering::Relaxed);
            chat::ready(Ok(Value::String(big.clone())))
        };
        let batch: Vec<Value> = (0..MAX_BATCH)
            .map(|id| json!({"jsonrpc": "2.0", "id": id, "method": "m"}))
            .collect();
        let reply = reply_to(&Value::Array(batch).to_string(), call)
            .await
            .unwrap();
        let replies = reply.as_array().unwrap();
        assert_eq!(replies.len(), MAX_BATCH);
        let ran = runs.load(Ordering::Relaxed);
        assert!((1..MAX_BATCH).contains(&ran), "{ran} ran");
        assert!(reply.to_string().len() <= MAX_MESSAGE_BYTES + (2 << 20));
        let last = &replies[MAX_BATCH - 1];
        assert_eq!(last["id"], MAX_BATCH - 1);
        assert_eq!(last["error"]["code"], -32000, "{last}");
    }

    /// A request whose handler waits holds up neither the connection's events nor its
    /// other requests, and is answered once it is done.
    #[tokio::test]
    async fn a_waiting_request_holds_up_neither_events_nor_other_requests() {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message as Frame;

        let release = Arc::new(tokio::sync::Notify::new());
        let (outbox, events) = mpsc::unbounded_channel::<Outgoing>();
        let events = Arc::new(std::sync::Mutex::new(Some(events)));
        let parked = Arc::clone(&release);
        let app = Router::new().route(
            "/",
            get(move |ws: WebSocketUpgrade| {
                let events = events.lock().unwrap().take();
                let release = Arc::clone(&parked);
                async move {
                    ws.on_upgrade(move |mut socket| async move {
                        let mut events = events.expect("one connection");
                        let call = move |method: &str, _: Value| -> Reply {
                            if method == "wait" {
                                let release = Arc::clone(&release);
                                Box::pin(async move {
                                    release.notified().await;
                                    Ok(json!("released"))
                                })
                            } else {
                                chat::ready(Ok(json!("pong")))
                            }
                        };
                        serve_socket(&mut socket, &mut events, &call, Duration::from_secs(30))
                            .await;
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
            .await
            .unwrap();
        let request = |id: u64, method: &str| {
            Frame::text(json!({"jsonrpc": "2.0", "id": id, "method": method}).to_string())
        };
        ws.send(request(1, "wait")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        outbox
            .send(Outgoing::from(json!({"method": "event"})))
            .unwrap();
        assert_eq!(read(&mut ws).await["method"], "event");
        ws.send(request(2, "ping")).await.unwrap();
        let pong = read(&mut ws).await;
        assert_eq!((&pong["id"], &pong["result"]), (&json!(2), &json!("pong")));
        release.notify_one();
        let released = read(&mut ws).await;
        assert_eq!(
            (&released["id"], &released["result"]),
            (&json!(1), &json!("released"))
        );
    }

    /// With 16 requests waiting, the connection still reads: a request that answers at once
    /// (an approval's answer, say) is answered, and one that would wait is answered busy.
    #[tokio::test]
    async fn a_connection_with_16_waiting_requests_still_reads_and_answers() {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message as Frame;

        let release = Arc::new(tokio::sync::Notify::new());
        let (_outbox, events) = mpsc::unbounded_channel::<Outgoing>();
        let events = Arc::new(std::sync::Mutex::new(Some(events)));
        let parked = Arc::clone(&release);
        let app = Router::new().route(
            "/",
            get(move |ws: WebSocketUpgrade| {
                let events = events.lock().unwrap().take();
                let release = Arc::clone(&parked);
                async move {
                    ws.on_upgrade(move |mut socket| async move {
                        let mut events = events.expect("one connection");
                        let call = move |method: &str, _: Value| -> Reply {
                            if method == "wait" {
                                let release = Arc::clone(&release);
                                Box::pin(async move {
                                    release.notified().await;
                                    Ok(json!("released"))
                                })
                            } else {
                                chat::ready(Ok(json!("answered")))
                            }
                        };
                        serve_socket(&mut socket, &mut events, &call, Duration::from_secs(30))
                            .await;
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
            .await
            .unwrap();
        let request = |id: u64, method: &str| {
            Frame::text(json!({"jsonrpc": "2.0", "id": id, "method": method}).to_string())
        };
        for id in 1..=16 {
            ws.send(request(id, "wait")).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        ws.send(request(17, "approval.respond")).await.unwrap();
        let answered = read(&mut ws).await;
        assert_eq!(
            (&answered["id"], &answered["result"]),
            (&json!(17), &json!("answered"))
        );
        ws.send(request(18, "wait")).await.unwrap();
        let busy = read(&mut ws).await;
        assert_eq!(busy["id"], 18, "{busy}");
        assert_eq!(busy["error"]["code"], -32001, "{busy}");
        release.notify_waiters();
        let mut released = Vec::new();
        for _ in 0..16 {
            released.push(read(&mut ws).await["id"].as_u64().unwrap());
        }
        released.sort_unstable();
        assert_eq!(released, (1..=16).collect::<Vec<_>>());
    }

    async fn read(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Value {
        use futures::StreamExt;
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("a frame within 5 s")
                .unwrap()
                .unwrap();
            if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }
}
