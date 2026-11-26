//! The model picker's methods (`model.options`, `model.save_key`, `model.disconnect`), `reload.env`
//! and `config.set model`, as upstream's tui_gateway answers them. Refusals first.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "0123456789abcdef0123456789abcdef-model-config-token";
/// The scripted provider's key variable. Tests share the process environment, so the one that
/// changes the key ([`reload_env_makes_the_next_turn_use_the_new_key`]) uses its own.
const KEY_ENV: &str = "DESKTOP_MODEL_CONFIG_TEST_KEY";
const RELOAD_KEY_ENV: &str = "DESKTOP_MODEL_CONFIG_RELOAD_TEST_KEY";
/// A known provider whose key these tests save and remove (`XIAOMI_API_KEY`).
const SAVED: &str = "xiaomi";

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// What the scripted model saw: each request's `(model, Authorization)`.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

async fn complete(
    State(seen): State<Seen>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let model = body["model"].as_str().unwrap_or_default().to_string();
    seen.lock().unwrap().push((model, auth));
    let chunk = json!({"id": "c", "object": "chat.completion.chunk", "model": "m",
        "choices": [{"index": 0, "delta": {"content": "ok"}, "finish_reason": null}]});
    let last = json!({"id": "c", "object": "chat.completion.chunk", "model": "m",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
    let sse = format!("data: {chunk}\n\ndata: {last}\n\ndata: [DONE]\n\n");
    ([("content-type", "text/event-stream")], sse)
}

struct Started {
    addr: SocketAddr,
    home: tempfile::TempDir,
    seen: Seen,
}

async fn start() -> Started {
    start_with(KEY_ENV).await
}

/// A gateway whose home configures the scripted provider `mock` with 2 models and its key,
/// read from `key_env`, in `.env`.
async fn start_with(key_env: &str) -> Started {
    let seen: Seen = Arc::default();
    let model_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_addr = model_listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/chat/completions", post(complete))
        .with_state(Arc::clone(&seen));
    tokio::spawn(async move { axum::serve(model_listener, app).await });

    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        format!(
            "model:\n  default: mock-model\n  provider: mock\nproviders:\n  mock:\n    \
             api: http://{model_addr}/v1\n    name: Mock\n    api_mode: chat_completions\n    \
             key_env: {key_env}\n    models:\n      mock-model: {{}}\n      other-model: {{}}\n"
        ),
    )
    .unwrap();
    std::fs::write(home.path().join(".env"), format!("{key_env}=first-key\n")).unwrap();
    let config = hermes_config::load_config(home.path().to_str()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        hermes_desktop_gateway::Chat::new(config),
    ));
    Started { addr, home, seen }
}

struct Client {
    socket: Socket,
}

async fn connect(addr: SocketAddr) -> Client {
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws?token={TOKEN}"))
        .await
        .unwrap();
    let mut client = Client { socket };
    assert_eq!(client.next_json().await["params"]["type"], "gateway.ready");
    client
}

impl Client {
    async fn next_json(&mut self) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(20), self.socket.next())
                .await
                .expect("no frame within 20 seconds")
                .expect("socket closed");
            match frame.unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }

    /// The reply to one request; events that arrive meanwhile are dropped.
    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.socket
            .send(Message::text(request.to_string()))
            .await
            .unwrap();
        loop {
            let frame = self.next_json().await;
            if frame["method"] != "event" {
                assert_eq!(frame["id"], id, "{frame}");
                return frame;
            }
        }
    }

    async fn create(&mut self) -> String {
        let created = self
            .call(1, "session.create", json!({"source": "desktop"}))
            .await;
        created["result"]["session_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Runs one turn in `session` to its `message.complete`.
    async fn turn(&mut self, session: &str) {
        let params = json!({"session_id": session, "text": "hello"});
        let request =
            json!({"jsonrpc": "2.0", "id": 2, "method": "prompt.submit", "params": params});
        self.socket
            .send(Message::text(request.to_string()))
            .await
            .unwrap();
        loop {
            let frame = self.next_json().await;
            if frame["id"] == 2 {
                assert!(frame.get("error").is_none(), "{frame}");
            }
            if frame["params"]["type"] == "message.complete" {
                assert_eq!(frame["params"]["session_id"], session);
                return;
            }
        }
    }
}

fn code(reply: &Value) -> i64 {
    reply["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("not refused: {reply}"))
}

fn env_lines(home: &Path) -> String {
    std::fs::read_to_string(home.join(".env")).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_requests_are_refused_with_upstreams_codes_and_change_nothing() {
    let started = start().await;
    let mut client = connect(started.addr).await;
    let before = env_lines(started.home.path());

    let reply = client
        .call(1, "model.save_key", json!({"slug": SAVED}))
        .await;
    assert_eq!(code(&reply), 4001);
    let reply = client
        .call(2, "model.save_key", json!({"slug": " ", "api_key": "k"}))
        .await;
    assert_eq!(code(&reply), 4001);
    let reply = client
        .call(
            3,
            "model.save_key",
            json!({"slug": "no-such-provider", "api_key": "k"}),
        )
        .await;
    assert_eq!(code(&reply), 4002);
    // A provider whose key does not come from an environment variable.
    let reply = client
        .call(
            4,
            "model.save_key",
            json!({"slug": "ollama", "api_key": "k"}),
        )
        .await;
    assert_eq!(code(&reply), 4004);
    let reply = client.call(5, "model.disconnect", json!({})).await;
    assert_eq!(code(&reply), 4001);
    let reply = client
        .call(6, "model.disconnect", json!({"slug": SAVED}))
        .await;
    assert_eq!(code(&reply), 4005);
    assert_eq!(env_lines(started.home.path()), before);

    // config.set: a model switch needs a live session; unknown keys and empty values are refused.
    let reply = client
        .call(
            7,
            "config.set",
            json!({"key": "model", "value": "other-model"}),
        )
        .await;
    assert_eq!(code(&reply), 4001);
    let reply = client
        .call(
            8,
            "config.set",
            json!({"session_id": "no-such-session", "key": "model", "value": "other-model"}),
        )
        .await;
    assert_eq!(code(&reply), 4001);
    let session = client.create().await;
    let reply = client
        .call(
            9,
            "config.set",
            json!({"session_id": session, "key": "model", "value": ""}),
        )
        .await;
    assert_eq!(code(&reply), 4002);
    let reply = client
        .call(
            10,
            "config.set",
            json!({"session_id": session, "key": "no.such.key", "value": "1"}),
        )
        .await;
    assert_eq!(code(&reply), 4002);
    let reply = client
        .call(
            11,
            "config.set",
            json!({"session_id": session, "key": "model", "value": "other-model --once"}),
        )
        .await;
    assert_eq!(
        code(&reply),
        4002,
        "a switch flag this gateway does not honour"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn model_options_lists_the_configured_providers_and_the_current_model() {
    let started = start().await;
    let mut client = connect(started.addr).await;

    let reply = client.call(1, "model.options", json!({})).await;
    let result = &reply["result"];
    assert_eq!(result["model"], "mock-model", "{reply}");
    assert_eq!(result["provider"], "mock");
    let rows = result["providers"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "only configured providers by default: {reply}"
    );
    let mock = &rows[0];
    assert_eq!(mock["slug"], "mock");
    assert_eq!(mock["is_current"], true);
    assert_eq!(mock["authenticated"], true);
    assert_eq!(mock["key_env"], KEY_ENV);
    assert_eq!(mock["models"], json!(["mock-model", "other-model"]));
    assert_eq!(mock["total_models"], 2);

    let reply = client
        .call(2, "model.options", json!({"include_unconfigured": true}))
        .await;
    let rows = reply["result"]["providers"].as_array().unwrap();
    let saved = rows
        .iter()
        .find(|row| row["slug"] == SAVED)
        .unwrap_or_else(|| panic!("{reply}"));
    assert_eq!(saved["models"], json!([]));
    assert_eq!(saved["is_current"], false);
    assert_eq!(saved["source"], "canonical");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_key_goes_to_the_profiles_env_file_and_disconnect_removes_it() {
    let started = start().await;
    let home = started.home.path();
    let mut client = connect(started.addr).await;

    let reply = client
        .call(
            1,
            "model.save_key",
            json!({"slug": SAVED, "api_key": "  saved-key  "}),
        )
        .await;
    let row = &reply["result"]["provider"];
    assert_eq!(row["slug"], SAVED, "{reply}");
    assert_eq!(row["authenticated"], true);
    let env = env_lines(home);
    assert!(env.contains(&format!("{KEY_ENV}=first-key\n")), "{env}");
    assert!(env.contains("XIAOMI_API_KEY=saved-key\n"), "{env}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join(".env"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The saved provider now lists as configured.
    let reply = client.call(2, "model.options", json!({})).await;
    let rows = reply["result"]["providers"].as_array().unwrap();
    assert!(rows
        .iter()
        .any(|row| row["slug"] == SAVED && row["authenticated"] == true));

    let reply = client
        .call(3, "model.disconnect", json!({"slug": SAVED}))
        .await;
    assert_eq!(
        reply["result"],
        json!({"slug": SAVED, "name": SAVED, "disconnected": true})
    );
    let env = env_lines(home);
    assert!(!env.contains("XIAOMI_API_KEY"), "{env}");
    assert!(env.contains(&format!("{KEY_ENV}=first-key\n")), "{env}");
    let reply = client
        .call(4, "model.disconnect", json!({"slug": SAVED}))
        .await;
    assert_eq!(code(&reply), 4005);
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_env_makes_the_next_turn_use_the_new_key() {
    let started = start_with(RELOAD_KEY_ENV).await;
    let mut client = connect(started.addr).await;
    let session = client.create().await;
    client.turn(&session).await;

    std::fs::write(
        started.home.path().join(".env"),
        format!("{RELOAD_KEY_ENV}=second-key\n"),
    )
    .unwrap();
    let reply = client.call(3, "reload.env", json!({})).await;
    assert_eq!(reply["result"], json!({"updated": 1}), "{reply}");
    client.turn(&session).await;
    let seen = started.seen.lock().unwrap().clone();
    let keys: Vec<&str> = seen.iter().map(|(_, auth)| auth.as_str()).collect();
    assert_eq!(keys, ["Bearer first-key", "Bearer second-key"]);
    // Nothing changed since: nothing to update.
    let reply = client.call(4, "reload.env", json!({})).await;
    assert_eq!(reply["result"], json!({"updated": 0}));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_switch_changes_only_that_sessions_next_turns() {
    let started = start().await;
    let mut first = connect(started.addr).await;
    let mut second = connect(started.addr).await;
    let switched = first.create().await;
    let other = second.create().await;

    let reply = first
        .call(
            3,
            "config.set",
            json!({"session_id": switched, "key": "model", "value": "other-model --provider mock"}),
        )
        .await;
    assert_eq!(
        reply["result"],
        json!({"key": "model", "value": "other-model", "warning": "",
               "confirm_required": false, "confirm_message": "", "scope": "session"}),
        "{reply}"
    );
    first.turn(&switched).await;
    second.turn(&other).await;
    let seen = started.seen.lock().unwrap().clone();
    let models: Vec<&str> = seen.iter().map(|(model, _)| model.as_str()).collect();
    assert_eq!(models, ["other-model", "mock-model"]);
}
