use super::*;

#[cfg(unix)]
fn lock_dir(path: &Path) -> PathBuf {
    path.with_file_name("store.json.lock")
}

/// A lock left by a writer that died: owner `dead`, last touched a minute ago.
#[cfg(unix)]
fn dead_lock(path: &Path) {
    let dir = lock_dir(path);
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("owner"), "dead").unwrap();
    let old = SystemTime::now() - Duration::from_secs(60);
    std::fs::File::open(&dir)
        .unwrap()
        .set_modified(old)
        .unwrap();
}

#[test]
fn the_install_root_climbs_out_of_a_profile_home_only() {
    let root = Path::new("/r");
    assert_eq!(install_root_of(root), root);
    assert_eq!(install_root_of(&root.join("profiles").join("x")), root);
    assert_eq!(
        install_root_of(&root.join("other").join("x")),
        root.join("other").join("x")
    );
}

#[test]
fn the_profiles_folder_is_matched_in_any_case_as_upstream_does() {
    let root = Path::new("/r");
    assert_eq!(install_root_of(&root.join("Profiles").join("x")), root);
    assert_eq!(install_root_of(&root.join("PROFILES").join("x")), root);
}

#[cfg(unix)]
#[test]
fn a_stale_lock_is_broken_and_taken() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.json");
    dead_lock(&path);
    let lock = StoreLock::take(&path, "store").unwrap();
    assert!(lock.held());
}

#[cfg(unix)]
#[test]
fn a_stale_break_leaves_a_lock_retaken_since_it_was_judged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.json");
    dead_lock(&path);
    let seen = stale_owner(&lock_dir(&path)).expect("judged stale");
    // Another waiter breaks the same dead lock first and takes the store.
    break_stale(&lock_dir(&path), &seen);
    let live = StoreLock::take(&path, "store").unwrap();
    // The slower waiter now acts on its earlier judgement.
    break_stale(&lock_dir(&path), &seen);
    assert!(live.held(), "a live lock was removed as stale");
    assert!(StoreLock::take(&path, "store").is_err());
}

/// Waiters A, B and C over a dead lock: A judges it stale; B breaks it and takes the store; A then
/// acts on its judgement whilst C takes any name left free. B must still hold the store.
#[cfg(unix)]
#[test]
fn a_waiter_acting_on_an_old_judgement_never_displaces_a_live_lock() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.json");
    dead_lock(&path);
    let (b, c) = (Rc::new(RefCell::new(None)), Rc::new(RefCell::new(None)));
    let (b_path, b_slot, c_slot) = (path.clone(), b.clone(), c.clone());
    let b_takes = move || {
        *b_slot.borrow_mut() = Some(StoreLock::take(&b_path, "store").unwrap());
        // Armed only now, so it fires in A's break, not in B's.
        let c_takes = move || *c_slot.borrow_mut() = StoreLock::take(&b_path, "store").ok();
        SEAMS.with(|s| s.borrow_mut().push(("aside", Box::new(c_takes))));
    };
    SEAMS.with(|s| s.borrow_mut().push(("judged", Box::new(b_takes))));
    break_if_stale(&lock_dir(&path));
    let b = b.borrow_mut().take().expect("B took the store");
    assert!(
        b.held(),
        "a live lock was displaced by a waiter's old judgement"
    );
    assert!(c.borrow().is_none(), "C took the store whilst B held it");
}

/// Whilst another waiter of this gateway holds the break turn, a waiter leaves the stale lock to
/// it; once the turn is free, the next waiter breaks it.
#[cfg(unix)]
#[test]
fn a_waiter_leaves_a_stale_lock_to_the_waiter_already_breaking_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.json");
    dead_lock(&path);
    let turn = std::fs::File::create(path.with_file_name("store.json.lock.break")).unwrap();
    turn.lock().unwrap();
    break_if_stale(&lock_dir(&path));
    assert_eq!(
        std::fs::read(lock_dir(&path).join("owner")).unwrap(),
        b"dead",
        "a waiter broke the lock whilst another held the turn"
    );
    drop(turn);
    break_if_stale(&lock_dir(&path));
    assert!(!lock_dir(&path).exists(), "the free turn broke nothing");
}

/// A waiter that cannot open the break turn file breaks nothing.
#[cfg(unix)]
#[test]
fn a_waiter_that_cannot_open_the_break_turn_breaks_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.json");
    dead_lock(&path);
    std::fs::create_dir(path.with_file_name("store.json.lock.break")).unwrap();
    break_if_stale(&lock_dir(&path));
    assert_eq!(
        std::fs::read(lock_dir(&path).join("owner")).unwrap(),
        b"dead"
    );
}

/// What the protocol promises a writer displaced by a racing break: it no longer holds the lock,
/// so the write answers busy, and releasing it leaves the new holder's lock alone.
#[test]
fn a_displaced_writer_no_longer_holds_and_leaves_the_new_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.json");
    let displaced = StoreLock::take(&path, "store").unwrap();
    // A racing break moved this lock aside and another writer took the name.
    let owner = path.with_file_name("store.json.lock").join("owner");
    std::fs::write(&owner, "newer").unwrap();
    assert!(
        !displaced.held(),
        "a displaced writer still thinks it holds"
    );
    drop(displaced);
    assert_eq!(std::fs::read(&owner).unwrap(), b"newer");
}

#[test]
fn a_lock_error_names_the_store_without_os_text() {
    let dir = tempfile::tempdir().unwrap();
    // The parent folder is missing, so the lock folder cannot be made.
    let path = dir.path().join("missing").join("store.json");
    assert_eq!(
        StoreLock::take(&path, "preferences store").err().as_deref(),
        Some("preferences store is unreadable")
    );
}
