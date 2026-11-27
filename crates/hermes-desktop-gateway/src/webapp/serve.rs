//! The Webapp's state, bind, control verbs and routes.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Map, Value};
use tokio::sync::watch;

use super::api::{served_profile, Caller, Ctx};
use super::assets::{self, Bootstrap};
use super::credential::{self, Credential};
use super::handoff::{self, Kind, Tickets};
use super::settings::{self, Settings};

/// What the runtime's control socket says about the Webapp.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    /// `webapp.enabled` is not true, or the block cannot be used (the reason).
    Off(Option<String>),
    /// Bound at `url`; serving while the bundle is present.
    Serving(String),
    /// Asked for but not served (the reason): the port was taken, or the credential cannot be kept.
    Unavailable(String),
}

pub struct Webapp {
    home: PathBuf,
    profile: String,
    dist: Option<PathBuf>,
    status: Status,
    /// The bound address, for matching a redeeming connection's `/proc/net/tcp` row.
    local: SocketAddr,
    credential: Option<Credential>,
    tickets: Tickets,
    /// Browser sockets open now.
    sockets: Arc<AtomicUsize>,
    /// Whether a launch link opened by a browser launcher can be checked here (tests pass false).
    links_checked: bool,
}

/// The listener a local runtime serves on and its Webapp: the configured address when the Webapp
/// is on, else a free 127.0.0.1 port. A taken configured port falls back to a free one, with the
/// Webapp unavailable and both addresses named.
pub fn bind(home: &Path) -> Result<(std::net::TcpListener, Arc<Webapp>), String> {
    bind_with(home, settings::read(home), assets::dist_from_env())
}

fn bind_free() -> Result<std::net::TcpListener, String> {
    std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("cannot bind 127.0.0.1: {e}"))
}

pub(crate) fn bind_with(
    home: &Path,
    settings: Settings,
    dist: Option<PathBuf>,
) -> Result<(std::net::TcpListener, Arc<Webapp>), String> {
    let (listener, status) = if settings.enabled {
        let ip = settings::bind_ip(&settings.host);
        match std::net::TcpListener::bind((ip, settings.port)) {
            Ok(listener) => {
                let url = settings::origin_of(ip, settings.port);
                (listener, Status::Serving(url))
            }
            Err(e) => {
                let listener = bind_free()?;
                let native = local_origin(&listener);
                let reason = format!(
                    "port {} on {} is in use ({e}); native clients use {native}",
                    settings.port, settings.host
                );
                tracing::warn!("Webapp unavailable: {reason}");
                (listener, Status::Unavailable(reason))
            }
        }
    } else {
        (bind_free()?, Status::Off(settings.reason.clone()))
    };
    let local = listener
        .local_addr()
        .map_err(|e| format!("cannot read the bound address: {e}"))?;
    let (credential, status) = match status {
        Status::Serving(url) => match Credential::load(home) {
            Ok(credential) => (Some(credential), Status::Serving(url)),
            Err(e) => (None, Status::Unavailable(e.to_string())),
        },
        other => (None, other),
    };
    let webapp = Webapp {
        home: home.to_path_buf(),
        profile: profile_name(home),
        dist,
        status,
        local,
        credential,
        tickets: Tickets::default(),
        sockets: Arc::default(),
        links_checked: handoff::launch_links_checked(),
    };
    Ok((listener, Arc::new(webapp)))
}

fn local_origin(listener: &std::net::TcpListener) -> String {
    listener
        .local_addr()
        .map(|a| settings::origin_of(a.ip(), a.port()))
        .unwrap_or_default()
}

/// The profile a home is: its folder's name under a `profiles` folder, else `default`.
pub fn profile_name(home: &Path) -> String {
    match (home.parent().and_then(Path::file_name), home.file_name()) {
        (Some(parent), Some(name)) if parent == "profiles" => name.to_string_lossy().into_owned(),
        _ => "default".into(),
    }
}

/// Whether `headers` hold no `Origin`, or exactly one that is [`own_origin`].
pub fn origin_absent_or_own(headers: &HeaderMap, port: u16) -> bool {
    headers.get_all(header::ORIGIN).iter().next().is_none() || own_origin(headers, port)
}

/// Whether `headers` hold exactly one `Origin` naming this request's own `Host`, and that `Host`
/// is a loopback name on `port`, the listener's. The Host guard checks the same in front of every
/// route; this does not rely on it, so a rebinding page (its own name as `Host` and `Origin`) is
/// refused here too.
pub fn own_origin(headers: &HeaderMap, port: u16) -> bool {
    let origins: Vec<_> = headers.get_all(header::ORIGIN).iter().collect();
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    match (origins.as_slice(), host) {
        ([origin], Some(host)) => {
            crate::is_loopback_authority(host, port)
                && origin
                    .to_str()
                    .is_ok_and(|o| o.eq_ignore_ascii_case(&format!("http://{host}")))
        }
        _ => false,
    }
}

/// A browser socket the runtime counts while it is open.
pub struct OpenSocket(Arc<AtomicUsize>);

impl Drop for OpenSocket {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Webapp {
    /// The URL native clients dial: the bound address.
    pub fn native_origin(&self) -> String {
        settings::origin_of(self.local.ip(), self.local.port())
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// The credential while the Webapp is bound and serving.
    fn serving_credential(&self) -> Option<&Credential> {
        match self.status {
            Status::Serving(_) => self.credential.as_ref(),
            _ => None,
        }
    }

    /// Whether this listener serves the Webapp's routes (it was bound for it).
    pub fn serves_routes(&self) -> bool {
        self.serving_credential().is_some()
    }

    /// The generation of `token` when it is the current browser credential.
    pub fn admit_browser(&self, token: &str) -> Option<u64> {
        self.serving_credential()?.matches(token)
    }

    /// Spends a browser socket ticket, giving its generation while that is still current.
    pub fn take_socket_ticket(&self, ticket: &str) -> Option<u64> {
        let credential = self.serving_credential()?;
        let generation = self.tickets.take_socket(ticket)?;
        (generation == credential.generation()).then_some(generation)
    }

    /// The credential's generation watch and a count of this open socket, for a browser socket
    /// that must close when its generation ends.
    pub fn open_socket(&self) -> Option<(watch::Receiver<u64>, OpenSocket)> {
        let credential = self.serving_credential()?;
        self.sockets.fetch_add(1, Ordering::SeqCst);
        Some((
            credential.subscribe(),
            OpenSocket(Arc::clone(&self.sockets)),
        ))
    }

    fn bundle_missing(&self) -> Option<String> {
        assets::missing_reason(self.dist.as_deref())
    }

    /// `webapp-status`.
    pub fn status_json(&self) -> Value {
        match &self.status {
            Status::Off(reason) => json!({"state": "off", "url": null, "reason": reason}),
            Status::Unavailable(reason) => {
                json!({"state": "unavailable", "url": null, "reason": reason})
            }
            Status::Serving(url) => match self.bundle_missing() {
                Some(reason) => json!({"state": "unavailable", "url": null, "reason": reason}),
                None => json!({"state": "serving", "url": url, "reason": null}),
            },
        }
    }

    fn serving(&self) -> Result<(&str, &Credential), String> {
        match (&self.status, &self.credential) {
            (Status::Off(_), _) => Err(
                "The Webapp is off. Set webapp.enabled: true in config.yaml and restart the gateway."
                    .into(),
            ),
            (Status::Unavailable(reason), _) => Err(format!("The Webapp is unavailable: {reason}")),
            (Status::Serving(url), Some(credential)) => Ok((url, credential)),
            (Status::Serving(_), None) => Err("The Webapp is unavailable".into()),
        }
    }

    /// `webapp-launch {profile?}`: a one-use launch link, private where its redeemer cannot be
    /// checked (then printed by `hermes webapp`, never handed to a browser launcher).
    pub fn launch(&self, params: &Map<String, Value>) -> Result<Value, String> {
        let profile = match params.get("profile") {
            None | Some(Value::Null) => None,
            Some(Value::String(p)) if profile_id_ok(p) => Some(p.as_str()),
            Some(_) => return Err("ValueError: profile must be a profile name".into()),
        };
        let (url, credential) = self.serving()?;
        if let Some(missing) = self.bundle_missing() {
            return Err(format!("The Webapp is unavailable: {missing}"));
        }
        let kind = if self.links_checked {
            Kind::Launch
        } else {
            Kind::Printed
        };
        let ticket = self.tickets.mint(kind, credential.generation())?;
        let mut fragment = format!("ticket={ticket}");
        if let Some(profile) = profile {
            fragment.push_str(&format!("&query=%3Fprofile%3D{profile}"));
        }
        Ok(json!({
            "url": format!("{url}/webapp/launch#{fragment}"),
            "private": !self.links_checked,
            "expires_in_seconds": handoff::LAUNCH_TTL.as_secs(),
        }))
    }

    /// `webapp-sign-out`: a new credential; every ticket and browser socket of the old one ends.
    pub fn sign_out(&self) -> Result<Value, String> {
        let Some(credential) = self.serving_credential() else {
            credential::rotate_stored(&self.home).map_err(|e| e.to_string())?;
            return Ok(
                json!({"signed_out": true, "serving": false, "sign_in_gate": false,
                "connections_closed": 0}),
            );
        };
        let open = self.sockets.load(Ordering::SeqCst);
        let generation = credential.rotate().map_err(|e| e.to_string())?;
        self.tickets.retire_before(generation);
        Ok(
            json!({"signed_out": true, "serving": true, "sign_in_gate": false,
            "connections_closed": open}),
        )
    }

    /// One control verb (`webapp-status`, `webapp-launch`, `webapp-sign-out`) for a peer the
    /// socket has checked (`peer_ok`: same uid, private home).
    pub fn control(
        &self,
        verb: &str,
        request: &Map<String, Value>,
        peer_ok: bool,
    ) -> Result<Value, String> {
        if !peer_ok {
            return Err("PermissionError: authenticated runtime bootstrap unavailable".into());
        }
        let empty = Map::new();
        let params = match request.get("params") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(params)) => params,
            Some(_) => return Err("ValueError: params must be an object".into()),
        };
        match verb {
            "webapp-status" => Ok(self.status_json()),
            "webapp-launch" => self.launch(params),
            "webapp-sign-out" => self.sign_out(),
            other => Err(format!("unknown verb: {other}")),
        }
    }
}

/// Upstream `PROFILE_ID_RE`: `^[a-z0-9][a-z0-9_-]{0,63}$`.
fn profile_id_ok(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

fn detail(status: StatusCode, text: &str) -> Response {
    (status, Json(json!({"detail": text}))).into_response()
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Routes outside the credential guard: the handoff pages, the ticket exchange and the bundle.
pub fn public_routes<S: Clone + Send + Sync + 'static>(webapp: Arc<Webapp>) -> Router<S> {
    Router::new()
        .route("/webapp/launch", get(launch_page))
        .route("/webapp/window", get(window_page))
        .route("/webapp/window-session", post(window_session))
        .fallback(bundle)
        .with_state(webapp)
}

/// Routes behind the credential guard: the browser's own ticket mints.
pub fn guarded_routes<S: Clone + Send + Sync + 'static>(webapp: Arc<Webapp>) -> Router<S> {
    Router::new()
        .route("/api/webapp/window-ticket", post(window_ticket))
        .route("/api/webapp/socket-ticket", post(socket_ticket))
        .with_state(webapp)
}

fn page(title: &str, status: &str, script: &str) -> Response {
    let (html, policy) = handoff::handoff_page(title, status, script);
    let mut response =
        no_store(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response());
    let headers = response.headers_mut();
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if let Ok(policy) = HeaderValue::from_str(&policy) {
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
    }
    response
}

async fn launch_page() -> Response {
    page(
        "Opening Hermes",
        "Opening Hermes Webapp…",
        handoff::LAUNCH_SCRIPT,
    )
}

async fn window_page() -> Response {
    page(
        "Opening Hermes",
        "Authorizing this Hermes window…",
        handoff::WINDOW_SCRIPT,
    )
}

/// Exchanges a one-use handoff ticket for the credential. The body is never read; the request
/// must carry exactly one `Origin`, this listener's own.
async fn window_session(State(webapp): State<Arc<Webapp>>, request: Request) -> Response {
    let headers = request.headers();
    if !own_origin(headers, webapp.local.port()) {
        return detail(
            StatusCode::FORBIDDEN,
            "Webapp window handoff requires a same-origin request",
        );
    }
    let expired = || {
        detail(
            StatusCode::FORBIDDEN,
            "Webapp window link expired or was already used; open a new window",
        )
    };
    let Some(credential) = webapp.serving_credential() else {
        return expired();
    };
    let ticket = headers
        .get("x-hermes-window-ticket")
        .and_then(|t| t.to_str().ok())
        .unwrap_or_default();
    let client = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let peer_ok = || {
        client.is_some_and(|client| {
            handoff::loopback_peer_uid(client, webapp.local) == Some(own_uid())
        })
    };
    let token = webapp
        .tickets
        .take_handoff(ticket, peer_ok)
        .and_then(|generation| credential.current(generation));
    match token {
        Some(token) => no_store(Json(json!({"token": token})).into_response()),
        None => expired(),
    }
}

fn own_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

fn browser_generation(ctx: &Ctx) -> Option<u64> {
    match ctx.caller {
        Caller::Browser { generation } => Some(generation),
        _ => None,
    }
}

async fn window_ticket(State(webapp): State<Arc<Webapp>>, ctx: Ctx) -> Response {
    let Some(generation) = browser_generation(&ctx) else {
        return detail(StatusCode::UNAUTHORIZED, "Unauthorized");
    };
    match webapp.tickets.mint(Kind::Window, generation) {
        Ok(ticket) => no_store(Json(json!({"ticket": ticket})).into_response()),
        Err(e) => detail(StatusCode::TOO_MANY_REQUESTS, e),
    }
}

/// Sockets upstream's Webapp mints tickets for that this build does not serve.
const UNSERVED_SOCKETS: [&str; 4] = [
    "host-terminal",
    "speak-stream",
    "transcribe-stream",
    "relay",
];

async fn socket_ticket(
    State(webapp): State<Arc<Webapp>>,
    ctx: Ctx,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let socket = query.get("socket").map(String::as_str).unwrap_or_default();
    if socket != "gateway" {
        return if UNSERVED_SOCKETS.contains(&socket) {
            detail(
                StatusCode::NOT_FOUND,
                &format!("Webapp {socket} tickets are unavailable on this surface"),
            )
        } else {
            detail(StatusCode::BAD_REQUEST, "Unknown socket")
        };
    }
    let Some(generation) = browser_generation(&ctx) else {
        return detail(StatusCode::UNAUTHORIZED, "Unauthorized");
    };
    if let Err(e) = served_profile(&ctx.api, query.get("profile").map(String::as_str)) {
        return e.into_response();
    }
    match webapp.tickets.mint(Kind::Socket, generation) {
        Ok(ticket) => no_store(
            Json(json!({"ticket": ticket, "ttl_seconds": handoff::SOCKET_TTL.as_secs()}))
                .into_response(),
        ),
        Err(e) => detail(StatusCode::TOO_MANY_REQUESTS, e),
    }
}

/// Any other `GET` or `HEAD`: a file of the bundle, or the entry document.
async fn bundle(State(webapp): State<Arc<Webapp>>, method: Method, request: Request) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return detail(StatusCode::NOT_FOUND, "Not Found");
    }
    let path = request.uri().path().to_string();
    let served = Arc::clone(&webapp);
    tokio::task::spawn_blocking(move || {
        let boot = Bootstrap {
            profile: &served.profile,
        };
        assets::serve(served.dist.as_deref(), &path, &boot)
    })
    .await
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

impl std::fmt::Debug for Webapp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Webapp")
            .field("status", &self.status)
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(port: u16) -> Settings {
        Settings {
            enabled: true,
            port,
            ..Settings::default()
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn with(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(name, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn an_own_origin_is_a_loopback_host_on_this_port_not_whatever_host_is_sent() {
        let (host, origin) = (header::HOST, header::ORIGIN);
        for name in ["127.0.0.1:9120", "localhost:9120", "[::1]:9120"] {
            let pair = with(&[
                (host.clone(), name),
                (origin.clone(), &format!("http://{name}")),
            ]);
            assert!(own_origin(&pair, 9120), "{name}");
            assert!(origin_absent_or_own(&pair, 9120), "{name}");
        }
        // A rebinding page sends its own name as both Host and Origin: refused without relying on
        // the Host guard in front.
        for name in [
            "evil.example:9120",
            "127.0.0.1:9121",
            "127.0.0.2:9120",
            "localhost",
        ] {
            let pair = with(&[
                (host.clone(), name),
                (origin.clone(), &format!("http://{name}")),
            ]);
            assert!(!own_origin(&pair, 9120), "{name}");
            assert!(!origin_absent_or_own(&pair, 9120), "{name}");
        }
        let foreign = with(&[
            (host.clone(), "127.0.0.1:9120"),
            (origin.clone(), "http://evil.example"),
        ]);
        assert!(!own_origin(&foreign, 9120));
        let absent = with(&[(host.clone(), "127.0.0.1:9120")]);
        assert!(!own_origin(&absent, 9120));
        assert!(origin_absent_or_own(&absent, 9120));
        let twice = with(&[
            (host, "127.0.0.1:9120"),
            (origin.clone(), "http://127.0.0.1:9120"),
            (origin, "http://127.0.0.1:9120"),
        ]);
        assert!(!own_origin(&twice, 9120));
    }

    #[test]
    fn without_a_bundle_the_webapp_says_how_to_get_one_and_mints_nothing() {
        let home = tempfile::tempdir().unwrap();
        let (_listener, webapp) = bind_with(home.path(), on(free_port()), None).unwrap();
        let status = webapp.status_json();
        assert_eq!(status["state"], "unavailable");
        let reason = status["reason"].as_str().unwrap();
        assert!(reason.starts_with(assets::NOT_BUILT), "{reason}");
        assert!(reason.contains(assets::DIST_ENV), "{reason}");
        let refused = webapp.launch(&Map::new()).unwrap_err();
        assert!(refused.contains(assets::DIST_ENV), "{refused}");
    }

    #[test]
    fn every_verb_needs_a_checked_peer() {
        let home = tempfile::tempdir().unwrap();
        let (_listener, webapp) = bind_with(home.path(), on(free_port()), None).unwrap();
        for verb in ["webapp-status", "webapp-launch", "webapp-sign-out"] {
            let refused = webapp.control(verb, &Map::new(), false).unwrap_err();
            assert!(refused.starts_with("PermissionError"), "{verb}: {refused}");
        }
        assert!(!home.path().join("webapp/session-token.new").exists());
    }

    #[test]
    fn where_redeemers_cannot_be_checked_the_link_is_private() {
        let home = tempfile::tempdir().unwrap();
        let dist = tempfile::tempdir().unwrap();
        std::fs::write(dist.path().join("index.html"), "<html></html>").unwrap();
        let (_listener, mut webapp) =
            bind_with(home.path(), on(free_port()), Some(dist.path().into())).unwrap();
        Arc::get_mut(&mut webapp).unwrap().links_checked = false;
        let link = webapp.launch(&Map::new()).unwrap();
        assert_eq!(link["private"], true);
        let ticket = link["url"]
            .as_str()
            .unwrap()
            .split("#ticket=")
            .nth(1)
            .unwrap();
        // A printed ticket spends without the peer check, as upstream's does.
        assert_eq!(webapp.tickets.take_handoff(ticket, || false), Some(1));
    }

    #[test]
    fn a_profile_home_names_its_profile() {
        assert_eq!(profile_name(Path::new("/h/.hermes/profiles/work")), "work");
        assert_eq!(profile_name(Path::new("/h/.hermes")), "default");
        assert!(profile_id_ok("a-b_9") && !profile_id_ok("A") && !profile_id_ok("-a"));
        assert!(!profile_id_ok(&"a".repeat(65)) && !profile_id_ok(""));
    }
}
