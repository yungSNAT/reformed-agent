//! The Webapp's start-up routes through the full local runtime: who may call them, and what the
//! Webapp-only routes do for a signed-in browser.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use hermes_desktop_gateway::local::{self, LocalRuntime};
use reqwest::Method;
use serde_json::{json, Value};

/// Every route in the group, with the method the page uses.
const ROUTES: [(&str, &str); 9] = [
    ("GET", "/api/ui-preferences"),
    ("PATCH", "/api/ui-preferences"),
    ("GET", "/api/desktop-plugins/root"),
    ("GET", "/api/desktop-plugins/list?path=/"),
    ("GET", "/api/local-models/status"),
    ("GET", "/api/local-models/jobs"),
    ("POST", "/api/sessions/owner-backfill"),
    ("GET", "/api/fs/default-cwd"),
    ("GET", "/api/fs/default-cwd?profile=default"),
];

/// Upstream serves these on the Webapp's surface only.
const WEBAPP_ONLY: [&str; 3] = [
    "/api/ui-preferences",
    "/api/desktop-plugins/root",
    "/api/desktop-plugins/list",
];

fn bundle() {
    static DIST: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIST.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("index.html"),
            "<!doctype html><html><head></head><body></body></html>",
        )
        .unwrap();
        std::env::set_var("HERMES_WEBAPP_DIST", dir.path());
        dir
    });
}

fn private_home() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    (dir, home)
}

fn serving(home: &Path) -> (LocalRuntime, String) {
    bundle();
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(
        home.join("config.yaml"),
        format!("webapp:\n  enabled: true\n  port: {port}\n"),
    )
    .unwrap();
    let chat = hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
    let runtime = local::start(home, chat).expect("the runtime starts");
    (runtime, format!("http://127.0.0.1:{port}"))
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

/// The browser credential, the way the launch page gets it.
async fn credential(home: &Path, origin: &str) -> String {
    let reply = control(home, "webapp-launch", json!({}));
    let url = reply["result"]["url"].as_str().unwrap().to_owned();
    let ticket = url.split("#ticket=").nth(1).unwrap();
    let reply = reqwest::Client::new()
        .post(format!("{origin}/webapp/window-session"))
        .header("X-Hermes-Window-Ticket", ticket)
        .header("Origin", origin)
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 200);
    let body: Value = reply.json().await.unwrap();
    body["token"].as_str().unwrap().to_owned()
}

/// A one-use `native-http` ticket from the control socket.
fn native_ticket(home: &Path) -> String {
    let instance = control(home, "identify", json!({}))["result"]["instance_id"].clone();
    control(
        home,
        "session-ticket",
        json!({"profile_id": home.to_str().unwrap(), "instance_id": instance, "purpose": "native-http"}),
    )["result"]["ticket"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn call(
    origin: &str,
    method: &str,
    route: &str,
    headers: &[(&str, &str)],
) -> reqwest::RequestBuilder {
    let mut request = reqwest::Client::new().request(
        Method::from_bytes(method.as_bytes()).unwrap(),
        format!("{origin}{route}"),
    );
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if method != "GET" {
        request = request.json(&json!({}));
    }
    request
}

#[tokio::test(flavor = "multi_thread")]
async fn every_route_refuses_a_caller_without_the_credential_or_from_another_origin() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;
    for (method, route) in ROUTES {
        let none = call(&origin, method, route, &[]).send().await.unwrap();
        assert_eq!(none.status(), 401, "{method} {route} without a credential");
        let wrong = call(
            &origin,
            method,
            route,
            &[("X-Hermes-Session-Token", "wrong")],
        )
        .send()
        .await
        .unwrap();
        assert_eq!(
            wrong.status(),
            401,
            "{method} {route} with a wrong credential"
        );
        let foreign = call(
            &origin,
            method,
            route,
            &[
                ("X-Hermes-Session-Token", &token),
                ("Origin", "http://evil.example"),
            ],
        )
        .send()
        .await
        .unwrap();
        assert_eq!(
            foreign.status(),
            403,
            "{method} {route} from another origin"
        );
    }
    assert!(!home.join("ui-preferences.json").exists());
    assert!(!home.join("desktop-plugins").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_webapp_only_routes_do_not_exist_for_a_native_client() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    for (method, route) in ROUTES {
        let ticket = native_ticket(&home);
        let reply = call(
            &origin,
            method,
            route,
            &[("X-Hermes-Gateway-Ticket", &ticket)],
        )
        .send()
        .await
        .unwrap();
        let webapp_only = WEBAPP_ONLY.iter().any(|w| route.starts_with(w));
        let expected = if webapp_only { 404 } else { 200 };
        assert_eq!(
            reply.status(),
            expected,
            "{method} {route} for a native client"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_reads_and_writes_the_machines_preferences() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;
    let auth = [
        ("X-Hermes-Session-Token", token.as_str()),
        ("Origin", origin.as_str()),
    ];
    let empty: Value = call(&origin, "GET", "/api/ui-preferences", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty, json!({"exists": false, "revision": 0, "values": {}}));

    let written = call(&origin, "PATCH", "/api/ui-preferences", &auth)
        .json(&json!({"values": {"hermes.desktop.theme": "dark"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(written.status(), 200);
    let read: Value = call(&origin, "GET", "/api/ui-preferences", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(read["values"], json!({"hermes.desktop.theme": "dark"}));
    assert_eq!(read["revision"], json!(1));
    let mode = std::fs::metadata(home.join("ui-preferences.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    let refused = call(&origin, "PATCH", "/api/ui-preferences", &auth)
        .json(&json!({"values": {"api_key": "x"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_listing_stays_inside_the_plugins_root() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;
    let auth = [
        ("X-Hermes-Session-Token", token.as_str()),
        ("Origin", origin.as_str()),
    ];
    let root: Value = call(&origin, "GET", "/api/desktop-plugins/root", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let root = root["root"].as_str().unwrap().to_owned();
    assert_eq!(Path::new(&root), home.join("desktop-plugins"));
    let listed = call(
        &origin,
        "GET",
        &format!("/api/desktop-plugins/list?path={root}"),
        &auth,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(listed.status(), 200);
    for outside in [home.to_str().unwrap().to_owned(), format!("{root}/../..")] {
        let reply = call(
            &origin,
            "GET",
            &format!("/api/desktop-plugins/list?path={outside}"),
            &auth,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(reply.status(), 404, "{outside}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn another_profile_is_not_served_here() {
    let (_dir, home) = private_home();
    let (_runtime, origin) = serving(&home);
    let token = credential(&home, &origin).await;
    let auth = [
        ("X-Hermes-Session-Token", token.as_str()),
        ("Origin", origin.as_str()),
    ];
    let cwd = call(&origin, "GET", "/api/fs/default-cwd?profile=other", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(cwd.status(), 404);
    let backfill = call(&origin, "POST", "/api/sessions/owner-backfill", &auth)
        .json(&json!({"profile": "other"}))
        .send()
        .await
        .unwrap();
    assert_eq!(backfill.status(), 404);
    let ours: Value = call(&origin, "POST", "/api/sessions/owner-backfill", &auth)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        ours,
        json!({"ok": true, "stamped": 0, "profile": "default"})
    );
}
