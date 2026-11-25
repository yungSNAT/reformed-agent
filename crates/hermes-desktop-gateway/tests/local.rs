//! Local mode, as upstream Hermes Desktop v0.23.2 uses it (apps/desktop/electron/local-gateway.ts):
//! `hermes gateway ensure --json` finds the runtime through `<home>/gateway.sock`'s `identify`,
//! the desktop mints a one-use ticket there with `session-ticket`, then dials
//! `/api/ws?native_dial=<nonce>&ticket=<ticket>` without an Origin and sends each REST call with
//! its own `X-Hermes-Gateway-Ticket`.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use futures::StreamExt;
use hermes_desktop_gateway::local::{self, LocalRuntime};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message};

/// A home under /tmp, whose short path leaves room for the socket wherever TMPDIR points.
fn private_home() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = std::fs::canonicalize(dir.path()).unwrap();
    (dir, home)
}

fn start(home: &Path) -> LocalRuntime {
    // These tests make no turns, so the chat needs no model.
    let chat = hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
    local::start(home, chat).expect("the runtime starts")
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
    let runtime = start(&home);
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
    let runtime = start(&home);
    let socket = std::fs::symlink_metadata(home.join("gateway.sock")).unwrap();
    assert_eq!(socket.mode() & 0o777, 0o600);
    let lock: Value =
        serde_json::from_slice(&std::fs::read(home.join("gateway.lock")).unwrap()).unwrap();
    assert_eq!(lock["pid"], std::process::id());
    let state: Value =
        serde_json::from_slice(&std::fs::read(home.join("gateway_state.json")).unwrap()).unwrap();
    assert_eq!(state["gateway_state"], "running");
    assert_eq!(state["pid"], std::process::id());
    let lock_inode = std::fs::metadata(home.join("gateway.lock")).unwrap().ino();
    drop(runtime);
    for name in ["gateway.sock", "gateway_state.json"] {
        assert!(!home.join(name).exists(), "{name} left behind");
    }
    // The lock file stays, emptied: a stopping owner never unlinks the inode it still locks, so
    // a new claimant always locks that same inode and waits for it.
    let lock = std::fs::metadata(home.join("gateway.lock")).unwrap();
    assert_eq!((lock.ino(), lock.len()), (lock_inode, 0));
    let next = start_when_free(&home).await;
    assert_eq!(
        std::fs::metadata(home.join("gateway.lock")).unwrap().ino(),
        lock_inode
    );
    drop(next);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_runtime_for_the_same_home_is_refused() {
    let (_dir, home) = private_home();
    let _runtime = start(&home);
    let chat = hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
    let err = local::start(&home, chat).err().expect("refused");
    assert!(err.contains("already"), "{err}");
}

/// Holds `<home>/gateway.lock` as a running owner does.
fn hold_lock(home: &Path) -> std::fs::File {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.join("gateway.lock"))
        .unwrap();
    // SAFETY: flock on a descriptor `file` keeps open.
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    file
}

/// The lock names a live process that is no owner, as after a crash and pid reuse: no process
/// holds it, so it is free.
#[tokio::test(flavor = "multi_thread")]
async fn a_lock_and_socket_left_by_a_gone_owner_are_replaced_even_when_its_pid_lives() {
    let (_dir, home) = private_home();
    let mut unrelated = tokio::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    std::fs::write(
        home.join("gateway.lock"),
        format!("{{\"pid\": {}}}", unrelated.id().unwrap()),
    )
    .unwrap();
    let stale = std::os::unix::net::UnixListener::bind(home.join("gateway.sock")).unwrap();
    drop(stale);
    let started = local::start(
        &home,
        hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default()),
    );
    unrelated.kill().await.unwrap();
    let runtime = started.expect("the runtime starts");
    assert_eq!(
        identify(&home)["result"]["instance_id"],
        runtime.instance_id()
    );
    let lock: Value =
        serde_json::from_slice(&std::fs::read(home.join("gateway.lock")).unwrap()).unwrap();
    assert_eq!(lock["pid"], std::process::id());
}

/// A stopping host withdraws local mode at once (no socket, no state file, so ensure cannot
/// report it ready after its pid file is gone) but keeps the home until it exits.
#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_that_stops_serving_keeps_the_home_until_dropped() {
    let (_dir, home) = private_home();
    let mut runtime = start(&home);
    runtime.stop_serving();
    assert!(!home.join("gateway.sock").exists());
    assert!(!home.join("gateway_state.json").exists());
    assert!(local::lock_held(&home), "the home stays claimed");
    let chat = hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
    assert!(local::start(&home, chat).is_err(), "a second owner started");
    drop(runtime);
    assert!(!local::lock_held(&home));
}

/// Connections a stopping host accepted earlier outlive the stop, so they must not be served as
/// a ready owner: no `ready` identify, no new ticket, and no ticket minted earlier opens anything.
#[tokio::test(flavor = "multi_thread")]
async fn connections_accepted_before_a_stop_get_no_ready_owner_and_no_ticket() {
    let (_dir, home) = private_home();
    let mut runtime = start(&home);
    let instance = runtime.instance_id().to_string();
    let url = format!("{}/api/sessions/none/messages", runtime.api_origin());
    let client = reqwest::Client::new();
    // A served request leaves a keep-alive connection in the client's pool.
    let ticket = mint(&home, &instance, "native-http");
    let served = client.get(&url).header("X-Hermes-Gateway-Ticket", &ticket);
    let served = served.send().await.unwrap();
    assert_eq!(served.status(), 200);
    served.bytes().await.unwrap();
    let earlier = mint(&home, &instance, "native-http");
    let connect = || {
        let stream = UnixStream::connect(home.join("gateway.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    };
    let (identify_stream, ticket_stream) = (connect(), connect());
    // Long enough for the control socket to accept both.
    tokio::time::sleep(Duration::from_millis(300)).await;

    runtime.stop_serving();
    let reply = |mut stream: &UnixStream, request: Value| {
        let _ = stream.write_all(format!("{request}\n").as_bytes());
        let mut line = String::new();
        let _ = BufReader::new(stream).read_line(&mut line);
        line
    };
    let identified = reply(
        &identify_stream,
        json!({"protocol": 1, "id": 1, "verb": "identify"}),
    );
    assert!(!identified.contains("\"ready\""), "{identified}");
    let minted = reply(
        &ticket_stream,
        ticket_request(&home, &instance, "native-http"),
    );
    assert!(!minted.contains("\"ticket\""), "{minted}");
    let late = client.get(&url).header("X-Hermes-Gateway-Ticket", &earlier);
    let late = late.send().await.map(|response| response.status().as_u16());
    assert!(!matches!(late, Ok(200)), "{late:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_lock_refuses_a_runtime_and_a_dropped_runtime_frees_it() {
    let (_dir, home) = private_home();
    let held = hold_lock(&home);
    let chat = hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
    let err = local::start(&home, chat).err().expect("refused");
    assert!(err.contains("already"), "{err}");
    drop(held);
    drop(start_when_free(&home).await);
    drop(start_when_free(&home).await);
}

/// Starts a runtime once the lock is free. A child another test forks shares this process's open
/// descriptors until it execs, so a lock can outlive its holder here by a moment.
async fn start_when_free(home: &Path) -> LocalRuntime {
    for _ in 0..100 {
        let chat = hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
        match local::start(home, chat) {
            Ok(runtime) => return runtime,
            Err(err) if err.contains("already") => {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(err) => panic!("{err}"),
        }
    }
    panic!("the lock stayed held for 2 seconds");
}

/// A planted link must not let the run truncate and overwrite another file: a symlink is never
/// followed, and a hard link is refused where other users can write the home (upstream accepts
/// only a regular, single-link file this user owns).
#[tokio::test(flavor = "multi_thread")]
async fn a_lock_path_that_is_a_link_is_refused_and_its_target_left_alone() {
    let (_dir, home) = private_home();
    let target = home.join("precious");
    std::fs::write(&target, "keep").unwrap();
    let lock = home.join("gateway.lock");
    std::os::unix::fs::symlink(&target, &lock).unwrap();
    let chat = || hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
    assert!(local::start(&home, chat()).is_err(), "symlink accepted");
    std::fs::remove_file(&lock).unwrap();
    std::fs::hard_link(&target, &lock).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o770)).unwrap();
    assert!(
        local::start(&home, chat()).is_err(),
        "hard link accepted in a shared home"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
}

/// Another user who can hard-link the lock from elsewhere raises its link count; in a home only
/// this user can write, that must not stop the runtime from starting.
#[tokio::test(flavor = "multi_thread")]
async fn a_hard_link_made_outside_a_private_home_does_not_block_the_claim() {
    let (_dir, home) = private_home();
    drop(start(&home));
    let elsewhere = tempfile::tempdir_in("/tmp").unwrap();
    std::fs::hard_link(home.join("gateway.lock"), elsewhere.path().join("x")).unwrap();
    let runtime = start_when_free(&home).await;
    assert_eq!(
        identify(&home)["result"]["instance_id"],
        runtime.instance_id()
    );
}

#[test]
fn a_home_too_deep_for_a_socket_path_is_named_and_not_started() {
    let (_dir, home) = private_home();
    assert!(local::control_socket_fits(&home));
    let deep = home.join("d".repeat(100));
    std::fs::create_dir(&deep).unwrap();
    std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!local::control_socket_fits(&deep));
    let (verdict, text, code) = ensure(&deep, Duration::from_secs(2), never_spawned());
    assert_eq!(code, 7, "{text}");
    assert_eq!(verdict["state"], "inaccessible");
    assert_eq!(verdict["reason_code"], "control_path_too_long");
    assert!(!deep.join("logs").exists(), "nothing started");
}

/// As upstream, the runtime still starts (its messaging side needs no ticket), but its socket
/// mints nothing and ensure names the repair.
#[tokio::test(flavor = "multi_thread")]
async fn a_home_other_users_can_write_mints_no_tickets_and_ensure_refuses_it() {
    let (_dir, home) = private_home();
    let runtime = start(&home);
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
    let runtime = start(&home);
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
    let runtime = start(&home);
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
    let runtime = start(&home);
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
    let runtime = start(&home);
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
fn ensure_waits_for_a_claimed_home_instead_of_starting_another() {
    let (_dir, home) = private_home();
    // Claimed and not yet served, as `gateway run` holds it until its pid file is written.
    let _claimed = local::claim(&home).unwrap();
    let (verdict, text, code) = ensure(&home, Duration::from_millis(500), never_spawned());
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

/// A lock no runtime can claim (a link, a FIFO) is named at once, as upstream's discovery does,
/// not read as a start in progress until the deadline.
#[test]
fn ensure_names_a_lock_no_runtime_can_claim_at_once() {
    for plant in ["link", "fifo"] {
        let (_dir, home) = private_home();
        let lock = home.join("gateway.lock");
        if plant == "link" {
            std::fs::write(home.join("elsewhere"), "").unwrap();
            std::os::unix::fs::symlink(home.join("elsewhere"), &lock).unwrap();
        } else {
            assert!(Command::new("mkfifo").arg(&lock).status().unwrap().success());
        }
        let started = std::time::Instant::now();
        let (verdict, text, code) = ensure(&home, Duration::from_secs(20), never_spawned());
        assert!(started.elapsed() < Duration::from_secs(10), "{plant}: {text}");
        assert_eq!(code, 7, "{plant}: {text}");
        assert_eq!(verdict["state"], "inaccessible", "{plant}: {text}");
        assert!(verdict["reason_code"].is_null(), "{plant}: {text}");
        let detail = verdict["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("gateway.lock"), "{plant}: {text}");
    }
}

/// Creation sets a lock's mode once, so a lock an older build left under the umask is narrowed.
#[tokio::test(flavor = "multi_thread")]
async fn a_lock_left_with_an_open_mode_is_narrowed_to_0600() {
    let (_dir, home) = private_home();
    let lock = home.join("gateway.lock");
    std::fs::write(&lock, "").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
    let _runtime = start(&home);
    assert_eq!(std::fs::metadata(&lock).unwrap().mode() & 0o777, 0o600);
}

#[tokio::test(flavor = "multi_thread")]
async fn staging_folders_a_killed_bind_left_are_removed_at_start() {
    let (_dir, home) = private_home();
    let stale = home.join(".g0a1b2c");
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(stale.join("gateway.sock"), "").unwrap();
    let unrelated = home.join(".gnupg1");
    std::fs::create_dir(&unrelated).unwrap();
    let _runtime = start(&home);
    assert!(!stale.exists(), "the stale staging folder stayed");
    assert!(unrelated.exists(), "a folder outside the staging pattern went");
}

#[test]
fn ensure_does_not_follow_a_linked_stdio_log() {
    let (_dir, home) = private_home();
    let target = home.join("precious");
    std::fs::write(&target, "keep").unwrap();
    std::fs::create_dir(home.join("logs")).unwrap();
    std::os::unix::fs::symlink(&target, home.join("logs").join("gateway-stdio.log")).unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", "echo written; exit 3"]);
    let (verdict, text, code) = ensure(&home, Duration::from_secs(10), command);
    assert_eq!(code, 7, "{text}");
    assert_eq!(verdict["reason_code"], "runtime_start_failed", "{text}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
}

/// One launch's policy, session and supervisor markers must not become the shared runtime's.
#[test]
fn ensure_starts_the_runtime_without_the_callers_session_and_policy() {
    let (_dir, home) = private_home();
    let seen = home.join("env.txt");
    let mut command = Command::new("sh");
    command
        .args(["-c", &format!("env > '{}'; exit 3", seen.display())])
        .env("HERMES_YOLO_MODE", "1")
        .env("HERMES_SESSION_ID", "s1")
        .env("INVOCATION_ID", "unit")
        .env("HERMES_KEEP_ME", "1");
    let (_, text, code) = ensure(&home, Duration::from_secs(10), command);
    assert_eq!(code, 7, "{text}");
    let env = std::fs::read_to_string(&seen).unwrap();
    for key in ["HERMES_YOLO_MODE=", "HERMES_SESSION_ID=", "INVOCATION_ID="] {
        assert!(
            !env.lines().any(|line| line.starts_with(key)),
            "{key} reached the runtime"
        );
    }
    assert!(env.lines().any(|line| line == "HERMES_KEEP_ME=1"), "{env}");
}
