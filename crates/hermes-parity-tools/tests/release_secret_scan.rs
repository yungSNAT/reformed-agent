use std::fs;
use std::process::Command;

#[test]
fn release_scan_preserves_locations_and_redacts_both_outputs() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    let marker = concat!("-----BEGIN ", "PRIVATE KEY-----");
    fs::write(root.join("sample.txt"), format!("ordinary\n{marker}\n")).unwrap();
    for args in [&["init", "-q"][..], &["add", "--", "sample.txt"][..]] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }

    let report_path = ".sync-reports/scan.json";
    let output = Command::new(env!("CARGO_BIN_EXE_hermes-parity-tools"))
        .args(["release_secret_scan", "--report", report_path])
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        "[release-secret-scan] FAILED: potential secrets detected"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        "- sample.txt:2 [private_key_block] [redacted]"
    );
    let bytes = fs::read(root.join(report_path)).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(report["total_findings"], 1);
    assert_eq!(
        report["findings"],
        serde_json::json!([{
            "file": "sample.txt",
            "line": 2,
            "pattern": "private_key_block",
            "snippet": "[redacted]"
        }])
    );
}
