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

#[tokio::test]
async fn ops_eval_runs_against_saved_sessions_and_displays_the_saved_report() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::ops_control_contract::ops_eval_runs_against_saved_sessions_and_displays_the_saved_report",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    std::env::set_var("HERMES_REPO_ROOT", &home);
    let mut app = build_test_app_with_stream(&home).await;
    let status = command_output(&mut app, "/ops", &["eval"]).await;
    assert!(status.starts_with("No eval reports found yet."), "{status}");
    assert_eq!(
        command_output(&mut app, "/ops", &["eval", "latest"]).await,
        "No eval reports found."
    );

    let sessions = home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("fixture.json"),
        json!({"messages":[
            {"role":"user","content":"Please inspect /objective status."},
            {"role":"assistant","content":"Verified the objective."},
        ]})
        .to_string(),
    )
    .unwrap();
    let output = command_output(&mut app, "/ops", &["eval", "run"]).await;
    let (payload, path) = output.split_once("\nreport_path=").unwrap();
    let path = Path::new(path);
    assert_eq!(path.parent(), Some(home.join("eval-reports").as_path()));
    let filename = path.file_name().unwrap().to_str().unwrap();
    assert!(filename.starts_with("session-eval-harness-"));
    let report: Value = serde_json::from_str(payload).unwrap();
    let persisted = std::fs::read_to_string(path).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&persisted).unwrap(), report);
    assert_eq!(report["summary"]["sessions_analyzed"], 1);
    assert_eq!(report["summary"]["user_turns"], 1);
    assert_eq!(report["summary"]["assistant_turns"], 1);
    assert_eq!(
        command_output(&mut app, "/ops", &["eval", "latest"]).await,
        format!("Latest eval report: {filename}\n{persisted}")
    );
    let status = command_output(&mut app, "/ops", &["eval", "status"]).await;
    assert!(status.contains(&format!("({filename})")), "{status}");
}

#[tokio::test]
async fn ops_recommendations_and_autopilot_overrides_follow_local_state() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::ops_control_contract::ops_recommendations_and_autopilot_overrides_follow_local_state",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    std::env::set_var("HERMES_REPO_ROOT", &home);
    let mut app = build_test_app_with_stream(&home).await;
    let reports = home.join(".sync-reports");
    std::fs::create_dir_all(&reports).unwrap();
    for (command, stem, label, report, expected) in [
        (
            "evolve",
            "self-evolution-loop",
            "Self-evolution",
            json!({"ok":false,"summary":{"intelligence_index":75.0},"recommendations":[
                {"id":"CHECK_EVAL","severity":"P1","title":"Check evaluation","command":"/ops eval run"}
            ]}),
            "[P1] CHECK_EVAL: Check evaluation\n  cmd: /ops eval run",
        ),
        (
            "autopilot",
            "performance-autopilot",
            "Autopilot",
            json!({"ok":true,"adaptive_index":90.0,"profile_recommendation":"reliability","recommendations":[
                {"id":"CHECK_LATENCY","severity":"P2","title":"Check latency","recommendation":"Inspect slow routes"}
            ]}),
            "[P2] CHECK_LATENCY: Check latency\n  recommendation: Inspect slow routes",
        ),
    ] {
        let output = command_output(&mut app, "/ops", &[command, "recommend"]).await;
        assert!(
            output.starts_with("No ") && output.contains("reports found."),
            "{output}"
        );
        let old = format!("{stem}-2026-01-01.json");
        let latest = format!("{stem}-2026-01-02.json");
        std::fs::write(reports.join(old), json!({"recommendations":[]}).to_string()).unwrap();
        std::fs::write(reports.join(&latest), report.to_string()).unwrap();
        assert_eq!(
            command_output(&mut app, "/ops", &[command, "recs"]).await,
            format!("{label} recommendations ({latest}):\n{expected}")
        );
        let status = command_output(&mut app, "/ops", &[command, "status"]).await;
        assert!(
            status.contains("recs=1") && status.contains(&latest),
            "{status}"
        );
        std::fs::write(
            reports.join(&latest),
            json!({"recommendations":[]}).to_string(),
        )
        .unwrap();
        assert_eq!(
            command_output(&mut app, "/ops", &[command, "recommend"]).await,
            format!("No recommendations found in {latest}.")
        );
    }

    for (setting, value, default, variable) in [
        (
            "profile",
            "reliability",
            "off",
            "HERMES_PERF_AUTOPILOT_PROFILE",
        ),
        ("mode", "off", "advisory", "HERMES_PERF_AUTOPILOT_MODE"),
    ] {
        let setting_args = ["autopilot", setting, value];
        let status_args = ["autopilot", setting, "status"];
        let status = command_output(&mut app, "/ops", &status_args).await;
        assert!(status.contains(&format!("{setting}={default}")), "{status}");
        let list = command_output(&mut app, "/ops", &["autopilot", setting, "list"]).await;
        assert!(list.contains(&format!("- {value}:")), "{list}");
        assert_eq!(
            command_output(&mut app, "/ops", &setting_args).await,
            format!("autopilot {setting} set to '{value}'")
        );
        let invalid = command_output(&mut app, "/ops", &["autopilot", setting, "invalid"]).await;
        assert!(
            invalid.starts_with(&format!("Unknown {setting} 'invalid'.")),
            "{invalid}"
        );
        assert_eq!(std::env::var(variable).unwrap(), value);
    }
    let status = command_output(&mut app, "/ops", &["autopilot", "status"]).await;
    assert!(status.contains("mode=off profile=reliability"), "{status}");
    std::env::set_var("HERMES_PERF_AUTOPILOT_STATUS", "fixture");
    let cleared = command_output(&mut app, "/ops", &["autopilot", "clear"]).await;
    assert!(
        cleared.starts_with("Cleared autopilot runtime overrides"),
        "{cleared}"
    );
    for variable in [
        "HERMES_PERF_AUTOPILOT_MODE",
        "HERMES_PERF_AUTOPILOT_PROFILE",
        "HERMES_PERF_AUTOPILOT_STATUS",
    ] {
        assert!(std::env::var_os(variable).is_none(), "{variable}");
    }
    let status = command_output(&mut app, "/ops", &["autopilot", "status"]).await;
    assert!(status.contains("mode=advisory profile=off"), "{status}");
}
