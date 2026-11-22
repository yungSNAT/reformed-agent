use super::*;
use serde_json::json;

fn write_route_report(path: &Path, payload: &serde_json::Value) {
    std::fs::write(path, payload.to_string()).expect("write route health fixture");
}

fn harness_test_name(test: &str) -> String {
    let (_, module) = module_path!()
        .split_once("::")
        .expect("coverage tests are in a child module");
    format!("{module}::{test}")
}

#[test]
fn route_health_summary_formats_report_and_distinguishes_missing_data() {
    let dir = tempfile::tempdir().expect("create route health fixture directory");
    let path = dir.path().join("route-health.json");

    let missing = summarize_route_health_state(&path);
    assert_eq!(missing, "route_health=unknown");

    std::fs::write(&path, "{").expect("write malformed route health report");
    assert_eq!(summarize_route_health_state(&path), "route_health=unknown");

    write_route_report(
        &path,
        &json!({
            "overall": false,
            "summary": {"health_score": "0.875"},
            "generated_at": 42
        }),
    );
    assert_eq!(
        summarize_route_health_state(&path),
        "route_health=unknown score=0.00 @ unknown"
    );

    write_route_report(
        &path,
        &json!({
            "overall": "degraded",
            "summary": {"health_score": 0.875},
            "generated_at": "2026-02-01T12:00:00Z"
        }),
    );
    let summary = summarize_route_health_state(&path);
    assert_eq!(
        summary,
        "route_health=degraded score=0.88 @ 2026-02-01T12:00:00Z"
    );

    // Also used as a libtest subprocess fixture for the two output streams.
    println!("{summary}");
    eprintln!("{missing}");
}

#[test]
fn route_health_trace_distinguishes_absent_empty_and_unparseable_entries() {
    let dir = tempfile::tempdir().expect("create route health fixture directory");
    let path = dir.path().join("route-health.json");
    assert_eq!(summarize_route_health_details(&path), None);

    for payload in [json!({}), json!({"entries": {"key": "not-an-array"}})] {
        write_route_report(&path, &payload);
        assert_eq!(summarize_route_health_details(&path), None);
    }

    write_route_report(&path, &json!({"entries": []}));
    assert_eq!(
        summarize_route_health_details(&path).as_deref(),
        Some("route_health_trace=no_entries")
    );

    write_route_report(
        &path,
        &json!({"entries": [null, {"key": 7}, {"tier": "primary"}]}),
    );
    assert_eq!(
        summarize_route_health_details(&path).as_deref(),
        Some("route_health_trace=no_parseable_entries")
    );
}

#[test]
fn route_health_trace_lists_the_three_lowest_scores_and_filters_reasons() {
    let dir = tempfile::tempdir().expect("create route health fixture directory");
    let path = dir.path().join("route-health.json");
    write_route_report(
        &path,
        &json!({
            "entries": [
                {"key": "healthy", "tier": "primary", "health_score": 0.99},
                {
                    "key": "slow",
                    "tier": "standard",
                    "health_score": 0.4,
                    "reasons": ["timeout", 17, "rate_limit"]
                },
                {"tier": "ignored", "health_score": -1},
                {"key": "unmeasured", "health_score": "unknown"},
                {"key": "degraded", "tier": "fallback", "health_score": 0.25, "reasons": [null]}
            ]
        }),
    );
    assert_eq!(
        summarize_route_health_details(&path).as_deref(),
        Some(
            "route_health_trace=unmeasured tier=unknown score=0.00 reasons=no_reasons ; \
             degraded tier=fallback score=0.25 reasons=no_reasons ; \
             slow tier=standard score=0.40 reasons=timeout|rate_limit"
        )
    );
}

#[test]
fn route_state_paths_follow_the_hermes_home_override() {
    const CHILD_HOME: &str = "HERMES_COVERAGE_EVOLVE_AND_OPS_TEST_HOME";
    const VERIFIED: &str = "route state paths verified";

    if let Some(home) = std::env::var_os(CHILD_HOME) {
        let home = PathBuf::from(home);
        assert_eq!(
            route_learning_state_path(),
            home.join("route-learning.json")
        );
        assert_eq!(route_health_state_path(), home.join("route-health.json"));
        assert_eq!(
            route_autotune_state_path(),
            home.join("route-autotune.json")
        );
        assert_eq!(route_autotune_env_path(), home.join("route-autotune.env"));
        println!("{VERIFIED}");
        return;
    }

    // Set the override only in the child, so parallel tests keep their own environment.
    let dir = tempfile::tempdir().expect("create isolated home directory");
    let home = dir.path().join("hermes");
    std::fs::create_dir(&home).expect("create Hermes home");
    let test = harness_test_name("route_state_paths_follow_the_hermes_home_override");
    let output = std::process::Command::new(std::env::current_exe().expect("locate test binary"))
        .args(["--exact", test.as_str(), "--nocapture", "--test-threads=1"])
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("USERPROFILE", dir.path())
        .env("HERMES_HOME", &home)
        .env(CHILD_HOME, &home)
        .output()
        .expect("run isolated path assertions");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(VERIFIED),
        "isolated test {}:\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn current_command_separates_stdout_from_stderr() {
    let test =
        harness_test_name("route_health_summary_formats_report_and_distinguishes_missing_data");
    let rendered = run_current_hermes_cli_command(&[
        "--exact",
        test.as_str(),
        "--nocapture",
        "--test-threads=1",
    ])
    .await
    .expect("run the local test fixture");

    let (stdout, stderr) = rendered
        .split_once("\n\nstderr:\n")
        .expect("stderr should be labeled and separated from stdout");
    assert!(stdout.contains("route_health=degraded score=0.88 @ 2026-02-01T12:00:00Z"));
    assert!(!stdout.starts_with("(exit: "));
    assert_eq!(stderr, "route_health=unknown");
}

#[tokio::test]
async fn current_command_reports_status_when_both_streams_are_empty() {
    let absent = harness_test_name("no_such_coverage_test");
    let rendered = run_current_hermes_cli_command(&[
        "--list",
        "--format",
        "terse",
        "--exact",
        absent.as_str(),
    ])
    .await
    .expect("list zero matching tests");

    assert!(
        rendered.starts_with("(exit: ") && rendered.ends_with(')'),
        "expected only an exit status, got {rendered:?}"
    );
    assert_eq!(rendered.lines().count(), 1);
}

#[tokio::test]
async fn current_command_returns_io_error_when_process_cannot_be_spawned() {
    let error = run_current_hermes_cli_command(&["invalid\0argument"])
        .await
        .expect_err("an argument containing NUL must prevent spawning");
    assert!(
        matches!(&error, AgentError::Io(message) if message.starts_with("run current hermes command failed: ")),
        "unexpected error: {error:?}"
    );
}
