// `auth verify` outcomes that read HOME, HERMES_HOME and provider key
// variables. Each test runs in an isolated child, which owns its environment.

fn isolated_verify_test(test: &str) -> bool {
    let (_, module) = module_path!().split_once("::").expect("test module path");
    crate::isolated_test_profile::enter(&format!("{module}::{test}"))
}

async fn fixture_vault() -> (tempfile::TempDir, FileTokenStore, AuthManager) {
    let dir = tempfile::tempdir().expect("vault directory");
    let store = FileTokenStore::new(dir.path().join("fixture-store.json"))
        .await
        .expect("open vault");
    let manager = AuthManager::new(store.clone());
    (dir, store, manager)
}

async fn verify(provider: &str, store: &FileTokenStore, manager: &AuthManager) -> AuthVerifyResult {
    verify_single_oauth_provider(provider, store, manager)
        .await
        .expect("verify provider")
}

fn fixture_value(kind: &str) -> String {
    format!("fixture-{kind}-{}", std::process::id())
}

#[tokio::test]
async fn openai_verify_reports_env_missing_and_imports_an_existing_session() {
    const TEST: &str = "openai_verify_reports_env_missing_and_imports_an_existing_session";
    if !isolated_verify_test(TEST) {
        return;
    }
    use base64::Engine as _;
    let (_dir, store, manager) = fixture_vault().await;

    std::env::set_var("HERMES_OPENAI_API_KEY", fixture_value("env"));
    let from_env = verify("openai", &store, &manager).await;
    assert_eq!(from_env.outcome.as_str(), "unverified");
    assert!(from_env.credential_present);
    assert_eq!(
        from_env.detail.as_deref(),
        Some("Environment token is present but no OAuth credential state was available.")
    );
    std::env::remove_var("HERMES_OPENAI_API_KEY");

    let missing = verify("openai", &store, &manager).await;
    assert_eq!(missing.outcome.as_str(), "missing");
    assert!(!missing.credential_present);
    assert!(store.get("openai").await.is_none());

    // An existing session file is imported into the vault and reported valid.
    let access = fixture_value("openai-access");
    let refresh = fixture_value("openai-refresh");
    let exp = jiff::Timestamp::now().as_second() + 3600;
    let identity = format!(
        "e30.{}.fixture",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::json!({ "exp": exp }).to_string())
    );
    let session_dir = std::path::PathBuf::from(std::env::var_os("HOME").expect("isolated HOME")).join(".codex");
    std::fs::create_dir_all(&session_dir).expect("session folder");
    std::fs::write(
        session_dir.join("auth.json"),
        serde_json::json!({
            "tokens": {
                "access_token": access,
                "refresh_token": refresh,
                "id_token": identity,
            }
        })
        .to_string(),
    )
    .expect("write existing session");
    let imported = verify("openai", &store, &manager).await;
    assert_eq!(imported.outcome.as_str(), "valid");
    assert!(imported.credential_present);
    assert!(imported.expires_at.is_some());
    let saved = store.get("openai").await.expect("imported credential");
    assert_eq!(saved.access_token, access);
    assert_eq!(saved.refresh_token.as_deref(), Some(refresh.as_str()));
    assert_eq!(saved.token_type, "bearer");
    let expires = saved.expires_at.expect("expiry from the identity token");
    assert!(expires.as_second() > jiff::Timestamp::now().as_second());
    assert!(expires.as_second() <= exp + 1);
}

#[tokio::test]
async fn other_provider_verify_reports_env_stored_expired_and_missing_credentials() {
    const TEST: &str = "other_provider_verify_reports_env_stored_expired_and_missing_credentials";
    if !isolated_verify_test(TEST) {
        return;
    }
    let (_dir, store, manager) = fixture_vault().await;

    let missing = verify("openrouter", &store, &manager).await;
    assert_eq!(missing.outcome.as_str(), "missing");
    assert!(!missing.credential_present);

    let mut credential = OAuthCredential {
        provider: "openrouter".to_string(),
        access_token: fixture_value("openrouter"),
        refresh_token: None,
        token_type: "bearer".to_string(),
        scope: None,
        expires_at: None,
    };
    manager
        .save_credential(credential.clone())
        .await
        .expect("save credential");
    let valid = verify("openrouter", &store, &manager).await;
    assert_eq!(valid.outcome.as_str(), "valid");
    assert!(valid.credential_present);
    assert!(valid.expires_at.is_none());

    let expired_at = hermes_core::time::now_plus_secs(-3600);
    credential.expires_at = Some(expired_at);
    manager
        .save_credential(credential)
        .await
        .expect("save expired credential");
    let expired = verify("openrouter", &store, &manager).await;
    assert_eq!(expired.outcome.as_str(), "expired");
    assert_eq!(
        expired.expires_at,
        Some(hermes_core::time::rfc3339(expired_at))
    );

    std::env::set_var("OPENROUTER_API_KEY", fixture_value("env"));
    let from_env = verify("openrouter", &store, &manager).await;
    assert_eq!(from_env.outcome.as_str(), "unverified");
    assert_eq!(
        from_env.detail.as_deref(),
        Some("Provider uses env credential source; live OAuth verification is unavailable.")
    );

    // A provider without OAuth is reported, not counted as a failure.
    run_auth_verify("openrouter", &store, &manager)
        .await
        .expect("unsupported provider is not a failure");
}

#[tokio::test]
async fn anthropic_verify_reports_expired_unverified_missing_and_unreadable_state() {
    const TEST: &str = "anthropic_verify_reports_expired_unverified_missing_and_unreadable_state";
    if !isolated_verify_test(TEST) {
        return;
    }
    let (_dir, store, manager) = fixture_vault().await;
    let now = jiff::Timestamp::now().as_millisecond();

    let expired_ms = now - 3_600_000;
    save_provider_auth_state(
        "anthropic",
        serde_json::json!({
            "access_token": fixture_value("anthropic"),
            "expires_at_ms": expired_ms,
        }),
    )
    .expect("save expired state");
    let expired = verify("anthropic", &store, &manager).await;
    assert_eq!(expired.outcome.as_str(), "expired");
    assert!(expired.oauth_state_present);
    assert_eq!(
        expired.expires_at,
        jiff::Timestamp::from_millisecond(expired_ms)
            .ok()
            .map(hermes_core::time::rfc3339)
    );
    assert!(expired.detail.is_none());

    // A future expiry without an access token cannot be verified.
    save_provider_auth_state(
        "anthropic",
        serde_json::json!({ "expires_at_ms": now + 3_600_000 }),
    )
    .expect("save tokenless state");
    let tokenless = verify("anthropic", &store, &manager).await;
    assert_eq!(tokenless.outcome.as_str(), "unverified");

    save_provider_auth_state("anthropic", serde_json::json!({ "source": "fixture" }))
        .expect("save state without expiry");
    let without_expiry = verify("anthropic", &store, &manager).await;
    assert_eq!(without_expiry.outcome.as_str(), "missing");

    std::env::set_var("ANTHROPIC_API_KEY", fixture_value("env"));
    let from_env = verify("anthropic", &store, &manager).await;
    assert_eq!(from_env.outcome.as_str(), "unverified");
    assert!(from_env.credential_present);
    std::env::remove_var("ANTHROPIC_API_KEY");

    save_provider_auth_state("anthropic", serde_json::json!("fixture-not-an-object"))
        .expect("save unreadable state");
    let unreadable = verify("anthropic", &store, &manager).await;
    assert_eq!(unreadable.outcome.as_str(), "missing");
    assert_eq!(
        unreadable.detail.as_deref(),
        Some("Anthropic OAuth status could not be read.")
    );
}
