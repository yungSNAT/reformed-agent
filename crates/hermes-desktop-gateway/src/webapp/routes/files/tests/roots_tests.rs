use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use axum::extract::ConnectInfo;
use axum::http::{Extensions, StatusCode};
use rustix::io::Errno;

use super::super::fs::{open_resolved, Opened};
use super::super::roots::{
    extra_root_allowed, home_root_allowed, io_refusal, local_only, peer, resolve, roots_for,
    sensitive, Refusal,
};
use super::{at, roots, tree};
use crate::webapp::api::{ApiError, Caller};

#[test]
fn credential_names_and_folders_are_sensitive_anywhere() {
    for p in [
        "/h/.env",
        "/h/.env.local",
        "/h/x/.envrc",
        "/h/.ssh",
        "/h/.ssh/id_rsa",
        "/h/backup/id_rsa",
        "/h/backup/id_ed25519",
        "/h/backup/id_ecdsa",
        "/h/backup/id_dsa",
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
    // A link with an innocent name to a secret is judged by where it leads, whether or not the
    // leaf exists, so an inside-root link to a sensitive folder is not an existence oracle.
    symlink(t.root.join(".ssh/id_ed25519"), t.root.join("notes.txt")).unwrap();
    assert_eq!(
        resolve(&at(&t, "notes.txt"), &r, Some(&t.root)).unwrap_err(),
        Refusal::Sensitive
    );
    let aws = t.root.join(".aws");
    std::fs::create_dir(&aws).unwrap();
    std::fs::write(aws.join("notes.txt"), "secret").unwrap();
    symlink(&aws, t.root.join("lnk")).unwrap();
    for p in ["lnk/notes.txt", "lnk/missing"] {
        assert_eq!(
            resolve(&at(&t, p), &r, Some(&t.root)).unwrap_err(),
            Refusal::Sensitive,
            "{p}"
        );
    }
    let cfg = t.root.join(".config");
    std::fs::create_dir(&cfg).unwrap();
    symlink(&cfg, t.root.join("cfg")).unwrap();
    assert_eq!(
        resolve(&at(&t, "cfg/gh/missing"), &r, Some(&t.root)).unwrap_err(),
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

#[test]
fn a_path_outside_the_roots_answers_alike_whether_or_not_it_exists() {
    let t = tree();
    let r = roots(&t);
    symlink(&t.outside, t.root.join("escape")).unwrap();
    let locked = t.outside.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let outside_aws = t.outside.join(".aws");
    std::fs::create_dir(&outside_aws).unwrap();
    std::fs::write(outside_aws.join("notes.txt"), "secret").unwrap();
    symlink(&outside_aws, t.outside.join("lnk")).unwrap();
    for p in [
        t.outside.clone(),
        t.outside.join("secret.txt"),
        t.outside.join("missing"),
        t.outside.join("missing/deeper"),
        locked.join("x"),
        t.outside.join("lnk/notes.txt"),
        t.outside.join("lnk/missing"),
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
        "/",
        "/usr",
        "/etc",
        "/home",
        "/var",
        "/private",
        "/private/tmp",
        "/Users",
        "/Library",
        "/System",
        "/root",
    ] {
        assert!(!extra_root_allowed(Path::new(bad), Some(home)), "{bad}");
    }
    for ok in [
        "/home/u",
        "/home/u/proj",
        "/srv/work",
        "/opt/src/app",
        "/tmp/x",
        "/private/tmp/x",
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
fn a_system_or_kernel_home_folder_is_not_a_root() {
    for bad in [
        "/",
        "/proc/self",
        "/sys/kernel",
        "/dev/shm",
        "/etc",
        "/run",
        "/root",
        "/var",
        "/var/home",
        "/var/lib/x",
        "/home",
        "/Users",
        "/tmp",
        "/private/tmp",
        "/private/var/db",
    ] {
        assert!(!home_root_allowed(Path::new(bad)), "{bad}");
    }
    for ok in [
        "/home/u",
        "/Users/u",
        "/var/home/u",
        "/tmp/x",
        "/private/tmp/x",
    ] {
        assert!(home_root_allowed(Path::new(ok)), "{ok}");
    }
    let root_home = roots_for(Some(Path::new("/")), Path::new("/"), &[]);
    assert!(root_home.allowed.is_empty(), "{:?}", root_home.allowed);
    assert_eq!(
        resolve("/etc/hosts", &root_home, Some(Path::new("/"))).unwrap_err(),
        Refusal::Outside
    );
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            resolve("/proc/self/environ", &root_home, Some(Path::new("/"))).unwrap_err(),
            Refusal::Outside
        );
        let proc_home = roots_for(Some(Path::new("/proc/self")), Path::new("/proc/self"), &[]);
        assert!(proc_home.allowed.is_empty(), "{:?}", proc_home.allowed);
    }
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
        (Errno::NOENT.raw_os_error(), Refusal::NotFound),
        (Errno::ACCES.raw_os_error(), Refusal::Denied),
        (Errno::NOTDIR.raw_os_error(), Refusal::NotADirectory),
        (Errno::LOOP.raw_os_error(), Refusal::Outside),
        (Errno::ISDIR.raw_os_error(), Refusal::NotFound),
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
    assert_eq!(
        resolve("~", &roots(&t), Some(&t.root)).unwrap().path,
        t.root
    );
    assert!(resolve("~/proj", &roots(&t), Some(&t.root)).is_ok());
}
