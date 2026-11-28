#[cfg(target_os = "linux")]
use std::os::unix::fs::symlink;

use hermes_core::git::GitRead;

use super::super::git::{
    classify, git, repo_dir, repo_status, rev_parse_args, untracked_lines, valid_ref, walk_entries,
    DIFF_ARGS, STATUS_ARGS, UNTRACKED_LINE_MAX_BYTES,
};
use super::super::roots::Roots;
#[cfg(target_os = "linux")]
use super::{at, roots};
use super::{commit_all, run, status_at, tree};

#[test]
fn only_plain_refs_reach_rev_parse() {
    for ok in [
        "HEAD",
        "main",
        "origin/main",
        "HEAD~2",
        "v1.2^{commit}",
        "abc123",
    ] {
        assert!(valid_ref(ok), "{ok}");
    }
    for bad in [
        "",
        "--output=x",
        "-n",
        "a..b",
        "a b",
        "HEAD:path",
        "a;b",
        &"x".repeat(257),
    ] {
        assert!(!valid_ref(bad), "{bad}");
    }
}

#[test]
fn porcelain_entries_parse_as_upstream() {
    let raw = "# branch.head main\0# branch.ab +2 -1\x001 .M N... 100644 100644 100644 a b src/a b.rs\x002 R. N... 100644 100644 100644 a b R100 new.rs\0old.rs\0u UU N... 1 2 3 4 a b c both.rs\0? new file.txt\0";
    let entries = walk_entries(raw);
    let paths: Vec<&str> = entries.iter().map(|(_, _, p)| p.as_str()).collect();
    assert_eq!(paths, ["src/a b.rs", "new.rs", "both.rs", "new file.txt"]);
    assert_eq!(classify('1', ".M", "x")["unstaged"], true);
    assert_eq!(classify('2', "R.", "x")["staged"], true);
}

#[test]
fn status_runs_no_program_a_hostile_repository_names() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    commit_all(&repo);
    let marker = t.outside.join("PWNED");
    let cmd = format!("touch {}", marker.display());
    for (key, value) in [
        ("core.fsmonitor", cmd.as_str()),
        ("filter.x.clean", cmd.as_str()),
        ("diff.external", cmd.as_str()),
        ("core.hooksPath", t.outside.to_str().unwrap()),
    ] {
        run(&repo, &["config", key, value]);
    }
    std::fs::write(repo.join(".gitattributes"), "* filter=x\n").unwrap();
    std::fs::write(repo.join("src/main.rs"), "fn main() { changed(); }\n").unwrap();
    std::fs::write(repo.join("fresh.txt"), "a\nb\n").unwrap();
    let status = status_at(&t, "proj");
    assert!(!marker.exists(), "a configured program ran");
    let status = status.expect("a status (git 2.39.1 or newer)");
    assert_eq!(status["branch"], "main");
    assert_eq!(status["untracked"], 2);
    assert!(status["added"].as_u64().unwrap() >= 2);
}

#[test]
fn discovery_stops_at_the_root() {
    let t = tree();
    run(t.outside.parent().unwrap(), &["init", "-q"]);
    // The repository is above the root: a folder inside the root is not in one.
    assert_eq!(status_at(&t, "proj"), None);
}

#[test]
fn a_submodule_cannot_run_its_own_filter_on_status() {
    let t = tree();
    let sub = t.outside.join("sub");
    std::fs::create_dir(&sub).unwrap();
    run(&sub, &["init", "-q", "-b", "main"]);
    std::fs::write(sub.join("a.txt"), "one\n").unwrap();
    commit_all(&sub);
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    run(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.to_str().unwrap(),
            "sub",
        ],
    );
    commit_all(&repo);
    // Hostile configuration inside the submodule's own git folder, as an extracted archive holds.
    let marker = t.outside.join("PWNED");
    let clean = format!("sh -c 'touch {}; cat'", marker.display());
    run(
        &repo,
        &[
            "config",
            "-f",
            ".git/modules/sub/config",
            "filter.evil.clean",
            &clean,
        ],
    );
    std::fs::write(repo.join("sub/.gitattributes"), "* filter=evil\n").unwrap();
    std::fs::write(repo.join("sub/a.txt"), "two\n").unwrap();
    let status = status_at(&t, "proj");
    assert!(!marker.exists(), "a submodule's filter ran");
    let status = status.expect("a status (git 2.39.1 or newer)");
    assert_eq!(status["branch"], "main");
}

#[cfg(target_os = "linux")]
#[test]
fn git_runs_in_the_folder_that_was_checked() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    commit_all(&repo);
    let other = t.root.join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("x.txt"), "x\n").unwrap();
    run(&other, &["init", "-q", "-b", "elsewhere"]);
    commit_all(&other);
    let dir = repo_dir(&at(&t, "proj"), &roots(&t), Some(&t.root)).unwrap();
    std::fs::rename(&repo, t.root.join("proj.old")).unwrap();
    symlink(&other, &repo).unwrap();
    let status = repo_status(&dir, &[]).expect("a status for the folder checked");
    assert_eq!(status["branch"], "main", "git followed a link swapped in");
}

#[test]
fn untracked_lines_are_counted_from_the_repository_top() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    commit_all(&repo);
    std::fs::write(repo.join("top.txt"), "a\nb\nc\n").unwrap();
    std::fs::write(repo.join("src/new.txt"), "x\ny\n").unwrap();
    let status = status_at(&t, "proj/src").expect("a status for the folder");
    assert_eq!(status["untracked"], 2);
    assert_eq!(status["added"], 5);
}

#[test]
fn every_git_call_carries_the_overrides_no_behaviour_test_can_see() {
    // Status runs no hook, and diff already passes --no-ext-diff, so these keys cannot fail a
    // behaviour test; this test fails if one is dropped.
    let t = tree();
    let mut cmd = git(&t.root, &t.root, GitRead::Status).expect("git 2.39.1 or later");
    assert!(
        cmd.get_envs().any(|(k, v)| {
            k == "GIT_CEILING_DIRECTORIES" && v == Some(t.root.parent().unwrap().as_os_str())
        }),
        "GIT_CEILING_DIRECTORIES missing"
    );
    cmd.args(STATUS_ARGS);
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let submodules: Vec<&String> = args
        .iter()
        .filter(|a| a.starts_with("--ignore-submodules="))
        .collect();
    assert_eq!(
        submodules.last().map(|a| a.as_str()),
        Some("--ignore-submodules=all"),
        "status's own `=all` must come last and outrank the guard: {args:?}"
    );
    for want in [
        "core.hooksPath=/dev/null",
        "diff.external=",
        "diff.submodule=short",
        "status.submoduleSummary=false",
    ] {
        assert!(args.iter().any(|a| a == want), "{want} missing: {args:?}");
    }
    assert!(STATUS_ARGS.contains(&"--ignore-submodules=all"));
    for want in [
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=all",
        "-z",
    ] {
        assert!(DIFF_ARGS.contains(&want), "{want}");
    }
    let wanted = rev_parse_args("main");
    assert_eq!(wanted[wanted.len() - 2..], ["--end-of-options", "main"]);
}

#[test]
fn untracked_line_counts_skip_what_is_not_small_readable_text() {
    let t = tree();
    let top = t.root.join("proj");
    let private = top.join("private");
    std::fs::create_dir(&private).unwrap();
    std::fs::write(private.join("x.txt"), "a\n").unwrap();
    std::fs::write(top.join("short.txt"), "a\nb").unwrap();
    std::fs::write(top.join("binary.txt"), b"a\0b\n").unwrap();
    std::fs::write(top.join(".env"), "A=1\n").unwrap();
    std::fs::write(
        top.join("big.txt"),
        vec![b'\n'; UNTRACKED_LINE_MAX_BYTES as usize + 1],
    )
    .unwrap();
    let refused = [private];
    let lines = |rel: &str| untracked_lines(&t.root, &top, rel, &refused);
    assert_eq!(
        lines("short.txt"),
        2,
        "a last line without a newline counts"
    );
    for rel in [
        "binary.txt",
        ".env",
        "big.txt",
        "private/x.txt",
        "src",
        "/etc/hostname",
        "../outside/secret.txt",
    ] {
        assert_eq!(lines(rel), 0, "{rel}");
    }
}

#[test]
fn status_names_the_remote_default_branch_and_the_distance_from_upstream() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "work"]);
    commit_all(&repo);
    run(
        &repo,
        &["remote", "add", "origin", "https://example.invalid/x.git"],
    );
    run(&repo, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
    run(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    run(&repo, &["config", "branch.work.remote", "origin"]);
    run(&repo, &["config", "branch.work.merge", "refs/heads/trunk"]);
    std::fs::write(repo.join("b.txt"), "b\n").unwrap();
    commit_all(&repo);
    let status = status_at(&t, "proj").expect("a status");
    assert_eq!(status["branch"], "work");
    assert_eq!(status["defaultBranch"], "trunk");
    assert_eq!(status["ahead"], 1);
    assert_eq!(status["behind"], 0);
}

#[test]
fn a_plain_origin_head_falls_back_to_the_local_default_branch() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    commit_all(&repo);
    run(&repo, &["update-ref", "refs/remotes/origin/HEAD", "HEAD"]);
    let status = status_at(&t, "proj").expect("a status");
    assert_eq!(status["defaultBranch"], "main");
}

#[test]
fn status_of_a_repository_without_a_commit_counts_its_untracked_lines() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "x\n").unwrap();
    let status = status_at(&t, "proj").expect("a status");
    assert_eq!(status["branch"], "main");
    // `a.txt` and the folder `src/`, whose lines are not counted.
    assert_eq!(status["untracked"], 2);
    assert_eq!(status["added"], 1);
}

#[test]
fn core_worktree_and_gitfile_escapes_are_refused() {
    let t = tree();
    std::fs::write(t.hermes.join("gateway-connection-tokens.json"), "{}").unwrap();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    commit_all(&repo);
    run(
        &repo,
        &["config", "core.worktree", t.hermes.to_str().unwrap()],
    );
    assert_eq!(status_at(&t, "proj"), None, "core.worktree into .hermes");

    run(
        &repo,
        &["config", "core.worktree", t.root.to_str().unwrap()],
    );
    run(&repo, &["config", "status.showUntrackedFiles", "all"]);
    assert_eq!(status_at(&t, "proj"), None, "core.worktree to root");

    run(&repo, &["config", "--unset", "core.worktree"]);
    run(&repo, &["config", "--unset", "status.showUntrackedFiles"]);
    let wt = t.root.join("wt");
    run(
        &repo,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );
    let wt_status = status_at(&t, "wt").expect("linked worktree inside root");
    assert_eq!(wt_status["branch"], "feat");

    let ext = t.outside.join("ext");
    std::fs::create_dir(&ext).unwrap();
    run(&ext, &["init", "-q", "-b", "secret"]);
    std::fs::write(ext.join("s.txt"), "x\n").unwrap();
    commit_all(&ext);
    let linked = t.root.join("linked");
    std::fs::create_dir(&linked).unwrap();
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", ext.join(".git").display()),
    )
    .unwrap();
    assert_eq!(status_at(&t, "linked"), None, "gitfile outside root");
}

#[test]
fn a_worktree_commondir_escaping_the_root_or_hermes_home_is_refused() {
    let t = tree();
    let ext = t.outside.join("ext");
    std::fs::create_dir(&ext).unwrap();
    run(&ext, &["init", "-q", "-b", "secret"]);
    std::fs::write(ext.join("s.txt"), "x\n").unwrap();
    commit_all(&ext);
    let hermes_repo = t.hermes.join("secret_repo");
    std::fs::create_dir(&hermes_repo).unwrap();
    run(&hermes_repo, &["init", "-q", "-b", "hermes-secret"]);
    std::fs::write(hermes_repo.join("h.txt"), "h\n").unwrap();
    commit_all(&hermes_repo);
    for (name, target_git, branch) in [
        ("commondir-outside", ext.join(".git"), "secret"),
        (
            "commondir-hermes",
            hermes_repo.join(".git"),
            "hermes-secret",
        ),
    ] {
        let fake_git = t.root.join(format!("{name}-git"));
        let wt_dir = t.root.join(name);
        std::fs::create_dir(&fake_git).unwrap();
        std::fs::create_dir(&wt_dir).unwrap();
        std::fs::write(fake_git.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
        std::fs::write(
            fake_git.join("gitdir"),
            format!("{}\n", wt_dir.join(".git").display()),
        )
        .unwrap();
        std::fs::write(
            fake_git.join("commondir"),
            format!("{}\n", target_git.display()),
        )
        .unwrap();
        std::fs::write(
            wt_dir.join(".git"),
            format!("gitdir: {}\n", fake_git.display()),
        )
        .unwrap();
        assert_eq!(status_at(&t, name), None, "{name}");
    }
}

#[test]
fn a_newline_in_a_directory_name_cannot_spoof_repo_top() {
    let t = tree();
    let ext = t.outside.join("ext");
    std::fs::create_dir(&ext).unwrap();
    run(&ext, &["init", "-q", "-b", "secret"]);
    std::fs::write(ext.join("s.txt"), "x\n").unwrap();
    commit_all(&ext);
    std::fs::create_dir_all(t.root.join("a/.git")).unwrap();
    let r_no_slash = t.root.to_str().unwrap().trim_start_matches('/');
    let nl_rel = format!("a/\n/{r_no_slash}/a/.git/\n/{r_no_slash}/a/.git");
    let nl_dir = t.root.join(&nl_rel);
    std::fs::create_dir_all(&nl_dir).unwrap();
    std::fs::write(
        nl_dir.join(".git"),
        format!("gitdir: {}\n", ext.join(".git").display()),
    )
    .unwrap();
    assert_eq!(
        status_at(&t, &nl_rel),
        None,
        "newline-in-path repo_top spoof"
    );
}

#[test]
fn a_repository_at_the_root_omits_refused_entries_and_numstat_counts() {
    let t = tree();
    run(&t.root, &["init", "-q", "-b", "main"]);
    commit_all(&t.root);
    std::fs::write(t.hermes.join("auth.json"), "1\n2\n3\n4\n5\n6\n").unwrap();
    std::fs::write(t.root.join("proj/src/main.rs"), "fn main() {}\n// extra\n").unwrap();
    let status = status_at(&t, "proj").expect("real repo at root");
    let paths: Vec<&str> = status["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert!(
        !paths.iter().any(|p| p.starts_with(".hermes")),
        "refused tree listed in status: {paths:?}"
    );
    assert_eq!(
        status["added"], 1,
        "refused tree tracked lines leaked into added: {status}"
    );
}

#[test]
fn a_root_under_a_colon_parent_still_stops_discovery_above_it() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let colon_parent = base.join("a:b");
    let root = colon_parent.join("home");
    let proj = root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    run(&colon_parent, &["init", "-q", "-b", "outer"]);
    let r = Roots::new(std::slice::from_ref(&root), &[]);
    let repo = repo_dir(proj.to_str().unwrap(), &r, Some(&root)).unwrap();
    assert_eq!(repo_status(&repo, &[]), None);
}

#[test]
fn a_renamed_all_digit_file_does_not_inflate_line_counts() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("7"), "one\n").unwrap();
    commit_all(&repo);
    run(&repo, &["mv", "7", "8"]);
    let status = status_at(&t, "proj").expect("a status");
    assert_eq!(status["added"], 0, "rename path inflated added: {status}");
    assert_eq!(status["removed"], 0);
}

#[test]
fn a_filename_starting_with_a_tab_does_not_swallow_following_numstat_records() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    for name in ["\ta.txt", "b.txt", "c.txt"] {
        std::fs::write(repo.join(name), "one\n").unwrap();
    }
    commit_all(&repo);
    for name in ["\ta.txt", "b.txt", "c.txt"] {
        std::fs::write(repo.join(name), "one\ntwo\n").unwrap();
    }
    let status = status_at(&t, "proj").expect("a status");
    assert_eq!(
        status["added"], 3,
        "leading-tab filename swallowed numstat records: {status}"
    );
    assert_eq!(status["removed"], 0);
}
