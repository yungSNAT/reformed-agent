use super::*;
use hermes_config::managed_gateway::test_lock;

struct EnvScope {
    original: Vec<(&'static str, Option<String>)>,
    _g: std::sync::MutexGuard<'static, ()>,
}

impl EnvScope {
    fn new() -> Self {
        let g = test_lock::lock();
        let keys = [
            "HERMES_BROWSER_BACKEND",
            "BROWSER_CLOUD_PROVIDER",
            "BROWSER_PROVIDER",
            "BROWSERBASE_API_KEY",
            "BROWSERBASE_PROJECT_ID",
            "BROWSERBASE_BASE_URL",
            "BROWSERBASE_PROXIES",
            "BROWSERBASE_ADVANCED_STEALTH",
            "BROWSERBASE_KEEP_ALIVE",
            "BROWSERBASE_SESSION_TIMEOUT",
            "BROWSER_USE_API_KEY",
            "BROWSER_USE_GATEWAY_URL",
            "FIRECRAWL_API_KEY",
            "FIRECRAWL_API_URL",
            "FIRECRAWL_BROWSER_TTL",
            "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
            "HERMES_TASK_ID",
            "HERMES_HOME",
            "HERMES_AGENT_ULTRA_HOME",
            "TOOL_GATEWAY_USER_TOKEN",
            "TOOL_GATEWAY_DOMAIN",
            "TOOL_GATEWAY_SCHEME",
            "CAMOFOX_URL",
            "CAMOFOX_CDP_URL",
            "CAMOFOX_PROFILE",
            "CHROME_CDP_URL",
            "BROWSER_CDP_URL",
            "CAMOFOX_REWRITE_LOOPBACK_URLS",
            "CAMOFOX_LOOPBACK_HOST_ALIAS",
            "HERMES_BROWSER_COMMAND_TIMEOUT_SECONDS",
            "BROWSER_COMMAND_TIMEOUT",
        ];
        let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
        for k in &keys {
            std::env::remove_var(k);
        }
        Self { original, _g: g }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (k, v) in &self.original {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

#[test]
fn camofox_mode_env_honors_cdp_override() {
    let _scope = EnvScope::new();
    std::env::set_var("CAMOFOX_URL", "http://localhost:9377");
    assert!(camofox_mode_enabled_from_env());
    assert_eq!(browser_backend_choice_from_env(), "camofox");

    std::env::set_var(
        "BROWSER_CDP_URL",
        "ws://127.0.0.1:9222/devtools/browser/abc",
    );
    assert!(!camofox_mode_enabled_from_env());
    assert_eq!(browser_backend_choice_from_env(), "cdp");

    std::env::set_var("BROWSER_CDP_URL", "  ");
    assert!(camofox_mode_enabled_from_env());
}

#[test]
fn camofox_identity_is_profile_scoped_and_task_stable() {
    let profile_a = tempfile::tempdir().expect("profile a");
    let profile_b = tempfile::tempdir().expect("profile b");

    assert_eq!(
        camofox_state_dir_for_home(profile_a.path()),
        profile_a.path().join("browser_auth").join("camofox")
    );
    let first = camofox_identity_for_home(profile_a.path(), Some("task-1"));
    let second = camofox_identity_for_home(profile_a.path(), Some("task-1"));
    let other_task = camofox_identity_for_home(profile_a.path(), Some("task-2"));
    let other_profile = camofox_identity_for_home(profile_b.path(), Some("task-1"));

    assert_eq!(first, second);
    assert!(first.user_id.starts_with("hermes_"));
    assert!(first.session_key.starts_with("task_"));
    assert_eq!(first.user_id, other_task.user_id);
    assert_ne!(first.session_key, other_task.session_key);
    assert_ne!(first.user_id, other_profile.user_id);
}

#[test]
fn camofox_from_env_uses_cdp_endpoint_and_named_profile() {
    let _scope = EnvScope::new();
    std::env::set_var(
        "CAMOFOX_CDP_URL",
        "http://127.0.0.1:9333/devtools/browser/local",
    );
    std::env::set_var("CAMOFOX_PROFILE", "qa-profile");

    let backend = CamoFoxBrowserBackend::from_env();

    assert_eq!(
        backend.inner.endpoint,
        "http://127.0.0.1:9333/devtools/browser/local"
    );
    assert_eq!(backend.profile, "qa-profile");
}

#[test]
fn camofox_loopback_rewrite_is_opt_in_and_preserves_url_parts() {
    let (unchanged, metadata) = rewrite_loopback_url_for_camofox(
        "http://127.0.0.1:8766/#settings",
        false,
        "host.docker.internal",
    );
    assert_eq!(unchanged, "http://127.0.0.1:8766/#settings");
    assert!(metadata.is_none());

    let (rewritten, metadata) = rewrite_loopback_url_for_camofox(
        "http://127.0.0.1:8766/path?q=1#settings",
        true,
        "host.docker.internal",
    );
    let metadata = metadata.expect("rewrite metadata");
    assert_eq!(
        rewritten,
        "http://host.docker.internal:8766/path?q=1#settings"
    );
    assert_eq!(metadata.from, "127.0.0.1");
    assert_eq!(metadata.to, "host.docker.internal");
    assert_eq!(
        metadata.original_url,
        "http://127.0.0.1:8766/path?q=1#settings"
    );
    assert_eq!(metadata.rewritten_url, rewritten);

    let (rewritten_v6, metadata_v6) =
        rewrite_loopback_url_for_camofox("http://[::1]:8080/path", true, "192.168.1.10");
    assert_eq!(rewritten_v6, "http://192.168.1.10:8080/path");
    assert_eq!(metadata_v6.expect("v6 rewrite").from, "::1");

    let (public_url, public_metadata) = rewrite_loopback_url_for_camofox(
        "https://example.com:8443/path?q=1#top",
        true,
        "host.docker.internal",
    );
    assert_eq!(public_url, "https://example.com:8443/path?q=1#top");
    assert!(public_metadata.is_none());
}

#[test]
fn browser_url_secret_exfiltration_guard_blocks_sensitive_query_params() {
    let err = validate_url_does_not_exfiltrate_secret(
        "https://example.com/callback?api_key=sk-abcdef1234567890",
    )
    .expect_err("api key should be blocked");
    assert!(err.to_string().contains("api_key"));
    assert!(err.to_string().contains("API key or token"));

    let err = validate_url_does_not_exfiltrate_secret(
        "https://openrouter.ai/callback?token=or-abcdef1234567890",
    )
    .expect_err("token should be blocked");
    assert!(err.to_string().contains("token"));

    validate_url_does_not_exfiltrate_secret("https://example.com/search?q=api_key docs")
        .expect("normal search URL should be allowed");
}

#[test]
fn browser_observation_redaction_removes_secret_values() {
    let redacted = redact_browser_observation(
        "Dashboard api_key = FAKESECRETVALUE1234567890 token: ghp_fakeToken1234567890 Authorization: Bearer abcdefghijklmnop",
    );
    assert!(!redacted.contains("FAKESECRETVALUE1234567890"));
    assert!(!redacted.contains("ghp_fakeToken1234567890"));
    assert!(!redacted.contains("abcdefghijklmnop"));
    assert!(redacted.contains("Dashboard"));
    assert!(redacted.contains("[REDACTED]"));
}

#[test]
fn browser_cloud_fallback_response_preserves_local_success_with_metadata() {
    let local = json!({
        "status": "navigated",
        "url": "https://example.com",
        "features": {"local": true}
    })
    .to_string();
    let rendered = browser_fallback_response(local, "BrowserUseProvider", "401 Unauthorized");
    let value: Value = serde_json::from_str(&rendered).expect("fallback json");

    assert_eq!(value["status"], "navigated");
    assert_eq!(value["fallback_from_cloud"], true);
    assert_eq!(value["fallback_provider"], "BrowserUseProvider");
    assert_eq!(value["fallback_reason"], "401 Unauthorized");
    assert_eq!(value["features"]["local"], true);
}

#[test]
fn browser_cdp_override_bypasses_auto_cloud_provider_detection() {
    let _scope = EnvScope::new();
    std::env::set_var("BROWSER_USE_API_KEY", "direct-key");
    assert_eq!(browser_backend_choice_from_env(), "browser-use");

    std::env::set_var("CHROME_CDP_URL", "ws://host:9222/devtools/browser/abc");
    assert_eq!(browser_backend_choice_from_env(), "cdp");

    std::env::set_var("HERMES_BROWSER_BACKEND", "browser-use");
    assert_eq!(browser_backend_choice_from_env(), "browser-use");
}

#[test]
fn browser_use_config_prefers_direct_key_unless_gateway_is_requested() {
    let _scope = EnvScope::new();
    std::env::set_var("BROWSER_USE_API_KEY", "direct-key");
    std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
    std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "nous-token");
    std::env::set_var("BROWSER_USE_GATEWAY_URL", "http://127.0.0.1:3009/");
    std::env::set_var("HERMES_TASK_ID", "task-browser-use");

    let cfg = BrowserUseConfig::from_env().expect("browser use direct config");

    assert_eq!(cfg.api_key, "direct-key");
    assert_eq!(cfg.base_url(), BROWSER_USE_BASE_URL_DEFAULT);
    assert!(!cfg.managed_mode());
    assert_eq!(cfg.task_id, "task-browser-use");
}

#[test]
fn browser_use_config_honors_browser_use_gateway_preference() {
    let _scope = EnvScope::new();
    let home = tempfile::tempdir().expect("temp hermes home");
    std::fs::write(
        home.path().join("config.yaml"),
        "browser:\n  cloud_provider: browser-use\n  use_gateway: true\n",
    )
    .expect("write config");
    std::env::set_var("HERMES_HOME", home.path());
    std::env::set_var("BROWSER_USE_API_KEY", "direct-key");
    std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
    std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "nous-token");
    std::env::set_var("BROWSER_USE_GATEWAY_URL", "http://127.0.0.1:3009/");

    let cfg = BrowserUseConfig::from_env().expect("browser use managed config");

    assert_eq!(cfg.api_key, "nous-token");
    assert_eq!(cfg.base_url(), "http://127.0.0.1:3009");
    assert!(cfg.managed_mode());
}

#[test]
fn browser_use_availability_accepts_expired_cached_nous_token() {
    let _scope = EnvScope::new();
    let home = tempfile::tempdir().expect("temp hermes home");
    std::fs::write(
        home.path().join("auth.json"),
        serde_json::to_vec_pretty(&json!({
            "providers": {"nous": {
                "access_token": "expired-but-present",
                "expires_at": "2000-01-01T00:00:00Z",
            }}
        }))
        .expect("auth json serializes"),
    )
    .expect("write auth.json");
    std::env::set_var("HERMES_HOME", home.path());
    std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");

    assert!(BrowserUseConfig::is_configured_from_env_or_managed());
    assert_eq!(browser_backend_choice_from_env(), "browser-use");
}

#[test]
fn browser_use_payload_and_idempotency_rules_match_provider_contract() {
    let cfg = BrowserUseConfig {
        api_key: "key".into(),
        base_url: BROWSER_USE_BASE_URL_DEFAULT.into(),
        managed_mode: true,
        task_id: "task".into(),
    };

    assert_eq!(browser_use_session_payload(false), json!({}));
    assert_eq!(
        browser_use_session_payload(true),
        json!({"timeout": 5, "proxyCountryCode": "us"})
    );
    assert_eq!(
        browser_use_headers(&cfg, Some("browser-use-session-create:abc")),
        vec![
            ("Content-Type", "application/json".to_string()),
            ("X-Browser-Use-API-Key", "key".to_string()),
            (
                "X-Idempotency-Key",
                "browser-use-session-create:abc".to_string()
            ),
        ]
    );
    assert!(browser_use_should_preserve_pending_create_key(
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        ""
    ));
    assert!(browser_use_should_preserve_pending_create_key(
        reqwest::StatusCode::CONFLICT,
        r#"{"error":{"message":"Managed Browser Use session creation is already in progress"}}"#
    ));
    assert!(!browser_use_should_preserve_pending_create_key(
        reqwest::StatusCode::BAD_REQUEST,
        r#"{"error":{"message":"bad request"}}"#
    ));
}

#[tokio::test]
async fn browser_use_create_session_sends_managed_gateway_contract() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
    let addr = listener.local_addr().expect("server addr");
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        loop {
            let n = stream.read(&mut tmp).await.expect("read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&buf);
            if text.contains("\r\n\r\n") && text.contains(r#""proxyCountryCode":"us""#) {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buf).to_string();
        let _ = tx.send(request);
        let body =
            r#"{"id":"bu_local_session_1","connectUrl":"wss://browser-use.example/session"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nx-external-call-id: call-browser-use-1\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write response");
    });

    let cfg = BrowserUseConfig {
        api_key: "nous-token".into(),
        base_url: format!("http://{addr}"),
        managed_mode: true,
        task_id: "task-browser-use-managed".into(),
    };
    let backend = BrowserUseBrowserBackend::new(cfg);
    let session = backend.create_session().await.expect("create session");
    let request = rx.await.expect("captured request").to_ascii_lowercase();

    assert!(request.starts_with("post /browsers "));
    assert!(request.contains("x-browser-use-api-key: nous-token"));
    assert!(request.contains("x-idempotency-key: browser-use-session-create:"));
    assert!(request.contains(r#""timeout":5"#));
    assert!(request.contains(r#""proxycountrycode":"us""#));
    assert_eq!(session.bb_session_id, "bu_local_session_1");
    assert_eq!(session.cdp_url, "wss://browser-use.example/session");
}

#[tokio::test]
async fn browser_use_close_session_sends_stop_patch() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
    let addr = listener.local_addr().expect("server addr");
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        loop {
            let n = stream.read(&mut tmp).await.expect("read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&buf);
            if text.contains("\r\n\r\n") && text.contains(r#""action":"stop""#) {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buf).to_string();
        let _ = tx.send(request);
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await
            .expect("write response");
    });

    let cfg = BrowserUseConfig {
        api_key: "direct-key".into(),
        base_url: format!("http://{addr}"),
        managed_mode: false,
        task_id: "task-browser-use-close".into(),
    };
    let backend = BrowserUseBrowserBackend::new(cfg);

    assert!(backend
        .close_session("bu_local_session_close")
        .await
        .expect("close session"));
    let request = rx.await.expect("captured request").to_ascii_lowercase();
    assert!(request.starts_with("patch /browsers/bu_local_session_close "));
    assert!(request.contains("x-browser-use-api-key: direct-key"));
    assert!(request.contains(r#""action":"stop""#));
}

#[test]
fn browser_backend_choice_accepts_browser_use_env_and_config() {
    let _scope = EnvScope::new();
    std::env::set_var("BROWSER_CLOUD_PROVIDER", "browser_use");
    assert_eq!(browser_backend_choice_from_env(), "browser-use");

    std::env::remove_var("BROWSER_CLOUD_PROVIDER");
    let home = tempfile::tempdir().expect("temp hermes home");
    std::fs::write(
        home.path().join("config.yaml"),
        "browser:\n  cloud_provider: managed-browser\n",
    )
    .expect("write config");
    std::env::set_var("HERMES_HOME", home.path());
    assert_eq!(browser_backend_choice_from_env(), "browser-use");
}

#[test]
fn browser_backend_choice_accepts_firecrawl_env_and_config() {
    let _scope = EnvScope::new();
    std::env::set_var("BROWSER_CLOUD_PROVIDER", "firecrawl");
    assert_eq!(browser_backend_choice_from_env(), "firecrawl");

    std::env::remove_var("BROWSER_CLOUD_PROVIDER");
    std::env::set_var("FIRECRAWL_API_KEY", "fc-key");
    assert_eq!(browser_backend_choice_from_env(), "cdp");

    std::env::remove_var("FIRECRAWL_API_KEY");
    let home = tempfile::tempdir().expect("temp hermes home");
    std::fs::write(
        home.path().join("config.yaml"),
        "browser:\n  cloud_provider: firecrawl\n",
    )
    .expect("write config");
    std::env::set_var("HERMES_HOME", home.path());
    assert_eq!(browser_backend_choice_from_env(), "firecrawl");
}

#[test]
fn firecrawl_browser_config_from_env_matches_provider_contract() {
    let _scope = EnvScope::new();
    std::env::set_var("FIRECRAWL_API_KEY", "fc-key");
    std::env::set_var("FIRECRAWL_API_URL", "https://firecrawl.example.com/");
    std::env::set_var("FIRECRAWL_BROWSER_TTL", "900");
    std::env::set_var("HERMES_TASK_ID", "task-firecrawl");

    let cfg = FirecrawlBrowserConfig::from_env().expect("firecrawl config");

    assert_eq!(cfg.api_key, "fc-key");
    assert_eq!(cfg.base_url(), "https://firecrawl.example.com");
    assert_eq!(cfg.ttl_secs, 900);
    assert_eq!(cfg.task_id, "task-firecrawl");
    assert_eq!(firecrawl_browser_session_payload(&cfg), json!({"ttl": 900}));
}

#[tokio::test]
async fn firecrawl_browser_create_session_sends_v2_browser_contract() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
    let addr = listener.local_addr().expect("server addr");
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        loop {
            let n = stream.read(&mut tmp).await.expect("read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&buf);
            if text.contains("\r\n\r\n") && text.contains(r#""ttl":450"#) {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buf).to_string();
        let _ = tx.send(request);
        let body = r#"{"id":"fc_session_1","cdpUrl":"wss://firecrawl.example/session"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write response");
    });

    let cfg = FirecrawlBrowserConfig {
        api_key: "fc-key".into(),
        base_url: format!("http://{addr}"),
        ttl_secs: 450,
        task_id: "task-firecrawl-create".into(),
    };
    let backend = FirecrawlBrowserBackend::new(cfg);
    let session = backend.create_session().await.expect("create session");
    let request = rx.await.expect("captured request").to_ascii_lowercase();

    assert!(request.starts_with("post /v2/browser "));
    assert!(request.contains("authorization: bearer fc-key"));
    assert!(request.contains("content-type: application/json"));
    assert!(request.contains(r#""ttl":450"#));
    assert_eq!(session.bb_session_id, "fc_session_1");
    assert_eq!(session.cdp_url, "wss://firecrawl.example/session");
}

#[tokio::test]
async fn firecrawl_browser_close_session_uses_delete_endpoint() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
    let addr = listener.local_addr().expect("server addr");
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 1024];
        loop {
            let n = stream.read(&mut tmp).await.expect("read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if String::from_utf8_lossy(&buf).contains("\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buf).to_string();
        let _ = tx.send(request);
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await
            .expect("write response");
    });

    let cfg = FirecrawlBrowserConfig {
        api_key: "fc-key".into(),
        base_url: format!("http://{addr}"),
        ttl_secs: 300,
        task_id: "task-firecrawl-close".into(),
    };
    let backend = FirecrawlBrowserBackend::new(cfg);

    assert!(backend
        .close_session("fc_session_close")
        .await
        .expect("close session"));
    let request = rx.await.expect("captured request").to_ascii_lowercase();
    assert!(request.starts_with("delete /v2/browser/fc_session_close "));
    assert!(request.contains("authorization: bearer fc-key"));
}

#[test]
fn browserbase_config_from_env_normalizes_base_url_and_timeout() {
    let _scope = EnvScope::new();
    std::env::set_var("BROWSERBASE_API_KEY", "bb-key");
    std::env::set_var("BROWSERBASE_PROJECT_ID", "proj");
    std::env::set_var("BROWSERBASE_BASE_URL", "https://proxy.example.com/");
    std::env::set_var("BROWSERBASE_SESSION_TIMEOUT", "30000");
    std::env::set_var("HERMES_TASK_ID", "task-42");

    let cfg = BrowserbaseConfig::from_env().expect("browserbase config");

    assert_eq!(cfg.api_key, "bb-key");
    assert_eq!(cfg.project_id, "proj");
    assert_eq!(cfg.base_url(), "https://proxy.example.com");
    assert_eq!(
        cfg.session_timeout_secs,
        Some(BROWSERBASE_MAX_SESSION_TIMEOUT_SECS)
    );
    assert_eq!(cfg.task_id, "task-42");
}

#[test]
fn browserbase_payload_matches_provider_feature_knobs() {
    let mut cfg = BrowserbaseConfig::new("key".into(), "proj".into());
    cfg.session_timeout_secs = Some(120);
    cfg.advanced_stealth = true;

    assert_eq!(
        browserbase_session_payload(&cfg, false, false),
        json!({
            "projectId": "proj",
            "keepAlive": true,
            "timeout": 120,
            "proxies": true,
            "browserSettings": {"advancedStealth": true},
        })
    );
    assert_eq!(
        browserbase_session_payload(&cfg, true, true),
        json!({
            "projectId": "proj",
            "timeout": 120,
            "browserSettings": {"advancedStealth": true},
        })
    );
}

#[test]
fn browser_vision_payload_is_llm_content_independent() {
    let raw = browser_vision_payload("inspect", json!({"data": "png-bytes"}));
    let value: Value = serde_json::from_str(&raw).expect("vision payload json");

    assert_eq!(value["status"], "vision_analysis");
    assert_eq!(value["instruction"], "inspect");
    assert_eq!(value["screenshot"]["data"], "png-bytes");
    assert_eq!(
        value["note"],
        "Screenshot captured; vision analysis requires LLM integration"
    );
}

#[test]
fn browser_backend_choice_prefers_explicit_provider_then_browserbase_creds() {
    let _scope = EnvScope::new();
    assert_eq!(browser_backend_choice_from_env(), "cdp");

    std::env::set_var("BROWSERBASE_API_KEY", "bb-key");
    std::env::set_var("BROWSERBASE_PROJECT_ID", "proj");
    assert_eq!(browser_backend_choice_from_env(), "browserbase");

    std::env::set_var("HERMES_BROWSER_BACKEND", "camofox");
    assert_eq!(browser_backend_choice_from_env(), "camofox");

    std::env::set_var("BROWSER_CLOUD_PROVIDER", "browserbase");
    assert_eq!(browser_backend_choice_from_env(), "camofox");
}

#[test]
fn browser_backend_choice_accepts_browser_cloud_provider() {
    let _scope = EnvScope::new();
    std::env::set_var("BROWSER_CLOUD_PROVIDER", "browserbase");
    assert_eq!(browser_backend_choice_from_env(), "browserbase");
}

#[test]
fn browser_cdp_open_timeout_uses_first_open_floor_and_env_ceiling() {
    let _scope = EnvScope::new();

    std::env::set_var("HERMES_BROWSER_COMMAND_TIMEOUT_SECONDS", "30");
    assert_eq!(
        cdp_open_timeout(true),
        std::time::Duration::from_secs(MIN_FIRST_CDP_OPEN_TIMEOUT_SECS)
    );
    assert_eq!(
        cdp_open_timeout(false),
        std::time::Duration::from_secs(MIN_CDP_OPEN_TIMEOUT_SECS)
    );

    std::env::set_var("HERMES_BROWSER_COMMAND_TIMEOUT_SECONDS", "180");
    assert_eq!(cdp_open_timeout(true), std::time::Duration::from_secs(180));
    assert_eq!(cdp_open_timeout(false), std::time::Duration::from_secs(180));
}

#[test]
fn browser_cdp_timeout_error_includes_actionable_hints() {
    let rendered = format_cdp_timeout_error(
        "Page.navigate",
        "http://127.0.0.1:9222",
        std::time::Duration::from_secs(120),
    );

    assert!(rendered.contains("timed out after 120 seconds"));
    assert!(rendered.contains("http://127.0.0.1:9222"));
    assert!(rendered.contains("--remote-debugging-port=9222"));
    assert!(rendered.contains("first browser open"));
}

#[test]
fn browser_cdp_command_error_includes_sandbox_hint_when_relevant() {
    let err = format_cdp_command_error(
        "Page.navigate",
        "http://127.0.0.1:9222",
        ToolError::ExecutionFailed("No usable sandbox!".to_string()),
    );
    let rendered = err.to_string();

    assert!(rendered.contains("No usable sandbox"));
    assert!(rendered.contains("--no-sandbox,--disable-dev-shm-usage"));
}

#[tokio::test]
async fn browser_cdp_timeout_path_surfaces_endpoint_and_hints() {
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.expect("accept");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    });

    let backend = CdpBrowserBackend::new(format!("http://{addr}"));
    let err = backend
        .cdp_call(
            "Page.navigate",
            json!({"url": "https://example.com"}),
            &[],
            std::time::Duration::from_millis(25),
        )
        .await
        .expect_err("timeout");
    let rendered = err.to_string();

    assert!(rendered.contains("timed out after 0.025 seconds"));
    assert!(rendered.contains(&addr.to_string()));
    assert!(rendered.contains("--remote-debugging-port=9222"));
}

#[tokio::test]
async fn browser_navigate_failure_is_labeled_failed_to_open() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);

    let backend = CdpBrowserBackend::new(format!("http://{addr}"));
    let err = backend
        .navigate("https://example.com")
        .await
        .expect_err("closed CDP endpoint");
    let rendered = err.to_string();

    assert!(rendered.contains("Failed to open https://example.com"));
    assert!(rendered.contains("--remote-debugging-port=9222"));
    assert!(!backend.first_navigation.load(Ordering::SeqCst));
}

#[tokio::test]
async fn explicit_browserbase_without_credentials_fails_at_runtime() {
    let _scope = EnvScope::new();
    std::env::set_var("HERMES_BROWSER_BACKEND", "browserbase");
    let backend = browser_backend_from_env();
    let err = backend.navigate("https://example.com").await.unwrap_err();
    assert!(err
        .to_string()
        .contains("BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID"));
}

#[tokio::test]
async fn configured_browserbase_without_credentials_fails_at_runtime() {
    let _scope = EnvScope::new();
    let home = tempfile::tempdir().expect("temp hermes home");
    std::fs::write(
        home.path().join("config.yaml"),
        "browser:\n  cloud_provider: browserbase\n",
    )
    .expect("write config");
    std::env::set_var("HERMES_HOME", home.path());

    let backend = browser_backend_from_env();
    let err = backend.navigate("https://example.com").await.unwrap_err();
    assert!(err
        .to_string()
        .contains("BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID"));
}

/// Frames a fixture WebSocket sends back for one command, after an unrelated
/// event and a reply to another id. `None` leaves the command unanswered.
type CdpResponder = std::sync::Arc<dyn Fn(&Value) -> Option<Vec<Value>> + Send + Sync>;

/// The response to `command` carrying `result`, routed back to the same
/// flattened session.
fn cdp_result(command: &Value, result: Value) -> Value {
    let mut reply = json!({"id": command["id"], "result": result});
    if let Some(session_id) = command.get("sessionId") {
        reply["sessionId"] = session_id.clone();
    }
    reply
}

fn answer_all(result: Value) -> CdpResponder {
    std::sync::Arc::new(move |command| Some(vec![cdp_result(command, result.clone())]))
}

/// Loopback WebSocket that accepts any number of connections and records
/// each command it receives with the index of its connection.
async fn spawn_cdp_websocket(
    responder: CdpResponder,
) -> (
    std::net::SocketAddr,
    tokio::sync::mpsc::UnboundedReceiver<(usize, Value)>,
) {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind websocket");
    let addr = listener.local_addr().expect("websocket addr");
    let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut next_index = 0;
        while let Ok((stream, _)) = listener.accept().await {
            let index = next_index;
            next_index += 1;
            let responder = responder.clone();
            let command_tx = command_tx.clone();
            tokio::spawn(async move {
                let mut socket = tokio_tungstenite::accept_async(stream)
                    .await
                    .expect("websocket handshake");
                while let Some(Ok(frame)) = socket.next().await {
                    let text = match frame {
                        Message::Text(text) => text,
                        Message::Close(_) => break,
                        _ => continue,
                    };
                    let command: Value = serde_json::from_str(text.as_str()).expect("command json");
                    let _ = command_tx.send((index, command.clone()));
                    let Some(frames) = responder(&command) else {
                        continue;
                    };
                    let preamble = [
                        json!({"method": "Page.frameNavigated", "params": {"frame": {"id": "F1"}}}),
                        json!({"id": 9_999, "result": {"stale": true}}),
                    ];
                    for message in preamble.into_iter().chain(frames) {
                        if socket
                            .send(Message::text(message.to_string()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            });
        }
    });
    (addr, command_rx)
}

/// Loopback stand-in for a local Chrome: `/json` lists a service worker
/// before the page, as Chrome can, and the page's WebSocket uses `responder`.
async fn spawn_local_cdp(
    responder: CdpResponder,
) -> (
    wiremock::MockServer,
    tokio::sync::mpsc::UnboundedReceiver<(usize, Value)>,
) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (ws_addr, commands) = spawn_cdp_websocket(responder).await;
    let http = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"type": "service_worker", "webSocketDebuggerUrl": "ws://127.0.0.1:1/devtools/page/SW"},
            {"type": "page", "webSocketDebuggerUrl": format!("ws://{ws_addr}/devtools/page/P1")},
        ])))
        .mount(&http)
        .await;
    (http, commands)
}

/// Every command recorded so far, without its connection index.
fn drain_commands(
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<(usize, Value)>,
) -> Vec<Value> {
    std::iter::from_fn(|| commands.try_recv().ok().map(|(_, command)| command)).collect()
}

#[tokio::test]
async fn cdp_command_sends_over_websocket_and_returns_matching_result() {
    let (http, mut commands) = spawn_local_cdp(answer_all(
        json!({"result": {"type": "number", "value": 2}}),
    ))
    .await;
    let backend = CdpBrowserBackend::new(http.uri());

    let result = backend
        .cdp_command("Runtime.evaluate", json!({"expression": "1 + 1"}))
        .await
        .expect("cdp result");

    assert_eq!(result, json!({"result": {"type": "number", "value": 2}}));
    let (_, command) = commands.recv().await.expect("command reached the browser");
    assert_eq!(command["method"], "Runtime.evaluate");
    assert_eq!(command["params"], json!({"expression": "1 + 1"}));
    assert!(command["id"].is_u64());
}

#[tokio::test]
async fn cdp_command_maps_protocol_error_to_tool_error() {
    let (http, _commands) = spawn_local_cdp(std::sync::Arc::new(|command: &Value| {
        Some(vec![json!({
            "id": command["id"],
            "error": {"code": -32_000, "message": "Cannot navigate to invalid URL"},
        })])
    }))
    .await;
    let backend = CdpBrowserBackend::new(http.uri());

    let err = backend
        .cdp_command("Page.navigate", json!({"url": "nonsense"}))
        .await
        .expect_err("cdp error");
    let rendered = err.to_string();

    assert!(rendered.contains("Cannot navigate to invalid URL"));
    assert!(rendered.contains("-32000"));
    assert!(rendered.contains("Page.navigate"));
}

#[tokio::test]
async fn cdp_command_without_page_target_fails() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let http = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"type": "service_worker", "webSocketDebuggerUrl": "ws://127.0.0.1:1/devtools/page/SW"},
        ])))
        .mount(&http)
        .await;
    let backend = CdpBrowserBackend::new(http.uri());

    let err = backend
        .cdp_command("Runtime.evaluate", json!({"expression": "1"}))
        .await
        .expect_err("no page target");

    assert!(err.to_string().contains("No Chrome page target found"));
}

#[tokio::test]
async fn cdp_command_times_out_when_browser_never_answers() {
    let (http, mut commands) = spawn_local_cdp(std::sync::Arc::new(|_: &Value| None)).await;
    let backend = CdpBrowserBackend::new(http.uri());

    let err = backend
        .cdp_call(
            "Runtime.evaluate",
            json!({"expression": "1"}),
            &[],
            std::time::Duration::from_millis(500),
        )
        .await
        .expect_err("silent browser");

    assert!(err.to_string().contains("timed out after 0.500 seconds"));
    let (_, command) = commands.recv().await.expect("command reached the browser");
    assert_eq!(command["method"], "Runtime.evaluate");
}

#[tokio::test]
async fn cdp_navigate_reports_page_navigation_error_text() {
    let (http, _commands) = spawn_local_cdp(answer_all(
        json!({"frameId": "F1", "loaderId": "L1", "errorText": "net::ERR_NAME_NOT_RESOLVED"}),
    ))
    .await;
    let backend = CdpBrowserBackend::new(http.uri());

    let err = backend
        .navigate("https://missing.invalid/")
        .await
        .expect_err("failed navigation");
    let rendered = err.to_string();

    assert!(rendered.contains("Failed to open https://missing.invalid/"));
    assert!(rendered.contains("net::ERR_NAME_NOT_RESOLVED"));
}

#[tokio::test]
async fn cdp_click_reports_script_exception() {
    let (http, _commands) = spawn_local_cdp(answer_all(json!({
        "result": {"type": "object", "subtype": "error"},
        "exceptionDetails": {
            "text": "Uncaught",
            "exception": {"description": "SyntaxError: '##' is not a valid selector"},
        },
    })))
    .await;
    let backend = CdpBrowserBackend::new(http.uri());

    let err = backend.click("##").await.expect_err("script exception");

    assert!(err.to_string().contains("'##' is not a valid selector"));
}

#[tokio::test]
async fn cdp_click_reports_missing_element() {
    let (http, mut commands) = spawn_local_cdp(answer_all(
        json!({"result": {"type": "string", "value": "not found"}}),
    ))
    .await;
    let backend = CdpBrowserBackend::new(http.uri());

    let err = backend.click("#absent").await.expect_err("missing element");

    assert!(matches!(err, ToolError::NotFound(_)));
    assert!(err.to_string().contains("#absent"));
    let (_, command) = commands.recv().await.expect("command reached the browser");
    let expression = command["params"]["expression"]
        .as_str()
        .expect("expression");
    assert!(expression.contains(r##"document.querySelector("#absent")"##));
}

#[tokio::test]
async fn cdp_vision_returns_captured_screenshot_data() {
    let (http, mut commands) = spawn_local_cdp(answer_all(json!({"data": "iVBORw0KGgo="}))).await;
    let backend = CdpBrowserBackend::new(http.uri());

    let raw = backend.vision("describe").await.expect("screenshot");
    let value: Value = serde_json::from_str(&raw).expect("vision json");

    assert_eq!(value["screenshot"]["data"], "iVBORw0KGgo=");
    let (_, command) = commands.recv().await.expect("command reached the browser");
    assert_eq!(command["method"], "Page.captureScreenshot");
}

#[tokio::test]
async fn cdp_vision_accepts_a_reply_larger_than_16_mib() {
    let data = "A".repeat(17 * 1024 * 1024);
    let (http, _commands) = spawn_local_cdp(answer_all(json!({"data": data}))).await;
    let backend = CdpBrowserBackend::new(http.uri());

    let raw = backend.vision("describe").await.expect("large screenshot");
    let value: Value = serde_json::from_str(&raw).expect("vision json");

    assert_eq!(
        value["screenshot"]["data"].as_str().map(str::len),
        Some(data.len())
    );
}

#[tokio::test]
async fn cdp_selector_and_text_are_spliced_as_string_literals() {
    let (http, mut commands) = spawn_local_cdp(answer_all(
        json!({"result": {"type": "string", "value": "typed"}}),
    ))
    .await;
    let backend = CdpBrowserBackend::new(http.uri());
    let selector = "a[title='it\\'s']\n>b";
    let text = "say \"hi\"\\n'\nbye";

    backend.r#type(selector, text).await.expect("typed");
    backend.get_images(Some(selector)).await.expect("images");

    let sent = drain_commands(&mut commands);
    let expressions: Vec<&str> = sent
        .iter()
        .map(|command| {
            command["params"]["expression"]
                .as_str()
                .expect("expression")
        })
        .collect();
    let selector_literal = serde_json::to_string(selector).expect("selector literal");
    let text_literal = serde_json::to_string(text).expect("text literal");
    assert_eq!(expressions.len(), 2);
    assert!(expressions[0].contains(&format!("document.querySelector({selector_literal})")));
    assert!(expressions[0].contains(&format!("el.value = {text_literal};")));
    assert!(expressions[1].contains(&format!("document.querySelectorAll({selector_literal})")));
    assert!(expressions
        .iter()
        .all(|expression| !expression.contains('\n')));
}

#[tokio::test]
async fn cdp_press_sends_key_down_then_key_up() {
    let (http, mut commands) = spawn_local_cdp(answer_all(json!({}))).await;
    let backend = CdpBrowserBackend::new(http.uri());

    for key in ["Enter", "Tab", "ArrowDown", "a"] {
        backend.press(key).await.expect("key press");
    }
    let err = backend.press("NoSuchKey").await.expect_err("unknown key");

    assert!(matches!(err, ToolError::InvalidParams(_)));
    let sent: Vec<Value> = drain_commands(&mut commands)
        .into_iter()
        .inspect(|command| assert_eq!(command["method"], "Input.dispatchKeyEvent"))
        .map(|command| command["params"].clone())
        .collect();
    assert_eq!(
        sent,
        vec![
            json!({"type": "keyDown", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13, "text": "\r"}),
            json!({"type": "keyUp", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13}),
            json!({"type": "rawKeyDown", "key": "Tab", "code": "Tab", "windowsVirtualKeyCode": 9}),
            json!({"type": "keyUp", "key": "Tab", "code": "Tab", "windowsVirtualKeyCode": 9}),
            json!({"type": "rawKeyDown", "key": "ArrowDown", "code": "ArrowDown", "windowsVirtualKeyCode": 40}),
            json!({"type": "keyUp", "key": "ArrowDown", "code": "ArrowDown", "windowsVirtualKeyCode": 40}),
            json!({"type": "keyDown", "key": "a", "text": "a"}),
            json!({"type": "keyUp", "key": "a"}),
        ]
    );
}

#[tokio::test]
async fn cdp_console_read_returns_replayed_messages() {
    let (http, mut commands) = spawn_local_cdp(std::sync::Arc::new(|command: &Value| {
        let mut frames = Vec::new();
        if command["method"] == "Runtime.enable" {
            frames.push(json!({
                "method": "Runtime.executionContextCreated",
                "params": {"context": {"id": 1}},
            }));
            frames.push(json!({
                "method": "Runtime.consoleAPICalled",
                "params": {
                    "type": "log",
                    "args": [{"type": "string", "value": "hello"}, {"type": "number", "value": 2}],
                },
            }));
            frames.push(json!({
                "method": "Runtime.exceptionThrown",
                "params": {
                    "exceptionDetails": {"text": "Uncaught", "exception": {"description": "Error: boom"}},
                },
            }));
        }
        frames.push(cdp_result(command, json!({})));
        Some(frames)
    }))
    .await;
    let backend = CdpBrowserBackend::new(http.uri());

    let raw = backend.console("read").await.expect("console read");
    let value: Value = serde_json::from_str(&raw).expect("console json");

    assert_eq!(value["status"], "console_read");
    assert_eq!(
        value["messages"],
        json!([
            {"type": "log", "text": "hello 2"},
            {"type": "exception", "text": "Error: boom"},
        ])
    );
    let methods: Vec<Value> = drain_commands(&mut commands)
        .into_iter()
        .map(|command| command["method"].clone())
        .collect();
    assert_eq!(
        methods,
        vec![json!("Runtime.enable"), json!("Runtime.disable")]
    );
}

/// A remote browser endpoint with one page target that answers through its
/// flattened session; `page` gives each page command's result.
fn remote_browser(page: fn(&Value) -> Value) -> CdpResponder {
    std::sync::Arc::new(move |command: &Value| {
        let result = match command["method"].as_str() {
            Some("Target.getTargets") => json!({"targetInfos": [
                {"targetId": "SW", "type": "service_worker"},
                {"targetId": "P1", "type": "page"},
            ]}),
            Some("Target.attachToTarget") => json!({"sessionId": "S1"}),
            _ if command["sessionId"] == "S1" => page(command),
            _ => {
                return Some(vec![json!({
                    "id": command["id"],
                    "error": {"code": -32_601, "message": "not attached to a page"},
                })])
            }
        };
        Some(vec![cdp_result(command, result)])
    })
}

/// Vendor API stub that creates one session whose CDP URL is `cdp_url`.
async fn vendor_api(route: &str, cdp_url: String) -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"id": "vendor_session_1", "connectUrl": cdp_url, "cdpUrl": cdp_url}),
        ))
        .mount(&api)
        .await;
    api
}

#[tokio::test]
async fn browserbase_commands_share_one_attached_connection() {
    let (ws_addr, mut commands) =
        spawn_cdp_websocket(remote_browser(|command| match command["method"].as_str() {
            Some("Page.captureScreenshot") => json!({"data": "iVBORw0KGgo="}),
            _ => json!({"nodes": []}),
        }))
        .await;
    let api = vendor_api("/v1/sessions", format!("ws://{ws_addr}/devtools/browser/B")).await;
    let mut cfg = BrowserbaseConfig::new("bb-key".into(), "proj".into());
    cfg.base_url = api.uri();
    let backend = BrowserbaseBrowserBackend::new(cfg);

    let raw = backend.vision("describe").await.expect("screenshot");
    backend.snapshot().await.expect("snapshot");

    let value: Value = serde_json::from_str(&raw).expect("vision json");
    assert_eq!(value["screenshot"]["data"], "iVBORw0KGgo=");
    let mut summary = Vec::new();
    while let Ok((index, command)) = commands.try_recv() {
        assert_eq!(index, 0, "every command uses the first connection");
        summary.push((
            command["method"].clone(),
            command["params"].clone(),
            command["sessionId"].clone(),
        ));
    }
    assert_eq!(
        summary,
        vec![
            (json!("Target.getTargets"), json!({}), Value::Null),
            (
                json!("Target.attachToTarget"),
                json!({"targetId": "P1", "flatten": true}),
                Value::Null
            ),
            (
                json!("Page.captureScreenshot"),
                json!({"format": "png"}),
                json!("S1")
            ),
            (json!("Accessibility.getFullAXTree"), json!({}), json!("S1")),
        ]
    );
}

#[tokio::test]
async fn browser_use_navigate_reports_page_navigation_error_text() {
    let (ws_addr, _commands) = spawn_cdp_websocket(remote_browser(
        |_| json!({"frameId": "F1", "errorText": "net::ERR_CONNECTION_REFUSED"}),
    ))
    .await;
    let api = vendor_api("/browsers", format!("ws://{ws_addr}/devtools/browser/B")).await;
    let mut cfg = BrowserUseConfig::new_direct("bu-key".into());
    cfg.base_url = api.uri();
    let backend = BrowserUseBrowserBackend::new(cfg);

    let err = backend
        .navigate("https://example.com/")
        .await
        .expect_err("failed navigation");

    let rendered = err.to_string();
    assert!(rendered.contains("Failed to open https://example.com/"));
    assert!(rendered.contains("net::ERR_CONNECTION_REFUSED"));
}

#[tokio::test]
async fn firecrawl_click_reports_missing_element() {
    let (ws_addr, _commands) = spawn_cdp_websocket(remote_browser(
        |_| json!({"result": {"type": "string", "value": "not found"}}),
    ))
    .await;
    let api = vendor_api("/v2/browser", format!("ws://{ws_addr}/devtools/browser/B")).await;
    let backend = FirecrawlBrowserBackend::new(FirecrawlBrowserConfig {
        api_key: "fc-key".into(),
        base_url: api.uri(),
        ttl_secs: 300,
        task_id: "task-firecrawl-click".into(),
    });

    let err = backend.click("#absent").await.expect_err("missing element");

    assert!(matches!(err, ToolError::NotFound(_)));
}

#[tokio::test]
async fn browserbase_lost_connection_releases_the_session() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let closed_addr = closed.local_addr().expect("addr");
    drop(closed);
    let api = vendor_api(
        "/v1/sessions",
        format!("ws://{closed_addr}/devtools/browser/B"),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/v1/sessions/vendor_session_1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&api)
        .await;
    let mut cfg = BrowserbaseConfig::new("bb-key".into(), "proj".into());
    cfg.base_url = api.uri();
    let backend = BrowserbaseBrowserBackend::new(cfg);

    let err = backend.snapshot().await.expect_err("unreachable browser");

    assert!(err.to_string().contains("Browserbase browser CDP command"));
    assert!(backend.session.lock().await.is_none());
    api.verify().await;
}

#[tokio::test]
async fn cloud_wss_endpoint_attempts_a_tls_handshake() {
    // A TCP listener that drops each connection: the handshake fails, but
    // only after the client has tried TLS.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    let api = vendor_api("/v1/sessions", format!("wss://{addr}/devtools/browser/B")).await;
    let mut cfg = BrowserbaseConfig::new("bb-key".into(), "proj".into());
    cfg.base_url = api.uri();
    let backend = BrowserbaseBrowserBackend::new(cfg);

    let err = backend.snapshot().await.expect_err("no TLS server");

    let rendered = err.to_string();
    assert!(rendered.contains("Failed to open the CDP WebSocket"));
    assert!(!rendered.contains("TLS support not compiled in"));
}
