use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

fn parity_tool(root: &Path, command: &str) -> Command {
    let mut tool = Command::new(env!("CARGO_BIN_EXE_hermes-parity-tools"));
    tool.args([command, "--repo-root"]).arg(root);
    tool
}

fn assert_exit(output: Output, code: i32, context: &str) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{context}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn case_proofs_reject_superior_cases_when_required_evidence_is_missing() {
    let root = tempfile::tempdir().unwrap();
    let source = "crates/example/src/lib.rs";
    fs::create_dir_all(root.path().join("crates/example/src")).unwrap();
    fs::write(root.path().join(source), "#[test]\nfn proof() {}\n").unwrap();
    let manifest = root.path().join("cases.json");
    let case = json!({
        "id": "sample",
        "domain": "runtime",
        "classification": "superior",
        "upstream_behavior": "Input is accepted.",
        "ultra_behavior": "Input is accepted and validated.",
        "why": "Validation explains rejected input.",
        "source_artifacts": [source],
        "rust_tests": [{"file": source, "name": "proof"}]
    });
    for command in [
        "generate-deep-problem-solving-diff",
        "generate-behavioral-similarity-diff",
    ] {
        let invocation = || {
            let mut tool = parity_tool(root.path(), command);
            tool.args([
                "--cases",
                "cases.json",
                "--out-json",
                "report.json",
                "--out-md",
                "report.md",
                "--min-superiority-cases",
                "1",
            ]);
            if command == "generate-deep-problem-solving-diff" {
                tool.args(["--min-total-cases", "1"]);
            }
            tool
        };
        fs::write(&manifest, json!({"cases": [&case]}).to_string()).unwrap();
        assert_exit(invocation().arg("--check").output().unwrap(), 0, command);
        for (field, missing_value) in [
            ("why", json!("")),
            ("source_artifacts", json!([])),
            ("rust_tests", json!([])),
        ] {
            let mut incomplete = case.clone();
            incomplete[field] = missing_value;
            fs::write(&manifest, json!({"cases": [incomplete]}).to_string()).unwrap();
            assert_exit(
                invocation().arg("--check").output().unwrap(),
                1,
                &format!("{command}: missing {field}"),
            );

            let report = root.path().join("report.json");
            let markdown = root.path().join("report.md");
            assert!(!report.exists() && !markdown.exists());
            assert_exit(invocation().output().unwrap(), 0, command);
            let evidence: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
            assert_eq!(evidence["cases"][0]["verified"], false);
            assert_eq!(evidence["gate"]["pass"], false);
            assert_eq!(evidence["summary"]["unverified_cases"], 1);
            assert_eq!(evidence["gate_failures"][0]["kind"], "unverified");
            fs::remove_file(report).unwrap();
            fs::remove_file(markdown).unwrap();
        }
    }
}

#[test]
fn gpar_closure_check_exits_for_pending_work_without_writing_reports() {
    let root = tempfile::tempdir().unwrap();
    let parity = root.path().join("docs/parity");
    fs::create_dir_all(&parity).unwrap();
    for (disposition, exit_code) in [("pending", 1), ("ported", 0)] {
        fs::write(
            parity.join("upstream-missing-queue.json"),
            json!({"commits": [
                {"target_ticket": 20, "disposition": disposition},
                {"target_ticket": 24, "disposition": "pending"}
            ]})
            .to_string(),
        )
        .unwrap();
        assert_exit(
            parity_tool(root.path(), "generate-gpar-01-04-proof")
                .arg("--check")
                .output()
                .unwrap(),
            exit_code,
            disposition,
        );
        assert!(!parity.join("gpar-01-04-proof.json").exists());
        assert!(!parity.join("gpar-01-04-proof.md").exists());
    }
}
