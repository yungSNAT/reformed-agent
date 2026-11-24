use super::*;
use std::fs;
use std::process::Command;

fn snapshot_info(stem: &str) -> SessionInfo {
    SessionInfo {
        session_id: stem.to_owned(),
        model: "coverage-model".to_owned(),
        personality: None,
        message_count: 0,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

fn write_snapshot_fixture(dir: &Path, stem: &str) -> PathBuf {
    let path = dir.join(format!("{stem}.json"));
    let payload = serde_json::json!({
        "session_info": snapshot_info(stem),
        "messages": [],
    });
    fs::write(&path, payload.to_string()).unwrap();
    path
}

fn io_message(error: AgentError) -> String {
    let AgentError::Io(message) = error else {
        panic!("expected an I/O error, got {error:?}");
    };
    message
}

#[test]
fn pruning_a_missing_snapshot_leaves_the_byte_total_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_snapshot_fixture(dir.path(), "missing");
    let entry = list_session_snapshot_entries(dir.path())
        .into_iter()
        .find(|entry| entry.path == path)
        .expect("snapshot was not listed");
    let original_total = entry.size_bytes + 7;
    let mut total = original_total;

    fs::remove_file(&path).unwrap();
    App::prune_session_snapshot_entry(&entry, &mut total).unwrap();

    assert_eq!(total, original_total);
    assert!(!path.exists());
}

#[test]
fn pruning_a_snapshot_replaced_by_a_directory_reports_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_snapshot_fixture(dir.path(), "replaced");
    let entry = list_session_snapshot_entries(dir.path())
        .into_iter()
        .find(|entry| entry.path == path)
        .expect("snapshot was not listed");
    let original_total = entry.size_bytes + 7;
    let mut total = original_total;

    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let message = io_message(App::prune_session_snapshot_entry(&entry, &mut total).unwrap_err());

    assert!(message.starts_with(&format!(
        "Failed to prune session snapshot {}:",
        path.display()
    )));
    assert_eq!(total, original_total);
    assert!(path.is_dir());
}

#[test]
fn snapshot_write_reports_a_directory_at_the_destination_path() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    let path = sessions.join("blocked.json");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("keep"), b"untouched").unwrap();

    let message = io_message(
        write_session_snapshot(dir.path(), "blocked", &snapshot_info("blocked"), None, &[])
            .unwrap_err(),
    );

    assert!(message.starts_with(&format!(
        "Failed to write session snapshot {}:",
        path.display()
    )));
    assert_eq!(fs::read(path.join("keep")).unwrap(), b"untouched");
}

#[test]
fn oneshot_database_failure_does_not_create_a_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("sessions.db");
    fs::create_dir(&db_path).unwrap();
    let messages = [hermes_core::Message::user(String::from("not saved"))];

    let result = save_oneshot_session(dir.path(), "coverage-model", None, &messages, &[]);

    assert!(result.is_err(), "database failure was not propagated");
    assert!(db_path.is_dir());
    assert!(!dir.path().join("sessions").exists());
}

#[test]
fn oneshot_snapshot_directory_failure_is_reported_after_database_write() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    fs::write(&sessions, b"not a directory").unwrap();
    let messages = [hermes_core::Message::user(String::from(
        "saved to the database",
    ))];

    let message = io_message(
        save_oneshot_session(dir.path(), "coverage-model", None, &messages, &[]).unwrap_err(),
    );

    assert!(message.starts_with(&format!(
        "Failed to create sessions dir {}:",
        sessions.display()
    )));
    assert_eq!(fs::read(&sessions).unwrap(), b"not a directory");
    assert!(dir.path().join("sessions.db").is_file());
}

/// Re-run just this test with its own environment so the snapshot limits do not
/// race with other tests in the original harness.
fn with_snapshot_limits(test_name: &str, limits: (usize, u64, u64), check: impl FnOnce(&Path)) {
    const CHILD: &str = "HERMES_COVERAGE_SNAPSHOT_LIMITS_CHILD";
    const FINISHED: &str = "snapshot limit assertions completed";
    let (max_files, max_bytes, min_free) = limits;
    if std::env::var_os(CHILD).is_some_and(|value| value == "1") {
        assert_eq!(snapshot_max_files(), max_files);
        assert_eq!(snapshot_max_total_bytes(), max_bytes);
        assert_eq!(snapshot_min_free_bytes(), min_free);
        let dir = tempfile::tempdir().unwrap();
        check(dir.path());
        println!("{FINISHED}");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
    command
        .arg("--exact")
        .arg(format!("{module}::{test_name}"))
        .arg("--nocapture")
        .env(CHILD, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("HERMES_HOME", home.path());
    for prefix in [
        "HERMES_SESSION_SNAPSHOT",
        "HERMES_SNAPSHOT",
        "HERMES_SESSION",
    ] {
        command
            .env(format!("{prefix}_MAX_FILES"), max_files.to_string())
            .env(format!("{prefix}_MAX_TOTAL_BYTES"), max_bytes.to_string())
            .env(format!("{prefix}_MAX_BYTES"), max_bytes.to_string())
            .env(format!("{prefix}_MIN_FREE_BYTES"), min_free.to_string());
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(FINISHED),
        "snapshot limit test did not complete:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_oneshot_pruned_without_evicting_interactive_snapshots(sessions: &Path) {
    let first = write_snapshot_fixture(sessions, "interactive-first");
    let second = write_snapshot_fixture(sessions, "interactive-second");
    let first_contents = fs::read(&first).unwrap();
    let second_contents = fs::read(&second).unwrap();
    let oneshot = write_snapshot_fixture(sessions, "oneshot-12345678-1234-4234-8234-123456789abc");

    App::enforce_session_snapshot_guardrails(sessions, &oneshot).unwrap();

    assert!(
        !oneshot.exists(),
        "the one-shot snapshot must yield to the limit"
    );
    assert_eq!(fs::read(first).unwrap(), first_contents);
    assert_eq!(fs::read(second).unwrap(), second_contents);
}

#[test]
fn snapshot_count_limit_does_not_evict_interactive_history_for_oneshot() {
    with_snapshot_limits(
        "snapshot_count_limit_does_not_evict_interactive_history_for_oneshot",
        (1, 0, 0),
        assert_oneshot_pruned_without_evicting_interactive_snapshots,
    );
}

#[test]
fn snapshot_byte_limit_does_not_evict_interactive_history_for_oneshot() {
    with_snapshot_limits(
        "snapshot_byte_limit_does_not_evict_interactive_history_for_oneshot",
        (0, 1, 0),
        assert_oneshot_pruned_without_evicting_interactive_snapshots,
    );
}

#[cfg(unix)]
#[test]
fn disk_guardrail_reports_unmet_minimum_without_pruning_current_snapshot() {
    with_snapshot_limits(
        "disk_guardrail_reports_unmet_minimum_without_pruning_current_snapshot",
        (0, 0, u64::MAX),
        |sessions| {
            let current = write_snapshot_fixture(sessions, "current");
            let contents = fs::read(&current).unwrap();
            let message = io_message(
                App::enforce_session_snapshot_guardrails(sessions, &current).unwrap_err(),
            );

            assert!(message.starts_with("Session snapshot write blocked by disk guardrail:"));
            assert!(message.contains(&format!("required_min={} bytes", u64::MAX)));
            assert!(message.contains(&format!("(dir={})", sessions.display())));
            assert_eq!(fs::read(current).unwrap(), contents);
        },
    );
}
