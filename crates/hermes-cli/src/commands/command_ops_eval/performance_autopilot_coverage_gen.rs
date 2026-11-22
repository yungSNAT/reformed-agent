use super::*;

fn section(ok: bool, stdout: &str) -> serde_json::Value {
    serde_json::json!({
        "ok": ok,
        "exit_code": if ok { 0 } else { 1 },
        "stdout_tail": stdout,
        "stderr_tail": "",
    })
}

fn healthy_ctx_payload() -> serde_json::Value {
    serde_json::json!({
        "health": {"ok": true},
        "warnings": [],
        "context_pack": {
            "retrieval": {
                "route_owner_class": "rust",
                "source_counts": {"topic": 2},
                "fallback_counts": {"python_hot_path_total": 0}
            }
        },
        "status": {"queue": {"pendingTotal": 0}}
    })
}

fn degraded_ctx_payload() -> serde_json::Value {
    serde_json::json!({
        "health": {"ok": false},
        "warnings": ["w1"],
        "context_pack": {
            "retrieval": {
                "source_counts": {},
                "fallback_counts": {"python_hot_path_total": 3}
            }
        },
        "status": {"queue": {"pendingTotal": 20}}
    })
}

fn rec_ids(recs: &[serde_json::Value]) -> Vec<String> {
    recs.iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect()
}

fn action_keys(indexes: &serde_json::Value) -> Vec<String> {
    indexes
        .get("adaptive_actions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("key").and_then(|k| k.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn approx(value: &serde_json::Value, key: &str, expected: f64) {
    let got = value.get(key).and_then(|v| v.as_f64()).unwrap();
    assert!(
        (got - expected).abs() < 1e-9,
        "{key}: got {got}, expected {expected}"
    );
}

#[allow(unused_unsafe)]
fn set_env(key: &str, value: Option<&str>) {
    unsafe {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        set_env(key, Some(value));
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        set_env(self.key, self.prev.as_deref());
    }
}

#[tokio::test]
async fn probe_command_spawn_failure_reports_error() {
    if run_isolated("probe_command_spawn_failure_reports_error") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");
    let result = run_autopilot_probe_command_with_timeout(
        "echo hi",
        &missing,
        100,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert_eq!(result["command"], "echo hi");
    assert_eq!(result["exit_code"], -1);
    assert_eq!(result["ok"], false);
    assert_eq!(result["stdout_tail"], "");
    assert!(result["stderr_tail"]
        .as_str()
        .unwrap()
        .starts_with("spawn failed:"));
}

#[test]
fn native_section_from_report_ok_and_failure() {
    if run_isolated("native_section_from_report_ok_and_failure") {
        return;
    }

    let path = std::path::Path::new("/tmp/eval-report.json");
    let report = serde_json::json!({"ok": true, "generated_at": "2024-01-01T00:00:00Z"});
    let sec = autopilot_native_section_from_report("native gate", &report, path);
    assert_eq!(sec["command"], "native gate");
    assert_eq!(sec["exit_code"], 0);
    assert_eq!(sec["ok"], true);
    assert_eq!(sec["started_at"], "2024-01-01T00:00:00Z");
    assert_eq!(sec["finished_at"], "2024-01-01T00:00:00Z");
    assert_eq!(sec["duration_ms"], 0);
    assert_eq!(sec["stderr_tail"], "");
    let tail = sec["stdout_tail"].as_str().unwrap();
    assert!(tail.ends_with(&format!("report_path={}", path.display())));
    assert!(tail.contains("\"generated_at\""));

    let bad = serde_json::json!({"note": "x"});
    let sec = autopilot_native_section_from_report("g", &bad, path);
    assert_eq!(sec["exit_code"], 1);
    assert_eq!(sec["ok"], false);
    assert_eq!(sec["started_at"], "unknown");
    assert_eq!(sec["finished_at"], "unknown");
}

#[tokio::test]
async fn contextlattice_preflight_against_mock_servers() {
    if run_isolated("contextlattice_preflight_against_mock_servers") {
        return;
    }

    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Healthy server.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path("/health"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "telemetry": {"queueDepth": 3}})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::path("/memory/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "degraded": false,
            "retrieval_debug": {"route_owner_class": "rust", "source_counts": {"topic": 1}},
            "result_state": "hit"
        })))
        .mount(&server)
        .await;
    let uri = server.uri();
    {
        let _guard = EnvGuard::set("CONTEXTLATTICE_ORCHESTRATOR_URL", &format!("{uri}/"));
        assert_eq!(contextlattice_orchestrator_url(), uri);
        let sec = contextlattice_preflight_section().await;
        assert_eq!(
            sec["command"],
            format!("GET {uri}/health + POST {uri}/memory/search")
        );
        assert_eq!(sec["ok"], true);
        assert_eq!(sec["exit_code"], 0);
        assert_eq!(sec["stderr_tail"], "");
        let payload = parse_contextlattice_payload(&sec).unwrap();
        assert_eq!(payload["health"]["ok"], true);
        assert_eq!(payload["status"]["queue"]["pendingTotal"], 3);
        assert_eq!(payload["context_pack"]["result_state"], "hit");
        assert_eq!(payload["context_pack"]["degraded"], false);
        assert_eq!(payload["warnings"].as_array().unwrap().len(), 0);
        let (healthy, warnings, py, route, counts, queue) =
            contextlattice_autopilot_summary(Some(&payload));
        assert!(healthy);
        assert_eq!(warnings, 0);
        assert_eq!(py, 0);
        assert_eq!(route, "rust");
        assert_eq!(counts, serde_json::json!({"topic": 1}));
        assert_eq!(queue, 3);
    }

    // Server returning non-JSON bodies.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::path("/memory/search"))
        .respond_with(ResponseTemplate::new(200).set_body_string("still not json"))
        .mount(&server)
        .await;
    {
        let _guard = EnvGuard::set("CONTEXTLATTICE_ORCHESTRATOR_URL", &server.uri());
        let sec = contextlattice_preflight_section().await;
        assert_eq!(sec["ok"], false);
        assert_eq!(sec["exit_code"], 1);
        let payload = parse_contextlattice_payload(&sec).unwrap();
        let warnings = payload["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0]
            .as_str()
            .unwrap()
            .starts_with("health_json_parse_failed"));
        assert!(warnings[1]
            .as_str()
            .unwrap()
            .starts_with("search_json_parse_failed"));
    }

    // Server returning HTTP errors.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path("/health"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::path("/memory/search"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    {
        let _guard = EnvGuard::set("CONTEXTLATTICE_ORCHESTRATOR_URL", &server.uri());
        let sec = contextlattice_preflight_section().await;
        assert_eq!(sec["ok"], false);
        assert_eq!(sec["exit_code"], 1);
        let payload = parse_contextlattice_payload(&sec).unwrap();
        let warnings = payload["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0]
            .as_str()
            .unwrap()
            .starts_with("health_request_failed"));
        assert!(warnings[1]
            .as_str()
            .unwrap()
            .starts_with("search_request_failed"));
        assert_eq!(payload["status"]["queue"]["pendingTotal"], 0);
    }
}

#[test]
fn parse_contextlattice_payload_edge_cases() {
    if run_isolated("parse_contextlattice_payload_edge_cases") {
        return;
    }

    assert!(parse_contextlattice_payload(&serde_json::json!({})).is_none());
    assert!(parse_contextlattice_payload(&serde_json::json!({"stdout_tail": "   "})).is_none());
    assert!(parse_contextlattice_payload(&serde_json::json!({"stdout_tail": "} x {"})).is_none());
    let parsed = parse_contextlattice_payload(
        &serde_json::json!({"stdout_tail": "prefix {\"a\":1} suffix"}),
    )
    .unwrap();
    assert_eq!(parsed, serde_json::json!({"a": 1}));
}

#[test]
fn contextlattice_summary_none_defaults() {
    if run_isolated("contextlattice_summary_none_defaults") {
        return;
    }

    let (healthy, warnings, py, route, counts, queue) = contextlattice_autopilot_summary(None);
    assert!(!healthy);
    assert_eq!(warnings, 0);
    assert_eq!(py, 0);
    assert!(route.is_empty());
    assert_eq!(counts, serde_json::json!({}));
    assert_eq!(queue, 0);
}

#[test]
fn recommendations_all_failing() {
    if run_isolated("recommendations_all_failing") {
        return;
    }

    let recs = build_performance_autopilot_recommendations(
        &section(false, ""),
        &section(false, ""),
        &section(false, ""),
        &section(false, ""),
    );
    assert_eq!(
        rec_ids(&recs),
        vec![
            "HOTPATH_FAIL",
            "EVAL_TREND_FAIL",
            "MCP_STALE_RECOVERY_FAIL",
            "CONTEXTLATTICE_PREFLIGHT_FAIL",
            "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE",
        ]
    );
    assert_eq!(recs[0]["severity"], "P0");
    assert_eq!(recs[2]["severity"], "P1");
    assert_eq!(recs[3]["severity"], "P0");
}

#[test]
fn recommendations_slow_and_degraded_context() {
    if run_isolated("recommendations_slow_and_degraded_context") {
        return;
    }

    let recs = build_performance_autopilot_recommendations(
        &section(true, "bench\ntool_policy_hot_path_ns_per_eval=20000\n"),
        &section(true, ""),
        &section(true, ""),
        &section(true, &degraded_ctx_payload().to_string()),
    );
    assert_eq!(
        rec_ids(&recs),
        vec![
            "HOTPATH_SLOW",
            "CONTEXTLATTICE_UNHEALTHY",
            "CONTEXTLATTICE_PYTHON_FALLBACK",
            "CONTEXTLATTICE_QUEUE_PRESSURE",
        ]
    );
    assert_eq!(recs[3]["severity"], "P2");
}

#[test]
fn recommendations_stable() {
    if run_isolated("recommendations_stable") {
        return;
    }

    let recs = build_performance_autopilot_recommendations(
        &section(true, "tool_policy_hot_path_ns_per_eval=1000"),
        &section(true, ""),
        &section(true, ""),
        &section(true, &healthy_ctx_payload().to_string()),
    );
    assert_eq!(rec_ids(&recs), vec!["PERF_STABLE"]);
    assert_eq!(recs[0]["severity"], "P3");
}

#[test]
fn indexes_balanced_profile() {
    if run_isolated("indexes_balanced_profile") {
        return;
    }

    let hot = section(true, "tool_policy_hot_path_ns_per_eval=1000");
    let ok = section(true, "");
    let ctx = section(true, &healthy_ctx_payload().to_string());
    let recs = build_performance_autopilot_recommendations(&hot, &ok, &ok, &ctx);
    let idx = compute_performance_autopilot_indexes(&hot, &ok, &ok, &ctx, &recs);
    approx(&idx, "performance_index", 100.0);
    approx(&idx, "intelligence_index", 98.0);
    approx(&idx, "adaptive_index", 99.1);
    assert_eq!(idx["profile_recommendation"], "balanced");
    assert_eq!(
        action_keys(&idx),
        vec![
            "HERMES_PERF_AUTOPILOT_PROFILE",
            "HERMES_PERF_AUTOPILOT_STATUS"
        ]
    );
}

#[test]
fn indexes_quality_profile_with_p2_severity() {
    if run_isolated("indexes_quality_profile_with_p2_severity") {
        return;
    }

    let ok = section(true, "");
    let ctx = section(true, &healthy_ctx_payload().to_string());
    let recs = vec![serde_json::json!({"id": "X", "severity": "p2"})];
    let idx = compute_performance_autopilot_indexes(&ok, &section(false, ""), &ok, &ctx, &recs);
    approx(&idx, "performance_index", 100.0);
    approx(&idx, "intelligence_index", 76.0);
    assert_eq!(idx["profile_recommendation"], "quality");
    assert_eq!(
        action_keys(&idx),
        vec![
            "HERMES_PERF_AUTOPILOT_PROFILE",
            "HERMES_REPLAY_ENABLED",
            "HERMES_MODEL_AUTO_REMEDIATE"
        ]
    );
}

#[test]
fn indexes_reliability_profile() {
    if run_isolated("indexes_reliability_profile") {
        return;
    }

    let ok = section(true, "");
    let ctx = section(true, &healthy_ctx_payload().to_string());
    let idx = compute_performance_autopilot_indexes(&ok, &ok, &section(false, ""), &ctx, &[]);
    approx(&idx, "performance_index", 80.0);
    approx(&idx, "intelligence_index", 100.0);
    assert_eq!(idx["profile_recommendation"], "reliability");
    assert_eq!(
        action_keys(&idx),
        vec!["HERMES_PERF_AUTOPILOT_PROFILE", "HERMES_TOOL_POLICY_MODE"]
    );
}

#[test]
fn indexes_safety_profile() {
    if run_isolated("indexes_safety_profile") {
        return;
    }

    let ok = section(true, "");
    let fail = section(false, "");
    let idx = compute_performance_autopilot_indexes(&fail, &ok, &ok, &fail, &[]);
    approx(&idx, "performance_index", 40.0);
    approx(&idx, "intelligence_index", 70.0);
    assert_eq!(idx["profile_recommendation"], "safety");
    assert_eq!(
        action_keys(&idx),
        vec![
            "HERMES_PERF_AUTOPILOT_PROFILE",
            "HERMES_TOOL_POLICY_MODE",
            "HERMES_REPLAY_ENABLED"
        ]
    );
}

#[test]
fn indexes_fallback_and_queue_pressure_penalties() {
    if run_isolated("indexes_fallback_and_queue_pressure_penalties") {
        return;
    }

    let ok = section(true, "");
    let ctx = section(true, &degraded_ctx_payload().to_string());
    let idx = compute_performance_autopilot_indexes(&ok, &ok, &ok, &ctx, &[]);
    approx(&idx, "performance_index", 100.0);
    approx(&idx, "intelligence_index", 89.0);
    assert_eq!(idx["profile_recommendation"], "balanced");
}

#[test]
fn default_paths_live_under_sync_reports() {
    if run_isolated("default_paths_live_under_sync_reports") {
        return;
    }

    let root = std::path::Path::new("/repo/root");
    let (json, md) = default_performance_autopilot_paths(root);
    assert_eq!(json.parent().unwrap(), root.join(".sync-reports"));
    assert_eq!(md.parent().unwrap(), root.join(".sync-reports"));
    let json_name = json.file_name().unwrap().to_str().unwrap().to_string();
    let md_name = md.file_name().unwrap().to_str().unwrap().to_string();
    assert!(json_name.starts_with("performance-autopilot-"));
    assert!(json_name.ends_with(".json"));
    assert!(md_name.ends_with(".md"));
    assert_eq!(
        json_name.trim_end_matches(".json"),
        md_name.trim_end_matches(".md")
    );
}

#[test]
fn markdown_report_full_and_defaults() {
    if run_isolated("markdown_report_full_and_defaults") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("report.md");
    let report = serde_json::json!({
        "generated_at": "2024-01-01T00:00:00Z",
        "ok": true,
        "intelligence_index": 87.5,
        "performance_index": 90.0,
        "adaptive_index": 88.875,
        "profile_recommendation": "quality",
        "sections": {
            "hotpath": {"ok": true, "exit_code": 0},
            "eval": {"ok": false, "exit_code": 1}
        },
        "recommendations": [
            {"id": "HOTPATH_SLOW", "severity": "P1", "title": "Slow", "recommendation": "Fix it"}
        ],
        "adaptive_actions": [
            {"key": "K", "value": "V", "reason": "because"}
        ]
    });
    write_performance_autopilot_markdown(&path, &report).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# Performance Autopilot Report\n"));
    assert!(text.ends_with('\n'));
    assert!(text.contains("- generated_at: `2024-01-01T00:00:00Z`"));
    assert!(text.contains("- ok: `true`"));
    assert!(text.contains("- intelligence_index: `87.50`"));
    assert!(text.contains("- performance_index: `90.00`"));
    assert!(
        text.contains("- adaptive_index: `88.88`") || text.contains("- adaptive_index: `88.87`")
    );
    assert!(text.contains("- profile_recommendation: `quality`"));
    assert!(text.contains("- `hotpath`: PASS (exit=0)"));
    assert!(text.contains("- `eval`: FAIL (exit=1)"));
    assert!(text.contains("- **HOTPATH_SLOW (P1)**: Slow - Fix it"));
    assert!(text.contains("## Adaptive Actions"));
    assert!(text.contains("- `K=V` (because)"));

    let path2 = dir.path().join("minimal.md");
    write_performance_autopilot_markdown(&path2, &serde_json::json!({"adaptive_actions": []}))
        .unwrap();
    let text = std::fs::read_to_string(&path2).unwrap();
    assert!(text.contains("- generated_at: `unknown`"));
    assert!(text.contains("- ok: `false`"));
    assert!(text.contains("- profile_recommendation: `balanced`"));
    assert!(text.contains("## Sections"));
    assert!(text.contains("## Recommendations"));
    assert!(!text.contains("## Adaptive Actions"));
}

#[test]
fn markdown_write_to_directory_fails() {
    if run_isolated("markdown_write_to_directory_fails") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let err = write_performance_autopilot_markdown(dir.path(), &serde_json::json!({})).unwrap_err();
    assert!(matches!(err, AgentError::Io(ref msg) if msg.starts_with("write ")));
}

#[test]
fn env_file_with_eval_and_context_recs() {
    if run_isolated("env_file_with_eval_and_context_recs") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("perf.env");
    let report = serde_json::json!({
        "generated_at": "T1",
        "profile_recommendation": "quality",
        "recommendations": [
            {"id": "EVAL_TREND_FAIL"},
            {"id": "CONTEXTLATTICE_UNHEALTHY"}
        ],
        "adaptive_actions": [
            {"key": "A", "value": "1"},
            {"value": "missing-key"},
            {"key": "B"}
        ]
    });
    write_performance_autopilot_env(&path, &report).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        "# generated_at=T1\n\
HERMES_MODEL_AUTO_REMEDIATE=1\n\
HERMES_REPLAY_ENABLED=1\n\
HERMES_CONTEXTLATTICE_MODE=max\n\
HERMES_CONTEXTLATTICE_RETRIEVAL_MODE=deep\n\
HERMES_CONTEXTLATTICE_REQUIRE_READBACK=1\n\
HERMES_PERF_AUTOPILOT_PROFILE=quality\n\
A=1\n"
    );
}

#[test]
fn env_file_stable_defaults() {
    if run_isolated("env_file_stable_defaults") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stable.env");
    let report = serde_json::json!({
        "recommendations": [{"id": "PERF_STABLE"}]
    });
    write_performance_autopilot_env(&path, &report).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        "# generated_at=unknown\nHERMES_PERF_AUTOPILOT_STATUS=stable\nHERMES_PERF_AUTOPILOT_PROFILE=balanced\n"
    );
}

#[test]
fn env_write_to_directory_fails() {
    if run_isolated("env_write_to_directory_fails") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let err = write_performance_autopilot_env(dir.path(), &serde_json::json!({})).unwrap_err();
    assert!(matches!(err, AgentError::Io(ref msg) if msg.starts_with("write ")));
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path.split_once("::").map_or(path.as_str(), |(_, rest)| rest);
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
    true
}
