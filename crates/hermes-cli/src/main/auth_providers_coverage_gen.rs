use super::*;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn gw_with(platform: &str, pairs: &[(&str, &str)]) -> hermes_config::GatewayConfig {
    let mut gw = hermes_config::GatewayConfig::default();
    let entry = gw.platforms.entry(platform.to_string()).or_default();
    for (k, v) in pairs {
        entry.extra.insert((*k).to_string(), (*v).into());
    }
    gw
}

fn b64(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
}

fn pool_entry(id: &str, label: &str) -> AuthPoolEntry {
    AuthPoolEntry {
        id: id.to_string(),
        label: label.to_string(),
        auth_type: "api_key".to_string(),
        source: "manual".to_string(),
        access_token: format!("tok-{id}"), // nosemgrep: codacy.yaml.security.hard-coded-tokens
        last_status: None,
        last_status_at: None,
        last_error_code: None,
    }
}

fn config_msg(err: AgentError) -> String {
    match err {
        AgentError::Config(m) => m,
        other => panic!("expected Config error, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn default_auth_provider_uses_explicit_pool_environment_then_config() {
    if !crate::isolated_test_profile::enter(
        "coverage_gen_main_auth_providers::default_auth_provider_uses_explicit_pool_environment_then_config",
    ) {
        return;
    }
    let home = PathBuf::from(std::env::var_os("HERMES_HOME").expect("isolated HERMES_HOME"));
    let config = home.join("config.yaml");
    std::fs::write(&config, "model: bare-model\n").unwrap();
    assert_eq!(resolve_auth_provider(None), "nous");

    std::fs::write(&config, "model: openrouter:fixture-model\n").unwrap();
    assert_eq!(resolve_auth_provider(None), "openrouter");

    std::env::set_var("HERMES_AUTH_DEFAULT_PROVIDER", " DEEPSEEK ");
    assert_eq!(resolve_auth_provider(None), "deepseek");

    std::env::set_var("HERMES_AUTH_PROVIDER_POOL", " , Kimi-Coding, openrouter ");
    assert_eq!(resolve_auth_provider(None), "kimi-coding");
    assert_eq!(resolve_auth_provider(Some("  ".into())), "kimi-coding");
    assert_eq!(resolve_auth_provider(Some(" GROQ ".into())), "groq");

    std::env::set_var("HERMES_AUTH_PROVIDER_POOL", " , ");
    assert_eq!(resolve_auth_provider(None), "deepseek");
    std::env::set_var("HERMES_AUTH_DEFAULT_PROVIDER", " ");
    assert_eq!(resolve_auth_provider(None), "openrouter");
}

#[test]
fn normalize_rare_aliases() {
    assert_eq!(
        normalize_auth_provider("alibaba_coding"),
        "alibaba-coding-plan"
    );
    assert_eq!(
        normalize_auth_provider("Oobabooga"),
        "text-generation-webui"
    );
    assert_eq!(
        normalize_secret_provider("alibaba-coding"),
        "alibaba-coding-plan"
    );
    assert_eq!(
        normalize_secret_provider("alibaba_coding_plan"),
        "alibaba-coding-plan"
    );
}

#[test]
fn secret_aliases_nous_api_and_vertex() {
    assert_eq!(
        secret_provider_aliases("nous_api"),
        vec!["nous-api", "nous_api", "nousapi", "nous-portal-api"]
    );
    assert_eq!(
        secret_provider_aliases("vertex-ai"),
        vec![
            "vertex",
            "google-vertex",
            "vertex-ai",
            "gcp-vertex",
            "vertexai"
        ]
    );
}

#[test]
fn auth_type_defaults_to_api_key_for_non_oauth() {
    assert_eq!(resolve_auth_type_for_provider("deepseek", None), "api_key");
    assert_eq!(
        resolve_auth_type_for_provider("deepseek", Some(" API-Key ")),
        "api_key"
    );
}

#[test]
fn unix_millis_parsing() {
    let ts = parse_unix_millis_utc(Some(1_700_000_000_000)).expect("valid ts");
    assert_eq!(ts.as_millisecond(), 1_700_000_000_000);
    assert!(parse_unix_millis_utc(None).is_none());
    assert!(parse_unix_millis_utc(Some(i64::MAX)).is_none());
}

#[test]
fn auth_pool_store_roundtrip_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("auth").join("pool.json");
    assert!(load_auth_pool_store(&path).unwrap().providers.is_empty());

    let mut store = AuthPoolStore::default();
    let mut entry = pool_entry("id-a", "first");
    entry.last_status = Some("ok".to_string());
    entry.last_error_code = Some(429);
    store.providers.insert(
        "openai".to_string(),
        vec![entry, pool_entry("id-b", "second")],
    );
    save_auth_pool_store(&path, &store).unwrap();
    assert!(path.exists());

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"last_status\""));
    assert!(!raw.contains("last_status_at"));

    let loaded = load_auth_pool_store(&path).unwrap();
    let entries = loaded.providers.get("openai").unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].id, "id-a");
    assert_eq!(entries[0].last_error_code, Some(429));
    assert_eq!(entries[1].access_token, "tok-id-b");

    std::fs::write(&path, "not json").unwrap();
    let err = load_auth_pool_store(&path).unwrap_err();
    assert!(config_msg(err).contains("parse pool"));
}

#[test]
fn resolve_pool_target_variants() {
    let entries = vec![pool_entry("id-a", "first"), pool_entry("id-b", "second")];
    assert_eq!(resolve_pool_target(&entries, "1"), Some(0));
    assert_eq!(resolve_pool_target(&entries, "2"), Some(1));
    assert_eq!(resolve_pool_target(&entries, "0"), None);
    assert_eq!(resolve_pool_target(&entries, "3"), None);
    assert_eq!(resolve_pool_target(&entries, "id-b"), Some(1));
    assert_eq!(resolve_pool_target(&entries, "second"), Some(1));
    assert_eq!(resolve_pool_target(&entries, "missing"), None);
}

#[test]
fn mask_secret_short_values() {
    assert_eq!(mask_secret(""), "(empty)");
    assert_eq!(mask_secret("abc"), "********");
    assert_eq!(mask_secret("abcdefgh"), "********");
    assert_eq!(mask_secret("abcdefghijkl"), "********");
}

#[test]
fn truthy_values() {
    assert!(is_truthy("1"));
    assert!(is_truthy(" TRUE "));
    assert!(is_truthy("yes"));
    assert!(is_truthy("On"));
    assert!(!is_truthy("no"));
    assert!(!is_truthy(""));
    let expected = std::env::var("HERMES_ALLOW_SECRET_STDOUT")
        .ok()
        .is_some_and(|v| is_truthy(&v));
    assert_eq!(secret_stdout_allowed(), expected);
}

#[test]
fn weixin_account_path_shape() {
    let p = weixin_account_file_path("acct-1");
    assert!(p.ends_with(
        std::path::Path::new("weixin")
            .join("accounts")
            .join("acct-1.json")
    ));
}

#[test]
fn qqbot_portal_host_resolution() {
    let gw = gw_with(
        "qqbot",
        &[("portal_host", "  "), ("qq_portal_host", " alt.host ")],
    );
    assert_eq!(qqbot_portal_host_from_disk(&gw), "alt.host");
    let gw = gw_with("qqbot", &[("portal_host", "main.host")]);
    assert_eq!(qqbot_portal_host_from_disk(&gw), "main.host");
    let empty = hermes_config::GatewayConfig::default();
    assert_eq!(
        qqbot_portal_host_from_disk(&empty),
        env_or("QQ_PORTAL_HOST", "q.qq.com")
    );
}

#[test]
fn qqbot_onboard_endpoint_resolution() {
    let gw = gw_with(
        "qqbot",
        &[
            ("onboard_create_path", " "),
            ("qr_create_path", "/custom/create"),
            ("onboard_poll_path", "/custom/poll"),
        ],
    );
    let (create, poll) = qqbot_onboard_endpoints_from_disk(&gw);
    assert_eq!(create, env_or("QQ_ONBOARD_CREATE_PATH", "/custom/create"));
    assert_eq!(poll, env_or("QQ_ONBOARD_POLL_PATH", "/custom/poll"));

    let empty = hermes_config::GatewayConfig::default();
    let (create, poll) = qqbot_onboard_endpoints_from_disk(&empty);
    assert_eq!(
        create,
        env_or("QQ_ONBOARD_CREATE_PATH", "/lite/create_bind_task")
    );
    assert_eq!(
        poll,
        env_or("QQ_ONBOARD_POLL_PATH", "/lite/poll_bind_result")
    );
}

#[test]
fn qqbot_extractors() {
    let v = serde_json::json!({"a": " ", "b": " val ", "n": "  42 ", "m": 7, "x": "nope"});
    assert_eq!(
        qqbot_extract_string(&v, &["a", "b"]),
        Some("val".to_string())
    );
    assert_eq!(qqbot_extract_string(&v, &["a", "zz"]), None);
    assert_eq!(qqbot_extract_i64(&v, &["missing", "n"]), Some(42));
    assert_eq!(qqbot_extract_i64(&v, &["m"]), Some(7));
    assert_eq!(qqbot_extract_i64(&v, &["x", "missing"]), None);
}

#[test]
fn qqbot_decrypt_error_paths() {
    let msg = config_msg(qqbot_decrypt_secret("AAAA", "not base64!!").unwrap_err());
    assert!(msg.contains("invalid bind key base64"));

    let msg = config_msg(qqbot_decrypt_secret("AAAA", &b64(&[1u8; 16])).unwrap_err());
    assert!(msg.contains("expected 32-byte key, got 16"));

    let key = b64(&[7u8; 32]);
    let msg = config_msg(qqbot_decrypt_secret("%%%", &key).unwrap_err());
    assert!(msg.contains("invalid encrypted secret"));

    let msg = config_msg(qqbot_decrypt_secret(&b64(&[0u8; 10]), &key).unwrap_err());
    assert!(msg.contains("too short"));

    let msg = config_msg(qqbot_decrypt_secret(&b64(&[0u8; 40]), &key).unwrap_err());
    assert!(msg.contains("decrypt failed"));
}

#[test]
fn qqbot_decrypt_roundtrip_with_generated_key() {
    let key_b64 = qqbot_generate_bind_key_base64();
    let key_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &key_b64).unwrap();
    assert_eq!(key_bytes.len(), 32);

    let cipher =
        <aes_gcm::Aes256Gcm as aes_gcm::aead::KeyInit>::new_from_slice(&key_bytes).unwrap();
    let nonce_bytes = [3u8; 12];
    let nonce = aes_gcm::Nonce::from_slice(&nonce_bytes);
    let ct = aes_gcm::aead::Aead::encrypt(&cipher, nonce, &b"qq-client-secret"[..]).unwrap();
    let mut payload = nonce_bytes.to_vec();
    payload.extend_from_slice(&ct);

    let plain = qqbot_decrypt_secret(&format!(" {} ", b64(&payload)), &key_b64).unwrap();
    assert_eq!(plain, "qq-client-secret");
}

#[test]
fn qqbot_url_helpers() {
    let headers = qqbot_api_headers();
    assert_eq!(
        headers
            .get(reqwest::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        headers
            .get(reqwest::header::ACCEPT)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        headers
            .get(reqwest::header::USER_AGENT)
            .unwrap()
            .to_str()
            .unwrap(),
        "HermesAgentUltra/qqbot-onboard"
    );
    assert_eq!(
        qqbot_join_https_url(" host.com/ ", "/a/b"),
        "https://host.com/a/b"
    );
    assert_eq!(qqbot_join_https_url("host.com", "c"), "https://host.com/c");
    assert!(qqbot_connect_url("t 1").contains("task_id=t%201"));
}

#[tokio::test]
async fn qqbot_binding_requests_parse_created_and_confirmed_tasks() {
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    Mock::given(method("POST"))
        .and(path("/create"))
        .and(body_json(serde_json::json!({"key": "fixture-bind-value"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0, "data": {"task_id": "fixture-task"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/poll"))
        .and(body_json(serde_json::json!({"task_id": "fixture-task"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": "0",
            "data": {
                "status": "2",
                "app_id": "fixture-app",
                "encrypt_secret": "opaque-fixture-value",
                "user_openid": "fixture-user"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let origin = server.uri();
    let task = qqbot_create_bind_task(&client, &format!("{origin}/create"), "fixture-bind-value")
        .await
        .unwrap();
    assert_eq!(task, "fixture-task");
    let (status, app_id, sealed_value, user) =
        qqbot_poll_bind_result(&client, &format!("{origin}/poll"), &task)
            .await
            .unwrap();
    assert_eq!(status, 2);
    assert_eq!(app_id, "fixture-app");
    assert_eq!(sealed_value, "opaque-fixture-value");
    assert_eq!(user, "fixture-user");
    server.verify().await;
}

#[tokio::test]
async fn qqbot_binding_requests_report_service_and_response_errors() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for (route, response, detail) in [
        (
            "/unavailable",
            ResponseTemplate::new(503).set_body_json(serde_json::json!({"msg": "try later"})),
            "failed (503 Service Unavailable)",
        ),
        (
            "/incomplete",
            ResponseTemplate::new(200).set_body_string("{"),
            "parse failed",
        ),
        (
            "/busy",
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"retcode": 7, "message": "try later"})),
            "retcode=7: try later",
        ),
    ] {
        Mock::given(method("POST"))
            .and(path(route))
            .respond_with(response)
            .expect(2)
            .mount(&server)
            .await;
        let url = format!("{}{route}", server.uri());
        let err = qqbot_create_bind_task(&client, &url, "fixture-bind-value")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("create_bind_task {detail}")),
            "{err}"
        );
        let err = qqbot_poll_bind_result(&client, &url, "fixture-task")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("poll_bind_result {detail}")),
            "{err}"
        );
    }
    server.verify().await;
}

#[tokio::test]
async fn qqbot_app_id_and_secret_from_existing() {
    let app = qqbot_app_id_from_env_or_prompt(Some("  app-123  "))
        .await
        .unwrap();
    assert_eq!(app, env_or("QQ_APP_ID", "app-123"));
    let secret = qqbot_client_secret_from_env_or_prompt(Some(" sec-456 "))
        .await
        .unwrap();
    assert_eq!(secret, env_or("QQ_CLIENT_SECRET", "sec-456"));
}

#[test]
fn weixin_base_url_resolution() {
    let gw = gw_with("weixin", &[("base_url", " https://wx.example ")]);
    assert_eq!(weixin_login_base_url_from_disk(&gw), "https://wx.example");
    let blank = gw_with("weixin", &[("base_url", "  ")]);
    let expected = env_or("WEIXIN_BASE_URL", "https://ilinkai.weixin.qq.com");
    assert_eq!(weixin_login_base_url_from_disk(&blank), expected);
    let empty = hermes_config::GatewayConfig::default();
    assert_eq!(weixin_login_base_url_from_disk(&empty), expected);
}

#[test]
fn weixin_endpoint_resolution() {
    let gw = gw_with(
        "weixin",
        &[
            ("qr_start_endpoint", " start/x "),
            ("qr_get_qrcode_status_endpoint", "poll/y"),
        ],
    );
    assert_eq!(
        weixin_login_endpoints_from_disk(&gw),
        ("start/x".to_string(), "poll/y".to_string())
    );
    let gw = gw_with(
        "weixin",
        &[
            ("qr_get_bot_qrcode_endpoint", " "),
            ("qr_poll_endpoint", "p2"),
        ],
    );
    assert_eq!(
        weixin_login_endpoints_from_disk(&gw),
        ("ilink/bot/get_bot_qrcode".to_string(), "p2".to_string())
    );
    let empty = hermes_config::GatewayConfig::default();
    assert_eq!(
        weixin_login_endpoints_from_disk(&empty),
        (
            "ilink/bot/get_bot_qrcode".to_string(),
            "ilink/bot/get_qrcode_status".to_string()
        )
    );
}

#[test]
fn weixin_extract_string_picks_first_nonempty() {
    let v = serde_json::json!({"a": "  ", "b": " x ", "c": 3});
    assert_eq!(
        weixin_extract_string(&v, &["a", "c", "b"]),
        Some("x".to_string())
    );
    assert_eq!(weixin_extract_string(&v, &["a", "c"]), None);
}

#[test]
fn render_qr_does_not_panic() {
    let r = std::panic::catch_unwind(|| {
        render_qr_to_terminal("");
        render_qr_to_terminal("abcde");
        render_qr_to_terminal("abcdefghi");
        render_qr_to_terminal("https://example.com/qr?x=1");
    });
    assert!(r.is_ok());
}

#[tokio::test]
async fn weixin_qr_fetch_http_error() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ilink/bot/get_bot_qrcode"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let base = format!("{}/", server.uri());
    let err = weixin_qr_login_flow(&base, "/ilink/bot/get_bot_qrcode", "poll", None)
        .await
        .unwrap_err();
    let msg = config_msg(err);
    assert!(msg.contains("get_bot_qrcode failed"));
    assert!(msg.contains("500"));
    assert!(msg.contains("boom"));
}

#[tokio::test]
async fn weixin_qr_fetch_missing_qrcode() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"qrcode": "  "})))
        .mount(&server)
        .await;
    let err = weixin_qr_login_flow(&server.uri(), "start", "poll", Some("hint"))
        .await
        .unwrap_err();
    assert!(config_msg(err).contains("missing qrcode"));
}

#[tokio::test]
async fn weixin_qr_fetch_invalid_json() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let err = weixin_qr_login_flow(&server.uri(), "start", "poll", None)
        .await
        .unwrap_err();
    match err {
        AgentError::Io(m) => assert!(m.contains("get_bot_qrcode parse")),
        other => panic!("expected Io error, got {other:?}"),
    }
}
