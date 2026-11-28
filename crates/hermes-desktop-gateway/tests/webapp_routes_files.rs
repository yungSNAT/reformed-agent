//! The file and repository routes through the full local runtime: who may call them, what a
//! signed-in Webapp tab or a native client on this machine may read, and that a repository's
//! own configuration runs no program through a read.
#![cfg(unix)]

mod webapp_support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use webapp_support::*;

/// Every route in the group (all answer GET only), with the path under `work_tree`'s base that
/// it serves to an admitted caller.
const ROUTES: [(&str, &str); 4] = [
    ("/api/fs/list", ""),
    ("/api/fs/read-data-url", "proj/a.txt"),
    ("/api/git/status", "proj"),
    ("/api/git/review/rev-parse", "proj"),
];

fn at(route: &str, path: &Path) -> String {
    let mut url = reqwest::Url::parse("http://x").unwrap();
    url.set_path(route);
    url.query_pairs_mut()
        .append_pair("path", path.to_str().unwrap());
    format!("{}?{}", url.path(), url.query().unwrap())
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// A private folder `base` that the served Hermes home `base/hermes` sits in and that the home's
/// `terminal.cwd` names, so `base` is a readable root and the home a refused tree inside it.
/// `base/proj` is a repository whose own config names a marker-writing program as its fsmonitor
/// and external diff. Returns `(guard, base, home, marker)`.
fn work_tree() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let (dir, base) = private_home();
    let home = base.join("hermes");
    std::fs::create_dir(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let proj = base.join("proj");
    std::fs::create_dir(&proj).unwrap();
    git(&proj, &["init", "-q", "-b", "main"]);
    std::fs::write(proj.join("a.txt"), "one\n").unwrap();
    git(&proj, &["add", "a.txt"]);
    git(&proj, &["commit", "-q", "-m", "one"]);
    std::fs::write(proj.join("a.txt"), "two\n").unwrap();
    let marker = base.join("ran");
    let program = base.join("program.sh");
    std::fs::write(
        &program,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let program = program.to_str().unwrap();
    git(&proj, &["config", "core.fsmonitor", program]);
    git(&proj, &["config", "diff.external", program]);
    (dir, base, home, marker)
}

/// Adds `terminal.cwd: <base>` to the config `serving` wrote.
fn work_folder(home: &Path, base: &Path) {
    let path = home.join("config.yaml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!("{config}terminal:\n  cwd: {}\n", base.display()),
    )
    .unwrap();
}

async fn assert_route_guards(origin: &str, token: &str, route: &str) {
    // The route serves this path to the credential from the page's own origin, so a 403
    // below is the guard's, not the route's.
    let own = call(
        origin,
        "GET",
        route,
        &[("X-Hermes-Session-Token", token), ("Origin", origin)],
    )
    .send()
    .await
    .unwrap();
    assert_eq!(own.status(), 200, "{route} from its own origin");
    let none = call(origin, "GET", route, &[]).send().await.unwrap();
    assert_eq!(none.status(), 401, "{route} without a credential");
    let wrong = call(origin, "GET", route, &[("X-Hermes-Session-Token", "wrong")])
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401, "{route} with a wrong credential");
    let foreign = call(
        origin,
        "GET",
        route,
        &[
            ("X-Hermes-Session-Token", token),
            ("Origin", "http://evil.example"),
        ],
    )
    .send()
    .await
    .unwrap();
    assert_eq!(foreign.status(), 403, "{route} from another origin");
    let rebound = call(
        origin,
        "GET",
        route,
        &[
            ("X-Hermes-Session-Token", token),
            ("Origin", origin),
            ("Host", "evil.example"),
        ],
    )
    .send()
    .await
    .unwrap();
    assert_eq!(rebound.status(), 403, "{route} under another host name");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_route_refuses_a_caller_without_the_credential_or_from_another_origin() {
    let (_dir, base, home, _marker) = work_tree();
    let (_runtime, origin) = serving(&home);
    work_folder(&home, &base);
    let token = credential(&home, &origin).await;
    for (route, path) in ROUTES {
        let target = if path.is_empty() {
            base.clone()
        } else {
            base.join(path)
        };
        assert_route_guards(&origin, &token, &at(route, &target)).await;
    }
}

async fn send_as(
    who: &str,
    home: &Path,
    origin: &str,
    token: &str,
    route: &str,
) -> reqwest::Response {
    // A native ticket is one-use, so each native request mints its own.
    let ticket = (who == "native").then(|| native_ticket(home));
    let headers: Vec<(&str, &str)> = match &ticket {
        Some(ticket) => vec![("X-Hermes-Gateway-Ticket", ticket.as_str())],
        None => vec![("X-Hermes-Session-Token", token), ("Origin", origin)],
    };
    call(origin, "GET", route, &headers).send().await.unwrap()
}

async fn assert_working_folder_access(
    who: &str,
    base: &Path,
    home: &Path,
    origin: &str,
    token: &str,
) {
    let list = send_as(who, home, origin, token, &at("/api/fs/list", base)).await;
    assert_eq!(list.status(), 200, "{who} lists the working folder");
    let body: Value = list.json().await.unwrap();
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("{who}: {body}"))
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(names.contains(&"proj"), "{who}: {body}");
    assert!(!names.contains(&"hermes"), "{who}: {body}");

    let file = send_as(
        who,
        home,
        origin,
        token,
        &at("/api/fs/read-data-url", &base.join("proj/a.txt")),
    )
    .await;
    assert_eq!(
        file.status(),
        200,
        "{who} reads a file in the working folder"
    );
    let body: Value = file.json().await.unwrap();
    assert!(
        body["dataUrl"]
            .as_str()
            .is_some_and(|u| u.starts_with("data:")),
        "{who}: {body}"
    );
}

async fn assert_hermes_home_refused(who: &str, home: &Path, origin: &str, token: &str) {
    // A name no credential rule refuses, so only the Hermes home rule keeps it out.
    let home_reply = send_as(
        who,
        home,
        origin,
        token,
        &at("/api/fs/read-data-url", &home.join("notes.txt")),
    )
    .await;
    assert_eq!(home_reply.status(), 403, "{who} reads the Hermes home");
    let home_body = home_reply.text().await.unwrap();
    assert!(
        !home_body.contains("hermes-home-marker"),
        "{who} read Hermes home bytes"
    );
    let listing = send_as(who, home, origin, token, &at("/api/fs/list", home)).await;
    assert_eq!(listing.status(), 403, "{who} lists the Hermes home");
    let list_body = listing.text().await.unwrap();
    assert!(
        !list_body.contains("notes.txt"),
        "{who} listed Hermes home entries"
    );
}

async fn assert_git_access(who: &str, base: &Path, home: &Path, origin: &str, token: &str) {
    let status = send_as(
        who,
        home,
        origin,
        token,
        &at("/api/git/status", &base.join("proj")),
    )
    .await;
    assert_eq!(status.status(), 200, "{who} reads repository status");
    let body: Value = status.json().await.unwrap();
    assert_eq!(body["branch"], "main", "{who}: {body}");
    assert_eq!(body["changed"], 1, "{who}: {body}");

    let sha = send_as(
        who,
        home,
        origin,
        token,
        &at("/api/git/review/rev-parse", &base.join("proj")),
    )
    .await;
    assert_eq!(sha.status(), 200, "{who} resolves HEAD");
    let body: Value = sha.json().await.unwrap();
    assert!(
        body["sha"].as_str().is_some_and(|s| s.len() == 40),
        "{who}: {body}"
    );
}

/// A signed-in tab and a native client each read the working folder and its repository, never
/// the Hermes home inside it, and the repository's fsmonitor and external diff never run.
#[tokio::test(flavor = "multi_thread")]
async fn a_local_caller_reads_the_working_folder_but_not_the_hermes_home() {
    let (_dir, base, home, marker) = work_tree();
    let (_runtime, origin) = serving(&home);
    work_folder(&home, &base);
    std::fs::write(home.join("notes.txt"), "hermes-home-marker").unwrap();
    let token = credential(&home, &origin).await;
    for who in ["browser", "native"] {
        assert_working_folder_access(who, &base, &home, &origin, &token).await;
        assert_hermes_home_refused(who, &home, &origin, &token).await;
        assert_git_access(who, &base, &home, &origin, &token).await;
    }
    assert!(
        !marker.exists(),
        "the repository's fsmonitor or external diff ran"
    );
}

/// The answers a signed-in tab gets besides a plain read: a missing `path`, an unchanged file
/// named by its ETag, a git route on a missing or outside folder, and a named or refused ref.
#[tokio::test(flavor = "multi_thread")]
async fn the_routes_answer_a_missing_path_an_unchanged_file_and_a_refused_ref() {
    let (_dir, base, home, _marker) = work_tree();
    let (_runtime, origin) = serving(&home);
    work_folder(&home, &base);
    let token = credential(&home, &origin).await;
    let get = |route: String, extra: Option<(&'static str, String)>| {
        let mut headers = vec![
            ("X-Hermes-Session-Token", token.clone()),
            ("Origin", origin.clone()),
        ];
        headers.extend(extra);
        let headers: Vec<(&str, &str)> = headers.iter().map(|(n, v)| (*n, v.as_str())).collect();
        call(&origin, "GET", &route, &headers).send()
    };

    let bare = get("/api/fs/list".into(), None).await.unwrap();
    assert_eq!(bare.status(), 400, "a listing without a path");

    let file = at("/api/fs/read-data-url", &base.join("proj/a.txt"));
    let first = get(file.clone(), None).await.unwrap();
    assert_eq!(first.status(), 200);
    let tag = first.headers()["etag"].to_str().unwrap().to_owned();
    let again = get(file, Some(("If-None-Match", format!("W/{tag}, \"other\""))))
        .await
        .unwrap();
    assert_eq!(again.status(), 304, "a file unchanged since its ETag");
    assert_eq!(again.headers()["etag"].to_str().unwrap(), tag);

    let missing = get(at("/api/git/status", &base.join("gone")), None)
        .await
        .unwrap();
    assert_eq!(missing.status(), 200, "status of a missing folder");
    assert_eq!(missing.json::<Value>().await.unwrap(), Value::Null);
    let outside = get(at("/api/git/status", Path::new("/")), None)
        .await
        .unwrap();
    assert_eq!(outside.status(), 403, "status outside the roots");

    let rev = |r: &str| {
        format!(
            "{}&ref={r}",
            at("/api/git/review/rev-parse", &base.join("proj"))
        )
    };
    let named = get(rev("main"), None).await.unwrap();
    assert_eq!(named.status(), 200, "a named ref");
    let body: Value = named.json().await.unwrap();
    assert!(
        body["sha"].as_str().is_some_and(|s| s.len() == 40),
        "{body}"
    );
    let option = get(rev("-n"), None).await.unwrap();
    assert_eq!(option.status(), 400, "a ref that reads as an option");
}
