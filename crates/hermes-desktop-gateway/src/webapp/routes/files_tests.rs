use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::symlink;

use super::*;

struct Tree {
    _dir: tempfile::TempDir,
    root: PathBuf,
    hermes: PathBuf,
    outside: PathBuf,
}

/// A home folder with a Hermes home inside it, and a folder outside both.
fn tree() -> Tree {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let root = base.join("home");
    let hermes = root.join(".hermes");
    let outside = base.join("outside");
    for d in [
        &root,
        &hermes,
        &outside,
        &root.join("proj/src"),
        &root.join(".ssh"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(root.join("proj/src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(hermes.join("auth.json"), "{}").unwrap();
    std::fs::write(outside.join("secret.txt"), "outside").unwrap();
    std::fs::write(root.join(".ssh/id_ed25519"), "key").unwrap();
    Tree {
        _dir: dir,
        root,
        hermes,
        outside,
    }
}

fn roots(t: &Tree) -> Roots {
    Roots::new(
        std::slice::from_ref(&t.root),
        std::slice::from_ref(&t.hermes),
    )
}

fn at(t: &Tree, rel: &str) -> String {
    t.root.join(rel).to_string_lossy().into_owned()
}

#[test]
fn credential_names_and_folders_are_sensitive_anywhere() {
    for p in [
        "/h/.env",
        "/h/.env.local",
        "/h/x/.envrc",
        "/h/.ssh",
        "/h/.ssh/id_rsa",
        "/h/.AWS/config",
        "/p/.config/gh/hosts.yml",
        "/h/.hermes/webapp/session-token",
        "/h/gateway-connections.json",
        "/h/Auth.JSON",
    ] {
        assert!(sensitive(Path::new(p)), "{p}");
    }
    for p in [
        "/h/proj/main.rs",
        "/h/.config/nvim/init.lua",
        "/h/envelope.txt",
    ] {
        assert!(!sensitive(Path::new(p)), "{p}");
    }
}

#[test]
fn paths_outside_the_roots_or_badly_formed_are_refused() {
    let t = tree();
    let r = roots(&t);
    assert_eq!(
        resolve("proj", &r, Some(&t.root)).unwrap_err(),
        Refusal::BadPath("Path must be absolute")
    );
    assert_eq!(
        resolve("/tmp\0x", &r, Some(&t.root)).unwrap_err(),
        Refusal::BadPath("Invalid path")
    );
    assert_eq!(
        resolve(&t.outside.to_string_lossy(), &r, Some(&t.root)).unwrap_err(),
        Refusal::Outside
    );
    assert_eq!(
        resolve(&at(&t, "proj/../../outside/secret.txt"), &r, Some(&t.root)).unwrap_err(),
        Refusal::Outside
    );
    assert_eq!(
        resolve("/", &r, Some(&t.root)).unwrap_err(),
        Refusal::Outside
    );
    assert_eq!(
        resolve(&at(&t, "nope"), &r, Some(&t.root)).unwrap_err(),
        Refusal::NotFound
    );
    assert!(resolve("~/proj", &r, Some(&t.root)).is_ok());
}

#[test]
fn the_hermes_home_and_credential_folders_are_refused_inside_the_root() {
    let t = tree();
    let r = roots(&t);
    for p in [".hermes", ".hermes/auth.json", ".ssh", ".ssh/id_ed25519"] {
        assert_eq!(
            resolve(&at(&t, p), &r, Some(&t.root)).unwrap_err(),
            Refusal::Sensitive,
            "{p}"
        );
    }
    // A link with an innocent name to a secret is judged by where it leads.
    symlink(t.root.join(".ssh/id_ed25519"), t.root.join("notes.txt")).unwrap();
    assert_eq!(
        resolve(&at(&t, "notes.txt"), &r, Some(&t.root)).unwrap_err(),
        Refusal::Sensitive
    );
}

#[test]
fn a_link_out_of_the_root_is_refused_and_one_inside_it_works() {
    let t = tree();
    let r = roots(&t);
    symlink(&t.outside, t.root.join("escape")).unwrap();
    assert_eq!(
        resolve(&at(&t, "escape/secret.txt"), &r, Some(&t.root)).unwrap_err(),
        Refusal::Outside
    );
    symlink(t.root.join("proj"), t.root.join("shortcut")).unwrap();
    let ok = resolve(&at(&t, "shortcut/src/main.rs"), &r, Some(&t.root)).unwrap();
    assert!(matches!(open_resolved(&ok), Ok(Opened::File(..))));
}

#[test]
fn a_link_swapped_in_after_the_check_is_not_followed() {
    let t = tree();
    let r = roots(&t);
    let checked = resolve(&at(&t, "proj/src/main.rs"), &r, Some(&t.root)).unwrap();
    std::fs::rename(t.root.join("proj/src"), t.root.join("proj/src.old")).unwrap();
    symlink(&t.outside, t.root.join("proj/src")).unwrap();
    std::fs::write(t.outside.join("main.rs"), "outside bytes").unwrap();
    assert!(
        open_resolved(&checked).is_err(),
        "followed a swapped link out of the root"
    );
}

#[test]
fn a_fifo_is_refused_without_blocking() {
    let t = tree();
    let fifo = t.root.join("pipe");
    let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
    // SAFETY: c is a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let resolved = resolve(&fifo.to_string_lossy(), &roots(&t), Some(&t.root)).unwrap();
    assert!(matches!(open_resolved(&resolved), Err(Refusal::NotAFile)));
}

#[test]
fn a_listing_hides_build_and_credential_entries_and_lists_folders_first() {
    let t = tree();
    for d in ["node_modules", "Zeta", "alpha"] {
        std::fs::create_dir(t.root.join("proj").join(d)).unwrap();
    }
    std::fs::write(t.root.join("proj/.env"), "K=V").unwrap();
    std::fs::write(t.root.join("proj/b.txt"), "").unwrap();
    symlink(&t.outside, t.root.join("proj/link")).unwrap();
    let resolved = resolve(&at(&t, "proj"), &roots(&t), Some(&t.root)).unwrap();
    let Ok(Opened::Dir(dir)) = open_resolved(&resolved) else {
        panic!("not a folder")
    };
    let (entries, truncated) = list_entries(dir, &resolved.path).unwrap();
    let names: Vec<(&str, bool)> = entries
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap(),
                e["isDirectory"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            ("alpha", true),
            ("src", true),
            ("Zeta", true),
            ("b.txt", false),
            ("link", false)
        ]
    );
    assert!(!truncated);
}

#[test]
fn reads_are_capped_by_the_bytes_read() {
    let t = tree();
    std::fs::write(t.root.join("big.bin"), vec![1u8; 11]).unwrap();
    let file = std::fs::File::open(t.root.join("big.bin")).unwrap();
    assert_eq!(read_capped(file, 10).unwrap_err(), Refusal::TooLarge);
    let file = std::fs::File::open(t.root.join("big.bin")).unwrap();
    assert_eq!(read_capped(file, 11).unwrap().len(), 11);
}

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

fn run(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn status_runs_no_program_a_hostile_repository_names() {
    let t = tree();
    let repo = t.root.join("proj");
    run(&repo, &["init", "-q", "-b", "main"]);
    run(
        &repo,
        &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"],
    );
    run(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "one",
        ],
    );
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
fn only_local_browser_and_native_callers_are_served() {
    let lo4 = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5));
    let lo6 = Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 5));
    let mapped = Some(SocketAddr::new(
        IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()),
        5,
    ));
    let lan = Some(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9)),
        5,
    ));
    for peer in [lo4, lo6, mapped] {
        assert!(local_only(Caller::Browser { generation: 1 }, peer).is_ok());
        assert!(local_only(Caller::Native, peer).is_ok());
    }
    assert!(local_only(Caller::Token, lo4).is_err());
    assert!(local_only(Caller::Browser { generation: 1 }, lan).is_err());
    assert!(local_only(Caller::Native, None).is_err());
}

// ---- Review round 1 -------------------------------------------------------------------------

fn commit_all(dir: &Path) {
    run(
        dir,
        &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"],
    );
    run(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "one",
        ],
    );
}

/// The status `git/status` answers for `rel` under the tree's root.
fn status_at(t: &Tree, rel: &str) -> Option<Value> {
    let dir = repo_dir(&at(t, rel), &roots(t), Some(&t.root)).ok()?;
    repo_status(&dir, std::slice::from_ref(&t.hermes))
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
    let Some(status) = status else { return }; // git older than 2.39.1: nothing ran either
    assert_eq!(status["branch"], "main");
}

#[test]
fn a_path_outside_the_roots_answers_alike_whether_or_not_it_exists() {
    use std::os::unix::fs::PermissionsExt;
    let t = tree();
    let r = roots(&t);
    symlink(&t.outside, t.root.join("escape")).unwrap();
    let locked = t.outside.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    for p in [
        t.outside.clone(),
        t.outside.join("secret.txt"),
        t.outside.join("missing"),
        t.outside.join("missing/deeper"),
        locked.join("x"),
        t.root.join("escape/missing"),
        t.root.join("escape/secret.txt"),
        t.root.join("proj/../../outside/missing"),
        PathBuf::from("/nonexistent-webapp-test/x"),
    ] {
        assert_eq!(
            resolve(&p.to_string_lossy(), &r, Some(&t.root)).unwrap_err(),
            Refusal::Outside,
            "{}",
            p.display()
        );
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        resolve(&at(&t, "nope"), &r, Some(&t.root)).unwrap_err(),
        Refusal::NotFound
    );
    assert_eq!(
        resolve(&at(&t, ".hermes/nope"), &r, Some(&t.root)).unwrap_err(),
        Refusal::Sensitive
    );
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
fn browser_profiles_and_key_stores_are_sensitive() {
    for p in [
        "/h/.mozilla/firefox/p.default/cookies.sqlite",
        "/h/snap/firefox/common/.mozilla/firefox/p/logins.json",
        // Folders alone, with names no file rule matches.
        "/h/.mozilla/firefox/p.default/prefs.js",
        "/h/.password-store/notes.txt",
        "/h/.config/google-chrome/Default/Cookies",
        "/h/.config/chromium/Default/Login Data",
        "/h/.config/BraveSoftware/Brave-Browser/Default/History",
        "/h/.config/microsoft-edge/Default/Preferences",
        "/h/.password-store/bank.gpg",
        "/h/.local/share/keyrings/login.keyring",
        "/Users/u/Library/Cookies/Cookies.binarycookies",
        "/Users/u/Library/Keychains/login.keychain-db",
        "/Users/u/Library/Application Support/Google/Chrome/Default/Preferences",
        "/Users/u/Library/Application Support/Firefox/Profiles/p/prefs.js",
        "/Users/u/Library/Application Support/BraveSoftware/x",
        "/h/proj/key4.db",
    ] {
        assert!(sensitive(Path::new(p)), "{p}");
    }
    for p in [
        "/h/.config/nvim/init.lua",
        "/Users/u/Library/Application Support/Code/User/settings.json",
        "/h/proj/cookie-banner.tsx",
    ] {
        assert!(!sensitive(Path::new(p)), "{p}");
    }
}

#[test]
fn a_working_folder_holding_system_folders_is_not_a_root() {
    let home = Path::new("/home/u");
    for bad in [
        "/", "/usr", "/etc", "/home", "/var", "/private", "/Users", "/Library", "/System", "/root",
    ] {
        assert!(!extra_root_allowed(Path::new(bad), Some(home)), "{bad}");
    }
    for ok in [
        "/home/u",
        "/home/u/proj",
        "/srv/work",
        "/opt/src/app",
        "/tmp/x",
    ] {
        assert!(extra_root_allowed(Path::new(ok), Some(home)), "{ok}");
    }
}

#[test]
fn a_working_folder_in_a_system_state_folder_is_not_a_root() {
    let home = Some(Path::new("/home/u"));
    for bad in [
        "/run/secrets",
        "/run/user/1000",
        "/etc/ssl/private",
        "/root/proj",
        "/var/lib/docker",
        "/var/home/other",
        "/private/etc/ssh",
        "/private/var/db",
    ] {
        assert!(!extra_root_allowed(Path::new(bad), home), "{bad}");
    }
    // A home under /var (as on some image-based systems) admits that home only.
    let var_home = Some(Path::new("/var/home/u"));
    assert!(extra_root_allowed(Path::new("/var/home/u/proj"), var_home));
    for bad in ["/var/home/other/proj", "/var/lib/x", "/run/secrets"] {
        assert!(!extra_root_allowed(Path::new(bad), var_home), "{bad}");
    }
}

#[test]
fn a_working_folder_in_a_kernel_file_system_is_never_a_root() {
    // `/proc/self/environ` and the like: refused wherever the folder sits, home included.
    for bad in [
        "/proc/self",
        "/proc/123/cwd",
        "/sys/kernel",
        "/dev/shm",
        "/dev",
    ] {
        assert!(
            !extra_root_allowed(Path::new(bad), Some(Path::new("/"))),
            "{bad}"
        );
        assert!(
            !extra_root_allowed(Path::new(bad), Some(Path::new("/home/u"))),
            "{bad}"
        );
    }
    // A link to one is judged by where it resolves, as `admit` canonicalises first.
    #[cfg(target_os = "linux")]
    {
        let t = tree();
        symlink("/proc/self", t.root.join("me")).unwrap();
        let real = std::fs::canonicalize(t.root.join("me")).unwrap();
        assert!(
            !extra_root_allowed(&real, Some(&t.root)),
            "{}",
            real.display()
        );
    }
}

#[test]
fn every_git_call_carries_the_overrides_no_behaviour_test_can_see() {
    // Status runs no hook, and diff already passes --no-ext-diff, so these keys cannot fail a
    // behaviour test; this test fails if one is dropped.
    let t = tree();
    let mut cmd = git(&t.root, &t.root, GitRead::Status).expect("git 2.39.1 or later");
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
fn the_peer_is_the_connections_own_address() {
    let mut ext = Extensions::new();
    assert_eq!(peer(&ext), None);
    let lan = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9)), 5);
    ext.insert(ConnectInfo(lan));
    assert_eq!(peer(&ext), Some(lan));
    assert!(local_only(Caller::Browser { generation: 1 }, peer(&ext)).is_err());
}

#[test]
fn a_file_over_the_attachment_limit_answers_413_and_one_at_it_is_read() {
    let t = tree();
    let big = t.root.join("big.bin");
    let file = std::fs::File::create(&big).unwrap();
    file.set_len(DATA_URL_MAX_BYTES + 1).unwrap();
    let resolved = resolve(&at(&t, "big.bin"), &roots(&t), Some(&t.root)).unwrap();
    assert_eq!(
        data_url(&resolved, &[]).unwrap_err().0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    file.set_len(DATA_URL_MAX_BYTES).unwrap();
    let (tag, body) = data_url(&resolved, &[]).unwrap();
    assert!(body.unwrap().starts_with("data:"));
    let (_, unchanged) = data_url(&resolved, std::slice::from_ref(&tag)).unwrap();
    assert!(unchanged.is_none(), "a matching ETag read the file again");
}

#[test]
fn each_refusal_answers_with_its_status() {
    for (refusal, want) in [
        (Refusal::BadPath("x"), StatusCode::BAD_REQUEST),
        (Refusal::Outside, StatusCode::FORBIDDEN),
        (Refusal::Sensitive, StatusCode::FORBIDDEN),
        (Refusal::NotFound, StatusCode::NOT_FOUND),
        (Refusal::NotADirectory, StatusCode::BAD_REQUEST),
        (Refusal::NotAFile, StatusCode::BAD_REQUEST),
        (Refusal::Denied, StatusCode::FORBIDDEN),
        (Refusal::TooLarge, StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let name = format!("{refusal:?}");
        let ApiError(status, text) = ApiError::from(refusal);
        assert_eq!(status, want, "{name}");
        assert!(!text.is_empty(), "{name}");
    }
}

#[test]
fn io_errors_map_to_refusals() {
    for (code, want) in [
        (libc::ENOENT, Refusal::NotFound),
        (libc::EACCES, Refusal::Denied),
        (libc::ENOTDIR, Refusal::NotADirectory),
        (libc::ELOOP, Refusal::Outside),
        (libc::EISDIR, Refusal::NotFound),
    ] {
        let err = std::io::Error::from_raw_os_error(code);
        assert_eq!(io_refusal(&err), want, "errno {code}");
    }
}

#[test]
fn a_home_relative_path_needs_a_home_folder() {
    let t = tree();
    for raw in ["~", "~/proj"] {
        assert_eq!(
            resolve(raw, &roots(&t), None).unwrap_err(),
            Refusal::BadPath("No home folder"),
            "{raw}"
        );
    }
    assert!(resolve("~/proj", &roots(&t), Some(&t.root)).is_ok());
}

#[test]
fn a_walk_opens_only_what_its_root_holds() {
    let t = tree();
    let walk = |root: &Path, path: PathBuf| {
        open_resolved(&Resolved {
            root: root.to_path_buf(),
            path,
        })
        .err()
    };
    assert_eq!(walk(&t.root, t.outside.clone()), Some(Refusal::Outside));
    assert_eq!(
        walk(&t.root, t.root.join("../outside")),
        Some(Refusal::Outside)
    );
    assert_eq!(
        walk(&t.root.join("gone"), t.root.join("gone/x")),
        Some(Refusal::NotFound)
    );
    assert_eq!(
        walk(&t.root, t.root.join("proj/missing.rs")),
        Some(Refusal::NotFound)
    );
    assert_eq!(walk(&t.root, t.root.join("proj/src/main.rs")), None);
}

#[test]
fn a_bad_descriptor_or_a_name_with_a_nul_fails_cleanly() {
    use std::os::unix::ffi::OsStrExt;
    struct Closed;
    impl std::os::fd::AsRawFd for Closed {
        fn as_raw_fd(&self) -> std::os::fd::RawFd {
            -1
        }
    }
    assert_eq!(
        fstat(&Closed).err().and_then(|e| e.raw_os_error()),
        Some(libc::EBADF)
    );
    let named = openat(libc::AT_FDCWD, OsStr::from_bytes(b"a\0b"), libc::O_RDONLY);
    assert_eq!(
        named.err().and_then(|e| e.raw_os_error()),
        Some(libc::EINVAL)
    );
}

fn list(t: &Tree, raw: &str) -> Result<Value, ApiError> {
    listing(resolve(raw, &roots(t), None).and_then(|r| {
        let opened = open_resolved(&r)?;
        Ok((r, opened))
    }))
}

#[test]
fn a_listing_answers_missing_unreadable_and_file_paths_in_its_body() {
    use std::os::unix::fs::PermissionsExt;
    let t = tree();
    let answer = |code: &str| json!({"entries": [], "error": code});
    assert_eq!(list(&t, &at(&t, "gone")).unwrap(), answer("ENOENT"));
    assert_eq!(
        list(&t, &at(&t, "proj/src/main.rs")).unwrap(),
        answer("ENOTDIR")
    );
    assert_eq!(
        list(&t, &at(&t, "proj/src/main.rs/x")).unwrap(),
        answer("ENOTDIR")
    );
    let locked = t.root.join("locked");
    std::fs::create_dir_all(locked.join("inner")).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let shut = list(&t, &at(&t, "locked")).unwrap();
    let inner = list(&t, &at(&t, "locked/inner")).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(shut, answer("EACCES"), "a folder it may not read");
    assert_eq!(inner, answer("EACCES"), "a path under a folder it may not search");
    let ApiError(status, _) = list(&t, t.outside.to_str().unwrap()).unwrap_err();
    assert_eq!(status, StatusCode::FORBIDDEN, "outside the roots");
}

#[test]
fn a_listing_past_the_entry_limit_says_it_is_truncated() {
    let t = tree();
    let many = t.root.join("many");
    std::fs::create_dir(&many).unwrap();
    for i in 0..=LIST_MAX_ENTRIES {
        std::fs::File::create(many.join(format!("f{i}"))).unwrap();
    }
    let body = list(&t, &at(&t, "many")).unwrap();
    assert_eq!(body["truncated"], true);
    assert_eq!(body["entries"].as_array().unwrap().len(), LIST_MAX_ENTRIES);
}

#[test]
fn a_descriptor_that_is_not_a_folder_lists_as_a_read_error() {
    let t = tree();
    let file: std::os::fd::OwnedFd = std::fs::File::open(t.root.join("proj/src/main.rs"))
        .unwrap()
        .into();
    let resolved = resolve(&at(&t, "proj/src"), &roots(&t), None).unwrap();
    assert_eq!(
        listing(Ok((resolved, Opened::Dir(file)))).unwrap(),
        json!({"entries": [], "error": "read-error"})
    );
}

#[test]
fn a_folder_is_neither_read_nor_taken_as_a_file_and_a_file_is_no_repository() {
    let t = tree();
    let folder = std::fs::File::open(&t.root).unwrap();
    assert_eq!(read_capped(folder, 10).unwrap_err(), Refusal::NotFound);
    let proj = resolve(&at(&t, "proj"), &roots(&t), None).unwrap();
    let ApiError(status, _) = data_url(&proj, &[]).unwrap_err();
    assert_eq!(status, StatusCode::BAD_REQUEST, "read-data-url of a folder");
    let Err(ApiError(status, _)) = repo_dir(&at(&t, "proj/src/main.rs"), &roots(&t), None) else {
        panic!("a file opened as a repository folder");
    };
    assert_eq!(status, StatusCode::BAD_REQUEST, "git route on a file");
}

#[test]
fn a_data_url_names_the_type_upstream_gives_each_extension() {
    for (name, want) in [
        ("a.PNG", "image/png"),
        ("a.jpg", "image/jpeg"),
        ("a.jpeg", "image/jpeg"),
        ("a.gif", "image/gif"),
        ("a.webp", "image/webp"),
        ("a.svg", "image/svg+xml"),
        ("a.bmp", "image/bmp"),
        ("a.ico", "image/vnd.microsoft.icon"),
        ("a.avif", "image/avif"),
        ("a.heic", "image/heic"),
        ("a.pdf", "application/pdf"),
        ("a.json", "application/json"),
        ("a.txt", "text/plain"),
        ("a.log", "text/plain"),
        ("a.md", "text/markdown"),
        ("a.csv", "text/csv"),
        ("a.html", "text/html"),
        ("a.htm", "text/html"),
        ("a.css", "text/css"),
        ("a.js", "text/javascript"),
        ("a.mjs", "text/javascript"),
        ("a.mp3", "audio/mpeg"),
        ("a.wav", "audio/wav"),
        ("a.ogg", "audio/ogg"),
        ("a.mp4", "video/mp4"),
        ("a.webm", "video/webm"),
        ("a.bin", "application/octet-stream"),
        ("noext", "application/octet-stream"),
    ] {
        assert_eq!(mime_type(Path::new(name)), want, "{name}");
    }
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
    assert_eq!(lines("short.txt"), 2, "a last line without a newline counts");
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
fn a_prefix_that_leaves_the_folder_gives_no_top() {
    assert_eq!(
        top_of(Path::new("/a/b/c"), "b/c/"),
        Some(PathBuf::from("/a"))
    );
    assert_eq!(top_of(Path::new("/a/b"), "../x"), None);
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
