use super::*;
use serde_json::json;
use std::fs;

#[test]
fn whitespace_only_auth_store_has_no_saved_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.json");
    fs::write(&path, " \r\n\t ").unwrap();

    let store = load_auth_store(&path).unwrap();
    assert!(store.providers.is_empty());
    assert!(store.active_provider.is_none());
    assert!(store.updated_at.is_none());
}

#[cfg(unix)]
#[test]
fn atomic_write_rejects_a_path_without_a_parent() {
    let result = write_owner_only_atomic(Path::new("/"), "{}");
    assert!(matches!(
        result,
        Err(AgentError::Io(message)) if message == "credential path / has no parent directory"
    ));
}

#[test]
fn atomic_write_cleans_up_when_rename_fails() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("auth.json");
    fs::create_dir(&target).unwrap();
    let marker = target.join("keep");
    fs::write(&marker, "original").unwrap();

    let result = write_owner_only_atomic(&target, "{}");
    assert!(matches!(
        result,
        Err(AgentError::Io(message))
            if message.starts_with("rename ")
                && message.contains(&format!(" -> {}:", target.display()))
    ));
    assert_eq!(fs::read_to_string(marker).unwrap(), "original");
    let remaining: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(remaining, vec![target]);
}

#[test]
fn anthropic_flat_import_accepts_aliases_and_normalizes_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");
    fs::write(
        &path,
        json!({
            "api_key": " fixture-access ",
            "refreshToken": " fixture-refresh ",
            "expires": "1700000000",
            "source": " \t "
        })
        .to_string(),
    )
    .unwrap();

    let imported = load_anthropic_oauth_import_from_path(&path).unwrap();
    assert_eq!(imported.source_path, path);
    assert_eq!(imported.state.access_token, "fixture-access");
    assert_eq!(
        imported.state.refresh_token.as_deref(),
        Some("fixture-refresh")
    );
    assert_eq!(imported.state.expires_at_ms, Some(1_700_000_000_000));
    assert_eq!(imported.source, "discovered_external");

    fs::write(
        &path,
        json!({
            "access_token": " preferred ",
            "api_key": "ignored",
            "refresh_token": " \t ",
            "expiresAt": 1_700_000_000_123_i64,
            "source": " fixture-source "
        })
        .to_string(),
    )
    .unwrap();
    let imported = load_anthropic_oauth_import_from_path(&path).unwrap();
    assert_eq!(imported.state.access_token, "preferred");
    assert!(imported.state.refresh_token.is_none());
    assert_eq!(imported.state.expires_at_ms, Some(1_700_000_000_123));
    assert_eq!(imported.source, "fixture-source");
}

fn fixture_token(claims: &Value) -> String {
    let payload = base64::Engine::encode(&BASE64_URL_SAFE_NO_PAD, claims.to_string());
    format!("fixture.{payload}.fixture")
}

fn nous_fixture(token: &str) -> NousAuthState {
    NousAuthState {
        portal_base_url: "not a URL".to_owned(),
        inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_owned(),
        client_id: " \t ".to_owned(),
        scope: Some(" \t ".to_owned()),
        token_type: "Bearer".to_owned(),
        access_token: token.to_owned(),
        refresh_token: None,
        obtained_at: "2020-01-01T00:00:00Z".to_owned(),
        expires_at: None,
        expires_in: None,
        agent_key: Some(token.to_owned()),
        agent_key_id: Some("previous-id".to_owned()),
        agent_key_expires_at: None,
        agent_key_expires_in: None,
        agent_key_reused: Some(true),
        agent_key_obtained_at: None,
    }
}

#[test]
fn nous_token_without_exp_claim_uses_stored_expiry() {
    let token = fixture_token(&json!({"scp": [NOUS_INFERENCE_INVOKE_SCOPE]}));
    let past = "2020-01-01T00:00:00Z";
    let future = "2099-01-01T00:00:00Z";

    assert!(timestamp_is_expiring(Some(past), i64::MIN));
    assert!(!timestamp_is_expiring(Some(future), 60));
    assert!(timestamp_is_expiring(Some(future), i64::MAX));

    for expiry in [None, Some("not a timestamp"), Some(past)] {
        assert_eq!(
            nous_invoke_jwt_status(&token, None, expiry, 60),
            Some("invoke_jwt_expiry_unknown_or_expiring")
        );
    }
    assert_eq!(nous_invoke_jwt_status(&token, None, Some(future), 60), None);
}

#[test]
fn nous_expiry_fallback_is_trimmed_and_blank_fallback_is_ignored() {
    let out_of_range = fixture_token(&json!({"exp": i64::MAX}));
    assert_eq!(
        nous_jwt_expires_at(&out_of_range, Some(" 2099-01-01T00:00:00Z \t")).as_deref(),
        Some("2099-01-01T00:00:00Z")
    );
    assert_eq!(nous_jwt_expires_at(&out_of_range, Some(" \t ")), None);
    assert_eq!(nous_jwt_expires_at(&out_of_range, None), None);
}

#[test]
fn blank_nous_access_token_is_rejected_without_overwriting_the_agent_key() {
    let mut state = nous_fixture(" \t ");
    state.agent_key = Some("previous-fixture-key".to_owned());
    assert!(!nous_invoke_jwt_is_usable(
        &state.access_token,
        Some(NOUS_INFERENCE_INVOKE_SCOPE),
        Some("2099-01-01T00:00:00Z"),
        0
    ));
    assert!(matches!(
        assert_nous_invoke_jwt_usable(&state, None, 0),
        Err(AgentError::AuthFailed(message))
            if message.contains("(access_token_not_jwt)")
                && message.contains("hermes auth add nous")
    ));

    let before = serde_json::to_value(&state).unwrap();
    set_nous_agent_key_from_invoke_jwt(&mut state);
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
}

#[tokio::test]
async fn nous_refresh_rejects_missing_tokens_and_invalid_portal_urls() {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut state = nous_fixture("fixture-access");
    for refresh_token in [None, Some(" \t ".to_owned())] {
        state.refresh_token = refresh_token;
        let before = serde_json::to_value(&state).unwrap();
        assert!(matches!(
            refresh_nous_access_token(&mut state, &client).await,
            Err(AgentError::AuthFailed(message))
                if message == "Session expired and no refresh token is available for Nous OAuth."
        ));
        assert_eq!(serde_json::to_value(&state).unwrap(), before);
    }

    // The malformed portal URL fails during request construction, before any I/O.
    state.refresh_token = Some("fixture-refresh".to_owned());
    let before = serde_json::to_value(&state).unwrap();
    assert!(matches!(
        refresh_nous_access_token(&mut state, &client).await,
        Err(AgentError::AuthFailed(message))
            if message.starts_with("Nous OAuth refresh failed:")
    ));
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
}

#[cfg(unix)]
fn check_openai_discovery(root: &Path, store_path: &Path) {
    assert!(discover_existing_openai_oauth().unwrap().is_none());
    assert!(discover_existing_openai_codex_oauth().unwrap().is_none());

    let external = json!({
        "tokens": {"access_token": " fixture-external ", "refresh_token": " fixture-refresh "},
        "last_refresh": "2020-01-01T00:00:00Z"
    })
    .to_string();
    let openai_path = root.join("openai-external.json");
    fs::write(&openai_path, &external).unwrap();
    let imported = discover_existing_openai_oauth().unwrap().unwrap();
    assert_eq!(imported.source_path, openai_path);
    assert_eq!(imported.state.tokens.access_token, "fixture-external");
    fs::remove_file(openai_path).unwrap();

    let mut codex = CodexAuthState {
        tokens: CodexTokens {
            access_token: " \t ".to_owned(),
            refresh_token: None,
            expires_in: None,
        },
        base_url: " ".to_owned(),
        last_refresh: String::new(),
        auth_mode: Some(" \t ".to_owned()),
        source: None,
    };
    assert_eq!(save_codex_auth_state(&codex).unwrap().as_path(), store_path);
    assert!(load_codex_oauth_import_from_store(store_path).is_none());

    codex.tokens.access_token = "fixture-codex".to_owned();
    assert_eq!(save_codex_auth_state(&codex).unwrap().as_path(), store_path);
    let codex_value = serde_json::to_value(&codex).unwrap();

    codex.tokens.access_token = "fixture-openai".to_owned();
    assert_eq!(
        save_openai_auth_state(&codex).unwrap().as_path(),
        store_path
    );
    let store = load_auth_store(store_path).unwrap();
    assert_eq!(store.active_provider.as_deref(), Some("openai"));
    assert_eq!(store.providers.get("openai-codex"), Some(&codex_value));
    assert_eq!(
        store.providers.get("openai"),
        Some(&serde_json::to_value(&codex).unwrap())
    );

    let codex_path = root.join("codex-external.json");
    fs::write(&codex_path, external).unwrap();
    assert!(discover_existing_openai_oauth().unwrap().is_none());
    let imported = discover_existing_openai_codex_oauth().unwrap().unwrap();
    assert_eq!(imported.source_path, codex_path);
    assert_eq!(imported.state.tokens.access_token, "fixture-external");
    fs::remove_file(codex_path).unwrap();

    let imported = discover_existing_openai_codex_oauth().unwrap().unwrap();
    assert_eq!(imported.source_path.as_path(), store_path);
    assert_eq!(imported.state.tokens.access_token, "fixture-codex");
    assert_eq!(imported.state.base_url, DEFAULT_CODEX_BASE_URL);
    assert!(parse_iso_timestamp_utc(&imported.state.last_refresh).is_some());
    assert_eq!(imported.state.auth_mode.as_deref(), Some("chatgpt"));
    assert_eq!(
        imported.state.source.as_deref(),
        Some("discovered_auth_store")
    );
}

#[cfg(unix)]
fn check_anthropic_discovery(root: &Path, store_path: &Path) {
    assert!(discover_existing_anthropic_oauth().unwrap().is_none());
    let config = root.join("claude-config");
    fs::create_dir(&config).unwrap();
    let path = config.join(".credentials.json");
    fs::write(
        &path,
        r#"{"claudeAiOauth":{"accessToken":" fixture-claude ","expiresAt":1700000000}}"#,
    )
    .unwrap();

    let imported = discover_existing_anthropic_oauth().unwrap().unwrap();
    assert_eq!(imported.source_path, path);
    assert_eq!(imported.source, "claude_code_credentials_file");
    assert_eq!(imported.state.access_token, "fixture-claude");
    assert_eq!(imported.state.expires_at_ms, Some(1_700_000_000_000));
    fs::remove_file(path).unwrap();

    let saved = save_provider_auth_state(
        "anthropic",
        json!({
            "api_key": " fixture-stored ",
            "refresh_token": " fixture-refresh ",
            "expires": "1700000001",
            "source": " \t "
        }),
    )
    .unwrap();
    assert_eq!(saved.as_path(), store_path);
    let imported = discover_existing_anthropic_oauth().unwrap().unwrap();
    assert_eq!(imported.source_path.as_path(), store_path);
    assert_eq!(imported.source, "hermes_pkce");
    assert_eq!(imported.state.access_token, "fixture-stored");
    assert_eq!(
        imported.state.refresh_token.as_deref(),
        Some("fixture-refresh")
    );
    assert_eq!(imported.state.expires_at_ms, Some(1_700_000_001_000));
}

#[cfg(target_os = "macos")]
fn check_fake_keychain(root: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let security = root.join("bin").join("security");
    fs::write(&security, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&security, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(run_anthropic_keychain_read().is_none());

    fs::write(&security, "#!/bin/sh\nprintf '  \\n'\n").unwrap();
    assert!(run_anthropic_keychain_read().is_none());

    fs::write(
        &security,
        "#!/bin/sh\nprintf '%s\\n' '  {\"claudeAiOauth\":{\"accessToken\":\"fixture-keychain\"}}  '\n",
    )
    .unwrap();
    assert_eq!(
        run_anthropic_keychain_read().as_deref(),
        Some(r#"{"claudeAiOauth":{"accessToken":"fixture-keychain"}}"#)
    );
    let imported = discover_existing_anthropic_oauth().unwrap().unwrap();
    assert_eq!(imported.source_path, anthropic_keychain_source_path());
    assert_eq!(imported.source, "macos_keychain");
    assert_eq!(imported.state.access_token, "fixture-keychain");
    fs::remove_file(security).unwrap();
}

#[cfg(unix)]
async fn check_nous_discovery_and_resolution(root: &Path, store_path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let token = fixture_token(&json!({
        "scope": NOUS_INFERENCE_INVOKE_SCOPE,
        "exp": 4_070_908_800_i64
    }));
    let state = nous_fixture(&token);
    let path = root.join("nous-external.json");
    fs::write(&path, "{").unwrap();
    assert!(load_nous_oauth_import_from_path(&path).is_none());

    fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();
    let imported = discover_existing_nous_oauth().unwrap().unwrap();
    assert_eq!(imported.source_path, path);
    assert_eq!(imported.state.access_token, token);
    fs::remove_file(path).unwrap();

    assert_eq!(save_nous_auth_state(&state).unwrap().as_path(), store_path);
    let before = fs::read(store_path).unwrap();
    assert!(matches!(
        resolve_nous_runtime_credentials(true, false, 0, 0).await,
        Err(AgentError::AuthFailed(message))
            if message.contains("(force_refresh)")
                && message.contains("no refresh token is available")
    ));
    assert_eq!(fs::read(store_path).unwrap(), before);

    let resolved = resolve_nous_runtime_credentials(false, false, 0, 0)
        .await
        .unwrap();
    assert_eq!(resolved.provider, "nous");
    assert_eq!(resolved.api_key, token);
    assert_eq!(resolved.scope.as_deref(), Some(DEFAULT_NOUS_SCOPE));
    assert!(resolved.key_id.is_none());

    let saved: NousAuthState = serde_json::from_value(
        read_provider_auth_state_from_store_path(store_path, "nous").unwrap(),
    )
    .unwrap();
    assert_eq!(saved.client_id, DEFAULT_NOUS_CLIENT_ID);
    assert_eq!(saved.scope.as_deref(), Some(DEFAULT_NOUS_SCOPE));
    assert_eq!(saved.agent_key.as_deref(), Some(token.as_str()));
    assert_eq!(saved.agent_key_reused, Some(false));
    assert_eq!(
        fs::metadata(store_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[tokio::test]
async fn auth_discovery_and_resolution_use_only_isolated_fixtures() {
    const SANDBOX_ENV: &str = "HERMES_AUTH_COVERAGE_TEST_SANDBOX";
    if let Some(root) = std::env::var_os(SANDBOX_ENV).map(PathBuf::from) {
        assert_eq!(dirs::home_dir().as_deref(), Some(root.as_path()));
        let store_path = auth_json_path();
        assert!(store_path.starts_with(&root) && store_path != root);
        check_openai_discovery(&root, &store_path);
        check_anthropic_discovery(&root, &store_path);
        #[cfg(target_os = "macos")]
        check_fake_keychain(&root);
        check_nous_discovery_and_resolution(&root, &store_path).await;
        return;
    }

    // A subprocess isolates discovery from the caller's home and avoids changing
    // environment variables while other unit tests are running.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::create_dir(root.join("bin")).unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let test = format!("{module}::auth_discovery_and_resolution_use_only_isolated_fixtures");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg(test)
        .arg("--nocapture")
        .env_clear()
        .env(SANDBOX_ENV, &root)
        .env("HOME", &root)
        .env("HERMES_HOME", root.join(".hermes"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("PATH", root.join("bin"))
        .env(
            "HERMES_OPENAI_OAUTH_FILE",
            root.join("openai-external.json"),
        )
        .env(
            "HERMES_OPENAI_CODEX_OAUTH_FILE",
            root.join("codex-external.json"),
        )
        .env(
            "CLAUDE_CONFIG_DIR",
            format!("  {}  ", root.join("claude-config").display()),
        )
        .env("HERMES_NOUS_OAUTH_FILE", root.join("nous-external.json"))
        .env("HERMES_PORTAL_BASE_URL", "not a URL");
    for name in ["LLVM_PROFILE_FILE", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }

    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("running 1 test"),
        "isolated test exited with {}:\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
