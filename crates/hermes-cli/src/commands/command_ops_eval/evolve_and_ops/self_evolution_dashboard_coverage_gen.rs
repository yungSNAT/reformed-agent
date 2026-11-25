use super::*;

#[test]
fn self_evolution_recommendations_missing_file_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("does-not-exist.json");
    let recs = self_evolution_recommendations(&missing);
    assert!(recs.is_empty());
}

#[test]
fn self_evolution_recommendations_without_array_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("report.json");
    std::fs::write(&path, r#"{"ok":true,"recommendations":"not-an-array"}"#).expect("write");
    let recs = self_evolution_recommendations(&path);
    assert!(recs.is_empty());

    let path2 = dir.path().join("report2.json");
    std::fs::write(&path2, r#"{"ok":false}"#).expect("write");
    assert!(self_evolution_recommendations(&path2).is_empty());
}

#[test]
fn performance_autopilot_recommendations_missing_file_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("nope.json");
    let recs = performance_autopilot_recommendations(&missing);
    assert!(recs.is_empty());
}

#[test]
fn performance_autopilot_recommendations_without_array_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("perf.json");
    std::fs::write(&path, r#"{"ok":true,"adaptive_index":42.0}"#).expect("write");
    let recs = performance_autopilot_recommendations(&path);
    assert!(recs.is_empty());

    let path2 = dir.path().join("perf2.json");
    std::fs::write(&path2, r#"{"recommendations":{"id":"X"}}"#).expect("write");
    assert!(performance_autopilot_recommendations(&path2).is_empty());
}

#[test]
fn parse_env_file_kv_missing_file_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.env");
    let kv = parse_env_file_kv(&missing);
    assert!(kv.is_empty());
}

#[test]
fn write_autopilot_runtime_event_appends_json_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let applied = vec![
        ("HERMES_PERF_AUTOPILOT_MODE".to_string(), "auto".to_string()),
        (
            "HERMES_PERF_AUTOPILOT_PROFILE".to_string(),
            "fast".to_string(),
        ),
    ];
    write_autopilot_runtime_event(dir.path(), "sess-1", "auto", "fast", &applied);
    write_autopilot_runtime_event(dir.path(), "sess-2", "manual", "balanced", &[]);

    let file = dir.path().join("performance-autopilot-runtime.jsonl");
    let raw = std::fs::read_to_string(&file).expect("read jsonl");
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2);

    let first: serde_json::Value = serde_json::from_str(lines[0]).expect("json line 1");
    assert_eq!(first["session_id"], "sess-1");
    assert_eq!(first["mode"], "auto");
    assert_eq!(first["profile"], "fast");
    assert!(first["created_at"].as_str().is_some_and(|s| !s.is_empty()));
    let applied_json = first["applied"].as_array().expect("applied array");
    assert_eq!(applied_json.len(), 2);
    assert_eq!(applied_json[0][0], "HERMES_PERF_AUTOPILOT_MODE");
    assert_eq!(applied_json[0][1], "auto");
    assert_eq!(applied_json[1][1], "fast");

    let second: serde_json::Value = serde_json::from_str(lines[1]).expect("json line 2");
    assert_eq!(second["session_id"], "sess-2");
    assert_eq!(second["mode"], "manual");
    assert_eq!(second["profile"], "balanced");
    assert_eq!(second["applied"].as_array().map(|a| a.len()), Some(0));
}

#[test]
fn write_autopilot_runtime_event_missing_dir_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing_dir = dir.path().join("no-such-subdir");
    write_autopilot_runtime_event(&missing_dir, "s", "m", "p", &[]);
    assert!(!missing_dir
        .join("performance-autopilot-runtime.jsonl")
        .exists());
}

#[test]
fn dashboard_status_line_enabled_with_url() {
    let payload = serde_json::json!({"enabled": true, "url": "http://127.0.0.1:9999"});
    assert_eq!(
        dashboard_status_line_from_payload(&payload),
        "dashboard: ON (http://127.0.0.1:9999)"
    );
}

#[test]
fn dashboard_status_line_defaults_when_missing() {
    let payload = serde_json::json!({});
    assert_eq!(
        dashboard_status_line_from_payload(&payload),
        "dashboard: OFF (n/a)"
    );
}

#[test]
fn dashboard_status_line_disabled_and_wrong_types() {
    let payload = serde_json::json!({"enabled": "yes", "url": 5});
    assert_eq!(
        dashboard_status_line_from_payload(&payload),
        "dashboard: OFF (n/a)"
    );
    let payload2 = serde_json::json!({"enabled": false, "url": "http://x"});
    assert_eq!(
        dashboard_status_line_from_payload(&payload2),
        "dashboard: OFF (http://x)"
    );
}
