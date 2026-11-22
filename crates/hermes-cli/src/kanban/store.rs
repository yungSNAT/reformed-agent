use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use hermes_core::AgentError;

use super::{KanbanStore, KANBAN_SCHEMA_VERSION, KANBAN_STATE_DIR};

const KANBAN_STORE_FILE: &str = "boards.json";

pub fn kanban_store_path() -> PathBuf {
    kanban_store_path_in(&hermes_config::hermes_home())
}

pub(super) fn kanban_store_path_in(home: &Path) -> PathBuf {
    home.join(KANBAN_STATE_DIR).join(KANBAN_STORE_FILE)
}

pub fn load_store() -> Result<KanbanStore, AgentError> {
    lock_store()?.load()
}

/// Atomically replace a board snapshot. Read-modify-write commands hold one
/// lock across both operations instead.
pub fn save_store(store: &KanbanStore) -> Result<PathBuf, AgentError> {
    lock_store()?.save(store)
}

/// Keep the lock beside the JSON file so replacing the JSON preserves the lock.
pub(crate) struct KanbanStoreLock {
    path: PathBuf,
    _file: File,
}

pub(crate) fn lock_store() -> Result<KanbanStoreLock, AgentError> {
    KanbanStoreLock::open(kanban_store_path())
}

impl KanbanStoreLock {
    pub(super) fn open(path: PathBuf) -> Result<Self, AgentError> {
        let lock_path = path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                AgentError::Io(format!("create {} failed: {error}", parent.display()))
            })?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| {
                AgentError::Io(format!("open {} failed: {error}", lock_path.display()))
            })?;
        file.lock().map_err(|error| {
            AgentError::Io(format!("lock {} failed: {error}", lock_path.display()))
        })?;
        Ok(Self { path, _file: file })
    }

    pub(crate) fn load(&self) -> Result<KanbanStore, AgentError> {
        load_store_from_path(&self.path)
    }

    pub(crate) fn save(self, store: &KanbanStore) -> Result<PathBuf, AgentError> {
        save_store_to_path(store, &self.path)
    }
}

pub(super) fn load_store_from_path(path: &Path) -> Result<KanbanStore, AgentError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let store = KanbanStore::default();
            save_store_to_path(&store, path)?;
            return Ok(store);
        }
        Err(error) if error.kind() == ErrorKind::InvalidData => {
            return Err(corrupt_store_error(path, format!("read failed: {error}")));
        }
        Err(error) => {
            return Err(AgentError::Io(format!(
                "read {} failed: {error}",
                path.display()
            )));
        }
    };
    let mut store = serde_json::from_str::<KanbanStore>(&raw)
        .map_err(|e| corrupt_store_error(path, format!("parse failed: {e}")))?;
    if store.boards.is_empty() {
        return Err(corrupt_store_error(
            path,
            "store has no boards; refusing to auto-initialize over existing state",
        ));
    }
    normalize_store(&mut store);
    Ok(store)
}

pub(super) fn save_store_to_path(store: &KanbanStore, path: &Path) -> Result<PathBuf, AgentError> {
    let serialized = serde_json::to_vec_pretty(store)
        .map_err(|e| AgentError::Config(format!("serialize kanban store failed: {}", e)))?;
    hermes_config::loader::atomic_write_bytes(path, &serialized)
        .map_err(|e| AgentError::Io(format!("write {} failed: {}", path.display(), e)))?;
    Ok(path.to_path_buf())
}

fn normalize_store(store: &mut KanbanStore) {
    if store.schema_version == 0 {
        store.schema_version = KANBAN_SCHEMA_VERSION;
    }
    if store.boards.is_empty() {
        *store = KanbanStore::default();
        return;
    }
    if !store
        .boards
        .iter()
        .any(|board| board.id == store.current_board_id)
    {
        store.current_board_id = store.boards[0].id.clone();
    }
}

fn corrupt_store_error(path: &Path, reason: impl AsRef<str>) -> AgentError {
    let backup = backup_corrupt_store(path);
    let backup_detail = backup
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<backup failed>".to_string());
    AgentError::Config(format!(
        "Refusing to open corrupt kanban store at {}: {}. Original preserved; backup at {}.",
        path.display(),
        reason.as_ref(),
        backup_detail
    ))
}

fn backup_corrupt_store(path: &Path) -> Option<PathBuf> {
    let resolved = path.canonicalize().ok()?;
    let parent = resolved.parent()?.to_path_buf();
    let base_name = resolved.file_name()?.to_string_lossy();
    let timestamp = jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ");
    let base_backup_name = format!("{base_name}.corrupt.{timestamp}");
    for idx in 0..100 {
        let backup_name = if idx == 0 {
            base_backup_name.clone()
        } else {
            format!("{base_backup_name}.{idx}")
        };
        let candidate = parent.join(backup_name);
        if candidate.parent() != Some(parent.as_path()) || candidate.exists() {
            continue;
        }
        if std::fs::copy(&resolved, &candidate).is_ok() {
            return Some(candidate);
        }
    }
    None
}
