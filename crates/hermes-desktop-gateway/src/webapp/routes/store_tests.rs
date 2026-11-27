use super::*;

fn lock_dir(path: &Path) -> PathBuf {
    path.with_file_name("store.json.lock")
}

/// A lock left by a writer that died: owner `dead`, last touched a minute ago.
fn dead_lock(path: &Path) {
    let dir = lock_dir(path);
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("owner"), "dead").unwrap();
    let old = SystemTime::now() - Duration::from_secs(60);
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    // Windows opens a folder only with backup semantics, and sets its time only with write access.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        open.write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    open.open(&dir).unwrap().set_modified(old).unwrap();
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

/// A folder or a link in the break turn file's place never wedges writes: the waiter breaks the
/// dead lock as before, and the link is not followed (Unix refuses it; Windows opens the link).
#[test]
fn a_waiter_that_cannot_open_the_break_turn_still_breaks_a_dead_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.json");
    dead_lock(&path);
    std::fs::create_dir(path.with_file_name("store.json.lock.break")).unwrap();
    break_if_stale(&lock_dir(&path));
    assert!(
        !lock_dir(&path).exists(),
        "an unopenable turn wedged the lock"
    );

    let linked = tempfile::tempdir().unwrap();
    let path = linked.path().join("store.json");
    dead_lock(&path);
    let target = linked.path().join("elsewhere");
    let link = path.with_file_name("store.json.lock.break");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&target, link)
        .expect("a symlink on Windows needs Developer Mode or an elevated shell");
    break_if_stale(&lock_dir(&path));
    assert!(!lock_dir(&path).exists(), "a linked turn wedged the lock");
    assert!(!target.exists(), "the turn file followed a link");
}

/// A FIFO with no reader in the break turn file's place neither hangs the waiter nor wedges the
/// dead lock: the turn is opened without blocking, fails, and the waiter breaks as before.
#[cfg(unix)]
#[test]
fn a_fifo_in_the_break_turn_place_neither_hangs_nor_wedges_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.json");
    dead_lock(&path);
    let fifo = std::ffi::CString::new(
        path.with_file_name("store.json.lock.break")
            .into_os_string()
            .into_encoded_bytes(),
    )
    .unwrap();
    // SAFETY: `fifo` is a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let (done, broke) = std::sync::mpsc::channel();
    let lock = lock_dir(&path);
    std::thread::spawn(move || {
        break_if_stale(&lock);
        let _ = done.send(());
    });
    broke
        .recv_timeout(Duration::from_secs(10))
        .expect("the waiter hung opening a FIFO turn");
    assert!(!lock_dir(&path).exists(), "a FIFO turn wedged the lock");
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
