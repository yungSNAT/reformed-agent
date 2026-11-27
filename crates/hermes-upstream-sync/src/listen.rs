//! `listen`: the GitHub webhook receiver (`serve_webhook` in the script).
//!
//! Stricter than the script, which gave every connection a thread with no
//! time limit:
//!
//! - it will not start without a secret unless told to accept unsigned
//!   deliveries on a loopback address;
//! - a request head must arrive within 10 seconds of the connection opening
//!   and its body within 10 seconds of a body slot being taken (GitHub gives
//!   up on a delivery after 10 seconds); one request per connection;
//! - at most 128 connections are open, 32 of them from any one address
//!   group (an IPv4 /24 or an IPv6 /64);
//! - a body slot is taken only once the head is read and the method, path
//!   and length pass; there are 16 slots, at most 4 held by one address
//!   group, and a request waits up to 5 seconds for one before 503;
//! - bodies are capped at GitHub's 25 MB payload limit, must declare their
//!   length and are read into one buffer of that length;
//! - a body already queued under another delivery id is not queued again;
//! - the delivery id must be file-name safe.
//!
//! The secret, headers and bodies are never logged or echoed.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use hermes_py::Py;
use hmac::{Hmac, Mac};
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Bytes, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::event::{valid_delivery_id, SyncEvent};
use crate::queue::Publisher;
use crate::{log, Level};

/// GitHub caps webhook payloads at 25 MB.
pub const MAX_BODY_BYTES: usize = 25 * 1024 * 1024;
/// GitHub ends a delivery that has had no reply for 10 seconds.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const BODY_TIMEOUT: Duration = Duration::from_secs(10);
const SLOT_WAIT: Duration = Duration::from_secs(5);
const MAX_SLOTS: usize = 16;
const MAX_PEER_SLOTS: usize = 4;
const MAX_CONNECTIONS: usize = 128;
const MAX_PEER_CONNECTIONS: usize = 32;
/// A backstop: no connection outlives this, whatever it is waiting on.
const CONNECTION_DEADLINE: Duration = Duration::from_secs(60);

type Reply = Response<Full<Bytes>>;

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

/// The addresses `--host` names, resolved once: the listener binds the first
/// of the addresses [`check_secret`] approved that it can.
pub fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve --host {host}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("could not resolve --host {host}: no addresses"));
    }
    Ok(addrs)
}

/// Refuses an empty secret unless unsigned deliveries were asked for and
/// every address is loopback. The script accepted every request unsigned
/// whenever `GITHUB_WEBHOOK_SECRET` was unset.
pub fn check_secret(config: &ListenConfig, addrs: &[SocketAddr]) -> Result<(), String> {
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
    let mut digest = [0u8; 32];
    for (byte, pair) in digest.iter_mut().zip(hex.chunks(2)) {
        *byte = (nibble(pair[0]) << 4) | nibble(pair[1]);
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&digest).is_ok()
}

/// The group a peer's limits count against: its IPv4 /24 or IPv6 /64. One
/// host can hold several neighbouring IPv4 addresses and usually a whole
/// IPv6 /64.
fn address_group(peer: IpAddr) -> IpAddr {
    match peer {
        IpAddr::V4(v4) => IpAddr::V4(Ipv4Addr::from(u32::from(v4) & 0xffff_ff00)),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => address_group(IpAddr::V4(v4)),
            None => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !u128::from(u64::MAX))),
        },
    }
}

/// One shared value per address group, alive whilst anything holds it: the
/// number of holders is what that group has open.
struct PerPeer<T>(Mutex<HashMap<IpAddr, Weak<T>>>);

impl<T> PerPeer<T> {
    fn new() -> Self {
        PerPeer(Mutex::new(HashMap::new()))
    }

    fn get(&self, peer: IpAddr, make: impl FnOnce() -> T) -> Arc<T> {
        let peer = address_group(peer);
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, held| held.strong_count() > 0);
        if let Some(held) = map.get(&peer).and_then(Weak::upgrade) {
            return held;
        }
        let held = Arc::new(make());
        map.insert(peer, Arc::downgrade(&held));
        held
    }
}

struct Shared {
    config: ListenConfig,
    queue: Arc<dyn Publisher>,
    slots: Arc<Semaphore>,
    peer_slots: PerPeer<Semaphore>,
}

impl Shared {
    /// One of the peer's group's slots, then one of all; `None` after
    /// [`SLOT_WAIT`].
    async fn take_slot(&self, peer: IpAddr) -> Option<[OwnedSemaphorePermit; 2]> {
        let own = self.peer_slots.get(peer, || Semaphore::new(MAX_PEER_SLOTS));
        let slots = Arc::clone(&self.slots);
        let wait = async move {
            let own = own.acquire_owned().await.ok()?;
            let any = slots.acquire_owned().await.ok()?;
            Some([own, any])
        };
        tokio::time::timeout(SLOT_WAIT, wait).await.ok().flatten()
    }
}

pub async fn serve(
    addrs: &[SocketAddr],
    config: ListenConfig,
    queue: Arc<dyn Publisher>,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(addrs)
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
        slots: Arc::new(Semaphore::new(MAX_SLOTS)),
        peer_slots: PerPeer::new(),
    });
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let peer_connections = PerPeer::<()>::new();
    let interrupted = tokio::signal::ctrl_c();
    tokio::pin!(interrupted);
    loop {
        // At the cap, new connections wait in the listen backlog.
        let open = tokio::select! {
            open = Arc::clone(&connections).acquire_owned() => match open {
                Ok(open) => open,
                Err(_) => return Ok(()),
            },
            _ = &mut interrupted => {
                log(Level::Info, "webhook server interrupted; exiting");
                return Ok(());
            }
        };
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(e) => {
                    // Out of file descriptors, most likely: let some close.
                    log(Level::Warning, &format!("could not accept a connection: {e}"));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            },
            _ = &mut interrupted => {
                log(Level::Info, "webhook server interrupted; exiting");
                return Ok(());
            }
        };
        let from_peer = peer_connections.get(peer.ip(), || ());
        if Arc::strong_count(&from_peer) > MAX_PEER_CONNECTIONS {
            continue; // dropping the stream closes it
        }
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let _held = (open, from_peer);
            let service = service_fn(move |request| {
                let shared = Arc::clone(&shared);
                async move { Ok::<_, Infallible>(handle(&shared, peer, request).await) }
            });
            let connection = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEAD_TIMEOUT)
                .keep_alive(false)
                .serve_connection(TokioIo::new(stream), service);
            let _ = tokio::time::timeout(CONNECTION_DEADLINE, connection).await;
        });
    }
}

async fn handle(shared: &Shared, peer: SocketAddr, request: Request<Incoming>) -> Reply {
    // The path without its query string: a query can carry anything.
    let request_line = format!(
        "{} {} {:?}",
        request.method(),
        request.uri().path(),
        request.version()
    );
    let response = respond(shared, peer.ip(), request).await;
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

async fn respond(shared: &Shared, peer: IpAddr, request: Request<Incoming>) -> Reply {
    let config = &shared.config;
    if request.method() != Method::POST {
        return text(
            StatusCode::NOT_IMPLEMENTED,
            &format!("Unsupported method ('{}')", request.method()),
        );
    }
    // The script compared the raw request target, so an absolute-form target
    // (`POST http://host/path`) never matched.
    let target = request.uri().path_and_query().map_or("", |p| p.as_str());
    if request.uri().authority().is_some() || target != config.path {
        return text(StatusCode::NOT_FOUND, "Not Found");
    }
    let (parts, body) = request.into_parts();
    let declared = match declared_length(&parts.headers) {
        Ok(declared) => declared,
        Err((status, message)) => return text(status, message),
    };
    let Some(_slot) = shared.take_slot(peer).await else {
        return text(StatusCode::SERVICE_UNAVAILABLE, "busy");
    };
    let body = match read_body(body, declared).await {
        Ok(body) => body,
        Err((status, message)) => return text(status, message),
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
        .and_then(hermes_py::loads)
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
    // The signature covers the body but not the delivery id, so a captured
    // body resent under a new id is a replay.
    let body_sha256 = format!("{:x}", Sha256::digest(&body));
    let queue = Arc::clone(&shared.queue);
    let queued = tokio::task::spawn_blocking(move || {
        queue.enqueue(&event, &body_sha256).map(|q| (q, event))
    })
    .await;
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
            log(Level::Error, &format!("could not queue a delivery: {e}"));
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

/// The declared `Content-Length`; refused when absent or over the cap, before
/// any of the body is read.
fn declared_length(headers: &HeaderMap) -> Result<usize, (StatusCode, &'static str)> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or((StatusCode::LENGTH_REQUIRED, "length required"))?;
    if declared > MAX_BODY_BYTES as u64 {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "payload too large"));
    }
    Ok(declared as usize)
}

/// The body, read within [`BODY_TIMEOUT`] into one buffer of the declared
/// length; more than [`MAX_BODY_BYTES`] is refused whilst reading.
async fn read_body<B>(mut body: B, declared: usize) -> Result<Vec<u8>, (StatusCode, &'static str)>
where
    B: Body<Data = Bytes> + Unpin,
{
    let read = async {
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| (StatusCode::BAD_REQUEST, "invalid body"))?;
            let Ok(chunk) = frame.into_data() else {
                continue; // trailers
            };
            if bytes.len() + chunk.len() > MAX_BODY_BYTES {
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "payload too large"));
            }
            if bytes.capacity() == 0 {
                bytes.reserve_exact(declared.max(chunk.len()));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    };
    tokio::time::timeout(BODY_TIMEOUT, read)
        .await
        .unwrap_or(Err((StatusCode::REQUEST_TIMEOUT, "request timeout")))
}

fn header_text(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .unwrap_or_default()
}

fn reply(status: StatusCode, content_type: &'static str, body: String) -> Reply {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn text(status: StatusCode, message: &str) -> Reply {
    reply(status, "text/plain; charset=utf-8", message.to_string())
}

/// 200 with `json.dumps(payload)`'s bytes, as the script answered.
fn json(payload: Py) -> Reply {
    reply(
        StatusCode::OK,
        "application/json",
        hermes_py::dumps(&payload, None, false),
    )
}

#[cfg(test)]
#[path = "listen_tests.rs"]
mod tests;
