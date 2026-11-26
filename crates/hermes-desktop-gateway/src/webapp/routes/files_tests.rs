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
    let status = repo_status(&repo, &t.root);
    assert!(!marker.exists(), "a configured program ran");
    let Some(status) = status else { return }; // git older than 2.39.1 gives no command: nothing ran either
    assert_eq!(status["branch"], "main");
    assert_eq!(status["untracked"], 2);
    assert!(status["added"].as_u64().unwrap() >= 2);
}

#[test]
fn discovery_stops_at_the_root() {
    let t = tree();
    run(t.outside.parent().unwrap(), &["init", "-q"]);
    // The repository is above the root: a folder inside the root is not in one.
    assert_eq!(repo_status(&t.root.join("proj"), &t.root), None);
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
