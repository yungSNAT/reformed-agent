use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError};

use serde_json::Value;

/// The tool parameter through which the agent loop names its session to the
/// file tools, which keep per session what each file's reader has seen.
pub const SESSION_PARAM: &str = "_hermes_session";

/// The tool parameter carrying the agent loop's tool call id, so a
/// `read_file` is settled by [`settle_view`] with what the model was given.
pub const CALL_PARAM: &str = "_hermes_call";

/// Names the agent loop's session and call to the file tools, replacing any
/// value the model sent; without a session the tools use their shared
/// default. Other tools' arguments are left as they are.
pub fn scope_file_tool_params(
    tool_name: &str,
    params: &mut Value,
    session_id: Option<&str>,
    call_id: &str,
) {
    if !matches!(tool_name, "read_file" | "write_file" | "patch") {
        return;
    }
    let Some(args) = params.as_object_mut() else {
        return;
    };
    match session_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => args.insert(SESSION_PARAM.to_string(), Value::from(id)),
        None => args.remove(SESSION_PARAM),
    };
    args.insert(CALL_PARAM.to_string(), Value::from(call_id));
}

/// Removes the parameters only the agent loop may set, for callers that pass
/// a client's arguments straight to the tools.
pub fn strip_reserved_params(params: &mut Value) {
    if let Some(args) = params.as_object_mut() {
        args.remove(SESSION_PARAM);
        args.remove(CALL_PARAM);
    }
}

type ViewSettler = fn(session_id: Option<&str>, call_id: &str, shown: &str);

static VIEW_SETTLER: OnceLock<ViewSettler> = OnceLock::new();

/// Installs the file tools' settler for [`settle_view`]; the first one stays.
pub fn set_view_settler(settler: ViewSettler) {
    let _ = VIEW_SETTLER.set(settler);
}

/// Hands the file tools a tool result as the model will see it, after the
/// agent loop's own cuts, so a read counts only if the model was given it
/// whole. `session_id` is the one the agent loop gave
/// [`scope_file_tool_params`] for the call.
pub fn settle_view(session_id: Option<&str>, call_id: &str, shown: &str) {
    if let Some(settler) = VIEW_SETTLER.get() {
        settler(session_id, call_id, shown);
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_tools_carry_the_runs_session_and_call_not_the_models() {
        let mut params = json!({ "path": "a.txt" });
        params[SESSION_PARAM] = json!("forged");
        params[CALL_PARAM] = json!("forged");
        scope_file_tool_params("write_file", &mut params, Some("s1"), "call-1");
        assert_eq!(params[SESSION_PARAM], "s1");
        assert_eq!(params[CALL_PARAM], "call-1");

        scope_file_tool_params("read_file", &mut params, None, "call-2");
        assert!(params.get(SESSION_PARAM).is_none(), "{params}");
        assert_eq!(params[CALL_PARAM], "call-2");

        let mut other = json!({ "command": "ls" });
        other[SESSION_PARAM] = json!("kept");
        scope_file_tool_params("terminal", &mut other, Some("s1"), "call-3");
        assert_eq!(other[SESSION_PARAM], "kept");
        assert!(other.get(CALL_PARAM).is_none(), "{other}");

        strip_reserved_params(&mut params);
        assert_eq!(params, json!({ "path": "a.txt" }));
    }
}
