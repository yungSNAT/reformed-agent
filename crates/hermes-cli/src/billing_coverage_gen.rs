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
        access_token: access_token.to_string(),
        refresh_token: Some("refresh".to_string()),
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
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::fail_open_state_records_error_and_portal");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
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
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::from_response_maps_additional_error_codes"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
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
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::auth_error_maps_to_login_hint");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let rendered = billing_error_to_agent_error(BillingError::auth("Login needed.")).to_string();
    assert!(rendered.contains("Login needed. (401)"));
    assert!(rendered.contains("Run `hermes portal` to log in."));
}

#[test]
fn from_state_rejects_blank_access_token() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::from_state_rejects_blank_access_token");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
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
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::client_exposes_portal_base_url");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let client = offline_client();
    assert_eq!(client.portal_base_url(), "https://portal.example");
}

#[tokio::test]
async fn client_validates_inputs_before_requests() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::client_validates_inputs_before_requests");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
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
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::simple_actions_return_usage_text");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
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
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::announce_charge_key_succeeds");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    assert!(announce_charge_key("fixture-key").is_ok());
}

#[tokio::test]
async fn portal_and_unconfirmed_auto_reload_use_resolved_portal() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::portal_and_unconfirmed_auto_reload_use_resolved_portal"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
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
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::portal_base_from_fallback_url_uses_resolved_base_when_unusable"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
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
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::overview_renders_state_for_logged_in_member"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
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

#[tokio::test]
async fn limit_falls_back_to_logged_out_on_auth_error() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::limit_falls_back_to_logged_out_on_auth_error"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _guard = test_env_lock::lock_async().await;
    let server = MockServer::start().await;
    let _env = store_env(&server.uri());
    save_nous_auth_state(&sample_state("access", server.uri())).expect("save state");
    Mock::given(method("GET"))
        .and(path("/api/billing/state"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "invalid_token"})))
        .expect(1)
        .mount(&server)
        .await;
    let out = handle_billing_args(&["limit".to_string()])
        .await
        .expect("limit");
    assert!(out.contains("Not logged into Nous Portal"));
    assert!(!out.contains("Last error"));
    assert!(out.contains(&format!(
        "Manage on portal: {}/billing?topup=open",
        server.uri()
    )));
}

#[tokio::test]
async fn status_fails_open_on_server_error() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::status_fails_open_on_server_error");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _guard = test_env_lock::lock_async().await;
    let server = MockServer::start().await;
    let _env = store_env(&server.uri());
    save_nous_auth_state(&sample_state("access", server.uri())).expect("save state");
    Mock::given(method("GET"))
        .and(path("/api/billing/state"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": "boom"})))
        .expect(1)
        .mount(&server)
        .await;
    let out = handle_billing_args(&["status".to_string()])
        .await
        .expect("status");
    assert!(out.contains("Last error: Billing request failed. (500)"));
    assert!(out.contains(&format!(
        "Manage on portal: {}/billing?topup=open",
        server.uri()
    )));
}

#[tokio::test]
async fn confirmed_charge_without_key_generates_uuid_key() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::confirmed_charge_without_key_generates_uuid_key"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _guard = test_env_lock::lock_async().await;
    let server = MockServer::start().await;
    let _env = store_env(&server.uri());
    save_nous_auth_state(&sample_state("access", server.uri())).expect("save state");
    Mock::given(method("POST"))
        .and(path("/api/billing/charge"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({"chargeId": "ch-auto"})))
        .expect(1)
        .mount(&server)
        .await;
    let out = handle_billing_args(&[
        "charge".to_string(),
        "50".to_string(),
        "--confirm".to_string(),
    ])
    .await
    .expect("charge");
    assert!(out.contains("charge_id: ch-auto"));
    let key = out
        .lines()
        .find_map(|line| line.strip_prefix("  idempotency_key: "))
        .expect("key line");
    assert!(Uuid::parse_str(key).is_ok());
}

#[tokio::test]
async fn non_charge_redirects_are_followed() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::non_charge_redirects_are_followed");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _guard = test_env_lock::lock_async().await;
    let server = MockServer::start().await;
    let _env = store_env(&server.uri());
    Mock::given(method("GET"))
        .and(path("/api/billing/charge/redir"))
        .respond_with(ResponseTemplate::new(307).insert_header(
            "Location",
            format!("{}/api/billing/charge/final", server.uri()).as_str(),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/billing/charge/final"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "pending"})))
        .expect(1)
        .mount(&server)
        .await;
    let client = match NousBillingClient::from_state(&sample_state("access", server.uri())) {
        Ok(client) => client,
        Err(err) => panic!("client: {err}"),
    };
    let payload = client.get_charge_status("redir").await.expect("status");
    assert_eq!(payload, json!({"status": "pending"}));
}

#[test]
fn charge_status_keeps_null_amount() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::charge_status_keeps_null_amount");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let output =
        render_charge_status(&json!({"status": "pending", "amountUsd": null})).expect("status");
    let rendered: Value = serde_json::from_str(
        output
            .strip_prefix("Billing charge status\n")
            .expect("heading"),
    )
    .expect("json");
    assert_eq!(rendered, json!({"status": "pending", "amountUsd": null}));
}

fn logged_in_state() -> BillingState {
    let mut state = BillingState::logged_out(Some("https://p/billing".to_string()));
    state.logged_in = true;
    state
}

#[test]
fn render_admin_with_terminal_billing_off() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::render_admin_with_terminal_billing_off");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let mut state = logged_in_state();
    state.role = Some("ADMIN".to_string());
    state.charge_presets = vec!["5".to_string()];
    state.monthly_cap = Some(MonthlyCap {
        limit_usd: Some("10".to_string()),
        spent_this_month_usd: Some("1".to_string()),
        is_default_ceiling: false,
    });
    state.auto_reload = Some(AutoReload {
        enabled: false,
        threshold_usd: None,
        reload_to_usd: None,
    });
    let out = render_billing_state(&state);
    assert!(out.contains("Terminal billing: off"));
    assert!(out.contains("Charge presets: $5"));
    assert!(out.contains("Monthly spend: $1 of $10 used [#---------] 10%"));
    assert!(!out.contains("(default ceiling)"));
    assert!(out.contains("Auto-reload: off"));
    assert!(!out.contains("->"));
    assert!(!out.contains("Saved card"));
    assert!(out.contains("Terminal billing is turned off for this org."));
    assert!(out.contains("Manage on portal: https://p/billing"));
}

#[test]
fn render_owner_who_can_charge_without_card() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::render_owner_who_can_charge_without_card");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let mut state = logged_in_state();
    state.role = Some("owner".to_string());
    state.cli_billing_enabled = true;
    let out = render_billing_state(&state);
    assert!(out.contains("Terminal billing: enabled"));
    assert!(out.contains("Saved card: none. Set up a saved card on the portal before charging."));
    assert!(out.contains("Write actions require explicit confirmation"));
}

#[test]
fn render_without_role_requires_admin() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::render_without_role_requires_admin");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let state = logged_in_state();
    let out = render_billing_state(&state);
    assert!(out.contains("Balance: -"));
    assert!(out.contains("Billing changes require an org admin/owner."));
}

#[test]
fn render_billing_limit_variants() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::render_billing_limit_variants");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let logged_out = BillingState::logged_out(Some("https://p/billing".to_string()));
    assert_eq!(
        render_billing_limit(&logged_out),
        render_billing_state(&logged_out)
    );

    let state = logged_in_state();
    assert_eq!(
        render_billing_limit(&state),
        "Monthly spend limit\nNo monthly cap data available.\nLimit changes are read-only from the terminal; manage limits on the portal.\nManage on portal: https://p/billing"
    );

    let mut capped = logged_in_state();
    capped.portal_url = None;
    capped.monthly_cap = Some(MonthlyCap {
        limit_usd: Some("100".to_string()),
        spent_this_month_usd: Some("50".to_string()),
        is_default_ceiling: true,
    });
    let out = render_billing_limit(&capped);
    assert!(out.contains("Monthly spend: $50 of $100 used (default ceiling) [#####-----] 50%"));
    assert!(!out.contains("Manage on portal"));
}

#[test]
fn usage_bar_handles_missing_and_invalid_values() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::usage_bar_handles_missing_and_invalid_values"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    assert_eq!(billing_usage_bar(None, Some("1")), "");
    assert_eq!(billing_usage_bar(Some("x"), Some("1")), "");
    assert_eq!(billing_usage_bar(Some("1"), Some("x")), "");
    assert_eq!(billing_usage_bar(Some("1"), Some("0")), "");
    assert_eq!(
        billing_usage_bar(Some("5"), Some("10")),
        " [#####-----] 50%"
    );
}

#[test]
fn money_helpers_reject_invalid_input_and_format_negatives() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::money_helpers_reject_invalid_input_and_format_negatives"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    assert!(parse_money_str("   ").is_none());
    assert!(parse_money_str("1.").is_none());
    assert!(parse_money_str("1.a").is_none());
    assert_eq!(format_money("abc"), "-");
    assert!(parse_amount_number("0").is_none());
    assert!(parse_amount_number("-5").is_none());
    assert_eq!(parse_amount_number("2.5"), Some(2.5));
    assert_eq!(format_money("-5.5"), "-$5.50");
}

#[test]
fn absolutize_portal_url_variants() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::absolutize_portal_url_variants");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    assert!(absolutize_portal_url_with_base("   ", "https://b").is_none());
    assert_eq!(
        absolutize_portal_url_with_base("https://x/y", "https://b").as_deref(),
        Some("https://x/y")
    );
    assert_eq!(
        absolutize_portal_url_with_base("billing", "https://b/").as_deref(),
        Some("https://b/billing")
    );
}

#[test]
fn billing_scope_detection_handles_malformed_tokens_and_claims() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(
            module_path!(),
            "::billing_scope_detection_handles_malformed_tokens_and_claims"
        );
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    assert!(!nous_token_has_billing_scope(None, None));
    assert!(!nous_token_has_billing_scope(None, Some("nodots")));
    assert!(!nous_token_has_billing_scope(None, Some("a.!!!.c")));
    let not_json = format!("a.{}.c", URL_SAFE_NO_PAD.encode(b"not json"));
    assert!(!nous_token_has_billing_scope(None, Some(&not_json)));
    assert!(nous_token_has_billing_scope(
        None,
        Some(&encoded_token(&json!({"scp": [BILLING_MANAGE_SCOPE]})))
    ));
    assert!(nous_token_has_billing_scope(
        None,
        Some(&encoded_token(&json!({"scope": "openid billing:manage"})))
    ));
    assert!(!nous_token_has_billing_scope(
        None,
        Some(&encoded_token(&json!({"scope": 5})))
    ));
}
