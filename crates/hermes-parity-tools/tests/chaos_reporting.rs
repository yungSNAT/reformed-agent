#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn run_cli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hermes-parity-tools"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

#[test]
fn chaos_harness_preserves_failure_details_with_and_without_metrics() {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("reporter.sh");
    let path = tmp.path().join("harness.json");
    for (body, excerpt, runs) in [
        (
            "printf '%s\\n' 'fixture failed before recording metrics' >&2",
            "fixture failed before recording metrics",
            serde_json::json!([]),
        ),
        (
            "printf '%s\\n' 'adapter chaos harness results: invalid'",
            "adapter chaos harness results: invalid",
            serde_json::json!([]),
        ),
        (
            r#"printf '%s\n' 'adapter chaos harness results: [null,{"scenario":"retry-queue","actual":{"outcome":"error"}}]'
printf '%s\n' 'Adapter chaos harness mismatches:' 'fixture reply mismatch' >&2"#,
            "Adapter chaos harness mismatches:\nfixture reply mismatch",
            serde_json::json!([{"scenario":"retry-queue","actual":{"outcome":"error"}}]),
        ),
    ] {
        fs::write(&script, format!("#!/bin/sh\n{body}\nexit 7\n")).unwrap();
        let result = run_cli(
            tmp.path(),
            &[
                "run-adapter-chaos-harness",
                "--command",
                "/bin/sh ./reporter.sh",
                "--output",
                "harness.json",
            ],
        );
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        let report: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(report["passed"], false);
        assert_eq!(report["exit_code"], 7);
        assert_eq!(report["failure_excerpt"], excerpt);
        assert_eq!(report["scenario_runs"], runs);
    }
}

#[test]
fn chaos_compare_uses_the_latest_sample_and_reports_missing_metrics() {
    let tmp = tempfile::tempdir().unwrap();
    let baseline = r#"{"passed":true,"scenario_runs":[null,{},
        {"scenario":" retry-queue ","actual":{"attempts":1,"outcome":"success"}},
        {"scenario":"retry-queue","actual":{"attempts":3,"outcome":"success"}}]}"#;
    fs::write(tmp.path().join("baseline.json"), baseline).unwrap();
    let current = tmp.path().join("current.json");
    fs::write(
        &current,
        r#"{"passed":true,"scenario_runs":[{"scenario":"retry-queue","actual":{"attempts":2,"outcome":"success"}}]}"#,
    )
    .unwrap();
    let compare = |extra: &[&str]| {
        let mut args = vec![
            "compare-adapter-chaos-reports",
            "--baseline",
            "baseline.json",
            "--current",
            "current.json",
        ];
        args.extend_from_slice(extra);
        run_cli(tmp.path(), &args)
    };
    let passed = compare(&["--json"]);
    assert_eq!(passed.status.code(), Some(0), "{passed:?}");
    let report: Value = serde_json::from_slice(&passed.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["regressions"], serde_json::json!([]));

    fs::write(&current, r#"{"passed":false,"scenario_runs":[]}"#).unwrap();
    let failed = compare(&["--output", "comparison.json"]);
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    let output = String::from_utf8(failed.stdout).unwrap();
    assert!(output.contains("FAILED (regressions=3)"), "{output}");
    let path = tmp.path().join("comparison.json");
    let saved = fs::read(&path).unwrap();
    let report: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(report["ok"], false);
    let regressions = report["regressions"].as_array().unwrap();
    assert_eq!(regressions[0]["type"], "missing_current_scenario_metrics");
    assert_eq!(regressions[1]["type"], "scenario_missing");
    assert_eq!(regressions[1]["scenario"], "retry-queue");
    assert_eq!(regressions[2]["type"], "overall_pass_regression");

    for (contents, error) in [
        ("[", "failed to parse report"),
        ("[]", "must be a JSON object"),
    ] {
        fs::write(&current, contents).unwrap();
        let invalid = compare(&["--output", "comparison.json"]);
        assert!(!invalid.status.success());
        let stderr = String::from_utf8(invalid.stderr).unwrap();
        assert!(stderr.contains(error), "{stderr}");
        assert_eq!(fs::read(&path).unwrap(), saved);
    }
}
