// Coverage tests for `commands/hermes_migrate.rs`.
//
// Lines not covered here: 127-129 run only on Windows. 136, 141-144, 149-154
// and 488 need the apply pass to fail after a clean plan. 554-555 need JSON
// serialisation of a converted job to fail. None of these can be triggered
// reliably from a unit test.

use super::*;

/// A temporary root holding separate Hermes and Reformed homes.
fn homes() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let from = root.path().join("hermes");
    let to = root.path().join("reformed");
    fs::create_dir(&from).unwrap();
    fs::create_dir(&to).unwrap();
    (root, from, to)
}

/// Plans a migration into an existing Reformed home without writing anything.
fn dry_run(from: &Path, to: &Path) -> MigrateReport {
    migrate_hermes(&MigrateRequest {
        from,
        to,
        apply: false,
        into_existing: true,
    })
    .unwrap()
}

/// Returns `cron/jobs.json` content that the cron parser accepts.
fn valid_jobs() -> Vec<u8> {
    let candidates: [&[u8]; 4] = [
        br#"{"jobs":[]}"#,
        br#"{"jobs":[],"updated_at":"2024-01-01T00:00:00Z"}"#,
        b"[]",
        b"{}",
    ];
    candidates
        .iter()
        .map(|candidate| candidate.to_vec())
        .find(|candidate| cron::jobs(candidate).is_ok())
        .expect("one candidate jobs.json should parse")
}

#[cfg(not(windows))]
#[test]
fn default_hermes_home_is_dot_hermes_in_user_home() {
    let mut name = std::ffi::OsString::from(".hermes");
    name.push(std::env::var_os("HERMES_DATA_DIR_SUFFIX").unwrap_or_default());
    let expected = hermes_config::user_home_dir().join(name);
    assert_eq!(default_hermes_home(), expected);
}

#[test]
fn existing_target_that_is_a_file_is_refused() {
    let (root, from, _) = homes();
    let target = root.path().join("target-file");
    fs::write(&target, b"x").unwrap();
    let error = migrate_hermes(&MigrateRequest {
        from: &from,
        to: &target,
        apply: false,
        into_existing: true,
    })
    .unwrap_err();
    assert!(
        matches!(&error, AgentError::Config(message) if message.contains("is not a folder")),
        "unexpected error: {error:?}"
    );
}

#[cfg(unix)]
#[test]
fn open_regular_rejects_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let error = open_regular(dir.path()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("not a regular file"));
}

#[test]
fn rollback_reports_entries_it_could_not_remove() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("never-created");
    let writer = Writer {
        created: vec![missing.clone()],
    };
    assert_eq!(writer.rollback(), vec![missing]);
}

#[test]
fn state_db_folder_is_skipped() {
    let (_root, from, to) = homes();
    fs::create_dir(from.join("state.db")).unwrap();
    let report = dry_run(&from, &to);
    assert!(!report.failed);
    assert!(report
        .lines
        .iter()
        .any(|line| line == "state.db: skipped, not a regular file"));
}

#[test]
fn state_db_skipped_when_target_has_sessions() {
    let (_root, from, to) = homes();
    fs::write(from.join("state.db"), b"not a database").unwrap();
    fs::create_dir(to.join("sessions")).unwrap();
    let report = dry_run(&from, &to);
    assert!(!report.failed);
    assert!(report
        .lines
        .iter()
        .any(|line| line
            == "state.db: skipped, the Reformed home already has sessions, left unchanged"));
}

#[test]
fn cron_jobs_folder_is_skipped() {
    let (_root, from, to) = homes();
    fs::create_dir_all(from.join("cron").join("jobs.json")).unwrap();
    let report = dry_run(&from, &to);
    assert!(!report.failed);
    assert!(report
        .lines
        .iter()
        .any(|line| line == "cron/jobs.json: skipped, not a regular file"));
}

#[test]
fn oversized_cron_jobs_is_an_error() {
    let (_root, from, to) = homes();
    fs::create_dir(from.join("cron")).unwrap();
    let file = File::create(from.join("cron").join("jobs.json")).unwrap();
    file.set_len(MAX_JOBS_BYTES + 1).unwrap();
    drop(file);
    let report = dry_run(&from, &to);
    assert!(report.failed);
    assert!(report
        .lines
        .iter()
        .any(|line| line == "error: cron/jobs.json: larger than 64 MiB"));
}

#[test]
fn unparsable_cron_jobs_is_an_error() {
    let (_root, from, to) = homes();
    fs::create_dir(from.join("cron")).unwrap();
    fs::write(from.join("cron").join("jobs.json"), b"not json {{{").unwrap();
    let report = dry_run(&from, &to);
    assert!(report.failed);
    assert!(report
        .lines
        .iter()
        .any(|line| line.starts_with("error: cron/jobs.json: ")));
    assert_eq!(
        report.lines.last().map(String::as_str),
        Some("Dry run: nothing was written. Run again with --apply to migrate.")
    );
}

#[test]
fn cron_skipped_when_target_cron_is_a_file() {
    let (_root, from, to) = homes();
    fs::create_dir(from.join("cron")).unwrap();
    fs::write(from.join("cron").join("jobs.json"), valid_jobs()).unwrap();
    fs::write(to.join("cron"), b"not a folder").unwrap();
    let report = dry_run(&from, &to);
    assert!(!report.failed);
    assert!(report
        .lines
        .iter()
        .any(|line| line == "cron/jobs.json: skipped, cron in the Reformed home is not a folder"));
}

#[cfg(unix)]
#[test]
fn cron_skipped_when_target_cron_cannot_be_inspected() {
    let (root, from, _) = homes();
    fs::create_dir(from.join("cron")).unwrap();
    fs::write(from.join("cron").join("jobs.json"), valid_jobs()).unwrap();
    // A regular file as the target makes `<target>/cron` fail with ENOTDIR.
    let target = root.path().join("plain-file");
    fs::write(&target, b"x").unwrap();
    let report = Run::new(&from, &target, None).items().unwrap();
    assert!(!report.failed);
    assert!(report
        .lines
        .iter()
        .any(|line| line == "cron/jobs.json: skipped, cron in the Reformed home is not a folder"));
}
