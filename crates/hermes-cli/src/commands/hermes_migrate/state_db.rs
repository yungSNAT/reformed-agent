//! Hermes `state.db` read as the records `hermes sessions export` writes.
//!
//! The database is opened read-only and `immutable`, which takes no lock and
//! creates or writes no sidecar file in the Hermes home. Unapplied journal or
//! WAL changes are refused, as only Hermes can apply them.
//! Errors name a session and a fixed reason, never a stored value.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use hermes_core::AgentError;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value};

use super::super::hermes_history::{safe_session_id, ExportSession, SessionImporter};

/// Session columns the export record carries, in order; `system_prompt` follows.
const SESSION_FIELDS: [&str; 10] = [
    "id",
    "started_at",
    "ended_at",
    "end_reason",
    "last_activity_at",
    "parent_session_id",
    "title",
    "model",
    "model_config",
    "source",
];
/// Message columns the conversion reads.
const MESSAGE_FIELDS: [&str; 8] = [
    "role",
    "content",
    "tool_call_id",
    "tool_calls",
    "tool_name",
    "reasoning_content",
    "timestamp",
    "active",
];
/// Hermes marks JSON-encoded message content with this prefix.
const CONTENT_JSON_PREFIX: &str = "\u{0}json:";

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A sidecar's state: absent, an empty or non-empty regular file, or something
/// else (a link or special file), which SQLite would follow and is refused.
enum Sidecar {
    Absent,
    Empty,
    Full,
    Other,
}

fn sidecar_state(path: &Path) -> Sidecar {
    match fs::symlink_metadata(path) {
        Err(_) => Sidecar::Absent,
        Ok(meta) if !meta.is_file() => Sidecar::Other,
        Ok(meta) if meta.len() == 0 => Sidecar::Empty,
        Ok(_) => Sidecar::Full,
    }
}

pub(super) fn open(path: &Path) -> Result<Connection, String> {
    const RERUN: &str = "start and stop Hermes once, then run this again";
    let (journal, wal, shm) = (
        sidecar_state(&sidecar(path, "-journal")),
        sidecar_state(&sidecar(path, "-wal")),
        sidecar_state(&sidecar(path, "-shm")),
    );
    if [&journal, &wal, &shm]
        .iter()
        .any(|state| matches!(state, Sidecar::Other))
    {
        return Err("a state.db sidecar file is a link or special file".into());
    }
    if matches!(journal, Sidecar::Full) {
        return Err(format!(
            "an unfinished write is in state.db-journal; {RERUN}"
        ));
    }
    // A read-only open of a WAL database still writes, and can create,
    // state.db-shm; only the immutable open leaves the Hermes home as it is.
    if matches!(wal, Sidecar::Full) {
        return Err(format!(
            "unapplied changes are in state.db-wal; stop Hermes if it runs, else {RERUN}"
        ));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    Connection::open_with_flags(immutable_uri(path)?, flags).map_err(|error| failure(&error))
}

fn failure(error: &rusqlite::Error) -> String {
    match error.sqlite_error_code() {
        Some(code) => format!("could not be read as a Hermes session database ({code:?})"),
        None => "could not be read as a Hermes session database".into(),
    }
}

/// `file:` URI with every byte outside the unreserved set percent-encoded.
fn immutable_uri(path: &Path) -> Result<String, String> {
    let text = path.to_str().ok_or("its path is not valid Unicode")?;
    #[cfg(windows)]
    let text = {
        let text = text.strip_prefix(r"\\?\").unwrap_or(text);
        let drive = text.as_bytes();
        if drive.len() < 3 || !drive[0].is_ascii_alphabetic() || &drive[1..3] != b":\\" {
            return Err("it is not on a local drive; copy the Hermes home to one first".into());
        }
        format!("/{}", text.replace('\\', "/"))
    };
    let mut uri = String::from("file:");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    Ok(uri)
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    names.collect()
}

fn value(value: ValueRef<'_>) -> Result<Value, &'static str> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(number) => Value::from(number),
        ValueRef::Real(number) => serde_json::Number::from_f64(number)
            .map(Value::Number)
            .ok_or("a number that is not finite")?,
        ValueRef::Text(text) => Value::String(
            std::str::from_utf8(text)
                .map_err(|_| "text that is not UTF-8")?
                .to_string(),
        ),
        ValueRef::Blob(_) => return Err("a binary value"),
    })
}

/// A message row as `_row_to_message_dict` gives it: JSON-encoded content
/// decoded (kept raw when it does not parse) and `tool_calls` parsed, `[]`
/// when invalid.
fn message(row: &rusqlite::Row<'_>, names: &[&str]) -> Result<Value, &'static str> {
    let mut fields = Map::new();
    for (index, name) in names.iter().enumerate() {
        let mut field = value(row.get_ref(index).map_err(|_| "an unreadable column")?)?;
        match (*name, &field) {
            ("content", Value::String(text)) => {
                if let Some(encoded) = text.strip_prefix(CONTENT_JSON_PREFIX) {
                    if let Ok(decoded) = serde_json::from_str(encoded) {
                        field = decoded;
                    }
                }
            }
            ("tool_calls", Value::String(text)) if !text.is_empty() => {
                field = serde_json::from_str(text).unwrap_or_else(|_| Value::Array(Vec::new()));
            }
            _ => {}
        }
        fields.insert((*name).to_string(), field);
    }
    Ok(Value::Object(fields))
}

/// Feeds every session to `importer` in one read transaction and returns the
/// number of message rows that belong to no session.
pub(super) fn read(
    conn: &Connection,
    importer: &mut SessionImporter<'_>,
) -> Result<usize, AgentError> {
    let unreadable = |error: rusqlite::Error| AgentError::Config(failure(&error));
    let tx = conn.unchecked_transaction().map_err(unreadable)?;
    let session_columns = columns(&tx, "sessions").map_err(unreadable)?;
    let message_columns = columns(&tx, "messages").map_err(unreadable)?;
    if !["id", "started_at"]
        .iter()
        .all(|name| session_columns.contains(*name))
        || !["session_id", "role", "timestamp"]
            .iter()
            .all(|name| message_columns.contains(*name))
    {
        return Err(AgentError::Config(
            "could not be read: it lacks the session or message columns Hermes writes".into(),
        ));
    }
    let prompts: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'system_prompts')",
            [],
            |row| row.get(0),
        )
        .map_err(unreadable)?;
    let session_field = |name: &str| {
        if session_columns.contains(name) {
            format!("s.{name}")
        } else {
            "NULL".into()
        }
    };
    let joined = prompts && session_columns.contains("system_prompt_hash");
    let prompt = match (joined, session_columns.contains("system_prompt")) {
        (true, true) => "COALESCE(sp.prompt, s.system_prompt)",
        (true, false) => "sp.prompt",
        (false, true) => "s.system_prompt",
        (false, false) => "NULL",
    };
    let sessions_sql = format!(
        "SELECT {}, {prompt} FROM sessions s{} ORDER BY s.id",
        SESSION_FIELDS.map(session_field).join(", "),
        if joined {
            " LEFT JOIN system_prompts sp ON sp.hash = s.system_prompt_hash"
        } else {
            ""
        }
    );
    let message_names: Vec<&str> = MESSAGE_FIELDS
        .into_iter()
        .filter(|name| message_columns.contains(*name))
        .collect();
    let messages_sql = format!(
        "SELECT {} FROM messages WHERE session_id = ?1 ORDER BY rowid",
        message_names.join(", ")
    );
    let mut sessions = tx.prepare(&sessions_sql).map_err(unreadable)?;
    let mut messages = tx.prepare(&messages_sql).map_err(unreadable)?;
    let mut rows = sessions.query([]).map_err(unreadable)?;
    let mut position = 0;
    while let Some(row) = rows.next().map_err(unreadable)? {
        position += 1;
        let id = match row.get_ref(0).map_err(unreadable)? {
            ValueRef::Text(id) => std::str::from_utf8(id).ok().map(str::to_string),
            _ => None,
        };
        let label = id
            .clone()
            .filter(|id| safe_session_id(id))
            .unwrap_or_else(|| format!("#{position}"));
        let invalid = |reason: &str| AgentError::Config(format!("session {label}: {reason}"));
        let id = id.ok_or_else(|| invalid("invalid session ID"))?;
        let mut record = Map::new();
        for (index, name) in SESSION_FIELDS
            .iter()
            .chain(["system_prompt"].iter())
            .enumerate()
        {
            let field = value(row.get_ref(index).map_err(unreadable)?).map_err(invalid)?;
            record.insert((*name).to_string(), field);
        }
        let mut transcript = Vec::new();
        let mut message_rows = messages.query([&id]).map_err(unreadable)?;
        while let Some(message_row) = message_rows.next().map_err(unreadable)? {
            transcript.push(message(message_row, &message_names).map_err(invalid)?);
        }
        record.insert("messages".into(), Value::Array(transcript));
        let exported: ExportSession = serde_json::from_value(Value::Object(record))
            .map_err(|_| invalid("invalid session record"))?;
        importer.add(exported, &invalid)?;
    }
    let orphans: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM messages m WHERE NOT EXISTS (SELECT 1 FROM sessions s WHERE s.id = m.session_id)",
            [],
            |row| row.get(0),
        )
        .map_err(unreadable)?;
    Ok(usize::try_from(orphans).unwrap_or_default())
}
