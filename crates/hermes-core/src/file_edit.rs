use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

/// The tool parameter through which the agent loop names its session to the
/// file tools, which keep per session what each file's reader has seen.
pub const SESSION_PARAM: &str = "_hermes_session";

/// Coordinate local edits that resolve to the same file in this process.
/// A file that has yet to be created uses its nearest existing ancestor so
/// its lock stays the same after creation.
pub async fn file_edit_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
        LazyLock::new(Mutex::default);
    let file = edited_file(path).await;
    let mut locks = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    Arc::clone(locks.entry(file).or_default())
}

/// The one name of the file an edit of `path` changes: links resolved, and
/// for a file yet to be created, its nearest existing ancestor resolved.
pub async fn edited_file(path: &Path) -> PathBuf {
    // Follow a dangling symlink to the file a write through it would create.
    let mut named = path.to_path_buf();
    for _ in 0..40 {
        if tokio::fs::metadata(&named).await.is_ok() {
            break;
        }
        let Ok(target) = tokio::fs::read_link(&named).await else {
            break;
        };
        named = named.parent().unwrap_or(Path::new("")).join(target);
    }
    let mut file = named.clone();
    for existing in named.ancestors() {
        if let Ok(real) = tokio::fs::canonicalize(existing).await {
            let missing = named.strip_prefix(existing).unwrap_or(Path::new(""));
            file = if missing.as_os_str().is_empty() {
                real
            } else {
                real.join(missing)
            };
            break;
        }
    }
    file
}
