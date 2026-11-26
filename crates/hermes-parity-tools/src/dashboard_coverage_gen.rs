use super::*;

fn parse(source: &str) -> Py {
    py::loads(source).unwrap_or_else(|_| panic!("failed to parse test JSON: {source}"))
}

#[test]
fn text_uses_default_when_missing() {
    assert_eq!(text(None, "fallback"), "fallback");
    let value = parse(r#"{"a": "hello"}"#);
    assert_eq!(text(value.get("a"), "fallback"), "hello");
}

#[test]
fn snapshot_returns_value_or_unknown() {
    let value = parse(r#"{"a": "x", "b": ""}"#);
    assert_eq!(snapshot(&value, "a"), "x");
    assert_eq!(snapshot(&value, "b"), "unknown");
    assert_eq!(snapshot(&value, "missing"), "unknown");
}

#[test]
fn child_returns_null_for_missing_key() {
    let value = parse(r#"{"a": {"b": 1}}"#);
    assert!(child(&value, "a").is_dict());
    assert!(matches!(child(&value, "zzz"), Py::Null));
}

#[test]
fn gate_status_variants() {
    assert_eq!(gate_status(&parse(r#"{"pass": true}"#)), "PASS");
    assert_eq!(gate_status(&parse(r#"{"pass": false}"#)), "FAIL");
    assert_eq!(gate_status(&parse(r#"{"pass": 1}"#)), "UNKNOWN");
    assert_eq!(gate_status(&parse(r#"{}"#)), "UNKNOWN");
}

#[test]
fn checks_filters_by_status() {
    let gate = parse(
        r#"{"checks": [
            {"status": "FAIL", "metric": "drift", "actual": 5, "limit": 0},
            {"status": "warn"},
            {"status": "fail", "metric": "other", "actual": 1, "limit": 2},
            "not a dict"
        ]}"#,
    );
    assert_eq!(
        checks(&gate, "fail"),
        "drift (actual=5, limit=0); other (actual=1, limit=2)"
    );
    assert_eq!(
        checks(&gate, "warn"),
        "unknown_metric (actual=n/a, limit=n/a)"
    );
    assert_eq!(checks(&gate, "pass"), "none");
    assert_eq!(checks(&parse(r#"{"pass": true}"#), "fail"), "none");
    assert_eq!(checks(&parse(r#"{"checks": "nope"}"#), "fail"), "none");
}

#[test]
fn integer_formats_values() {
    assert_eq!(integer(&Py::Null).unwrap(), "0");
    assert_eq!(integer(&Py::Bool(false)).unwrap(), "0");
    assert_eq!(integer(&Py::Bool(true)).unwrap(), "1");
    assert_eq!(integer(&parse("42")).unwrap(), "42");
    assert_eq!(integer(&Py::Float(2.9)).unwrap(), "2");
    assert_eq!(integer(&Py::Float(-3.7)).unwrap(), "-3");
    assert_eq!(integer(&Py::Float(0.4)).unwrap(), "0");
    assert!(integer(&parse(r#""12""#)).is_ok());
    assert_eq!(
        integer(&parse("[1]")).unwrap_err(),
        "expected an integer dashboard metric"
    );
    assert!(integer(&Py::Float(f64::INFINITY)).is_err());
    assert_eq!(integer(&parse("[]")).unwrap(), "0");
}

#[test]
fn fraction_formats_values() {
    assert_eq!(fraction(&Py::Null).unwrap(), "0.0000");
    assert_eq!(fraction(&Py::Bool(true)).unwrap(), "1.0000");
    assert_eq!(fraction(&parse("3")).unwrap(), "3.0000");
    assert_eq!(fraction(&Py::Float(0.25)).unwrap(), "0.2500");
    assert_eq!(fraction(&parse(r#""0.5""#)).unwrap(), "0.5000");
    assert!(fraction(&parse(r#""abc""#)).is_err());
    assert_eq!(
        fraction(&parse("[1]")).unwrap_err(),
        "expected a decimal dashboard metric"
    );
    let infinite = fraction(&Py::Float(f64::INFINITY)).unwrap();
    assert!(infinite.contains("inf"), "{infinite}");
    assert_eq!(infinite, infinite.to_lowercase());
}

#[test]
fn table_helpers_append_headings() {
    let mut lines = Vec::new();
    metric_table(&mut lines, "Heading");
    assert_eq!(
        lines,
        vec![
            String::new(),
            "## Heading".to_string(),
            String::new(),
            "| Metric | Value |".to_string(),
            "| --- | ---: |".to_string(),
        ]
    );
}

#[test]
fn metrics_formats_ratio_and_integers() {
    let mut lines = Vec::new();
    let source = parse(r#"{"count": 3, "ratio": 0.5}"#);
    metrics(
        &mut lines,
        &source,
        &[
            ("Count", "count"),
            ("Ratio", "ratio"),
            ("Missing", "missing"),
        ],
        "ratio",
    )
    .unwrap();
    assert_eq!(
        lines,
        vec![
            "| Count | 3 |".to_string(),
            "| Ratio | 0.5000 |".to_string(),
            "| Missing | 0 |".to_string(),
        ]
    );
    let bad = parse(r#"{"count": [1]}"#);
    assert!(metrics(&mut lines, &bad, &[("Count", "count")], "").is_err());
}

#[test]
fn comparison_table_lists_all_rows() {
    let mut lines = Vec::new();
    let summary = parse(r#"{"total_cases": 4, "my_ratio": 0.75, "gaps": 1}"#);
    comparison_table(&mut lines, "Cmp", &summary, "my_ratio", "My ratio").unwrap();
    assert!(lines.contains(&"## Cmp".to_string()));
    assert!(lines.contains(&"| Total cases | 4 |".to_string()));
    assert!(lines.contains(&"| My ratio | 0.7500 |".to_string()));
    assert!(lines.contains(&"| Gaps | 1 |".to_string()));
    assert!(lines.contains(&"| Missing Rust test refs | 0 |".to_string()));
}

fn null_sources() -> Sources {
    Sources {
        matrix: Py::Null,
        streams: Py::Null,
        queue: Py::Null,
        proof: Py::Null,
        coverage: Py::Null,
        harness: Py::Null,
        behavioural: Py::Null,
        deep: Py::Null,
    }
}

#[test]
fn render_with_empty_sources() {
    let output = render(&null_sources()).unwrap();
    assert!(output.starts_with("# Parity Dashboard\n"));
    assert!(output.contains("_Generated from source artifacts: `unknown`_"));
    assert!(output.contains("- Upstream target: `unknown` @ `unknown`"));
    assert!(output.contains("- Release gate: **UNKNOWN**"));
    assert!(output.contains("- Release gate failures: none"));
    assert!(output.contains("| Tracked behavior coverage ratio | 0.0000 |"));
    assert!(output.contains("| commits_behind | 0 |"));
    assert!(output.contains("## Workstream States"));
    assert!(!output.contains("## Workstream Detail"));
    assert!(output.contains(&format!("- `{DEEP_PROOF}`")));
    assert!(output.ends_with("\n\n"));
}

#[test]
fn render_propagates_metric_errors() {
    let mut sources = null_sources();
    sources.coverage = parse(r#"{"summary": {"tracked_behavior_rows": [1]}}"#);
    assert!(render(&sources).is_err());

    let mut sources = null_sources();
    sources.streams = parse(r#"{"states": {"done": [1]}}"#);
    assert!(render(&sources).is_err());

    let mut sources = null_sources();
    sources.matrix = parse(r#"{"summary": {"commits_behind": {"x": 1}}}"#);
    assert!(render(&sources).is_err());
}

#[test]
fn render_uses_fallback_time_and_empty_workstreams() {
    let mut sources = null_sources();
    sources.matrix = parse(r#"{"generated_at_utc": "2024-05-05"}"#);
    sources.streams = parse(r#"{"workstreams": []}"#);
    let output = render(&sources).unwrap();
    assert!(output.contains("_Generated from source artifacts: `2024-05-05`_"));
    assert!(!output.contains("## Workstream Detail"));
}

#[test]
fn load_handles_dict_non_dict_invalid_and_missing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("dict.json"), r#"{"a": 1}"#).unwrap();
    fs::write(dir.path().join("list.json"), "[1, 2]").unwrap();
    fs::write(dir.path().join("bad.json"), "{not json").unwrap();
    assert!(load(dir.path(), "dict.json").unwrap().is_dict());
    assert!(matches!(load(dir.path(), "list.json").unwrap(), Py::Null));
    assert!(matches!(load(dir.path(), "bad.json").unwrap(), Py::Null));
    assert!(matches!(
        load(dir.path(), "missing.json").unwrap(),
        Py::Null
    ));
}

#[test]
fn run_writes_full_dashboard() {
    let dir = tempfile::tempdir().unwrap();
    let parity = dir.path().join("docs/parity");
    fs::create_dir_all(&parity).unwrap();
    let write = |name: &str, body: &str| fs::write(dir.path().join(name), body).unwrap();
    write(
        WORKSTREAMS,
        r#"{"generated_at_utc": "2024-01-01", "upstream_ref": "main", "upstream_sha": "abc123",
            "states": {"done": 3, "active": 2},
            "workstreams": [{"workstream": "WS1", "title": "First", "state": "done"}, {"title": "NoId"}, 5]}"#,
    );
    write(
        PARITY_MATRIX,
        r#"{"generated_at_utc": "2024-01-02", "summary": {"commits_behind": 4, "commits_ahead": 2.7}}"#,
    );
    write(
        QUEUE,
        r#"{"summary": {"total_commits": 7, "by_disposition": {"pending": 3, "ported": true, "superseded": 0}}}"#,
    );
    write(
        GLOBAL_PROOF,
        r#"{"generated_at_utc": "2024-02-03",
            "release_gate": {"pass": false, "checks": [{"status": "fail", "metric": "drift", "actual": 5, "limit": 0}]},
            "ci_gate": {"pass": true, "checks": [{"status": "warn", "metric": "lag", "actual": 2, "limit": 1}]}}"#,
    );
    write(
        COVERAGE,
        r#"{"audit_gate": {"pass": true, "critical_gaps": 0},
            "summary": {"tracked_behavior_rows": 10, "covered_behavior_rows": 9,
                        "tracked_behavior_coverage_ratio": 0.9, "rust_test_functions": 100,
                        "missing_rust_test_refs": 0}}"#,
    );
    write(
        HARNESS,
        r#"{"gate": {"pass": false, "critical_gaps": 2, "missing_rust_test_refs": 1},
            "summary": {"domains_total": 5, "domains_passing": 4, "domain_coverage_ratio": 0.8,
                        "direct_rust_tests": 12}}"#,
    );
    write(
        BEHAVIOURAL,
        r#"{"gate": {"pass": true},
            "summary": {"total_cases": 20, "equal_or_better_cases": 19, "superior_cases": 3,
                        "behavioral_similarity_ratio": 0.95, "regressions": 1, "gaps": 0,
                        "unverified_cases": 0, "missing_rust_test_refs": 0}}"#,
    );
    write(DEEP_PROOF, "{not valid json");

    let args = DashboardArgs {
        repo_root: dir.path().to_string_lossy().into_owned(),
        parity_matrix: PARITY_MATRIX.to_string(),
        workstream_status: WORKSTREAMS.to_string(),
        queue_json: QUEUE.to_string(),
        proof_json: GLOBAL_PROOF.to_string(),
        test_coverage_audit: COVERAGE.to_string(),
        sota_harness_matrix: HARNESS.to_string(),
        behavioral_diff: BEHAVIOURAL.to_string(),
        deep_problem_solving_diff: DEEP_PROOF.to_string(),
        output: "out/nested/PARITY_DASHBOARD.md".to_string(),
    };
    assert_eq!(run(args).unwrap(), 0);

    let written = fs::read_to_string(dir.path().join("out/nested/PARITY_DASHBOARD.md")).unwrap();
    assert!(written.contains("_Generated from source artifacts: `2024-02-03`_"));
    assert!(written.contains("- Upstream target: `main` @ `abc123`"));
    assert!(written.contains("- Workstream snapshot generated: `2024-01-01`"));
    assert!(written.contains("- Parity matrix generated: `2024-01-02`"));
    assert!(written.contains("- Queue snapshot generated: `unknown`"));
    assert!(written.contains("- Release gate: **FAIL**"));
    assert!(written.contains("- CI/tree-drift gate: **PASS**"));
    assert!(written.contains("- Test coverage audit: **PASS**"));
    assert!(written.contains("- SOTA harness matrix: **FAIL**"));
    assert!(written.contains("- Behavioral similarity diff: **PASS**"));
    assert!(written.contains("- Deep problem-solving diff: **UNKNOWN**"));
    assert!(written.contains("- Release gate failures: drift (actual=5, limit=0)"));
    assert!(written.contains("- CI gate failures: none"));
    assert!(written.contains("- CI gate warnings: lag (actual=2, limit=1)"));
    assert!(written.contains("| Tracked behavior coverage ratio | 0.9000 |"));
    assert!(written.contains("| Rust test functions | 100 |"));
    assert!(written.contains("| Domain coverage ratio | 0.8000 |"));
    assert!(written.contains("| Critical gaps | 2 |"));
    assert!(written.contains("| Similarity ratio | 0.9500 |"));
    assert!(written.contains("| Deep problem-solving ratio | 0.0000 |"));
    assert!(written.contains("| Total commits in queue | 7 |"));
    assert!(written.contains("| Ported | 1 |"));
    assert!(written.contains("| Superseded | 0 |"));
    assert!(written.contains("| commits_behind | 4 |"));
    assert!(written.contains("| commits_ahead | 2 |"));
    let active = written.find("| active | 2 |").unwrap();
    let done = written.find("| done | 3 |").unwrap();
    assert!(active < done);
    assert!(written.contains("## Workstream Detail"));
    assert!(written.contains("| WS1 | First | done |"));
    assert!(written.contains("| ? | NoId |  |"));
    assert!(written.contains(&format!("- `{PARITY_MATRIX}`")));
}
