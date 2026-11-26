use super::*;

#[test]
fn gate_section_from_report_delegates_to_autopilot_section() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("report.json");
    let report = serde_json::json!({"ok": true, "summary": {"total": 1}});
    let got = gate_section_from_report("native eval trend gate", &report, &path);
    let expected = autopilot_native_section_from_report("native eval trend gate", &report, &path);
    assert_eq!(got, expected);

    let failing = serde_json::json!({"ok": false});
    let got_fail = gate_section_from_report("other", &failing, &path);
    let expected_fail = autopilot_native_section_from_report("other", &failing, &path);
    assert_eq!(got_fail, expected_fail);
}

#[test]
fn recommendations_all_failing_with_empty_objective() {
    let sections = serde_json::json!({});
    let recs = build_self_evolution_recommendations_native("   ", &sections);
    assert_eq!(recs.len(), 3);
    assert_eq!(recs[0]["id"], "PARITY_DRIFT");
    assert_eq!(recs[0]["severity"], "P0");
    assert_eq!(
        recs[0]["title"],
        "Resolve command/TUI parity drift before feature work"
    );
    assert_eq!(
        recs[0]["reason"],
        "Native parity proof or shared backlog gate failed."
    );
    assert_eq!(
        recs[0]["command"],
        "/ops gate status && hermes-ultra doctor --deep --snapshot"
    );
    assert_eq!(recs[1]["id"], "EVAL_REGRESSION");
    assert_eq!(recs[1]["reason"], "Eval trend gate failed.");
    assert_eq!(recs[2]["id"], "ELITE_GATE_FAIL");
    assert_eq!(recs[2]["reason"], "Native elite gate failed.");
}

#[test]
fn recommendations_parity_drift_with_objective() {
    let sections = serde_json::json!({
        "golden_parity": {"ok": false},
        "eval_trend": {"ok": true},
        "elite_sync": {"ok": true},
    });
    let recs = build_self_evolution_recommendations_native(" faster ", &sections);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0]["id"], "PARITY_DRIFT");
    assert_eq!(
        recs[0]["reason"],
        "Native parity proof or shared backlog gate failed. Objective: faster."
    );
}

#[test]
fn recommendations_promote_baseline_when_all_pass() {
    let sections = serde_json::json!({
        "golden_parity": {"ok": true},
        "eval_trend": {"ok": true},
        "elite_sync": {"ok": true},
    });
    let recs = build_self_evolution_recommendations_native("ship it", &sections);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0]["id"], "PROMOTE_BASELINE");
    assert_eq!(recs[0]["severity"], "P2");
    assert_eq!(recs[0]["title"], "Promote current state as next baseline");
    assert_eq!(
        recs[0]["reason"],
        "All enabled native sections passed; safe to store this run as a quality baseline. Objective: ship it."
    );
    assert_eq!(recs[0]["command"], "hermes-ultra doctor --deep --snapshot");

    let recs_empty = build_self_evolution_recommendations_native("", &sections);
    assert_eq!(
        recs_empty[0]["reason"],
        "All enabled native sections passed; safe to store this run as a quality baseline."
    );
}

#[cfg(unix)]
#[tokio::test]
async fn slo_auto_rollback_dry_run_skips_rollback_when_violated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let report_path = dir.path().join("slo.json");
    let (report, path) = run_slo_auto_rollback_native(
        dir.path(),
        "false",
        "echo rollback",
        true,
        Some(&report_path),
    )
    .await
    .expect("slo run");
    assert_eq!(path, report_path);
    assert_eq!(report["violated"], true);
    assert_eq!(report["ok"], false);
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["rollback"]["skipped"], true);
    assert_eq!(report["rollback"]["reason"], "dry_run");
    assert_eq!(report["rollback"]["command"], "echo rollback");
    assert_eq!(report["rollback"]["ok"], false);
    assert!(path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn slo_auto_rollback_no_violation_has_null_rollback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let report_path = dir.path().join("slo-ok.json");
    let (report, path) = run_slo_auto_rollback_native(
        dir.path(),
        "true",
        "echo rollback",
        false,
        Some(&report_path),
    )
    .await
    .expect("slo run");
    assert_eq!(path, report_path);
    assert_eq!(report["violated"], false);
    assert_eq!(report["ok"], true);
    assert!(report["rollback"].is_null());
    assert!(path.exists());
}
