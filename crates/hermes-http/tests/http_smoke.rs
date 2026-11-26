use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::LazyLock;
use tokio::sync::Mutex;
use tower::ServiceExt;

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

async fn post_rpc(payload: serde_json::Value) -> serde_json::Value {
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/rpc")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn health_ok() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn metrics_prometheus_has_counters() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let s = String::from_utf8(body.to_vec()).unwrap();
    assert!(s.contains("hermes_llm_requests_total"));
}

#[tokio::test]
async fn command_help_runs_through_gateway() {
    let _ = tracing_subscriber::fmt::try_init();
    let cfg = hermes_config::GatewayConfig::default();
    let state = hermes_http::HttpServerState::build(cfg).await.unwrap();
    let app = hermes_http::router(state);
    let payload = serde_json::json!({ "command": "/help" });
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/commands")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["accepted"], true);
    let out = v["output"].as_str().unwrap();
    assert!(
        out.to_lowercase().contains("help") || out.contains('/'),
        "unexpected output: {}",
        out
    );
}

#[tokio::test]
async fn rpc_project_facts_returns_structured_workspace_data() {
    let _ = tracing_subscriber::fmt::try_init();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname='rpc-facts'\nversion='0.1.0'\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("AGENTS.md"), "# rules").unwrap();

    let payload = serde_json::json!({
        "id": 1,
        "method": "project.facts",
        "params": { "cwd": repo.path() }
    });

    let v = post_rpc(payload).await;

    assert_eq!(v["id"], 1);
    assert_eq!(v["error"], serde_json::Value::Null);
    let facts = &v["result"]["facts"];
    assert_eq!(
        facts["root"],
        repo.path().canonicalize().unwrap().display().to_string()
    );
    assert!(facts["manifests"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "Cargo.toml"));
    assert!(facts["verifyCommands"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "cargo test"));
    assert!(facts["contextFiles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "AGENTS.md"));
}

#[tokio::test]
async fn rpc_project_tree_returns_bounded_entries() {
    let _ = tracing_subscriber::fmt::try_init();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[package]\nname='rpc-tree'\nversion='0.1.0'\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn rpc_tree() {}\n").unwrap();
    std::fs::create_dir_all(repo.path().join("target/debug")).unwrap();
    std::fs::write(repo.path().join("target/debug/ignored"), "ignored").unwrap();
    std::fs::write(repo.path().join("z-extra-a.txt"), "extra").unwrap();
    std::fs::write(repo.path().join("z-extra-b.txt"), "extra").unwrap();

    let payload = serde_json::json!({
        "id": "tree",
        "method": "project.tree",
        "params": {
            "cwd": repo.path(),
            "max_depth": 4,
            "max_entries": 4
        }
    });

    let v = post_rpc(payload).await;

    assert_eq!(v["id"], "tree");
    assert_eq!(v["error"], serde_json::Value::Null);
    let tree = &v["result"]["tree"];
    assert_eq!(tree["status"], "ok");
    assert_eq!(tree["maxEntries"], 4);
    assert_eq!(tree["truncated"], true);
    let paths = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value["path"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(paths.contains(&"Cargo.toml".to_string()));
    assert!(paths.contains(&"src".to_string()));
    assert!(paths.contains(&"src/lib.rs".to_string()));
    assert!(!paths.iter().any(|path| path.starts_with("target")));
}

#[tokio::test]
async fn rpc_verification_status_returns_passive_terminal_evidence() {
    let _ = tracing_subscriber::fmt::try_init();
    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname='rpc-verification'\nversion='0.1.0'\n",
    )
    .unwrap();
    let _home = EnvGuard::set("HERMES_HOME", home.to_str().unwrap());
    hermes_tools::verification_evidence::record_terminal_result(
        "cargo   test -p hermes-http",
        Some(&repo),
        Some("sid-1"),
        0,
        "ok",
    )
    .unwrap();

    let payload = serde_json::json!({
        "id": "verify",
        "method": "verification.status",
        "params": { "cwd": repo, "session_id": "sid-1" }
    });

    let v = post_rpc(payload).await;

    assert_eq!(v["id"], "verify");
    assert_eq!(v["result"]["verification"]["status"], "passed");
    assert_eq!(
        v["result"]["verification"]["evidence"]["canonicalCommand"],
        "cargo test -p hermes-http"
    );
    assert_eq!(
        v["result"]["verification"]["evidence"]["outputPreview"],
        "ok"
    );
}

#[tokio::test]
async fn rpc_llm_oneshot_rejects_missing_prompt_without_provider_call() {
    let _ = tracing_subscriber::fmt::try_init();
    let payload = serde_json::json!({
        "id": "missing",
        "method": "llm.oneshot",
        "params": {}
    });

    let v = post_rpc(payload).await;

    assert_eq!(v["id"], "missing");
    assert_eq!(v["error"]["code"], 4030);
    assert!(v["result"].is_null());
}

const HTTP_FIXTURE_HOME: &str = "HERMES_HTTP_LOCAL_FIXTURE_HOME";
const HTTP_FIXTURE_PROVIDER: &str = "HERMES_HTTP_LOCAL_FIXTURE_PROVIDER";

#[tokio::test]
async fn configured_http_gateway_delivers_and_persists_plain_and_streaming_turns() {
    if let Some(home) = std::env::var_os(HTTP_FIXTURE_HOME) {
        let home = std::path::PathBuf::from(home);
        let base_url = std::env::var(HTTP_FIXTURE_PROVIDER).unwrap();
        let endpoint = reqwest::Url::parse(&base_url).unwrap();
        assert_eq!(endpoint.scheme(), "http");
        assert_eq!(endpoint.host_str(), Some("127.0.0.1"));
        assert!(endpoint.port().is_some());
        for streaming in [false, true] {
            let state_root = home.join(if streaming { "stream" } else { "plain" });
            verify_configured_gateway_turn(&state_root, &base_url, streaming).await;
        }
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let managed = home.path().join("managed");
    std::fs::create_dir(&managed).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = std::sync::Arc::new(Mutex::new(Vec::new()));
    let provider = axum::Router::new()
        .route(
            "/chat/completions",
            axum::routing::post(local_chat_completion),
        )
        .with_state(requests.clone());
    let server = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });

    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "configured_http_gateway_delivers_and_persists_plain_and_streaming_turns",
        ])
        .kill_on_drop(true)
        .env_clear()
        .current_dir(home.path())
        .env(HTTP_FIXTURE_HOME, home.path())
        .env(HTTP_FIXTURE_PROVIDER, base_url)
        .env("HERMES_HOME", home.path())
        .env("HERMES_MANAGED_DIR", managed)
        .env(
            "HERMES_AUTH_FILE",
            home.path().join("empty-fixture-auth.json"),
        )
        .env("HOME", home.path())
        .env("USERPROFILE", home.path());
    if let Some(root) = std::env::var_os("SystemRoot") {
        child.env("SystemRoot", root);
    }
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        child.env(
            "LLVM_PROFILE_FILE",
            std::env::current_dir().unwrap().join(profile),
        );
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(20), child.output())
        .await
        .expect("isolated HTTP gateway test finishes")
        .unwrap();
    server.abort();
    let _ = server.await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed;"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 2);
    for (request, streaming) in requests.iter().zip([false, true]) {
        assert_eq!(request["model"], "deterministic");
        assert_eq!(request["stream"].as_bool().unwrap_or(false), streaming);
        let messages = request["messages"].as_array().unwrap();
        assert!(messages.iter().any(|message| {
            message["role"] == "user" && message["content"] == "Read the HTTP fixture"
        }));
    }
}

async fn local_chat_completion(
    axum::extract::State(requests): axum::extract::State<
        std::sync::Arc<Mutex<Vec<serde_json::Value>>>,
    >,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use serde_json::json;

    let streaming = request["stream"] == true;
    requests.lock().await.push(request);
    if streaming {
        let mut body = String::new();
        for choice in [
            json!({"index": 0, "delta": {"role": "assistant", "content": "Fixture stream delivered."}}),
            json!({"index": 0, "delta": {}, "finish_reason": "stop"}),
        ] {
            let chunk = json!({
                "id": "fixture", "object": "chat.completion.chunk", "created": 0,
                "model": "deterministic", "choices": [choice],
            });
            body.push_str(&format!("data: {chunk}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");
        ([("content-type", "text/event-stream")], body).into_response()
    } else {
        axum::Json(json!({
            "id": "fixture", "object": "chat.completion", "created": 0,
            "model": "deterministic",
            "choices": [{
                "index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "Fixture plain reply."},
            }],
        }))
        .into_response()
    }
}

async fn verify_configured_gateway_turn(home: &std::path::Path, base_url: &str, streaming: bool) {
    use serde_json::json;

    let mut config = hermes_config::GatewayConfig {
        home_dir: Some(home.display().to_string()),
        model: Some("fixture-provider:deterministic".into()),
        ..Default::default()
    };
    config.streaming.enabled = streaming;
    config.sessions.auto_prune = false;
    config.llm_providers.insert(
        "fixture-provider".into(),
        serde_json::from_value(json!({
            "base_url": base_url, "api_key": "local-http-fixture",
            "api_mode": "chat_completions", "discover_models": false,
            "models": ["deterministic"], "request_timeout_seconds": 5,
        }))
        .unwrap(),
    );
    let session_manager = hermes_gateway::SessionManager::new(config.session.clone());
    let app = hermes_http::router(hermes_http::HttpServerState::build(config).await.unwrap());
    let response = app
        .oneshot(
            Request::post("/v1/sessions/path-session/messages")
                .header("content-type", "application/json")
                .header("x-hermes-session-key", "fixture-session")
                .body(Body::from(
                    json!({
                        "text": "Read the HTTP fixture", "provider": "fixture-provider",
                        "model": "deterministic", "user_id": "fixture-user",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let reply = if streaming {
        "Fixture stream delivered."
    } else {
        "Fixture plain reply."
    };
    assert_eq!(response["session_id"], "fixture-session");
    assert!(
        response["reply"].as_str().unwrap().contains(reply),
        "{response}"
    );
    assert_eq!(response["message_count"], 2);

    let session_key =
        session_manager.compose_session_key("http", "fixture-session", "fixture-user");
    let saved = hermes_agent::session_persistence::SessionPersistence::new(home)
        .load_session(&session_key)
        .unwrap();
    assert_eq!(
        saved.last().and_then(|message| message.content.as_deref()),
        Some(reply)
    );
}
