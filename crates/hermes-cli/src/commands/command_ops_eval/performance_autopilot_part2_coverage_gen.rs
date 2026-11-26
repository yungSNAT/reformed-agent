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
