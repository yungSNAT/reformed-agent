//! Git commands for reading a repository whose own configuration may be
//! hostile: nothing in it can make git run a program or fetch.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::subprocess::CommandNoWindowExt;

/// How `workspace_git` keeps git from fetching an object the repository lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LazyFetchGuard {
    /// Git 2.45 and later take `--no-lazy-fetch`.
    Flag,
    /// Git 2.39.1 to 2.44 have no such option, and fetch only from a promisor
    /// remote, so the snapshot is kept for a repository whose config names none.
    NoPromisorRemote,
}

/// The guard the installed git supports, or `None` for a git that cannot
/// produce the snapshot safely.
pub fn lazy_fetch_guard() -> Option<LazyFetchGuard> {
    probe_lazy_fetch_guard(std::ffi::OsStr::new("git"))
}

/// [`lazy_fetch_guard`] for the git program `git`. Each probe has the
/// snapshot's time limit, so a git that never answers gives no guard.
fn probe_lazy_fetch_guard(git: &std::ffi::OsStr) -> Option<LazyFetchGuard> {
    // `git version` reads no repository.
    let version = |option: Option<&str>| {
        output_within_timeout(
            Command::new(git)
                .args(option)
                .arg("version")
                .stdin(std::process::Stdio::null())
                .suppress_windows_console(),
            GIT_SNAPSHOT_TIMEOUT,
        )
        .filter(|output| output.status.success())
    };
    if version(Some("--no-lazy-fetch")).is_some() {
        return Some(LazyFetchGuard::Flag);
    }
    let printed = version(None)?;
    is_git_2_39_1_or_later(&String::from_utf8_lossy(&printed.stdout))
        .then_some(LazyFetchGuard::NoPromisorRemote)
}

/// Whether `git version` printed 2.39.1 or a later version. An older git gets
/// no snapshot. `git status` reads the workspace's `.gitattributes`, and a
/// hostile one can overflow the attribute parser before 2.39.1
/// (CVE-2022-23521). Before 2.31 git also ignores the indexed `GIT_CONFIG_*`
/// variables that carry the filter overrides. A distribution's patched build
/// of an older version is refused too, as its number does not show the patch.
pub fn is_git_2_39_1_or_later(version: &str) -> bool {
    let mut numbers = version
        .trim()
        .strip_prefix("git version ")
        .unwrap_or_default()
        .split(|c: char| c == '.' || c.is_whitespace())
        .map(|number| number.parse::<u32>().ok());
    let (Some(Some(major)), Some(Some(minor)), Some(Some(patch))) =
        (numbers.next(), numbers.next(), numbers.next())
    else {
        return false;
    };
    (major, minor, patch) >= (2, 39, 1)
}

/// A `git` command for reading the workspace that the workspace's own
/// `.git/config` cannot use to run a program: an empty `core.fsmonitor`
/// disables the fsmonitor program on every git version, `log.showSignature`
/// off stops `git log` running `gpg.program`, and `--no-optional-locks` keeps
/// `git status` from writing the index, so no index hook runs. Disable every
/// configured filter too: status can run clean/process commands while hashing
/// tracked files. Reading config names itself does not execute those commands.
/// Git must not fetch an object the repository lacks from a promisor remote
/// either, which runs `core.sshCommand` or a remote helper: `guard` says how.
pub fn workspace_git(root: &Path, guard: LazyFetchGuard) -> Option<Command> {
    // Every config key git reads for this repository, from every scope and
    // included file. `git config` alone reads the file `GIT_CONFIG` names in
    // their place, so that variable is dropped. The keys are matched here,
    // byte for byte: git's own `--get-regexp` passes over a key holding a byte
    // its locale cannot decode, so a repository could hide a filter or a
    // remote from it.
    let listed = output_within_timeout(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["config", "--includes", "--null", "--name-only", "--list"])
            .env_remove("GIT_CONFIG")
            .stdin(std::process::Stdio::null())
            .suppress_windows_console(),
        GIT_SNAPSHOT_TIMEOUT,
    )?;
    // Refuse the snapshot if the listing fails, rather than running status
    // with an incomplete set of overrides.
    if !listed.status.success() {
        return None;
    }
    let mut drivers = BTreeMap::new();
    let mut filter_keys = 0;
    for key in listed.stdout.split(|byte| *byte == 0) {
        // Git prints a key as `section.name` or `section.subsection.name`, with
        // the section and the name in lower case. Only a subsection holds dots.
        let Some(first) = key.iter().position(|byte| *byte == b'.') else {
            continue;
        };
        let last = key.iter().rposition(|byte| *byte == b'.').unwrap_or(first);
        match (&key[..first], &key[last + 1..]) {
            (b"filter", b"clean" | b"smudge" | b"process" | b"required") => {
                if filter_keys == 256 {
                    return None;
                }
                filter_keys += 1;
                drivers.insert(std::str::from_utf8(&key[..last]).ok()?, ());
            }
            // Any of these keys can make a promisor remote, with or without a
            // remote name, and nothing on the command line switches one off.
            // Whatever its value, without `--no-lazy-fetch` the repository
            // gets no snapshot.
            (b"remote", b"promisor" | b"partialclonefilter") | (b"extensions", b"partialclone")
                if guard == LazyFetchGuard::NoPromisorRemote =>
            {
                return None;
            }
            _ => {}
        }
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["-c", "core.fsmonitor=", "-c", "log.showSignature=false"])
        .arg("--no-optional-locks")
        .stdin(std::process::Stdio::null())
        .suppress_windows_console();
    match guard {
        LazyFetchGuard::Flag => cmd.arg("--no-lazy-fetch"),
        // A second barrier. Git 2.39.4, 2.40.2, 2.41.1, 2.42.2, 2.43.4, 2.44.1
        // and the releases after each refuse to fetch when this is set; the
        // ones before them ignore it.
        LazyFetchGuard::NoPromisorRemote => cmd.env("GIT_NO_LAZY_FETCH", "1"),
    };
    let mut config_count = 0;
    for driver in drivers.keys() {
        for (setting, value) in [
            ("clean", ""),
            ("smudge", ""),
            ("process", ""),
            ("required", "false"),
        ] {
            // -c splits on '=', which is also valid inside a driver name.
            // Indexed environment entries keep the key separate from its value.
            cmd.env(
                format!("GIT_CONFIG_KEY_{config_count}"),
                format!("{driver}.{setting}"),
            )
            .env(format!("GIT_CONFIG_VALUE_{config_count}"), value);
            config_count += 1;
        }
    }
    cmd.env("GIT_CONFIG_COUNT", config_count.to_string())
        .env_remove("GIT_CONFIG_PARAMETERS");
    Some(cmd)
}

/// A `git` command for reading the repository at `root` that its own
/// configuration cannot use to run a program, as for the snapshot, or `None`
/// when the installed git is older than 2.39.1 or the configuration cannot
/// be read. Run it with [`output_within_timeout`].
pub fn read_only_git(root: &Path) -> Option<Command> {
    workspace_git(root, lazy_fetch_guard()?)
}

/// How long one git call of the snapshot may take. A config key can name a
/// FIFO (`include.path`, `mailmap.file`), and git then waits for a writer
/// that never comes.
pub const GIT_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// The command's exit status and standard output, or `None` when it cannot
/// start, outlasts `timeout` (it is then killed) or leaves its output open
/// past it.
///
/// shortcut: only git itself is killed, so a process git started and left
/// blocked keeps running, and so does the thread reading the output it
/// holds open; killing git's process group would end both.
pub fn output_within_timeout(cmd: &mut Command, timeout: Duration) -> Option<Output> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read whilst waiting, so a full pipe cannot stall git.
    let (read, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = read.send(stdout.read_to_end(&mut buf).map(|_| buf));
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    // A process git started can hold the output open after git exits.
    let stdout = output
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()?
        .ok()?;
    Some(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_git_2_39_1_or_later_gets_the_older_git_fallback() {
        for (version, expected) in [
            ("git version 2.39.1\n", true),
            ("git version 2.39.3 (Apple Git-146)\n", true),
            ("git version 2.40.0.rc0\n", true),
            ("git version 2.43.0.windows.1\n", true),
            ("git version 2.44.1\n", true),
            ("git version 3.0.0\n", true),
            ("git version 2.39.0\n", false),
            ("git version 2.39.0.56.g57e2c6ebbe\n", false),
            ("git version 2.34.1\n", false),
            ("git version 2.30.9\n", false),
            ("git version 2.39\n", false),
            ("git version unknown\n", false),
            ("2.44.1\n", false),
            ("", false),
        ] {
            assert_eq!(is_git_2_39_1_or_later(version), expected, "{version:?}");
        }
    }

    /// A `git` that never answers `git version` must not hold up the caller.
    #[cfg(unix)]
    #[test]
    fn a_stalled_git_version_probe_gives_no_guard_in_bounded_time() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join("git");
        std::fs::write(&git, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        assert_eq!(probe_lazy_fetch_guard(git.as_os_str()), None);
        assert!(started.elapsed() < 2 * GIT_SNAPSHOT_TIMEOUT + Duration::from_secs(5));
    }

    /// A process git leaves running can hold the output open after git exits.
    #[cfg(unix)]
    #[test]
    fn output_held_open_after_exit_is_given_up_on() {
        let started = Instant::now();
        let output = output_within_timeout(
            Command::new("sh").args(["-c", "sleep 30 & exit 0"]),
            GIT_SNAPSHOT_TIMEOUT,
        );
        assert!(output.is_none());
        assert!(started.elapsed() < GIT_SNAPSHOT_TIMEOUT + Duration::from_secs(5));
    }
}
