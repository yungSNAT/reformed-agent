//! The Webapp on the local runtime's listener, as upstream Hermes v0.23.2's `hermes webapp` and
//! browser use it: `webapp-launch` on the control socket gives a one-use link; the launch page
//! posts its ticket to `/webapp/window-session` for the browser credential; the page mints one
//! socket ticket a dial at `/api/webapp/socket-ticket` and opens `/api/ws` with it; REST calls carry
//! the credential in `X-Hermes-Session-Token`. Every refusal here is an access boundary.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use futures::StreamExt;
use hermes_desktop_gateway::local::{self, LocalRuntime};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message};

const INDEX: &str = "<!doctype html><html><head><title>Hermes</title><script>boot()</script></head><body></body></html>";

/// One bundle for this test binary, named by `HERMES_WEBAPP_DIST` before any runtime starts.
fn bundle() -> &'static Path {
    static DIST: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &DIST
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let dist = dir.path().join("dist");
            std::fs::create_dir_all(dist.join("assets")).unwrap();
            std::fs::write(dist.join("index.html"), INDEX).unwrap();
            std::fs::write(dist.join("assets/app.js"), "console.log(1)").unwrap();
            std::fs::write(dist.join("assets/app.css"), "body{}").unwrap();
            std::fs::write(dist.join(".hidden"), "dot").unwrap();
            std::fs::hard_link(dist.join("index.html"), dist.join("other.html")).unwrap();
            std::fs::write(dir.path().join("secret.js"), "outside").unwrap();
            std::os::unix::fs::symlink(dir.path().join("secret.js"), dist.join("assets/link.js"))
                .unwrap();
            std::os::unix::fs::symlink(dir.path(), dist.join("up")).unwrap();
            std::env::set_var("HERMES_WEBAPP_DIST", &dist);
            (dir, dist)
        })
        .1
}

fn private_home() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    (dir, home)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start(home: &Path, webapp_yaml: &str) -> LocalRuntime {
    bundle();
    std::fs::write(home.join("config.yaml"), webapp_yaml).unwrap();
    let chat = hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
    local::start(home, chat).expect("the runtime starts")
}

fn control(home: &Path, verb: &str, params: Value) -> Value {
    let mut stream = UnixStream::connect(home.join("gateway.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = json!({"protocol": 1, "id": 1, "verb": verb, "params": params});
    stream.write_all(format!("{request}\n").as_bytes()).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

/// A served runtime and its Webapp origin.
fn serving(home: &Path) -> (LocalRuntime, String) {
    let port = free_port();
    let runtime = start(home, &format!("webapp:\n  enabled: true\n  port: {port}\n"));
    (runtime, format!("http://127.0.0.1:{port}"))
}

fn launch_ticket(home: &Path) -> String {
    let reply = control(home, "webapp-launch", json!({}));
    assert_eq!(reply["ok"], true, "{reply}");
    let url = reply["result"]["url"].as_str().unwrap();
    url.split("#ticket=").nth(1).unwrap().to_string()
}

async fn window_session(
    origin: &str,
    sent_origin: Option<&str>,
    ticket: &str,
) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(format!("{origin}/webapp/window-session"))
        .header("X-Hermes-Window-Ticket", ticket);
    if let Some(sent) = sent_origin {
        request = request.header("Origin", sent);
    }
    request.send().await.unwrap()
}

/// The browser credential, the way the launch page gets it.
async fn credential(home: &Path, origin: &str) -> String {
    let ticket = launch_ticket(home);
    let reply = window_session(origin, Some(origin), &ticket).await;
    assert_eq!(reply.status(), 200);
    let body: Value = reply.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

async fn socket_ticket(origin: &str, token: &str, socket: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{origin}/api/webapp/socket-ticket?socket={socket}"))
        .header("X-Hermes-Session-Token", token)
        .header("Origin", origin)
        .send()
        .await
        .unwrap()
}

async fn minted_socket_ticket(origin: &str, token: &str) -> String {
    let reply = socket_ticket(origin, token, "gateway").await;
    assert_eq!(reply.status(), 200);
    let body: Value = reply.json().await.unwrap();
    assert_eq!(body["ttl_seconds"], 30);
    body["ticket"].as_str().unwrap().to_string()
}

fn browser_dial(
    origin: &str,
    sent_origin: Option<&str>,
    protocols: &str,
) -> tungstenite::handshake::client::Request {
    let url = format!(
        "{}/api/ws?native_dial=browser",
        origin.replacen("http", "ws", 1)
    );
    let mut request = url.into_client_request().unwrap();
    if let Some(sent) = sent_origin {
        request
            .headers_mut()
            .insert("Origin", sent.parse().unwrap());
    }
    if !protocols.is_empty() {
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", protocols.parse().unwrap());
    }
    request
}

async fn refused_status(request: tungstenite::handshake::client::Request) -> u16 {
    match tokio_tungstenite::connect_async(request).await {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(other) => panic!("expected an HTTP refusal, got {other}"),
        Ok(_) => panic!("the upgrade was accepted"),
    }
}

fn protocols(ticket: &str) -> String {
    format!("hermes-gateway-v1, hermes-gateway-ticket.{ticket}")
}

#[tokio::test(flavor = "multi_thread")]
async fn without_webapp_enabled_nothing_is_served_and_the_verbs_say_why() {
    let (_dir, home) = private_home();
    let runtime = start(&home, "webapp:\n  enabled: true\n  host: 0.0.0.0\n");
    let status = control(&home, "webapp-status", json!({}));
    assert_eq!(status["result"]["state"], "off", "{status}");
    assert!(status["result"]["reason"]
        .as_str()
        .unwrap()
        .contains("sign-in gate"));
    let launch = control(&home, "webapp-launch", json!({}));
    assert_eq!(launch["ok"], false);
    assert!(launch["error"]
        .as_str()
        .unwrap()
        .starts_with("The Webapp is off"));
    // The listener stays a free loopback port for native clients and serves no Webapp route: an
    // unknown path meets the native token guard, as before the Webapp existed.
    let origin = runtime.api_origin().to_string();
    assert!(origin.starts_with("http://127.0.0.1:"));
    let page = reqwest::get(format!("{origin}/")).await.unwrap();
    assert_eq!(page.status(), 401);
    let launch_page = reqwest::get(format!("{origin}/webapp/launch"))
        .await
        .unwrap();
    assert_eq!(launch_page.status(), 401);
    // Sign-out with no Webapp served still replaces the stored credential.
    let out = control(&home, "webapp-sign-out", json!({}));
    assert_eq!(out["result"]["serving"], false, "{out}");
    assert!(home.join("webapp/session-token").is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_configured_port_serves_native_clients_and_the_webapp() {
    let (_dir, home) = private_home();
    let (runtime, origin) = serving(&home);
    assert_eq!(runtime.api_origin(), origin);
    let status = control(&home, "webapp-status", json!({}));
    assert_eq!(
        status["result"],
        json!({"state": "serving", "url": origin, "reason": null})
    );
    let reply = control(&home, "webapp-launch", json!({"profile": "work"}));
    let result = &reply["result"];
    let url = result["url"].as_str().unwrap();
    assert!(
        url.starts_with(&format!("{origin}/webapp/launch#ticket=")),
        "{url}"
    );
    assert!(url.ends_with("&query=%3Fprofile%3Dwork"), "{url}");
    assert_eq!(result["expires_in_seconds"], 60);
    assert_eq!(result["private"], !cfg!(target_os = "linux"));
    let bad = control(&home, "webapp-launch", json!({"profile": "../x"}));
    assert_eq!(bad["ok"], false);
    let page = reqwest::get(format!("{origin}/webapp/launch"))
        .await
        .unwrap();
    assert_eq!(page.status(), 200);
    assert_eq!(page.headers()["cache-control"], "no-store");
    let policy = page.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(policy.contains("script-src 'nonce-") && policy.contains("frame-ancestors 'none'"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_taken_port_leaves_native_clients_served_and_names_both_addresses() {
    let (_dir, home) = private_home();
    let taken = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = taken.local_addr().unwrap().port();
    let runtime = start(
        &home,
        &format!("webapp:\n  enabled: true\n  port: {port}\n"),
    );
    let status = control(&home, "webapp-status", json!({}));
    assert_eq!(status["result"]["state"], "unavailable", "{status}");
    let reason = status["result"]["reason"].as_str().unwrap();
    assert!(reason.contains(&format!("port {port}")), "{reason}");
    assert!(reason.contains(runtime.api_origin()), "{reason}");
    assert_ne!(runtime.api_origin(), format!("http://127.0.0.1:{port}"));
    let probe = reqwest::get(format!("{}/api/status", runtime.api_origin()))
        .await
        .unwrap();
    assert_eq!(probe.status(), 200);
    assert_eq!(
        reqwest::get(format!("{}/", runtime.api_origin()))
            .await
            .unwrap()
            .status(),
        401
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_launch_ticket_gives_the_credential_once_and_only_same_origin() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let ticket = launch_ticket(&home);
    // No Origin, a foreign one and 2 of them are refused, and none spends the ticket.
    assert_eq!(window_session(&origin, None, &ticket).await.status(), 403);
    assert_eq!(
        window_session(&origin, Some("http://evil.example"), &ticket)
            .await
            .status(),
        403
    );
    let doubled = reqwest::Client::new()
        .post(format!("{origin}/webapp/window-session"))
        .header("X-Hermes-Window-Ticket", &ticket)
        .header("Origin", &origin)
        .header("Origin", &origin)
        .send()
        .await
        .unwrap();
    assert_eq!(doubled.status(), 403);
    let reply = window_session(&origin, Some(&origin), &ticket).await;
    assert_eq!(reply.status(), 200);
    assert_eq!(reply.headers()["cache-control"], "no-store");
    let token = reply.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let stored = std::fs::read_to_string(home.join("webapp/session-token")).unwrap();
    assert_eq!(stored.trim(), token);
    assert_eq!(
        window_session(&origin, Some(&origin), &ticket)
            .await
            .status(),
        403
    );
    assert_eq!(
        window_session(&origin, Some(&origin), "").await.status(),
        403
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_routes_admit_the_browser_credential_on_its_own_branch() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;
    let client = reqwest::Client::new();
    let get = |token: &str, origin_header: Option<&str>| {
        let mut request = client
            .get(format!("{origin}/api/hermes/update/check"))
            .header("X-Hermes-Session-Token", token);
        if let Some(o) = origin_header {
            request = request.header("Origin", o);
        }
        request.send()
    };
    assert_eq!(get(&token, None).await.unwrap().status(), 200);
    assert_eq!(get(&token, Some(&origin)).await.unwrap().status(), 200);
    assert_eq!(
        get(&token, Some("http://evil.example"))
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(get("wrong", None).await.unwrap().status(), 401);
    // The Origin is judged before the credential: a foreign page learns nothing about a guess.
    assert_eq!(
        get("wrong", Some("http://evil.example"))
            .await
            .unwrap()
            .status(),
        403
    );
    // Both credentials at once are refused, and the native ticket is left unspent.
    let instance = control(&home, "identify", json!({}))["result"]["instance_id"].clone();
    let native = control(
        &home,
        "session-ticket",
        json!({"profile_id": home.to_str().unwrap(),
        "instance_id": instance, "purpose": "native-http"}),
    )["result"]["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    let both = client
        .get(format!("{origin}/api/hermes/update/check"))
        .header("X-Hermes-Session-Token", &token)
        .header("X-Hermes-Gateway-Ticket", &native)
        .send()
        .await
        .unwrap();
    assert_eq!(both.status(), 401);
    let native_only = client
        .get(format!("{origin}/api/hermes/update/check"))
        .header("X-Hermes-Gateway-Ticket", &native)
        .send()
        .await
        .unwrap();
    assert_eq!(native_only.status(), 200);
}

/// A page at `evil.example` that rebinds its name to 127.0.0.1 sends its own name as both `Host`
/// and `Origin`. Every Webapp route refuses it (403) and spends nothing: the handoff and socket
/// checks themselves refuse a `Host` that is not a loopback name on this port, and the routes that
/// carry no `Origin` (the bundle, a REST call with the credential) rely on the Host guard, so this
/// fails if that guard stops covering any of them.
#[tokio::test(flavor = "multi_thread")]
async fn a_rebinding_host_is_refused_on_every_webapp_route() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let port = origin.rsplit(':').next().unwrap();
    let evil_host = format!("evil.example:{port}");
    let evil_origin = format!("http://{evil_host}");
    let client = reqwest::Client::new();

    let ticket = launch_ticket(&home);
    let handoff = client
        .post(format!("{origin}/webapp/window-session"))
        .header("Host", &evil_host)
        .header("Origin", &evil_origin)
        .header("X-Hermes-Window-Ticket", &ticket)
        .send()
        .await
        .unwrap();
    assert_eq!(handoff.status(), 403);
    // Unspent: the launch page on the real origin still redeems it.
    let redeemed = window_session(&origin, Some(&origin), &ticket).await;
    assert_eq!(redeemed.status(), 200);
    let token = redeemed.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    for (path, sends_origin) in [
        ("/api/hermes/update/check", false),
        ("/api/hermes/update/check", true),
        ("/api/status", false),
    ] {
        let mut request = client
            .get(format!("{origin}{path}"))
            .header("Host", &evil_host)
            .header("X-Hermes-Session-Token", &token);
        if sends_origin {
            request = request.header("Origin", &evil_origin);
        }
        assert_eq!(request.send().await.unwrap().status(), 403, "{path}");
    }
    let minted = client
        .post(format!("{origin}/api/webapp/socket-ticket?socket=gateway"))
        .header("Host", &evil_host)
        .header("Origin", &evil_origin)
        .header("X-Hermes-Session-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(minted.status(), 403);
    for path in ["/", "/index.html", "/assets/app.js", "/webapp/launch"] {
        let page = client
            .get(format!("{origin}{path}"))
            .header("Host", &evil_host)
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), 403, "{path}");
    }

    let socket = minted_socket_ticket(&origin, &token).await;
    let mut dial = browser_dial(&origin, Some(&evil_origin), &protocols(&socket));
    dial.headers_mut()
        .insert("Host", evil_host.parse().unwrap());
    assert_eq!(refused_status(dial).await, 403);
    // Unspent: the tab on the real origin opens its socket with it.
    let real = browser_dial(&origin, Some(&origin), &protocols(&socket));
    assert!(tokio_tungstenite::connect_async(real).await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn socket_tickets_need_the_credential_and_name_the_gateway_socket_only() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;
    assert_eq!(
        socket_ticket(&origin, "wrong", "gateway").await.status(),
        401
    );
    assert_eq!(socket_ticket(&origin, &token, "relay").await.status(), 404);
    assert_eq!(
        socket_ticket(&origin, &token, "host-terminal")
            .await
            .status(),
        404
    );
    assert_eq!(
        socket_ticket(&origin, &token, "nonsense").await.status(),
        400
    );
    let foreign = reqwest::Client::new()
        .post(format!("{origin}/api/webapp/socket-ticket?socket=gateway"))
        .header("X-Hermes-Session-Token", &token)
        .header("Origin", "http://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);
    let other_profile = reqwest::Client::new()
        .post(format!(
            "{origin}/api/webapp/socket-ticket?socket=gateway&profile=other"
        ))
        .header("X-Hermes-Session-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(other_profile.status(), 404);
    let reply = socket_ticket(&origin, &token, "gateway").await;
    assert_eq!(reply.headers()["cache-control"], "no-store");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_socket_opens_once_with_its_ticket_and_own_origin() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;

    let ticket = minted_socket_ticket(&origin, &token).await;
    let foreign = browser_dial(&origin, Some("http://evil.example"), &protocols(&ticket));
    assert_eq!(refused_status(foreign).await, 403);
    // Without an Origin it is a native dial, which knows no browser ticket.
    assert_eq!(
        refused_status(browser_dial(&origin, None, &protocols(&ticket))).await,
        401
    );
    let (mut socket, response) =
        tokio_tungstenite::connect_async(browser_dial(&origin, Some(&origin), &protocols(&ticket)))
            .await
            .expect("the browser socket opens");
    assert_eq!(
        response.headers()["sec-websocket-protocol"],
        "hermes-gateway-v1"
    );
    let Some(Ok(Message::Text(ready))) = socket.next().await else {
        panic!("no gateway.ready");
    };
    assert!(ready.contains("gateway.ready"), "{ready}");
    // Spent.
    assert_eq!(
        refused_status(browser_dial(&origin, Some(&origin), &protocols(&ticket))).await,
        401
    );

    // A native ticket with a browser Origin is refused and stays unspent for its native dial.
    let instance = control(&home, "identify", json!({}))["result"]["instance_id"].clone();
    let native = control(
        &home,
        "session-ticket",
        json!({"profile_id": home.to_str().unwrap(),
        "instance_id": instance, "purpose": "interactive"}),
    )["result"]["ticket"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        refused_status(browser_dial(&origin, Some(&origin), &protocols(&native))).await,
        401
    );
    let native_dial = browser_dial(&origin, None, &protocols(&native));
    assert!(tokio_tungstenite::connect_async(native_dial).await.is_ok());

    // A ticket in both the query and the subprotocol is refused.
    let a = minted_socket_ticket(&origin, &token).await;
    let b = minted_socket_ticket(&origin, &token).await;
    let url = format!("{}/api/ws?ticket={b}", origin.replacen("http", "ws", 1));
    let mut both = url.into_client_request().unwrap();
    both.headers_mut().insert("Origin", origin.parse().unwrap());
    both.headers_mut()
        .insert("Sec-WebSocket-Protocol", protocols(&a).parse().unwrap());
    assert_eq!(refused_status(both).await, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_out_closes_browser_sockets_and_ends_the_old_credential() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;
    let ticket = minted_socket_ticket(&origin, &token).await;
    let pending = minted_socket_ticket(&origin, &token).await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(browser_dial(&origin, Some(&origin), &protocols(&ticket)))
            .await
            .unwrap();
    let _ready = socket.next().await;

    let out = control(&home, "webapp-sign-out", json!({}));
    assert_eq!(
        out["result"],
        json!({"signed_out": true, "serving": true, "sign_in_gate": false, "connections_closed": 1})
    );
    let closed = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap();
    match closed {
        Some(Ok(Message::Close(Some(frame)))) => assert_eq!(u16::from(frame.code), 4401),
        other => panic!("expected a close frame, got {other:?}"),
    }
    assert_eq!(
        socket_ticket(&origin, &token, "gateway").await.status(),
        401
    );
    assert_eq!(
        refused_status(browser_dial(&origin, Some(&origin), &protocols(&pending))).await,
        401
    );
    let renewed = credential(&home, &origin).await;
    assert_ne!(renewed, token);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bundle_is_served_from_its_root_only() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let get = |path: &str| reqwest::get(format!("{origin}{path}"));

    for entry in [
        "/",
        "/index.html",
        "/other.html",
        "/chat/123",
        "/%69ndex.html",
    ] {
        let page = get(entry).await.unwrap();
        assert_eq!(page.status(), 200, "{entry}");
        assert_eq!(page.headers()["cache-control"], "no-store", "{entry}");
        let policy = page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_string();
        let body = page.text().await.unwrap();
        assert!(
            body.contains("window.__HERMES_UI_SURFACE__=\"webapp\""),
            "{entry}: {body}"
        );
        assert!(body.contains("window.__HERMES_DASHBOARD_PROFILE__=\"default\""));
        assert!(!body.contains(
            std::fs::read_to_string(home.join("webapp/session-token"))
                .unwrap()
                .trim()
        ));
        assert!(policy.contains("'sha256-"), "{policy}");
        assert!(policy.contains("frame-ancestors 'none'"));
    }
    let script = get("/assets/app.js").await.unwrap();
    assert_eq!(script.status(), 200);
    assert_eq!(
        script.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
    assert_eq!(script.headers()["x-content-type-options"], "nosniff");
    assert_eq!(script.text().await.unwrap(), "console.log(1)");

    for refused in [
        "/assets/link.js",
        "/up/secret.js",
        "/../secret.js",
        "/%2e%2e/secret.js",
        "/assets/%2e%2e/%2e%2e/secret.js",
        "/assets%2f..%2f..%2fsecret.js",
        "/%5c..%5csecret.js",
        "/.hidden",
        "/assets/missing.js",
        "/a%00b.js",
    ] {
        let response = get(refused).await.unwrap();
        assert_eq!(response.status(), 404, "{refused}");
        assert!(
            !response.text().await.unwrap().contains("outside"),
            "{refused}"
        );
    }
    let api = get("/api/nothing").await.unwrap();
    assert_eq!(api.status(), 404);
    assert_eq!(
        api.json::<Value>().await.unwrap()["detail"],
        "No such API endpoint: /api/nothing"
    );

    let wrong_host = reqwest::Client::new()
        .get(format!("{origin}/"))
        .header("Host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_host.status(), 403);
}
