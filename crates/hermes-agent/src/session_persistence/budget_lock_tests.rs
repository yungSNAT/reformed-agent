use super::SessionPersistence;

#[test]
fn a_lock_held_only_briefly_is_waited_out() {
    let home = tempfile::tempdir().unwrap();
    let store = SessionPersistence::new(home.path());
    let held = store.lock_session_budget("fixture").unwrap();
    let (locked, release) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            // Stands in for a fork child that has not yet exec'd.
            release.recv().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
            drop(held);
        });
        locked.send(()).unwrap();
        assert!(store.lock_session_budget("fixture").is_ok());
    });
}

#[test]
fn a_lock_that_stays_held_is_refused_with_the_holder_reason() {
    let home = tempfile::tempdir().unwrap();
    let store = SessionPersistence::new(home.path());
    let _held = store.lock_session_budget("fixture").unwrap();
    let message = store
        .lock_session_budget("fixture")
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("Cannot lock session budget for this run or reset"),
        "{message}"
    );
    assert!(message.contains("would block"), "{message}");
}
