#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Barrier};

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

#[cfg(unix)]
#[test]
fn writers_racing_a_stale_lock_never_hold_it_together() {
    let dir = tempfile::tempdir().unwrap();
    let path = Arc::new(dir.path().join("store.json"));
    dead_lock(&path);
    let holding = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(Barrier::new(8));
    let writers: Vec<_> = (0..8)
        .map(|_| {
            let (path, holding, most, start) =
                (path.clone(), holding.clone(), most.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                let Ok(lock) = StoreLock::take(&path, "store") else {
                    return false;
                };
                let now = holding.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(30));
                let still = lock.held();
                holding.fetch_sub(1, Ordering::SeqCst);
                still
            })
        })
        .collect();
    let held: Vec<bool> = writers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(
        most.load(Ordering::SeqCst),
        1,
        "2 writers held the lock at once"
    );
    assert!(
        held.iter().all(|&h| h),
        "a writer lost its lock whilst holding it"
    );
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
