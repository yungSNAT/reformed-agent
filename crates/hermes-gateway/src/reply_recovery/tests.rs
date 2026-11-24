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

#[test]
fn a_process_updates_and_clears_only_its_own_records() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    write(&dir, &record("mine", &this_process()));
    write(&dir, &record("theirs", "1-1"));

    answered(&dir, "mine", "done");
    answered(&dir, "theirs", "not mine to write");
    let left = left_by_others(&dir);
    assert_eq!(left.len(), 1, "a process never recovers its own turns");
    assert_eq!(left[0].session_key, "theirs");
    assert_eq!(left[0].reply, None);
    assert_eq!(left[0].thread_id.as_deref(), Some("7"));

    finish(&dir, "theirs");
    finish(&dir, "mine");
    assert_eq!(
        left_by_others(&dir).len(),
        1,
        "finish leaves another's record"
    );
    assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 1);
}

#[test]
fn each_send_is_counted_before_it_is_made_and_the_cap_drops_the_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    let mut stuck = record("stuck", "1-1");
    write(&dir, &stuck);
    for n in 1..=MAX_SEND_ATTEMPTS {
        assert!(count_attempt(&dir, &mut stuck));
        assert_eq!(
            left_by_others(&dir)[0].attempts,
            n,
            "written before the send"
        );
    }
    assert!(!count_attempt(&dir, &mut stuck));
    assert!(left_by_others(&dir).is_empty());
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
