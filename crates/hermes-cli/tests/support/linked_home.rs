//! The fixture for the CLI's and the HTTP server's `linked_home` tests, which
//! include this file with `#[path]`.

use std::path::{Path, PathBuf};

pub type Snapshot = Vec<(PathBuf, Option<Vec<u8>>)>;

/// Every entry under `root` (folders without bytes), in path order.
pub fn snapshot(root: &Path) -> Snapshot {
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

/// A user home whose `.hermes-agent-ultra` links to upstream's `.hermes`,
/// which holds a config file; returns the home, upstream's folder and its
/// snapshot before the run.
pub fn linked_home() -> (tempfile::TempDir, PathBuf, Snapshot) {
    let home = tempfile::tempdir().unwrap();
    let upstream = home.path().join(".hermes");
    std::fs::create_dir_all(&upstream).unwrap();
    std::fs::write(upstream.join("config.yaml"), "model: upstream\n").unwrap();
    std::os::unix::fs::symlink(&upstream, home.path().join(".hermes-agent-ultra")).unwrap();
    let before = snapshot(&upstream);
    (home, upstream, before)
}
