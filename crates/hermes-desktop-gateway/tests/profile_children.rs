//! Named profiles served from child gateways (`serve_with_profiles`): a socket opened with
//! `?profile=<name>` and a request naming a profile reach that profile's own process, with its
//! own keys and sessions, and the number of children is bounded. Refusals first.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use futures::{SinkExt, StreamExt};
use hermes_desktop_gateway::{Chat, ChildGateways};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{self, Message};

const TOKEN: &str = "0123456789abcdef0123456789abcdef-children-token";
const BIN: &str = env!("CARGO_BIN_EXE_hermes-desktop-gateway");
/// The stem of the key variable every profile's provider reads; each profile's `.env` sets its
/// own value.
const KEY_ENV: &str = "DESKTOP_PROFILE_CHILDREN_TEST_KEY";
/// A key only the parent has (the root's `.env`); no named profile sets it.
const PARENT_KEY_ENV: &str = "DESKTOP_PROFILE_CHILDREN_PARENT_ONLY_KEY";

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A scripted model that answers 'ok' and keeps each request's `Authorization` header.
async fn complete(
    State(seen): State<Arc<Mutex<Vec<String>>>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    seen.lock().unwrap().push(auth.to_string());
    let chunk = json!({"id": "c", "object": "chat.completion.chunk", "model": "mock-model",
        "choices": [{"index": 0, "delta": {"content": "ok"}, "finish_reason": null}]});
    let last = json!({"id": "c", "object": "chat.completion.chunk", "model": "mock-model",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
    let sse = format!("data: {chunk}\n\ndata: {last}\n\ndata: [DONE]\n\n");
    ([("content-type", "text/event-stream")], sse)
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

struct Started {
    addr: SocketAddr,
    root: tempfile::TempDir,
    keys: Arc<Mutex<Vec<String>>>,
}

/// An install whose default profile and named `inbox` and `research` profiles all use the
/// scripted model, each with its own key; served with at most `limit` children.
async fn start(limit: usize) -> Started {
    start_at(Some(limit), "").await
}

/// As [`start`], with the gateway's home at `at` under the install root (`""` for the root) and
/// no children when `limit` is `None`. A further named profile, `bare`, sets no key of its own:
/// its provider reads [`PARENT_KEY_ENV`], which only the root's `.env` sets.
async fn start_at(limit: Option<usize>, at: &str) -> Started {
    // As the binary does: the child environment is taken before any configuration is loaded.
    let children = limit.map(|limit| ChildGateways::new(PathBuf::from(BIN)).with_limit(limit));
    let keys = Arc::new(Mutex::new(Vec::new()));
    let model_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model_listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(complete))
        .with_state(Arc::clone(&keys));
    tokio::spawn(async move { axum::serve(model_listener, app).await });

    // Tests share this process's environment and each gateway loads its home's `.env` into it,
    // so each install gets its own key variable.
    static INSTALLS: AtomicUsize = AtomicUsize::new(0);
    let key_env = format!("{KEY_ENV}_{}", INSTALLS.fetch_add(1, Ordering::Relaxed));
    let root = tempfile::tempdir().unwrap();
    let config = format!(
        "model:\n  default: mock-model\n  provider: mock\nproviders:\n  mock:\n    \
         api: http://{model_addr}/v1\n    name: Mock\n    api_mode: chat_completions\n    \
         key_env: {key_env}\n    models:\n      mock-model: {{}}\n"
    );
    for (home, key) in [
        (root.path().to_path_buf(), "root-key"),
        (root.path().join("profiles/inbox"), "inbox-key"),
        (root.path().join("profiles/research"), "research-key"),
    ] {
        write(&home.join("config.yaml"), &config);
        write(&home.join(".env"), &format!("{key_env}={key}\n"));
    }
    let root_env = format!("{key_env}=root-key\n{PARENT_KEY_ENV}=parent-secret\n");
    write(&root.path().join(".env"), &root_env);
    write(
        &root.path().join("profiles/bare/config.yaml"),
        &config.replace(&key_env, PARENT_KEY_ENV),
    );
    let home = root.path().join(at);
    let loaded = hermes_config::load_config(home.to_str()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let chat = Chat::new(loaded);
    match children {
        Some(children) => tokio::spawn(hermes_desktop_gateway::serve_with_profiles(
            listener,
            TOKEN.to_string(),
            chat,
            children,
        )),
        None => tokio::spawn(hermes_desktop_gateway::serve(
            listener,
            TOKEN.to_string(),
            chat,
        )),
    };
    // The root's `.env` is in this process's environment only when the gateway runs there; the
    // isolation tests need it here as a parent's own key.
    // SAFETY: tests set only this test-only name.
    unsafe { std::env::set_var(PARENT_KEY_ENV, "parent-secret") };
    Started { addr, root, keys }
}

/// Opens `/api/ws` with `query`; the HTTP status when the upgrade is refused.
async fn dial(addr: SocketAddr, query: &str) -> Result<Client, u16> {
    match tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws?{query}")).await {
        Ok((socket, _)) => {
            let mut client = Client {
                socket,
                events: VecDeque::new(),
            };
            assert_eq!(client.next_json().await["params"]["type"], "gateway.ready");
            Ok(client)
        }
        Err(tungstenite::Error::Http(response)) => Err(response.status().as_u16()),
        Err(other) => panic!("dial failed: {other}"),
    }
}

async fn connect(addr: SocketAddr, profile: Option<&str>) -> Client {
    let query = match profile {
        Some(name) => format!("token={TOKEN}&profile={name}"),
        None => format!("token={TOKEN}"),
    };
    dial(addr, &query).await.expect("socket accepted")
}

struct Client {
    socket: Socket,
    events: VecDeque<Value>,
}

impl Client {
    async fn next_json(&mut self) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(30), self.socket.next())
                .await
                .expect("no frame within 30 seconds")
                .expect("socket closed");
            match frame.unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }

    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.socket
            .send(Message::text(request.to_string()))
            .await
            .unwrap();
        loop {
            let frame = self.next_json().await;
            if frame["method"] == "event" {
                self.events.push_back(frame);
                continue;
            }
            assert_eq!(frame["id"], id, "{frame}");
            return frame;
        }
    }

    /// Creates a session (in `profile` when given), runs one turn in it and returns its id.
    async fn turn(&mut self, profile: Option<&str>) -> String {
        let mut params = json!({"source": "desktop", "cols": 96});
        if let Some(name) = profile {
            params["profile"] = json!(name);
        }
        let created = self.call(1, "session.create", params).await;
        let session = created["result"]["session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("{created}"))
            .to_string();
        let mut params = json!({"session_id": session, "text": "hello"});
        if let Some(name) = profile {
            params["profile"] = json!(name);
        }
        let submitted = self.call(2, "prompt.submit", params).await;
        assert!(submitted.get("error").is_none(), "{submitted}");
        loop {
            let frame = match self.events.pop_front() {
                Some(frame) => frame,
                None => self.next_json().await,
            };
            if frame["params"]["type"] == "message.complete" {
                assert_eq!(frame["params"]["session_id"], session.as_str());
                return session;
            }
        }
    }
}

/// The history route's status and message count for `session`, with `profile` when given.
async fn history(addr: SocketAddr, session: &str, profile: Option<&str>) -> (u16, usize) {
    let mut url = format!("http://{addr}/api/sessions/{session}/messages");
    if let Some(name) = profile {
        url.push_str(&format!("?profile={name}"));
    }
    let response = reqwest::Client::new()
        .get(url)
        .header("x-hermes-session-token", TOKEN)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or_default();
    let count = body["messages"].as_array().map_or(0, Vec::len);
    (status, count)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_profile_socket_is_refused_without_the_token_or_for_an_unlisted_profile() {
    let started = start(4).await;
    let addr = started.addr;
    assert_eq!(dial(addr, "profile=inbox").await.err(), Some(401));
    assert_eq!(
        dial(addr, "token=wrong&profile=inbox").await.err(),
        Some(401)
    );
    // Unlisted: no identity marker, and a path.
    std::fs::create_dir_all(started.root.path().join("profiles/ghost")).unwrap();
    for name in ["ghost", "..%2Fetc", "Inbox"] {
        let query = format!("token={TOKEN}&profile={name}");
        assert_eq!(dial(addr, &query).await.err(), Some(404), "{name}");
    }
    let (status, _) = history(addr, "any", Some("ghost")).await;
    assert_eq!(status, 404);
    // On the default socket a request naming an unlisted profile is refused, not served here.
    let mut client = connect(addr, None).await;
    let refused = client
        .call(
            1,
            "session.create",
            json!({"profile": "ghost", "source": "desktop"}),
        )
        .await;
    assert_eq!(refused["error"]["code"], 4000, "{refused}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gateway_without_children_refuses_a_named_profile() {
    let root = tempfile::tempdir().unwrap();
    write(&root.path().join("profiles/inbox/config.yaml"), "");
    let loaded = hermes_config::load_config(root.path().to_str()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        Chat::new(loaded),
    ));
    let query = format!("token={TOKEN}&profile=inbox");
    assert_eq!(dial(addr, &query).await.err(), Some(404));
    // The profile it runs in is its own.
    connect(addr, Some("default")).await;
}

#[test]
fn child_mode_needs_a_token() {
    let output = std::process::Command::new(BIN)
        .args(["--port", "0", "--child"])
        .env_remove("HERMES_DASHBOARD_SESSION_TOKEN")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--child needs"));
}

#[tokio::test(flavor = "multi_thread")]
async fn each_profile_runs_with_its_own_keys_and_keeps_its_own_sessions() {
    let started = start(4).await;
    let addr = started.addr;

    let mut inbox = connect(addr, Some("inbox")).await;
    let in_inbox = inbox.turn(None).await;
    let mut default = connect(addr, None).await;
    let in_default = default.turn(None).await;
    assert_eq!(
        *started.keys.lock().unwrap(),
        ["Bearer inbox-key", "Bearer root-key"]
    );

    // Each turn is kept in its own profile only; a session a profile does not have reads as an
    // empty history there, as the history route answers for any unknown session.
    assert_eq!(history(addr, &in_inbox, Some("inbox")).await, (200, 2));
    assert_eq!(history(addr, &in_inbox, None).await, (200, 0));
    assert_eq!(history(addr, &in_default, None).await, (200, 2));
    assert_eq!(history(addr, &in_default, Some("default")).await, (200, 2));
    assert_eq!(history(addr, &in_default, Some("inbox")).await, (200, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_naming_a_profile_runs_in_that_profile() {
    let started = start(4).await;
    let addr = started.addr;
    // The Webapp keeps the default socket and names the tab's profile in each request.
    let mut client = connect(addr, None).await;
    let in_research = client.turn(Some("research")).await;
    let in_default = client.turn(Some("default")).await;
    assert_eq!(
        *started.keys.lock().unwrap(),
        ["Bearer research-key", "Bearer root-key"]
    );
    assert_eq!(
        history(addr, &in_research, Some("research")).await,
        (200, 2)
    );
    assert_eq!(history(addr, &in_research, None).await, (200, 0));
    assert_eq!(history(addr, &in_default, None).await, (200, 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn children_are_bounded_and_an_idle_one_gives_up_its_slot() {
    let started = start(1).await;
    let addr = started.addr;
    let inbox = connect(addr, Some("inbox")).await;
    let research = format!("token={TOKEN}&profile=research");
    // The only slot is leased: refused without touching the live connection.
    assert_eq!(dial(addr, &research).await.err(), Some(503));
    let mut inbox = inbox;
    inbox.turn(None).await;
    #[cfg(target_os = "linux")]
    let inbox_children = children_in(&started.root.path().join("profiles/inbox"));
    #[cfg(target_os = "linux")]
    assert_eq!(inbox_children.len(), 1, "one child serves inbox");

    // Once the inbox connection closes, its child is idle and gives the slot up.
    drop(inbox);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        match dial(addr, &research).await {
            Ok(mut client) => {
                client.turn(None).await;
                break;
            }
            Err(503) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(status) => panic!("research refused with {status}"),
        }
    }
    // The evicted child was killed and its exit awaited: no process, not even a zombie, is left.
    #[cfg(target_os = "linux")]
    for pid in inbox_children {
        assert_gone(pid).await;
    }
}

/// The processes whose working folder is `home`: the children serving that profile.
#[cfg(target_os = "linux")]
fn children_in(home: &Path) -> Vec<u32> {
    let home = std::fs::canonicalize(home).unwrap();
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| std::fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd == home))
        .collect()
}

/// Waits up to 10 s for process `pid` to be gone and reaped (a zombie keeps its `/proc` entry).
#[cfg(target_os = "linux")]
async fn assert_gone(pid: u32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "process {pid} is still there"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_never_gets_a_key_only_the_parent_has() {
    // `bare` sets no key; its provider reads a variable only the parent's environment holds.
    // The scripted model is on loopback, so a turn without a key still reaches it.
    let started = start(4).await;
    let addr = started.addr;
    let mut client = connect(addr, None).await;
    client.turn(Some("bare")).await;
    let mut scoped = connect(addr, Some("bare")).await;
    scoped.turn(None).await;
    let keys = started.keys.lock().unwrap().clone();
    assert_eq!(keys.len(), 2, "{keys:?}");
    for key in &keys {
        assert!(!key.contains("parent-secret"), "{keys:?}");
        assert!(!key.contains("root-key"), "{keys:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gateway_in_a_profile_serves_that_profile_and_never_runs_default_with_its_keys() {
    let started = start_at(Some(4), "profiles/inbox").await;
    let addr = started.addr;
    let mut client = connect(addr, None).await;
    // `default` is the install root, which this process does not serve: refused, nothing made.
    let refused = client
        .call(
            1,
            "session.create",
            json!({"profile": "default", "source": "desktop"}),
        )
        .await;
    assert_eq!(refused["error"]["code"], 4000, "{refused}");
    // Its own profile, named, runs here with its own keys.
    let own = client.turn(Some("inbox")).await;
    assert_eq!(*started.keys.lock().unwrap(), ["Bearer inbox-key"]);
    assert_eq!(history(addr, &own, Some("inbox")).await, (200, 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_children_the_processs_own_profile_is_served_and_others_refused() {
    let started = start_at(None, "profiles/inbox").await;
    let addr = started.addr;
    let mut client = connect(addr, None).await;
    let own = client.turn(Some("inbox")).await;
    assert_eq!(history(addr, &own, None).await, (200, 2));
    // Nor does its history route read another profile's sessions.
    assert_eq!(history(addr, &own, Some("research")).await.0, 404);
    for name in ["research", "default"] {
        let refused = client
            .call(
                1,
                "session.create",
                json!({"profile": name, "source": "desktop"}),
            )
            .await;
        assert_eq!(refused["error"]["code"], 4000, "{name}: {refused}");
    }
    assert_eq!(*started.keys.lock().unwrap(), ["Bearer inbox-key"]);

    let started = start_at(None, "").await;
    let mut client = connect(started.addr, None).await;
    client.turn(Some("default")).await;
    assert_eq!(*started.keys.lock().unwrap(), ["Bearer root-key"]);
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_never_reports_ready_is_killed_and_reaped() {
    let started = start(4).await;
    let script = started.root.path().join("silent-child.sh");
    // Ignores its arguments, records its pid in its working folder and never prints.
    write(
        &script,
        "#!/bin/sh\necho $$ > \"$PWD/silent.pid\"\nexec sleep 60\n",
    );
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let children = ChildGateways::new(script).with_start_timeout(Duration::from_millis(500));
    let loaded = hermes_config::load_config(started.root.path().to_str()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(hermes_desktop_gateway::serve_with_profiles(
        listener,
        TOKEN.to_string(),
        Chat::new(loaded),
        children,
    ));
    let query = format!("token={TOKEN}&profile=inbox");
    assert_eq!(dial(addr, &query).await.err(), Some(502));
    let pid_file = started.root.path().join("profiles/inbox/silent.pid");
    let pid: u32 = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_gone(pid).await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn a_crashed_child_fails_the_next_request_at_once_and_the_one_after_restarts_it() {
    let started = start(4).await;
    let mut client = connect(started.addr, Some("inbox")).await;
    client.turn(None).await;
    let pids = children_in(&started.root.path().join("profiles/inbox"));
    assert_eq!(pids.len(), 1);
    let killed = std::process::Command::new("kill")
        .args(["-9", &pids[0].to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    // Dead; its exit is collected when the profile is next leased.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while std::fs::read_to_string(format!("/proc/{}/stat", pids[0]))
        .is_ok_and(|stat| !stat.contains(") Z "))
    {
        assert!(tokio::time::Instant::now() < deadline, "child not dead");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let started_at = tokio::time::Instant::now();
    let failed = client
        .call(1, "session.create", json!({"source": "desktop"}))
        .await;
    assert_eq!(failed["error"]["code"], 5032, "{failed}");
    assert!(
        started_at.elapsed() < Duration::from_secs(5),
        "waited for the reply bound"
    );
    client.turn(None).await;
    assert_eq!(
        *started.keys.lock().unwrap(),
        ["Bearer inbox-key", "Bearer inbox-key"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scoped_socket_runs_only_in_its_profile_whatever_a_request_names() {
    let started = start(4).await;
    let addr = started.addr;
    let mut client = connect(addr, Some("research")).await;
    // Another profile, a number, a list: refused, never run here or elsewhere.
    for profile in [
        json!("inbox"),
        json!("default"),
        json!(7),
        json!(["research"]),
    ] {
        let refused = client
            .call(
                1,
                "session.create",
                json!({"profile": profile, "source": "desktop"}),
            )
            .await;
        assert_eq!(refused["error"]["code"], 4000, "{profile}: {refused}");
    }
    // A null profile is no profile: the socket's own.
    let created = client
        .call(
            1,
            "session.create",
            json!({"profile": null, "source": "desktop"}),
        )
        .await;
    let session = created["result"]["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{created}"))
        .to_string();
    let submitted = client
        .call(
            2,
            "prompt.submit",
            json!({"session_id": session, "text": "hello", "profile": null}),
        )
        .await;
    assert!(submitted.get("error").is_none(), "{submitted}");
    loop {
        let frame = match client.events.pop_front() {
            Some(frame) => frame,
            None => client.next_json().await,
        };
        if frame["params"]["type"] == "message.complete" {
            break;
        }
    }
    assert_eq!(*started.keys.lock().unwrap(), ["Bearer research-key"]);
    assert_eq!(history(addr, &session, Some("research")).await, (200, 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn on_the_default_socket_a_profile_that_is_not_a_name_is_refused() {
    let started = start(4).await;
    let mut client = connect(started.addr, None).await;
    for profile in [json!(7), json!({"name": "inbox"})] {
        let refused = client
            .call(
                1,
                "session.create",
                json!({"profile": profile, "source": "desktop"}),
            )
            .await;
        assert_eq!(refused["error"]["code"], 4000, "{profile}: {refused}");
    }
    // Null is no profile: the default socket's own.
    let created = client
        .call(
            1,
            "session.create",
            json!({"profile": null, "source": "desktop"}),
        )
        .await;
    assert!(created["result"]["session_id"].is_string(), "{created}");
}
