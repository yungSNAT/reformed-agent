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

#[test]
fn invalid_charges_preserve_a_pending_reservation_and_spend() {
    let home = tempfile::tempdir().unwrap();
    let store = SessionPersistence::new(home.path());
    store.ensure_db().unwrap();
    let id = "fixture";

    let no_reservation = store.charge_session(id, 0.25).unwrap_err();
    assert!(no_reservation
        .to_string()
        .contains("Missing pending session charge"));
    store.begin_budget_request(id).unwrap();
    store.charge_session(id, 0.5).unwrap();
    store.begin_budget_request(id).unwrap();

    for amount in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.25] {
        let invalid = store.charge_session(id, amount).unwrap_err();
        assert!(invalid.to_string().contains("Invalid provider charge"));
        assert_eq!(store.session_spend_usd(id).unwrap(), 0.5);
    }

    let reopened = SessionPersistence::new(home.path());
    let pending = reopened.begin_budget_request(id).unwrap_err();
    assert!(pending.to_string().contains("unconfirmed spend"));
    reopened.charge_session(id, 0.25).unwrap();
    assert_eq!(reopened.session_spend_usd(id).unwrap(), 0.75);
    let duplicate = reopened.charge_session(id, 0.25).unwrap_err();
    assert!(duplicate
        .to_string()
        .contains("Missing pending session charge"));
    assert_eq!(reopened.session_spend_usd(id).unwrap(), 0.75);
}

#[test]
fn resetting_session_spend_does_not_create_a_missing_database() {
    let home = tempfile::tempdir().unwrap();
    let store = SessionPersistence::new(home.path());
    store.reset_session_spend("fixture").unwrap();
    assert!(!store.db_path().exists());
}

#[test]
fn invalid_saved_spend_is_reported_until_reset() {
    let home = tempfile::tempdir().unwrap();
    let store = SessionPersistence::new(home.path());
    store.ensure_db().unwrap();
    let id = "fixture";
    store.begin_budget_request(id).unwrap();
    rusqlite::Connection::open(store.db_path())
        .unwrap()
        .execute(
            "UPDATE sessions SET spent_usd = ?2 WHERE id = ?1",
            rusqlite::params![id, -0.5],
        )
        .unwrap();

    let invalid = store.session_spend_usd(id).unwrap_err();
    assert!(invalid.to_string().contains("Invalid saved session spend"));
    store.reset_session_spend(id).unwrap();
    assert_eq!(store.session_spend_usd(id).unwrap(), 0.0);
    store.begin_budget_request(id).unwrap();
}
