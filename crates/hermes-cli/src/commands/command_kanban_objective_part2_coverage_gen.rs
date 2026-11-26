#[test]
fn planner_modes_parse_aliases_and_emit_canonical_names() {
    if run_isolated("planner_modes_parse_aliases_and_emit_canonical_names") {
        return;
    }

    for (raw, mode, canonical) in [
        (" DISABLED ", PlanCapabilityMode::Off, "off"),
        ("0", PlanCapabilityMode::Off, "off"),
        (" WARN ", PlanCapabilityMode::Advisory, "advisory"),
        ("1", PlanCapabilityMode::Advisory, "advisory"),
        (" STRICT ", PlanCapabilityMode::Enforce, "enforce"),
    ] {
        assert_eq!(PlanCapabilityMode::parse(raw), Some(mode));
        assert_eq!(mode.as_str(), canonical);
    }
    assert_eq!(PlanCapabilityMode::parse("mandatory"), None);

    for (raw, profile, canonical) in [
        (" FAST ", TaskDepthProfile::Shallow, "shallow"),
        ("default", TaskDepthProfile::Balanced, "balanced"),
        (" THOROUGH ", TaskDepthProfile::Deep, "deep"),
        ("exhaustive", TaskDepthProfile::Max, "max"),
    ] {
        assert_eq!(TaskDepthProfile::parse(raw), Some(profile));
        assert_eq!(profile.as_str(), canonical);
    }
    assert_eq!(TaskDepthProfile::parse("medium"), None);
}

#[test]
fn planner_infers_requirements_independently() {
    if run_isolated("planner_infers_requirements_independently") {
        return;
    }

    for (task, expected) in [
        ("Summarize the meeting", [false, false, false, false]),
        ("PATCH the typo", [true, false, false, false]),
        ("Perform an AUDIT", [false, true, false, false]),
        ("Work end-to-end", [false, false, true, false]),
        ("Summarize the SCREENSHOT", [false, false, false, true]),
        (
            "AUDIT the ENTIRE REPO and check the DIAGRAM",
            [true, true, true, true],
        ),
    ] {
        let req = infer_plan_requirements(task);
        assert_eq!(
            [
                req.require_tools,
                req.require_reasoning,
                req.require_long_context,
                req.require_vision
            ],
            expected,
            "{task}"
        );
        assert_eq!(
            req.is_empty(),
            !expected.into_iter().any(|required| required)
        );
    }
}

#[test]
fn task_depth_profiles_apply_runtime_budgets() {
    if run_isolated("task_depth_profiles_apply_runtime_budgets") {
        return;
    }

    if isolated_test_root("task_depth_profiles_apply_runtime_budgets", &[]).is_none() {
        return;
    }
    const KEYS: [&str; 8] = [
        "HERMES_TASK_DEPTH_PROFILE",
        "HERMES_MAX_ITERATIONS",
        "HERMES_TOOL_CALL_MAX_CONCURRENCY",
        "HERMES_MAX_DELEGATE_DEPTH",
        "HERMES_PERF_GOV_WINDOW",
        "HERMES_PERF_GOV_LATENCY_WARN_MS",
        "HERMES_PERF_GOV_LATENCY_CRITICAL_MS",
        "HERMES_REPO_REVIEW_BUDGET_PROFILE",
    ];
    let _restore = RestoreEnvironment::capture(&KEYS);
    assert_eq!(current_task_depth_profile(), TaskDepthProfile::Balanced);

    for (profile, expected) in [
        (
            TaskDepthProfile::Shallow,
            [
                "shallow",
                "18",
                "10",
                "1",
                "6",
                "2800.00",
                "5200.00",
                "aggressive",
            ],
        ),
        (
            TaskDepthProfile::Deep,
            [
                "deep", "120", "6", "3", "10", "4800.00", "9000.00", "relaxed",
            ],
        ),
        (
            TaskDepthProfile::Max,
            ["max", "250", "5", "4", "12", "6500.00", "12000.00", "off"],
        ),
        (
            TaskDepthProfile::Balanced,
            [
                "balanced", "50", "12", "4", "8", "3500.00", "6500.00", "off",
            ],
        ),
    ] {
        apply_task_depth_profile(profile);
        for (key, value) in KEYS.into_iter().zip(expected) {
            assert_eq!(std::env::var(key).unwrap(), value, "{key}");
        }
        assert_eq!(current_task_depth_profile(), profile);
        assert_eq!(
            task_depth_runtime_summary(),
            format!(
                "task_depth profile={} max_iterations={} tool_concurrency={} max_delegate_depth={} repo_budget_profile={}",
                expected[0], expected[1], expected[2], expected[3], expected[7]
            )
        );
    }
}

#[test]
fn graph_candidate_walk_filters_subtrees_and_respects_limit() {
    if run_isolated("graph_candidate_walk_filters_subtrees_and_respects_limit") {
        return;
    }

    let root = tempfile::tempdir().expect("create graph project");
    let candidates = ["lib.RS", "main.py", "src/nested/view.tsx", "src/util.js"];
    for path in candidates.into_iter().chain([
        "README.md",
        "LICENSE",
        ".git/skip.rs",
        "target/skip.rs",
        "node_modules/skip.js",
        ".venv/skip.py",
        "venv/skip.py",
        "__pycache__/skip.py",
        ".mypy_cache/skip.py",
        ".pytest_cache/skip.py",
    ]) {
        let file = root.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "").unwrap();
    }
    let mut expected: Vec<_> = candidates
        .iter()
        .map(|path| root.path().join(path))
        .collect();
    expected.sort();
    let mut found = Vec::new();
    collect_graph_candidate_files(root.path(), 100, &mut found).unwrap();
    found.sort();
    assert_eq!(found, expected);

    found.clear();
    collect_graph_candidate_files(root.path(), 1, &mut found).unwrap();
    assert_eq!(found.len(), 1);
    assert!(expected.contains(&found[0]));

    let missing = root.path().join("does-not-exist");
    collect_graph_candidate_files(&missing, 1, &mut found).unwrap();
    assert_eq!(found.len(), 1);
    let error = collect_graph_candidate_files(&missing, 2, &mut found).unwrap_err();
    assert!(
        matches!(error, AgentError::Io(message) if message.starts_with(&format!("read_dir {}:", missing.display())))
    );
}

#[test]
fn graph_import_extraction_and_node_sanitization() {
    if run_isolated("graph_import_extraction_and_node_sanitization") {
        return;
    }

    assert_eq!(
        extract_semantic_refs_for_file(
            "rs",
            "  use crate::model; // note\nmod worker;\nuse ;\nmod ;\nfn main() {}",
        ),
        ["crate::model", "worker"]
    );
    assert_eq!(
        extract_semantic_refs_for_file(
            "py",
            "import os, , pathlib as paths\n  from .worker import run\nprint('done')",
        ),
        ["os", "pathlib", ".worker"]
    );
    for ext in ["ts", "tsx", "js", "jsx"] {
        assert_eq!(
            extract_semantic_refs_for_file(
                ext,
                "import { run } from './worker';\nexport { view } from \"./view\";\nconst path = require( 'node:path' );",
            ),
            ["./worker", "./view", "node:path"],
            "{ext}"
        );
    }
    assert!(extract_semantic_refs_for_file("txt", "use crate::model;").is_empty());
    assert_eq!(sanitize_graph_node("A-z_9./: \n?é"), "A-z_9./:____");
}

#[test]
fn graph_settings_prefer_current_names_and_fall_back_to_legacy_names() {
    if run_isolated("graph_settings_prefer_current_names_and_fall_back_to_legacy_names") {
        return;
    }

    if isolated_test_root(
        "graph_settings_prefer_current_names_and_fall_back_to_legacy_names",
        &[],
    )
    .is_none()
    {
        return;
    }
    const CURRENT_URL: &str = "CONTEXTLATTICE_ORCHESTRATOR_URL";
    const LEGACY_URL: &str = "MEMMCP_ORCHESTRATOR_URL";
    const CURRENT_KEY: &str = "CONTEXTLATTICE_API_KEY";
    const LEGACY_KEY: &str = "MEMMCP_API_KEY";
    let _restore = RestoreEnvironment::capture(&[CURRENT_URL, LEGACY_URL, CURRENT_KEY, LEGACY_KEY]);

    assert_eq!(contextlattice_base_url_for_graph(), "http://127.0.0.1:8075");
    assert_eq!(contextlattice_api_key_for_graph(), None);

    std::env::set_var(LEGACY_URL, "https://legacy.invalid");
    std::env::set_var(LEGACY_KEY, "legacy-key");
    std::env::set_var(CURRENT_URL, " \t");
    std::env::set_var(CURRENT_KEY, "");
    assert_eq!(
        contextlattice_base_url_for_graph(),
        "https://legacy.invalid"
    );
    assert_eq!(
        contextlattice_api_key_for_graph().as_deref(),
        Some("legacy-key")
    );

    std::env::set_var(CURRENT_URL, "https://current.invalid");
    std::env::set_var(CURRENT_KEY, "current-key");
    assert_eq!(
        contextlattice_base_url_for_graph(),
        "https://current.invalid"
    );
    assert_eq!(
        contextlattice_api_key_for_graph().as_deref(),
        Some("current-key")
    );

    std::env::remove_var(CURRENT_URL);
    std::env::remove_var(CURRENT_KEY);
    std::env::set_var(LEGACY_URL, " ");
    std::env::set_var(LEGACY_KEY, "\t");
    assert_eq!(contextlattice_base_url_for_graph(), "http://127.0.0.1:8075");
    assert_eq!(contextlattice_api_key_for_graph(), None);
}

#[tokio::test]
async fn graph_diagnostics_report_rejected_urls_without_network() {
    if run_isolated("graph_diagnostics_report_rejected_urls_without_network") {
        return;
    }

    // Reqwest rejects the file scheme before attempting a connection.
    if isolated_test_root(
        "graph_diagnostics_report_rejected_urls_without_network",
        &[
            (
                "CONTEXTLATTICE_ORCHESTRATOR_URL",
                "file:///coverage-diagnostics/",
            ),
            ("CONTEXTLATTICE_API_KEY", "coverage-key"),
        ],
    )
    .is_none()
    {
        return;
    }

    let lines = contextlattice_embedding_diagnostics_lines().await;
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(
        lines[0].starts_with("health_status: unreachable ("),
        "{lines:?}"
    );
    assert!(
        lines[1].starts_with("embedding_diagnostics: unavailable (unreachable: "),
        "{lines:?}"
    );
    assert_eq!(lines[2], "embedding_diagnostics: fallback=recall_telemetry");
    assert!(
        lines[3].starts_with("recall_telemetry: unreachable ("),
        "{lines:?}"
    );
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
