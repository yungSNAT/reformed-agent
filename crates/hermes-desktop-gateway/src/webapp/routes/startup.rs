//! Routes the Webapp page calls whilst it starts that need no session or profile method: the
//! machine's UI preferences, the desktop plugins folder, the local model runtime (off), the
//! legacy session owner stamp (nothing to stamp) and the default working folder.
//!
//! Upstream at the pin: `hermes_cli/web_routers/ui_preferences.py` with `hermes_cli/ui_preferences.py`,
//! `web_routers/desktop_plugins.py`, `web_routers/local_models.py`, `web_routers/sessions.py`
//! (`owner-backfill`) and `web_routers/files.py` (`default-cwd`). The preferences and plugins routes
//! are the Webapp's own (upstream mounts them only on its surface), so other callers get 404.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::store::{
    blocking, home, install_root, not_found, webapp_only, write_private, StoreLock,
};
use crate::webapp::api::{served_profile, ApiError, ApiResult, Ctx};

pub fn router() -> Router {
    Router::new()
        .route(
            "/api/ui-preferences",
            get(get_ui_preferences).patch(patch_ui_preferences),
        )
        .route("/api/desktop-plugins/root", get(desktop_plugins_root))
        .route("/api/desktop-plugins/list", get(desktop_plugins_list))
        .route("/api/local-models/status", get(local_models_status))
        .route("/api/local-models/jobs", get(local_models_jobs))
        .route("/api/sessions/owner-backfill", post(owner_backfill))
        .route("/api/fs/default-cwd", get(default_cwd))
}

// ---- UI preferences -------------------------------------------------------------------------

/// The machine's preferences document, `<root>/ui-preferences.json`, shared with native Desktop.
pub(crate) const PREFERENCES_FILE: &str = "ui-preferences.json";
const KEY_PREFIXES: [&str; 3] = ["hermes.desktop.", "hermes-desktop-", "hermes-onboarding-"];
const MAX_KEY_CHARS: usize = 256;
const MAX_VALUE_CHARS: usize = 256 * 1024;
const MAX_KEYS: usize = 512;
const MAX_TOTAL_CHARS: usize = 2 * 1024 * 1024;

/// Writers in this process take turns before the cross-process lock, as upstream's thread lock.
static WRITERS: Mutex<()> = Mutex::new(());

#[derive(Debug)]
pub(crate) enum PrefsError {
    /// A write the store refuses: 400.
    Refused(String),
    /// The store cannot be read or its lock is held: 503, and the client retries.
    Busy(String),
}

impl From<PrefsError> for ApiError {
    fn from(err: PrefsError) -> Self {
        match err {
            PrefsError::Refused(detail) => ApiError(StatusCode::BAD_REQUEST, detail),
            PrefsError::Busy(detail) => ApiError(StatusCode::SERVICE_UNAVAILABLE, detail),
        }
    }
}

/// A read of the store: the snapshot upstream answers and whether the file was unparseable.
pub(crate) struct Snapshot {
    pub(crate) exists: bool,
    pub(crate) revision: i64,
    pub(crate) values: Map<String, Value>,
    pub(crate) generation: Option<String>,
    pub(crate) corrupt: bool,
}

impl Snapshot {
    fn missing(corrupt: bool) -> Self {
        Snapshot {
            exists: false,
            revision: 0,
            values: Map::new(),
            generation: None,
            corrupt,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut out =
            json!({"exists": self.exists, "revision": self.revision, "values": self.values});
        if let Some(generation) = &self.generation {
            out["generation"] = json!(generation);
        }
        out
    }
}

/// A missing or unparseable file reads as no store; any other read failure is `Busy`, since
/// merging over it would drop every key.
pub(crate) fn read_preferences(path: &Path) -> Result<Snapshot, PrefsError> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Snapshot::missing(false))
        }
        Err(_) => return Err(PrefsError::Busy("preferences store is unreadable".into())),
    };
    let raw = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&raw);
    let Ok(Value::Object(doc)) = serde_json::from_slice::<Value>(raw) else {
        return Ok(Snapshot::missing(true));
    };
    let values = match doc.get("values") {
        Some(Value::Object(values)) => values
            .iter()
            .filter(|(_, v)| v.is_string())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        _ => Map::new(),
    };
    Ok(Snapshot {
        exists: true,
        revision: doc.get("revision").and_then(Value::as_i64).unwrap_or(0),
        values,
        generation: Some(
            doc.get("generation")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
        corrupt: false,
    })
}

fn validate(changes: &Map<String, Value>) -> Result<(), PrefsError> {
    for (key, value) in changes {
        if !KEY_PREFIXES.iter().any(|p| key.starts_with(p)) || key.chars().count() > MAX_KEY_CHARS {
            return Err(PrefsError::Refused(format!(
                "unsupported preference key: '{key}'"
            )));
        }
        let ok = match value {
            Value::Null => true,
            Value::String(v) => v.chars().count() <= MAX_VALUE_CHARS,
            _ => false,
        };
        if !ok {
            return Err(PrefsError::Refused(format!("unsupported value for {key}")));
        }
    }
    Ok(())
}

/// Merges `changes` per key (`null` deletes) and returns the new snapshot. A `seed` write only
/// adds keys the store lacks, and creates no store from nothing.
pub(crate) fn write_preferences(
    path: &Path,
    changes: &Map<String, Value>,
    seed: bool,
) -> Result<Value, PrefsError> {
    validate(changes)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| PrefsError::Busy("preferences store is unreadable".into()))?;
    }
    let _turn = WRITERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let lock = StoreLock::take(path, "preferences store").map_err(PrefsError::Busy)?;
    let current = read_preferences(path)?;
    let mut values = current.values.clone();
    for (key, value) in changes {
        match value {
            Value::Null if !seed => {
                values.remove(key);
            }
            Value::Null => {}
            value if seed => {
                values.entry(key.clone()).or_insert_with(|| value.clone());
            }
            value => {
                values.insert(key.clone(), value.clone());
            }
        }
    }
    let total: usize = values
        .iter()
        .map(|(k, v)| k.chars().count() + v.as_str().map_or(0, |v| v.chars().count()))
        .sum();
    if values.len() > MAX_KEYS || total > MAX_TOTAL_CHARS {
        return Err(PrefsError::Refused("preferences store is full".into()));
    }
    if values == current.values && (current.exists || values.is_empty()) {
        return Ok(current.to_json());
    }
    let revision = current.revision + 1;
    let generation = current
        .generation
        .filter(|g| !g.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()[..16].to_owned());
    if !lock.held() {
        return Err(PrefsError::Busy(
            "preferences store lock was taken over".into(),
        ));
    }
    if current.corrupt {
        // Kept aside, never silently replaced: it may hold preferences worth recovering.
        let ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let mut aside = path.file_name().unwrap_or_default().to_os_string();
        aside.push(format!(".corrupt-{ms}"));
        let _ = std::fs::copy(path, path.with_file_name(aside));
    }
    let doc =
        json!({"version": 1, "generation": generation, "revision": revision, "values": values});
    write_private(path, &serde_json::to_vec(&doc).unwrap_or_default())
        .map_err(|_| PrefsError::Busy("preferences store is unwritable".into()))?;
    drop(lock);
    Ok(json!({"exists": true, "revision": revision, "values": values, "generation": generation}))
}

async fn get_ui_preferences(ctx: Ctx) -> ApiResult {
    webapp_only(&ctx)?;
    let path = install_root(&ctx).join(PREFERENCES_FILE);
    blocking(move || Ok(read_preferences(&path)?.to_json()))
        .await
        .map(Json)
}

#[derive(Deserialize)]
struct PreferencesPatch {
    #[serde(default)]
    values: HashMap<String, Option<String>>,
    #[serde(default)]
    seed: bool,
}

async fn patch_ui_preferences(ctx: Ctx, Json(body): Json<PreferencesPatch>) -> ApiResult {
    webapp_only(&ctx)?;
    let path = install_root(&ctx).join(PREFERENCES_FILE);
    let changes: Map<String, Value> = body
        .values
        .into_iter()
        .map(|(k, v)| (k, v.map_or(Value::Null, Value::String)))
        .collect();
    blocking(move || Ok(write_preferences(&path, &changes, body.seed)?))
        .await
        .map(Json)
}

// ---- Desktop plugins ------------------------------------------------------------------------

/// `<root>/desktop-plugins`, created on demand and resolved. Upstream first copies installed
/// packages' desktop halves into it; this build installs no such packages, so there is nothing to
/// copy and the folder holds what the user put there.
pub(crate) fn plugins_root(root: &Path) -> Result<PathBuf, ApiError> {
    let dir = root.join("desktop-plugins");
    std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::canonicalize(&dir))
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "desktop plugins folder is unavailable".into(),
            )
        })
}

/// The entries of the plugins root or of one plugin folder in it. The path is judged as written
/// (no `..`, at most one level below the root); a linked plugin folder is followed, as native
/// Desktop follows it.
pub(crate) fn list_plugins(root: &Path, raw: &str) -> Result<Value, ApiError> {
    let path_error = || not_found("Not a desktop plugin path");
    let parts: Vec<_> = Path::new(raw)
        .strip_prefix(root)
        .map_err(|_| path_error())?
        .components()
        .collect();
    if parts.len() > 2 || !parts.iter().all(|c| matches!(c, Component::Normal(_))) {
        return Err(path_error());
    }
    let target = parts.iter().fold(root.to_path_buf(), |p, c| p.join(c));
    if parts.len() > 1 || !target.is_dir() {
        return Err(not_found("Not a desktop plugin folder"));
    }
    let mut entries: Vec<(String, Value)> = std::fs::read_dir(&target)
        .map_err(|_| not_found("Not a desktop plugin folder"))?
        .filter_map(Result::ok)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = target.join(&name);
            let row = json!({
                "name": name,
                "path": path.to_string_lossy(),
                "isDirectory": path.is_dir(),
            });
            (name, row)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(json!({"entries": entries.into_iter().map(|(_, row)| row).collect::<Vec<_>>()}))
}

async fn desktop_plugins_root(ctx: Ctx) -> ApiResult {
    webapp_only(&ctx)?;
    let root = install_root(&ctx);
    blocking(move || Ok(json!({"root": plugins_root(&root)?.to_string_lossy()})))
        .await
        .map(Json)
}

async fn desktop_plugins_list(ctx: Ctx, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    webapp_only(&ctx)?;
    let Some(path) = q.get("path").cloned() else {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "path is required".into(),
        ));
    };
    let root = install_root(&ctx);
    blocking(move || list_plugins(&plugins_root(&root)?, &path))
        .await
        .map(Json)
}

// ---- Local model runtime: not in this build -------------------------------------------------

/// Upstream's status for a machine with the runtime off and nothing installed: the pane then
/// offers nothing to load.
async fn local_models_status(_ctx: Ctx) -> ApiResult {
    Ok(Json(json!({
        "enabled": false,
        "tag": null,
        "configured_tag": null,
        "update_available": false,
        "runtime_installed": false,
        "runtime_backend": null,
        "server_running": false,
        "server_base_url": null,
        "active_model_id": null,
        "loaded_models": {},
        "loading": {},
        "placement": {},
        "models": [],
        "models_dir": null,
    })))
}

async fn local_models_jobs(_ctx: Ctx) -> ApiResult {
    Ok(Json(json!({"jobs": []})))
}

// ---- Legacy session owners: nothing to stamp ------------------------------------------------

#[derive(Deserialize, Default)]
struct OwnerBackfill {
    #[serde(default)]
    profile: Option<String>,
}

/// Upstream stamps sessions saved before rows carried an owning profile. This build's sessions
/// table has no owner column, so no row is unowned and the stamp count is 0.
async fn owner_backfill(ctx: Ctx, body: Option<Json<OwnerBackfill>>) -> ApiResult {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    served_profile(&ctx.api, body.profile.as_deref())?;
    Ok(Json(
        json!({"ok": true, "stamped": 0, "profile": ctx.api.profile}),
    ))
}

// ---- Default working folder -----------------------------------------------------------------

/// The folder a new session starts in: `terminal.cwd` from the served home's config when it
/// names a folder, else `TERMINAL_CWD`, else the user's home folder.
pub(crate) fn default_cwd_for(home: &Path) -> PathBuf {
    let configured =
        hermes_config::loader::load_effective_config_yaml_value(&home.join("config.yaml"))
            .and_then(|config| {
                config
                    .get("terminal")
                    .and_then(|t| t.get("cwd"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
    let candidates = [configured, std::env::var("TERMINAL_CWD").ok()];
    for raw in candidates.into_iter().flatten() {
        let raw = raw.trim();
        if matches!(raw, "" | "." | "auto" | "cwd") {
            continue;
        }
        let candidate = expand_home(raw);
        let candidate = std::fs::canonicalize(&candidate).unwrap_or(candidate);
        if candidate.is_dir() {
            return candidate;
        }
    }
    dirs_home()
}

fn expand_home(raw: &str) -> PathBuf {
    match raw.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            dirs_home().join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(raw),
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The checked-out branch of `cwd`, or "" outside a repository, on a detached head, or when git
/// is missing, too old to read safely, or slow.
pub(crate) fn branch_of(cwd: &Path) -> String {
    let Some(mut git) = hermes_core::git::read_only_git(cwd) else {
        return String::new();
    };
    git.args(["branch", "--show-current"]);
    match hermes_core::git::output_within_timeout(&mut git, Duration::from_secs(2)) {
        Some(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        _ => String::new(),
    }
}

async fn default_cwd(ctx: Ctx, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    served_profile(&ctx.api, q.get("profile").map(String::as_str))?;
    let home = home(&ctx);
    blocking(move || {
        let cwd = default_cwd_for(&home);
        let branch = branch_of(&cwd);
        Ok(json!({"cwd": cwd.to_string_lossy(), "branch": branch}))
    })
    .await
    .map(Json)
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
