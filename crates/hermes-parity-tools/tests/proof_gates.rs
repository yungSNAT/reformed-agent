use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{json, Value};

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
fn workstream_report_counts_source_evidence_and_marks_deferred_work() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let isolate = |command: &mut Command| {
        command
            .env_clear()
            .env("HOME", root)
            .env("USERPROFILE", root)
            .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .current_dir(root);
        for key in ["PATH", "SystemRoot", "LLVM_PROFILE_FILE"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
    };
    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        isolate(&mut command);
        let output = command.args(args).output().unwrap();
        let stdout = std::str::from_utf8(&output.stdout)
            .unwrap()
            .trim()
            .to_owned();
        assert_exit(output, 0, &format!("fixture git {args:?}"));
        stdout
    };
    let write = |name: &str, content: &str| {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    };
    let commit = |message: &str| {
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            message,
        ]);
    };
    git(&["init", "--quiet", "--template="]);
    write("skills/fixture/SKILL.md", "Synthetic skill");
    write(
        "optional-skills/fixture/SKILL.md",
        "Synthetic optional skill",
    );
    git(&["add", "--", "skills", "optional-skills"]);
    commit("Fixture upstream");
    let upstream = git(&["rev-parse", "HEAD"]);
    write("skills/local/SKILL.md", "Synthetic local skill");
    git(&["add", "--", "skills/local"]);
    commit("Fixture local");
    let local = git(&["rev-parse", "HEAD"]);
    assert_ne!(local, upstream);

    let wiring = "wire_cron_scheduler_backend(&tool_registry);";
    for file in ["src/main.rs", "src/app.rs", "src/commands/cron.rs"] {
        write(&format!("crates/hermes-cli/{file}"), wiring);
    }
    write(
        "crates/hermes-cli/src/runtime_tool_wiring.rs",
        "pub fn wire_gateway_messaging_backend() {}\npub fn wire_gateway_clarify_backend() {}\npub fn wire_stdio_clarify_backend() {}",
    );
    write(
        "crates/hermes-cli/tests/e2e_cli.rs",
        "fn e2e_fixture() {}\nfn e2e_second() {}",
    );
    write(
        ".github/workflows/ci.yml",
        "tests:\r\n  runs-on: fixture\r\n  steps:\r\n    - cargo test --workspace\r\n    - cargo test -p hermes-parity-tests\r\n    - check-runtime-placeholders.sh\r\n    - clippy-warning-gate.sh\r\n",
    );
    for name in [
        "upstream_webhook_sync.py",
        "setup-upstream-webhook-launchd.sh",
        "sync-upstream.sh",
    ] {
        write(&format!("scripts/{name}"), "fixture");
    }
    write(
        "scripts/install-upstream-webhook-launchd.sh",
        "# Reads GITHUB_WEBHOOK_SECRET",
    );
    write("docs/parity/compatibility-policy.md", "Synthetic policy");
    write(
        "docs/parity/intentional-divergence.json",
        r#"{"items":[{"path_prefixes":["skills/","optional-skills/"]},{"path_prefixes":["web/","ui-tui/"]}]}"#,
    );
    write(
        "docs/parity/shared-different-classification.json",
        r#"{"items":[{"classification":"deferred","path":"skills/fixture/SKILL.md"}]}"#,
    );
    for name in [
        "python-test-suite-coverage",
        "hermes-cli-test-coverage",
        "ui-tui-source-coverage",
    ] {
        write(&format!("docs/parity/{name}.json"), r#"{"entries":[]}"#);
    }

    let mut command = parity_tool(root, "generate-workstream-status");
    isolate(&mut command);
    assert_exit(
        command
            .args(["--no-fetch", "--upstream-ref", "HEAD^", "--upstream-url"])
            .arg(root.join("unused-upstream"))
            .output()
            .unwrap(),
        0,
        "generate isolated workstream reports",
    );
    let report: Value =
        serde_json::from_slice(&fs::read(root.join("docs/parity/workstream-status.json")).unwrap())
            .unwrap();
    assert_eq!(report["head_sha"], local);
    assert_eq!(report["upstream_ref"], "HEAD^");
    assert_eq!(report["upstream_sha"], upstream);
    assert_eq!(report["states"], json!({"complete":6,"in_progress":1}));
    let streams = report["workstreams"].as_array().unwrap();
    let get = |id: &str| {
        streams
            .iter()
            .find(|entry| entry["workstream"] == id)
            .unwrap()
    };
    assert_eq!(get("WS2")["metrics"]["wiring_sites_detected"], 3);
    assert_eq!(get("WS3")["metrics"]["runtime_wiring_functions"], 3);
    assert_eq!(get("WS4")["state"], "in_progress");
    assert_eq!(
        get("WS4")["metrics"],
        json!({
            "upstream_skill_files": 2,
            "local_skill_files": 3,
            "divergence_documented": true,
            "rows_not_shown_matching": 1,
        })
    );
    assert_eq!(get("WS5")["metrics"]["e2e_cli_tests"], 2);
    assert_eq!(get("WS6")["metrics"]["ci_jobs"], 1);
    let markdown = fs::read_to_string(root.join("docs/parity/workstream-status.md")).unwrap();
    assert!(markdown.contains(&format!("- Local HEAD: `{local}`")));
    assert!(markdown.contains(&format!("- Upstream: `HEAD^` (`{upstream}`)")));
    assert!(markdown.contains("| `WS4` | Skills parity | **in_progress** |"));
    assert!(markdown.contains("rows are deferred or not rechecked"));
}

#[test]
fn adapter_matrix_reports_only_feature_gated_platforms_and_public_memory_plugins() {
    let root = tempfile::tempdir().unwrap();
    for (relative, source) in [
        (
            "crates/hermes-gateway/src/platforms/mod.rs",
            "#[cfg(feature = \"second\")]\r\npub mod zeta;\r\n\
             #[cfg(feature = \"first\")] pub mod alpha;\r\npub mod always_on;\r\n",
        ),
        (
            "crates/hermes-agent/src/memory_plugins/mod.rs",
            "pub mod snapshot;\r\nmod private_plugin;\r\n",
        ),
    ] {
        let path = root.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    assert_exit(
        parity_tool(root.path(), "generate-adapter-matrix")
            .args(["--out-json", "matrix.json", "--out-md", "matrix.md"])
            .output()
            .unwrap(),
        0,
        "render adapter feature matrix",
    );
    let report: Value =
        serde_json::from_slice(&fs::read(root.path().join("matrix.json")).unwrap()).unwrap();
    assert_eq!(report["summary"]["platform_adapters"], 2);
    assert_eq!(report["summary"]["memory_plugins"], 1);
    assert_eq!(report["summary"]["non_rust_native"], 0);
    let markdown = fs::read_to_string(root.path().join("matrix.md")).unwrap();
    let rows = report["items"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    for (row, (category, name, feature)) in rows.iter().zip([
        ("memory_plugin", "snapshot", "always"),
        ("platform_adapter", "alpha", "first"),
        ("platform_adapter", "zeta", "second"),
    ]) {
        assert_eq!(
            row,
            &json!({"category": category, "name": name, "feature": feature, "status": "rust-native"})
        );
        assert!(markdown.contains(&format!(
            "| {category} | `{name}` | `{feature}` | rust-native |"
        )));
    }
}

#[test]
fn global_proof_reports_overdue_evidence_and_rejects_release_without_rewriting_reports() {
    let root = tempfile::tempdir().unwrap();
    let parity = root.path().join("docs/parity");
    fs::create_dir_all(&parity).unwrap();
    for (name, report) in [
        (
            "global-parity-thresholds",
            json!({
                "program": "fixture parity",
                "ci_thresholds": {"min_test_intent_mapping_ratio": 0.9},
                "release_thresholds": {
                    "max_deferred_review_overdue": 0,
                    "required_workstreams_complete": ["GPAR-01", "GPAR-04"],
                },
            }),
        ),
        (
            "parity-matrix",
            json!({"summary": {}, "top_shared_different": [{"path": "fixture.rs"}]}),
        ),
        (
            "workstream-status",
            json!({"states": {"fixture": "complete"}}),
        ),
        (
            "test-intent-mapping",
            json!({"summary": {"mapping_ratio": 1.0}}),
        ),
        (
            "adapter-feature-matrix",
            json!({"summary": {"non_rust_native": 0}}),
        ),
        (
            "shared-different-classification",
            json!({"items": [{
                "path": "fixture.rs", "classification": "deferred", "review_date": "2000-01-01"
            }]}),
        ),
        (
            "shared-diff-backlog",
            json!({"summary": {"deferred_review": 1}}),
        ),
        (
            "divergence-validation",
            json!({"summary": {"errors": 0, "unowned": 0, "review_overdue": 0}}),
        ),
        (
            "upstream-missing-queue",
            json!({"summary": {
                "total_commits": 2, "by_disposition": {"ported": 2}, "by_target_ticket": {"12": 2}
            }}),
        ),
        (
            "test-coverage-audit",
            json!({
                "summary": {
                    "tracked_behavior_rows": 5,
                    "covered_behavior_rows": 3,
                    "coverage_manifest_deferred_entries": 1,
                    "coverage_manifest_not_rechecked_entries": 1,
                    "tracked_behavior_coverage_ratio": 0.6,
                },
                "audit_gate": {"pass": true, "critical_gaps": 0},
                "coverage_manifests": [{"path": "docs/parity/fixture-manifest.json"}],
            }),
        ),
        (
            "fixture-manifest",
            json!({"entries": [
                {"status": "deferred_not_covered", "review_date": "2000-01-01"},
                {"status": "covered"},
            ]}),
        ),
        (
            "sota-harness-matrix",
            json!({"summary": {"domain_coverage_ratio": 1.0}, "gate": {"pass": true}}),
        ),
        (
            "behavioral-similarity-diff",
            json!({"summary": {}, "gate": {"pass": false}}),
        ),
        (
            "deep-problem-solving-diff",
            json!({"summary": {}, "gate": {"pass": true}}),
        ),
    ] {
        fs::write(parity.join(format!("{name}.json")), report.to_string()).unwrap();
    }
    let run = |args: &[&str]| {
        parity_tool(root.path(), "generate-global-parity-proof")
            .args(args)
            .output()
            .unwrap()
    };
    let json_path = parity.join("global-parity-proof.json");
    let markdown_path = parity.join("global-parity-proof.md");
    assert_exit(run(&["--check-ci"]), 0, "CI threshold passes");
    assert_exit(
        run(&["--check-release"]),
        1,
        "overdue fixture evidence fails release",
    );
    assert!(!json_path.exists() && !markdown_path.exists());
    assert_exit(run(&[]), 0, "generate proof reports");
    let raw_json = fs::read(&json_path).unwrap();
    let markdown = fs::read_to_string(&markdown_path).unwrap();
    let report: Value = serde_json::from_slice(&raw_json).unwrap();
    assert_eq!(report["ci_gate"]["pass"], true);
    assert_eq!(report["release_gate"]["pass"], false);
    assert_eq!(
        report["metrics"]["max_deferred_review_overdue"].as_f64(),
        Some(2.0)
    );
    assert_eq!(
        report["metrics"]["max_test_coverage_unaccounted_rows"].as_f64(),
        Some(0.0)
    );
    for index in ["GPAR-01", "GPAR-04", "GPAR-06", "GPAR-09", "GPAR-11"] {
        assert_eq!(report["gpar_completion"][index], true, "{index}");
    }
    assert_eq!(report["gpar_completion"]["GPAR-10"], false);
    assert!(markdown.contains("- CI gate: **PASS**"));
    assert!(markdown.contains("- Release gate: **FAIL**"));
    assert!(markdown.contains("| `GPAR-06` | yes |"));
    assert!(markdown.contains("| `GPAR-10` | no |"));
    assert_exit(
        run(&["--check-release"]),
        1,
        "check keeps both generated reports",
    );
    assert_eq!(fs::read(json_path).unwrap(), raw_json);
    assert_eq!(fs::read_to_string(markdown_path).unwrap(), markdown);
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
