//! The HTTP server refuses to start when its implicit home is a link into
//! upstream Hermes's `~/.hermes`, before it writes anything there.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
fn server_refuses_to_start_with_its_home_linked_into_upstreams() {
    let home = tempfile::tempdir().unwrap();
    let upstream = home.path().join(".hermes");
    std::fs::create_dir_all(&upstream).unwrap();
    std::fs::write(upstream.join("config.yaml"), "model: upstream\n").unwrap();
    std::os::unix::fs::symlink(&upstream, home.path().join(".hermes-agent-ultra")).unwrap();
    let before = snapshot(&upstream);
    let stderr_path = home.path().join("stderr.log");

    let mut child = Command::new(env!("CARGO_BIN_EXE_hermes-http"))
        .env("HOME", home.path())
        .env_remove("HERMES_HOME")
        .env_remove("HERMES_AGENT_ULTRA_HOME")
        .env("HERMES_HTTP_ADDR", "127.0.0.1:0")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let stderr = std::fs::read_to_string(&stderr_path).unwrap();
    let status = status.unwrap_or_else(|| panic!("hermes-http kept running: {stderr}"));
    assert!(!status.success(), "exited {status}: {stderr}");
    assert!(stderr.contains("Refusing to start"), "{stderr}");
    assert_eq!(snapshot(&upstream), before);
}
