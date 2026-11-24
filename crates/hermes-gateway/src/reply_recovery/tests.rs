use super::*;

fn record(session: &str, process: &str) -> PendingReply {
    PendingReply {
        session_key: session.to_string(),
        platform: "telegram".to_string(),
        chat_id: "42".to_string(),
        thread_id: Some("7".to_string()),
        process: process.to_string(),
        reply: None,
        attempts: 0,
    }
}

fn never_alive(_pid: u32) -> bool {
    false
}

#[test]
fn a_start_takes_over_only_records_that_stopped_processes_left() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    write(&dir, &record("mine", &this_process()));
    write(&dir, &record("stopped", "1-1"));
    write(&dir, &record("running", "7-1"));
    std::fs::write(dir.join("broken.json"), b"{ not json").expect("write");

    answered(&dir, "mine", "done");
    answered(&dir, "stopped", "not mine to write");
    assert_eq!(take_over(&dir, |pid| pid == 7), 1);
    let taken = recovering(&dir);
    assert_eq!(taken.len(), 1, "own and live writers' records stay");
    assert_eq!(taken[0].1.session_key, "stopped");
    assert_eq!(taken[0].1.reply, None);
    assert_eq!(taken[0].1.thread_id.as_deref(), Some("7"));
    assert!(
        !dir.join("broken.json").exists(),
        "an unreadable record is deleted"
    );

    // A new turn of the same session writes its own record beside it.
    write(&dir, &record("stopped", &this_process()));
    assert_eq!(recovering(&dir).len(), 1);
    finish(&dir, "stopped");
    finish(&dir, "mine");
    assert_eq!(
        recovering(&dir).len(),
        1,
        "finish leaves taken-over records"
    );
    assert_eq!(json_files(&dir).len(), 1, "the running writer's record");
}

#[test]
fn each_send_is_counted_before_it_is_made_and_the_cap_drops_the_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    write(&dir, &record("stuck", "1-1"));
    assert_eq!(take_over(&dir, never_alive), 1);
    let (path, mut stuck) = recovering(&dir).remove(0);
    for n in 1..=MAX_SEND_ATTEMPTS {
        assert!(count_attempt(&path, &mut stuck));
        assert_eq!(recovering(&dir)[0].1.attempts, n, "written before the send");
    }
    assert!(!count_attempt(&path, &mut stuck));
    assert!(recovering(&dir).is_empty());
}

#[cfg(unix)]
#[test]
fn no_send_is_made_when_its_count_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    write(&dir, &record("stuck", "1-1"));
    take_over(&dir, never_alive);
    let (path, mut stuck) = recovering(&dir).remove(0);
    let taken = recovering_dir(&dir);
    std::fs::set_permissions(&taken, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let counted = count_attempt(&path, &mut stuck);
    std::fs::set_permissions(&taken, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert!(!counted);
}

#[cfg(unix)]
#[test]
fn records_are_readable_by_their_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    write(&dir, &record("private", "1-1"));
    let mode = |p: &Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    let file = std::fs::read_dir(&dir)
        .expect("dir")
        .flatten()
        .next()
        .expect("record")
        .path();
    assert_eq!(mode(&file), 0o600);
}
