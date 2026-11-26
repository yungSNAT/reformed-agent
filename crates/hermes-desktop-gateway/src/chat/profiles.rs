//! The install's profiles as a remote Hermes Desktop lists them: `GET /api/profiles` (Electron's
//! connection roster and the Bots roster read it) and `profiles.list`, in upstream's row shapes
//! (hermes_cli/web_routers/profiles.py `_profile_to_dict`, tui_gateway/methods_profiles.py).
//!
//! Every value is read from files without loading a profile's configuration, which would put
//! its `.env` into this process's environment. Fields this gateway does not track carry
//! upstream's 'unknown' value: no messaging gateway liveness, distribution or shell alias, and
//! no session previews, so the Desktop creates a bot's chat itself.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use std::sync::Arc;

use super::{Chat, Handler, RpcError};

/// Files whose presence makes a folder under `profiles/` a profile (upstream
/// hermes_constants.py `_PROFILE_IDENTITY_MARKERS`).
const IDENTITY_MARKERS: [&str; 6] = [
    "config.yaml",
    ".env",
    "SOUL.md",
    "profile.yaml",
    "auth.json",
    "state.db",
];

/// Where a deleted profile leaves its tombstone, beside the live ones.
const TOMBSTONES: &str = ".deleted";

/// Folders a skill count never enters (upstream agent/skill_utils.py `EXCLUDED_SKILL_DIRS`, plus
/// every hidden folder) and the support folders of a skill.
const SKIPPED_DIRS: [&str; 4] = ["venv", "node_modules", "site-packages", "__pycache__"];
const SUPPORT_DIRS: [&str; 4] = ["references", "templates", "assets", "scripts"];

/// Bounds on one skill count, which runs on every roster poll.
const MAX_SKILL_DEPTH: usize = 8;
const MAX_SKILL_DIRS: usize = 10_000;

/// Largest `profile.yaml` read; a larger one is treated as absent.
const MAX_META_BYTES: u64 = 1 << 20;

const AVATAR_EXTENSIONS: [&str; 3] = ["png", "jpg", "webp"];

/// Upstream's code for a roster that could not be read.
const LIST_FAILED: i64 = 5061;

/// This area's methods.
pub(super) const METHODS: &[(&str, Handler)] = &[("profiles.list", |r| {
    Box::pin(off_thread(r.chat, LIST_FAILED, |chat| Ok(chat.profiles_list())))
})];

/// Runs `work`, which reads or writes files, on a blocking thread; `code` reports its panic.
pub(super) async fn off_thread(
    chat: Arc<Chat>,
    code: i64,
    work: impl FnOnce(&Chat) -> Result<Value, RpcError> + Send + 'static,
) -> Result<Value, RpcError> {
    tokio::task::spawn_blocking(move || work(&chat))
        .await
        .unwrap_or_else(|e| Err((code, e.to_string())))
}

/// Upstream's profile id grammar, `^[a-z0-9][a-z0-9_-]{0,63}$`: the name is a path component.
pub(crate) fn valid_profile_name(name: &str) -> bool {
    name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The install root: `home`, or its grandparent when `home` is `<root>/profiles/<name>` (as the
/// CLI's `--profile` resolves it).
pub(crate) fn install_root(home: &Path) -> PathBuf {
    match home.parent() {
        Some(parent) if parent.file_name() == Some("profiles".as_ref()) => {
            parent.parent().unwrap_or(parent).to_path_buf()
        }
        _ => home.to_path_buf(),
    }
}

/// The home of the named profile `name` when it is listed: a valid name other than `default`, a
/// real folder (not a link) under `<root>/profiles` with an identity marker and no tombstone.
pub(crate) fn named_profile_home(root: &Path, name: &str) -> Option<PathBuf> {
    if name == "default" || !valid_profile_name(name) {
        return None;
    }
    let profiles = root.join("profiles");
    let home = profiles.join(name);
    let is_dir = std::fs::symlink_metadata(&home).is_ok_and(|m| m.is_dir());
    let identified = IDENTITY_MARKERS.iter().any(|marker| {
        let path = home.join(marker);
        path.is_file() || path.is_symlink()
    });
    let deleted = profiles.join(TOMBSTONES).join(name).exists();
    (is_dir && identified && !deleted).then_some(home)
}

/// The default profile, then the named ones by name.
fn profile_homes(root: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut homes = Vec::new();
    if root.is_dir() {
        homes.push(("default".to_string(), root.to_path_buf(), true));
    }
    let mut named: Vec<(String, PathBuf, bool)> = std::fs::read_dir(root.join("profiles"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let home = named_profile_home(root, &name)?;
            Some((name, home, false))
        })
        .collect();
    named.sort_by(|a, b| a.0.cmp(&b.0));
    homes.extend(named);
    homes
}

/// `model` and `provider` from a profile's `config.yaml` (upstream `_read_config_model`): a
/// string `model` is the model; a mapping gives `default` (else `model`) and `provider`.
fn config_model(home: &Path) -> (Value, Value) {
    let path = home.join("config.yaml");
    if !path.is_file() {
        return (Value::Null, Value::Null);
    }
    let config = hermes_config::load_effective_config_yaml_value(&path).unwrap_or_default();
    let text = |value: Option<&Value>| match value {
        Some(Value::String(s)) if !s.is_empty() => Value::String(s.clone()),
        _ => Value::Null,
    };
    match config.get("model") {
        Some(Value::String(model)) => (Value::String(model.clone()), Value::Null),
        Some(Value::Object(model)) => {
            let name = match text(model.get("default")) {
                Value::Null => text(model.get("model")),
                name => name,
            };
            (name, text(model.get("provider")))
        }
        _ => (Value::Null, Value::Null),
    }
}

/// What `profile.yaml` says about a profile (upstream `read_profile_meta`); empty values when
/// it is missing, unreadable or too large.
struct Meta {
    description: String,
    description_auto: bool,
    display_name: String,
    bot_title: String,
    ui_meta: Value,
    previous_names: Vec<String>,
}

fn profile_meta(home: &Path) -> Meta {
    let path = home.join("profile.yaml");
    let data: Map<String, Value> = std::fs::metadata(&path)
        .ok()
        .filter(|m| m.is_file() && m.len() <= MAX_META_BYTES)
        .and_then(|_| std::fs::read_to_string(&path).ok())
        .and_then(|text| serde_yaml::from_str::<Value>(&text).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    let text = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let bots = data
        .get("ui_meta")
        .and_then(|meta| meta.get("hermes-bots"))
        .and_then(Value::as_object)
        .filter(|bots| !bots.is_empty());
    let mut previous_names: Vec<String> = Vec::new();
    for name in data
        .get("previous_names")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
    {
        if !name.is_empty() && !previous_names.iter().any(|seen| seen == name) {
            previous_names.push(name.to_string());
        }
    }
    Meta {
        description: text("description"),
        description_auto: data.get("description_auto").and_then(Value::as_bool) == Some(true),
        display_name: text("display_name"),
        bot_title: bots
            .and_then(|bots| bots.get("title"))
            .and_then(Value::as_str)
            .map(|title| title.trim().to_string())
            .unwrap_or_default(),
        ui_meta: bots.map_or_else(|| json!({}), |bots| json!({"hermes-bots": bots})),
        previous_names,
    }
}

/// Skills installed in a profile: folders under `skills/` holding a `SKILL.md`, not entering
/// hidden, dependency or cache folders, a skill's support folders or links, and stopping at
/// [`MAX_SKILL_DEPTH`] levels or [`MAX_SKILL_DIRS`] folders.
fn skill_count(home: &Path) -> usize {
    let mut count = 0;
    let mut visited = 0;
    let mut pending = vec![(home.join("skills"), 0)];
    while let Some((dir, depth)) = pending.pop() {
        visited += 1;
        if visited > MAX_SKILL_DIRS {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let entries: Vec<_> = entries.flatten().collect();
        let is_skill = entries
            .iter()
            .any(|e| e.file_name() == "SKILL.md" && e.file_type().is_ok_and(|t| t.is_file()));
        count += usize::from(is_skill);
        if depth == MAX_SKILL_DEPTH {
            continue;
        }
        for entry in entries {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let skipped = name.starts_with('.')
                || SKIPPED_DIRS.contains(&name.as_str())
                || (is_skill && SUPPORT_DIRS.contains(&name.as_str()));
            if !skipped && entry.file_type().is_ok_and(|t| t.is_dir()) {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    count
}

fn has_avatar(home: &Path) -> bool {
    AVATAR_EXTENSIONS
        .iter()
        .any(|ext| home.join("assets").join(format!("avatar.{ext}")).is_file())
}

/// The install's id when one has been recorded (`<root>/install_id`, upstream
/// hermes_cli/install_identity.py); empty otherwise. Never written here.
fn install_id(root: &Path) -> String {
    std::fs::read_to_string(root.join("install_id"))
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| id.len() <= 128 && id.chars().all(|c| c.is_ascii_graphic()))
        .unwrap_or_default()
}

/// One listed profile with what both listings share.
struct Row {
    name: String,
    home: PathBuf,
    is_default: bool,
    model: Value,
    provider: Value,
    skill_count: usize,
    meta: Meta,
}

fn rows(root: &Path) -> impl Iterator<Item = Row> {
    profile_homes(root).into_iter().map(|(name, home, is_default)| {
        let (model, provider) = config_model(&home);
        Row {
            skill_count: skill_count(&home),
            meta: profile_meta(&home),
            name,
            home,
            is_default,
            model,
            provider,
        }
    })
}

impl Chat {
    /// `GET /api/profiles`: `{profiles}` in upstream's `_profile_to_dict` shape.
    pub(crate) fn profiles_route(&self) -> Value {
        let rows: Vec<Value> = rows(&install_root(&self.home))
            .map(|row| {
                json!({
                    "name": row.name,
                    "path": row.home.display().to_string(),
                    "is_default": row.is_default,
                    "model": row.model,
                    "provider": row.provider,
                    "has_env": row.home.join(".env").exists(),
                    "skill_count": row.skill_count,
                    "gateway_running": false,
                    "description": row.meta.description,
                    "description_auto": row.meta.description_auto,
                    "display_name": row.meta.display_name,
                    "bot_title": row.meta.bot_title,
                    "ui_meta": row.meta.ui_meta,
                    "distribution_name": null,
                    "distribution_version": null,
                    "distribution_source": null,
                    "has_alias": false,
                })
            })
            .collect();
        json!({ "profiles": rows })
    }

    /// `profiles.list`: rows in upstream's `ProfileRow` shape without session previews
    /// (`include_sessions` is accepted and has nothing to add), and `bot_mode_protocol: false`,
    /// as this backend does not add the teammate protocol to a session's prompt.
    fn profiles_list(&self) -> Value {
        let root = install_root(&self.home);
        let rows: Vec<Value> = rows(&root)
            .map(|row| {
                json!({
                    "name": row.name,
                    "path": row.home.display().to_string(),
                    "is_default": row.is_default,
                    "model": row.model,
                    "provider": row.provider,
                    "description": row.meta.description,
                    "display_name": row.meta.display_name,
                    "skill_count": row.skill_count,
                    "previous_names": row.meta.previous_names,
                    "ui_meta": row.meta.ui_meta,
                    "has_avatar": has_avatar(&row.home),
                })
            })
            .collect();
        json!({
            "profiles": rows,
            "bot_mode_protocol": false,
            "install_id": install_id(&root),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names_follow_upstreams_grammar() {
        for good in ["a", "inbox", "0-bot_2", &"a".repeat(64)] {
            assert!(valid_profile_name(good), "{good}");
        }
        for bad in ["", "-a", "_a", "A", "a.b", "a/b", "..", &"a".repeat(65), "é"] {
            assert!(!valid_profile_name(bad), "{bad}");
        }
    }

    #[test]
    fn install_root_climbs_out_of_a_profile_home() {
        let root = Path::new("/r");
        assert_eq!(install_root(root), root);
        assert_eq!(install_root(&root.join("profiles").join("x")), root);
        assert_eq!(install_root(&root.join("other").join("x")), root.join("other/x"));
    }

    #[test]
    fn skill_count_stops_at_its_depth_bound() {
        let home = tempfile::tempdir().unwrap();
        let mut deep = home.path().join("skills");
        for level in 0..=MAX_SKILL_DEPTH + 1 {
            deep = deep.join(format!("d{level}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("SKILL.md"), "").unwrap();
        std::fs::write(home.path().join("skills").join("SKILL.md"), "").unwrap();
        assert_eq!(skill_count(home.path()), 1);
    }
}
