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

fn isolate_git(command: &mut Command, root: &Path) {
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
}

fn fixture_git(root: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    isolate_git(&mut command, root);
    let output = command.args(args).output().unwrap();
    let stdout = std::str::from_utf8(&output.stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert_exit(output, 0, &format!("fixture git {args:?}"));
    stdout
}

fn fixture_commit(root: &Path, message: &str) {
    fixture_git(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            message,
        ],
    );
}

#[cfg(unix)]
#[test]
fn fork_carry_reports_tests_that_run_and_keeps_declared_gaps_visible() {
    let fixture = tempfile::tempdir_in("/tmp").unwrap();
    let root = fixture.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"carry-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        root.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"carry-fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn draft_title(input: &str) -> &str {
    input.trim()
}

#[cfg(test)]
mod tests {
    #[test]
    fn keeps_draft() {
        assert_eq!(super::draft_title(" Outline "), "Outline");
    }
}
"#,
    )
    .unwrap();
    fixture_git(root, &["init", "--quiet", "--template="]);
    fixture_git(
        root,
        &["add", "--", "Cargo.toml", "Cargo.lock", "src/lib.rs"],
    );
    fixture_commit(root, "Fork carry fixture");

    fs::create_dir_all(root.join("docs/parity")).unwrap();
    let registry = root.join("docs/parity/fork-carry-contracts.v1.json");
    let cargo = Path::new(env!("CARGO"));
    for (test, cargo_path, code, expected) in [
        (
            "tests::keeps_draft",
            cargo.to_path_buf(),
            0,
            "- PASS carry-draft",
        ),
        (
            "tests::missing_draft",
            cargo.to_path_buf(),
            1,
            "exact test missing",
        ),
        (
            "tests::keeps_draft",
            root.join("missing-cargo"),
            1,
            "could not launch Cargo",
        ),
    ] {
        let document = json!({
            "version": 1,
            "required": [{
                "id": "carry-draft", "behaviour": "Keeps the draft",
                "package": "carry-fixture", "test": test
            }],
            "uncovered": [{"id": "carry-preview", "behaviour": "Preview still needs a test"}]
        });
        fs::write(&registry, document.to_string()).unwrap();
        let mut command = parity_tool(root, "check-fork-carry");
        isolate_git(&mut command, root);
        let output = command
            .env("CARGO", cargo_path)
            .env("CARGO_HOME", root.join("cargo-home"))
            .env("CARGO_NET_OFFLINE", "true")
            .env("RUSTC", cargo.with_file_name("rustc"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert_exit(output, code, expected);
        assert!(stdout.contains(expected), "{stdout}");
        assert!(stdout.contains("- UNCOVERED carry-preview: Preview still needs a test"));
        let passed = i32::from(code == 0);
        assert!(stdout.contains(&format!(
            "{passed}/1 required contracts passed. 1 known gaps."
        )));
    }
}

#[test]
fn workstream_report_counts_source_evidence_and_marks_deferred_work() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let git = |args: &[&str]| fixture_git(root, args);
    let write = |name: &str, content: &str| {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    };
    git(&["init", "--quiet", "--template="]);
    write("skills/fixture/SKILL.md", "Synthetic skill");
    write(
        "optional-skills/fixture/SKILL.md",
        "Synthetic optional skill",
    );
    git(&["add", "--", "skills", "optional-skills"]);
    fixture_commit(root, "Fixture upstream");
    let upstream = git(&["rev-parse", "HEAD"]);
    write("skills/local/SKILL.md", "Synthetic local skill");
    git(&["add", "--", "skills/local"]);
    fixture_commit(root, "Fixture local");
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
    isolate_git(&mut command, root);
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
fn divergence_gate_reports_local_paths_and_distinguishes_warnings_from_errors() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    fixture_git(root, &["init", "--quiet", "--template="]);
    fs::write(root.join("sample.rs"), "// fixture").unwrap();
    fixture_git(root, &["add", "--", "sample.rs"]);
    fixture_commit(root, "Fixture paths");

    let registry = root.join("registry.json");
    let report_path = root.join("reports/divergence.json");
    let invoke = |flags: &[&str]| {
        let mut command = parity_tool(root, "validate-intentional-divergence");
        isolate_git(&mut command, root);
        command
            .args(["--no-fetch", "--upstream-ref", "HEAD", "--upstream-url"])
            .arg(root)
            .args(["--divergence-file", "registry.json", "--output-json"])
            .arg(&report_path)
            .args(flags)
            .output()
            .unwrap()
    };
    let reviewed = json!({
        "id": "active",
        "status": "approved",
        "workstream": "fixture",
        "summary": "A tracked path",
        "owner": " Fixture ",
        "ticket": 1,
        "last_reviewed": "2000-01-01",
        "review_date": "9999-12-31",
        "rationale": "Synthetic divergence evidence",
        "path_prefixes": ["sample.rs"]
    });
    let mut overdue = reviewed.clone();
    overdue["id"] = json!("overdue");
    overdue["review_date"] = json!("2000-01-01");
    overdue["path_prefixes"] = json!(["missing"]);
    fs::write(&registry, json!({"items": [reviewed, overdue]}).to_string()).unwrap();
    assert_exit(invoke(&["--check"]), 1, "warnings fail strict check");
    assert_exit(
        invoke(&["--check", "--allow-warnings"]),
        0,
        "warning allowance",
    );
    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["divergence_file"], "registry.json");
    assert_eq!(
        report["summary"],
        json!({
            "items": 2, "errors": 0, "warnings": 2, "unowned": 0,
            "review_overdue": 1, "local_paths": 1, "upstream_paths": 1
        })
    );
    assert_eq!(report["items"][0]["owner"], "Fixture");
    assert_eq!(report["items"][0]["matched_sample"], json!(["sample.rs"]));
    assert_eq!(report["items"][1]["overdue"], true);

    fs::write(&registry, r#"{"items":[{"owner":""}]}"#).unwrap();
    assert_exit(
        invoke(&["--check", "--allow-warnings"]),
        1,
        "warning allowance still rejects errors",
    );
    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["summary"]["unowned"], 1);
    assert!(report["summary"]["errors"].as_u64().unwrap() > 0);

    let previous = fs::read(&report_path).unwrap();
    for (input, diagnostic) in [
        ("{", "invalid JSON"),
        ("[]", "invalid items payload"),
        (r#"{"items":0}"#, "invalid items payload"),
        (r#"{"items":[null]}"#, "registry item is not an object"),
    ] {
        fs::write(&registry, input).unwrap();
        let output = invoke(&["--check"]);
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
        assert_exit(output, 1, diagnostic);
        assert_eq!(fs::read(&report_path).unwrap(), previous);
    }
}

#[test]
fn test_intent_report_marks_mapped_and_missing_evidence_in_both_formats() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let evidence = "crates/hermes-gateway/src/platforms/fixture.rs";
    let path = root.join(evidence);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "// fixture adapter").unwrap();
    assert_exit(
        parity_tool(root, "generate-test-intent-mapping")
            .args([
                "--out-json",
                "report/mapping.json",
                "--out-md",
                "report/mapping.md",
            ])
            .output()
            .unwrap(),
        0,
        "render test intent mapping reports",
    );
    let report: Value =
        serde_json::from_slice(&fs::read(root.join("report/mapping.json")).unwrap()).unwrap();
    let intents = report["intents"].as_array().unwrap();
    let row = |id: &str| intents.iter().find(|entry| entry["id"] == id).unwrap();
    let mapped = row("gateway-platform-behavior");
    assert_eq!(mapped["mapped"], true);
    assert_eq!(mapped["evidence_count"], 1);
    assert_eq!(mapped["evidence_sample"], json!([evidence]));
    let missing = row("tool-runtime-behavior");
    assert_eq!(missing["mapped"], false);
    assert_eq!(missing["evidence_sample"], json!([]));
    assert_eq!(report["summary"]["mapped_intents"], 1);
    assert_eq!(report["summary"]["unmapped_intents"], intents.len() - 1);
    let markdown = fs::read_to_string(root.join("report/mapping.md")).unwrap();
    assert!(markdown.contains("| `gateway-platform-behavior` | yes | 1 |"));
    assert!(markdown.contains("| `tool-runtime-behavior` | no | 0 |"));
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
fn deep_proof_reports_missing_references_and_unmet_thresholds() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let source = "tests/fixture.rs";
    fs::create_dir(root.join("tests")).unwrap();
    fs::write(root.join(source), "#[test]\nfn actual_test() {}\n").unwrap();
    let reference = json!({"file": source, "name": "absent_test"});
    fs::write(
        root.join("cases.json"),
        json!({"cases": [{
            "id": "fixture-case",
            "domain": "fixture",
            "classification": "invalid",
            "upstream_behavior": "Synthetic input.",
            "ultra_behavior": "Synthetic output.",
            "why": "Exercise proof validation.",
            "source_artifacts": [source, "notes/absent.txt"],
            "rust_tests": [reference.clone()]
        }]})
        .to_string(),
    )
    .unwrap();
    let run = |check: bool| {
        let mut tool = parity_tool(root, "generate-deep-problem-solving-diff");
        tool.current_dir(root).args([
            "--cases",
            "cases.json",
            "--out-json",
            "reports/result.json",
            "--out-md",
            "reports/result.md",
            "--min-total-cases",
            "2",
            "--min-superiority-cases",
            "1",
        ]);
        if check {
            tool.arg("--check");
        }
        tool.output().unwrap()
    };

    assert_exit(run(true), 1, "check rejects incomplete proof");
    assert!(!root.join("reports").exists());
    assert_exit(run(false), 0, "generate diagnostics for incomplete proof");
    let output = fs::read(root.join("reports/result.json")).unwrap();
    let report: Value = serde_json::from_slice(&output).unwrap();
    let case = &report["cases"][0];
    assert_eq!(case["verified"], false);
    assert_eq!(case["missing_rust_test_refs"], json!([reference]));
    assert_eq!(
        case["missing_source_artifacts"],
        json!(["notes/absent.txt"])
    );
    assert_eq!(case["issues"][0]["kind"], "schema");
    assert_eq!(report["gate"]["pass"], false);
    let failures: Vec<_> = report["gate_failures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|failure| failure["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        failures,
        [
            "total_cases",
            "deep_problem_solving_ratio",
            "superiority_cases",
            "unverified",
            "missing_rust_test_refs",
            "missing_source_artifacts",
        ]
    );
    assert!(root.join("reports/result.md").is_file());
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

#[test]
fn gpar_closure_reports_totals_for_only_scoped_tickets() {
    let root = tempfile::tempdir().unwrap();
    let parity = root.path().join("docs/parity");
    fs::create_dir_all(&parity).unwrap();
    let queue = parity.join("upstream-missing-queue.json");
    let invoke = || {
        parity_tool(root.path(), "generate-gpar-01-04-proof")
            .output()
            .unwrap()
    };
    fs::write(
        &queue,
        json!({"commits": [
            {"target_ticket": 20, "disposition": "ported"},
            {"target_ticket": "21", "disposition": "superseded"},
            {"target_ticket": 22.9},
            {"target_ticket": false},
            {"target_ticket": 24},
            {}
        ]})
        .to_string(),
    )
    .unwrap();
    assert_exit(invoke(), 0, "render scoped closure proof");
    let report_path = parity.join("gpar-01-04-proof.json");
    let report = fs::read(&report_path).unwrap();
    let proof: Value = serde_json::from_slice(&report).unwrap();
    assert_eq!(proof["scope"]["tickets"], json!([20, 21, 22, 23]));
    assert_eq!(
        proof["summary"],
        json!({
            "total_commits": 3, "by_disposition": {"pending": 1, "ported": 1, "superseded": 1},
            "total_pending": 1, "pass": false
        })
    );
    assert_eq!(
        proof["ticket_breakdown"]["21"]["by_disposition"]["superseded"],
        1
    );
    assert_eq!(proof["ticket_breakdown"]["22"]["pending"], 1);
    assert_eq!(proof["ticket_breakdown"]["23"]["total"], 0);
    let markdown = fs::read_to_string(parity.join("gpar-01-04-proof.md")).unwrap();
    assert!(markdown.contains("- Gate: `FAIL`"));
    assert!(markdown.contains("| #22 | GPAR-03 UX parity | 1 | 1 | 0 | 0 |"));

    for (input, diagnostic) in [
        ("{}", "upstream queue commits must be an array"),
        (
            r#"{"commits":[false]}"#,
            "upstream queue commit must be an object",
        ),
        (
            r#"{"commits":[{"target_ticket":{}}]}"#,
            "target_ticket must be an integer",
        ),
    ] {
        fs::write(&queue, input).unwrap();
        let output = invoke();
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
        assert_exit(output, 1, diagnostic);
        assert_eq!(fs::read(&report_path).unwrap(), report);
    }
}
