//! The local runtime with the Webapp serving, and the callers that reach it, for the Webapp
//! route tests.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use hermes_desktop_gateway::local::{self, LocalRuntime};
use reqwest::Method;
use serde_json::{json, Value};

pub fn bundle() {
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

pub fn private_home() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    (dir, home)
}

pub fn serving(home: &Path) -> (LocalRuntime, String) {
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

pub fn control(home: &Path, verb: &str, params: Value) -> Value {
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
pub async fn credential(home: &Path, origin: &str) -> String {
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
pub fn native_ticket(home: &Path) -> String {
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

pub fn call(
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
