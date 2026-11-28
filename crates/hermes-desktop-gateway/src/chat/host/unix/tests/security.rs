use std::os::unix::fs::symlink;

use super::super::allowed;
use super::super::complete::items;

/// S1: a symlinked `home/profiles` directory must never be followed into an external directory.
#[test]
fn a_symlinked_profiles_directory_is_refused() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("work.yaml"), "").unwrap();
    let home = tempfile::tempdir().unwrap();
    symlink(outside.path(), home.path().join("profiles")).unwrap();
    let argv: Vec<String> = ["profile", "delete", "work", "--yes"]
        .into_iter()
        .map(String::from)
        .collect();
    assert!(!allowed(home.path(), &argv));
}

/// S2: renaming the root directory and replacing its pathname with a symlink must never list
/// external entries.
#[test]
fn a_renamed_root_replaced_by_a_symlink_never_lists_external_entries() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "x").unwrap();
    let parent = tempfile::tempdir().unwrap();
    let work = parent.path().join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("local.txt"), "y").unwrap();
    let moved = parent.path().join("moved");
    std::fs::rename(&work, &moved).unwrap();
    symlink(outside.path(), &work).unwrap();
    assert!(items(&work, "s").is_empty());
}
