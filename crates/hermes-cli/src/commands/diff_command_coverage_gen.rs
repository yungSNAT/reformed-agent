use super::*;
use std::fs;

#[test]
fn renders_staged_and_all_labels_with_the_requested_detail() {
    let mut diff = WorkingDiff {
        stat: "tracked.txt | 1 +".to_string(),
        diff: "+added\n".to_string(),
        untracked: Vec::new(),
    };

    let staged = parse_diff_args(&["staged"]).unwrap();
    assert_eq!(
        render_working_diff(&staged, &diff),
        "Staged:\ntracked.txt | 1 +\n\n+added"
    );

    diff.untracked.push("new.txt".to_string());
    let all = parse_diff_args(&["all", "--stat"]).unwrap();
    assert_eq!(
        render_working_diff(&all, &diff),
        "All (vs HEAD):\ntracked.txt | 1 +\nUntracked:\n  + new.txt"
    );
}

#[test]
fn untracked_patches_skip_missing_and_non_file_entries_and_respect_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<_> = (0..MAX_UNTRACKED_DIFFS + 2)
        .map(|index| {
            let shown = format!("entry-{index}");
            let path = dir.path().join(&shown);
            if index == 1 {
                fs::create_dir(&path).unwrap();
            } else if index >= MAX_UNTRACKED_DIFFS - 1 {
                fs::write(&path, "visible\n").unwrap();
            }
            Untracked {
                path: Some(OsString::from(&shown)),
                shown,
            }
        })
        .collect();

    let last_included = files[MAX_UNTRACKED_DIFFS - 1].path.as_deref().unwrap();
    let output = untracked_diff(dir.path(), &files, |path| {
        assert_eq!(path, last_included, "unexpected patch request");
        Ok(Git::Done(Some(1), b"+visible\n".to_vec()))
    })
    .unwrap();

    assert_eq!(output, "+visible\n... (2 more untracked files not shown)");
}

// Run Git-facing assertions in a fresh test process so they can control Git
// discovery and configuration without changing the parent process environment.
fn with_isolated_git(test: &str, git_on_path: bool, check: impl FnOnce(&Path)) {
    const CHILD_WORKTREE: &str = "HERMES_DIFF_COVERAGE_CHILD_WORKTREE";
    const COMPLETED: &str = "isolated diff assertions completed";

    if let Some(worktree) = std::env::var_os(CHILD_WORKTREE) {
        check(Path::new(&worktree));
        println!("{COMPLETED}");
        return;
    }

    let _env = crate::test_env_lock::lock();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let worktree = dir.path().join("worktree");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&worktree).unwrap();

    let (_, module) = module_path!().split_once("::").unwrap();
    let test_name = format!("{module}::{test}");
    let path = if git_on_path {
        std::env::var_os("PATH").unwrap_or_default()
    } else {
        home.as_os_str().to_os_string()
    };

    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", &test_name, "--nocapture"])
        .current_dir(&worktree)
        .env_clear()
        .env("PATH", path)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env(CHILD_WORKTREE, &worktree);

    for key in [
        "LLVM_PROFILE_FILE",
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "SystemRoot",
        "WINDIR",
        "PATHEXT",
    ] {
        if let Some(value) = std::env::var_os(key) {
            child.env(key, value);
        }
    }

    let output = child.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.lines().any(|line| line == COMPLETED),
        "isolated test {test_name} ended with {}:\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn git_failures_have_command_specific_results() {
    with_isolated_git("git_failures_have_command_specific_results", true, |cwd| {
        let init = Command::new("git")
            .current_dir(cwd)
            .args(["init", "--quiet", "--template="])
            .output()
            .unwrap();
        assert!(
            init.status.success(),
            "git init: {}",
            String::from_utf8_lossy(&init.stderr)
        );

        // A new repository is a worktree, but there is no HEAD to diff against.
        let all = parse_diff_args(&["all", "--stat"]).unwrap();
        assert_eq!(
            collect_working_diff(cwd, &all).unwrap_err(),
            "git diff failed (exit 128)."
        );

        // An embedded NUL prevents the diff child from starting after the probe
        // succeeds. The bounded-output helper reports no output in this case.
        let invalid_path = DiffRequest {
            mode: DiffMode::Working,
            stat_only: true,
            paths: vec!["invalid\0path".to_string()],
        };
        assert_eq!(
            collect_working_diff(cwd, &invalid_path).unwrap_err(),
            "git diff timed out."
        );

        fs::write(cwd.join("untracked.txt"), "untracked\n").unwrap();
        let before = untracked_files(cwd).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].shown, "untracked.txt");

        // ls-files needs the index to distinguish tracked from untracked files.
        fs::write(cwd.join(".git").join("index"), b"bad index").unwrap();
        assert!(untracked_files(cwd).unwrap().is_empty());
    });
}

#[cfg(unix)]
#[test]
fn untracked_listing_propagates_git_discovery_failure() {
    with_isolated_git(
        "untracked_listing_propagates_git_discovery_failure",
        false,
        |cwd| {
            assert_eq!(
                untracked_files(cwd).unwrap_err(),
                concat!(
                    "git could not be run safely here: it is missing, older than 2.39.1, ",
                    "or this repository's configuration could not be read or names a user."
                )
            );
        },
    );
}
