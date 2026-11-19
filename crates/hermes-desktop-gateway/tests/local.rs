//! Local mode, as upstream Hermes Desktop v0.23.2 uses it (apps/desktop/electron/local-gateway.ts):
//! `hermes gateway ensure --json` finds the runtime through `<home>/gateway.sock`'s `identify`,
//! the desktop mints a one-use ticket there with `session-ticket`, then dials
//! `/api/ws?native_dial=<nonce>&ticket=<ticket>` without an Origin and sends each REST call with
//! its own `X-Hermes-Gateway-Ticket`.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use futures::StreamExt;
use hermes_desktop_gateway::local::{self, LocalRuntime};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message};

fn private_home() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    (dir, home)
}

async fn start(home: &Path) -> LocalRuntime {
    // These tests make no turns, so the chat needs no model.
    let chat = hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
    local::start(home, chat).await.expect("the runtime starts")
}

/// One request line to the control socket and its one reply line, as the desktop sends them. It
/// blocks, so the tests that call it run on a multi-threaded runtime, where the server keeps running.
fn control(home: &Path, request: Value) -> Value {
    let mut stream = UnixStream::connect(home.join("gateway.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(format!("{request}\n").as_bytes()).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn identify(home: &Path) -> Value {
    control(home, json!({"protocol": 1, "id": 1, "verb": "identify"}))
}

fn ticket_request(home: &Path, instance: &str, purpose: &str) -> Value {
    json!({"protocol": 1, "id": 1, "verb": "session-ticket", "params": {
        "profile_id": home.to_str().unwrap(), "instance_id": instance, "purpose": purpose}})
}

fn mint(home: &Path, instance: &str, purpose: &str) -> String {
    let reply = control(home, ticket_request(home, instance, purpose));
    assert_eq!(reply["ok"], true, "{reply}");
    reply["result"]["ticket"].as_str().unwrap().to_string()
}

fn ws_url(origin: &str, query: &str) -> String {
    format!("{}/api/ws{query}", origin.replacen("http", "ws", 1))
}

async fn refused_status(request: tungstenite::handshake::client::Request) -> u16 {
    match tokio_tungstenite::connect_async(request).await {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(other) => panic!("expected an HTTP refusal, got {other}"),
        Ok(_) => panic!("the upgrade was accepted"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn identify_names_a_ready_session_authority_runtime_for_this_home() {
    let (_dir, home) = private_home();
    let runtime = start(&home).await;
    let reply = identify(&home);
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["protocol"], 1);
    assert_eq!(reply["id"], 1);
    let result = &reply["result"];
    assert_eq!(result["runtime_protocol"], 1);
    assert_eq!(result["state"], "ready");
    assert_eq!(result["supervisor"], "none");
    assert_eq!(result["authority_epoch"], 1);
    assert_eq!(result["instance_id"], runtime.instance_id());
    assert_eq!(result["pid"], std::process::id());
    assert_eq!(result["capabilities"], json!(["session-authority-v1"]));
    let home_text = home.to_str().unwrap();
    assert_eq!(
        result["served_profiles"],
        json!([{"home": home_text, "profile_id": home_text}])
    );
    let origin = result["api_origin"].as_str().unwrap();
    assert_eq!(origin, runtime.api_origin());
    assert!(origin.starts_with("http://127.0.0.1:"), "{origin}");
    // The advertised origin serves the public probe.
    let status = reqwest::get(format!("{origin}/api/status")).await.unwrap();
    assert_eq!(status.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_publishes_its_owner_files_privately_and_removes_them_when_dropped() {
    let (_dir, home) = private_home();
    let runtime = start(&home).await;
    let socket = std::fs::symlink_metadata(home.join("gateway.sock")).unwrap();
    assert_eq!(socket.mode() & 0o777, 0o600);
    let lock: Value =
        serde_json::from_slice(&std::fs::read(home.join("gateway.lock")).unwrap()).unwrap();
    assert_eq!(lock["pid"], std::process::id());
    let state: Value =
        serde_json::from_slice(&std::fs::read(home.join("gateway_state.json")).unwrap()).unwrap();
    assert_eq!(state["gateway_state"], "running");
    assert_eq!(state["pid"], std::process::id());
    drop(runtime);
    for name in ["gateway.sock", "gateway.lock", "gateway_state.json"] {
        assert!(!home.join(name).exists(), "{name} left behind");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_runtime_for_the_same_home_is_refused() {
    let (_dir, home) = private_home();
    let _runtime = start(&home).await;
    let chat = hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
    let err = local::start(&home, chat).await.err().expect("refused");
    assert!(err.contains("already"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lock_and_socket_left_by_a_dead_owner_are_replaced() {
    let (_dir, home) = private_home();
    let mut child = Command::new("true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    std::fs::write(home.join("gateway.lock"), format!("{{\"pid\": {dead}}}")).unwrap();
    let stale = std::os::unix::net::UnixListener::bind(home.join("gateway.sock")).unwrap();
    drop(stale);
    let runtime = start(&home).await;
    assert_eq!(
        identify(&home)["result"]["instance_id"],
        runtime.instance_id()
    );
}

/// As upstream, the runtime still starts (its messaging side needs no ticket), but its socket
/// mints nothing and ensure names the repair.
#[tokio::test(flavor = "multi_thread")]
async fn a_home_other_users_can_write_mints_no_tickets_and_ensure_refuses_it() {
    let (_dir, home) = private_home();
    let runtime = start(&home).await;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o775)).unwrap();
    assert_eq!(identify(&home)["ok"], true);
    let reply = control(
        &home,
        ticket_request(&home, runtime.instance_id(), "interactive"),
    );
    assert_eq!(reply["ok"], false, "{reply}");
    let probe = home.clone();
    let (verdict, text, code) = tokio::task::spawn_blocking(move || {
        ensure(&probe, Duration::from_secs(2), never_spawned())
    })
    .await
    .unwrap();
    assert_eq!(code, 7, "{text}");
    assert_eq!(verdict["reason_code"], "unsafe_control_permissions");
    assert!(
        verdict["detail"].as_str().unwrap().contains("chmod"),
        "{text}"
    );
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interactive_ticket_opens_the_socket_once_and_without_an_origin_only() {
    let (_dir, home) = private_home();
    let runtime = start(&home).await;
    let origin = runtime.api_origin().to_string();
    let reply = control(
        &home,
        ticket_request(&home, runtime.instance_id(), "interactive"),
    );
    let result = &reply["result"];
    assert_eq!(result["expires_in_seconds"], 30);
    assert_eq!(result["instance_id"], runtime.instance_id());
    assert_eq!(result["profile_id"], home.to_str().unwrap());
    assert_eq!(result["runtime_protocol"], 1);
    let ticket = result["ticket"].as_str().unwrap();
    assert!(ticket.len() >= 32);

    let url = ws_url(&origin, &format!("?native_dial=n1&ticket={ticket}"));
    let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(text) = first else {
        panic!("{first:?}")
    };
    let ready: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(ready["params"]["type"], "gateway.ready");
    // Used once.
    assert_eq!(
        refused_status(url.as_str().into_client_request().unwrap()).await,
        401
    );

    // A ticket presented with any Origin is refused, and spent.
    let ticket = mint(&home, runtime.instance_id(), "interactive");
    let url = ws_url(&origin, &format!("?ticket={ticket}"));
    let mut request = url.as_str().into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Origin", "file://".parse().unwrap());
    assert_eq!(refused_status(request).await, 403);
    assert_eq!(
        refused_status(url.as_str().into_client_request().unwrap()).await,
        401
    );

    // A REST ticket does not open the socket, and the remote token path is off.
    let ticket = mint(&home, runtime.instance_id(), "native-http");
    let url = ws_url(&origin, &format!("?ticket={ticket}"));
    assert_eq!(
        refused_status(url.as_str().into_client_request().unwrap()).await,
        401
    );
    for query in ["", "?token=", "?ticket=", "?ticket=made-up"] {
        let url = ws_url(&origin, query);
        assert_eq!(
            refused_status(url.as_str().into_client_request().unwrap()).await,
            401,
            "{query}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_needs_a_one_use_native_http_ticket_and_no_origin() {
    let (_dir, home) = private_home();
    let runtime = start(&home).await;
    let url = format!("{}/api/sessions/none/messages", runtime.api_origin());
    let client = reqwest::Client::new();
    let get = |ticket: Option<&str>, origin: Option<&str>| {
        let mut request = client.get(&url);
        if let Some(ticket) = ticket {
            request = request.header("X-Hermes-Gateway-Ticket", ticket);
        }
        if let Some(origin) = origin {
            request = request.header("Origin", origin);
        }
        async move { request.send().await.unwrap().status().as_u16() }
    };
    assert_eq!(get(None, None).await, 401);
    let interactive = mint(&home, runtime.instance_id(), "interactive");
    assert_eq!(get(Some(&interactive), None).await, 401);
    let ticket = mint(&home, runtime.instance_id(), "native-http");
    // Past authentication: the empty home answers with no messages.
    assert_eq!(get(Some(&ticket), None).await, 200);
    assert_eq!(get(Some(&ticket), None).await, 401);
    let ticket = mint(&home, runtime.instance_id(), "native-http");
    assert_eq!(get(Some(&ticket), Some("file://")).await, 403);
    assert_eq!(get(Some(&ticket), None).await, 401);
    // The remote token header opens nothing here.
    let mut request = client.get(&url);
    request = request.header("X-Hermes-Session-Token", "x".repeat(64));
    assert_eq!(request.send().await.unwrap().status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn session_ticket_refuses_anything_but_upstreams_exact_request() {
    let (_dir, home) = private_home();
    let runtime = start(&home).await;
    let instance = runtime.instance_id().to_string();
    let home_text = home.to_str().unwrap().to_string();
    let good = ticket_request(&home, &instance, "interactive");
    let mut cases = vec![
        (
            "stale instance",
            ticket_request(&home, "other", "interactive"),
        ),
        (
            "unknown purpose",
            ticket_request(&home, &instance, "native-profile-create"),
        ),
        ("protocol 2", {
            let mut r = good.clone();
            r["protocol"] = json!(2);
            r
        }),
        ("protocol as text", {
            let mut r = good.clone();
            r["protocol"] = json!("1");
            r
        }),
        ("extra envelope key", {
            let mut r = good.clone();
            r["extra"] = json!(true);
            r
        }),
        ("params not an object", {
            let mut r = good.clone();
            r["params"] = json!([]);
            r
        }),
        (
            "another profile",
            json!({"protocol": 1, "id": 1, "verb": "session-ticket", "params": {
            "profile_id": format!("{home_text}/profiles/work"), "instance_id": instance,
            "purpose": "interactive"}}),
        ),
        ("extra param", {
            let mut r = good.clone();
            r["params"]["extra"] = json!(1);
            r
        }),
        ("forwarded subject", {
            let mut r = good.clone();
            r["params"]["subject"] = json!("uid:0");
            r
        }),
        ("unknown scope", {
            let mut r = good.clone();
            r["params"]["scope"] = json!("everything");
            r
        }),
        ("host scope for REST", {
            let mut r = ticket_request(&home, &instance, "native-http");
            r["params"]["scope"] = json!("host");
            r
        }),
        ("restart_wait other than refuse", {
            let mut r = good.clone();
            r["params"]["restart_wait"] = json!("accept");
            r
        }),
    ];
    for missing in ["profile_id", "instance_id", "purpose"] {
        let mut r = good.clone();
        r["params"].as_object_mut().unwrap().remove(missing);
        cases.push(("missing param", r));
    }
    for (label, request) in cases {
        let reply = control(&home, request);
        assert_eq!(reply["ok"], false, "{label}: {reply}");
        assert!(reply["result"].is_null(), "{label}: {reply}");
    }
    // The desktop's own interactive request asks for a host-scoped grant.
    let mut host = good.clone();
    host["params"]["scope"] = json!("host");
    assert_eq!(control(&home, host)["ok"], true);
    let unknown = control(&home, json!({"protocol": 1, "id": 7, "verb": "nope"}));
    assert_eq!(unknown["ok"], false);
    assert_eq!(unknown["id"], 7);
}

fn ensure(home: &Path, timeout: Duration, command: Command) -> (Value, String, i32) {
    let verdict = local::ensure(home, timeout, command);
    let text = verdict.to_json();
    (
        serde_json::from_str(&text).unwrap(),
        text,
        verdict.exit_code(),
    )
}

fn never_spawned() -> Command {
    Command::new("/nonexistent/should-not-run")
}

#[tokio::test(flavor = "multi_thread")]
async fn ensure_reports_a_running_owner_in_upstreams_verdict_shape() {
    let (_dir, home) = private_home();
    let runtime = start(&home).await;
    let probe = home.clone();
    let (verdict, text, code) = tokio::task::spawn_blocking(move || {
        ensure(&probe, Duration::from_secs(5), never_spawned())
    })
    .await
    .unwrap();
    assert_eq!(code, 0, "{text}");
    let home_text = home.to_str().unwrap();
    assert_eq!(
        verdict,
        json!({
            "client_code_sha": null,
            "detail": null,
            "endpoint": {
                "api_origin": runtime.api_origin(),
                "authority_epoch": 1,
                "capabilities": ["session-authority-v1"],
                "code_sha": null,
                "control_home": null,
                "instance_id": runtime.instance_id(),
                "profile_id": home_text,
                "runtime_protocol": 1,
                "supervisor": "none",
            },
            "reason_code": null,
            "state": "ready",
        })
    );
    // One compact line with sorted keys, as `json.dumps(..., separators=(",", ":"), sort_keys=True)`.
    assert!(!text.contains('\n') && !text.contains(": "), "{text}");
    let keys = [
        "client_code_sha",
        "detail",
        "endpoint",
        "reason_code",
        "state",
    ];
    let positions: Vec<usize> = keys
        .iter()
        .map(|k| text.find(&format!("\"{k}\":")).unwrap())
        .collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "{text}");
}

#[test]
fn ensure_reports_a_start_that_dies_with_its_own_output() {
    let (_dir, home) = private_home();
    let mut command = Command::new("sh");
    command.args(["-c", "echo first; echo the start failed here >&2; exit 3"]);
    let (verdict, text, code) = ensure(&home, Duration::from_secs(10), command);
    assert_eq!(code, 7, "{text}");
    assert_eq!(verdict["state"], "inaccessible");
    assert_eq!(verdict["reason_code"], "runtime_exited");
    assert!(verdict["endpoint"].is_null());
    let detail = verdict["detail"].as_str().unwrap();
    assert!(detail.contains("the start failed here"), "{detail}");
}

#[test]
fn ensure_answers_starting_at_its_deadline_and_starts_one_owner_only() {
    let (_dir, home) = private_home();
    let marker = home.join("starts");
    let mut command = Command::new("sh");
    command.args(["-c", &format!("echo x >> '{}'; sleep 5", marker.display())]);
    let (verdict, text, code) = ensure(&home, Duration::from_secs(1), command);
    assert_eq!(code, 5, "{text}");
    assert_eq!(verdict["state"], "starting");
    assert_eq!(verdict["reason_code"], "deadline");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "x\n");
}

#[test]
fn ensure_waits_for_a_live_lock_holder_instead_of_starting_another() {
    let (_dir, home) = private_home();
    let mut holder = Command::new("sleep").arg("5").spawn().unwrap();
    std::fs::write(
        home.join("gateway.lock"),
        format!("{{\"pid\": {}}}", holder.id()),
    )
    .unwrap();
    let (verdict, text, code) = ensure(&home, Duration::from_millis(500), never_spawned());
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(code, 5, "{text}");
    assert_eq!(verdict["state"], "starting");
}

#[test]
fn ensure_refuses_a_control_socket_other_users_can_reach() {
    let (_dir, home) = private_home();
    let _listener = std::os::unix::net::UnixListener::bind(home.join("gateway.sock")).unwrap();
    std::fs::set_permissions(
        home.join("gateway.sock"),
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    let (verdict, text, code) = ensure(&home, Duration::from_secs(2), never_spawned());
    assert_eq!(code, 7, "{text}");
    assert_eq!(verdict["reason_code"], "unsafe_control_permissions");
}
