use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use axum::http::StatusCode;
use rustix::fs::{FileType, OFlags, CWD};
use rustix::io::Errno;
use serde_json::json;

use super::super::fs::{
    collect_entries, list_entries, mime_type, open_dir_walk, open_resolved, openat, read_capped,
    Opened, DATA_URL_MAX_BYTES, LIST_MAX_ENTRIES,
};
use super::super::git::repo_dir;
use super::super::roots::{resolve, Refusal, Resolved, Roots};
use super::super::{data_url, listing};
use super::{at, list, roots, tree};
use crate::webapp::api::ApiError;

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
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
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
    let (entries, truncated) =
        list_entries(dir, &resolved.path, std::slice::from_ref(&t.hermes)).unwrap();
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
    // Refused trees (such as the Hermes home) are hidden from their parent's listing too.
    let root_listing = list(&t, &at(&t, "")).unwrap();
    let root_names: Vec<&str> = root_listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(!root_names.contains(&".hermes"), "{root_names:?}");
    assert!(root_names.contains(&"proj"), "{root_names:?}");
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
    assert_eq!(
        open_dir_walk(Path::new("rel")).err(),
        Some(Refusal::Outside)
    );
    assert_eq!(
        open_dir_walk(&t.root.join("proj/..")).err(),
        Some(Refusal::Outside)
    );
}

#[test]
fn a_swapped_ancestor_of_a_nested_root_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let work = base.join("parent/work");
    std::fs::create_dir_all(work.join("proj")).unwrap();
    std::fs::write(work.join("proj/file.txt"), "inside").unwrap();
    let decoy = base.join("outside/work/proj");
    std::fs::create_dir_all(&decoy).unwrap();
    std::fs::write(decoy.join("file.txt"), "outside").unwrap();
    let r = Roots::new(std::slice::from_ref(&work), &[]);
    let checked_file = resolve(work.join("proj/file.txt").to_str().unwrap(), &r, None).unwrap();
    let checked_root = resolve(work.to_str().unwrap(), &r, None).unwrap();
    std::fs::rename(base.join("parent"), base.join("parent.old")).unwrap();
    symlink(base.join("outside"), base.join("parent")).unwrap();
    assert_eq!(open_resolved(&checked_file).err(), Some(Refusal::Outside));
    assert_eq!(open_resolved(&checked_root).err(), Some(Refusal::Outside));
}

#[test]
fn a_readdir_error_or_unknown_stat_failure_fails_closed() {
    let t = tree();
    let proj = resolve(&at(&t, "proj"), &roots(&t), None).unwrap();
    let Ok(Opened::Dir(dir)) = open_resolved(&proj) else {
        panic!("not a folder")
    };
    let mid_err = collect_entries(
        &dir,
        [
            Ok((CString::new("a.txt").unwrap(), FileType::RegularFile)),
            Err(std::io::Error::from_raw_os_error(Errno::IO.raw_os_error())),
        ],
        &proj.path,
        &[],
    );
    assert!(mid_err.is_err(), "mid-read error returned partial listing");
    let (unknown_ok, truncated) = collect_entries(
        &dir,
        [Ok((CString::new("src").unwrap(), FileType::Unknown))],
        &proj.path,
        &[],
    )
    .unwrap();
    assert!(!truncated);
    assert_eq!(unknown_ok.len(), 1);
    assert_eq!(unknown_ok[0]["isDirectory"], true);
    let unknown_err = collect_entries(
        &dir,
        [Ok((CString::new("missing").unwrap(), FileType::Unknown))],
        &proj.path,
        &[],
    );
    assert!(
        unknown_err.is_err(),
        "stat failure on Unknown entry collapsed to false"
    );
}

#[test]
fn a_non_searchable_folder_classifies_entries_without_statat() {
    let t = tree();
    let noexec = t.root.join("noexec");
    std::fs::create_dir_all(noexec.join("sub")).unwrap();
    std::fs::write(noexec.join("file.txt"), "x").unwrap();
    std::fs::set_permissions(&noexec, std::fs::Permissions::from_mode(0o400)).unwrap();
    let body = list(&t, &at(&t, "noexec"));
    std::fs::set_permissions(&noexec, std::fs::Permissions::from_mode(0o700)).unwrap();
    let body = body.unwrap();
    let names: Vec<(&str, bool)> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap(),
                e["isDirectory"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(names, [("sub", true), ("file.txt", false)]);
}

#[test]
fn openat_refuses_a_name_with_a_nul() {
    let named = openat(CWD, OsStr::from_bytes(b"a\0b"), OFlags::RDONLY);
    assert_eq!(
        named.err().and_then(|e| e.raw_os_error()),
        Some(Errno::INVAL.raw_os_error())
    );
}

#[test]
fn a_listing_answers_missing_unreadable_and_file_paths_in_its_body() {
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
    assert_eq!(
        inner,
        answer("EACCES"),
        "a path under a folder it may not search"
    );
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
        listing(Ok((resolved, Opened::Dir(file))), &[]).unwrap(),
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
