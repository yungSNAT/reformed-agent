use super::*;
use crate::auth::{save_nous_auth_state, DEFAULT_NOUS_CLIENT_ID, DEFAULT_NOUS_INFERENCE_URL};
use crate::test_env_lock;
use tempfile::tempdir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

struct StoreEnv {
    _guards: Vec<EnvVarGuard>,
    _tmp: tempfile::TempDir,
}

fn store_env(portal: &str) -> StoreEnv {
    let tmp = tempdir().expect("tempdir");
    let guards = vec![
        EnvVarGuard::set("NO_PROXY", "127.0.0.1"),
        EnvVarGuard::set("HERMES_HOME", tmp.path()),
        EnvVarGuard::set("HOME", tmp.path()),
        EnvVarGuard::set("HERMES_AUTH_FILE", tmp.path().join("auth.json")),
        EnvVarGuard::set("HERMES_NOUS_OAUTH_FILE", tmp.path().join("nous_oauth.json")),
        EnvVarGuard::set("HERMES_PORTAL_BASE_URL", portal),
    ];
    StoreEnv {
        _guards: guards,
        _tmp: tmp,
    }
}

fn sample_state(access_token: &str, portal_base_url: String) -> NousAuthState {
    NousAuthState {
        portal_base_url,
        inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
        client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
        scope: Some(DEFAULT_NOUS_SCOPE.to_string()),
        token_type: "Bearer".to_string(),
        access_token: access_token.to_string(), // nosemgrep: codacy.yaml.security.hard-coded-tokens
        refresh_token: Some("refresh".to_string()), // nosemgrep: codacy.yaml.security.hard-coded-tokens
        obtained_at: "2026-06-25T00:00:00Z".to_string(),
        expires_at: None,
        expires_in: Some(900),
        agent_key: None,
        agent_key_id: None,
        agent_key_expires_at: None,
        agent_key_expires_in: None,
        agent_key_reused: None,
        agent_key_obtained_at: None,
    }
}

fn offline_client() -> NousBillingClient {
    NousBillingClient {
        access_token: "access".to_string(),
        portal_base_url: "https://portal.example".to_string(),
        client: reqwest::Client::new(),
    }
}

fn encoded_token(claims: &Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claims"));
    format!("header.{payload}.sig")
}

#[test]
fn fail_open_state_records_error_and_portal() {
    if run_isolated("fail_open_state_records_error_and_portal") {
        return;
    }

    let state = BillingState::fail_open("boom", Some("https://p/billing".to_string()));
    assert!(!state.logged_in);
    assert_eq!(state.error.as_deref(), Some("boom"));
    assert_eq!(state.portal_url.as_deref(), Some("https://p/billing"));
    let rendered = render_billing_state(&state);
    assert!(rendered.contains("Not logged into Nous Portal"));
    assert!(rendered.contains("\nLast error: boom"));
    assert!(rendered.contains("\nManage on portal: https://p/billing"));
}

#[test]
fn from_response_maps_additional_error_codes() {
    if run_isolated("from_response_maps_additional_error_codes") {
        return;
    }

    let portal = "https://portal.example";
    let auth = BillingError::from_response(StatusCode::UNAUTHORIZED, json!({}), None, portal);
    assert_eq!(auth.kind, BillingErrorKind::Auth);
    assert_eq!(
        auth.to_string(),
        "Nous Portal authentication required. (401)"
    );

    let revoked = BillingError::from_response(
        StatusCode::FORBIDDEN,
        json!({"error": "remote_spending_revoked"}),
        None,
        portal,
    );
    assert_eq!(
        revoked.to_string(),
        "Remote spending was stopped for this terminal. (403)"
    );

    let disabled = BillingError::from_response(
        StatusCode::BAD_REQUEST,
        json!({"error": "cli_billing_disabled"}),
        None,
        portal,
    );
    assert_eq!(
        disabled.to_string(),
        "Remote spending is disabled for this account. (400)"
    );

    let cap = BillingError::from_response(
        StatusCode::PAYMENT_REQUIRED,
        json!({"error": "monthly_cap_exceeded"}),
        None,
        portal,
    );
    assert_eq!(cap.kind, BillingErrorKind::Http);
    assert_eq!(
        cap.to_string(),
        "This charge would exceed the monthly spending cap. (402)"
    );
}

#[test]
fn auth_error_maps_to_login_hint() {
    if run_isolated("auth_error_maps_to_login_hint") {
        return;
    }

    let rendered = billing_error_to_agent_error(BillingError::auth("Login needed.")).to_string();
    assert!(rendered.contains("Login needed. (401)"));
    assert!(rendered.contains("Run `hermes portal` to log in."));
}

#[test]
fn from_state_rejects_blank_access_token() {
    if run_isolated("from_state_rejects_blank_access_token") {
        return;
    }

    let state = sample_state("   ", "https://portal.example".to_string());
    match NousBillingClient::from_state(&state) {
        Ok(_) => panic!("blank token must be rejected"),
        Err(err) => {
            assert_eq!(err.kind, BillingErrorKind::Auth);
            assert_eq!(err.message, "Not logged into Nous Portal.");
        }
    }
}

#[test]
fn client_exposes_portal_base_url() {
    if run_isolated("client_exposes_portal_base_url") {
        return;
    }

    let client = offline_client();
    assert_eq!(client.portal_base_url(), "https://portal.example");
}

#[tokio::test]
async fn client_validates_inputs_before_requests() {
    if run_isolated("client_validates_inputs_before_requests") {
        return;
    }

    let client = offline_client();

    let err = client.post_charge("50", "   ").await.unwrap_err();
    assert_eq!(err.kind, BillingErrorKind::InvalidRequest);
    assert_eq!(err.error.as_deref(), Some("idempotency_key_required"));

    let err = client.post_charge("abc", "key-1").await.unwrap_err();
    assert_eq!(err.error.as_deref(), Some("invalid_amount"));
    assert_eq!(err.message, "A valid charge amount is required.");

    let err = client.get_charge_status("  ").await.unwrap_err();
    assert_eq!(err.error.as_deref(), Some("invalid_charge_id"));
    assert_eq!(err.to_string(), "A charge id is required.");

    let err = client.patch_auto_reload(true, "x", "5").await.unwrap_err();
    assert_eq!(err.error.as_deref(), Some("invalid_threshold"));

    let err = client.patch_auto_reload(true, "5", "0").await.unwrap_err();
    assert_eq!(err.error.as_deref(), Some("invalid_top_up_amount"));
    assert_eq!(err.message, "A valid top-up amount is required.");
}

#[tokio::test]
async fn simple_actions_return_usage_text() {
    if run_isolated("simple_actions_return_usage_text") {
        return;
    }

    let help = handle_billing_args(&["help".to_string()])
        .await
        .expect("help");
    assert_eq!(help, billing_usage());
    assert!(help.starts_with("Usage:\n  hermes billing"));

    let unknown = handle_billing_slash_args(&["bogus"])
        .await
        .expect("unknown");
    assert!(unknown.starts_with("Unknown billing action `bogus`.\n"));
    assert!(unknown.ends_with(billing_usage()));

    let charge = handle_billing_args(&["charge".to_string()])
        .await
        .expect("charge");
    assert_eq!(
        charge,
        "Usage: hermes billing charge <amount-usd> --confirm"
    );

    let status = handle_billing_args(&["charge-status".to_string()])
        .await
        .expect("charge status");
    assert_eq!(status, "Usage: hermes billing charge-status <charge-id>");

    let reload = handle_billing_slash_args(&["auto-reload", "20"])
        .await
        .expect("auto reload");
    assert_eq!(
        reload,
        "Usage: hermes billing auto-reload <threshold-usd> <top-up-usd> --confirm"
    );
}

#[test]
fn announce_charge_key_succeeds() {
    if run_isolated("announce_charge_key_succeeds") {
        return;
    }

    assert!(announce_charge_key("fixture-key").is_ok());
}

#[tokio::test]
async fn portal_and_unconfirmed_auto_reload_use_resolved_portal() {
    if run_isolated("portal_and_unconfirmed_auto_reload_use_resolved_portal") {
        return;
    }

    let _guard = test_env_lock::lock_async().await;
    let _env = store_env(" https://portal.test/ ");
    assert_eq!(resolve_portal_base_url(None), "https://portal.test");

    let portal = handle_billing_args(&["portal".to_string()])
        .await
        .expect("portal");
    assert_eq!(
        portal,
        "Nous billing portal\nhttps://portal.test/billing?topup=open"
    );

    let refused = handle_billing_slash_args(&["auto-reload", "20", "50"])
        .await
        .expect("refusal");
    assert_eq!(
        refused,
        "Refusing to change auto-reload without `--confirm`.\nManage on portal: https://portal.test/billing?topup=open"
    );
}

#[tokio::test]
async fn portal_base_from_fallback_url_uses_resolved_base_when_unusable() {
    if run_isolated("portal_base_from_fallback_url_uses_resolved_base_when_unusable") {
        return;
    }

    let _guard = test_env_lock::lock_async().await;
    let _portal = EnvVarGuard::set("HERMES_PORTAL_BASE_URL", "https://env.portal/");
    assert_eq!(
        portal_base_from_fallback_url("mailto:someone@example.com"),
        "https://env.portal"
    );
    assert_eq!(
        portal_base_from_fallback_url("not a url"),
        "https://env.portal"
    );
    assert_eq!(
        portal_base_from_fallback_url("http://host.example:8080/billing"),
        "http://host.example:8080"
    );
}

#[tokio::test]
async fn overview_renders_state_for_logged_in_member() {
    if run_isolated("overview_renders_state_for_logged_in_member") {
        return;
    }

    let _guard = test_env_lock::lock_async().await;
    let server = MockServer::start().await;
    let _env = store_env(&server.uri());
    save_nous_auth_state(&sample_state("access", server.uri())).expect("save state");
    Mock::given(method("GET"))
        .and(path("/api/billing/state"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "org": {"name": "Acme", "role": "MEMBER"},
            "balanceUsd": "5"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let out = handle_billing_args(&["overview".to_string()])
        .await
        .expect("overview");
    assert!(out.contains("Org: Acme"));
    assert!(out.contains("Role: MEMBER"));
    assert!(out.contains("Balance: $5"));
    assert!(out.contains("Billing changes require an org admin/owner."));
}

include!("billing_part2_coverage_gen.rs");
