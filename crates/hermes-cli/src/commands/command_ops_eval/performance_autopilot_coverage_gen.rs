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

fn set_env(key: &str, value: Option<&str>) {
    match value {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
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

include!("performance_autopilot_part2_coverage_gen.rs");
