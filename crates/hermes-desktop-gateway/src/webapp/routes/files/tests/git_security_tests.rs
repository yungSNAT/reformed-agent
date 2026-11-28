use std::os::unix::fs::{symlink, PermissionsExt};

use hermes_core::git::GitRead;

use super::super::git::{
    git_pathspecs, numstat_counts, open_repo, repo_dir, repo_status, repo_status_pinned,
    rev_parse_args,
};
use super::super::roots::Roots;
use super::{at, commit_all, roots, run, status_at, tree};

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
    run(&ext, &["init", "-q", "-b", "ext-branch"]);
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
    run(&ext, &["init", "-q", "-b", "ext-branch"]);
    std::fs::write(ext.join("s.txt"), "x\n").unwrap();
    commit_all(&ext);
    let hermes_repo = t.hermes.join("inner_repo");
    std::fs::create_dir(&hermes_repo).unwrap();
    run(&hermes_repo, &["init", "-q", "-b", "hermes-branch"]);
    std::fs::write(hermes_repo.join("h.txt"), "h\n").unwrap();
    commit_all(&hermes_repo);
    for (name, target_git, branch) in [
        ("commondir-outside", ext.join(".git"), "ext-branch"),
        (
            "commondir-hermes",
            hermes_repo.join(".git"),
            "hermes-branch",
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
    run(&ext, &["init", "-q", "-b", "ext-branch"]);
    std::fs::write(ext.join("s.txt"), "x\n").unwrap();
    commit_all(&ext);
    let decoy = t.root.join("a");
    std::fs::create_dir(&decoy).unwrap();
    run(&decoy, &["init", "-q", "-b", "decoy"]);
    std::fs::write(decoy.join("d.txt"), "d\n").unwrap();
    commit_all(&decoy);
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

#[test]
fn tracked_sensitive_files_are_excluded_from_git_execution_and_output() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join(".aws")).unwrap();
    std::fs::create_dir_all(repo.join(".ssh")).unwrap();
    std::fs::write(repo.join(".env"), "A=1\n").unwrap();
    std::fs::write(repo.join(".aws/credentials"), "k1\n").unwrap();
    std::fs::write(repo.join(".ssh/id_ed25519"), "p1\n").unwrap();
    commit_all(&repo);

    std::fs::write(repo.join("src/main.rs"), "fn main() {}\n// ok\n").unwrap();
    std::fs::write(repo.join(".env"), "A=1\nB=2\nC=3\n").unwrap();
    std::fs::write(repo.join(".aws/credentials"), "k1\nk2\nk3\n").unwrap();
    std::fs::write(repo.join(".ssh/id_ed25519"), "p1\np2\np3\n").unwrap();
    // Unreadable modified tracked `.env`: if git tried to open it, diff would fail with EACCES.
    std::fs::set_permissions(repo.join(".env"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let locked_status = status_at(&t, "proj");
    std::fs::set_permissions(repo.join(".env"), std::fs::Permissions::from_mode(0o600)).unwrap();
    for status in [
        locked_status.expect("locked status"),
        status_at(&t, "proj").expect("status"),
    ] {
        let paths: Vec<&str> = status["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f["path"].as_str())
            .collect();
        assert_eq!(paths, ["src/main.rs"]);
        assert_eq!(status["changed"], 1);
        assert_eq!(status["added"], 1);
        assert_eq!(status["removed"], 0);
    }
}

#[test]
fn git_pathspecs_and_numstat_filter_sensitive_and_refused_paths() {
    let t = tree();
    let specs = git_pathspecs(&t.root, std::slice::from_ref(&t.hermes));
    for want in [
        ":(top,exclude,literal).hermes",
        ":(top,exclude,icase,glob)**/.env",
        ":(top,exclude,icase,glob)**/.aws/**",
        ":(top,exclude,icase,glob)**/id_ed25519",
    ] {
        assert!(specs.iter().any(|s| s == want), "{want} missing");
    }
    assert_eq!(
        numstat_counts(
            "5\t2\t.env\x003\t1\t.hermes/x\x001\t0\tsrc/main.rs\x00",
            &t.root,
            std::slice::from_ref(&t.hermes),
        ),
        (1, 0)
    );
}

#[test]
fn swapping_dot_git_after_open_repo_cannot_redirect_status_or_rev_parse() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    commit_all(&repo);
    let marker = t.outside.join("PWNED");
    let clean = format!("sh -c 'touch {}; cat'", marker.display());
    run(&repo, &["config", "filter.evil.clean", &clean]);
    std::fs::write(repo.join(".gitattributes"), "* filter=evil\n").unwrap();

    let ext = t.outside.join("ext");
    std::fs::create_dir(&ext).unwrap();
    run(&ext, &["init", "-q", "-b", "ext-branch"]);
    std::fs::write(ext.join("s.txt"), "x\n").unwrap();
    commit_all(&ext);

    let dir = repo_dir(&at(&t, "proj"), &roots(&t), Some(&t.root)).unwrap();
    let pinned = open_repo(&dir, std::slice::from_ref(&t.hermes)).unwrap();
    let before = pinned
        .line(GitRead::RevParse, &rev_parse_args("HEAD"))
        .unwrap();
    std::fs::rename(repo.join(".git"), repo.join(".git.old")).unwrap();
    std::fs::write(
        repo.join(".git"),
        format!("gitdir: {}\n", ext.join(".git").display()),
    )
    .unwrap();
    std::fs::write(repo.join("src/main.rs"), "fn main() { changed(); }\n").unwrap();

    let status = repo_status_pinned(&pinned, std::slice::from_ref(&t.hermes))
        .expect("pinned status after .git swap");
    assert!(!marker.exists(), "filter ran after .git swap");
    assert_eq!(status["branch"], "main", "status followed swapped .git");
    let after = pinned
        .line(GitRead::RevParse, &rev_parse_args("HEAD"))
        .unwrap();
    assert_eq!(after, before, "rev-parse followed swapped .git");
}

#[test]
fn swapping_commondir_after_open_repo_is_refused() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    commit_all(&repo);
    let wt = t.root.join("wt");
    run(
        &repo,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );
    let ext = t.outside.join("ext");
    std::fs::create_dir(&ext).unwrap();
    run(&ext, &["init", "-q", "-b", "feat"]);
    std::fs::write(ext.join("s.txt"), "x\n").unwrap();
    commit_all(&ext);

    let dir = repo_dir(&at(&t, "wt"), &roots(&t), Some(&t.root)).unwrap();
    let pinned = open_repo(&dir, std::slice::from_ref(&t.hermes)).unwrap();
    let wt_git_text = std::fs::read_to_string(wt.join(".git")).unwrap();
    let wt_git = std::path::PathBuf::from(wt_git_text.trim().strip_prefix("gitdir: ").unwrap());
    std::fs::write(
        wt_git.join("commondir"),
        format!("{}\n", ext.join(".git").display()),
    )
    .unwrap();
    assert_eq!(repo_status_pinned(&pinned, &[]), None);
    assert_eq!(
        pinned.line(GitRead::RevParse, &rev_parse_args("HEAD")),
        None
    );

    std::fs::remove_file(wt_git.join("commondir")).unwrap();
    symlink(repo.join(".git"), wt_git.join("commondir")).unwrap();
    assert_eq!(repo_status_pinned(&pinned, &[]), None);
}
