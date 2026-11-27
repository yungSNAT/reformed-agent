use super::SessionPersistence;

#[test]
fn a_lock_is_released_even_whilst_another_descriptor_shares_it() {
    let home = tempfile::tempdir().unwrap();
    let store = SessionPersistence::new(home.path());
    let held = store.lock_session_budget("fixture").unwrap();
    // Stands in for a fork child that has not yet exec'd and still holds the description.
    let child_copy = held.0.try_clone().unwrap();
    drop(held);
    assert!(store.lock_session_budget("fixture").is_ok());
    drop(child_copy);
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
