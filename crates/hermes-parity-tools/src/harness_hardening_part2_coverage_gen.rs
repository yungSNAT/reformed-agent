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
    let mut child = Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
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
