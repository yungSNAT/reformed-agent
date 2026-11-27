//! Session persistence — save and load conversation sessions.
//!
//! Provides SQLite-backed session storage with optional FTS5 indexing for search,
//! human-readable markdown session logs, and trajectory format for RL training.
//!
//! Corresponds to Python `run_agent.py`'s `_persist_session`, `_save_session_log`,
//! and `_save_trajectory` methods.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use hermes_core::{AgentError, AgentResult, Message, MessageRole};
use jiff::{SignedDuration, Timestamp};

// ---------------------------------------------------------------------------
// SessionPersistence
// ---------------------------------------------------------------------------

/// Join leading consecutive system messages (Python `_cached_system_prompt` / Anthropic prefix parity for persistence).
pub fn leading_system_prompt_for_persist(messages: &[Message]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for m in messages {
        if m.role != MessageRole::System {
            break;
        }
        if let Some(c) = m
            .content
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            parts.push(c.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Save a gateway or HTTP API run as session `session_key` under `home` with
/// [`SessionPersistence::persist_agent_result`]. A blank `home` or
/// `session_key` saves nothing. A failed save is logged as a warning and the
/// reply still goes out.
pub fn persist_gateway_result(
    home: Option<&str>,
    session_key: &str,
    result: &AgentResult,
    model: &str,
    platform: &str,
) {
    let Some(home) = home.map(str::trim).filter(|h| !h.is_empty()) else {
        return;
    };
    if session_key.trim().is_empty() {
        return;
    }
    if let Err(err) = SessionPersistence::new(home).persist_agent_result(
        session_key,
        result,
        Some(model),
        Some(platform),
    ) {
        // The session key can carry a chat or user id, so it stays out of the log.
        tracing::warn!("failed to save a {} session: {}", platform, err);
    }
}

/// Manages session persistence to SQLite and markdown log files.
pub struct SessionPersistence {
    /// Path to the SQLite database file.
    db_path: PathBuf,
    /// Directory for session log files.
    sessions_dir: PathBuf,
    /// Directory for trajectory files.
    trajectories_dir: PathBuf,
}

/// Result of one startup auto-maintenance pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMaintenanceResult {
    pub skipped: bool,
    pub pruned: u64,
    pub vacuumed: bool,
    pub error: Option<String>,
}

/// Result of a soft rewind operation against persisted session rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindOutcome {
    pub target_message_id: i64,
    pub target_content: Option<String>,
    pub inactive_count: u64,
    pub active_message_count: u64,
    pub rewind_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessageRef {
    pub id: i64,
    pub content: Option<String>,
}

/// A session recorded by another install, written with its recorded times,
/// lineage and end state.
pub struct ImportedSession<'a> {
    pub id: &'a str,
    pub model: Option<&'a str>,
    pub platform: Option<&'a str>,
    pub title: Option<&'a str>,
    pub system_prompt: Option<&'a str>,
    pub model_config: Option<&'a str>,
    pub parent_session_id: Option<&'a str>,
    pub created_at: &'a str,
    pub updated_at: &'a str,
    pub ended_at: Option<&'a str>,
    pub end_reason: Option<&'a str>,
    /// Active messages in order, each with the time it was recorded.
    pub messages: &'a [(Message, String)],
}

/// Adds imported sessions inside the transaction opened by
/// [`SessionPersistence::import_sessions`].
pub struct SessionImport<'a> {
    conn: &'a rusqlite::Connection,
}

/// Result of a malformed `sessions.db` schema repair attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaRepairReport {
    pub repaired: bool,
    pub strategy: Option<String>,
    pub backup_path: Option<PathBuf>,
    pub error: Option<String>,
}

static SCHEMA_REPAIR_ATTEMPTS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
static FTS_TRIGGER_REPAIR_FAILURES: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

impl SessionPersistence {
    fn model_config_has_non_null_marker(model_config: Option<&str>, key: &str) -> bool {
        let Some(raw) = model_config
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return false;
        };
        serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|value| value.get(key).cloned())
            .is_some_and(|marker| !marker.is_null())
    }

    fn is_explicit_non_compression_child(
        model_config: Option<&str>,
        platform: Option<&str>,
    ) -> bool {
        Self::model_config_has_non_null_marker(model_config, "_branched_from")
            || Self::model_config_has_non_null_marker(model_config, "_delegate_from")
            || platform.map(str::trim) == Some("tool")
    }

    const FTS_TABLES: &'static [&'static str] = &["messages_fts", "messages_fts_trigram"];

    pub fn is_malformed_db_error_message(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("malformed database schema")
            || lower.contains("database disk image is malformed")
    }

    fn is_malformed_sqlite_error(error: &rusqlite::Error) -> bool {
        Self::is_malformed_db_error_message(&error.to_string())
    }

    fn is_malformed_agent_error(error: &AgentError) -> bool {
        Self::is_malformed_db_error_message(&error.to_string())
    }

    fn claim_schema_repair_attempt(db_path: &Path) -> bool {
        Self::claim_repair_attempt(&SCHEMA_REPAIR_ATTEMPTS, db_path)
    }

    fn repair_key(db_path: &Path) -> PathBuf {
        db_path
            .canonicalize()
            .unwrap_or_else(|_| db_path.to_path_buf())
    }

    /// True the first time this process claims `db_path` in `attempts`.
    fn claim_repair_attempt(
        attempts: &'static OnceLock<Mutex<HashSet<PathBuf>>>,
        db_path: &Path,
    ) -> bool {
        let attempts = attempts.get_or_init(|| Mutex::new(HashSet::new()));
        let Ok(mut attempts) = attempts.lock() else {
            return false;
        };
        attempts.insert(Self::repair_key(db_path))
    }

    fn repair_failed_before(
        attempts: &'static OnceLock<Mutex<HashSet<PathBuf>>>,
        db_path: &Path,
    ) -> bool {
        attempts
            .get()
            .and_then(|attempts| attempts.lock().ok())
            .is_some_and(|attempts| attempts.contains(&Self::repair_key(db_path)))
    }

    fn is_fts5_unavailable_message(message: &str) -> bool {
        hermes_core::is_fts5_unavailable_message(message)
    }

    fn is_fts5_unavailable_error(error: &rusqlite::Error) -> bool {
        Self::is_fts5_unavailable_message(&error.to_string())
    }

    fn fts_table_exists(conn: &rusqlite::Connection) -> Result<bool, AgentError> {
        Self::named_fts_table_exists(conn, "messages_fts")
    }

    fn named_fts_table_exists(
        conn: &rusqlite::Connection,
        table: &str,
    ) -> Result<bool, AgentError> {
        if !Self::FTS_TABLES.contains(&table) {
            return Err(AgentError::Config(format!(
                "Unsupported FTS table name for sessions db maintenance: {table}"
            )));
        }
        let sql = format!("SELECT 1 FROM {table} LIMIT 0");
        match conn.prepare(&sql) {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::SqliteFailure(_, Some(message)))
                if message.contains("no such table")
                    || Self::is_fts5_unavailable_message(&message) =>
            {
                Ok(false)
            }
            Err(err) if Self::is_fts5_unavailable_error(&err) => Ok(false),
            Err(err) => Err(AgentError::Io(format!(
                "Failed to inspect {table} availability: {err}"
            ))),
        }
    }

    fn optimize_fts_on_conn(conn: &rusqlite::Connection) -> Result<u32, AgentError> {
        let mut optimized = 0u32;
        for table in Self::FTS_TABLES {
            if !Self::named_fts_table_exists(conn, table)? {
                continue;
            }
            let sql = format!("INSERT INTO {table}({table}) VALUES('optimize')");
            match conn.execute(&sql, []) {
                Ok(_) => optimized += 1,
                Err(err) => {
                    tracing::warn!("FTS optimize failed for {}: {}", table, err);
                }
            }
        }
        Ok(optimized)
    }

    const FTS_INSERT_TRIGGER_SQL: &'static str =
        "CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
            INSERT INTO messages_fts(rowid, content, session_id, role)
            VALUES (new.id, new.content, new.session_id, new.role);
        END;";

    /// The index keeps no copy of the text (`content='messages'`), so FTS5 can only remove a row's
    /// postings when told the values it indexed: the delete and update triggers pass the old row
    /// with the `'delete'` command. A plain `DELETE FROM messages_fts` looks the row up in
    /// `messages`, which by then no longer holds those values, and leaves the index damaged.
    const FTS_DELETE_TRIGGERS_SQL: &'static str = "DROP TRIGGER IF EXISTS messages_ad;
        DROP TRIGGER IF EXISTS messages_au;

        CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content, session_id, role)
            VALUES ('delete', old.id, old.content, old.session_id, old.role);
        END;

        CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content, session_id, role)
            VALUES ('delete', old.id, old.content, old.session_id, old.role);
            INSERT INTO messages_fts(rowid, content, session_id, role)
            VALUES (new.id, new.content, new.session_id, new.role);
        END;";

    /// How many of the delete and update triggers exist, and how many use the `'delete'` command.
    fn fts_delete_trigger_state(conn: &rusqlite::Connection) -> rusqlite::Result<(i64, i64)> {
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(sql NOT LIKE '%DELETE FROM messages_fts%'), 0)
             FROM sqlite_master
             WHERE type = 'trigger' AND name IN ('messages_ad', 'messages_au')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    /// Only an index that no longer matches `messages` (SQLITE_CORRUPT_VTAB) is rebuilt; other
    /// corruption is damage to the file itself, which a rebuild would write into.
    fn is_fts_index_mismatch(error: &rusqlite::Error) -> bool {
        matches!(
            error,
            rusqlite::Error::SqliteFailure(e, _)
                if e.extended_code == rusqlite::ffi::SQLITE_CORRUPT_VTAB
        )
    }

    /// Installs the delete and update triggers, replacing any from before the `'delete'` command,
    /// and rebuilds the index only if FTS5's own check finds it no longer matches `messages`: one
    /// transaction, so a failure changes nothing. A repair that fails for a reason other than
    /// another connection's lock is not retried in this process while both old triggers remain,
    /// so it does not rerun the full check on every open; a lock wait is retried on the next open.
    fn install_fts_delete_triggers(
        conn: &rusqlite::Connection,
        db_path: &Path,
    ) -> Result<(), AgentError> {
        let io = |step: &str, e: rusqlite::Error| {
            AgentError::Io(format!(
                "Failed to {step} for the sessions FTS triggers: {e}"
            ))
        };
        let (present, current) =
            Self::fts_delete_trigger_state(conn).map_err(|e| io("inspect the schema", e))?;
        if current == 2
            || (present == 2 && Self::repair_failed_before(&FTS_TRIGGER_REPAIR_FAILURES, db_path))
        {
            return Ok(());
        }
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| io("begin a repair", e))?;
        // Another process may have installed them since the check above.
        let (_, current) =
            Self::fts_delete_trigger_state(&tx).map_err(|e| io("inspect the schema", e))?;
        if current == 2 {
            return Ok(());
        }
        let repaired = tx
            .execute_batch(Self::FTS_DELETE_TRIGGERS_SQL)
            .map_err(|e| ("create the triggers", e))
            .and_then(|()| {
                match tx.execute(
                    "INSERT INTO messages_fts(messages_fts, rank) VALUES('integrity-check', 1)",
                    [],
                ) {
                    Ok(_) => Ok(false),
                    Err(e) if Self::is_fts_index_mismatch(&e) => tx
                        .execute(
                            "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
                            [],
                        )
                        .map(|_| true)
                        .map_err(|e| ("rebuild the index", e)),
                    Err(e) => Err(("check the index", e)),
                }
            })
            .and_then(|rebuilt| {
                tx.commit()
                    .map(|()| rebuilt)
                    .map_err(|e| ("commit a repair", e))
            });
        match repaired {
            Ok(rebuilt) => {
                if present > 0 {
                    tracing::info!(
                        rebuilt,
                        "replaced the sessions FTS delete and update triggers"
                    );
                }
                Ok(())
            }
            Err((step, e)) => {
                // Waiting on another connection, such as a reader holding off the commit, is
                // retried on the next open, like failing to begin.
                if !matches!(
                    e.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) {
                    Self::claim_repair_attempt(&FTS_TRIGGER_REPAIR_FAILURES, db_path);
                }
                Err(io(step, e))
            }
        }
    }

    fn ensure_fts_schema(conn: &rusqlite::Connection, db_path: &Path) -> Result<bool, AgentError> {
        // Rows saved whilst the insert trigger was missing are not in the index: once it is
        // back, rebuild the index from `messages`.
        let insert_trigger_missing = Self::fts_table_exists(conn)?
            && conn
                .query_row(
                    "SELECT COUNT(*) = 0 FROM sqlite_master WHERE type = 'trigger' AND name = 'messages_ai'",
                    [],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(|e| {
                    AgentError::Io(format!("Failed to inspect the sessions FTS triggers: {e}"))
                })?;
        if insert_trigger_missing {
            // One transaction: if the rebuild fails, the trigger stays missing and the next open
            // tries again.
            let restored = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )
            .and_then(|tx| {
                tx.execute_batch(Self::FTS_INSERT_TRIGGER_SQL)?;
                tx.execute(
                    "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
                    [],
                )?;
                tx.commit()
            });
            restored.map_err(|e| {
                AgentError::Io(format!(
                    "Failed to restore the sessions FTS insert trigger and rebuild the index: {e}"
                ))
            })?;
            tracing::info!(
                "rebuilt the sessions FTS index after restoring its insert trigger ({})",
                db_path.display()
            );
        }
        Self::ensure_fts_tables_and_triggers(conn, db_path)
    }

    fn ensure_fts_tables_and_triggers(
        conn: &rusqlite::Connection,
        db_path: &Path,
    ) -> Result<bool, AgentError> {
        match conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                session_id UNINDEXED,
                role UNINDEXED,
                content='messages',
                content_rowid='id'
            );

            {}",
            Self::FTS_INSERT_TRIGGER_SQL
        )) {
            Ok(()) => match Self::install_fts_delete_triggers(conn, db_path) {
                Ok(()) => Ok(true),
                // A store that keeps both old triggers stays usable; search falls back on damage.
                Err(err) if matches!(Self::fts_delete_trigger_state(conn), Ok((2, _))) => {
                    tracing::warn!("{} ({})", err, db_path.display());
                    Ok(true)
                }
                Err(err) => Err(err),
            },
            Err(err) if Self::is_fts5_unavailable_error(&err) => {
                tracing::warn!(
                    "SQLite FTS5 unavailable for {}; session persistence will continue without full-text indexing: {}",
                    db_path.display(),
                    err
                );
                Ok(false)
            }
            Err(err) => Err(AgentError::Io(format!(
                "Failed to create FTS schema for sessions db: {err}"
            ))),
        }
    }

    fn rebuild_primary_fts_index(conn: &rusqlite::Connection) -> Result<(), AgentError> {
        if !Self::fts_table_exists(conn)? {
            return Ok(());
        }
        conn.execute(
            "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
            [],
        )
        .map(|_| ())
        .map_err(|e| AgentError::Io(format!("Failed to rebuild messages_fts: {e}")))
    }

    fn ensure_text_column(
        conn: &rusqlite::Connection,
        table: &str,
        column: &str,
    ) -> Result<(), AgentError> {
        match conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"), []) {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("duplicate column") {
                    Ok(())
                } else {
                    Err(AgentError::Io(format!(
                        "Failed to migrate {table}.{column}: {e}"
                    )))
                }
            }
        }
    }

    fn ensure_numeric_column(
        conn: &rusqlite::Connection,
        table: &str,
        column: &str,
        definition: &str,
    ) -> Result<(), AgentError> {
        let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {definition}");
        match conn.execute(&sql, []) {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("duplicate column") {
                    Ok(())
                } else {
                    Err(AgentError::Io(format!(
                        "Failed to migrate {table}.{column}: {e}"
                    )))
                }
            }
        }
    }

    /// Create a new persistence manager rooted at the given hermes home directory.
    pub fn new(hermes_home: impl AsRef<Path>) -> Self {
        let home = hermes_home.as_ref();
        Self {
            db_path: home.join("sessions.db"),
            sessions_dir: home.join("sessions"),
            trajectories_dir: home.join("trajectories"),
        }
    }

    /// Path to the SQLite session database.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn snapshot_file_is_empty_session(path: &Path, session_id: &str) -> bool {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return false;
        };
        let Some(snapshot_session_id) = value
            .get("session_info")
            .and_then(|info| info.get("session_id"))
            .and_then(|value| value.as_str())
        else {
            return false;
        };
        if snapshot_session_id != session_id {
            return false;
        }
        value
            .get("messages")
            .and_then(|messages| messages.as_array())
            .is_some_and(|messages| messages.is_empty())
    }

    fn remove_empty_session_files(&self, session_id: &str) -> Result<bool, AgentError> {
        let mut removed = false;

        let json_path = self.sessions_dir.join(format!("{session_id}.json"));
        if Self::snapshot_file_is_empty_session(&json_path, session_id) {
            std::fs::remove_file(&json_path).map_err(|e| {
                AgentError::Io(format!(
                    "Failed to remove empty session snapshot {}: {e}",
                    json_path.display()
                ))
            })?;
            removed = true;
        }

        let jsonl_path = self.sessions_dir.join(format!("{session_id}.jsonl"));
        if jsonl_path
            .metadata()
            .map(|metadata| metadata.len() == 0)
            .unwrap_or(false)
        {
            std::fs::remove_file(&jsonl_path).map_err(|e| {
                AgentError::Io(format!(
                    "Failed to remove empty session transcript {}: {e}",
                    jsonl_path.display()
                ))
            })?;
            removed = true;
        }

        Ok(removed)
    }

    fn copy_if_exists(src: &Path, dst: &Path) -> std::io::Result<bool> {
        if !src.exists() {
            return Ok(false);
        }
        std::fs::copy(src, dst)?;
        Ok(true)
    }

    fn backup_db_file(db_path: &Path) -> Option<PathBuf> {
        let stamp = jiff::Timestamp::now().strftime("%Y%m%d_%H%M%S");
        let backup_path = db_path.with_file_name(format!(
            "{}.malformed-backup-{stamp}",
            db_path.file_name()?.to_string_lossy()
        ));
        if let Err(err) = std::fs::copy(db_path, &backup_path) {
            tracing::warn!(
                "could not back up malformed sessions db {}: {}",
                db_path.display(),
                err
            );
            return None;
        }

        for suffix in ["-wal", "-shm"] {
            let sidecar = db_path.with_file_name(format!(
                "{}{suffix}",
                db_path.file_name().unwrap_or_default().to_string_lossy()
            ));
            let backup_sidecar = backup_path.with_file_name(format!(
                "{}{suffix}",
                backup_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ));
            if let Err(err) = Self::copy_if_exists(&sidecar, &backup_sidecar) {
                tracing::warn!(
                    "could not back up sessions db sidecar {}: {}",
                    sidecar.display(),
                    err
                );
            }
        }

        Some(backup_path)
    }

    fn db_opens_cleanly_path(db_path: &Path) -> Option<String> {
        let conn = match rusqlite::Connection::open(db_path) {
            Ok(conn) => conn,
            Err(err) => return Some(err.to_string()),
        };

        if let Err(err) = conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0)) {
            return Some(err.to_string());
        }

        let mut stmt = match conn.prepare("PRAGMA integrity_check") {
            Ok(stmt) => stmt,
            Err(err) => return Some(err.to_string()),
        };
        let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
            Ok(rows) => rows,
            Err(err) => return Some(err.to_string()),
        };
        let mut problems = Vec::new();
        for row in rows {
            match row {
                Ok(value) if value.eq_ignore_ascii_case("ok") => {}
                Ok(value) => problems.push(value),
                Err(err) => return Some(err.to_string()),
            }
            if problems.len() >= 3 {
                break;
            }
        }
        if !problems.is_empty() {
            return Some(problems.join("; "));
        }

        match conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| {
            row.get::<_, i64>(0)
        }) {
            Ok(_) => None,
            Err(err) => Some(err.to_string()),
        }
    }

    pub fn db_health_error(&self) -> Option<String> {
        if !self.db_path.exists() {
            return None;
        }
        Self::db_opens_cleanly_path(&self.db_path)
    }

    fn repair_schema_dedup_pass(db_path: &Path) -> Result<(), AgentError> {
        let conn = rusqlite::Connection::open(db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open malformed sessions db: {e}")))?;
        conn.execute_batch("PRAGMA writable_schema=ON")
            .map_err(|e| AgentError::Io(format!("Failed to enable writable_schema: {e}")))?;
        {
            let mut stmt = conn
                .prepare(
                    "SELECT type, name, MIN(rowid) AS keep
                     FROM sqlite_master
                     GROUP BY type, name
                     HAVING COUNT(*) > 1",
                )
                .map_err(|e| AgentError::Io(format!("Failed to inspect sqlite_master: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|e| AgentError::Io(format!("Failed to query sqlite_master: {e}")))?;
            for row in rows {
                let (object_type, name, keep_rowid) =
                    row.map_err(|e| AgentError::Io(format!("Failed to read schema row: {e}")))?;
                conn.execute(
                    "DELETE FROM sqlite_master
                     WHERE type = ?1 AND name = ?2 AND rowid <> ?3",
                    rusqlite::params![object_type, name, keep_rowid],
                )
                .map_err(|e| AgentError::Io(format!("Failed to deduplicate sqlite_master: {e}")))?;
            }
        }
        conn.execute_batch("PRAGMA writable_schema=OFF")
            .map_err(|e| AgentError::Io(format!("Failed to disable writable_schema: {e}")))?;
        Ok(())
    }

    fn repair_schema_drop_fts_pass(&self) -> Result<(), AgentError> {
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open malformed sessions db: {e}")))?;
        conn.execute_batch("PRAGMA writable_schema=ON")
            .map_err(|e| AgentError::Io(format!("Failed to enable writable_schema: {e}")))?;
        conn.execute(
            "DELETE FROM sqlite_master
             WHERE name LIKE 'messages_fts%'
                OR tbl_name LIKE 'messages_fts%'
                OR sql LIKE '%messages_fts%'",
            [],
        )
        .map_err(|e| AgentError::Io(format!("Failed to drop FTS schema objects: {e}")))?;
        conn.execute_batch("PRAGMA writable_schema=OFF")
            .map_err(|e| AgentError::Io(format!("Failed to disable writable_schema: {e}")))?;
        conn.execute_batch("VACUUM")
            .map_err(|e| AgentError::Io(format!("Failed to vacuum repaired sessions db: {e}")))?;
        drop(conn);

        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to reopen repaired sessions db: {e}")))?;
        Self::ensure_fts_schema(&conn, &self.db_path)?;
        Self::rebuild_primary_fts_index(&conn)
    }

    pub fn repair_malformed_schema(&self, backup: bool) -> SchemaRepairReport {
        let mut report = SchemaRepairReport {
            repaired: false,
            strategy: None,
            backup_path: None,
            error: None,
        };

        if !self.db_path.exists() {
            report.error = Some(format!("{} does not exist", self.db_path.display()));
            return report;
        }

        if backup {
            report.backup_path = Self::backup_db_file(&self.db_path);
        }

        if let Err(err) = Self::repair_schema_dedup_pass(&self.db_path) {
            tracing::warn!("sessions db schema dedup repair failed: {}", err);
        } else if Self::db_opens_cleanly_path(&self.db_path).is_none() {
            report.repaired = true;
            report.strategy = Some("dedup_schema".to_string());
            return report;
        }

        match self.repair_schema_drop_fts_pass() {
            Ok(()) => {
                let reason = Self::db_opens_cleanly_path(&self.db_path);
                if reason.is_none() {
                    report.repaired = true;
                    report.strategy = Some("drop_fts_rebuild".to_string());
                } else {
                    report.error = reason;
                }
            }
            Err(err) => {
                report.error = Some(err.to_string());
            }
        }

        report
    }

    /// Number of queryable FTS indexes in the session database.
    pub fn fts_index_count(&self) -> Result<u32, AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let mut count = 0u32;
        for table in Self::FTS_TABLES {
            if Self::named_fts_table_exists(&conn, table)? {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Create using default home resolution:
    /// `HERMES_HOME` → `HERMES_AGENT_ULTRA_HOME` → `~/.hermes-agent-ultra`,
    /// never upstream's `~/.hermes`.
    pub fn default_home() -> Self {
        Self::new(crate::memory_plugins::default_hermes_home())
    }

    pub fn ensure_db(&self) -> Result<(), AgentError> {
        match self.ensure_db_inner() {
            Ok(()) => Ok(()),
            Err(err)
                if Self::is_malformed_agent_error(&err)
                    && Self::claim_schema_repair_attempt(&self.db_path) =>
            {
                tracing::error!(
                    "sessions db schema is malformed ({}); attempting one automatic repair",
                    err
                );
                let report = self.repair_malformed_schema(true);
                if !report.repaired {
                    return Err(AgentError::Io(format!(
                        "sessions db malformed and repair failed: {}; backup: {}",
                        report
                            .error
                            .as_deref()
                            .unwrap_or("repair did not return a concrete error"),
                        report
                            .backup_path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "not created".to_string())
                    )));
                }
                self.ensure_db_inner()
            }
            Err(err) => Err(err),
        }
    }

    /// Ensure the SQLite database and tables exist.
    fn ensure_db_inner(&self) -> Result<(), AgentError> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("Failed to create db directory: {e}")))?;
        }

        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;

        if let Err(err) = conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0)) {
            if Self::is_malformed_sqlite_error(&err) {
                return Err(AgentError::Io(format!(
                    "sessions db schema malformed before initialization: {err}"
                )));
            }
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                model TEXT,
                platform TEXT DEFAULT 'cli',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                title TEXT,
                message_count INTEGER DEFAULT 0,
                system_prompt TEXT,
                parent_session_id TEXT,
                model_config TEXT,
                end_reason TEXT,
                ended_at TEXT,
                rewind_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_call_id TEXT,
                name TEXT,
                tool_calls TEXT,
                reasoning_content TEXT,
                created_at TEXT NOT NULL,
                active INTEGER NOT NULL DEFAULT 1,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON messages(session_id);

            CREATE TABLE IF NOT EXISTS state_meta (
                key TEXT PRIMARY KEY,
                value TEXT
            );",
        )
        .map_err(|e| AgentError::Io(format!("Failed to create tables: {e}")))?;

        for column in [
            "system_prompt",
            "parent_session_id",
            "model_config",
            "end_reason",
            "ended_at",
        ] {
            Self::ensure_text_column(&conn, "sessions", column)?;
        }
        Self::ensure_text_column(&conn, "messages", "reasoning_content")?;
        Self::ensure_text_column(&conn, "messages", "name")?;
        Self::ensure_numeric_column(
            &conn,
            "sessions",
            "rewind_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Self::ensure_numeric_column(&conn, "messages", "active", "INTEGER NOT NULL DEFAULT 1")?;
        Self::ensure_numeric_column(&conn, "sessions", "spent_usd", "REAL NOT NULL DEFAULT 0")?;
        Self::ensure_numeric_column(
            &conn,
            "sessions",
            "budget_pending",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_session_active
                ON messages(session_id, active, id)",
            [],
        )
        .map_err(|e| AgentError::Io(format!("Failed to create active message index: {e}")))?;
        Self::ensure_fts_schema(&conn, &self.db_path)?;

        Ok(())
    }

    /// Read this session's priced spend. Older sessions start at zero.
    pub fn session_spend_usd(&self, id: &str) -> Result<f64, AgentError> {
        self.ensure_db_inner()?;
        let conn = self.budget_connection()?;
        let spent: f64 = conn
            .query_row(
                "SELECT COALESCE((SELECT spent_usd FROM sessions WHERE id = ?1), 0.0)",
                [id],
                |row| row.get(0),
            )
            .map_err(|e| AgentError::Io(format!("Failed to read session spend: {e}")))?;
        if !spent.is_finite() || spent < 0.0 {
            return Err(AgentError::Io("Invalid saved session spend".into()));
        }
        Ok(spent)
    }

    /// Serialise capped runs and explicit resets for this session. The OS
    /// releases the lock when a process exits; keep the lock file's inode stable.
    pub(crate) fn lock_session_budget(&self, id: &str) -> Result<std::fs::File, AgentError> {
        use sha2::{Digest, Sha256};
        let dir = self.db_path.with_file_name("session-budget-locks");
        std::fs::create_dir_all(&dir)
            .map_err(|e| AgentError::Io(format!("Cannot open budget lock directory: {e}")))?;
        let path = dir.join(format!("{:x}", Sha256::digest(id.as_bytes())));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| AgentError::Io(format!("Cannot open session budget lock: {e}")))?;
        file.try_lock().map_err(|e| {
            AgentError::Io(format!(
                "Cannot lock session budget for this run or reset: {e}"
            ))
        })?;
        Ok(file)
    }

    fn budget_connection(&self) -> Result<rusqlite::Connection, AgentError> {
        rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open session budget: {e}")))
    }

    /// Record the outstanding request before dispatch. An interrupted request
    /// with unconfirmed spend must be reconciled before another one starts.
    pub(crate) fn begin_budget_request(&self, id: &str) -> Result<(), AgentError> {
        let now = hermes_core::time::now_rfc3339();
        let changed = self.budget_connection()?.execute(
            "INSERT INTO sessions (id, created_at, updated_at, budget_pending) VALUES (?1, ?2, ?2, 1)
             ON CONFLICT(id) DO UPDATE SET budget_pending = 1 WHERE sessions.budget_pending = 0",
            rusqlite::params![id, now],
        ).map_err(|e| AgentError::Io(format!("Cannot reserve session budget: {e}")))?;
        if changed == 0 {
            return Err(AgentError::Io("Previous provider request has unconfirmed spend. Check the provider's usage and /reset to start a new budget.".into()));
        }
        Ok(())
    }

    pub(crate) fn charge_session(&self, id: &str, usd: f64) -> Result<(), AgentError> {
        if !usd.is_finite() || usd < 0.0 {
            return Err(AgentError::Io(
                "Invalid provider charge; session spend is unconfirmed".into(),
            ));
        }
        let changed = self
            .budget_connection()?
            .execute(
                "UPDATE sessions SET spent_usd = spent_usd + ?2, budget_pending = 0
             WHERE id = ?1 AND budget_pending = 1",
                rusqlite::params![id, usd],
            )
            .map_err(|e| AgentError::Io(format!("Failed to record session spend: {e}")))?;
        if changed != 1 {
            return Err(AgentError::Io("Missing pending session charge".into()));
        }
        Ok(())
    }

    /// Clear a session's budget when the interactive user resets that session.
    pub fn reset_session_spend(&self, id: &str) -> Result<(), AgentError> {
        let _lock = self.lock_session_budget(id)?;
        if !self
            .db_path
            .try_exists()
            .map_err(|e| AgentError::Io(e.to_string()))?
        {
            return Ok(());
        }
        self.ensure_db_inner()?;
        self.budget_connection()?
            .execute(
                "UPDATE sessions SET spent_usd = 0, budget_pending = 0 WHERE id = ?1",
                [id],
            )
            .map_err(|e| AgentError::Io(format!("Failed to reset session spend: {e}")))?;
        Ok(())
    }

    /// Read a metadata key from `state_meta`.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>, AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT value FROM state_meta WHERE key = ?1")
            .map_err(|e| AgentError::Io(format!("Failed to prepare meta query: {e}")))?;
        match stmt.query_row(rusqlite::params![key], |row| row.get::<_, String>(0)) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AgentError::Io(format!("Failed to read state_meta: {e}"))),
        }
    }

    /// Upsert a metadata key in `state_meta`.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        conn.execute(
            "INSERT INTO state_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
        .map_err(|e| AgentError::Io(format!("Failed to upsert state_meta: {e}")))?;
        Ok(())
    }

    /// Merge fragmented FTS5 b-tree segments in `sessions.db`.
    ///
    /// This is a layout-only maintenance operation. Search rows and snippets are
    /// preserved, while long-lived FTS indexes can collapse many incremental
    /// segments into a smaller/faster representation. Optional or unavailable
    /// FTS indexes are skipped.
    pub fn optimize_fts(&self) -> Result<u32, AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        Self::optimize_fts_on_conn(&conn)
    }

    /// Reclaim free pages in `sessions.db` after large prune operations.
    pub fn vacuum(&self) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        if let Err(err) = Self::optimize_fts_on_conn(&conn) {
            tracing::warn!("FTS optimize before VACUUM failed: {}", err);
        }
        conn.execute_batch("VACUUM")
            .map_err(|e| AgentError::Io(format!("Failed to VACUUM sessions db: {e}")))?;
        Ok(())
    }

    /// Flush committed WAL frames and truncate the `sessions.db-wal` sidecar.
    ///
    /// SQLite's PASSIVE checkpoint leaves the WAL file at its high-water mark;
    /// TRUNCATE reclaims the sidecar after startup maintenance or explicit
    /// shutdown hooks. Databases not currently in WAL mode accept this pragma
    /// and return a no-op result.
    pub fn truncate_wal_checkpoint(&self) -> Result<(), AgentError> {
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
        let (busy, log_frames, checkpointed_frames): (i64, i64, i64) = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| AgentError::Io(format!("Failed to checkpoint sessions WAL: {e}")))?;

        if busy > 0 {
            tracing::warn!(
                "sessions.db WAL checkpoint could not truncate immediately: busy={}, log_frames={}, checkpointed_frames={}",
                busy,
                log_frames,
                checkpointed_frames
            );
        } else if log_frames > 0 {
            tracing::debug!(
                "sessions.db WAL checkpoint truncated {} frame(s); checkpointed={}",
                log_frames,
                checkpointed_frames
            );
        }
        Ok(())
    }

    /// Delete sessions whose `updated_at` is older than the retention window.
    /// As in Hermes Agent, a row that ended with compression goes only with
    /// every continuation after it, so a session keeps its compressed turns.
    ///
    /// Returns the number of deleted sessions.
    pub fn prune_sessions(&self, older_than_days: u32) -> Result<u64, AgentError> {
        self.ensure_db()?;
        // No session predates the earliest instant, so a longer window stops
        // there instead of overflowing.
        let cutoff = hermes_core::time::rfc3339(
            Timestamp::now()
                .checked_sub(SignedDuration::from_hours(24 * i64::from(older_than_days)))
                .unwrap_or(Timestamp::MIN),
        );
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;

        let mut session_ids: Vec<String> = Vec::new();
        {
            let mut stmt = conn
                .prepare(
                    "WITH RECURSIVE kept(id) AS (
                         SELECT s.parent_session_id FROM sessions s
                         JOIN sessions p ON p.id = s.parent_session_id
                         WHERE s.updated_at >= ?1 AND p.end_reason = 'compression'
                         UNION
                         SELECT p.parent_session_id FROM kept k
                         JOIN sessions p ON p.id = k.id
                         JOIN sessions g ON g.id = p.parent_session_id
                         WHERE g.end_reason = 'compression'
                     )
                     SELECT id FROM sessions
                     WHERE updated_at < ?1 AND id NOT IN (SELECT id FROM kept)",
                )
                .map_err(|e| AgentError::Io(format!("Failed to prepare prune query: {e}")))?;
            let rows = stmt
                .query_map(rusqlite::params![cutoff], |row| row.get::<_, String>(0))
                .map_err(|e| AgentError::Io(format!("Failed to query stale sessions: {e}")))?;
            for row in rows {
                session_ids.push(
                    row.map_err(|e| AgentError::Io(format!("Failed to read session id: {e}")))?,
                );
            }
        }
        if session_ids.is_empty() {
            return Ok(0);
        }

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| AgentError::Io(format!("Failed to open prune transaction: {e}")))?;
        for sid in &session_ids {
            tx.execute(
                "DELETE FROM messages WHERE session_id = ?1",
                rusqlite::params![sid],
            )
            .map_err(|e| AgentError::Io(format!("Failed to delete message rows: {e}")))?;
            tx.execute("DELETE FROM sessions WHERE id = ?1", rusqlite::params![sid])
                .map_err(|e| AgentError::Io(format!("Failed to delete session row: {e}")))?;
        }
        tx.commit()
            .map_err(|e| AgentError::Io(format!("Failed to commit prune transaction: {e}")))?;
        if let Some(home) = self.sessions_dir.parent() {
            let archives = home.join(crate::context::tool_result_prune::PRUNED_SUBDIR);
            for sid in &session_ids {
                crate::context::cache_keep_alive::forget(sid);
                crate::sub_agent_orchestrator::forget_session_results(home, sid);
                crate::context::tool_result_prune::remove_session_archives(&archives, sid);
            }
        }
        Ok(session_ids.len() as u64)
    }

    /// Delete a session from the store: its row, messages and search entries
    /// in one transaction, then its `.jsonl` transcript. As in Hermes Agent, a
    /// session that names it as parent is kept and loses the link. Returns
    /// whether the store held the session.
    pub fn delete_session(&self, session_id: &str) -> Result<bool, AgentError> {
        // The id names a file below, so it must be a plain file name.
        if Path::new(session_id).file_name() != Some(std::ffi::OsStr::new(session_id)) {
            return Err(AgentError::Config(format!(
                "Invalid session id '{session_id}'"
            )));
        }
        let mut deleted = false;
        if self.db_path.exists() {
            self.ensure_db()?;
            let conn = rusqlite::Connection::open(&self.db_path)
                .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| AgentError::Io(format!("Failed to open delete transaction: {e}")))?;
            deleted = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                    rusqlite::params![session_id],
                    |row| row.get(0),
                )
                .map_err(|e| AgentError::Io(format!("Failed to look up session: {e}")))?;
            // As upstream, a session the store does not hold changes nothing there.
            if deleted {
                for sql in [
                    "UPDATE sessions SET parent_session_id = NULL WHERE parent_session_id = ?1",
                    "DELETE FROM messages WHERE session_id = ?1",
                    "DELETE FROM sessions WHERE id = ?1",
                ] {
                    tx.execute(sql, rusqlite::params![session_id])
                        .map_err(|e| {
                            AgentError::Io(format!("Failed to delete session data: {e}"))
                        })?;
                }
                tx.commit()
                    .map_err(|e| AgentError::Io(format!("Failed to commit session delete: {e}")))?;
            }
        }
        crate::context::cache_keep_alive::forget(session_id);
        if let Some(home) = self.sessions_dir.parent() {
            crate::sub_agent_orchestrator::forget_session_results(home, session_id);
            let archives = home.join(crate::context::tool_result_prune::PRUNED_SUBDIR);
            crate::context::tool_result_prune::remove_session_archives(&archives, session_id);
        }
        let transcript = self.sessions_dir.join(format!("{session_id}.jsonl"));
        match std::fs::remove_file(&transcript) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(AgentError::Io(format!(
                "Failed to remove session transcript {}: {e}",
                transcript.display()
            ))),
            _ => Ok(deleted),
        }
    }

    /// Delete a session row only when it never gained resumable content.
    ///
    /// Empty sessions have no messages, no title, and no child sessions. The
    /// check and delete run in one SQLite statement so a concurrently flushed
    /// message cannot be removed after the emptiness check. Exact empty JSON
    /// snapshot files are removed conservatively after the row delete.
    pub fn delete_session_if_empty(&self, session_id: &str) -> Result<bool, AgentError> {
        let session_id = session_id.trim();
        if session_id.is_empty() || !self.db_path.exists() {
            return Ok(false);
        }
        self.ensure_db()?;
        let conn = rusqlite::Connection::open(&self.db_path)
            .map_err(|e| AgentError::Io(format!("Failed to open sessions db: {e}")))?;

        let deleted = conn
            .execute(
                "DELETE FROM sessions
                 WHERE id = ?1
                   AND NULLIF(TRIM(title), '') IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM messages
                       WHERE messages.session_id = sessions.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM sessions child
                       WHERE child.parent_session_id = sessions.id
                   )",
                rusqlite::params![session_id],
            )
            .map_err(|e| AgentError::Io(format!("Failed to delete empty session row: {e}")))?
            > 0;

        if deleted {
            self.remove_empty_session_files(session_id)?;
        }
        Ok(deleted)
    }

    /// Opportunistic startup maintenance with interval gating via `state_meta`.
    ///
    /// Never propagates errors to callers.
    pub fn maybe_auto_prune_and_vacuum(
        &self,
        retention_days: u32,
        min_interval_hours: u32,
        vacuum_after_prune: bool,
    ) -> AutoMaintenanceResult {
        let mut result = AutoMaintenanceResult {
            skipped: false,
            pruned: 0,
            vacuumed: false,
            error: None,
        };

        let now = jiff::Timestamp::now().as_second() as f64;
        let min_seconds = (min_interval_hours as f64) * 3600.0;

        match self.get_meta("last_auto_prune") {
            Ok(Some(last_raw)) => {
                if let Ok(last_ts) = last_raw.parse::<f64>() {
                    if now - last_ts < min_seconds {
                        result.skipped = true;
                        return result;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                result.error = Some(e.to_string());
                return result;
            }
        }

        match self.prune_sessions(retention_days) {
            Ok(pruned) => {
                result.pruned = pruned;
                if vacuum_after_prune && pruned > 0 {
                    match self.vacuum() {
                        Ok(()) => result.vacuumed = true,
                        Err(e) => tracing::warn!("sessions.db VACUUM failed: {}", e),
                    }
                }
            }
            Err(e) => {
                result.error = Some(e.to_string());
                return result;
            }
        }

        if let Err(e) = self.set_meta("last_auto_prune", &now.to_string()) {
            result.error = Some(e.to_string());
            return result;
        }

        if let Err(e) = self.truncate_wal_checkpoint() {
            tracing::warn!("sessions.db WAL checkpoint failed: {}", e);
        }

        result
    }
}

include!("session_persistence/session_io.rs");

#[cfg(test)]
#[path = "session_persistence/empty_snapshot_tests.rs"]
mod empty_snapshot_tests;

#[cfg(test)]
#[path = "session_persistence/metadata_tests.rs"]
mod metadata_tests;

#[cfg(test)]
mod tests;
