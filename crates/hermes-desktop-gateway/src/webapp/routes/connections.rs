//! The machine's saved gateways and the relay that carries the Webapp's REST calls to them.
//!
//! Upstream at the pin: `hermes_cli/gateway_connections.py` (the store),
//! `hermes_cli/web_routers/gateway_connections.py` (the 4 routes) and
//! `hermes_cli/web_routers/webapp_relay_proxy.py` (the relay). The list is
//! `<root>/gateway-connections.json`, shared with native Desktop under the same `<store>.lock`
//! folder lock as the UI preferences. Only the Webapp reaches these routes; other callers get 404.
//!
//! Stricter than upstream, each on purpose:
//! - this gateway saves and relays only `remote` entries with a session token; `ssh`, `cloud`,
//!   `local` and `oauth` entries are listed and never rewritten or dialled (any but `local` may
//!   be removed, as upstream allows);
//! - a store whose list holds an entry that is not an object is copied aside, as a corrupt one
//!   is, before a write drops that entry;
//! - a saved URL may not carry a user name or password (the URL is in the public view), and a
//!   target address must not be link-local, cloud metadata, unspecified, multicast, broadcast,
//!   IPv4-mapped, zoned or this listener itself; a private or ULA address needs the entry's
//!   `allowPrivateNetwork`. Every relayed connection is pinned to the addresses checked;
//! - the public view is built from a field list, `tokenPreview` never shows the secret, and a URL
//!   is shown without the user name or password native Desktop may have saved in it;
//! - an edit that changes a gateway's address must send its token again, and saved header values
//!   and private-network consent do not follow it to the new address;
//! - only upstream's methods are relayed, only to `fetch` and XHR (`Sec-Fetch-Dest: empty`), a
//!   percent-encoded plain character in the path is refused, and a response body that stalls for
//!   `BODY_IDLE_TIMEOUT` or passes `MAX_RESPONSE` bytes ends the relayed response;
//! - error texts are fixed: no transport error (which can hold a URL) reaches a response or log.
//!
//! Unix only, as the Webapp's local mode is: the store's owner-only guard is Unix's.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as UrlPath, Request};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{json, Map, Value};

use super::store::{blocking, webapp_only, Served, StoreLock};
use crate::webapp::api::{ApiError, ApiResult, Ctx};

/// Each handler gets `served` from the route, not from the request; no request value builds a
/// path.
pub fn router(served: &Served) -> Router {
    let (list, save, last, remove, relay) = (
        served.clone(),
        served.clone(),
        served.clone(),
        served.clone(),
        served.clone(),
    );
    Router::new()
        .route(
            "/api/gateway-connections",
            get(move |ctx: Ctx| list_connections(list.clone(), ctx)).post(
                move |ctx: Ctx, body: Result<Json<Value>, JsonRejection>| {
                    save_connection(save.clone(), ctx, body)
                },
            ),
        )
        .route(
            "/api/gateway-connections/last-used",
            post(move |ctx: Ctx, body: Result<Json<Value>, JsonRejection>| {
                set_last_used(last.clone(), ctx, body)
            }),
        )
        .route(
            "/api/gateway-connections/{id}",
            delete(move |ctx: Ctx, id: UrlPath<String>| remove_connection(remove.clone(), ctx, id)),
        )
        .route(
            "/api/relay/{id}/{*path}",
            any(
                move |ctx: Ctx, id: UrlPath<(String, String)>, request: Request| {
                    relay_http(relay.clone(), ctx, id, request)
                },
            ),
        )
}

// ---- Store ----------------------------------------------------------------------------------

pub(crate) const STORE_NAME: &str = "gateway-connections.json";
const LOCAL_ID: &str = "local";
const LABEL_MAX: usize = 64;
/// Fields of an entry the browser may see. Anything else (a token, a header value, a field a
/// later Desktop adds) stays on the server.
const PUBLIC_FIELDS: [&str; 14] = [
    "id",
    "kind",
    "label",
    "url",
    "authMode",
    "name",
    "org",
    "host",
    "user",
    "port",
    "keyPath",
    "remoteHermesPath",
    "remoteProfile",
    "allowPrivateNetwork",
];
/// Saved header names that would let an entry rewrite the request itself or carry a credential
/// that has its own field (upstream's list, plus hop-by-hop names and `proxy-*`).
const FORBIDDEN_SAVED_HEADERS: [&str; 16] = [
    "authorization",
    "connection",
    "content-length",
    "content-type",
    "cookie",
    "host",
    "origin",
    "referer",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "x-hermes-session-token",
    "keep-alive",
    "forwarded",
    "proxy-connection",
];

#[derive(Debug, PartialEq)]
pub(crate) enum StoreError {
    /// 400: the save or change is refused.
    Refused(String),
    /// 404: no saved connection has that id.
    Missing(String),
    /// 503: the store is locked, unreadable or not this user's.
    Busy(String),
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::Refused(m) => ApiError(StatusCode::BAD_REQUEST, m),
            StoreError::Missing(m) => ApiError(StatusCode::NOT_FOUND, m),
            StoreError::Busy(m) => ApiError(StatusCode::SERVICE_UNAVAILABLE, m),
        }
    }
}

fn refused(message: impl Into<String>) -> StoreError {
    StoreError::Refused(message.into())
}

/// The document as stored (`None` for no file) and whether an existing file failed to parse.
/// Any read failure other than a missing file refuses, as upstream: it says nothing about the
/// content. A file owned by another user is refused; one others may read is closed to 0600
/// before its secrets are used.
pub(crate) fn read_doc(path: &Path) -> Result<(Option<Map<String, Value>>, bool), StoreError> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((None, false)),
        Err(_) => {
            return Err(StoreError::Busy(
                "gateway connections store is unreadable".into(),
            ))
        }
    };
    guard_permissions(path)?;
    let text = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&raw);
    match serde_json::from_slice::<Value>(text) {
        Ok(Value::Object(doc)) => Ok((Some(doc), false)),
        _ => Ok((None, true)),
    }
}

fn guard_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| StoreError::Busy("gateway connections store is unreadable".into()))?;
    // SAFETY: getuid has no preconditions and cannot fail.
    if !meta.file_type().is_file() || meta.uid() != unsafe { libc::getuid() } {
        return Err(StoreError::Busy(
            "gateway connections store is not a file this user owns".into(),
        ));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| StoreError::Busy("gateway connections store is unreadable".into()))?;
    }
    Ok(())
}

fn revision(doc: Option<&Map<String, Value>>) -> u64 {
    doc.and_then(|d| d.get("revision"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn connections(doc: Option<&Map<String, Value>>) -> Vec<Map<String, Value>> {
    let listed = doc
        .and_then(|d| d.get("connections"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|c| c.as_object().cloned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if listed.iter().any(|c| str_field(c, "kind") == Some("local")) {
        listed
    } else {
        let mut with_local = vec![local_entry()];
        with_local.extend(listed);
        with_local
    }
}

fn local_entry() -> Map<String, Value> {
    let Value::Object(entry) = json!({"id": LOCAL_ID, "kind": "local", "label": "This device"})
    else {
        unreachable!("a JSON object literal");
    };
    entry
}

fn str_field<'a>(entry: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

/// The plaintext of a stored secret envelope; a `safeStorage` blob only Desktop's keychain opens
/// reads as unset.
fn plain(secret: Option<&Value>) -> Option<&str> {
    let value = match secret? {
        Value::String(s) => s.as_str(),
        Value::Object(env) if str_field(env, "encoding") != Some("safeStorage") => {
            str_field(env, "value")?
        }
        _ => return None,
    };
    (!value.is_empty()).then_some(value)
}

fn envelope(value: &str) -> Value {
    json!({"encoding": "plain", "value": value})
}

fn public_connection(entry: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for field in PUBLIC_FIELDS {
        if let Some(value @ (Value::String(_) | Value::Number(_) | Value::Bool(_))) =
            entry.get(field)
        {
            out.insert(field.to_owned(), value.clone());
        }
    }
    // Native Desktop keeps a user name and password in a saved URL; the browser never sees them.
    // Only an `http` or `https` URL is shown: `user:pw@host` parses with `user` as its scheme.
    match str_field(entry, "url").map(url::Url::parse) {
        Some(Ok(mut url)) if matches!(url.scheme(), "http" | "https") => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            out.insert("url".into(), json!(stored_url(&url)));
        }
        _ => {
            out.remove("url");
        }
    }
    let token_set = plain(entry.get("token")).is_some();
    out.insert("tokenSet".into(), json!(token_set));
    out.insert(
        "tokenPreview".into(),
        if token_set { json!("set") } else { Value::Null },
    );
    let names: Vec<&String> = entry
        .get("headers")
        .and_then(Value::as_object)
        .map(|h| h.keys().collect())
        .unwrap_or_default();
    out.insert("headerNames".into(), json!(names));
    Value::Object(out)
}

/// Native Desktop's `sanitizeConnectionsRegistry` shape: never a token or header value.
pub(crate) fn public_registry(doc: Option<&Map<String, Value>>) -> Value {
    let list = connections(doc);
    let ids: HashSet<&str> = list.iter().filter_map(|c| str_field(c, "id")).collect();
    let named = |key: &str| {
        doc.and_then(|d| str_field(d, key))
            .filter(|id| ids.contains(id))
            .unwrap_or(LOCAL_ID)
            .to_owned()
    };
    let primary = named("primary");
    let last_used = doc
        .and_then(|d| str_field(d, "lastUsed"))
        .filter(|id| ids.contains(id))
        .map_or_else(|| primary.clone(), str::to_owned);
    let launch_mode = match doc.and_then(|d| str_field(d, "launchMode")) {
        Some("last-used") => "last-used",
        _ => "primary",
    };
    let quarantined: Vec<Value> = doc
        .and_then(|d| d.get("quarantined"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_object)
                .map(|q| {
                    let label = q
                        .get("entry")
                        .and_then(Value::as_object)
                        .and_then(|e| str_field(e, "label"))
                        .unwrap_or("");
                    json!({"reason": str_field(q, "reason").unwrap_or("unknown"), "label": label})
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "version": 2,
        "primary": primary,
        "launchMode": launch_mode,
        "lastUsed": last_used,
        "secureTokenStorage": true,
        "connections": list.iter().map(public_connection).collect::<Vec<_>>(),
        "quarantined": quarantined,
    })
}

/// Replaces the store whole: a temporary file of mode 0600 in the same folder, then a rename.
fn write_doc(path: &Path, doc: &Map<String, Value>, corrupt: bool) -> Result<(), StoreError> {
    let busy = |_| StoreError::Busy("gateway connections store could not be written".into());
    if corrupt {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let mut aside = path.as_os_str().to_owned();
        aside.push(format!(".corrupt-{millis}"));
        std::fs::copy(path, PathBuf::from(aside)).map_err(busy)?;
    }
    let bytes = serde_json::to_vec_pretty(doc).map_err(|_| refused("unserialisable store"))?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".tmp-{}", uuid::Uuid::new_v4().simple()));
    let temp = PathBuf::from(temp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&temp).and_then(|mut file| {
        use std::io::Write;
        file.write_all(&bytes)?;
        file.sync_all()
    });
    let renamed = written.and_then(|()| std::fs::rename(&temp, path));
    if renamed.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    renamed.map_err(busy)
}

/// Applies `change` to the fresh document under the store lock and lands it. `change` sees the
/// document's entries (with the `local` entry) and returns what the route answers with. Keys the
/// change does not touch, unknown keys included, are written back as they were.
pub(crate) fn mutate<T>(
    path: &Path,
    change: impl FnOnce(&mut Map<String, Value>, &mut Vec<Map<String, Value>>) -> Result<T, StoreError>,
) -> Result<(Map<String, Value>, T), StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| StoreError::Busy("gateway connections store is unreadable".into()))?;
    }
    let lock = StoreLock::take(path, "gateway connections store").map_err(StoreError::Busy)?;
    let (doc, corrupt) = read_doc(path)?;
    // The rewritten list holds only entries that are objects; any other is kept in a copy.
    let unreadable_entry = doc
        .as_ref()
        .and_then(|d| d.get("connections"))
        .is_some_and(|c| !c.as_array().is_some_and(|l| l.iter().all(Value::is_object)));
    let mut list = connections(doc.as_ref());
    let mut next = doc.clone().unwrap_or_default();
    let answer = change(&mut next, &mut list)?;
    next.insert(
        "connections".into(),
        Value::Array(list.into_iter().map(Value::Object).collect()),
    );
    next.insert("version".into(), json!(2));
    let unchanged = doc.as_ref().is_some_and(|d| {
        let mut before = d.clone();
        before.insert("version".into(), json!(2));
        before == next
    });
    if !unchanged {
        next.insert("revision".into(), json!(revision(doc.as_ref()) + 1));
        if !lock.held() {
            return Err(StoreError::Busy(
                "gateway connections store lock was taken over".into(),
            ));
        }
        write_doc(path, &next, corrupt || unreadable_entry)?;
    }
    Ok((next, answer))
}

pub(crate) fn find<'a>(list: &'a [Map<String, Value>], id: &str) -> Option<&'a Map<String, Value>> {
    list.iter().find(|c| str_field(c, "id") == Some(id))
}

fn missing(id: &str) -> StoreError {
    StoreError::Missing(format!("No connection with id \"{id}\"."))
}

/// Whether this gateway saves and relays `entry`: a `remote` gateway with a session token.
fn relayable(entry: &Map<String, Value>) -> bool {
    str_field(entry, "kind") == Some("remote") && str_field(entry, "authMode") != Some("oauth")
}

/// Upstream's `normalizeRemoteBaseUrl`, refusing a user name or password in the URL.
pub(crate) fn normalize_url(raw: &str) -> Result<url::Url, StoreError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(refused("Remote gateway URL is required."));
    }
    let has_scheme = trimmed.split_once("://").is_some_and(|(s, _)| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
    });
    let with_scheme = if has_scheme {
        trimmed.to_owned()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = url::Url::parse(&with_scheme)
        .map_err(|_| refused("Remote gateway URL is not valid: Invalid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(refused(format!(
            "Remote gateway URL must be http:// or https://, got {}:",
            url.scheme()
        )));
    }
    if url.host().is_none() {
        return Err(refused("Remote gateway URL is not valid: Invalid URL"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refused(
            "Remote gateway URL must not hold a user name or password; save the session token instead.",
        ));
    }
    if let Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) = url.host() {
        let ip = literal_ip(&url).expect("an IP host");
        if let Err(reason) = judge_address(ip, true) {
            return Err(refused(format!("Remote gateway URL is refused: {reason}.")));
        }
    }
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url)
}

fn literal_ip(url: &url::Url) -> Option<IpAddr> {
    match url.host()? {
        url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        url::Host::Domain(_) => None,
    }
}

/// The base URL as stored: no trailing slash, as upstream writes it.
fn stored_url(url: &url::Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
}

/// Whether a relay may dial `ip`. Loopback is allowed (another gateway on this machine); private
/// and unique-local addresses only with the entry's consent.
pub(crate) fn judge_address(ip: IpAddr, allow_private: bool) -> Result<(), &'static str> {
    const METADATA_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast() {
                return Err("not a gateway address");
            }
            if v4.is_link_local() {
                return Err("a link-local address (cloud metadata lives there)");
            }
            if v4 == std::net::Ipv4Addr::new(168, 63, 129, 16) {
                return Err("a cloud platform address");
            }
            if v4.is_private() || (v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64) {
                return if allow_private {
                    Ok(())
                } else {
                    Err("a private network address; allow private networks for this gateway first")
                };
            }
            Ok(())
        }
        IpAddr::V6(v6) => {
            if v6.to_ipv4_mapped().is_some() {
                return Err("an IPv4-mapped address; use the IPv4 address");
            }
            let s = v6.segments();
            // NAT64 (64:ff9b::/96) reaches the IPv4 address it wraps: judge that one.
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [a, b] = s[6].to_be_bytes();
                let [c, d] = s[7].to_be_bytes();
                return judge_address(IpAddr::V4([a, b, c, d].into()), allow_private);
            }
            // IPv4-compatible (::a.b.c.d) and site-local (fec0::/10): both deprecated.
            if (s[..6] == [0; 6] && !v6.is_loopback() && !v6.is_unspecified())
                || s[0] & 0xffc0 == 0xfec0
            {
                return Err("a deprecated address form");
            }
            if v6.is_unspecified() || v6.is_multicast() {
                return Err("not a gateway address");
            }
            if v6 == METADATA_V6 {
                return Err("a cloud metadata address");
            }
            if v6.segments()[0] & 0xffc0 == 0xfe80 {
                return Err("a link-local address");
            }
            if v6.segments()[0] & 0xfe00 == 0xfc00 {
                return if allow_private {
                    Ok(())
                } else {
                    Err("a private network address; allow private networks for this gateway first")
                };
            }
            Ok(())
        }
    }
}

fn label_key(label: &str) -> String {
    label.trim().to_lowercase()
}

fn label_slug(label: &str) -> String {
    let mut slug = String::new();
    for c in label.trim().to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(48).collect();
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        "connection".into()
    } else {
        slug
    }
}

fn new_id(label: &str, list: &[Map<String, Value>]) -> String {
    let taken: HashSet<&str> = list.iter().filter_map(|c| str_field(c, "id")).collect();
    let base = label_slug(label);
    if !taken.contains(base.as_str()) && base != LOCAL_ID {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken.contains(id.as_str()) && id != LOCAL_ID)
        .expect("an unused id")
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+.^_`|~-".contains(&b))
        && !FORBIDDEN_SAVED_HEADERS.contains(&name.to_ascii_lowercase().as_str())
        && !name.to_ascii_lowercase().starts_with("proxy-")
}

/// A header value of visible characters, spaces and tabs only: no control character.
fn valid_header_value(value: &str) -> bool {
    !value.chars().any(|c| c.is_control() && c != '\t')
}

/// Upstream's `encryptIncomingRemoteHeaders`: a string is a new value, `null` keeps the stored
/// one, a name missing from `raw` is cleared. Names and values are checked, not repaired.
fn incoming_headers(
    raw: &Map<String, Value>,
    stored: Option<&Map<String, Value>>,
) -> Result<Map<String, Value>, StoreError> {
    let mut out = Map::new();
    for (name, value) in raw {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        if !valid_header_name(name) {
            return Err(refused(format!(
                "Header \"{name}\" cannot be saved for a gateway."
            )));
        }
        match value {
            Value::String(v) if v.trim().is_empty() => {}
            Value::String(v) if valid_header_value(v) => {
                out.insert(name.to_owned(), envelope(v.trim()));
            }
            Value::String(_) => {
                return Err(refused(format!(
                    "Header \"{name}\" holds a control character."
                )));
            }
            Value::Null => {
                if let Some(kept) = stored.and_then(|s| s.get(name)) {
                    out.insert(name.to_owned(), kept.clone());
                }
            }
            _ => {
                return Err(refused(format!(
                    "Header \"{name}\" must be a string or null."
                )))
            }
        }
    }
    Ok(out)
}

/// The stored document and one entry of it.
type DocAndEntry = (Map<String, Value>, Map<String, Value>);

/// Creates or edits one remote gateway. Returns the document and the saved entry.
pub(crate) fn save(path: &Path, payload: &Map<String, Value>) -> Result<DocAndEntry, StoreError> {
    let (doc, entry) = mutate(path, |_doc, list| {
        let id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let existing = id.and_then(|id| find(list, id)).cloned();
        if let Some(existing) = &existing {
            if !relayable(existing) {
                return Err(refused(
                    "This gateway edits remote gateways with a session token only.",
                ));
            }
        }
        let kind = payload
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("remote");
        let auth_mode = payload
            .get("authMode")
            .and_then(Value::as_str)
            .unwrap_or("token");
        if kind != "remote" || auth_mode != "token" {
            return Err(refused(
                "This gateway saves remote gateways with a session token only.",
            ));
        }
        let raw_url = payload
            .get("url")
            .and_then(Value::as_str)
            .or_else(|| existing.as_ref().and_then(|e| str_field(e, "url")))
            .unwrap_or("");
        let url = normalize_url(raw_url)?;
        let stored = stored_url(&url);
        // A secret saved for one address never follows the entry to another: a move sends its
        // token again, and saved header values and private-network consent stay behind.
        let kept = existing.as_ref().filter(|e| {
            str_field(e, "url")
                .and_then(|u| normalize_url(u).ok())
                .is_some_and(|u| stored_url(&u) == stored)
        });
        let label = payload
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                existing
                    .as_ref()
                    .and_then(|e| str_field(e, "label"))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| url.host_str().unwrap_or("remote").to_owned());
        if label.chars().count() > LABEL_MAX {
            return Err(refused(format!(
                "A gateway name is at most {LABEL_MAX} characters."
            )));
        }
        let own_id = existing
            .as_ref()
            .and_then(|e| str_field(e, "id"))
            .map(str::to_owned);
        for other in list
            .iter()
            .filter(|c| str_field(c, "id") != own_id.as_deref())
        {
            if str_field(other, "label").is_some_and(|l| label_key(l) == label_key(&label)) {
                return Err(refused(format!(
                    "A gateway named \"{label}\" is already saved."
                )));
            }
            if str_field(other, "url").is_some_and(|u| u.trim_end_matches('/') == stored) {
                return Err(refused("A gateway with this address is already saved."));
            }
        }
        let token = match payload.get("token").and_then(Value::as_str).map(str::trim) {
            Some(t) if !t.is_empty() => Some(envelope(t)),
            _ => kept.and_then(|e| e.get("token")).cloned(),
        };
        if plain(token.as_ref()).is_none() {
            return Err(refused(if existing.is_some() && kept.is_none() {
                "A gateway moved to another address needs its session token again."
            } else {
                "Remote gateway session token is required."
            }));
        }
        let headers = match payload.get("headers") {
            Some(Value::Object(raw)) => incoming_headers(
                raw,
                kept.and_then(|e| e.get("headers"))
                    .and_then(Value::as_object),
            )?,
            Some(Value::Null) | None => kept
                .and_then(|e| e.get("headers"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            Some(_) => return Err(refused("Gateway headers must be an object.")),
        };
        let allow_private = match payload.get("allowPrivateNetwork") {
            Some(Value::Bool(b)) => *b,
            None | Some(Value::Null) => kept
                .and_then(|e| e.get("allowPrivateNetwork"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            Some(_) => return Err(refused("allowPrivateNetwork must be true or false.")),
        };
        // A move starts a fresh entry: no field kept for the old address follows it.
        let mut entry = kept.cloned().unwrap_or_default();
        let id = own_id.unwrap_or_else(|| new_id(&label, list));
        entry.insert("id".into(), json!(id));
        entry.insert("kind".into(), json!("remote"));
        entry.insert("label".into(), json!(label));
        entry.insert("url".into(), json!(stored));
        entry.insert("authMode".into(), json!("token"));
        entry.insert("token".into(), token.expect("checked above"));
        if headers.is_empty() {
            entry.remove("headers");
        } else {
            entry.insert("headers".into(), Value::Object(headers));
        }
        entry.insert("allowPrivateNetwork".into(), json!(allow_private));
        match list
            .iter_mut()
            .find(|c| str_field(c, "id") == Some(id.as_str()))
        {
            Some(slot) => *slot = entry.clone(),
            None => list.push(entry.clone()),
        }
        Ok(entry)
    })?;
    Ok((doc, entry))
}

pub(crate) fn remove(path: &Path, id: &str) -> Result<Map<String, Value>, StoreError> {
    mutate(path, |doc, list| {
        let entry = find(list, id).ok_or_else(|| missing(id))?;
        if str_field(entry, "kind") == Some("local") {
            return Err(refused("The local connection cannot be removed."));
        }
        list.retain(|c| str_field(c, "id") != Some(id));
        for key in ["primary", "lastUsed"] {
            if doc.get(key).and_then(Value::as_str) == Some(id) {
                doc.insert(key.into(), json!(LOCAL_ID));
            }
        }
        Ok(())
    })
    .map(|(doc, ())| doc)
}

pub(crate) fn last_used(path: &Path, id: &str) -> Result<Map<String, Value>, StoreError> {
    mutate(path, |doc, list| {
        find(list, id).ok_or_else(|| missing(id))?;
        doc.insert("lastUsed".into(), json!(id));
        Ok(())
    })
    .map(|(doc, ())| doc)
}

// ---- Registry routes ------------------------------------------------------------------------

/// Resolved per request, as the start-up routes resolve theirs, so both stores follow the same
/// install root.
fn store_path(served: &Served) -> PathBuf {
    served.install_root().join(STORE_NAME)
}

fn answer(doc: &Map<String, Value>, extra: Option<(&str, Value)>) -> Value {
    let mut out = json!({"ok": true, "registry": public_registry(Some(doc))});
    if let Some((key, value)) = extra {
        out[key] = value;
    }
    out
}

async fn list_connections(served: Served, ctx: Ctx) -> ApiResult {
    webapp_only(&ctx)?;
    let path = store_path(&served);
    let doc = blocking(move || read_doc(&path).map(|(doc, _)| doc).map_err(ApiError::from)).await?;
    Ok(Json(public_registry(doc.as_ref())))
}

/// The body is parsed by axum's JSON extractor; any rejection is this route's own 400.
async fn save_connection(
    served: Served,
    ctx: Ctx,
    body: Result<Json<Value>, JsonRejection>,
) -> ApiResult {
    webapp_only(&ctx)?;
    let Ok(Json(Value::Object(payload))) = body else {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "A connection save needs a JSON object.".into(),
        ));
    };
    let path = store_path(&served);
    let (doc, entry) = blocking(move || save(&path, &payload).map_err(ApiError::from)).await?;
    Ok(Json(answer(
        &doc,
        Some(("connection", public_connection(&entry))),
    )))
}

async fn remove_connection(served: Served, ctx: Ctx, UrlPath(id): UrlPath<String>) -> ApiResult {
    webapp_only(&ctx)?;
    let path = store_path(&served);
    let doc = blocking(move || remove(&path, &id).map_err(ApiError::from)).await?;
    Ok(Json(answer(&doc, None)))
}

async fn set_last_used(
    served: Served,
    ctx: Ctx,
    body: Result<Json<Value>, JsonRejection>,
) -> ApiResult {
    webapp_only(&ctx)?;
    let id = body
        .ok()
        .and_then(|Json(b)| b.get("id").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let path = store_path(&served);
    let doc = blocking(move || last_used(&path, &id).map_err(ApiError::from)).await?;
    Ok(Json(answer(&doc, None)))
}

// ---- Relay ----------------------------------------------------------------------------------

const PREFIX: &str = "/api/relay/";
/// The most a relayed request body may carry: the largest frame upstream's gateway accepts.
pub(crate) const MAX_BODY: usize = 384 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// From the request's start until the gateway's headers arrive, the request body's upload
/// included: an upload slower than this fails with 502.
pub(crate) const HEADERS_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the gateway may go without sending a byte, its headers or its body, before the
/// relayed response ends.
pub(crate) const BODY_IDLE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(1)
} else {
    HEADERS_TIMEOUT
};
/// The most a relayed response body may carry.
pub(crate) const MAX_RESPONSE: usize = if cfg!(test) { 64 * 1024 } else { MAX_BODY };
/// Upstream's relayed methods; any other answers 405 (`TRACE` would echo the saved secrets).
const RELAYED_METHODS: [Method; 7] = [
    Method::GET,
    Method::HEAD,
    Method::POST,
    Method::PUT,
    Method::PATCH,
    Method::DELETE,
    Method::OPTIONS,
];
/// Request headers that never cross: hop-by-hop, this gateway's credentials and the browser's
/// provenance. Names starting `x-forwarded-`, `proxy-` or `sec-websocket-` and names the request's
/// `Connection` header lists are dropped as well.
const DROP_REQUEST: [&str; 18] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "cookie",
    "authorization",
    "origin",
    "referer",
    "x-hermes-session-token",
    "forwarded",
    "x-real-ip",
    "x-hermes-gateway-ticket",
    "x-hermes-window-ticket",
];
/// Response headers that cross back: what describes the body, nothing that acts on this origin.
const KEEP_RESPONSE: [&str; 9] = [
    "content-type",
    "content-length",
    "content-encoding",
    "content-language",
    "content-disposition",
    "content-range",
    "accept-ranges",
    "vary",
    "retry-after",
];
const SESSION_TOKEN_HEADER: &str = "x-hermes-session-token";

fn refusal(error: &str, id: &str, detail: &str, status: StatusCode) -> Response {
    (
        status,
        Json(json!({"error": error, "connection_id": id, "detail": detail})),
    )
        .into_response()
}

fn detail(status: StatusCode, text: &str) -> Response {
    ApiError(status, text.to_owned()).into_response()
}

/// The path and query a relayed request names on the gateway, or `None` when it is not one this
/// relay carries: not under the prefix, a second relay hop, a dot segment in any spelling, an
/// encoded slash or backslash, a percent-encoded plain character (`%61pi` would hide a second hop
/// from a gateway that decodes before routing), a backslash or a control character.
pub(crate) fn relay_target(raw_path: &str, query: Option<&str>, id: &str) -> Option<String> {
    let rest = raw_path.strip_prefix(PREFIX)?.strip_prefix(id)?;
    if encodes_a_plain_character(rest) {
        return None;
    }
    let rest = if rest.is_empty() { "/" } else { rest };
    if !rest.starts_with('/') || rest.starts_with("//") || rest.starts_with(PREFIX) {
        return None;
    }
    let lower = rest.to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") || lower.contains('\\') {
        return None;
    }
    let dot = |s: &str| matches!(s, "." | ".." | "%2e" | ".%2e" | "%2e." | "%2e%2e");
    if lower.split('/').any(dot) || rest.chars().any(|c| c.is_control() || c == ' ') {
        return None;
    }
    if lower.starts_with("/api/relay/") || lower == "/api/relay" {
        return None;
    }
    match query {
        Some(q) if !q.is_empty() => {
            if q.chars().any(|c| c.is_control()) {
                return None;
            }
            Some(format!("{rest}?{q}"))
        }
        _ => Some(rest.to_owned()),
    }
}

/// Whether `path` percent-encodes an unreserved character (a letter, digit, `-`, `.`, `_` or
/// `~`), which no client needs to encode.
fn encodes_a_plain_character(path: &str) -> bool {
    path.as_bytes().windows(3).any(|w| {
        w[0] == b'%'
            && std::str::from_utf8(&w[1..])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .is_some_and(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
    })
}

/// Resolves `dot` segments of an absolute path as a browser does.
fn without_dot_segments(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    let mut kept: Vec<&str> = Vec::new();
    for (i, segment) in segments.iter().enumerate() {
        let lower = segment.to_ascii_lowercase();
        let is_up = matches!(lower.as_str(), ".." | ".%2e" | "%2e." | "%2e%2e");
        let is_here = matches!(lower.as_str(), "." | "%2e");
        if is_up {
            kept.pop();
        } else if !is_here {
            kept.push(segment);
            continue;
        }
        if i == segments.len() - 1 {
            kept.push("");
        }
    }
    format!("/{}", kept.join("/"))
}

/// Where a gateway's redirect points, under this relay; `None` when it leaves the gateway.
pub(crate) fn relayed_location(value: &str, gateway_url: &str, base: &str) -> Option<String> {
    let value = match value.strip_prefix(gateway_url) {
        Some("") => "/",
        Some(rest) if rest.starts_with('/') => rest,
        _ => value,
    };
    if !value.starts_with('/')
        || value.starts_with("//")
        || value.contains('\\')
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let end = value.find(['?', '#']).unwrap_or(value.len());
    Some(format!(
        "{base}{}{}",
        without_dot_segments(&value[..end]),
        &value[end..]
    ))
}

/// Every address `host` names, each judged; refused when any one is refused (no mixed answers),
/// when it names this listener, or when nothing resolves.
pub(crate) async fn checked_addresses(
    url: &url::Url,
    allow_private: bool,
    own: Option<SocketAddr>,
) -> Result<Vec<SocketAddr>, &'static str> {
    let port = url.port_or_known_default().ok_or("no port")?;
    let addrs: Vec<SocketAddr> = match url.host() {
        Some(url::Host::Ipv4(ip)) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        Some(url::Host::Ipv6(ip)) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        Some(url::Host::Domain(name)) => {
            tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::lookup_host((name, port)))
                .await
                .map_err(|_| "the name does not resolve in time")?
                .map_err(|_| "the name does not resolve")?
                .collect()
        }
        None => return Err("no host"),
    };
    if addrs.is_empty() {
        return Err("the name does not resolve");
    }
    for addr in &addrs {
        if let SocketAddr::V6(v6) = addr {
            if v6.scope_id() != 0 {
                return Err("a zoned IPv6 address");
            }
        }
        judge_address(addr.ip(), allow_private)?;
        if own.is_some_and(|own| addr.port() == own.port() && addr.ip().is_loopback()) {
            return Err("this gateway itself");
        }
    }
    Ok(addrs)
}

/// What one relayed call needs from the store, read once.
pub(crate) struct Dial {
    pub label: String,
    pub base: url::Url,
    pub token: String,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub allow_private: bool,
}

pub(crate) enum DialError {
    Missing,
    NotRelayable(String),
    Store(StoreError),
}

/// The saved entry `id` as a dial, its headers revalidated (native Desktop may have written any).
pub(crate) fn dial_of(path: &Path, id: &str) -> Result<Dial, DialError> {
    let (doc, _) = read_doc(path).map_err(DialError::Store)?;
    let list = connections(doc.as_ref());
    let entry = find(&list, id).ok_or(DialError::Missing)?;
    let label = str_field(entry, "label").unwrap_or(id).to_owned();
    if !relayable(entry) {
        return Err(DialError::NotRelayable(format!(
            "{label} is not a remote gateway with a session token, which is all this gateway relays."
        )));
    }
    let base = str_field(entry, "url")
        .and_then(|u| normalize_url(u).ok())
        .ok_or_else(|| {
            DialError::NotRelayable(format!(
                "{label} has no valid address. Edit it in Settings → Gateways."
            ))
        })?;
    let token = plain(entry.get("token"))
        .ok_or_else(|| {
            DialError::NotRelayable(format!(
                "{label} has no saved credential. Edit it in Settings → Gateways."
            ))
        })?
        .to_owned();
    let mut headers = Vec::new();
    for (name, secret) in entry
        .get("headers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let value = plain(Some(secret)).unwrap_or("");
        let parsed = (valid_header_name(name) && valid_header_value(value))
            .then(|| {
                Some((
                    HeaderName::try_from(name.as_str()).ok()?,
                    HeaderValue::from_str(value).ok()?,
                ))
            })
            .flatten();
        match parsed {
            Some(pair) if !value.is_empty() => headers.push(pair),
            Some(_) => {}
            None => {
                return Err(DialError::NotRelayable(format!(
                    "{label} has a saved header this gateway will not send. Edit it in Settings → Gateways."
                )))
            }
        }
    }
    let allow_private = entry
        .get("allowPrivateNetwork")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(Dial {
        label,
        base,
        token,
        headers,
        allow_private,
    })
}

fn forwarded_headers(incoming: &HeaderMap, dial: &Dial) -> HeaderMap {
    let nominated: HashSet<String> = incoming
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|n| n.trim().to_ascii_lowercase())
        .collect();
    let mut out = HeaderMap::new();
    for (name, value) in incoming {
        let lower = name.as_str();
        if DROP_REQUEST.contains(&lower)
            || nominated.contains(lower)
            || lower.starts_with("x-forwarded-")
            || lower.starts_with("proxy-")
            || lower.starts_with("sec-websocket-")
        {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    for (name, value) in &dial.headers {
        out.insert(name.clone(), value.clone());
    }
    if let Ok(token) = HeaderValue::from_str(&dial.token) {
        out.insert(HeaderName::from_static(SESSION_TOKEN_HEADER), token);
    }
    out
}

/// Sends `request` to saved gateway `id` and answers with what it returns, under the relay's
/// header and redirect rules. `own` is this listener's address, which the relay never dials.
pub(crate) async fn relay(
    store: &Path,
    id: &str,
    own: Option<SocketAddr>,
    request: Request,
) -> Response {
    if !RELAYED_METHODS.contains(request.method()) {
        return detail(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }
    // Only `fetch` and XHR: a relayed script, style or frame would run under this origin.
    if request.headers().get_all("sec-fetch-dest").iter().any(|v| {
        !v.to_str()
            .is_ok_and(|v| v.trim().eq_ignore_ascii_case("empty"))
    }) {
        return detail(
            StatusCode::FORBIDDEN,
            "Relayed requests must come from this Webapp's scripts.",
        );
    }
    if request.headers().get_all("sec-fetch-site").iter().any(|v| {
        !v.to_str()
            .is_ok_and(|v| v.trim().eq_ignore_ascii_case("same-origin"))
    }) {
        return detail(
            StatusCode::FORBIDDEN,
            "Relayed requests must come from this Webapp's own page.",
        );
    }
    let dial = {
        let (store, id) = (store.to_owned(), id.to_owned());
        tokio::task::spawn_blocking(move || dial_of(&store, &id)).await
    };
    let dial = match dial {
        Ok(Ok(dial)) => dial,
        Ok(Err(DialError::Missing)) => return detail(StatusCode::NOT_FOUND, "No such connection"),
        Ok(Err(DialError::NotRelayable(why))) => {
            return refusal("relay_reconnect", id, &why, StatusCode::CONFLICT)
        }
        Ok(Err(DialError::Store(err))) => return ApiError::from(err).into_response(),
        Err(_) => return detail(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
    };
    let Some(target) = relay_target(request.uri().path(), request.uri().query(), id) else {
        return detail(StatusCode::BAD_REQUEST, "Not a route this relay carries");
    };
    let addrs = match checked_addresses(&dial.base, dial.allow_private, own).await {
        Ok(addrs) => addrs,
        Err(why) => {
            return refusal(
                "relay_refused",
                id,
                &format!("{} is refused: {why}.", dial.label),
                StatusCode::FORBIDDEN,
            )
        }
    };
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(BODY_IDLE_TIMEOUT)
        .pool_max_idle_per_host(0);
    if let Some(name) = dial.base.domain() {
        builder = builder.resolve_to_addrs(name, &addrs);
    }
    let Ok(client) = builder.build() else {
        return detail(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
    };
    let base = stored_url(&dial.base);
    let method = request.method().clone();
    let headers = forwarded_headers(request.headers(), &dial);
    let has_body = !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS)
        || request
            .headers()
            .get(header::CONTENT_LENGTH)
            .is_some_and(|v| v.as_bytes() != b"0");
    let mut outbound = client
        .request(method, format!("{base}{target}"))
        .headers(headers);
    if has_body {
        let mut seen = 0usize;
        let limited = request.into_body().into_data_stream().map(move |chunk| {
            let chunk = chunk.map_err(std::io::Error::other)?;
            seen += chunk.len();
            if seen > MAX_BODY {
                return Err(std::io::Error::other("relayed body too large"));
            }
            Ok::<_, std::io::Error>(chunk)
        });
        outbound = outbound.body(reqwest::Body::wrap_stream(limited));
    }
    let sent = tokio::time::timeout(HEADERS_TIMEOUT, outbound.send()).await;
    let response = match sent {
        Ok(Ok(response)) => response,
        Ok(Err(_)) | Err(_) => {
            tracing::info!(connection = %id, "gateway relay failed");
            return refusal(
                "relay_unreachable",
                id,
                &format!("{} is not reachable from this gateway.", dial.label),
                StatusCode::BAD_GATEWAY,
            );
        }
    };
    if response.status() == StatusCode::UNAUTHORIZED {
        return refusal(
            "relay_reconnect",
            id,
            &format!(
                "{} no longer accepts the saved credential. Update it in Settings → Gateways.",
                dial.label
            ),
            StatusCode::CONFLICT,
        );
    }
    let mut out = HeaderMap::new();
    for (name, value) in response.headers() {
        if KEEP_RESPONSE.contains(&name.as_str()) {
            out.append(name.clone(), value.clone());
        }
    }
    if let Some(location) = response.headers().get(header::LOCATION) {
        let rewritten = location
            .to_str()
            .ok()
            .and_then(|l| relayed_location(l, &base, &format!("{PREFIX}{id}")))
            .and_then(|l| HeaderValue::from_str(&l).ok());
        match rewritten {
            Some(value) => {
                out.insert(header::LOCATION, value);
            }
            None => {
                return refusal(
                    "relay_redirect",
                    id,
                    &format!("{} redirected outside itself.", dial.label),
                    StatusCode::BAD_GATEWAY,
                )
            }
        }
    }
    out.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    out.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    out.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let status = response.status();
    let mut seen = 0usize;
    let body = Body::from_stream(response.bytes_stream().map(move |chunk| {
        let chunk = chunk.map_err(|_| std::io::Error::other("relayed response failed"))?;
        seen += chunk.len();
        if seen > MAX_RESPONSE {
            return Err(std::io::Error::other("relayed response too large"));
        }
        Ok::<_, std::io::Error>(chunk)
    }));
    let mut relayed = Response::new(body);
    *relayed.status_mut() = status;
    *relayed.headers_mut() = out;
    relayed
}

/// This listener's own address as the browser named it (the guard already checked it is
/// loopback): the relay never dials it.
fn own_address(headers: &HeaderMap) -> Option<SocketAddr> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let port = host.rsplit_once(':')?.1.parse().ok()?;
    Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port))
}

async fn relay_http(
    served: Served,
    ctx: Ctx,
    UrlPath((id, _path)): UrlPath<(String, String)>,
    request: Request,
) -> Response {
    if let Err(err) = webapp_only(&ctx) {
        return err.into_response();
    }
    let own = own_address(request.headers());
    relay(&store_path(&served), &id, own, request).await
}

#[cfg(test)]
#[path = "connections_tests.rs"]
mod tests;
