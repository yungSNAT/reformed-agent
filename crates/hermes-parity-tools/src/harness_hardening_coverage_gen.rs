use super::*;
use std::process::Command;

const COVERAGE_INPUT: &str = r#"{
    "audit_gate": {"pass": true},
    "summary": {
        "tracked_behavior_coverage_ratio": "0.875",
        "tracked_behavior_rows": 8,
        "covered_behavior_rows": "7",
        "rust_test_functions": 14.75,
        "coverage_manifest_entries": true,
        "missing_rust_test_refs": 0
    }
}"#;
const PROOF_INPUT: &str = r#"{
    "release_gate": {"pass": "ready"},
    "ci_gate": {"pass": false}
}"#;
const QUEUE_INPUT: &str = r#"{
    "summary": {"by_disposition": {"pending": "2"}, "total_commits": 6}
}"#;
const SOTA_INPUT: &str = r#"{
    "summary": {
        "domain_coverage_ratio": 0.75,
        "direct_rust_tests": 11,
        "workflow_replay_steps": 4,
        "protocol_cases": 5,
        "fault_scenarios": 3
    },
    "gate": {"pass": false, "critical_gaps": 1, "missing_rust_test_refs": 2},
    "domains": [
        false,
        {
            "id": "replay",
            "title": "Workflow replay",
            "status": "covered",
            "why": "Check deterministic playback",
            "fixtures": ["fixtures/replay.json"],
            "rust_tests": ["replay_is_repeatable"],
            "required_capabilities": ["playback"]
        },
        {}
    ]
}"#;
const THRESHOLDS_INPUT: &str = r#"{
    "release_thresholds": {"functional_parity": {
        "max_queue_pending_commits": "2",
        "min_test_coverage_tracked_behavior_ratio": "0.875",
        "min_sota_harness_domain_coverage_ratio": 0.75
    }}
}"#;

fn json(source: &str) -> Py {
    py::loads(source).expect("valid fixture JSON")
}

fn assert_json(value: &Py, keys: &[&str], expected: &str) {
    assert_eq!(
        py::dumps(field(value, keys), None, true),
        py::dumps(&json(expected), None, true),
        "unexpected value at {keys:?}"
    );
}

fn assert_timestamp(value: &Py, key: &str) {
    assert!(
        matches!(field(value, &[key]), Py::Str(stamp) if stamp.parse::<jiff::Timestamp>().is_ok()),
        "expected a timestamp at {key}"
    );
}

#[test]
fn load_distinguishes_missing_valid_malformed_and_unreadable_inputs() {
    let root = tempfile::tempdir().unwrap();
    assert_json(&load(root.path(), "missing.json").unwrap(), &[], "{}");

    let path = root.path().join("fixture.json");
    fs::write(&path, r#"{"summary":{"rows":[1,2]}}"#).unwrap();
    let loaded = load(root.path(), "fixture.json").unwrap();
    assert_json(&loaded, &["summary", "rows"], "[1,2]");
    assert_eq!(list(field(&loaded, &["summary", "rows"])).len(), 2);
    assert!(list(field(&loaded, &["summary"])).is_empty());
    assert_json(&loaded, &["summary", "rows", "missing"], "null");

    fs::write(&path, "{").unwrap();
    assert!(load(root.path(), "fixture.json").is_err());

    let directory = root.path().join("directory.json");
    fs::create_dir(&directory).unwrap();
    let error =
        load(root.path(), "directory.json").expect_err("reading a directory as JSON should fail");
    assert!(error.starts_with(&format!("{}: ", directory.display())));
}

#[test]
fn metric_coercion_handles_numeric_strings_booleans_and_non_numbers() {
    for (value, expected) in [
        (Py::Bool(true), 1),
        (Py::Bool(false), 0),
        (Py::Int(19), 19),
        (Py::Float(-3.75), -3),
        ("-27".into(), -27),
        ("not numeric".into(), 0),
        (Py::Null, 0),
    ] {
        assert!(matches!(integer(&value), Ok(Py::Int(actual)) if actual == expected));
    }

    for infinity in [f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            integer(&Py::Float(infinity)).err().as_deref(),
            Some("cannot convert an infinite harness metric to an integer")
        );
    }

    for (value, expected) in [
        (Py::Bool(true), 1.0_f64),
        (Py::Bool(false), 0.0),
        (Py::Int(-6), -6.0),
        (Py::Float(1.5), 1.5),
        ("0.875".into(), 0.875),
        ("not numeric".into(), 0.0),
        (Py::List(Vec::new()), 0.0),
    ] {
        assert_eq!(decimal(&value).to_bits(), expected.to_bits());
    }
}

#[test]
fn trend_replaces_duplicate_heads_and_retains_the_newest_64_entries() {
    let mut entries: Vec<_> = (0..65)
        .map(|index| Py::dict([("local_head", format!("history-{index}").into())]))
        .collect();
    entries.push(json(r#"{"local_head":"current","queue_pending":999}"#));
    let previous = json(
        r#"{
            "local_head": "history-65",
            "queue_pending": "7",
            "tracked_behavior_rows": 42,
            "covered_behavior_rows": 40,
            "rust_test_functions": 100,
            "coverage_manifest_entries": 8,
            "sota_direct_rust_tests": 12,
            "sota_workflow_replay_steps": 4,
            "sota_protocol_cases": 9
        }"#,
    );
    entries.push(previous.clone());
    entries.push(json(r#"{"local_head":"current","queue_pending":111}"#));
    entries.push(Py::Bool(true));

    let latest = json(
        r#"{
            "local_head": "current",
            "queue_pending": 4,
            "tracked_behavior_rows": 45,
            "covered_behavior_rows": 42.5,
            "rust_test_functions": 111,
            "coverage_manifest_entries": 10,
            "sota_direct_rust_tests": 17,
            "sota_workflow_replay_steps": false,
            "sota_protocol_cases": 15,
            "sota_fault_scenarios": 8
        }"#,
    );
    let report = trend(&Py::dict([("entries", Py::List(entries))]), &latest);
    let retained = list(field(&report, &["entries"]));
    assert_eq!(retained.len(), 64);
    for (entry, index) in retained[..63].iter().zip(3..66) {
        assert_eq!(text(entry, "local_head", ""), format!("history-{index}"));
    }
    assert_eq!(
        py::dumps(&retained[62], None, true),
        py::dumps(&previous, None, true)
    );
    let expected_latest = py::dumps(&latest, None, true);
    assert_eq!(py::dumps(&retained[63], None, true), expected_latest);
    assert_eq!(
        py::dumps(field(&report, &["latest"]), None, true),
        expected_latest
    );
    assert_json(
        &report,
        &["delta_from_previous_entry"],
        r#"{
            "queue_pending": -3.0,
            "tracked_behavior_rows": 3.0,
            "covered_behavior_rows": 2.5,
            "rust_test_functions": 11.0,
            "coverage_manifest_entries": 2.0,
            "sota_direct_rust_tests": 5.0,
            "sota_workflow_replay_steps": -4.0,
            "sota_protocol_cases": 6.0,
            "sota_fault_scenarios": 8.0
        }"#,
    );
}

#[test]
fn first_trend_entry_has_no_deltas_and_names_its_source_artifacts() {
    let report = trend(
        &json(r#"{"entries":"not a list"}"#),
        &json(r#"{"local_head":"first"}"#),
    );
    assert_timestamp(&report, "generated_at_utc");
    assert_json(&report, &["schema_version"], "1");
    assert_json(
        &report,
        &["purpose"],
        r#""Track parity coverage and SOTA harness movement over time.""#,
    );
    assert_json(&report, &["entries"], r#"[{"local_head":"first"}]"#);
    assert_json(&report, &["delta_from_previous_entry"], "{}");
    assert_json(
        &report,
        &["source_artifacts"],
        r#"{
            "global_parity_proof": "docs/parity/global-parity-proof.json",
            "harness_budget": "docs/parity/harness-budget.json",
            "harness_trend_ledger": "docs/parity/harness-trend-ledger.json",
            "sota_harness_matrix": "docs/parity/sota-harness-matrix.json",
            "test_coverage_audit": "docs/parity/test-coverage-audit.json",
            "upstream_missing_queue": "docs/parity/upstream-missing-queue.json"
        }"#,
    );
}

#[test]
fn replay_indexes_only_object_domains_and_supplies_missing_fields() {
    let report = replay(
        &json(COVERAGE_INPUT),
        &json(PROOF_INPUT),
        &json(QUEUE_INPUT),
        &json(SOTA_INPUT),
    )
    .unwrap();
    assert_json(
        &report,
        &["latest_gate_state"],
        r#"{
            "release_gate_pass": true,
            "ci_gate_pass": false,
            "test_coverage_gate_pass": true,
            "sota_harness_gate_pass": false,
            "queue_pending": 2
        }"#,
    );
    assert_json(
        &report,
        &["replay_evidence_groups"],
        r#"[
            {
                "id": "replay",
                "title": "Workflow replay",
                "status": "covered",
                "why": "Check deterministic playback",
                "contextlattice_topic": "hermes-agent-ultra/parity/sota/replay",
                "retrieval_query": "Hermes Agent Ultra SOTA harness evidence for replay including fixtures, Rust tests, failures, and queue status",
                "fixtures": ["fixtures/replay.json"],
                "rust_tests": ["replay_is_repeatable"],
                "required_capabilities": ["playback"]
            },
            {
                "id": "unknown",
                "title": "unknown",
                "status": "unknown",
                "why": "",
                "contextlattice_topic": "hermes-agent-ultra/parity/sota/unknown",
                "retrieval_query": "Hermes Agent Ultra SOTA harness evidence for unknown including fixtures, Rust tests, failures, and queue status",
                "fixtures": [],
                "rust_tests": [],
                "required_capabilities": []
            }
        ]"#,
    );
    assert_json(
        &report,
        &["contextlattice_checkpoint_template"],
        r#"{
            "project": "hermes-agent-ultra",
            "topic": "parity/sota",
            "command": "contextlattice_write -p hermes-agent-ultra -t parity/sota -f docs/parity/contextlattice-replay-evidence-index.md --stdin"
        }"#,
    );
}

include!("harness_hardening_part2_coverage_gen.rs");
