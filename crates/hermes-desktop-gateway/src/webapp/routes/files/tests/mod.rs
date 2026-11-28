use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::fs::open_resolved;
use super::git::{repo_dir, repo_status};
use super::listing;
use super::roots::{resolve, Roots};
use crate::webapp::api::ApiError;

pub(super) struct Tree {
    _dir: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) hermes: PathBuf,
    pub(super) outside: PathBuf,
}

/// A home folder with a Hermes home inside it, and a folder outside both.
pub(super) fn tree() -> Tree {
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

pub(super) fn roots(t: &Tree) -> Roots {
    Roots::new(
        std::slice::from_ref(&t.root),
        std::slice::from_ref(&t.hermes),
    )
}

pub(super) fn at(t: &Tree, rel: &str) -> String {
    t.root.join(rel).to_string_lossy().into_owned()
}

pub(super) fn run(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

pub(super) fn commit_all(dir: &Path) {
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
pub(super) fn status_at(t: &Tree, rel: &str) -> Option<Value> {
    let dir = repo_dir(&at(t, rel), &roots(t), Some(&t.root)).ok()?;
    repo_status(&dir, std::slice::from_ref(&t.hermes))
}

pub(super) fn list(t: &Tree, raw: &str) -> Result<Value, ApiError> {
    listing(
        resolve(raw, &roots(t), None).and_then(|r| {
            let opened = open_resolved(&r)?;
            Ok((r, opened))
        }),
        std::slice::from_ref(&t.hermes),
    )
}

mod fs_tests;
mod git_security_tests;
mod git_tests;
mod roots_tests;
