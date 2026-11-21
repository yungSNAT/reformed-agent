#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn failed_section_preserves_the_report_and_runs_the_other_checks() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let cargo = root.join("cargo");
    fs::write(
        &cargo,
        concat!(
            "#!/bin/sh\n",
            "test \"$*\" = 'metadata --format-version 1 --locked' || exit 82\n",
            "printf '%s\\n' '{\"packages\":[{\"name\":\"fixture\"}],\"workspace_members\":[]}'\n",
        ),
    )
    .unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700)).unwrap();
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(root.to_path_buf()).chain(std::env::split_paths(&existing_path)),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hermes-parity-tools"))
        .args([
            "run-security-release-gate-v2",
            "--secret-scan-cmd",
            "printf 'scan passed\\n'",
            "--signature-tests-cmd",
            "printf 'failure\\n' >&2; exit 7",
            "--redaction-tests-cmd",
            "printf 'redaction passed\\n'",
            "--multimodal-tests-cmd",
            "printf 'multimodal passed\\n'",
            "--report",
            "gate.json",
            "--json",
        ])
        .current_dir(root)
        .env("PATH", path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read(root.join("gate.json")).unwrap(), output.stdout);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
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
    let sbom: serde_json::Value = serde_json::from_slice(&inventory).unwrap();
    assert_eq!(sbom["packages"][0]["name"], "fixture");
    assert_eq!(sbom["package_count"], 1);
}
