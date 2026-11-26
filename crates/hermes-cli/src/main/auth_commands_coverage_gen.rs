use super::*;

fn sample_credential(provider: &str, refresh_token: Option<&str>) -> OAuthCredential {
    OAuthCredential {
        provider: provider.to_string(),
        access_token: "access-token-value".to_string(),
        refresh_token: refresh_token.map(str::to_string), // nosemgrep: codacy.yaml.security.hard-coded-tokens
        token_type: "bearer".to_string(),
        scope: None,
        expires_at: None,
    }
}

#[test]
fn auth_verify_outcome_as_str_maps_every_variant() {
    assert_eq!(AuthVerifyOutcome::Valid.as_str(), "valid");
    assert_eq!(
        AuthVerifyOutcome::ValidRefreshed.as_str(),
        "valid_refreshed"
    );
    assert_eq!(AuthVerifyOutcome::Unverified.as_str(), "unverified");
    assert_eq!(AuthVerifyOutcome::Missing.as_str(), "missing");
    assert_eq!(AuthVerifyOutcome::Expired.as_str(), "expired");
    assert_eq!(AuthVerifyOutcome::RefreshFailed.as_str(), "refresh_failed");
}

#[test]
fn auth_verify_outcome_is_success_only_for_valid_like_states() {
    assert!(AuthVerifyOutcome::Valid.is_success());
    assert!(AuthVerifyOutcome::ValidRefreshed.is_success());
    assert!(AuthVerifyOutcome::Unverified.is_success());
    assert!(!AuthVerifyOutcome::Missing.is_success());
    assert!(!AuthVerifyOutcome::Expired.is_success());
    assert!(!AuthVerifyOutcome::RefreshFailed.is_success());
}

#[test]
fn print_auth_verify_result_handles_optional_fields() {
    let with_extras = AuthVerifyResult {
        provider: "openai".to_string(),
        outcome: AuthVerifyOutcome::Expired,
        source: "token_store".to_string(),
        credential_present: true,
        oauth_state_present: false,
        expires_at: Some("2020-01-01T00:00:00Z".to_string()),
        detail: Some("some detail".to_string()),
    };
    print_auth_verify_result(&with_extras);
    assert_eq!(with_extras.outcome.as_str(), "expired");
    assert_eq!(
        with_extras.expires_at.as_deref(),
        Some("2020-01-01T00:00:00Z")
    );

    let without_extras = AuthVerifyResult {
        provider: "anthropic".to_string(),
        outcome: AuthVerifyOutcome::Missing,
        source: "none".to_string(),
        credential_present: false,
        oauth_state_present: false,
        expires_at: None,
        detail: None,
    };
    print_auth_verify_result(&without_extras);
    assert_eq!(without_extras.outcome.as_str(), "missing");
    assert!(without_extras.detail.is_none());
}

#[test]
fn nous_auth_error_requires_fresh_login_detects_revocation_phrases() {
    let phrases = [
        "invalid_grant",
        "Refresh token reuse detected",
        "refresh session has been revoked",
        "Session has been revoked",
        "Stored Nous auth state is invalid",
        "missing refresh token",
        "no refresh token available",
    ];
    for phrase in phrases {
        let err = AgentError::AuthFailed(phrase.to_string());
        assert!(
            nous_auth_error_requires_fresh_login(&err),
            "expected fresh login for {phrase}"
        );
    }
}

#[test]
fn nous_auth_error_requires_fresh_login_ignores_other_errors() {
    let err = AgentError::AuthFailed("network timeout while contacting portal".to_string());
    assert!(!nous_auth_error_requires_fresh_login(&err));
    let err = AgentError::Config("bad yaml".to_string());
    assert!(!nous_auth_error_requires_fresh_login(&err));
}

#[tokio::test]
async fn refresh_oauth_store_credential_requires_refresh_token() {
    let cred = sample_credential("openai", None);
    match refresh_oauth_store_credential("openai", &cred).await {
        Err(AgentError::AuthFailed(msg)) => {
            assert!(msg.contains("refresh token missing"), "{msg}");
            assert!(msg.contains("openai"), "{msg}");
        }
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("expected error for missing refresh token"),
    }
}

#[tokio::test]
async fn refresh_oauth_store_credential_rejects_blank_refresh_token() {
    let cred = sample_credential("anthropic", Some("   "));
    match refresh_oauth_store_credential("anthropic", &cred).await {
        Err(AgentError::AuthFailed(msg)) => {
            assert!(msg.contains("refresh token missing"), "{msg}");
            assert!(msg.contains("anthropic"), "{msg}");
        }
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("expected error for blank refresh token"),
    }
}

#[tokio::test]
async fn refresh_oauth_store_credential_rejects_unconfigured_provider() {
    let cred = sample_credential("nous", Some("refresh-token-value"));
    match refresh_oauth_store_credential("nous", &cred).await {
        Err(AgentError::AuthFailed(msg)) => {
            assert!(msg.contains("OAuth refresh not configured"), "{msg}");
            assert!(msg.contains("nous"), "{msg}");
        }
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("expected error for unconfigured provider"),
    }
}

#[tokio::test]
async fn auth_verify_refreshes_a_fixture_credential_and_keeps_it_on_rejection() {
    use wiremock::{
        matchers::{body_string_contains, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    let (_, module) = module_path!().split_once("::").unwrap();
    let test_name =
        format!("{module}::auth_verify_refreshes_a_fixture_credential_and_keeps_it_on_rejection");
    if !crate::isolated_test_profile::enter(&test_name) {
        return;
    }

    let server = MockServer::start().await;
    let home = tempfile::tempdir().unwrap();
    let credential_store = home.path().join("fixture-store.json");
    let store = FileTokenStore::new(&credential_store).await.unwrap();
    let manager = AuthManager::new(store.clone());
    let provider = "openai";
    let mut expired = sample_credential(provider, Some("fixture-old-renewal"));
    expired.expires_at = Some(hermes_core::time::now_plus_secs(-3600));
    manager.save_credential(expired.clone()).await.unwrap();
    std::env::set_var("HERMES_OPENAI_OAUTH_CLIENT_ID", "fixture-client");
    let set_endpoint = |path: &str| {
        let url = format!("{}{path}", server.uri());
        std::env::set_var("HERMES_OPENAI_OAUTH_TOKEN_URL", &url);
        assert_eq!(oauth_refresh_config_for_provider(provider).unwrap().0, url);
    };
    set_endpoint("/refresh");
    Mock::given(method("POST"))
        .and(path("/refresh"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=fixture-old-renewal"))
        .and(body_string_contains("client_id=fixture-client"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fixture-updated-access",
            "refresh_token": "fixture-updated-renewal",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;
    let verified = verify_single_oauth_provider(provider, &store, &manager)
        .await
        .unwrap();
    assert_eq!(verified.outcome.as_str(), "valid_refreshed");
    assert_eq!(verified.source, "token_store_refresh");
    assert!(verified.credential_present);
    let reopened = FileTokenStore::new(&credential_store).await.unwrap();
    let saved = reopened.get(provider).await.unwrap();
    assert_eq!(saved.access_token, "fixture-updated-access");
    assert_eq!(saved.refresh_token.as_deref(), Some("fixture-updated-renewal"));
    assert!(saved.expires_at.is_some());
    assert!(!saved.is_expired(60));
    run_auth_verify(provider, &reopened, &AuthManager::new(reopened.clone()))
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    set_endpoint("/declined");
    Mock::given(method("POST"))
        .and(path("/declined"))
        .and(body_string_contains("refresh_token=fixture-old-renewal"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant"
        })))
        .expect(1)
        .mount(&server)
        .await;
    manager.save_credential(expired.clone()).await.unwrap();
    let rejected = verify_single_oauth_provider(provider, &store, &manager)
        .await
        .unwrap();
    assert_eq!(rejected.outcome.as_str(), "refresh_failed");
    assert!(rejected.detail.is_some());
    let retained = FileTokenStore::new(&credential_store)
        .await
        .unwrap()
        .get(provider)
        .await
        .unwrap();
    assert_eq!(retained.access_token, expired.access_token);
    assert_eq!(retained.refresh_token, expired.refresh_token);
    assert_eq!(retained.expires_at, expired.expires_at);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);

    expired.refresh_token = None;
    manager.save_credential(expired).await.unwrap();
    assert!(matches!(
        run_auth_verify(provider, &store, &manager).await,
        Err(AgentError::AuthFailed(ref message)) if message.contains(provider)
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    server.verify().await;
}

include!("auth_commands_part2_coverage_gen.rs");
