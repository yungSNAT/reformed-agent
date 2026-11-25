use super::*;
use crate::test_env_lock;

fn nous_test_jwt(seconds: i64, scope: Value) -> String {
    let header = serde_json::json!({ "alg": "none", "typ": "JWT" });
    let claims = serde_json::json!({
        "exp": jiff::Timestamp::now().as_second() + seconds,
        "scope": scope,
    });
    format!(
        "{}.{}.sig",
        BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("header json")),
        BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("claims json"))
    )
}

fn nous_test_state(access_token: String) -> NousAuthState {
    NousAuthState {
        portal_base_url: DEFAULT_NOUS_PORTAL_URL.to_string(),
        inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
        client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
        scope: Some(DEFAULT_NOUS_SCOPE.to_string()),
        token_type: "Bearer".to_string(),
        access_token,
        refresh_token: Some("refresh".to_string()),
        obtained_at: hermes_core::time::now_rfc3339(),
        expires_at: None,
        expires_in: None,
        agent_key: None,
        agent_key_id: None,
        agent_key_expires_at: None,
        agent_key_expires_in: None,
        agent_key_reused: None,
        agent_key_obtained_at: None,
    }
}

#[test]
fn nous_runtime_api_key_prefers_agent_key() {
    let state = NousAuthState {
        portal_base_url: DEFAULT_NOUS_PORTAL_URL.to_string(),
        inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
        client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
        scope: Some(DEFAULT_NOUS_SCOPE.to_string()),
        token_type: "Bearer".to_string(),
        access_token: "portal-access".to_string(),
        refresh_token: Some("refresh".to_string()),
        obtained_at: hermes_core::time::now_rfc3339(),
        expires_at: None,
        expires_in: None,
        agent_key: Some("agent-key".to_string()),
        agent_key_id: None,
        agent_key_expires_at: None,
        agent_key_expires_in: None,
        agent_key_reused: None,
        agent_key_obtained_at: None,
    };
    assert_eq!(state.runtime_api_key().as_deref(), Some("agent-key"));
}

#[test]
fn nous_timestamp_is_expiring_treats_missing_as_expiring() {
    assert!(timestamp_is_expiring(None, 120));
    assert!(timestamp_is_expiring(Some(""), 120));
    assert!(timestamp_is_expiring(Some("not-a-date"), 120));
}

#[test]
fn nous_access_expires_at_refuses_an_expiry_past_year_9999() {
    let now = jiff::Timestamp::from_second(1_772_962_215).unwrap();
    assert_eq!(
        nous_access_expires_at(now, 3600).unwrap(),
        "2026-03-08T10:30:15Z"
    );
    // Kept, 9999-12-30T22:00:00Z would read back as a token still valid.
    assert!(nous_access_expires_at(now, 300_000_000_000).is_err());
    assert!(nous_access_expires_at(now, i64::MAX).is_err());
}

#[test]
fn nous_invoke_jwt_usable_uses_invoke_scope_and_jwt_expiry() {
    let token = nous_test_jwt(900, Value::String(NOUS_INFERENCE_INVOKE_SCOPE.to_string()));
    assert!(nous_invoke_jwt_is_usable(
        &token,
        Some(DEFAULT_NOUS_SCOPE),
        None,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
    ));
    assert_eq!(
        nous_invoke_jwt_status(
            &nous_test_jwt(900, Value::String("profile".to_string())),
            None,
            None,
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        ),
        Some("missing_inference_invoke_scope")
    );
    assert_eq!(
        nous_invoke_jwt_status(
            &nous_test_jwt(30, Value::String(NOUS_INFERENCE_INVOKE_SCOPE.to_string())),
            None,
            None,
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        ),
        Some("invoke_jwt_expiring")
    );
}

#[test]
fn nous_invoke_jwt_selection_mirrors_access_token_to_agent_key_fields() {
    let token = nous_test_jwt(900, serde_json::json!([NOUS_INFERENCE_INVOKE_SCOPE]));
    let mut state = nous_test_state(token.clone());
    state.expires_at = Some("2000-01-01T00:00:00Z".to_string());
    state.agent_key = Some("legacy-opaque-agent-key".to_string());
    state.agent_key_expires_at = Some("2099-01-01T00:00:00Z".to_string());

    assert_nous_invoke_jwt_usable(&state, None, NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS)
        .expect("invoke jwt usable");
    set_nous_agent_key_from_invoke_jwt(&mut state);

    assert_eq!(state.agent_key.as_deref(), Some(token.as_str()));
    assert_eq!(state.agent_key_id, None);
    assert_eq!(state.agent_key_reused, Some(false));
    let mirrored_expiry = state
        .agent_key_expires_at
        .as_deref()
        .and_then(parse_iso_timestamp_utc)
        .expect("jwt expiry mirrored");
    assert!(mirrored_expiry > Timestamp::now() + jiff::SignedDuration::from_secs(600));
    assert_eq!(state.expires_at, state.agent_key_expires_at);
}

#[cfg(unix)]
#[tokio::test]
async fn nous_device_login_polls_until_approved_and_keeps_the_requested_inference_url() {
    if !crate::isolated_test_profile::enter(
        "auth::tests::nous_device_login_polls_until_approved_and_keeps_the_requested_inference_url",
    ) {
        return;
    }
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let portal = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/device/code"))
        .and(body_string_contains("client_id=fixture-client"))
        .and(body_string_contains("scope=fixture-scope"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "fixture-device",
            "user_code": "FIXTURE-1",
            "verification_uri": format!("{}/verify", portal.uri()),
            "interval": 1
        })))
        .expect(1)
        .mount(&portal)
        .await;

    let jwt = nous_test_jwt(900, serde_json::json!([NOUS_INFERENCE_INVOKE_SCOPE]));
    for (priority, status, response) in [
        (
            1,
            400,
            serde_json::json!({"error": "authorization_pending"}),
        ),
        (2, 400, serde_json::json!({"error": "slow_down"})),
        (
            3,
            200,
            serde_json::json!({
                "access_token": jwt,
                "refresh_token": "fixture-refresh",
                "expires_in": 900,
                "inference_base_url": "https://untrusted.example.invalid/v1",
            }),
        ),
    ] {
        Mock::given(method("POST"))
            .and(path("/api/oauth/token"))
            .and(body_string_contains("client_id=fixture-client"))
            .and(body_string_contains("device_code=fixture-device"))
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
            ))
            .respond_with(ResponseTemplate::new(status).set_body_json(response))
            .with_priority(priority)
            .up_to_n_times(1)
            .expect(1)
            .mount(&portal)
            .await;
    }
    let local_inference = format!("{}/inference/v1", portal.uri());
    let state = tokio::time::timeout(
        Duration::from_secs(16),
        login_nous_device_code(NousDeviceCodeOptions {
            portal_base_url: Some(format!("{}/", portal.uri())),
            inference_base_url: Some(format!("{local_inference}/")),
            client_id: Some("fixture-client".into()),
            scope: Some("fixture-scope".into()),
            open_browser: false,
            ..Default::default()
        }),
    )
    .await
    .expect("fixture device login timed out")
    .expect("device approval produces usable state");
    assert_eq!(state.portal_base_url, portal.uri());
    assert_eq!(state.inference_base_url, local_inference);
    assert_eq!(state.scope.as_deref(), Some("fixture-scope"));
    assert_eq!(state.token_type, "Bearer");
    assert_eq!(state.refresh_token.as_deref(), Some("fixture-refresh"));
    assert_eq!(state.runtime_api_key().as_deref(), Some(jwt.as_str()));
    assert_eq!(state.expires_at, state.agent_key_expires_at);
    assert!(state.expires_at.is_some());
    assert_eq!(portal.received_requests().await.unwrap().len(), 4);
}

#[cfg(unix)]
#[tokio::test]
async fn anthropic_oauth_exchanges_the_pasted_code_using_the_isolated_endpoint() {
    if !crate::isolated_test_profile::enter_with_input(
        "auth::tests::anthropic_oauth_exchanges_the_pasted_code_using_the_isolated_endpoint",
        b"fixture-code#fixture-state\n",
    ) {
        return;
    }
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let service = MockServer::start().await;
    std::env::set_var(
        "HERMES_ANTHROPIC_OAUTH_AUTHORIZE_URL",
        format!("{}/authorize", service.uri()),
    );
    std::env::set_var(
        "HERMES_ANTHROPIC_OAUTH_TOKEN_URL",
        format!("{}/token", service.uri()),
    );
    std::env::set_var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID", "fixture-client");
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": " fixture-anthropic-access ",
            "refresh_token": " fixture-anthropic-refresh ",
            "expires_in": 600
        })))
        .expect(1)
        .mount(&service)
        .await;

    let before = jiff::Timestamp::now().as_millisecond();
    let state = login_anthropic_oauth(AnthropicOAuthLoginOptions {
        open_browser: false,
        ..Default::default()
    })
    .await
    .expect("local OAuth exchange");
    assert_eq!(state.access_token, "fixture-anthropic-access");
    assert_eq!(
        state.refresh_token.as_deref(),
        Some("fixture-anthropic-refresh")
    );
    let after = jiff::Timestamp::now().as_millisecond();
    assert!((before + 600_000..=after + 600_000).contains(&state.expires_at_ms.unwrap()));

    let requests = service.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(request["grant_type"], "authorization_code");
    assert_eq!(request["client_id"], "fixture-client");
    assert_eq!(request["code"], "fixture-code");
    assert_eq!(request["state"], "fixture-state");
    assert_eq!(request["redirect_uri"], ANTHROPIC_OAUTH_REDIRECT_URI);
    assert!(request["code_verifier"]
        .as_str()
        .is_some_and(|value| value.len() >= 43));
}

#[tokio::test]
async fn resolve_nous_runtime_credentials_selects_invoke_jwt_without_mint() {
    let _guard = test_env_lock::lock_async().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("auth.json");
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
    std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let token = nous_test_jwt(900, Value::String(NOUS_INFERENCE_INVOKE_SCOPE.to_string()));
    let mut state = nous_test_state(token.clone());
    state.agent_key = Some("legacy-opaque-agent-key".to_string());
    state.agent_key_expires_at = Some("2099-01-01T00:00:00Z".to_string());
    save_nous_auth_state(&state).expect("save nous state");

    let resolved = resolve_nous_runtime_credentials(
        false,
        true,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await
    .expect("resolve nous runtime credentials");
    assert_eq!(resolved.api_key, token);
    assert_eq!(resolved.source, NOUS_AUTH_PATH_INVOKE_JWT);
    assert!(resolved.expires_in.unwrap_or_default() > 600);

    let saved = read_provider_auth_state("nous")
        .expect("read saved state")
        .expect("saved state");
    assert_eq!(
        saved.get("agent_key").and_then(Value::as_str),
        Some(resolved.api_key.as_str())
    );
    assert!(
        saved.get("agent_key_id").is_none(),
        "invoke JWT path should not preserve legacy agent key id"
    );

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
}

#[tokio::test]
async fn resolve_nous_runtime_credentials_rejects_missing_invoke_scope() {
    let _guard = test_env_lock::lock_async().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("auth.json");
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
    std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let token = nous_test_jwt(900, Value::String("profile".to_string()));
    let mut state = nous_test_state(token);
    state.scope = Some("profile".to_string());
    state.refresh_token = None;
    save_nous_auth_state(&state).expect("save nous state");

    let err = resolve_nous_runtime_credentials(
        false,
        true,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await
    .expect_err("missing invoke scope should fail");
    assert!(
        err.to_string().contains("missing_inference_invoke_scope"),
        "unexpected error: {err}"
    );

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
}

#[test]
fn clear_provider_auth_state_is_noop_when_missing() {
    let provider = format!("missing-{}", uuid::Uuid::new_v4().simple());
    let removed = clear_provider_auth_state(&provider).expect("clear");
    assert!(!removed);
}

#[test]
fn codex_oauth_numeric_fields_accept_number_or_string() {
    let device_from_number: CodexDeviceUserCodeResponse =
        serde_json::from_value(serde_json::json!({
            "user_code": "ABCD-EFGH",
            "device_auth_id": "device-auth-id",
            "interval": 5
        }))
        .expect("numeric interval");
    assert_eq!(device_from_number.interval, Some(5));

    let device_from_string: CodexDeviceUserCodeResponse =
        serde_json::from_value(serde_json::json!({
            "user_code": "ABCD-EFGH",
            "device_auth_id": "device-auth-id",
            "interval": "5"
        }))
        .expect("string interval");
    assert_eq!(device_from_string.interval, Some(5));

    let token_from_string: CodexTokenResponse = serde_json::from_value(serde_json::json!({
        "access_token": "access",
        "refresh_token": "refresh",
        "expires_in": "3600"
    }))
    .expect("string expires_in");
    assert_eq!(token_from_string.expires_in, Some(3600));
}

#[test]
fn discover_existing_openai_oauth_reads_env_path() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("codex-auth.json");
    let exp = jiff::Timestamp::now().as_second() + 3600;
    let payload = serde_json::json!({ "exp": exp });
    let payload_b64 =
        BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("payload json"));
    let id_token = format!("header.{}.sig", payload_b64);
    let raw = serde_json::json!({
        "auth_mode": "chatgpt",
        "last_refresh": "2026-04-27T00:00:00Z",
        "tokens": {
            "access_token": "openai-access",
            "refresh_token": "openai-refresh",
            "id_token": id_token,
        }
    });
    std::fs::write(
        &auth_path,
        serde_json::to_string_pretty(&raw).expect("serialize auth fixture"),
    )
    .expect("write auth fixture");
    std::env::set_var(
        "HERMES_OPENAI_OAUTH_FILE",
        auth_path.to_string_lossy().to_string(),
    );

    let imported = discover_existing_openai_oauth()
        .expect("discover")
        .expect("imported");
    assert_eq!(imported.source_path, auth_path);
    assert_eq!(imported.state.tokens.access_token, "openai-access");
    assert_eq!(
        imported.state.tokens.refresh_token.as_deref(),
        Some("openai-refresh")
    );
    assert_eq!(imported.state.base_url, DEFAULT_CODEX_BASE_URL.to_string());
    assert_eq!(imported.state.auth_mode.as_deref(), Some("chatgpt"));
    assert!(
        imported.state.tokens.expires_in.unwrap_or_default() > 100,
        "expected imported token TTL from id_token exp"
    );
    std::env::remove_var("HERMES_OPENAI_OAUTH_FILE");
}

#[test]
fn discover_existing_openai_codex_oauth_reads_env_path() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("codex-auth.json");
    let exp = jiff::Timestamp::now().as_second() + 3600;
    let payload = serde_json::json!({ "exp": exp });
    let payload_b64 =
        BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("payload json"));
    let id_token = format!("header.{}.sig", payload_b64);
    let raw = serde_json::json!({
        "auth_mode": "chatgpt",
        "last_refresh": "2026-04-27T00:00:00Z",
        "tokens": {
            "access_token": "codex-access",
            "refresh_token": "codex-refresh",
            "id_token": id_token,
        }
    });
    std::fs::write(
        &auth_path,
        serde_json::to_string_pretty(&raw).expect("serialize auth fixture"),
    )
    .expect("write auth fixture");
    std::env::set_var(
        "HERMES_OPENAI_CODEX_OAUTH_FILE",
        auth_path.to_string_lossy().to_string(),
    );

    let imported = discover_existing_openai_codex_oauth()
        .expect("discover")
        .expect("imported");
    assert_eq!(imported.source_path, auth_path);
    assert_eq!(imported.state.tokens.access_token, "codex-access");
    assert_eq!(
        imported.state.tokens.refresh_token.as_deref(),
        Some("codex-refresh")
    );
    assert_eq!(imported.state.base_url, DEFAULT_CODEX_BASE_URL.to_string());
    assert_eq!(imported.state.auth_mode.as_deref(), Some("chatgpt"));
    std::env::remove_var("HERMES_OPENAI_CODEX_OAUTH_FILE");
}

#[test]
fn discover_existing_anthropic_oauth_reads_claude_credentials_file() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let cred_path = tmp.path().join(".credentials.json");
    let expires_at = jiff::Timestamp::now().as_millisecond() + 3_600_000;
    let raw = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": "ant-access",
            "refreshToken": "ant-refresh",
            "expiresAt": expires_at,
        }
    });
    std::fs::write(
        &cred_path,
        serde_json::to_string_pretty(&raw).expect("serialize credentials"),
    )
    .expect("write credentials");
    std::env::set_var(
        "CLAUDE_CODE_CREDENTIALS_FILE",
        cred_path.to_string_lossy().to_string(),
    );

    let imported = discover_existing_anthropic_oauth_with_keychain(None)
        .expect("discover")
        .expect("imported");
    assert_eq!(imported.source_path, cred_path);
    assert_eq!(imported.source, "claude_code_credentials_file");
    assert_eq!(imported.state.access_token, "ant-access");
    assert_eq!(imported.state.refresh_token.as_deref(), Some("ant-refresh"));
    assert_eq!(imported.state.expires_at_ms, Some(expires_at));
    std::env::remove_var("CLAUDE_CODE_CREDENTIALS_FILE");
}

#[test]
fn anthropic_oauth_keychain_payload_parses_valid_claude_code_entry() {
    let raw = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": "keychain-access",
            "refreshToken": "keychain-refresh",
            "expiresAt": 9_999_999_999_999i64,
        }
    });
    let imported = load_anthropic_oauth_import_from_keychain_payload(
        &serde_json::to_string(&raw).expect("serialize keychain payload"),
    )
    .expect("imported");

    assert_eq!(imported.source_path, anthropic_keychain_source_path());
    assert_eq!(imported.source, "macos_keychain");
    assert_eq!(imported.state.access_token, "keychain-access");
    assert_eq!(
        imported.state.refresh_token.as_deref(),
        Some("keychain-refresh")
    );
    assert_eq!(imported.state.expires_at_ms, Some(9_999_999_999_999));
}

#[test]
fn anthropic_oauth_keychain_payload_rejects_invalid_entries() {
    assert!(load_anthropic_oauth_import_from_keychain_payload("").is_none());
    assert!(load_anthropic_oauth_import_from_keychain_payload("not json").is_none());
    assert!(load_anthropic_oauth_import_from_keychain_payload(
        r#"{"someOtherService":{"accessToken":"tok"}}"#
    )
    .is_none());
    assert!(load_anthropic_oauth_import_from_keychain_payload(
        r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"refresh"}}"#
    )
    .is_none());
}

#[test]
fn discover_existing_anthropic_oauth_prefers_keychain_over_json_file() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let cred_path = tmp.path().join(".credentials.json");
    let raw = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": "json-access",
            "refreshToken": "json-refresh",
            "expiresAt": 9_999_999_999_999i64,
        }
    });
    std::fs::write(
        &cred_path,
        serde_json::to_string_pretty(&raw).expect("serialize credentials"),
    )
    .expect("write credentials");
    std::env::set_var(
        "CLAUDE_CODE_CREDENTIALS_FILE",
        cred_path.to_string_lossy().to_string(),
    );

    let keychain_import = AnthropicOAuthImport {
        state: AnthropicOAuthState {
            access_token: "keychain-access".to_string(),
            refresh_token: Some("keychain-refresh".to_string()),
            expires_at_ms: Some(9_999_999_999_999),
        },
        source_path: anthropic_keychain_source_path(),
        source: "macos_keychain".to_string(),
    };
    let imported = discover_existing_anthropic_oauth_with_keychain(Some(keychain_import))
        .expect("discover")
        .expect("imported");

    assert_eq!(imported.state.access_token, "keychain-access");
    assert_eq!(imported.source, "macos_keychain");
    std::env::remove_var("CLAUDE_CODE_CREDENTIALS_FILE");
}

#[test]
fn discover_existing_nous_oauth_reads_auth_store_provider_state() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_store = tmp.path().join("auth.json");
    let raw = serde_json::json!({
        "version": 1,
        "providers": {
            "nous": {
                "portal_base_url": DEFAULT_NOUS_PORTAL_URL,
                "inference_base_url": DEFAULT_NOUS_INFERENCE_URL,
                "client_id": DEFAULT_NOUS_CLIENT_ID,
                "scope": DEFAULT_NOUS_SCOPE,
                "token_type": "Bearer",
                "access_token": "nous-access",
                "refresh_token": "nous-refresh",
                "obtained_at": "2026-04-27T00:00:00Z",
                "agent_key": "nous-agent-key"
            }
        }
    });
    std::fs::write(
        &auth_store,
        serde_json::to_string_pretty(&raw).expect("serialize auth store"),
    )
    .expect("write auth store");
    std::env::set_var("HERMES_AUTH_FILE", auth_store.to_string_lossy().to_string());
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let imported = discover_existing_nous_oauth()
        .expect("discover")
        .expect("imported");
    assert_eq!(imported.source_path, auth_store);
    assert_eq!(imported.state.access_token, "nous-access");
    assert_eq!(
        imported.state.refresh_token.as_deref(),
        Some("nous-refresh")
    );
    assert_eq!(
        imported.state.runtime_api_key().as_deref(),
        Some("nous-agent-key")
    );
    std::env::remove_var("HERMES_AUTH_FILE");
}

#[test]
fn read_provider_auth_state_falls_back_to_global_store_when_primary_missing_provider() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var("HOME").ok();
    let prev_hermes_home = std::env::var("HERMES_HOME").ok();
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();

    std::env::set_var("HOME", tmp.path());
    std::env::remove_var("HERMES_HOME");

    let primary_store = tmp.path().join("profile-auth.json");
    let primary_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "openai": { "access_token": "primary-openai" }
        }
    });
    std::fs::write(
        &primary_store,
        serde_json::to_string_pretty(&primary_raw).expect("serialize primary auth"),
    )
    .expect("write primary auth");
    std::env::set_var(
        "HERMES_AUTH_FILE",
        primary_store.to_string_lossy().to_string(),
    );

    let fallback_store = tmp.path().join(".hermes-agent-ultra").join("auth.json");
    std::fs::create_dir_all(
        fallback_store
            .parent()
            .expect("fallback store should have parent"),
    )
    .expect("mkdir fallback parent");
    let fallback_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "nous": {
                "access_token": "fallback-nous-access"
            }
        }
    });
    std::fs::write(
        &fallback_store,
        serde_json::to_string_pretty(&fallback_raw).expect("serialize fallback auth"),
    )
    .expect("write fallback auth");

    let found = read_provider_auth_state("nous")
        .expect("read provider auth state")
        .expect("fallback provider should resolve");
    assert_eq!(
        found
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "fallback-nous-access"
    );

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_hermes_home {
        Some(v) => std::env::set_var("HERMES_HOME", v),
        None => std::env::remove_var("HERMES_HOME"),
    }
    match prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn read_valid_nous_auth_state_ignores_malformed_fallback_store() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var("HOME").ok();
    let prev_hermes_home = std::env::var("HERMES_HOME").ok();
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();

    std::env::set_var("HOME", tmp.path());
    std::env::remove_var("HERMES_HOME");

    let primary_store = tmp.path().join("profile-auth.json");
    let primary_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "openai": { "access_token": "primary-openai" }
        }
    });
    std::fs::write(
        &primary_store,
        serde_json::to_string_pretty(&primary_raw).expect("serialize primary auth"),
    )
    .expect("write primary auth");
    std::env::set_var(
        "HERMES_AUTH_FILE",
        primary_store.to_string_lossy().to_string(),
    );
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let fallback_store = tmp.path().join(".hermes").join("auth.json");
    std::fs::create_dir_all(
        fallback_store
            .parent()
            .expect("fallback store should have parent"),
    )
    .expect("mkdir fallback parent");
    let fallback_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "nous": {
                "client_id": DEFAULT_NOUS_CLIENT_ID,
                "inference_base_url": DEFAULT_NOUS_INFERENCE_URL,
                "last_auth_error": "unauthorized",
                "portal_base_url": DEFAULT_NOUS_PORTAL_URL,
                "scope": DEFAULT_NOUS_SCOPE,
                "token_type": "Bearer"
            }
        }
    });
    std::fs::write(
        &fallback_store,
        serde_json::to_string_pretty(&fallback_raw).expect("serialize fallback auth"),
    )
    .expect("write fallback auth");

    assert!(
        read_provider_auth_state("nous")
            .expect("read raw provider auth state")
            .is_some(),
        "raw fallback store should still be discoverable"
    );
    assert!(
        read_valid_nous_auth_state()
            .expect("read valid nous auth state")
            .is_none(),
        "malformed fallback state must not count as a valid Nous login"
    );

    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_hermes_home {
        Some(v) => std::env::set_var("HERMES_HOME", v),
        None => std::env::remove_var("HERMES_HOME"),
    }
    match prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn read_valid_nous_auth_state_rejects_profile_only_access_state_without_refresh() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("auth.json");
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
    std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let mut state = nous_test_state("profile-access-token".to_string());
    state.scope = Some("profile".to_string());
    state.refresh_token = None;
    save_nous_auth_state(&state).expect("save nous state");

    assert!(
        read_valid_nous_auth_state()
            .expect("read valid nous auth state")
            .is_none(),
        "profile-only access token without refresh must not count as runtime-valid"
    );

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
}

#[test]
fn read_valid_nous_auth_state_accepts_refreshable_access_state() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("auth.json");
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
    std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let mut state = nous_test_state("profile-access-token".to_string());
    state.scope = Some("profile".to_string());
    state.refresh_token = Some("refresh".to_string());
    save_nous_auth_state(&state).expect("save nous state");

    let found = read_valid_nous_auth_state()
        .expect("read valid nous auth state")
        .expect("refreshable state should count as valid");
    assert_eq!(found.refresh_token.as_deref(), Some("refresh"));

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
}

#[tokio::test]
async fn resolve_nous_runtime_credentials_ignores_malformed_fallback_store() {
    let _guard = test_env_lock::lock_async().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var("HOME").ok();
    let prev_hermes_home = std::env::var("HERMES_HOME").ok();
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();

    std::env::set_var("HOME", tmp.path());
    std::env::remove_var("HERMES_HOME");

    let primary_store = tmp.path().join("profile-auth.json");
    let primary_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "openai": { "access_token": "primary-openai" }
        }
    });
    std::fs::write(
        &primary_store,
        serde_json::to_string_pretty(&primary_raw).expect("serialize primary auth"),
    )
    .expect("write primary auth");
    std::env::set_var(
        "HERMES_AUTH_FILE",
        primary_store.to_string_lossy().to_string(),
    );
    std::env::remove_var("HERMES_NOUS_OAUTH_FILE");

    let fallback_store = tmp.path().join(".hermes").join("auth.json");
    std::fs::create_dir_all(
        fallback_store
            .parent()
            .expect("fallback store should have parent"),
    )
    .expect("mkdir fallback parent");
    let fallback_raw = serde_json::json!({
        "version": 1,
        "providers": {
            "nous": {
                "client_id": DEFAULT_NOUS_CLIENT_ID,
                "inference_base_url": DEFAULT_NOUS_INFERENCE_URL,
                "last_auth_error": "unauthorized",
                "portal_base_url": DEFAULT_NOUS_PORTAL_URL,
                "scope": DEFAULT_NOUS_SCOPE,
                "token_type": "Bearer"
            }
        }
    });
    std::fs::write(
        &fallback_store,
        serde_json::to_string_pretty(&fallback_raw).expect("serialize fallback auth"),
    )
    .expect("write fallback auth");

    let err = resolve_nous_runtime_credentials(
        false,
        true,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await
    .expect_err("malformed fallback store should not resolve runtime credentials");
    assert!(
        err.to_string()
            .contains("Hermes is not logged into Nous Portal"),
        "unexpected error: {err}"
    );

    match prev_nous_file {
        Some(v) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", v),
        None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
    }
    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_hermes_home {
        Some(v) => std::env::set_var("HERMES_HOME", v),
        None => std::env::remove_var("HERMES_HOME"),
    }
    match prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn auth_store_write_respects_auth_file_override() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let override_path = tmp.path().join("override").join("auth.json");
    let prev_hermes_home = std::env::var("HERMES_HOME").ok();
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    std::env::set_var("HERMES_HOME", &home);
    std::env::set_var("HERMES_AUTH_FILE", &override_path);

    let path = save_provider_auth_state(
        "nous",
        serde_json::json!({
            "access_token": "override-access-token",
            "agent_key": "override-agent-key"
        }),
    )
    .expect("save auth state");

    assert_eq!(path, override_path);
    assert!(
        override_path.exists(),
        "override auth file should be written"
    );
    assert!(
        !home.join("auth.json").exists(),
        "HERMES_AUTH_FILE writes must not touch HERMES_HOME/auth.json"
    );
    let saved = read_provider_auth_state("nous")
        .expect("read auth state")
        .expect("provider state");
    assert_eq!(
        saved.get("agent_key").and_then(Value::as_str),
        Some("override-agent-key")
    );

    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
    match prev_hermes_home {
        Some(v) => std::env::set_var("HERMES_HOME", v),
        None => std::env::remove_var("HERMES_HOME"),
    }
}

#[test]
fn auth_store_write_is_atomic_owner_only_and_cleans_tmp() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_hermes_home = std::env::var("HERMES_HOME").ok();
    let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
    std::env::set_var("HERMES_HOME", tmp.path());
    std::env::remove_var("HERMES_AUTH_FILE");

    let path = save_provider_auth_state(
        "openai",
        serde_json::json!({
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token"
        }),
    )
    .expect("save auth state");

    let raw = std::fs::read_to_string(&path).expect("read auth store");
    assert!(raw.contains("test-access-token"));
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
        .expect("read temp home")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temporary auth files should be cleaned up: {leftovers:?}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("auth metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "auth store should be owner-only");
    }

    match prev_hermes_home {
        Some(v) => std::env::set_var("HERMES_HOME", v),
        None => std::env::remove_var("HERMES_HOME"),
    }
    match prev_auth_file {
        Some(v) => std::env::set_var("HERMES_AUTH_FILE", v),
        None => std::env::remove_var("HERMES_AUTH_FILE"),
    }
}

#[test]
fn qwen_access_token_is_expiring_honors_skew() {
    let now_ms = jiff::Timestamp::now().as_millisecond();
    assert!(qwen_access_token_is_expiring(None, 120));
    assert!(qwen_access_token_is_expiring(Some(now_ms + 30_000), 120));
    assert!(!qwen_access_token_is_expiring(Some(now_ms + 300_000), 120));
}

#[test]
fn gemini_packed_refresh_roundtrip() {
    let packed =
        pack_gemini_refresh(Some("r1"), Some("proj"), Some("managed")).expect("packed refresh");
    assert_eq!(packed, "r1|proj|managed");
    let parsed = parse_packed_gemini_refresh(Some(&packed));
    assert_eq!(parsed.0.as_deref(), Some("r1"));
    assert_eq!(parsed.1.as_deref(), Some("proj"));
    assert_eq!(parsed.2.as_deref(), Some("managed"));
}

#[test]
fn gemini_access_token_is_expiring_honors_skew() {
    let now_ms = jiff::Timestamp::now().as_millisecond();
    assert!(gemini_access_token_is_expiring(None, 60));
    assert!(gemini_access_token_is_expiring(Some(now_ms + 1_000), 60));
    assert!(!gemini_access_token_is_expiring(Some(now_ms + 120_000), 60));
}

#[test]
fn gemini_state_read_write_roundtrip() {
    let _guard = test_env_lock::lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("google_oauth.json");
    std::env::set_var(
        "HERMES_GEMINI_OAUTH_FILE",
        auth_path.to_string_lossy().to_string(),
    );
    let state = GeminiOAuthFileState {
        refresh: Some("refresh-token|proj-1|managed-1".to_string()),
        access: Some("access-token".to_string()),
        expires: Some(jiff::Timestamp::now().as_millisecond() + 5 * 60 * 1000),
        email: Some("dev@example.com".to_string()),
        project_id: Some("proj-1".to_string()),
        managed_project_id: Some("managed-1".to_string()),
    };
    save_gemini_cli_state(&state).expect("save");
    let loaded = read_gemini_cli_state().expect("read");
    assert_eq!(loaded.access.as_deref(), Some("access-token"));
    assert_eq!(
        parse_packed_gemini_refresh(loaded.refresh.as_deref())
            .0
            .as_deref(),
        Some("refresh-token")
    );
    assert_eq!(loaded.project_id.as_deref(), Some("proj-1"));
    assert_eq!(loaded.managed_project_id.as_deref(), Some("managed-1"));
    std::env::remove_var("HERMES_GEMINI_OAUTH_FILE");
}

#[tokio::test]
async fn resolve_qwen_runtime_credentials_reads_qwen_cli_auth_file() {
    let _guard = test_env_lock::lock_async().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let auth_path = tmp.path().join("oauth_creds.json");
    let expiry_date = jiff::Timestamp::now().as_millisecond() + 5 * 60 * 1000;
    let payload = serde_json::json!({
        "access_token": "qwen-access-token",
        "refresh_token": "qwen-refresh-token",
        "token_type": "Bearer",
        "resource_url": "portal.qwen.ai",
        "expiry_date": expiry_date,
    });
    std::fs::write(&auth_path, serde_json::to_string_pretty(&payload).unwrap())
        .expect("write auth file");
    std::env::set_var(
        "HERMES_QWEN_CLI_AUTH_FILE",
        auth_path.to_string_lossy().to_string(),
    );
    std::env::set_var("HERMES_QWEN_BASE_URL", "https://portal.qwen.ai/v1");

    let resolved = resolve_qwen_runtime_credentials(false, false, 120)
        .await
        .expect("resolve");
    assert_eq!(resolved.provider, "qwen-oauth");
    assert_eq!(resolved.api_key, "qwen-access-token");
    assert_eq!(resolved.base_url, "https://portal.qwen.ai/v1".to_string());
    assert_eq!(resolved.expires_at_ms, Some(expiry_date));
    assert_eq!(
        resolved.refresh_token.as_deref(),
        Some("qwen-refresh-token")
    );

    std::env::remove_var("HERMES_QWEN_CLI_AUTH_FILE");
    std::env::remove_var("HERMES_QWEN_BASE_URL");
}

/// Sets or removes environment variables for one test and restores them on drop.
struct ScopedNousEnv(Vec<(&'static str, Option<String>)>);

impl ScopedNousEnv {
    fn new(auth_file: &Path, inference_override: Option<&str>) -> Self {
        let vars: [(&'static str, Option<String>); 5] = [
            (
                "HERMES_AUTH_FILE",
                Some(auth_file.to_string_lossy().to_string()),
            ),
            ("HERMES_NOUS_OAUTH_FILE", None),
            (
                "NOUS_INFERENCE_BASE_URL",
                inference_override.map(str::to_string),
            ),
            ("HERMES_PORTAL_BASE_URL", None),
            ("NOUS_PORTAL_BASE_URL", None),
        ];
        let prior = vars
            .iter()
            .map(|(name, _)| (*name, std::env::var(name).ok()))
            .collect();
        for (name, value) in vars {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        Self(prior)
    }
}

impl Drop for ScopedNousEnv {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

/// A Nous Portal stub on 127.0.0.1: the device-code endpoint approves at once and the token
/// endpoint answers `token_body`. Returns the portal base URL.
async fn spawn_nous_portal_stub(token_body: String) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind portal stub");
    let addr = listener.local_addr().expect("portal stub addr");
    let device_body = serde_json::json!({
        "device_code": "dev",
        "user_code": "ABCD-1234",
        "verification_uri_complete": "https://portal.example/device?code=ABCD",
        "expires_in": 600,
        "interval": 1,
    })
    .to_string();
    tokio::spawn(async move {
        while let Ok((mut stream, _peer)) = listener.accept().await {
            // Read the whole request before answering so the close does not reset the client.
            let mut request = Vec::new();
            let mut buf = [0_u8; 4096];
            while let Ok(n) = stream.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            if name.eq_ignore_ascii_case("content-length") {
                                value.trim().parse::<usize>().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let body = if request.starts_with(b"POST /api/oauth/device/code ") {
                &device_body
            } else {
                &token_body
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    format!("http://{addr}")
}

fn nous_token_body(portal_inference_url: &str) -> String {
    serde_json::json!({
        "access_token": nous_test_jwt(3600, Value::String(NOUS_INFERENCE_INVOKE_SCOPE.to_string())),
        "refresh_token": "refreshed",
        "scope": NOUS_INFERENCE_INVOKE_SCOPE,
        "expires_in": 3600,
        "inference_base_url": portal_inference_url,
    })
    .to_string()
}

async fn nous_device_login_with_portal_inference_url(
    portal_inference_url: &str,
    inference_override: Option<&str>,
) -> NousAuthState {
    let tmp = tempfile::tempdir().expect("tempdir");
    let _env = ScopedNousEnv::new(&tmp.path().join("auth.json"), inference_override);
    let portal = spawn_nous_portal_stub(nous_token_body(portal_inference_url)).await;
    login_nous_device_code(NousDeviceCodeOptions {
        portal_base_url: Some(portal),
        open_browser: false,
        ..NousDeviceCodeOptions::default()
    })
    .await
    .expect("device login")
}

#[tokio::test]
async fn nous_device_login_refuses_portal_inference_url_outside_allowlist() {
    let _guard = test_env_lock::lock_async().await;
    let state =
        nous_device_login_with_portal_inference_url("https://attacker.example.com/v1", None).await;
    assert_eq!(state.inference_base_url, DEFAULT_NOUS_INFERENCE_URL);
}

#[tokio::test]
async fn nous_device_login_refuses_non_https_portal_inference_url() {
    let _guard = test_env_lock::lock_async().await;
    let state = nous_device_login_with_portal_inference_url(
        "http://inference-api.nousresearch.com/v1",
        None,
    )
    .await;
    assert_eq!(state.inference_base_url, DEFAULT_NOUS_INFERENCE_URL);
}

#[tokio::test]
async fn nous_device_login_keeps_allowlisted_portal_inference_url() {
    let _guard = test_env_lock::lock_async().await;
    let state = nous_device_login_with_portal_inference_url(
        "https://inference-api.nousresearch.com/v2/",
        None,
    )
    .await;
    assert_eq!(
        state.inference_base_url,
        "https://inference-api.nousresearch.com/v2"
    );
}

#[tokio::test]
async fn nous_device_login_keeps_inference_host_named_by_operator_override() {
    let _guard = test_env_lock::lock_async().await;
    let state = nous_device_login_with_portal_inference_url(
        "https://inference.staging.example.com/v2",
        Some("https://inference.staging.example.com/v1"),
    )
    .await;
    assert_eq!(
        state.inference_base_url,
        "https://inference.staging.example.com/v2"
    );
}

#[tokio::test]
async fn nous_refresh_refuses_portal_inference_url_outside_allowlist() {
    let _guard = test_env_lock::lock_async().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let _env = ScopedNousEnv::new(&tmp.path().join("auth.json"), None);
    let mut state = nous_test_state(nous_test_jwt(
        900,
        Value::String(NOUS_INFERENCE_INVOKE_SCOPE.to_string()),
    ));
    state.portal_base_url =
        spawn_nous_portal_stub(nous_token_body("https://attacker.example.com/v1")).await;
    save_nous_auth_state(&state).expect("save nous state");

    let resolved = resolve_nous_runtime_credentials(
        true,
        true,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await
    .expect("refresh nous runtime credentials");
    assert_eq!(resolved.refresh_token.as_deref(), Some("refreshed"));
    assert_eq!(resolved.base_url, DEFAULT_NOUS_INFERENCE_URL);
    let saved = read_provider_auth_state("nous")
        .expect("read saved state")
        .expect("saved state");
    assert_eq!(
        saved.get("inference_base_url").and_then(Value::as_str),
        Some(DEFAULT_NOUS_INFERENCE_URL)
    );
}

#[tokio::test]
async fn nous_runtime_credentials_heal_stored_inference_url_outside_allowlist() {
    let _guard = test_env_lock::lock_async().await;
    for stored in [
        "https://attacker.example.com/v1",
        "http://inference-api.nousresearch.com/v1",
        "https://inference-api.nousresearch.com.attacker.example/v1",
        "https://inference-api.nousresearch.com@attacker.example.com/v1",
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedNousEnv::new(&tmp.path().join("auth.json"), None);
        let mut state = nous_test_state(nous_test_jwt(
            900,
            Value::String(NOUS_INFERENCE_INVOKE_SCOPE.to_string()),
        ));
        state.inference_base_url = stored.to_string();
        save_nous_auth_state(&state).expect("save nous state");

        let resolved = resolve_nous_runtime_credentials(
            false,
            true,
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
        )
        .await
        .expect("resolve nous runtime credentials");
        assert_eq!(resolved.base_url, DEFAULT_NOUS_INFERENCE_URL, "{stored}");
    }
}

#[test]
fn read_nous_auth_state_heals_stored_portal_url_outside_allowlist() {
    let _guard = test_env_lock::lock();
    for (stored, expected) in [
        ("http://portal.nousresearch.com", DEFAULT_NOUS_PORTAL_URL),
        ("https://api.nousresearch.com", DEFAULT_NOUS_PORTAL_URL),
        ("https://attacker.example.com", DEFAULT_NOUS_PORTAL_URL),
        ("http://127.0.0.1:8080/", "http://127.0.0.1:8080"),
        ("https://portal.nousresearch.com/", DEFAULT_NOUS_PORTAL_URL),
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedNousEnv::new(&tmp.path().join("auth.json"), None);
        let mut state = nous_test_state("nous-access".to_string());
        state.agent_key = Some("nous-agent-key".to_string());
        state.portal_base_url = stored.to_string();
        save_nous_auth_state(&state).expect("save nous state");

        let read = read_nous_auth_state()
            .expect("read nous state")
            .expect("nous state");
        assert_eq!(read.portal_base_url, expected, "{stored}");
    }
}

#[cfg(unix)]
#[test]
fn owner_only_credential_write_keeps_a_symlinked_store() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("vault").join("google_oauth.json");
    std::fs::create_dir_all(real.parent().expect("parent")).expect("vault dir");
    let link = tmp.path().join("google_oauth.json");
    std::os::unix::fs::symlink(&real, &link).expect("link");

    write_owner_only_atomic(&link, "{\"refresh\":\"made-up\"}\n").expect("write store");

    assert!(link.is_symlink(), "the credential link was replaced");
    assert_eq!(
        std::fs::read_to_string(&real).expect("read target"),
        "{\"refresh\":\"made-up\"}\n"
    );
    let mode = std::fs::metadata(&real)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn owner_only_credential_write_refuses_a_link_whose_target_folder_is_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // As a link into a file system that is not mounted leaves it.
    let missing = tmp.path().join("unmounted");
    let link = tmp.path().join("google_oauth.json");
    std::os::unix::fs::symlink(missing.join("google_oauth.json"), &link).expect("link");

    assert!(write_owner_only_atomic(&link, "{}\n").is_err());
    assert!(!missing.exists(), "the link's target folder was created");
}
