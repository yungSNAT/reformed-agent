//! The global `--profile` flag: it and `--config-dir` each choose the home, so giving both is
//! an error, and import-hermes-sessions, which writes to the folder --profile-dir names, refuses
//! them rather than ignoring them.

use assert_cmd::Command;

fn hermes(home: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("hermes").unwrap();
    command.env("HERMES_HOME", home);
    command
}

#[test]
fn profile_and_config_dir_together_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    let output = hermes(dir.path())
        .args(["--profile", "work", "--config-dir"])
        .arg(dir.path())
        .args(["profile", "list"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--profile and --config-dir"), "{stderr}");
}

#[test]
fn import_hermes_sessions_refuses_profile_rather_than_ignoring_it() {
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("export.jsonl");
    std::fs::write(&export, "").unwrap();
    let target = dir.path().join("imported");
    let output = hermes(dir.path())
        .args(["--profile", "work", "import-hermes-sessions"])
        .arg(&export)
        .arg("--profile-dir")
        .arg(&target)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--profile-dir"), "{stderr}");
    assert!(!target.exists(), "the import ran despite --profile");
}
