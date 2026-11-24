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
    let mine = write(&dir, &record("mine", &this_process())).expect("mine");
    let stopped = write(&dir, &record("stopped", "1-1")).expect("stopped");
    // Started since this boot: liveness decides.
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    write(&dir, &record("running", &format!("7-{started}"))).expect("running");
    std::fs::write(dir.join("broken.json"), b"{ not json").expect("write");

    assert!(!begin_send(&dir, &stopped, "not mine to write"));
    assert!(begin_send(&dir, &mine, "done"));
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
    let next_turn = write(&dir, &record("stopped", &this_process())).expect("next turn");
    assert_eq!(recovering(&dir).len(), 1);
    finish(&dir, &next_turn);
    finish(&dir, &mine);
    assert_eq!(
        recovering(&dir).len(),
        1,
        "finish leaves taken-over records"
    );
    assert_eq!(json_files(&dir).len(), 1, "the running writer's record");
}

#[test]
fn distinct_turn_records_do_not_overwrite_earlier_session_records() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    let turn1 = write(&dir, &record("same-session", &this_process())).expect("turn1");
    let turn2 = write(&dir, &record("same-session", &this_process())).expect("turn2");
    assert_ne!(turn1, turn2);
    assert!(begin_send(&dir, &turn1, "first reply"));
    finish(&dir, &turn2);
    assert!(turn1.exists(), "finishing turn 2 must preserve turn 1");
    let saved = read(&turn1).expect("turn1 readable");
    assert_eq!(saved.reply.as_deref(), Some("first reply"));
    assert_eq!(saved.attempts, 1);
}

#[test]
fn a_writer_from_before_the_boot_has_stopped_whatever_its_pid_names_now() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    let at = |secs: u64| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    // Written by pid 7, started at 1,000 s; pid 7 is alive now.
    write(&dir, &record("rebooted", "7-1000000000000")).expect("rebooted");
    assert_eq!(
        take_over_since_with_hook(&dir, |pid| pid == 7, Some(at(500)), || {}).moved,
        0,
        "same boot"
    );
    assert_eq!(
        take_over_since_with_hook(&dir, |pid| pid == 7, Some(at(2_000)), || {}).moved,
        1,
        "booted since"
    );
    assert_eq!(recovering(&dir)[0].1.session_key, "rebooted");
}

#[test]
fn each_send_is_counted_before_it_is_made_and_the_cap_drops_the_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    write(&dir, &record("stuck", "1-1")).expect("write");
    assert_eq!(take_over(&dir, never_alive), 1);
    let (path, mut stuck) = recovering(&dir).remove(0);
    for n in 1..=MAX_SEND_ATTEMPTS {
        assert_eq!(
            count_attempt(&dir, &path, &mut stuck),
            AttemptOutcome::Reserved
        );
        assert_eq!(recovering(&dir)[0].1.attempts, n, "written before the send");
    }
    assert_eq!(
        count_attempt(&dir, &path, &mut stuck),
        AttemptOutcome::Exhausted
    );
    assert!(recovering(&dir).is_empty());
}

#[test]
fn concurrent_snapshots_at_two_authorize_at_most_one_third_attempt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    let mut rec = record("raced", "1-1");
    rec.attempts = MAX_SEND_ATTEMPTS - 1;
    write(&dir, &rec).expect("write");
    assert_eq!(take_over(&dir, never_alive), 1);

    let (path1, mut snap1) = recovering(&dir).remove(0);
    let (path2, mut snap2) = recovering(&dir).remove(0);
    assert_eq!(path1, path2);

    assert_eq!(
        count_attempt(&dir, &path1, &mut snap1),
        AttemptOutcome::Reserved,
        "first snapshot reserves attempt 3"
    );
    assert_eq!(snap1.attempts, MAX_SEND_ATTEMPTS);
    assert_eq!(
        count_attempt(&dir, &path2, &mut snap2),
        AttemptOutcome::Exhausted,
        "second stale snapshot must see attempt 3 on disk and refuse send"
    );
    assert!(recovering(&dir).is_empty(), "exhausted record dropped");
}

#[test]
fn takeover_and_record_creation_serialize_under_cross_handle_lock() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    let mut old = record("chat", "1-1");
    old.reply = Some("old reply".to_string());
    write(&dir, &old).expect("old");

    let (paused_tx, paused_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let dir_a = dir.clone();
    let thread_a = std::thread::spawn(move || {
        take_over_since_with_hook(&dir_a, never_alive, None, || {
            paused_tx.send(()).expect("signal pause");
            go_rx.recv().expect("wait go");
        })
    });

    paused_rx.recv().expect("wait A inside takeover lock");
    let probe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path(&dir))
        .expect("open lock probe");
    assert!(
        probe.try_lock().is_err(),
        "second independent file handle must see the directory lock held"
    );
    drop(probe);

    let (w_done_tx, w_done_rx) = std::sync::mpsc::channel();
    let dir_w = dir.clone();
    let thread_w = std::thread::spawn(move || {
        let mut fresh = record("chat", "2-2");
        fresh.reply = Some("new reply".to_string());
        let written = write(&dir_w, &fresh);
        w_done_tx.send(written.is_some()).expect("w done");
    });

    let (b_done_tx, b_done_rx) = std::sync::mpsc::channel();
    let dir_b = dir.clone();
    let thread_b = std::thread::spawn(move || {
        let moved = take_over(&dir_b, never_alive);
        b_done_tx.send(moved).expect("b done");
    });

    std::thread::yield_now();
    assert!(
        matches!(
            w_done_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "new record creation must block while takeover holds the lock"
    );
    assert!(
        matches!(
            b_done_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "second takeover must block while first takeover holds the lock"
    );

    go_tx.send(()).expect("release A");
    assert_eq!(thread_a.join().expect("join A").moved, 1);
    assert!(w_done_rx.recv().expect("w result"));
    let _ = b_done_rx.recv().expect("b result");
    thread_w.join().expect("join W");
    thread_b.join().expect("join B");

    // In case B acquired the lock before W, take over any remaining stopped record.
    take_over(&dir, never_alive);
    let mut replies: Vec<Option<String>> =
        recovering(&dir).into_iter().map(|(_, r)| r.reply).collect();
    replies.sort();
    assert_eq!(
        replies,
        vec![Some("new reply".to_string()), Some("old reply".to_string())],
        "both old and new records must survive without overwriting each other"
    );
}

#[test]
fn no_send_is_made_when_its_count_cannot_be_written() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    write(&dir, &record("stuck", "1-1")).expect("write");
    take_over(&dir, never_alive);
    let (path, mut stuck) = recovering(&dir).remove(0);
    // Removed/missing record reports Missing (no false exhaustion warning).
    let missing_path = path.with_file_name("gone.json");
    assert_eq!(
        count_attempt(&dir, &missing_path, &mut stuck),
        AttemptOutcome::Missing
    );
    // A file where the folder was: the count cannot be written.
    let taken = recovering_dir(&dir);
    std::fs::remove_dir_all(&taken).expect("remove");
    std::fs::write(&taken, b"").expect("block");
    assert_eq!(
        count_attempt(&dir, &path, &mut stuck),
        AttemptOutcome::Missing
    );
}

#[cfg(unix)]
#[test]
fn count_attempt_write_failure_preserves_record_and_reports_write_failed() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    write(&dir, &record("stuck", "1-1")).expect("write");
    assert_eq!(take_over(&dir, never_alive), 1);
    let (path, mut stuck) = recovering(&dir).remove(0);

    let fail_file_chmod = |p: &Path, m: u32| -> std::io::Result<()> {
        if m == 0o600
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().contains(".tmp."))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected attempt write failure",
            ));
        }
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m))
    };
    assert_eq!(
        count_attempt_with(&dir, &path, &mut stuck, fail_file_chmod),
        AttemptOutcome::WriteFailed
    );
    let kept = recovering(&dir);
    assert_eq!(kept.len(), 1, "record must stay intact after WriteFailed");
    assert_eq!(kept[0].1.attempts, 0, "attempts must stay unchanged");
}

#[cfg(unix)]
#[test]
fn begin_send_write_failure_preserves_existing_record() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    let path = write(&dir, &record("live-s1", &this_process())).expect("write");

    let fail_file_chmod = |p: &Path, m: u32| -> std::io::Result<()> {
        if m == 0o600
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().contains(".tmp."))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected begin_send write failure",
            ));
        }
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m))
    };
    assert!(!begin_send_with(
        &dir,
        &path,
        "must not send",
        fail_file_chmod
    ));
    let kept = read(&path).expect("record must stay on disk when begin_send write fails");
    assert_eq!(kept.reply, None);
    assert_eq!(kept.attempts, 0);
}

#[cfg(unix)]
#[test]
fn records_are_readable_by_their_owner_only_and_chmod_failures_fail_closed() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    let path = write(&dir, &record("private", "1-1")).expect("write");
    let mode = |p: &Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&lock_path(&dir)), 0o600);
    assert_eq!(mode(&path), 0o600);

    // Inject chmod failure for files: must refuse to write temp or final bytes.
    let file_chmod_fails = |p: &Path, m: u32| -> std::io::Result<()> {
        if m == 0o600 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected file chmod failure",
            ));
        }
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m))
    };
    let rejected_dir = tmp.path().join("rejected-file-chmod");
    let rejected_path = rejected_dir.join("secret.json");
    assert!(!write_at_with(
        &rejected_path,
        &record("secret", "1-1"),
        file_chmod_fails
    ));
    assert!(!rejected_path.exists());
    let leftovers: Vec<_> = std::fs::read_dir(&rejected_dir)
        .expect("dir")
        .flatten()
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temp or final files may remain: {leftovers:?}"
    );

    // Inject chmod failure for a traversable 0o755 directory: must fail closed.
    let perm_dir = tmp.path().join("traversable");
    std::fs::create_dir_all(&perm_dir).expect("mkdir");
    std::fs::set_permissions(&perm_dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let dir_chmod_fails = |_: &Path, _: u32| -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "injected dir chmod failure",
        ))
    };
    let perm_path = perm_dir.join("secret.json");
    assert!(!write_at_with(
        &perm_path,
        &record("secret", "1-1"),
        dir_chmod_fails
    ));
    assert!(!perm_path.exists());
    assert!(std::fs::read_dir(&perm_dir).expect("dir").next().is_none());
}

#[cfg(unix)]
#[test]
fn symlinks_for_lock_records_and_recovering_dir_are_refused() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = records_dir(tmp.path());
    private_dir_with(&dir, set_unix_mode);

    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir_all(&outside_dir).expect("outside dir");
    let outside_json = outside_dir.join("leak.json");
    assert!(write_at_with(
        &outside_json,
        &record("sym", "1-1"),
        set_unix_mode
    ));

    // Symlinked json file in dir/ must be ignored by take_over.
    symlink(&outside_json, dir.join("linked.json")).expect("symlink json");
    assert_eq!(take_over(&dir, never_alive), 0);
    assert!(outside_json.exists(), "symlink target must not be moved");
    std::fs::remove_file(dir.join("linked.json")).expect("remove link");

    // Symlinked recovering/ dir must be ignored by recovering().
    symlink(&outside_dir, recovering_dir(&dir)).expect("symlink recovering");
    assert!(
        recovering(&dir).is_empty(),
        "recovering() must refuse a symlinked recovering/ directory"
    );
    std::fs::remove_file(recovering_dir(&dir)).expect("remove recovering link");

    // Symlinked .lock must cause write() to fail closed.
    let symlink_lock_dir = tmp.path().join("symlink-lock-dir");
    private_dir_with(&symlink_lock_dir, set_unix_mode);
    let outside_lock = tmp.path().join("outside.lock");
    std::fs::write(&outside_lock, b"").expect("outside lock");
    symlink(&outside_lock, lock_path(&symlink_lock_dir)).expect("symlink lock");
    assert!(
        write(&symlink_lock_dir, &record("blocked-by-symlink-lock", "1-1")).is_none(),
        "symlinked .lock must be refused"
    );
}
