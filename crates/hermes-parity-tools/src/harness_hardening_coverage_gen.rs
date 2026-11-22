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

#[test]
fn replay_accepts_empty_domain_sources_but_rejects_non_iterable_values() {
    for source in ["{}", r#"{"domains":"abc"}"#, r#"{"domains":{"id":"x"}}"#] {
        let report = replay(&Py::Null, &Py::Null, &Py::Null, &json(source)).unwrap();
        assert_json(&report, &["replay_evidence_groups"], "[]");
        assert_json(&report, &["latest_gate_state", "queue_pending"], "0");
    }
    assert_eq!(
        replay(&Py::Null, &Py::Null, &Py::Null, &json(r#"{"domains":42}"#),)
            .err()
            .as_deref(),
        Some("SOTA harness domains must be iterable")
    );
}

#[test]
fn budget_accepts_equality_at_every_limit_and_reports_default_failures() {
    let at_limits = json(
        r#"{
            "queue_pending": 2,
            "tracked_behavior_coverage_ratio": 0.875,
            "sota_domain_coverage_ratio": 0.75,
            "rust_test_functions": 8000,
            "coverage_manifest_entries": 600,
            "sota_workflow_replay_steps": 12,
            "sota_protocol_cases": 20,
            "sota_fault_scenarios": 16
        }"#,
    );
    let report = budget(&at_limits, &json(THRESHOLDS_INPUT)).unwrap();
    assert_json(&report, &["gate"], r#"{"pass":true,"failures":[]}"#);
    assert_eq!(
        py::dumps(field(&report, &["snapshot"]), None, true),
        py::dumps(&at_limits, None, true)
    );
    let rows = list(field(&report, &["budgets"]));
    let expected = [
        ("queue_pending", "2", "<="),
        ("tracked_behavior_coverage_ratio", "0.875", ">="),
        ("sota_domain_coverage_ratio", "0.75", ">="),
        ("rust_test_functions", "8000", "<="),
        ("coverage_manifest_entries", "600", "<="),
        ("sota_workflow_replay_steps", "12", "<="),
        ("sota_protocol_cases", "20", "<="),
        ("sota_fault_scenarios", "16", "<="),
    ];
    assert_eq!(rows.len(), expected.len());
    for (row, (id, value, operator)) in rows.iter().zip(expected) {
        assert_eq!(text(row, "id", ""), id);
        assert_eq!(text(row, "operator", ""), operator);
        assert!(!text(row, "reason", "").is_empty());
        assert_json(row, &["actual"], value);
        assert_json(row, &["limit"], value);
        assert_json(row, &["pass"], "true");
    }

    let mut over_limit = at_limits;
    over_limit.set("sota_fault_scenarios", Py::Int(17));
    let report = budget(&over_limit, &json("{}")).unwrap();
    assert_json(
        &report,
        &["gate"],
        r#"{
            "pass": false,
            "failures": [
                "queue_pending",
                "tracked_behavior_coverage_ratio",
                "sota_domain_coverage_ratio",
                "sota_fault_scenarios"
            ]
        }"#,
    );
    assert_eq!(
        budget(
            &over_limit,
            &json(r#"{"release_thresholds":{"functional_parity":[]}}"#),
        )
        .err()
        .as_deref(),
        Some("functional_parity thresholds must be an object")
    );
}

fn fixture_git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("start git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn snapshot_fixture(root: &Path) -> Result<Py, String> {
    snapshot(
        root,
        &json(COVERAGE_INPUT),
        &json(PROOF_INPUT),
        &json(QUEUE_INPUT),
        &json(SOTA_INPUT),
    )
}

fn assert_fixture_snapshot(root: &Path, head: &str, upstream: &str) {
    let clean = snapshot_fixture(root).unwrap();
    assert_timestamp(&clean, "recorded_at_utc");
    let mut expected = json(
        r#"{
            "release_gate_pass": true,
            "ci_gate_pass": false,
            "queue_pending": 2,
            "queue_total": 6,
            "tracked_behavior_coverage_ratio": 0.875,
            "sota_domain_coverage_ratio": 0.75,
            "tracked_behavior_rows": 8,
            "covered_behavior_rows": 7,
            "rust_test_functions": 14,
            "coverage_manifest_entries": 1,
            "missing_rust_test_refs": 0,
            "sota_direct_rust_tests": 11,
            "sota_workflow_replay_steps": 4,
            "sota_protocol_cases": 5,
            "sota_fault_scenarios": 3,
            "sota_critical_gaps": 1,
            "sota_missing_rust_test_refs": 2
        }"#,
    );
    expected.set("local_head", head.into());
    expected.set("upstream_head", upstream.into());
    expected.set(
        "recorded_at_utc",
        field(&clean, &["recorded_at_utc"]).clone(),
    );
    assert_eq!(
        py::dumps(&clean, None, true),
        py::dumps(&expected, None, true)
    );

    let change = root.join("untracked.txt");
    fs::write(&change, "local work\n").unwrap();
    assert_eq!(
        text(&snapshot_fixture(root).unwrap(), "local_head", ""),
        format!("{head}+worktree")
    );
    fs::remove_file(change).unwrap();

    let bad_queue = json(r#"{"summary":{"by_disposition":[]}}"#);
    assert_eq!(
        snapshot(root, &Py::Null, &Py::Null, &bad_queue, &Py::Null)
            .err()
            .as_deref(),
        Some("queue dispositions must be an object")
    );
}

fn exercise_report_modes(root: &Path, head: &str) {
    let run_at = |check| {
        run(HarnessHardeningArgs {
            repo_root: root.to_str().expect("UTF-8 fixture path").into(),
            check,
        })
    };
    assert_eq!(run_at(true), Ok(1));
    assert!(!root.join("docs").exists());

    fs::create_dir_all(root.join("docs/parity")).unwrap();
    for (path, source) in [
        (COVERAGE, COVERAGE_INPUT),
        (PROOF, PROOF_INPUT),
        (QUEUE, QUEUE_INPUT),
        (SOTA, SOTA_INPUT),
        (THRESHOLDS, THRESHOLDS_INPUT),
    ] {
        fs::write(root.join(path), source).unwrap();
    }

    let trend_path = root.join(format!("{TREND}.json"));
    fs::write(&trend_path, "[]").unwrap();
    assert_eq!(
        run_at(false),
        Err("harness sources must contain JSON objects".into())
    );
    assert_eq!(fs::read_to_string(&trend_path).unwrap(), "[]");
    fs::remove_file(&trend_path).unwrap();

    assert_eq!(run_at(true), Ok(0));
    for base in [TREND, REPLAY, BUDGET] {
        for suffix in ["json", "md"] {
            assert!(!root.join(format!("{base}.{suffix}")).exists());
        }
    }

    assert_eq!(run_at(false), Ok(0));
    let ledger = load(root, &format!("{TREND}.json")).unwrap();
    assert_eq!(
        text(field(&ledger, &["latest"]), "local_head", ""),
        format!("{head}+worktree")
    );
    assert_eq!(list(field(&ledger, &["entries"])).len(), 1);
    let index = load(root, &format!("{REPLAY}.json")).unwrap();
    assert_json(&index, &["latest_gate_state", "queue_pending"], "2");
    assert_eq!(list(field(&index, &["replay_evidence_groups"])).len(), 2);
    let review = load(root, &format!("{BUDGET}.json")).unwrap();
    assert_json(&review, &["gate"], r#"{"pass":true,"failures":[]}"#);
    for base in [TREND, REPLAY, BUDGET] {
        let markdown = fs::read_to_string(root.join(format!("{base}.md"))).unwrap();
        assert!(
            markdown.lines().any(|line| line.starts_with("# ")),
            "{base} should have a Markdown heading"
        );
    }
}

#[test]
fn git_snapshots_and_report_modes() {
    const FIXTURE_ROOT: &str = "HERMES_HARDENING_COVERAGE_FIXTURE_ROOT";

    // Isolate Git's home and configuration without changing the test runner's environment.
    if let Some(root) = std::env::var_os(FIXTURE_ROOT) {
        let root = Path::new(&root);
        fixture_git(root, &["init", "--quiet", "--template="]);
        for message in ["upstream fixture", "local fixture"] {
            fixture_git(
                root,
                &[
                    "-c",
                    "user.name=Harness Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    message,
                ],
            );
        }
        fixture_git(root, &["update-ref", "refs/remotes/upstream/main", "HEAD^"]);
        let head = fixture_git(root, &["rev-parse", "HEAD"]);
        let upstream = fixture_git(root, &["rev-parse", "HEAD^"]);
        assert_eq!(
            optional_git(root, &["rev-parse", "--verify", "refs/heads/missing"]).unwrap(),
            ""
        );
        assert_fixture_snapshot(root, &head, &upstream);
        exercise_report_modes(root, &head);
        return;
    }

    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("home");
    let root = sandbox.path().join("repo");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&root).unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let test_name = format!("{module}::git_snapshots_and_report_modes");
    let mut child = Command::new(std::env::current_exe().unwrap());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            child.env_remove(key);
        }
    }
    let output = child
        .args(["--exact", test_name.as_str(), "--nocapture"])
        .env(FIXTURE_ROOT, &root)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("start isolated Git test");
    assert!(
        output.status.success() && root.join(format!("{BUDGET}.md")).is_file(),
        "isolated Git test did not finish: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
