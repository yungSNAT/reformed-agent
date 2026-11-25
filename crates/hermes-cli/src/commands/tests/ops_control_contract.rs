use std::collections::VecDeque;

use hermes_tools::tool_policy::{ToolPolicyEngine, ToolPolicyMode};
use serde_json::{json, Value};

use super::*;

#[tokio::test]
async fn policy_simulation_reports_decisions_without_dispatch() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::ops_control_contract::policy_simulation_reports_decisions_without_dispatch",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    let registry = Arc::new(hermes_tools::ToolRegistry::new());
    register_cli_noop_tool(&registry, "fixture_allowed");
    register_cli_noop_tool(&registry, "fixture_denied");
    registry.set_policy(
        ToolPolicyEngine::new(ToolPolicyMode::Enforce).with_denylist(&["fixture_denied"]),
    );
    app.tool_registry = Arc::clone(&registry);
    let counters = registry.policy_counters();

    for (args, expected) in [
        (&[][..], "usage: /simulate <tool_name> [json-params]"),
        (&["STATUS"][..], "counters: allow=0 deny=0"),
        (&["  "][..], "Usage: /simulate <tool_name>"),
        (&["fixture_allowed", "[]"][..], "must be a JSON object"),
        (
            &["fixture_allowed", "{"][..],
            "simulate params parse error:",
        ),
    ] {
        let output = command_output(&mut app, "/simulate", args).await;
        assert!(output.contains(expected), "{output}");
    }

    for (args, params, allowed, code) in [
        (&["fixture_allowed"][..], json!({}), true, Value::Null),
        (
            &[" fixture_denied ", r#"{"input":"two"#, r#"words"}"#][..],
            json!({"input":"two words"}),
            false,
            json!("tool_denylisted"),
        ),
    ] {
        let output = command_output(&mut app, "/simulate", args).await;
        let preview: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(preview["tool"], args[0].trim());
        assert_eq!(preview["params"], params);
        assert_eq!(preview["decision"]["mode"], "enforce");
        assert_eq!(preview["decision"]["allow"], allowed);
        assert_eq!(preview["decision"]["would_block"], !allowed);
        assert_eq!(preview["decision"]["code"], code);
    }
    assert_eq!(registry.policy_counters(), counters);
}

struct DashboardFixture {
    exchanges: Mutex<VecDeque<(Value, Value)>>,
}

#[async_trait::async_trait]
impl hermes_core::ToolHandler for DashboardFixture {
    async fn execute(&self, params: Value) -> Result<String, hermes_core::ToolError> {
        let (expected, response) = self.exchanges.lock().unwrap().pop_front().unwrap();
        assert_eq!(params, expected);
        Ok(response.to_string())
    }

    fn schema(&self) -> hermes_core::ToolSchema {
        hermes_core::tool_schema(
            "dashboard_control",
            "Fixture dashboard",
            hermes_core::JsonSchema::new("object"),
        )
    }
}

#[tokio::test]
async fn ops_dashboard_and_status_use_tool_results_and_latest_gate_reports() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::ops_control_contract::ops_dashboard_and_status_use_tool_results_and_latest_gate_reports",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let reports = home.join(".sync-reports");
    std::env::set_var("HERMES_REPO_ROOT", &home);
    std::fs::create_dir_all(&reports).unwrap();
    for (name, ok, date) in [
        ("eval-trend-gate-2026-01-01.json", false, "older"),
        ("eval-trend-gate-2026-01-02.json", true, "newer"),
        ("slo-auto-rollback-2026-01-02.json", false, "fixture"),
    ] {
        std::fs::write(
            reports.join(name),
            json!({"ok":ok,"generated_at":date}).to_string(),
        )
        .unwrap();
    }
    let url = "http://127.0.0.1:4321";
    let enabled = json!({"enabled":true,"url":url,"config_path":"fixture.yaml"});
    let disabled = json!({"enabled":false,"url":url,"config_path":"fixture.yaml"});
    let handler = Arc::new(DashboardFixture {
        exchanges: Mutex::new(VecDeque::from([
            (
                json!({"action":"on","host":"127.0.0.1","port":4321,"insecure":true}),
                enabled.clone(),
            ),
            (json!({"action":"url"}), enabled.clone()),
            (json!({"action":"status"}), enabled.clone()),
            (json!({"action":"off"}), disabled.clone()),
            (json!({"action":"status"}), disabled),
            (
                json!({"action":"on","host":"fixture.invalid"}),
                json!({"error":"fixture rejected host"}),
            ),
            (json!({"action":"status"}), enabled),
        ])),
    });
    let registry = Arc::new(hermes_tools::ToolRegistry::new());
    registry.set_policy(ToolPolicyEngine::new(ToolPolicyMode::Off));
    registry.register(
        "dashboard_control",
        "fixture",
        hermes_core::ToolHandler::schema(handler.as_ref()),
        handler.clone(),
        Arc::new(|| true),
        Vec::new(),
        true,
        "Fixture dashboard",
        "",
        None,
    );
    app.tool_registry = registry;

    for (args, expected) in [
        (
            &["dashboard", "ON", "127.0.0.1", "4321", "--INSECURE"][..],
            format!("Dashboard enabled at {url}\nConfig: fixture.yaml"),
        ),
        (&["dashboard", "url"][..], format!("{url}\nfixture.yaml")),
        (
            &["dashboard"][..],
            format!("dashboard: ON ({url})\nConfig: fixture.yaml"),
        ),
        (
            &["dashboard", "off"][..],
            format!("Dashboard disabled (URL: {url})\nConfig: fixture.yaml"),
        ),
        (
            &["dashboard", "status"][..],
            format!("dashboard: OFF ({url})\nConfig: fixture.yaml"),
        ),
        (
            &["dashboard", "on", "fixture.invalid", "65536"][..],
            "Dashboard command failed: fixture rejected host".into(),
        ),
    ] {
        assert_eq!(command_output(&mut app, "/ops", args).await, expected);
    }

    let status = command_output(&mut app, "/ops", &["status"]).await;
    for expected in [
        "Operator Control Plane",
        "dashboard: ON (http://127.0.0.1:4321)",
        "eval=pass @ newer (eval-trend-gate-2026-01-02.json)",
        "slo=fail @ fixture (slo-auto-rollback-2026-01-02.json)",
        "evolve=unknown; autopilot=unknown",
    ] {
        assert!(status.contains(expected), "{status}");
    }
    assert!(!status.contains("older"), "{status}");
    assert_eq!(
        command_output(&mut app, "/ops", &["gate", "status"]).await,
        "eval_trend=pass @ newer (eval-trend-gate-2026-01-02.json)\n\
         slo_rollback=fail @ fixture (slo-auto-rollback-2026-01-02.json)\n\
         elite_sync_gate=unknown"
    );
    assert!(handler.exchanges.lock().unwrap().is_empty());
}
