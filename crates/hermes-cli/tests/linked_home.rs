//! The CLI refuses to start when its implicit home is a link into upstream
//! Hermes's `~/.hermes`, before it writes anything there.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// Every entry under `root` (folders without bytes), in path order.
fn snapshot(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                pending.push(path);
                entries.push((rel, None));
            } else {
                entries.push((rel, Some(std::fs::read(&path).unwrap())));
            }
        }
    }
    entries.sort();
    entries
}

#[test]
fn cli_refuses_to_start_with_its_home_linked_into_upstreams() {
    let home = tempfile::tempdir().unwrap();
    let upstream = home.path().join(".hermes");
    std::fs::create_dir_all(&upstream).unwrap();
    std::fs::write(upstream.join("config.yaml"), "model: upstream\n").unwrap();
    std::os::unix::fs::symlink(&upstream, home.path().join(".hermes-agent-ultra")).unwrap();
    let before = snapshot(&upstream);

    let output = Command::cargo_bin("hermes-agent-ultra")
        .unwrap()
        .env("HOME", home.path())
        .env_remove("HERMES_HOME")
        .env_remove("HERMES_AGENT_ULTRA_HOME")
        .args(["config", "set", "model", "openai:gpt-4o-mini"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "exited {}: {stderr}",
        output.status
    );
    assert!(stderr.contains("Refusing to start"), "{stderr}");
    assert!(
        stderr.contains(&format!("HERMES_HOME={}", upstream.display())),
        "{stderr}"
    );
    assert_eq!(snapshot(&upstream), before);
}
