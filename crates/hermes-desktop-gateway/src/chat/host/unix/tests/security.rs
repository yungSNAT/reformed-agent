use std::os::fd::AsFd;
use std::os::unix::fs::symlink;

use super::super::allowed;
use super::super::complete::{items_in, open_root};
use super::texts;

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
/// external entries; the anchored descriptor keeps serving the original directory inode.
#[test]
fn a_renamed_root_replaced_by_a_symlink_never_lists_external_entries() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "x").unwrap();
    let parent = tempfile::tempdir().unwrap();
    let work = parent.path().join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("local.txt"), "y").unwrap();
    let anchored = open_root(&work).expect("open original root");
    let moved = parent.path().join("moved");
    std::fs::rename(&work, &moved).unwrap();
    symlink(outside.path(), &work).unwrap();
    assert!(items_in(anchored.as_fd(), "s").is_empty());
    assert_eq!(texts(&items_in(anchored.as_fd(), "l")), ["local.txt"]);
    assert!(open_root(&work).is_none());
}

/// S3: pidfd-pinned sweeping never signals a process in another session or a PID whose original
/// process has already been replaced.
#[cfg(target_os = "linux")]
#[test]
fn a_pidfd_sweep_never_signals_a_replaced_or_foreign_session_process() {
    use super::super::supervise::{kill_pinned_if_in_session, open_pidfd};

    let mut victim = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let victim_pid = i32::try_from(victim.id()).unwrap();
    // Pin the real process and re-read its real /proc/<pid>/stat against a non-matching session:
    // must refuse to signal and leave the victim running.
    let killed = kill_pinned_if_in_session(victim_pid, "0", open_pidfd, |p| {
        std::fs::read_to_string(format!("/proc/{p}/stat")).ok()
    });
    assert!(!killed);
    assert!(victim.try_wait().unwrap().is_none(), "foreign child died");

    // Process exits between pidfd_open and stat re-read -> read_stat returns None.
    assert!(!kill_pinned_if_in_session(
        victim_pid,
        "42",
        open_pidfd,
        |_| None
    ));
    // Already-exited PID before pidfd_open -> pin returns None.
    assert!(open_pidfd(-1).is_none());
    assert!(!kill_pinned_if_in_session(-1, "42", open_pidfd, |_| {
        unreachable!("pin returned None")
    }));
    let _ = victim.kill();
    let _ = victim.wait();

    // PID replacement after pinning: pin a child, kill and reap it, then feed a matching stat
    // line claiming a replacement process now occupies the same numeric PID. pidfd_send_signal
    // on the dead pidfd must return false (ESRCH) without signalling any live process.
    let mut replaced = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let rpid = i32::try_from(replaced.id()).unwrap();
    let pinned = open_pidfd(rpid).expect("pin live child");
    let _ = replaced.kill();
    let _ = replaced.wait();
    let fake_stat = format!("{rpid} (sh) S 1 42 42");
    assert!(!kill_pinned_if_in_session(
        rpid,
        "42",
        |_| Some(pinned),
        |_| Some(fake_stat),
    ));
}
