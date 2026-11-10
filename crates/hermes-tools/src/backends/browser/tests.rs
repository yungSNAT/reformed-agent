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
    spawn_cdp_websocket_recording(responder, |_| true).await
}

/// [`spawn_cdp_websocket`], recording only the commands `record` accepts.
async fn spawn_cdp_websocket_recording(
    responder: CdpResponder,
    record: fn(&Value) -> bool,
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
                    if record(&command) {
                        let _ = command_tx.send((index, command.clone()));
                    }
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

/// Loopback stand-in for a local Chrome: `/json/version` gives the browser
/// WebSocket, whose first page answers through `page` once attached. Only
/// the page's commands are recorded.
async fn spawn_local_cdp(
    page: CdpResponder,
) -> (
    wiremock::MockServer,
    tokio::sync::mpsc::UnboundedReceiver<(usize, Value)>,
) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (ws_addr, commands) = spawn_cdp_websocket_recording(attachable_browser(page), |command| {
        command.get("sessionId").is_some()
    })
    .await;
    let http = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/json/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"webSocketDebuggerUrl": format!("ws://{ws_addr}/devtools/browser/B")}),
        ))
        .mount(&http)
        .await;
    (http, commands)
}

/// A browser endpoint that lists a service worker before its first page, as
/// Chrome can, attaches to that page as session `S1` and passes every other
/// command to `page`.
fn attachable_browser(page: CdpResponder) -> CdpResponder {
    std::sync::Arc::new(move |command: &Value| match command["method"].as_str() {
        Some("Target.getTargets") => Some(vec![cdp_result(
            command,
            json!({"targetInfos": [
                {"targetId": "SW", "type": "service_worker"},
                {"targetId": "P1", "type": "page"},
            ]}),
        )]),
        Some("Target.attachToTarget") => {
            Some(vec![cdp_result(command, json!({"sessionId": "S1"}))])
        }
        _ => page(command),
    })
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
    let (ws_addr, _commands) = spawn_cdp_websocket(std::sync::Arc::new(|command: &Value| {
        Some(vec![cdp_result(
            command,
            json!({"targetInfos": [{"targetId": "SW", "type": "service_worker"}]}),
        )])
    }))
    .await;
    let backend = CdpBrowserBackend::new(format!("ws://{ws_addr}/devtools/browser/B"));

    let err = backend
        .cdp_command("Runtime.evaluate", json!({"expression": "1"}))
        .await
        .expect_err("no page target");

    assert!(err.to_string().contains("has no page target"), "{err}");
}

/// A loopback address with nothing listening on it.
async fn closed_addr() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    listener.local_addr().expect("addr")
}

/// Fail when `rendered` quotes the user info, path or query of a CDP endpoint.
fn assert_no_endpoint_secret(rendered: &str) {
    for secret in ["cdp-user", "cdp-pass", "cdp-path-secret", "cdp-secret"] {
        assert!(!rendered.contains(secret), "{rendered}");
    }
}

#[tokio::test]
async fn local_cdp_http_endpoint_secret_stays_out_of_errors() {
    let closed = closed_addr().await;
    let backend = CdpBrowserBackend::new(format!(
        "http://cdp-user:cdp-pass@{closed}/cdp-path-secret?token=cdp-secret"
    ));
    let err = backend
        .navigate("https://example.com")
        .await
        .expect_err("closed CDP endpoint");
    let rendered = err.to_string();
    assert!(rendered.contains(&format!("http://{closed}")), "{rendered}");
    assert_no_endpoint_secret(&rendered);

    let silent = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let silent_addr = silent.local_addr().expect("addr");
    tokio::spawn(async move {
        let (_stream, _) = silent.accept().await.expect("accept");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    });
    let backend = CdpBrowserBackend::new(format!(
        "http://cdp-user:cdp-pass@{silent_addr}/cdp-path-secret?token=cdp-secret"
    ));
    let err = backend
        .cdp_call(
            "Runtime.evaluate",
            json!({"expression": "1"}),
            &[],
            std::time::Duration::from_millis(25),
        )
        .await
        .expect_err("timeout");
    let rendered = err.to_string();
    assert!(
        rendered.contains(&format!("http://{silent_addr}")),
        "{rendered}"
    );
    assert_no_endpoint_secret(&rendered);
}

#[tokio::test]
async fn local_cdp_http_error_does_not_echo_reason_phrase() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut request = [0; 1024];
        let _ = stream.read(&mut request).await.expect("read request");
        stream
            .write_all(b"HTTP/1.1 401 Unauthorized token=cdp-secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .expect("write response");
    });
    let backend = CdpBrowserBackend::new(format!("http://{address}/?token=cdp-secret"));

    let error = backend
        .snapshot()
        .await
        .expect_err("HTTP error")
        .to_string();

    assert!(error.contains("401"), "{error}");
    assert_no_endpoint_secret(&error);
}

#[tokio::test]
async fn local_cdp_websocket_endpoint_secret_stays_out_of_errors() {
    let closed = closed_addr().await;
    let backend = CdpBrowserBackend::new(format!(
        "ws://cdp-user:cdp-pass@{closed}/devtools/browser/cdp-path-secret?token=cdp-secret"
    ));

    let err = backend.snapshot().await.expect_err("closed CDP endpoint");

    let rendered = err.to_string();
    assert!(rendered.contains(&format!("ws://{closed}")), "{rendered}");
    assert_no_endpoint_secret(&rendered);
}

#[tokio::test]
async fn local_cdp_websocket_endpoint_runs_commands_on_the_first_page() {
    let (ws_addr, mut commands) = spawn_cdp_websocket(remote_browser(
        |_| json!({"result": {"type": "number", "value": 2}}),
    ))
    .await;
    let backend = CdpBrowserBackend::new(format!("ws://{ws_addr}/devtools/browser/B"));

    let result = backend
        .cdp_command("Runtime.evaluate", json!({"expression": "1 + 1"}))
        .await
        .expect("cdp result over a WebSocket endpoint");

    assert_eq!(result, json!({"result": {"type": "number", "value": 2}}));
    let sent: Vec<(Value, Value)> = drain_commands(&mut commands)
        .into_iter()
        .map(|command| (command["method"].clone(), command["sessionId"].clone()))
        .collect();
    assert_eq!(
        sent,
        vec![
            (json!("Target.getTargets"), Value::Null),
            (json!("Target.attachToTarget"), Value::Null),
            (json!("Runtime.evaluate"), json!("S1")),
        ]
    );
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
async fn cdp_screenshot_saves_png_under_hermes_cache() {
    let _scope = EnvScope::new();
    let home = tempfile::tempdir().expect("temp hermes home");
    std::env::set_var("HERMES_HOME", home.path());
    let (http, mut commands) = spawn_local_cdp(answer_all(json!({"data": "iVBORw0KGgo="}))).await;
    let backend = CdpBrowserBackend::new(http.uri());

    let raw = backend.screenshot().await.expect("screenshot");
    let value: Value = serde_json::from_str(&raw).expect("screenshot json");

    let path = std::path::PathBuf::from(value["screenshot_path"].as_str().expect("path"));
    assert!(path.starts_with(home.path().join("cache").join("screenshots")));
    assert_eq!(std::fs::read(&path).expect("saved"), b"\x89PNG\r\n\x1a\n");
    assert!(!raw.contains("iVBORw0KGgo="));
    let (_, command) = commands.recv().await.expect("command reached the browser");
    assert_eq!(command["method"], "Page.captureScreenshot");
}

#[tokio::test]
async fn cdp_screenshot_accepts_a_reply_larger_than_16_mib() {
    let _scope = EnvScope::new();
    let home = tempfile::tempdir().expect("temp hermes home");
    std::env::set_var("HERMES_HOME", home.path());
    let data = "A".repeat(17 * 1024 * 1024);
    let (http, _commands) = spawn_local_cdp(answer_all(json!({"data": data}))).await;
    let backend = CdpBrowserBackend::new(http.uri());

    let raw = backend.screenshot().await.expect("large screenshot");
    let value: Value = serde_json::from_str(&raw).expect("screenshot json");

    let path = value["screenshot_path"].as_str().expect("path");
    let saved = std::fs::metadata(path).expect("saved screenshot").len();
    assert_eq!(saved, (data.len() / 4 * 3) as u64);
}

#[tokio::test]
async fn cdp_screenshot_prunes_screenshots_older_than_a_day() {
    let _scope = EnvScope::new();
    let home = tempfile::tempdir().expect("temp hermes home");
    std::env::set_var("HERMES_HOME", home.path());
    let dir = home.path().join("cache").join("screenshots");
    std::fs::create_dir_all(&dir).expect("screenshot dir");
    let hours_ago =
        |hours: u64| std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600);
    let file_aged = |name: &str, modified: std::time::SystemTime| {
        let path = dir.join(name);
        std::fs::File::create(&path)
            .expect("create file")
            .set_modified(modified)
            .expect("set modified time");
        path
    };
    let old = file_aged("browser_screenshot_old.png", hours_ago(25));
    let recent = file_aged("browser_screenshot_recent.png", hours_ago(23));
    let unrelated = file_aged("notes.png", hours_ago(25));
    let (http, _commands) = spawn_local_cdp(answer_all(json!({"data": "iVBORw0KGgo="}))).await;

    let raw = CdpBrowserBackend::new(http.uri())
        .screenshot()
        .await
        .expect("screenshot");

    let value: Value = serde_json::from_str(&raw).expect("screenshot json");
    assert!(std::path::Path::new(value["screenshot_path"].as_str().expect("path")).exists());
    assert!(!old.exists(), "a screenshot older than a day is removed");
    assert!(recent.exists());
    assert!(unrelated.exists());
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
                "sessionId": command["sessionId"],
                "params": {"context": {"id": 1}},
            }));
            frames.push(json!({
                "method": "Runtime.consoleAPICalled",
                "sessionId": command["sessionId"],
                "params": {
                    "type": "log",
                    "args": [{"type": "string", "value": "hello"}, {"type": "number", "value": 2}],
                },
            }));
            frames.push(json!({
                "method": "Runtime.exceptionThrown",
                "sessionId": command["sessionId"],
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
    attachable_browser(std::sync::Arc::new(move |command: &Value| {
        if command["sessionId"] == "S1" {
            return Some(vec![cdp_result(command, page(command))]);
        }
        Some(vec![json!({
            "id": command["id"],
            "error": {"code": -32_601, "message": "not attached to a page"},
        })])
    }))
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
    let _scope = EnvScope::new();
    let home = tempfile::tempdir().expect("temp hermes home");
    std::env::set_var("HERMES_HOME", home.path());
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

    let raw = backend.screenshot().await.expect("screenshot");
    backend.snapshot().await.expect("snapshot");

    let value: Value = serde_json::from_str(&raw).expect("screenshot json");
    let path = value["screenshot_path"].as_str().expect("path");
    assert_eq!(std::fs::read(path).expect("saved"), b"\x89PNG\r\n\x1a\n");
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

/// Browserbase backend on a vendor stub whose sessions use `browser` and
/// whose session release is expected exactly once.
async fn browserbase_on(
    browser: CdpResponder,
) -> (
    BrowserbaseBrowserBackend,
    wiremock::MockServer,
    tokio::sync::mpsc::UnboundedReceiver<(usize, Value)>,
) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    let (ws_addr, commands) = spawn_cdp_websocket(browser).await;
    let api = vendor_api("/v1/sessions", format!("ws://{ws_addr}/devtools/browser/B")).await;
    Mock::given(method("POST"))
        .and(path("/v1/sessions/vendor_session_1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&api)
        .await;
    let mut cfg = BrowserbaseConfig::new("bb-key".into(), "proj".into());
    cfg.base_url = api.uri();
    (BrowserbaseBrowserBackend::new(cfg), api, commands)
}

/// How many sessions the vendor stub was asked to create at `route`.
async fn session_creations(api: &wiremock::MockServer, route: &str) -> usize {
    api.received_requests()
        .await
        .expect("recorded requests")
        .iter()
        .filter(|request| request.method.as_str() == "POST" && request.url.path() == route)
        .count()
}

#[tokio::test]
async fn cloud_lost_page_session_reconnects_on_the_next_action() {
    // Chrome answers a command for a detached session with -32001 (see
    // `DevToolsSession` in Chromium's content/browser/devtools).
    let attaches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let browser: CdpResponder = {
        let attaches = attaches.clone();
        Arc::new(move |command: &Value| {
            let reply = match command["method"].as_str() {
                Some("Target.getTargets") => cdp_result(
                    command,
                    json!({"targetInfos": [{"targetId": "P1", "type": "page"}]}),
                ),
                Some("Target.attachToTarget") => {
                    let n = attaches.fetch_add(1, Ordering::SeqCst) + 1;
                    cdp_result(command, json!({"sessionId": format!("S{n}")}))
                }
                _ if command["sessionId"] == "S1" => json!({
                    "id": command["id"],
                    "error": {"code": -32_001, "message": "Session with given id not found."},
                }),
                _ => cdp_result(command, json!({"nodes": []})),
            };
            Some(vec![reply])
        })
    };
    let (backend, api, mut commands) = browserbase_on(browser).await;

    let err = backend.snapshot().await.expect_err("page target gone");
    assert!(err.to_string().contains("-32001"), "{err}");
    backend
        .snapshot()
        .await
        .expect("the next action re-attaches");

    assert_eq!(attaches.load(Ordering::SeqCst), 2);
    let page_sessions: Vec<Value> = drain_commands(&mut commands)
        .into_iter()
        .filter(|command| command["method"] == "Accessibility.getFullAXTree")
        .map(|command| command["sessionId"].clone())
        .collect();
    assert_eq!(page_sessions, vec![json!("S1"), json!("S2")]);
    assert_eq!(session_creations(&api, "/v1/sessions").await, 2);
    api.verify().await;
}

#[tokio::test]
async fn queued_cloud_action_waits_for_lost_page_cleanup() {
    use std::sync::atomic::AtomicUsize;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    let created = Arc::new(AtomicUsize::new(0));
    let page_lost = Arc::new(AtomicBool::new(false));
    let browser_created = created.clone();
    let (ws_addr, mut commands) = spawn_cdp_websocket(Arc::new(move |command: &Value| {
        let first_session = browser_created.load(Ordering::SeqCst) == 1;
        let result = match command["method"].as_str() {
            Some("Target.getTargets") if first_session && page_lost.load(Ordering::SeqCst) => {
                cdp_result(command, json!({"targetInfos": []}))
            }
            Some("Target.getTargets") => cdp_result(
                command,
                json!({"targetInfos": [{"targetId": "P1", "type": "page"}]}),
            ),
            Some("Target.attachToTarget") => cdp_result(command, json!({"sessionId": "S1"})),
            _ if first_session => {
                page_lost.store(true, Ordering::SeqCst);
                json!({"id": command["id"], "error": {"code": -32_001, "message": "Session not found"}})
            }
            _ => cdp_result(command, json!({"nodes": []})),
        };
        Some(vec![result])
    }))
    .await;
    let api = MockServer::start().await;
    let vendor_created = created.clone();
    Mock::given(method("POST"))
        .and(path("/v1/sessions"))
        .respond_with(move |_: &Request| {
            let id = vendor_created.fetch_add(1, Ordering::SeqCst) + 1;
            ResponseTemplate::new(200).set_body_json(json!({
                "id": format!("vendor_session_{id}"),
                "connectUrl": format!("ws://{ws_addr}/devtools/browser/B"),
            }))
        })
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/sessions/vendor_session_1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&api)
        .await;
    let mut config = BrowserbaseConfig::new("bb-key".into(), "project".into());
    config.base_url = api.uri();
    let local = CdpBrowserBackend::new(format!("http://{}", closed_addr().await));
    let backend = CloudFallbackBrowserBackend {
        provider: "BrowserbaseProvider",
        cloud: BrowserbaseBrowserBackend::new(config),
        local,
        fallback_reason: Mutex::new(None),
    };
    cloud_connect(&backend.cloud)
        .await
        .expect("initial session");

    let mut first = backend.snapshot();
    let mut queued = backend.snapshot();
    assert!(futures::poll!(&mut first).is_pending());
    assert!(futures::poll!(&mut queued).is_pending());
    let (first, queued) = tokio::join!(first, queued);

    assert!(first.expect_err("lost page").to_string().contains("-32001"));
    queued.expect("the queued action attaches to a fresh cloud session");
    assert_eq!(created.load(Ordering::SeqCst), 2);
    let snapshots = drain_commands(&mut commands)
        .into_iter()
        .filter(|command| command["method"] == "Accessibility.getFullAXTree")
        .count();
    assert_eq!(snapshots, 2, "the lost action was not replayed");
}

#[tokio::test]
async fn cloud_rejected_attach_releases_the_session() {
    let (backend, api, _commands) = browserbase_on(Arc::new(|command: &Value| {
        let reply = match command["method"].as_str() {
            Some("Target.getTargets") => cdp_result(
                command,
                json!({"targetInfos": [{"targetId": "P1", "type": "page"}]}),
            ),
            _ => json!({
                "id": command["id"],
                "error": {"code": -32_602, "message": "No target with given id found"},
            }),
        };
        Some(vec![reply])
    }))
    .await;

    let err = backend.snapshot().await.expect_err("attach rejected");

    assert!(
        err.to_string().contains("No target with given id found"),
        "{err}"
    );
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

#[tokio::test]
async fn cloud_fallback_returns_cloud_page_error_without_touching_local() {
    let (ws_addr, _cloud_commands) = spawn_cdp_websocket(remote_browser(
        |_| json!({"result": {"type": "string", "value": "not found"}}),
    ))
    .await;
    let api = vendor_api("/v2/browser", format!("ws://{ws_addr}/devtools/browser/B")).await;
    let (local_http, mut local_commands) = spawn_local_cdp(answer_all(
        json!({"result": {"type": "string", "value": "clicked"}}),
    ))
    .await;
    let backend = CloudFallbackBrowserBackend {
        provider: "FirecrawlBrowserProvider",
        cloud: FirecrawlBrowserBackend::new(FirecrawlBrowserConfig {
            api_key: "fc-key".into(),
            base_url: api.uri(),
            ttl_secs: 300,
            task_id: "task-fallback-page-error".into(),
        }),
        local: CdpBrowserBackend::new(local_http.uri()),
        fallback_reason: Mutex::new(None),
    };

    let err = backend
        .click("#absent")
        .await
        .expect_err("cloud page error");

    assert!(matches!(err, ToolError::NotFound(_)), "{err}");
    assert!(drain_commands(&mut local_commands).is_empty());
    let local_requests = local_http.received_requests().await.expect("recorded");
    assert!(local_requests.is_empty());
}

#[tokio::test]
async fn cloud_fallback_uses_local_when_cloud_connection_fails() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let closed_addr = closed.local_addr().expect("addr");
    drop(closed);
    let api = vendor_api(
        "/v2/browser",
        format!("ws://{closed_addr}/devtools/browser/B"),
    )
    .await;
    let (local_http, mut local_commands) = spawn_local_cdp(answer_all(
        json!({"result": {"type": "string", "value": "clicked"}}),
    ))
    .await;
    let backend = CloudFallbackBrowserBackend {
        provider: "FirecrawlBrowserProvider",
        cloud: FirecrawlBrowserBackend::new(FirecrawlBrowserConfig {
            api_key: "fc-key".into(),
            base_url: api.uri(),
            ttl_secs: 300,
            task_id: "task-fallback-connection".into(),
        }),
        local: CdpBrowserBackend::new(local_http.uri()),
        fallback_reason: Mutex::new(None),
    };

    let raw = backend.click("#ok").await.expect("local fallback");
    let value: Value = serde_json::from_str(&raw).expect("fallback json");

    assert_eq!(value["status"], "clicked");
    assert_eq!(value["fallback_from_cloud"], true);
    assert_eq!(value["fallback_provider"], "FirecrawlBrowserProvider");
    let sent = drain_commands(&mut local_commands);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["method"], "Runtime.evaluate");
}

#[tokio::test]
async fn cloud_fallback_stays_local_after_the_cloud_connection_fails() {
    let closed = closed_addr().await;
    let api = vendor_api("/v2/browser", format!("ws://{closed}/devtools/browser/B")).await;
    let (local_http, mut local_commands) = spawn_local_cdp(answer_all(
        json!({"result": {"type": "string", "value": "clicked"}}),
    ))
    .await;
    let backend = CloudFallbackBrowserBackend {
        provider: "FirecrawlBrowserProvider",
        cloud: FirecrawlBrowserBackend::new(FirecrawlBrowserConfig {
            api_key: "fc-key".into(),
            base_url: api.uri(),
            ttl_secs: 300,
            task_id: "task-fallback-sticks".into(),
        }),
        local: CdpBrowserBackend::new(local_http.uri()),
        fallback_reason: Mutex::new(None),
    };

    for _ in 0..2 {
        let raw = backend.click("#ok").await.expect("local fallback");
        let value: Value = serde_json::from_str(&raw).expect("fallback json");
        assert_eq!(value["fallback_from_cloud"], true, "{raw}");
    }

    assert_eq!(session_creations(&api, "/v2/browser").await, 1);
    assert_eq!(drain_commands(&mut local_commands).len(), 2);
}

#[tokio::test]
async fn concurrent_browser_actions_share_one_cloud_failure() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/browser"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&api)
        .await;
    let (local_http, mut local_commands) = spawn_local_cdp(answer_all(
        json!({"result": {"type": "string", "value": "clicked"}}),
    ))
    .await;
    let backend = CloudFallbackBrowserBackend {
        provider: "FirecrawlBrowserProvider",
        cloud: FirecrawlBrowserBackend::new(FirecrawlBrowserConfig {
            api_key: "fc-key".into(),
            base_url: api.uri(),
            ttl_secs: 300,
            task_id: "task-parallel-fallback".into(),
        }),
        local: CdpBrowserBackend::new(local_http.uri()),
        fallback_reason: Mutex::new(None),
    };

    let (first, second) = tokio::join!(backend.click("#first"), backend.click("#second"));

    for result in [first, second] {
        let raw = result.expect("local fallback");
        let value: Value = serde_json::from_str(&raw).expect("fallback json");
        assert_eq!(value["fallback_from_cloud"], true, "{raw}");
    }
    assert_eq!(session_creations(&api, "/v2/browser").await, 1);
    assert_eq!(drain_commands(&mut local_commands).len(), 2);
}

/// DevTools discovery stub: `/json/version` answers with `status` and `body`.
async fn cdp_discovery(status: u16, body: Value) -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let discovery = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/json/version"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .expect(1)
        .mount(&discovery)
        .await;
    discovery
}

#[tokio::test]
async fn browser_use_resolves_http_discovery_url_before_connecting() {
    let (ws_addr, mut commands) =
        spawn_cdp_websocket(remote_browser(|_| json!({"nodes": []}))).await;
    let discovery = cdp_discovery(
        200,
        json!({"webSocketDebuggerUrl": format!("ws://{ws_addr}/devtools/browser/B")}),
    )
    .await;
    let api = vendor_api("/browsers", discovery.uri()).await;
    let mut cfg = BrowserUseConfig::new_direct("bu-key".into());
    cfg.base_url = api.uri();
    let backend = BrowserUseBrowserBackend::new(cfg);

    backend
        .snapshot()
        .await
        .expect("snapshot over resolved URL");

    let methods: Vec<Value> = drain_commands(&mut commands)
        .into_iter()
        .map(|command| command["method"].clone())
        .collect();
    assert_eq!(
        methods,
        vec![
            json!("Target.getTargets"),
            json!("Target.attachToTarget"),
            json!("Accessibility.getFullAXTree"),
        ]
    );
    discovery.verify().await;
}

#[tokio::test]
async fn browser_use_failed_discovery_counts_as_unavailable() {
    let discovery = cdp_discovery(500, json!({"error": "down"})).await;
    let api = vendor_api("/browsers", discovery.uri()).await;
    let mut cfg = BrowserUseConfig::new_direct("bu-key".into());
    cfg.base_url = api.uri();
    let (local_http, mut local_commands) = spawn_local_cdp(answer_all(json!({"nodes": []}))).await;
    let backend = CloudFallbackBrowserBackend {
        provider: "BrowserUseProvider",
        cloud: BrowserUseBrowserBackend::new(cfg),
        local: CdpBrowserBackend::new(local_http.uri()),
        fallback_reason: Mutex::new(None),
    };

    let raw = backend.snapshot().await.expect("local fallback");
    let value: Value = serde_json::from_str(&raw).expect("fallback json");

    assert_eq!(value["fallback_from_cloud"], true);
    let reason = value["fallback_reason"].as_str().expect("reason");
    assert!(reason.contains("CDP discovery"), "{reason}");
    assert!(reason.contains("/json/version"), "{reason}");
    assert_eq!(drain_commands(&mut local_commands).len(), 1);
    discovery.verify().await;
}

#[tokio::test]
async fn browser_use_discovery_sends_the_url_query_and_keeps_it_out_of_the_failure() {
    let discovery = cdp_discovery(500, json!({"error": "down"})).await;
    let api = vendor_api(
        "/browsers",
        format!("{}?apiKey=cdp-secret", discovery.uri()),
    )
    .await;
    let mut cfg = BrowserUseConfig::new_direct("bu-key".into());
    cfg.base_url = api.uri();
    let (local_http, _local_commands) = spawn_local_cdp(answer_all(json!({"nodes": []}))).await;
    let backend = CloudFallbackBrowserBackend {
        provider: "BrowserUseProvider",
        cloud: BrowserUseBrowserBackend::new(cfg),
        local: CdpBrowserBackend::new(local_http.uri()),
        fallback_reason: Mutex::new(None),
    };

    let raw = backend.snapshot().await.expect("local fallback");

    assert!(raw.contains("/json/version"), "{raw}");
    assert!(!raw.contains("cdp-secret"), "{raw}");
    let requests = discovery.received_requests().await.expect("recording");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/json/version");
    assert_eq!(requests[0].url.query(), Some("apiKey=cdp-secret"));
}

/// A PNG signature: enough for the tests to check the saved bytes.
const TEST_PNG: &[u8] = b"\x89PNG\r\n\x1a\nfixture";

/// Vision backend stub that records what it was asked and answers `answer`.
struct StubVision {
    answer: &'static str,
    seen: std::sync::Mutex<Vec<(String, String, Vec<u8>)>>,
}

#[async_trait]
impl crate::tools::vision::VisionBackend for StubVision {
    async fn analyze(&self, image_url: &str, question: &str) -> Result<String, ToolError> {
        let bytes = std::fs::read(image_url).expect("screenshot exists when analysed");
        self.seen.lock().expect("stub lock").push((
            image_url.to_string(),
            question.to_string(),
            bytes,
        ));
        Ok(self.answer.to_string())
    }
}

fn vision_handler(
    backend: Arc<dyn BrowserBackend>,
    vision: Result<Arc<dyn crate::tools::vision::VisionBackend>, String>,
) -> crate::tools::browser::BrowserVisionHandler {
    crate::tools::browser::BrowserVisionHandler::new(backend, vision)
}

/// Run `browser_vision` against a local fixture whose screenshot is `TEST_PNG`.
async fn run_browser_vision(
    vision: Result<Arc<dyn crate::tools::vision::VisionBackend>, String>,
) -> (String, Value) {
    use hermes_core::ToolHandler;

    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, TEST_PNG);
    let (http, _commands) = spawn_local_cdp(answer_all(json!({"data": encoded}))).await;
    let handler = vision_handler(Arc::new(CdpBrowserBackend::new(http.uri())), vision);
    let raw = handler
        .execute(json!({"instruction": "Is there a login form?"}))
        .await
        .expect("browser_vision result");
    let value: Value = serde_json::from_str(&raw).expect("vision json");
    (raw, value)
}

#[tokio::test]
async fn browser_vision_returns_analysis_and_saved_screenshot_path() {
    let _scope = EnvScope::new();
    let home = tempfile::tempdir().expect("temp hermes home");
    std::env::set_var("HERMES_HOME", home.path());
    let stub = Arc::new(StubVision {
        answer: "A login form with two fields.",
        seen: std::sync::Mutex::new(Vec::new()),
    });

    let (raw, value) = run_browser_vision(Ok(stub.clone())).await;

    assert_eq!(value["success"], true, "{raw}");
    assert_eq!(value["analysis"], "A login form with two fields.");
    let path = std::path::PathBuf::from(value["screenshot_path"].as_str().expect("path"));
    assert!(path.starts_with(home.path().join("cache").join("screenshots")));
    assert!(path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("browser_screenshot_") && name.ends_with(".png")));
    assert_eq!(std::fs::read(&path).expect("saved screenshot"), TEST_PNG);
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, TEST_PNG);
    assert!(!raw.contains(&encoded));
    let seen = stub.seen.lock().expect("stub lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(std::path::Path::new(&seen[0].0), path);
    assert!(seen[0].1.contains("Is there a login form?"));
    assert_eq!(seen[0].2, TEST_PNG);
}

#[tokio::test]
async fn browser_vision_without_vision_model_returns_path_and_note() {
    let _scope = EnvScope::new();
    let home = tempfile::tempdir().expect("temp hermes home");
    std::env::set_var("HERMES_HOME", home.path());

    let (raw, value) = run_browser_vision(Err(
        "No API key configured for vision provider `openai`".to_string(),
    ))
    .await;

    assert_eq!(value["success"], false, "{raw}");
    assert!(value["error"]
        .as_str()
        .is_some_and(|error| error.contains("No API key configured for vision provider")));
    assert!(value["note"]
        .as_str()
        .is_some_and(|note| note.contains("MEDIA:<path>")));
    let path = std::path::PathBuf::from(value["screenshot_path"].as_str().expect("path"));
    assert_eq!(std::fs::read(&path).expect("saved screenshot"), TEST_PNG);
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, TEST_PNG);
    assert!(!raw.contains(&encoded));
    assert!(!raw.contains("requires LLM integration"));
}
