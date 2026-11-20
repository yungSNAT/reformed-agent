//! Source guards for the Rust runtime: no placeholder markers and no Python
//! execution paths. `scripts/check-runtime-placeholders.sh` and
//! `scripts/check-rust-runtime-no-python.sh` run these tests.
//!
//! Files are those `git ls-files --cached --others --exclude-standard` lists,
//! the set ripgrep searched when the guards were shell scripts.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;

const THIS_FILE: &str = "crates/hermes-source-parity-tests/tests/runtime_source_guards.rs";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn repo_files(root: &Path, dirs: &[&str]) -> Vec<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
        ])
        .args(dirs)
        .output()
        .expect("run git ls-files");
    assert!(output.status.success(), "git ls-files failed: {output:?}");
    String::from_utf8(output.stdout)
        .expect("git paths are UTF-8")
        .split('\0')
        .filter(|path| !path.is_empty() && *path != THIS_FILE)
        .map(str::to_owned)
        .collect()
}

/// Every `path:line: text` in `files` that `pattern` matches. A listed file
/// that no longer exists (deleted, not yet staged) is skipped.
fn hits(root: &Path, files: &[String], pattern: &Regex) -> Vec<String> {
    let mut found = Vec::new();
    for path in files {
        let bytes = match fs::read(root.join(path)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => panic!("could not read runtime source {path}: {error}"),
        };
        let text = String::from_utf8_lossy(&bytes);
        for (index, line) in text.lines().enumerate() {
            if pattern.is_match(line) {
                found.push(format!("{path}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    found
}

fn assert_clean(what: &str, found: Vec<String>) {
    assert!(found.is_empty(), "{what} detected:\n{}", found.join("\n"));
}

#[test]
#[should_panic(expected = "could not read runtime source")]
fn a_runtime_source_read_error_fails_the_guard() {
    let pattern = Regex::new("unused pattern").unwrap();
    hits(&repo_root(), &["crates".to_owned()], &pattern);
}

#[test]
fn runtime_has_no_placeholder_markers() {
    let root = repo_root();
    let pattern = Regex::new(
        r"(?i)not yet implemented|TODO: implement|unimplemented!\(|todo!\(|placeholder summary|stub-only|placeholder that records the join intent|minimal stub LLM",
    )
    .unwrap();
    let files: Vec<String> = repo_files(&root, &["crates", "scripts"])
        .into_iter()
        .filter(|path| !path.split('/').any(|part| part == "tests"))
        .collect();
    assert_clean("Runtime placeholder markers", hits(&root, &files, &pattern));
}

#[test]
fn rust_runtime_spawns_no_python() {
    let root = repo_root();
    let crates = repo_files(&root, &["crates"]);
    let spawn = Regex::new(
        r#"Command::new\("python|tokio::process::Command::new\("python|std::process::Command::new\("python"#,
    )
    .unwrap();
    assert_clean(
        "Rust runtime Python process spawns",
        hits(&root, &crates, &spawn),
    );
    let reference = Regex::new(
        r"agent_orchestration\.py|scripts/agent_orchestration\.py|HERMES_CONTEXTLATTICE_ORCH_SCRIPT|HERMES_KANBAN_CONTEXTLATTICE_SCRIPT",
    )
    .unwrap();
    assert_clean(
        "Rust runtime Python ContextLattice script references",
        hits(&root, &crates, &reference),
    );
    let python_default = Regex::new(
        r#"default: 'python'|default": "python"|Supports Python|primarily Python|run Python|py_compile"#,
    )
    .unwrap();
    let code_execution = [
        "crates/hermes-tools/src/tools/code_execution.rs".to_owned(),
        "crates/hermes-tools/src/backends/code_execution.rs".to_owned(),
    ];
    assert_clean(
        "Python-default code execution contract",
        hits(&root, &code_execution, &python_default),
    );
}
