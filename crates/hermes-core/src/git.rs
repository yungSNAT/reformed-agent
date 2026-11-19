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
/// A repository whose own config names a user (see [`names_a_user`]) gets
/// no command.
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
            .args(["config", "--includes", "--null", "--show-scope", "--list"])
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
    // Each entry is its scope, then its key and, unless it has none, its value.
    let mut fields = listed.stdout.split(|byte| *byte == 0);
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        let (key, value) = match entry.iter().position(|byte| *byte == b'\n') {
            Some(newline) => (&entry[..newline], Some(&entry[newline + 1..])),
            None => (entry, None),
        };
        // The repository's own config, with the files it includes, cannot
        // make git look up a user. The machine's and the user's config are
        // trusted.
        if matches!(scope, b"local" | b"worktree") && names_a_user(key, value) {
            return None;
        }
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

/// Whether git would expand the value, or an include's `gitdir` or
/// `worktree` condition, as a path that begins `~name`: git finds that user's
/// home through `getpwnam`, which can ask a directory service about a name
/// the repository chose. Any value counts, as git takes many keys as paths.
/// Git tests an `includeIf` condition whatever the variable under it.
///
/// shortcut: git follows the includes, and expands their paths and
/// conditions, as soon as it starts in the repository, so the listing itself
/// still makes those lookups; this stops the ones status, log and the other
/// commands would make for every other path. Ending those too means reading
/// the repository's config files without git's repository discovery and
/// include traversal.
fn names_a_user(key: &[u8], value: Option<&[u8]>) -> bool {
    let condition = key
        .strip_prefix(b"includeif.")
        .and_then(|rest| {
            rest.iter()
                .rposition(|byte| *byte == b'.')
                .map(|dot| &rest[..dot])
        })
        .and_then(|condition| {
            [&b"gitdir:"[..], b"gitdir/i:", b"worktree:", b"worktree/i:"]
                .iter()
                .find_map(|prefix| condition.strip_prefix(*prefix))
        });
    value.into_iter().chain(condition).any(|path| {
        // Git 2.52 and later take a path marked optional.
        let path = path.strip_prefix(b":(optional)").unwrap_or(path);
        path.len() > 1 && path[0] == b'~' && path[1] != b'/'
    })
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

    /// A user name no machine has, so a lookup cannot find a home for it.
    const USER: &str = "r2058-croft";

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    /// A new repository at `repo` that gets a command before any change.
    fn new_repo(repo: &Path) {
        std::fs::create_dir_all(repo).unwrap();
        git(repo, &["init", "-q"]);
        assert!(workspace_git(repo, LazyFetchGuard::NoPromisorRemote).is_some());
    }

    /// Git expands a path that begins `~name` through `getpwnam`, which can
    /// ask a directory service for the name the repository chose.
    #[test]
    fn a_user_name_in_the_repository_config_gets_no_command() {
        for (key, value) in [
            ("core.excludesFile".to_string(), format!("~{USER}/x")),
            (
                "core.hooksPath".to_string(),
                format!(":(optional)~{USER}/x"),
            ),
            ("mailmap.file".to_string(), format!("~{USER}")),
            ("include.path".to_string(), format!("~{USER}/x")),
            (format!("includeIf.gitdir:~{USER}/.path"), "x".to_string()),
            (format!("includeIf.gitdir/i:~{USER}/.path"), "x".to_string()),
            // Git tests the condition before it looks at the variable.
            (format!("includeIf.gitdir:~{USER}/.note"), "x".to_string()),
            (format!("includeIf.gitdir/i:~{USER}/.note"), "x".to_string()),
            (format!("includeIf.worktree:~{USER}/.path"), "x".to_string()),
            (
                format!("includeIf.worktree/i:~{USER}/.note"),
                "x".to_string(),
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            new_repo(&repo);
            git(&repo, &["config", &key, &value]);
            assert!(
                workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_none(),
                "{key} = {value}"
            );
        }
    }

    #[test]
    fn the_home_directory_is_not_a_user_name() {
        for value in ["~/x", "~"] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            new_repo(&repo);
            git(&repo, &["config", "core.excludesFile", value]);
            assert!(
                workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_some(),
                "{value}"
            );
        }
    }

    /// Git reports a value from an included file under the scope of the
    /// file that includes it.
    #[test]
    fn a_user_name_in_an_included_file_gets_no_command() {
        for (included, kept) in [
            (format!("[core]\n\texcludesFile = ~{USER}/x\n"), false),
            ("[core]\n\texcludesFile = ~/x\n".to_string(), true),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            new_repo(&repo);
            std::fs::write(repo.join("shared.config"), &included).unwrap();
            // Relative to the directory of the file that includes it, `.git`.
            git(&repo, &["config", "include.path", "../shared.config"]);
            assert_eq!(
                workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_some(),
                kept,
                "{included}"
            );
        }
    }

    /// A linked worktree reads the main repository's config.
    #[test]
    fn a_user_name_in_a_linked_worktree_s_config_gets_no_command() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        new_repo(&repo);
        git(
            &repo,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        let linked = tmp.path().join("linked");
        git(&repo, &["worktree", "add", "-q", linked.to_str().unwrap()]);
        assert!(workspace_git(&linked, LazyFetchGuard::NoPromisorRemote).is_some());
        git(
            &repo,
            &["config", "core.excludesFile", &format!("~{USER}/x")],
        );
        assert!(workspace_git(&linked, LazyFetchGuard::NoPromisorRemote).is_none());
    }

    /// With `extensions.worktreeConfig`, each worktree has a config of its
    /// own, which git lists under the `worktree` scope.
    #[test]
    fn a_user_name_in_a_worktree_s_own_config_gets_no_command() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        new_repo(&repo);
        git(
            &repo,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        git(&repo, &["config", "extensions.worktreeConfig", "true"]);
        let linked = tmp.path().join("linked");
        git(&repo, &["worktree", "add", "-q", linked.to_str().unwrap()]);
        git(
            &linked,
            &[
                "config",
                "--worktree",
                "core.excludesFile",
                &format!("~{USER}/x"),
            ],
        );
        assert!(workspace_git(&linked, LazyFetchGuard::NoPromisorRemote).is_none());
        assert!(workspace_git(&repo, LazyFetchGuard::NoPromisorRemote).is_some());
    }
}
