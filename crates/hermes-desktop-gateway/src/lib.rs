//! Preview backend that Hermes Desktop attaches to by URL and token.
//!
//! Hermes Desktop's remote mode probes `GET /api/status`, then opens a WebSocket to
//! `/api/ws?token=...` and speaks JSON-RPC over it; the first frame from the backend is a
//! `gateway.ready` event. This crate serves that handshake. It answers every method with
//! JSON-RPC 'method not found' until the session methods are added.
//!
//! The listener is loopback only. Every request must carry a `Host` naming a loopback
//! address and the bound port (DNS rebinding), a browser `Origin` must be this server's own,
//! and the WebSocket upgrade needs the token. Each refusal happens before the upgrade.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

/// The environment variable Hermes Desktop and `hermes serve` use for the session token.
pub const TOKEN_ENV: &str = "HERMES_DASHBOARD_SESSION_TOKEN";

/// Shortest token accepted from the environment.
pub const MIN_TOKEN_LEN: usize = 32;

const LOOPBACK_NAMES: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

struct Gateway {
    token: String,
    port: u16,
    replay_epoch: String,
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
pub async fn serve(listener: tokio::net::TcpListener, token: String) -> std::io::Result<()> {
    let port = listener.local_addr()?.port();
    axum::serve(listener, router(token, port)).await
}

fn router(token: String, port: u16) -> Router {
    let gateway = Arc::new(Gateway {
        token,
        port,
        replay_epoch: uuid::Uuid::new_v4().simple().to_string(),
    });
    Router::new()
        .route("/api/status", get(status))
        .route("/api/ws", get(upgrade))
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

/// An absent `Origin` is a non-browser client; a browser page must be served from here.
fn origin_allowed(headers: &HeaderMap, port: u16) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    origin
        .to_str()
        .ok()
        .and_then(|o| o.strip_prefix("http://"))
        .is_some_and(|authority| is_loopback_authority(authority, port))
}

/// Public liveness probe: the version and that the socket needs a token; nothing else.
async fn status() -> Json<Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "auth_required": true,
        "gateway_running": true,
    }))
}

async fn upgrade(
    State(gw): State<Arc<Gateway>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let given = query.get("token").map(String::as_bytes).unwrap_or_default();
    if !bool::from(given.ct_eq(gw.token.as_bytes())) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !origin_allowed(&headers, gw.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let epoch = gw.replay_epoch.clone();
    ws.on_upgrade(move |socket| session(socket, epoch))
}

async fn session(mut socket: WebSocket, replay_epoch: String) {
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
    if socket
        .send(Message::Text(ready.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    while let Some(Ok(message)) = socket.recv().await {
        let reply = match message {
            Message::Text(text) => reply_to(&text),
            Message::Close(_) => break,
            _ => None,
        };
        if let Some(reply) = reply {
            if socket
                .send(Message::Text(reply.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    }
}

fn rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The reply to one inbound frame. Notifications (no id) and responses (no method) get none.
fn reply_to(text: &str) -> Option<Value> {
    let Ok(frame) = serde_json::from_str::<Value>(text) else {
        return Some(rpc_error(Value::Null, -32700, "parse error".into()));
    };
    let id = frame.get("id")?.clone();
    let method = frame.get("method")?.as_str()?;
    Some(rpc_error(id, -32601, format!("method not found: {method}")))
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
    fn tokens_are_long_and_distinct() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn frames_without_an_id_or_method_get_no_reply() {
        assert_eq!(reply_to(r#"{"jsonrpc":"2.0","method":"x"}"#), None);
        assert_eq!(reply_to(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#), None);
        assert_eq!(reply_to(r#"[1,2]"#), None);
    }
}
