//! `listen`: the GitHub webhook receiver (`serve_webhook` in the script).
//!
//! Stricter than the script: it will not start without a secret unless told
//! to accept unsigned deliveries on a loopback address; bodies are capped at
//! GitHub's 25 MB payload limit and must arrive within 30 seconds; at most
//! 16 requests are handled at once; the delivery id must be file-name safe.
//! The secret, headers and bodies are never logged or echoed.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use hermes_py::Py;
use hmac::{Hmac, Mac};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use sha2::Sha256;
use tokio::sync::Semaphore;

use crate::event::{valid_delivery_id, SyncEvent};
use crate::queue::Publisher;
use crate::{log, Level};

/// GitHub caps webhook payloads at 25 MB.
pub const MAX_BODY_BYTES: usize = 25 * 1024 * 1024;
const BODY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_IN_FLIGHT: usize = 16;

pub struct ListenConfig {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub secret: String,
    pub allow_unsigned: bool,
    pub expected_repo: String,
    pub expected_ref: String,
    pub backend_name: String,
}

/// Refuses an empty secret unless unsigned deliveries were asked for and
/// every address the host resolves to is loopback. The script accepted every
/// request unsigned whenever `GITHUB_WEBHOOK_SECRET` was unset.
pub fn check_secret(config: &ListenConfig) -> Result<(), String> {
    if !config.secret.is_empty() {
        return Ok(());
    }
    if !config.allow_unsigned {
        return Err(
            "GITHUB_WEBHOOK_SECRET is not set: refusing to accept unsigned deliveries \
                    (set it, or pass --allow-unsigned to accept them on a loopback address)"
                .to_string(),
        );
    }
    let addrs: Vec<SocketAddr> = (config.host.as_str(), config.port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve --host {}: {e}", config.host))?
        .collect();
    if addrs.is_empty() || !addrs.iter().all(|a| a.ip().is_loopback()) {
        return Err(format!(
            "--allow-unsigned needs a loopback --host; {} is not loopback",
            config.host
        ));
    }
    Ok(())
}

/// `X-Hub-Signature-256` must be `sha256=` and 64 lowercase hex digits (the
/// form GitHub sends and the only one the script's string comparison took);
/// the digest is compared in constant time.
pub fn verify_signature(body: &[u8], secret: &str, header: Option<&[u8]>) -> bool {
    let Some(hex) = header.and_then(|h| h.strip_prefix(b"sha256=")) else {
        return false;
    };
    if hex.len() != 64 || !hex.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return false;
    }
    let nibble = |b: u8| {
        if b.is_ascii_digit() {
            b - b'0'
        } else {
            b - b'a' + 10
        }
    };
    let digest: Vec<u8> = hex
        .chunks(2)
        .map(|p| (nibble(p[0]) << 4) | nibble(p[1]))
        .collect();
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&digest).is_ok()
}

struct Shared {
    config: ListenConfig,
    queue: Arc<dyn Publisher>,
    in_flight: Semaphore,
}

pub async fn serve(config: ListenConfig, queue: Arc<dyn Publisher>) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port))
        .await
        .map_err(|e| format!("could not listen on {}:{}: {e}", config.host, config.port))?;
    run(listener, config, queue).await
}

pub async fn run(
    listener: tokio::net::TcpListener,
    config: ListenConfig,
    queue: Arc<dyn Publisher>,
) -> Result<(), String> {
    log(
        Level::Info,
        &format!(
            "webhook server listening on http://{}:{}{} backend={} expected={} {}",
            config.host,
            config.port,
            config.path,
            config.backend_name,
            config.expected_repo,
            config.expected_ref
        ),
    );
    let shared = Arc::new(Shared {
        config,
        queue,
        in_flight: Semaphore::new(MAX_IN_FLIGHT),
    });
    let app = axum::Router::new().fallback(handle).with_state(shared);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
        log(Level::Info, "webhook server interrupted; exiting");
    })
    .await
    .map_err(|e| format!("webhook server failed: {e}"))
}

async fn handle(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let request_line = format!(
        "{} {} {:?}",
        request.method(),
        request.uri(),
        request.version()
    );
    let response = respond(&shared, request).await;
    log(
        Level::Info,
        &format!(
            "{} - \"{request_line}\" {} -",
            peer.ip(),
            response.status().as_u16()
        ),
    );
    response
}

async fn respond(shared: &Shared, request: Request) -> Response {
    let Ok(_permit) = shared.in_flight.acquire().await else {
        return text(StatusCode::SERVICE_UNAVAILABLE, "shutting down");
    };
    let config = &shared.config;
    if request.method() != Method::POST {
        return text(
            StatusCode::NOT_IMPLEMENTED,
            &format!("Unsupported method ('{}')", request.method()),
        );
    }
    let target = request.uri().path_and_query().map_or("", |p| p.as_str());
    if target != config.path {
        return text(StatusCode::NOT_FOUND, "Not Found");
    }
    let (parts, body) = request.into_parts();
    let body = match read_body(&parts.headers, body).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let headers = &parts.headers;
    let signature = headers.get("x-hub-signature-256").map(|v| v.as_bytes());
    if !config.secret.is_empty() && !verify_signature(&body, &config.secret, signature) {
        return text(StatusCode::UNAUTHORIZED, "invalid signature");
    }
    let event_name = header_text(headers, "x-github-event");
    if event_name != "push" {
        return json(Py::dict([
            ("accepted", Py::from(false)),
            ("reason", Py::from("ignored_event")),
        ]));
    }
    let delivery_id = header_text(headers, "x-github-delivery");
    if delivery_id.is_empty() {
        return text(StatusCode::BAD_REQUEST, "missing delivery id");
    }
    if !valid_delivery_id(&delivery_id) {
        return text(StatusCode::BAD_REQUEST, "invalid delivery id");
    }
    let payload = match std::str::from_utf8(&body)
        .map_err(|e| e.to_string())
        .and_then(|t| hermes_py::loads(t))
    {
        Ok(payload) => payload,
        Err(_) => return text(StatusCode::BAD_REQUEST, "invalid json"),
    };
    let Some(event) = SyncEvent::from_payload(&delivery_id, &event_name, payload) else {
        return text(StatusCode::BAD_REQUEST, "invalid json");
    };
    if event.repository != config.expected_repo || event.git_ref != config.expected_ref {
        return json(Py::dict([
            ("accepted", Py::from(false)),
            ("reason", Py::from("repo_or_ref_mismatch")),
            ("repository", Py::from(event.repository)),
            ("ref", Py::from(event.git_ref)),
        ]));
    }
    let queue = Arc::clone(&shared.queue);
    let queued =
        tokio::task::spawn_blocking(move || queue.enqueue(&event).map(|q| (q, event))).await;
    match queued {
        Ok(Ok((queued, event))) => json(Py::dict([
            ("accepted", Py::from(true)),
            ("queued", Py::from(queued)),
            ("delivery_id", Py::from(event.delivery_id)),
            ("after_sha", Py::from(event.after_sha)),
            ("repository", Py::from(event.repository)),
            ("ref", Py::from(event.git_ref)),
        ])),
        Ok(Err(e)) => {
            log(
                Level::Error,
                &format!("could not queue delivery {delivery_id}: {e}"),
            );
            text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not queue delivery",
            )
        }
        Err(_) => text(
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not queue delivery",
        ),
    }
}

/// The body, at most [`MAX_BODY_BYTES`], read within [`BODY_TIMEOUT`]. A
/// declared length over the cap is refused before anything is read.
async fn read_body(headers: &HeaderMap, body: Body) -> Result<Bytes, Response> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > MAX_BODY_BYTES as u64) {
        return Err(text(StatusCode::PAYLOAD_TOO_LARGE, "payload too large"));
    }
    match tokio::time::timeout(BODY_TIMEOUT, Limited::new(body, MAX_BODY_BYTES).collect()).await {
        Err(_) => Err(text(StatusCode::REQUEST_TIMEOUT, "request timeout")),
        Ok(Err(e)) if e.downcast_ref::<LengthLimitError>().is_some() => {
            Err(text(StatusCode::PAYLOAD_TOO_LARGE, "payload too large"))
        }
        Ok(Err(_)) => Err(text(StatusCode::BAD_REQUEST, "invalid body")),
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
    }
}

fn header_text(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .unwrap_or_default()
}

fn text(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message.to_string(),
    )
        .into_response()
}

/// 200 with `json.dumps(payload)`'s bytes, as the script answered.
fn json(payload: Py) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        hermes_py::dumps(&payload, None, false),
    )
        .into_response()
}

#[cfg(test)]
#[path = "listen_tests.rs"]
mod tests;
