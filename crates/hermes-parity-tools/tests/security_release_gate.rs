#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn write_cargo(root: &Path, metadata: &str) {
    let cargo = root.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\ntest \"$*\" = 'metadata --format-version 1 --locked' || exit 82\nprintf '%s\\n' '{metadata}'\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700)).unwrap();
}

fn run_gate(root: &Path, args: &[&str], use_system_path: bool) -> Value {
    let system_path = if use_system_path {
        std::env::var_os("PATH").unwrap_or_default()
    } else {
        Default::default()
    };
    let path = std::env::join_paths(
        std::iter::once(root.to_path_buf())
            .chain(std::env::split_paths(&system_path).filter(|path| !path.as_os_str().is_empty())),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hermes-parity-tools"))
        .args([
            "run-security-release-gate-v2",
            "--secret-scan-cmd",
            "printf 'scan passed\\n'",
            "--signature-tests-cmd",
            "printf 'signature passed\\n'",
            "--redaction-tests-cmd",
            "printf 'redaction passed\\n'",
            "--multimodal-tests-cmd",
            "printf 'multimodal passed\\n'",
            "--report",
            "gate.json",
            "--json",
        ])
        .args(args)
        .current_dir(root)
        .env("PATH", path)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read(root.join("gate.json")).unwrap(), output.stdout);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    report
}

#[test]
fn failed_section_preserves_the_report_and_runs_the_other_checks() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    write_cargo(
        root,
        r#"{"packages":[{"name":"fixture"}],"workspace_members":[]}"#,
    );
    let report = run_gate(
        root,
        &["--signature-tests-cmd", "printf 'failure\\n' >&2; exit 7"],
        true,
    );
    assert_eq!(report["summary"]["passed_sections"], 4);
    assert_eq!(report["sections"]["signature_tests"]["exit_code"], 7);
    assert_eq!(
        report["sections"]["signature_tests"]["stderr_tail"],
        "failure\n"
    );
    assert_eq!(
        report["sections"]["multimodal_tests"]["stdout_tail"],
        "multimodal passed\n"
    );
    let inventory = fs::read(root.join(".sync-reports/release-sbom-metadata.json")).unwrap();
    let sbom: Value = serde_json::from_slice(&inventory).unwrap();
    assert_eq!(sbom["packages"][0]["name"], "fixture");
    assert_eq!(sbom["package_count"], 1);
}

#[test]
fn invalid_metadata_leaves_a_failed_sbom_section_and_runs_later_checks() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    write_cargo(root, r#"{"packages":[17]}"#);

    let report = run_gate(root, &[], true);

    assert_eq!(report["summary"]["passed_sections"], 4);
    let sbom = &report["sections"]["sbom"];
    assert_eq!(sbom["ok"], false);
    assert_eq!(sbom["exit_code"], 0);
    assert_eq!(
        sbom["error"],
        "cargo metadata: each package must be an object"
    );
    assert!(sbom["stdout_tail"]
        .as_str()
        .unwrap()
        .contains("\"packages\""));
    assert_eq!(report["sections"]["multimodal_tests"]["ok"], true);
    assert!(!root
        .join(".sync-reports/release-sbom-metadata.json")
        .exists());
}

#[test]
fn missing_shell_leaves_a_diagnostic_and_runs_independent_metadata() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    write_cargo(root, "{}");

    let report = run_gate(root, &[], false);

    assert_eq!(report["summary"]["passed_sections"], 1);
    let scan = &report["sections"]["secret_scan"];
    assert_eq!(scan["ok"], false);
    assert!(scan["exit_code"].is_null());
    assert!(scan["error"]
        .as_str()
        .unwrap()
        .contains("could not run command"));
    assert_eq!(report["sections"]["sbom"]["ok"], true);
    assert_eq!(report["sections"]["multimodal_tests"]["ok"], false);
}

#[test]
fn invalid_output_and_unwritable_inventory_leave_other_sections_intact() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    write_cargo(root, "{}");
    fs::write(root.join("blocked"), "regular file").unwrap();

    let report = run_gate(
        root,
        &[
            "--secret-scan-cmd",
            "printf '\\377'; printf 'scan stderr\\n' >&2; exit 9",
            "--sbom-output",
            "blocked/sbom.json",
        ],
        true,
    );

    assert_eq!(report["summary"]["passed_sections"], 3);
    let scan = &report["sections"]["secret_scan"];
    assert_eq!(scan["ok"], false);
    assert_eq!(scan["exit_code"], 9);
    assert_eq!(scan["stderr_tail"], "scan stderr\n");
    assert!(scan["error"]
        .as_str()
        .unwrap()
        .contains("could not decode command stdout"));
    let sbom = &report["sections"]["sbom"];
    assert_eq!(sbom["ok"], false);
    assert_eq!(sbom["exit_code"], 0);
    assert!(sbom["error"].as_str().unwrap().contains("blocked"));
    assert_eq!(report["sections"]["multimodal_tests"]["ok"], true);
}
