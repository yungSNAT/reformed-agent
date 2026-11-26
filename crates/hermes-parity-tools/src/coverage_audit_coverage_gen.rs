use super::*;

fn parse(text: &str) -> Py {
    match py::loads(text) {
        Ok(value) => value,
        Err(error) => panic!("invalid test JSON: {error}"),
    }
}

fn catalogue() -> TestCatalogue {
    let mut tests = TestCatalogue::new();
    tests.insert(
        PathBuf::from("crates/x/tests/a.rs"),
        BTreeSet::from(["t1".to_string()]),
    );
    tests
}

fn sample_manifest(tests: &TestCatalogue) -> Manifest {
    let data = parse(
        r#"{"entries":[
            {"path":"a","status":"deferred_not_covered","rust_tests":[]},
            {"path":"b","status":"covered","not_rechecked_at_pin":true},
            {"path":"c","status":"covered","rust_tests":[
                1,
                {"file":"crates/x/tests/a.rs","name":"t1"},
                {"file":"crates/x/tests/a.rs","name":"missing"},
                {"file":"","name":"x"}
            ]}
        ]}"#,
    );
    Manifest::from_rows("docs/m.json", read_rows(&data, "entries"), tests)
}

fn setup() -> (tempfile::TempDir, TestCatalogue, Vec<Domain>) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("docs/parity")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("tests/foo.rs"), "#[test]\nfn t() {}\n").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    fs::write(
        root.join(DOMAIN_SPECS),
        r#"[
            {"id":"alpha","upstream_scope":"scope a","local_evidence_globs":["tests/foo.rs"]},
            {"id":"beta","upstream_scope":["x"],"local_evidence_globs":["src/lib.rs"]}
        ]"#,
    )
    .unwrap();
    let tests = catalogue();
    let mut domains = read_domains(root, &tests).unwrap();
    domains.push(Domain {
        id: "gamma".into(),
        scope: Py::Null,
        evidence: BTreeMap::new(),
        direct: 0,
    });
    (tmp, tests, domains)
}

fn kinds(report: &Py, key: &str) -> Vec<String> {
    read_rows(report, key)
        .iter()
        .map(|row| property(row, "kind", ""))
        .collect()
}

#[test]
fn path_string_joins_components_with_slashes() {
    assert_eq!(path_string(Path::new("a/b/c.rs")), "a/b/c.rs");
    assert_eq!(path_string(Path::new("single")), "single");
}

#[test]
fn truthy_handles_every_variant() {
    assert!(!truthy(&Py::Null));
    assert!(truthy(&Py::Bool(true)));
    assert!(!truthy(&Py::Bool(false)));
    assert!(truthy(&Py::Int(3)));
    assert!(!truthy(&Py::Int(0)));
    assert!(truthy(&Py::Float(1.5)));
    assert!(!truthy(&Py::Float(0.0)));
    assert!(truthy(&Py::Str("x".into())));
    assert!(!truthy(&Py::Str(String::new())));
    assert!(truthy(&Py::List(vec![Py::Int(1)])));
    assert!(!truthy(&Py::List(Vec::new())));
    assert!(truthy(&parse(r#"{"a":1}"#)));
    assert!(!truthy(&parse("{}")));
}

#[test]
fn integer_parses_supported_shapes() {
    let row = parse(r#"{"b":true,"i":5,"f":2.7,"s":" 42 ","bad":"x","list":[1],"zero":0}"#);
    assert_eq!(integer(&row, "b").unwrap(), 1);
    assert_eq!(integer(&row, "i").unwrap(), 5);
    assert_eq!(integer(&row, "f").unwrap(), 2);
    assert_eq!(integer(&row, "s").unwrap(), 42);
    assert_eq!(integer(&row, "zero").unwrap(), 0);
    assert_eq!(integer(&row, "absent").unwrap(), 0);
    assert_eq!(
        integer(&row, "bad").unwrap_err(),
        "invalid integer field bad"
    );
    assert_eq!(
        integer(&row, "list").unwrap_err(),
        "invalid integer field list"
    );
}

#[test]
fn floating_parses_supported_shapes() {
    let row = parse(r#"{"b":true,"i":3,"f":0.5,"s":" 1.25 ","bad":"abc","d":{"k":1}}"#);
    assert_eq!(floating(&row, "b").unwrap(), 1.0);
    assert_eq!(floating(&row, "i").unwrap(), 3.0);
    assert_eq!(floating(&row, "f").unwrap(), 0.5);
    assert_eq!(floating(&row, "s").unwrap(), 1.25);
    assert_eq!(floating(&row, "absent").unwrap(), 0.0);
    assert_eq!(
        floating(&row, "bad").unwrap_err(),
        "invalid float field bad"
    );
    assert_eq!(floating(&row, "d").unwrap_err(), "invalid float field d");
}

#[test]
fn manifest_classifies_rows_and_citations() {
    let tests = catalogue();
    let manifest = sample_manifest(&tests);
    assert_eq!(manifest.entries, 3);
    assert_eq!(manifest.deferred, vec!["a".to_string()]);
    assert_eq!(manifest.not_rechecked, vec!["b".to_string()]);
    assert_eq!(manifest.missing_arrays, vec!["b".to_string()]);
    assert_eq!(manifest.missing_refs.len(), 3);
    assert_eq!(property(&manifest.missing_refs[0], "path", ""), "c");
    assert_eq!(property(&manifest.missing_refs[0], "file", ""), "unknown");
    assert_eq!(property(&manifest.missing_refs[1], "name", ""), "missing");
    assert_eq!(property(&manifest.missing_refs[2], "name", ""), "x");
    assert_eq!(manifest.referenced.len(), 2);
    assert!(manifest
        .referenced
        .contains(&("crates/x/tests/a.rs".to_string(), "t1".to_string())));
    assert_eq!(manifest.statuses.get("covered"), Some(&2));
    assert_eq!(manifest.statuses.get("deferred_not_covered"), Some(&1));
    assert_eq!(manifest.valid(), 0);
    let report = manifest.to_report();
    assert_eq!(property(&report, "valid_entries_with_rust_tests", ""), "0");
    assert_eq!(property(&report, "referenced_rust_tests", ""), "2");
}

#[test]
fn missing_reference_builds_dict() {
    let row = missing_reference("p", "f.rs", "n");
    assert_eq!(property(&row, "path", ""), "p");
    assert_eq!(property(&row, "file", ""), "f.rs");
    assert_eq!(property(&row, "name", ""), "n");
}

#[test]
fn direct_evidence_detects_test_locations() {
    let tests = catalogue();
    assert!(direct_evidence(Path::new("crates/x/tests/a.rs"), &tests));
    assert!(direct_evidence(Path::new("tests/x.rs"), &tests));
    assert!(direct_evidence(Path::new("a/tests/x.rs"), &tests));
    assert!(direct_evidence(Path::new("a/fixtures/x.json"), &tests));
    assert!(direct_evidence(
        Path::new("crates/hermes-parity-tests/fixtures/x.json"),
        &tests
    ));
    assert!(!direct_evidence(Path::new("src/lib.rs"), &tests));
}

#[test]
fn string_array_formats_items() {
    assert_eq!(string_array(std::iter::empty()), "[]");
    assert_eq!(
        string_array(vec!["a".to_string(), "b\nc".to_string()].into_iter()),
        "[\n  a,\n  b\n  c\n]"
    );
}

#[test]
fn read_domains_classifies_and_serialises() {
    let (_tmp, tests, domains) = setup();
    assert_eq!(domains.len(), 3);
    assert_eq!(domains[0].id, "alpha");
    assert_eq!(domains[0].classification(), "direct_rust_test");
    assert_eq!(domains[0].evidence.len(), 1);
    assert_eq!(domains[0].direct, 1);
    assert_eq!(domains[1].id, "beta");
    assert_eq!(domains[1].classification(), "domain_contract");
    assert_eq!(domains[1].direct, 0);
    assert_eq!(domains[2].classification(), "missing");

    let alpha = domains[0].json(&tests);
    assert!(alpha.contains("\"id\": \"alpha\""));
    assert!(alpha.contains("\"classification\": \"direct_rust_test\""));
    assert!(alpha.contains("\"evidence_count\": 1"));
    assert!(!alpha.contains("\"evidence_sample\": []"));
    assert!(!alpha.contains("\"direct_test_evidence_sample\": []"));

    let beta = domains[1].json(&tests);
    assert!(beta.contains("\"classification\": \"domain_contract\""));
    assert!(beta.contains("\"direct_test_evidence_sample\": []"));

    let gamma = domains[2].json(&tests);
    assert!(gamma.contains("\"mapped\": false"));
    assert!(gamma.contains("\"evidence_sample\": []"));
}

#[test]
fn read_domains_rejects_invalid_specs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("docs/parity")).unwrap();
    let tests = catalogue();

    fs::write(root.join(DOMAIN_SPECS), "{}").unwrap();
    match read_domains(root, &tests) {
        Err(error) => assert!(error.contains("must contain a list")),
        Ok(_) => panic!("expected non-list error"),
    }

    fs::write(root.join(DOMAIN_SPECS), r#"[{"id":1}]"#).unwrap();
    match read_domains(root, &tests) {
        Err(error) => assert!(error.contains("invalid domain")),
        Ok(_) => panic!("expected invalid domain error"),
    }
}

#[test]
fn evaluate_reports_gaps_and_markdown_renders_domains_and_moves() {
    let (tmp, tests, domains) = setup();
    let root = tmp.path();
    fs::write(root.join("docs/parity/harness-budget.json"), "{}").unwrap();
    fs::write(root.join("docs/parity/harness-budget.md"), "# b\n").unwrap();
    let manifests = vec![sample_manifest(&tests)];
    let inputs = Inputs {
        queue: parse(r#"{"summary":{"by_disposition":{"pending":2},"total_commits":5}}"#),
        intent: parse("{}"),
        matrix: parse(r#"{"summary":{"commits_behind":3}}"#),
        divergences: parse(r#"{"summary":{"errors":1}}"#),
    };
    let report = evaluate(
        root,
        &tests,
        &domains,
        &manifests,
        &inputs,
        "2024-01-01T00:00:00Z",
    )
    .unwrap();

    let gate = report.get("audit_gate").unwrap();
    assert!(gate.get("pass") == Some(&Py::Bool(false)));

    let critical = kinds(&report, "critical_gaps");
    for kind in [
        "divergence_governance",
        "coverage_manifest_missing_tests",
        "coverage_manifest_missing_refs",
        "missing_domain_contracts",
    ] {
        assert!(
            critical.iter().any(|k| k == kind),
            "missing critical {kind}"
        );
    }
    let advisory = kinds(&report, "advisory_gaps");
    for kind in [
        "upstream_queue_pending",
        "coverage_manifest_deferred",
        "coverage_manifest_not_rechecked",
        "domain_contract_without_direct_test_file",
        "nonzero_tree_drift",
    ] {
        assert!(
            advisory.iter().any(|k| k == kind),
            "missing advisory {kind}"
        );
    }

    let summary = report.get("summary").unwrap();
    assert_eq!(property(summary, "tracked_behavior_rows", ""), "6");
    assert_eq!(property(summary, "covered_behavior_rows", ""), "2");
    assert_eq!(property(summary, "queue_pending", ""), "2");
    assert!(matches!(
        summary.get("tracked_behavior_coverage_ratio"),
        Some(Py::Float(v)) if (*v - 0.3333).abs() < 1e-9
    ));

    let markdown = render_markdown(&report, &manifests, &domains);
    assert!(markdown.contains("| `alpha` | `direct_rust_test` | 1 | 1 |"));
    assert!(markdown.contains("| `beta` | `domain_contract` | 1 | 0 |"));
    assert!(markdown.contains("| `gamma` | `missing` | 0 | 0 |"));
    assert!(markdown.contains(
        "- **Cross-version harness budget**: `docs/parity/harness-budget.json`, `docs/parity/harness-budget.md`"
    ));
    assert!(markdown.contains("**FAIL**"));
}
