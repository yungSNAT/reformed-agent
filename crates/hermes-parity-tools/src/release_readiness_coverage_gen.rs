use super::*;

use std::fs;
use std::path::Path;

fn write_file(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
    fs::write(path, body).expect("write file");
}

fn json(text: &str) -> Py {
    py::loads(text).expect("valid json")
}

fn passing_repo(root: &Path) {
    write_file(
        root,
        "Cargo.toml",
        "[workspace.package]\nversion = \"1.2.3\"\n",
    );
    write_file(
        root,
        "docs/parity/global-parity-proof.json",
        r#"{"release_gate": {"pass": true}, "ci_gate": {"pass": true}, "metrics": {"max_commits_behind": 1}}"#,
    );
    write_file(
        root,
        "docs/parity/upstream-missing-queue.json",
        r#"{"summary": {"by_disposition": {"pending": 0}}}"#,
    );
    write_file(
        root,
        "docs/parity/global-parity-thresholds.json",
        r#"{"drift_alert_thresholds": {"warn_commits_behind": 5}}"#,
    );
}

#[test]
fn read_artifact_missing_file_is_empty_dict() {
    let dir = tempfile::tempdir().unwrap();
    let value = read_artifact(dir.path(), "missing.json").expect("ok");
    assert!(value.is_dict());
    assert!(value.get("anything").is_none());
}

#[test]
fn read_artifact_parses_and_reports_errors() {
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "good.json", r#"{"a": {"b": 7}}"#);
    write_file(dir.path(), "bad.json", "{not json");
    fs::create_dir_all(dir.path().join("adir.json")).unwrap();

    let good = read_artifact(dir.path(), "good.json").expect("ok");
    assert_eq!(text(&good, "missing", "fallback"), "fallback");
    assert_eq!(integer(metric(&good, &["a", "b"])), Ok(7));

    let bad = read_artifact(dir.path(), "bad.json").expect_err("error");
    assert!(bad.starts_with("bad.json: "), "{bad}");

    let other = read_artifact(dir.path(), "adir.json").expect_err("error");
    assert!(other.starts_with("adir.json: "), "{other}");
}

#[test]
fn metric_walks_nested_keys() {
    let payload = json(r#"{"x": {"y": {"z": 3}}}"#);
    assert_eq!(integer(metric(&payload, &["x", "y", "z"])), Ok(3));
    assert!(metric(&payload, &["x", "missing", "z"]).is_none());
    assert!(metric(&payload, &[]).is_some());
}

#[test]
fn integer_handles_all_variants() {
    assert_eq!(integer(None), Ok(0));
    assert_eq!(integer(Some(&Py::Int(-4))), Ok(-4));
    assert_eq!(integer(Some(&Py::Bool(true))), Ok(1));
    assert_eq!(integer(Some(&Py::Bool(false))), Ok(0));
    assert_eq!(integer(Some(&Py::Float(f64::NAN))), Ok(0));
    assert_eq!(integer(Some(&Py::Float(3.7))), Ok(3));
    assert!(integer(Some(&Py::Float(1e40))).is_err());
    assert!(integer(Some(&Py::Float(f64::NEG_INFINITY))).is_err());
    assert_eq!(integer(Some(&Py::from("12"))), Ok(12));
    assert_eq!(integer(Some(&Py::Null)), Ok(0));
    assert_eq!(integer(Some(&Py::List(vec![]))), Ok(0));
}

#[test]
fn truth_follows_python_semantics() {
    assert!(!truth(None));
    assert!(!truth(Some(&Py::Null)));
    assert!(!truth(Some(&Py::Bool(false))));
    assert!(truth(Some(&Py::Bool(true))));
    assert!(truth(Some(&Py::Int(2))));
    assert!(!truth(Some(&Py::Int(0))));
    assert!(truth(Some(&Py::Float(0.5))));
    assert!(!truth(Some(&Py::Float(0.0))));
    assert!(truth(Some(&Py::from("x"))));
    assert!(!truth(Some(&Py::from(""))));
    assert!(truth(Some(&Py::List(vec![Py::Null]))));
    assert!(!truth(Some(&Py::List(vec![]))));
    assert!(!truth(Some(&Py::dict::<&str>([]))));
    assert!(truth(Some(&Py::dict([("a", Py::Int(1))]))));
}

#[test]
fn state_labels() {
    assert_eq!(state(true), "PASS");
    assert_eq!(state(false), "FAIL");
}

#[test]
fn workspace_version_variants() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(workspace_version(dir.path()), Ok("unknown".to_string()));

    write_file(dir.path(), "Cargo.toml", "[package]\nname = \"x\"\n");
    assert_eq!(workspace_version(dir.path()), Ok("unknown".to_string()));

    write_file(dir.path(), "Cargo.toml", "[package]\nversion = \"9.8.7\"\n");
    assert_eq!(workspace_version(dir.path()), Ok("9.8.7".to_string()));

    let other = tempfile::tempdir().unwrap();
    fs::create_dir_all(other.path().join("Cargo.toml")).unwrap();
    let error = workspace_version(other.path()).expect_err("error");
    assert!(error.starts_with("Cargo.toml: "), "{error}");
}

#[test]
fn git_output_outside_repository_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    if let Ok(output) = git_output(dir.path(), &["rev-parse", "--verify", "no-such-ref-xyz"]) {
        assert_eq!(output, "");
    }
}

#[test]
fn upstream_watch_rejects_non_objects() {
    let dict = Py::dict::<&str>([]);
    assert!(upstream_watch(&Py::Int(1), &dict, 0, true).is_err());
    assert!(upstream_watch(&dict, &Py::Int(1), 0, true).is_err());
}

#[test]
fn upstream_watch_statuses() {
    let proof = json(
        r#"{"metrics": {"max_commits_behind": 10, "max_files_only_upstream": 2}, "ci_gate": {"pass": true}}"#,
    );
    let thresholds = json(
        r#"{"drift_alert_thresholds": {"warn_commits_behind": 5, "warn_files_only_upstream": 0}}"#,
    );
    let watch = upstream_watch(&proof, &thresholds, 0, true).expect("ok");
    assert_eq!(text(&watch, "status", ""), "WATCH");
    assert!(truth(watch.get("pass")));
    assert!(truth(watch.get("ci_gate_pass")));
    match watch.get("alerts") {
        Some(Py::List(alerts)) => {
            assert_eq!(alerts.len(), 1);
            assert_eq!(text(&alerts[0], "metric", ""), "max_commits_behind");
            assert_eq!(integer(alerts[0].get("actual")), Ok(10));
            assert_eq!(integer(alerts[0].get("warn_threshold")), Ok(5));
            assert_eq!(text(&alerts[0], "status", ""), "watch");
        }
        _ => panic!("alerts missing"),
    }

    let clean = json(r#"{"metrics": {}, "ci_gate": {"pass": true}}"#);
    let empty = Py::dict::<&str>([]);
    let pass = upstream_watch(&clean, &empty, 0, true).expect("ok");
    assert_eq!(text(&pass, "status", ""), "PASS");
    assert_eq!(integer(pass.get("schema_version")), Ok(1));

    let no_ci = upstream_watch(&empty, &empty, 0, true).expect("ok");
    assert_eq!(text(&no_ci, "status", ""), "WATCH");

    let fail = upstream_watch(&clean, &empty, 3, true).expect("ok");
    assert_eq!(text(&fail, "status", ""), "FAIL");
    assert!(!truth(fail.get("pass")));
    assert_eq!(integer(fail.get("queue_pending")), Ok(3));

    let bad = json(r#"{"metrics": {"max_commits_behind": 1e300}}"#);
    assert!(upstream_watch(&bad, &empty, 0, true).is_err());
}

#[test]
fn gate_with_and_without_limit() {
    let with = gate("id1", "Label", Py::Int(3), Some(Py::Int(0)), false);
    assert_eq!(text(&with, "id", ""), "id1");
    assert_eq!(text(&with, "label", ""), "Label");
    assert_eq!(integer(with.get("actual")), Ok(3));
    assert_eq!(integer(with.get("limit")), Ok(0));
    assert!(!truth(with.get("pass")));

    let without = gate("id2", "Other", Py::from("PASS"), None, true);
    assert!(without.get("limit").is_none());
    assert!(truth(without.get("pass")));
}

#[test]
fn build_summary_empty_repository_fails() {
    let dir = tempfile::tempdir().unwrap();
    let Ok((report, ok)) = build_summary(dir.path()) else {
        return; // git unavailable
    };
    assert!(!ok);
    assert!(!truth(report.get("ok")));
    assert_eq!(text(&report, "workspace_version", ""), "unknown");
    let summary = report.get("summary").expect("summary");
    assert!(!truth(summary.get("release_gate_pass")));
    assert_eq!(text(summary, "upstream_delta_watch_status", ""), "FAIL");
    assert_eq!(integer(summary.get("upstream_delta_watch_alerts")), Ok(0));
    match report.get("checks") {
        Some(Py::List(checks)) => assert_eq!(checks.len(), 9),
        _ => panic!("checks missing"),
    }
}

#[test]
fn build_summary_passing_repository() {
    let dir = tempfile::tempdir().unwrap();
    passing_repo(dir.path());
    let Ok((report, ok)) = build_summary(dir.path()) else {
        return; // git unavailable
    };
    assert!(ok);
    assert_eq!(text(&report, "workspace_version", ""), "1.2.3");
    let summary = report.get("summary").expect("summary");
    assert_eq!(integer(summary.get("queue_pending")), Ok(0));
    assert_eq!(text(summary, "upstream_delta_watch_status", ""), "PASS");
    let sources = report.get("source_artifacts").expect("sources");
    assert_eq!(
        text(sources, "global_parity_proof", ""),
        "docs/parity/global-parity-proof.json"
    );

    let md = markdown(&report);
    assert!(md.contains("Status: **PASS**"), "{md}");
    assert!(md.contains("Workspace version: `1.2.3`"), "{md}");
    assert!(
        md.contains("| Upstream queue pending | `0` | `0` | PASS |"),
        "{md}"
    );
    assert!(
        md.contains("| Global release gate | `PASS` | `PASS` | PASS |"),
        "{md}"
    );
    assert!(md.contains("- `global_parity_proof`: `docs/parity/global-parity-proof.json`"));
    assert!(md.contains("- Status: `PASS`"), "{md}");
    assert!(md.contains("- Drift alerts: `0`"), "{md}");
}

#[test]
fn build_summary_reports_invalid_artifact() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "docs/parity/upstream-missing-queue.json",
        "{oops",
    );
    let error = build_summary(dir.path()).expect_err("error");
    assert!(error.contains("upstream-missing-queue.json"), "{error}");
}

#[test]
fn build_summary_reports_out_of_range_metric() {
    let dir = tempfile::tempdir().unwrap();
    write_file(
        dir.path(),
        "docs/parity/shared-diff-backlog.json",
        r#"{"summary": {"pending_classification": 1e300}}"#,
    );
    assert!(build_summary(dir.path()).is_err());
}

#[test]
fn markdown_with_tag_and_without_sections() {
    let report = Py::dict([
        ("ok", false.into()),
        ("tag", "v1.0".into()),
        ("head", "abcdef0123456789".into()),
    ]);
    let md = markdown(&report);
    assert!(md.contains("Status: **FAIL**"), "{md}");
    assert!(md.contains("Workspace version: `unknown`"), "{md}");
    assert!(md.contains("HEAD: `abcdef012345`\n"), "{md}");
    assert!(md.contains("Tag: `v1.0`"), "{md}");
    assert!(md.contains("## Upstream Delta Watch"));
    assert!(!md.contains("- Drift alerts"));

    let no_tag = markdown(&Py::dict::<&str>([]));
    assert!(!no_tag.contains("Tag:"));
}

#[test]
fn markdown_counts_watch_alerts() {
    let report = Py::dict([
        ("ok", true.into()),
        (
            "upstream_delta_watch",
            Py::dict([
                ("status", "WATCH".into()),
                ("alerts", Py::List(vec![Py::Null, Py::Null])),
            ]),
        ),
    ]);
    let md = markdown(&report);
    assert!(md.contains("- Status: `WATCH`"), "{md}");
    assert!(md.contains("- Queue pending: `0`"), "{md}");
    assert!(md.contains("- Drift alerts: `2`"), "{md}");

    let report = Py::dict([("upstream_delta_watch", Py::dict::<&str>([]))]);
    let md = markdown(&report);
    assert!(md.contains("- Drift alerts: `0`"), "{md}");
}

#[test]
fn run_writes_reports_and_honours_check() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().expect("utf8").to_string();
    let args = ReadinessArgs {
        repo_root: root.clone(),
        output_json: "reports/out.json".into(),
        output_md: "reports/out.md".into(),
        check: true,
    };
    let Ok(code) = run(args) else {
        return; // git unavailable
    };
    assert_eq!(code, 1);
    let json_text = fs::read_to_string(dir.path().join("reports/out.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_text).unwrap();
    assert_eq!(parsed["ok"], serde_json::Value::Bool(false));
    let md = fs::read_to_string(dir.path().join("reports/out.md")).unwrap();
    assert!(md.contains("Status: **FAIL**"));

    let args = ReadinessArgs {
        repo_root: root,
        output_json: "reports/out.json".into(),
        output_md: "reports/out.md".into(),
        check: false,
    };
    assert_eq!(run(args), Ok(0));
}

#[test]
fn run_passing_with_check_returns_zero() {
    let dir = tempfile::tempdir().unwrap();
    passing_repo(dir.path());
    let args = ReadinessArgs {
        repo_root: dir.path().to_str().expect("utf8").to_string(),
        output_json: "out/summary.json".into(),
        output_md: "out/summary.md".into(),
        check: true,
    };
    let Ok(code) = run(args) else {
        return; // git unavailable
    };
    assert_eq!(code, 0);
    let md = fs::read_to_string(dir.path().join("out/summary.md")).unwrap();
    assert!(md.contains("Status: **PASS**"));
}
