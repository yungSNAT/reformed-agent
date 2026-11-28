use std::fs;
use std::path::Path;

use super::*;

const FILES: [(&str, &str); 13] = [
    (
        "thresholds.json",
        r#"{"ci_thresholds": {}, "release_thresholds": {}, "program": "demo"}"#,
    ),
    ("parity.json", r#"{"summary": {}}"#),
    ("workstreams.json", r#"{"states": {}}"#),
    ("intents.json", r#"{"summary": {}}"#),
    ("adapters.json", r#"{"summary": {}}"#),
    ("shared_class.json", r#"{"items": []}"#),
    (
        "shared_backlog.json",
        r#"{"summary": {"deferred_review": 0}}"#,
    ),
    ("divergence.json", r#"{"summary": {}}"#),
    ("queue.json", r#"{"summary": {"by_disposition": {}}}"#),
    (
        "coverage.json",
        r#"{
            "summary": {
                "tracked_behavior_rows": 0,
                "covered_behavior_rows": 0,
                "coverage_manifest_deferred_entries": 0,
                "coverage_manifest_not_rechecked_entries": 0
            },
            "audit_gate": {},
            "coverage_manifests": []
        }"#,
    ),
    ("harness.json", r#"{"summary": {}, "gate": {}}"#),
    ("behavioral.json", r#"{"summary": {}, "gate": {}}"#),
    ("deep.json", r#"{"summary": {}, "gate": {}}"#),
];

fn args_for(root: &Path) -> GlobalProofArgs {
    GlobalProofArgs {
        repo_root: root.to_string_lossy().into_owned(),
        thresholds: "thresholds.json".to_string(),
        parity_matrix: "parity.json".to_string(),
        workstream_status: "workstreams.json".to_string(),
        intent_mapping: "intents.json".to_string(),
        adapter_matrix: "adapters.json".to_string(),
        shared_diff_classification: "shared_class.json".to_string(),
        shared_diff_backlog: "shared_backlog.json".to_string(),
        divergence_validation: "divergence.json".to_string(),
        patch_queue: "queue.json".to_string(),
        test_coverage_audit: "coverage.json".to_string(),
        sota_harness_matrix: "harness.json".to_string(),
        behavioral_diff: "behavioral.json".to_string(),
        deep_problem_solving_diff: "deep.json".to_string(),
        out_json: "out.json".to_string(),
        out_md: "out.md".to_string(),
        check_ci: false,
        check_release: false,
    }
}

fn write_all(root: &Path) {
    for (name, contents) in FILES {
        fs::write(root.join(name), contents).expect("write fixture");
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn source_path_strips_current_dir_components() {
    assert_eq!(source_path("./a.json"), "a.json");
    assert_eq!(source_path("."), ".");
    assert_eq!(source_path("./"), ".");
    assert_eq!(
        source_path("a/./b"),
        py::path_str(&Path::new("a").join("b"))
    );
}

#[test]
fn read_json_handles_valid_invalid_and_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("ok.json"), r#"{"a": 1}"#).expect("write");
    fs::write(root.join("list.json"), "[1, 2]").expect("write");
    fs::write(root.join("bad.json"), "{not json").expect("write");

    let ok = read_json(root, "ok.json").expect("valid object");
    assert!(ok.is_dict());
    assert!(matches!(get(&ok, "a"), Py::Int(1)));

    let err = read_json(root, "list.json").expect_err("list must fail");
    assert!(err.contains("expected a JSON object"), "{err}");

    let err = read_json(root, "bad.json").expect_err("bad must fail");
    assert!(err.contains("bad.json"), "{err}");

    assert!(read_json(root, "missing.json").is_err());
}

#[test]
fn get_and_entries_defaults() {
    let value = py::loads(r#"{"a": true}"#).expect("loads");
    assert!(matches!(get(&value, "a"), Py::Bool(true)));
    let missing = get(&value, "zzz");
    assert!(missing.is_dict());
    assert!(entries(missing).is_empty());

    let dict = Py::Dict(vec![("k".to_string(), Py::Null)]);
    let found = entries(&dict);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, "k");
    assert!(entries(&Py::Null).is_empty());
}

#[test]
fn items_supports_iterables_and_rejects_others() {
    let list = items(&Py::List(vec![Py::Int(1), Py::Int(2)]), "f").expect("list");
    assert_eq!(list.len(), 2);

    let dict = Py::Dict(vec![
        ("x".to_string(), Py::Null),
        ("y".to_string(), Py::Null),
    ]);
    let keys = items(&dict, "f").expect("dict");
    assert_eq!(keys.len(), 2);
    assert!(matches!(&keys[0], Py::Str(s) if s == "x"));
    assert!(matches!(&keys[1], Py::Str(s) if s == "y"));

    let chars = items(&Py::Str("ab".to_string()), "f").expect("str");
    assert_eq!(chars.len(), 2);
    assert!(matches!(&chars[0], Py::Str(s) if s == "a"));
    assert!(matches!(&chars[1], Py::Str(s) if s == "b"));

    let err = items(&Py::Int(3), "field").expect_err("int fails");
    assert_eq!(err, "field must be iterable");
}

#[test]
fn truth_follows_python_semantics() {
    assert!(!truth(&Py::Null));
    assert!(truth(&Py::Bool(true)));
    assert!(!truth(&Py::Bool(false)));
    assert!(truth(&Py::Int(2)));
    assert!(!truth(&Py::Int(0)));
    assert!(truth(&Py::Float(0.5)));
    assert!(!truth(&Py::Float(0.0)));
    assert!(truth(&Py::Str("x".to_string())));
    assert!(!truth(&Py::Str(String::new())));
    assert!(truth(&Py::List(vec![Py::Null])));
    assert!(!truth(&Py::List(vec![])));
    assert!(truth(&Py::Dict(vec![("a".to_string(), Py::Null)])));
    assert!(!truth(&Py::Dict(vec![])));
}

#[test]
fn number_parses_supported_values() {
    assert!(close(number(&Py::Int(3)).expect("int"), 3.0));
    assert!(close(number(&Py::Float(1.5)).expect("float"), 1.5));
    assert!(close(number(&Py::Bool(true)).expect("bool"), 1.0));
    assert!(close(number(&Py::Bool(false)).expect("bool"), 0.0));
    assert!(close(
        number(&Py::Str("1_000".to_string())).expect("underscore"),
        1000.0
    ));
    assert!(close(
        number(&Py::Str("  2.5 ".to_string())).expect("trimmed"),
        2.5
    ));
}

#[test]
fn number_rejects_invalid_values() {
    for bad in ["_1", "1_", "1__0", "abc", ""] {
        let err = number(&Py::Str(bad.to_string()))
            .err()
            .unwrap_or_else(|| panic!("{bad:?} should fail"));
        assert!(err.contains("invalid number"), "{err}");
    }
    let err = number(&Py::Null).expect_err("null fails");
    assert!(err.starts_with("expected a number, got"), "{err}");
    assert!(number(&Py::List(vec![])).is_err());
}

#[test]
fn reports_read_succeeds_with_valid_fixtures() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_all(dir.path());
    let args = args_for(dir.path());
    let reports = Reports::read(dir.path(), &args).expect("read valid fixtures");
    assert!(matches!(get(&reports.thresholds, "program"), Py::Str(s) if s == "demo"));
    assert!(get(&reports.queue, "summary").is_dict());
    assert!(get(&reports.harness, "gate").is_dict());
    assert!(reports.parity.is_dict());
    assert!(reports.workstreams.is_dict());
    assert!(reports.intents.is_dict());
    assert!(reports.adapters.is_dict());
    assert!(reports.shared_classification.is_dict());
    assert!(reports.shared_backlog.is_dict());
    assert!(reports.divergence.is_dict());
    assert!(reports.coverage.is_dict());
    assert!(reports.behavioral.is_dict());
    assert!(reports.deep.is_dict());
}

#[test]
fn reports_read_rejects_non_object_summary() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_all(dir.path());
    fs::write(dir.path().join("parity.json"), r#"{"summary": 1}"#).expect("write");
    let args = args_for(dir.path());
    let err = Reports::read(dir.path(), &args).err().expect("must fail");
    assert_eq!(err, "parity.json: summary must be an object");
}

#[test]
fn reports_read_rejects_non_object_by_disposition() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_all(dir.path());
    fs::write(
        dir.path().join("queue.json"),
        r#"{"summary": {"by_disposition": 1}}"#,
    )
    .expect("write");
    let args = args_for(dir.path());
    let err = Reports::read(dir.path(), &args).err().expect("must fail");
    assert_eq!(err, "queue.json: by_disposition must be an object");
}

#[test]
fn reports_read_rejects_non_object_thresholds_dicts() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_all(dir.path());
    fs::write(
        dir.path().join("thresholds.json"),
        r#"{"ci_thresholds": {}, "release_thresholds": 1}"#,
    )
    .expect("write");
    let args = args_for(dir.path());
    let err = Reports::read(dir.path(), &args).err().expect("must fail");
    assert_eq!(err, "thresholds.json: release_thresholds must be an object");
}

#[test]
fn reports_read_propagates_missing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_all(dir.path());
    fs::remove_file(dir.path().join("deep.json")).expect("remove");
    let args = args_for(dir.path());
    assert!(Reports::read(dir.path(), &args).is_err());
}

#[test]
fn summary_and_gate_extracts_fields() {
    let report =
        py::loads(r#"{"summary": {"a": 1}, "gate": {"pass": true}, "other": 2}"#).expect("loads");
    let result = summary_and_gate(&report, "gate");
    assert!(matches!(get(get(&result, "summary"), "a"), Py::Int(1)));
    assert!(matches!(get(get(&result, "gate"), "pass"), Py::Bool(true)));
    assert!(result.get("other").is_none());

    let empty = summary_and_gate(&Py::Dict(vec![]), "audit_gate");
    assert!(get(&empty, "summary").is_dict());
    assert!(entries(get(&empty, "audit_gate")).is_empty());
}

#[test]
fn sources_lists_normalised_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut args = args_for(dir.path());
    args.thresholds = "./thresholds.json".to_string();
    let result = sources(&args);
    assert_eq!(entries(&result).len(), 12);
    assert!(matches!(get(&result, "thresholds"), Py::Str(s) if s == "thresholds.json"));
    assert!(matches!(get(&result, "deep_problem_solving_diff"), Py::Str(s) if s == "deep.json"));
    assert!(result.get("shared_diff_backlog").is_none());
}

#[test]
fn run_fails_when_reports_are_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let args = args_for(dir.path());
    assert!(run(args).is_err());
    assert!(!dir.path().join("out.json").exists());
    assert!(!dir.path().join("out.md").exists());
}

#[test]
fn run_fails_when_report_shape_invalid() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_all(dir.path());
    fs::write(
        dir.path().join("harness.json"),
        r#"{"summary": {}, "gate": 1}"#,
    )
    .expect("write");
    let args = args_for(dir.path());
    let err = run(args).expect_err("must fail");
    assert_eq!(err, "harness.json: gate must be an object");
    assert!(!dir.path().join("out.json").exists());
}

#[test]
fn proof_metrics_reject_incomplete_deferral_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_all(root);
    let mut reports = Reports::read(root, &args_for(root)).expect("read valid fixtures");
    let valid_classification = std::mem::replace(
        &mut reports.shared_classification,
        py::loads(r#"{"items": [{"classification": "deferred"}]}"#).unwrap(),
    );
    assert_eq!(
        metrics::calculate(root, &reports).unwrap_err(),
        "report is missing review_date"
    );

    reports.shared_classification = py::loads(
        r#"{"items": [{"path": "fixture.rs", "classification": "intentional_divergence"}]}"#,
    )
    .unwrap();
    let metrics = metrics::calculate(root, &reports).unwrap();
    assert!(metrics.contains(&("max_deferred_review_overdue", 0.0)));

    reports.shared_classification = valid_classification;
    reports.shared_backlog = py::loads(r#"{"summary": {}}"#).unwrap();
    assert_eq!(
        metrics::calculate(root, &reports).unwrap_err(),
        "report is missing deferred_review"
    );
}

#[test]
fn proof_metrics_reject_overflow_subtracting_deferred_rows() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_all(root);
    let mut reports = Reports::read(root, &args_for(root)).expect("read valid fixtures");
    reports.coverage = Py::dict([(
        "summary",
        Py::dict([
            ("tracked_behavior_rows", Py::Int(i128::MIN)),
            ("covered_behavior_rows", Py::Int(0)),
            ("coverage_manifest_deferred_entries", Py::Int(1)),
            ("coverage_manifest_not_rechecked_entries", Py::Int(0)),
        ]),
    )]);
    assert_eq!(
        metrics::calculate(root, &reports).unwrap_err(),
        "coverage count exceeds the integer range"
    );
}

#[test]
fn proof_reports_write_error_before_creating_second_report() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_all(root);
    fs::create_dir(root.join("out.json")).unwrap();
    let error = run(args_for(root)).unwrap_err();
    assert!(error.contains("out.json"), "{error}");
    assert!(root.join("out.json").is_dir());
    assert!(!root.join("out.md").exists());
}

#[test]
fn proof_markdown_omits_ticket_summary_when_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_all(root);
    fs::write(
        root.join("queue.json"),
        r#"{"summary": {"total_commits": 2, "by_disposition": {}, "by_target_ticket": null}}"#,
    )
    .unwrap();

    assert_eq!(run(args_for(root)), Ok(0));
    let markdown = fs::read_to_string(root.join("out.md")).unwrap();
    assert!(markdown.contains("Upstream missing commits tracked: `2`."));
    assert!(!markdown.contains("By target ticket"));
}
