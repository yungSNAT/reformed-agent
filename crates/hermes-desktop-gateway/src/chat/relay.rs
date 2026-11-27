//! `bot_relay.roster.sync`: the Desktop pushes the agents it reaches on its other connections, and
//! the gateway keeps them as upstream does (tui_gateway/methods_bot_relay.py, tools/bot_relay.py):
//! each row checked and trimmed, one per (connection, profile), in `<root>/bot_relay/roster.json`
//! with owner-only access.
//!
//! Nothing in this runtime reads the roster yet: desktop turns run no tools, so no
//! `message_agent` call resolves a name through it. It is kept in upstream's place and shape for
//! that reader.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Map, Value};

use super::profiles::{install_root, off_thread};
use super::{Chat, Handler, RpcError};

/// Upstream's code for a roster that could not be written.
const ROSTER_WRITE_FAILED: i64 = 5090;

/// This area's methods.
pub(super) const METHODS: &[(&str, Handler)] = &[("bot_relay.roster.sync", |r| {
    let agents = r.params.get("agents").cloned();
    Box::pin(off_thread(r.chat, ROSTER_WRITE_FAILED, move |chat| {
        chat.roster_sync(agents.as_ref())
    }))
})];

/// Upstream's `_HANDLE_RE`, `^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$`, for handles, profiles and
/// connection ids.
fn valid_handle(text: &str) -> bool {
    text.len() <= 64
        && text.starts_with(|c: char| c.is_ascii_alphanumeric())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The first `max` characters of `text`.
fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// A field as upstream's `str(value or "")` reads it: a string as it is, a number as its digits,
/// `true` as `True`; zero, `false`, null, lists and objects as empty (upstream would print a
/// non-empty list or object, which no checked field accepts).
fn py_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Number(n)) if n.as_f64() != Some(0.0) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(u)) => u.to_string(),
            _ => n.to_string(),
        },
        _ => String::new(),
    }
}

/// A checked roster row (upstream `_normalize_roster_row`), or `None` to drop it.
fn normalize(row: &Value) -> Option<Map<String, Value>> {
    let row = row.as_object()?;
    let fields: Map<String, Value> = [
        "profile",
        "handle",
        "connection_id",
        "connection_label",
        "title",
        "description",
    ]
    .into_iter()
    .map(|key| (key.to_string(), Value::String(py_text(row.get(key)))))
    .collect();
    let text = |key: &str| fields[key].as_str().unwrap_or("").trim();
    let profile = text("profile");
    let connection_id = text("connection_id");
    let handle = match text("handle").trim_start_matches('@') {
        "" if profile == "default" => "hermes",
        "" => profile,
        handle => handle,
    };
    if profile.is_empty()
        || connection_id.is_empty()
        || ![handle, profile, connection_id]
            .into_iter()
            .all(valid_handle)
    {
        return None;
    }
    let description = text("description")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = Map::new();
    out.insert("profile".into(), profile.into());
    out.insert("handle".into(), handle.into());
    out.insert("connection_id".into(), connection_id.into());
    out.insert(
        "connection_label".into(),
        clip(text("connection_label"), 80).into(),
    );
    out.insert("title".into(), clip(text("title"), 120).into());
    out.insert("description".into(), clip(&description, 160).into());
    if let Some(online) = row.get("online").and_then(Value::as_bool) {
        out.insert("online".into(), online.into());
    }
    Some(out)
}

/// Writes `agents` as the roster under `root` and returns how many rows were kept.
fn write_roster(root: &Path, agents: Option<&Value>) -> Result<usize, String> {
    let mut by_key: BTreeMap<(String, String), Map<String, Value>> = BTreeMap::new();
    for row in agents
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(normalize)
    {
        let key = (
            row["connection_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            row["profile"].as_str().unwrap_or_default().to_string(),
        );
        by_key.entry(key).or_insert(row);
    }
    let count = by_key.len();
    let updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let roster =
        json!({"agents": by_key.into_values().collect::<Vec<_>>(), "updated_at": updated_at});
    let dir = root.join("bot_relay");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("roster.json");
    // A link, pipe or other non-file in the roster's place is replaced, never followed or
    // opened.
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file() && !m.is_dir()) {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    // Owner-only on every write, as upstream writes it, before the content goes in: the atomic
    // write keeps the mode it finds.
    create_private(&path).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(&roster).map_err(|e| e.to_string())?;
    // A link or non-file swapped in since is refused, never followed.
    hermes_config::atomic_write_bytes_no_follow(&path, &bytes, None).map_err(|e| e.to_string())?;
    Ok(count)
}

/// Makes `path` owner-only, creating it empty if absent. An existing one is opened without
/// following a link (nor waiting on a pipe) and its mode set on that descriptor, so a link made
/// in its place since the check above is refused, never followed.
fn create_private(path: &Path) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options.open(path) {
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(path)?
                    .set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        }
        other => other.map(drop),
    }
}

impl Chat {
    /// `bot_relay.roster.sync {agents}`: replaces the roster, answering `{count}`.
    fn roster_sync(&self, agents: Option<&Value>) -> Result<Value, RpcError> {
        write_roster(&install_root(&self.home), agents)
            .map(|count| json!({ "count": count }))
            .map_err(|err| (ROSTER_WRITE_FAILED, err))
    }
}
