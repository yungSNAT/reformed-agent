use super::*;

fn sample_credential(provider: &str, refresh_token: Option<&str>) -> OAuthCredential {
    OAuthCredential {
        provider: provider.to_string(),
        access_token: "access-token-value".to_string(),
        refresh_token: refresh_token.map(str::to_string),
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
