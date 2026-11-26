use assert_cmd::{cargo::cargo_bin, Command};
use hermes_agent::SessionPersistence;
use hermes_config::paths::gateway_pid_path_in;
use hermes_core::Message;
use std::{fs, path::Path, time::Duration};

fn hermes(home: &Path) -> Command {
    let managed = home.join("managed");
    fs::create_dir_all(&managed).unwrap();
    let mut cmd = Command::new(cargo_bin!("hermes"));
    cmd.env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("HERMES_HOME", home)
        .env("HERMES_MANAGED_DIR", managed)
        .current_dir(home)
        .timeout(Duration::from_secs(15));
    if let Some(value) = std::env::var_os("SystemRoot") {
        cmd.env("SystemRoot", value);
    }
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        let profile = std::path::PathBuf::from(profile);
        let profile = if profile.starts_with("%t") {
            profile
        } else {
            std::path::absolute(profile).unwrap()
        };
        cmd.env("LLVM_PROFILE_FILE", profile);
    }
    cmd
}

fn stdout(assertion: &assert_cmd::assert::Assert) -> &str {
    std::str::from_utf8(&assertion.get_output().stdout).unwrap()
}

#[test]
fn gateway_status_and_stop_remove_stale_metadata_but_keep_the_lock_file() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let pid = gateway_pid_path_in(home);
    let lock = home.join("gateway.lock");
    fs::write(&lock, "fixture lock").unwrap();

    let missing = hermes(home).args(["gateway", "status"]).assert().success();
    assert!(stdout(&missing).contains("not running (no PID file"));

    fs::write(&pid, "invalid fixture pid").unwrap();
    let invalid = hermes(home).args(["gateway", "status"]).assert().success();
    assert!(stdout(&invalid).contains("invalid PID file"));
    assert!(!pid.exists());

    fs::write(&pid, i32::MAX.to_string()).unwrap();
    let stale = hermes(home).args(["gateway", "status"]).assert().success();
    assert!(stdout(&stale).contains("stale metadata for PID"));
    assert!(!pid.exists());

    fs::write(&pid, i32::MAX.to_string()).unwrap();
    let stopped = hermes(home).args(["gateway", "stop"]).assert().success();
    assert!(stdout(&stopped).contains("removed the stale PID file"));
    assert!(!pid.exists());
    assert_eq!(fs::read_to_string(&lock).unwrap(), "fixture lock");
}

#[test]
fn gateway_start_prunes_expired_sessions_and_reports_incomplete_adapters() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    fs::write(
        home.join("config.yaml"),
        "sessions:\n  auto_prune: true\n  retention_days: 30\n  vacuum_after_prune: true\nplatforms:\n  qqbot:\n    enabled: true\n  wecom_callback:\n    enabled: true\n",
    )
    .unwrap();

    let persistence = SessionPersistence::new(home);
    for id in ["expired", "recent"] {
        persistence
            .persist_session(id, &[Message::user(id)], None, None, None, None)
            .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(persistence.db_path()).unwrap();
        let changed = conn
            .execute(
                "UPDATE sessions SET updated_at = '2000-01-01T00:00:00Z' WHERE id = 'expired'",
                [],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    let failed = hermes(home).args(["gateway", "run"]).assert().failure();
    let error = std::str::from_utf8(&failed.get_output().stderr).unwrap();
    for detail in [
        "Gateway requirement check failed",
        "qqbot.enabled=true",
        "wecom_callback.enabled=true",
    ] {
        assert!(error.contains(detail), "{error}");
    }
    assert!(!gateway_pid_path_in(home).exists());
    assert!(!home.join("gateway.lock").exists());

    let conn = rusqlite::Connection::open(persistence.db_path()).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let recent = persistence.load_session("recent").unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].content.as_deref(), Some("recent"));
}

#[test]
fn logs_report_malformed_utf8_at_the_selected_home() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let logs = home.join("logs");
    fs::create_dir(&logs).unwrap();
    fs::write(logs.join("hermes.log"), b"fixture log\n\xff").unwrap();

    let failed = hermes(home).args(["logs"]).assert().failure();
    let error = std::str::from_utf8(&failed.get_output().stderr).unwrap();
    assert!(error.contains("Failed to read log file"), "{error}");
    assert!(stdout(&failed).is_empty());
}

#[cfg(unix)]
#[test]
fn logs_follow_passes_the_line_count_and_reports_a_failed_tail() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let logs = home.join("logs");
    fs::create_dir(&logs).unwrap();
    let log = logs.join("hermes.log");
    fs::write(&log, "fixture log").unwrap();
    let test_binary = std::env::current_exe().unwrap();
    let executables = tempfile::tempdir_in(test_binary.parent().unwrap()).unwrap();
    let tail = executables.path().join("tail");
    fs::write(&tail, "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 7\n").unwrap();
    fs::set_permissions(&tail, fs::Permissions::from_mode(0o700)).unwrap();

    let followed = hermes(home)
        .env("PATH", executables.path())
        .args(["logs", "--follow", "4"])
        .assert()
        .success();
    let output = stdout(&followed);
    assert!(
        output.contains(&format!("-f\n-n\n4\n{}", log.display())),
        "{output}"
    );
    let error = std::str::from_utf8(&followed.get_output().stderr).unwrap();
    assert!(
        error
            .lines()
            .any(|line| line == "tail exited with status: exit status: 7"),
        "{error}"
    );
}
