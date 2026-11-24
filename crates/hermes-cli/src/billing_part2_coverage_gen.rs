#[tokio::test]
async fn limit_falls_back_to_logged_out_on_auth_error() {
    if run_isolated("limit_falls_back_to_logged_out_on_auth_error") {
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
    if run_isolated("status_fails_open_on_server_error") {
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
    if run_isolated("confirmed_charge_without_key_generates_uuid_key") {
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
    if run_isolated("non_charge_redirects_are_followed") {
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
    if run_isolated("charge_status_keeps_null_amount") {
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
    if run_isolated("render_admin_with_terminal_billing_off") {
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
    if run_isolated("render_owner_who_can_charge_without_card") {
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
    if run_isolated("render_without_role_requires_admin") {
        return;
    }

    let state = logged_in_state();
    let out = render_billing_state(&state);
    assert!(out.contains("Balance: -"));
    assert!(out.contains("Billing changes require an org admin/owner."));
}

#[test]
fn render_billing_limit_variants() {
    if run_isolated("render_billing_limit_variants") {
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
    if run_isolated("usage_bar_handles_missing_and_invalid_values") {
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
    if run_isolated("money_helpers_reject_invalid_input_and_format_negatives") {
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
    if run_isolated("absolutize_portal_url_variants") {
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
    if run_isolated("billing_scope_detection_handles_malformed_tokens_and_claims") {
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

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path
        .split_once("::")
        .map_or(path.as_str(), |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary")) // nosemgrep: rust.lang.security.current-exe.current-exe
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
    true
}
