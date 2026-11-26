//! The model picker's methods and a session's model switch, as upstream's tui_gateway answers
//! them (`methods_complete.py` for `model.options`, `model.save_key` and `model.disconnect`;
//! `methods_tools.py` for `reload.env`; `methods_config_set.py` and `model_switch.py` for
//! `config.set model`).
//!
//! Each acts on this gateway's own home: its `config.yaml`, its `.env` and this process's
//! environment, which serves only that home (another profile is served by its own process).
//! The picker lists what the configuration names and which keys are set; it fetches no live
//! model catalogue, prices or usage.

use std::path::Path;
use std::sync::Arc;

use hermes_config::{GatewayConfig, LlmProviderConfig};
use hermes_core::providers::{canonical_provider_id, provider_api_key_env_vars, KNOWN_PROVIDERS};
use serde_json::{json, Map, Value};

use super::{ready, Chat, Handler, Reply, RpcError, SESSION_NOT_FOUND};

/// Upstream's codes.
const REQUIRED: i64 = 4001;
const UNKNOWN: i64 = 4002;
const NOT_AN_API_KEY: i64 = 4003;
const NO_KEY_VARIABLE: i64 = 4004;
const NOTHING_SAVED: i64 = 4005;
const OPTIONS_FAILED: i64 = 5033;
const SAVE_FAILED: i64 = 5034;
const DISCONNECT_FAILED: i64 = 5035;
const RELOAD_FAILED: i64 = 5015;

/// Variables `reload.env` never takes from `.env`: they name this process's own launch.
const LAUNCH_ONLY: [&str; 2] = ["HERMES_HOME", crate::TOKEN_ENV];

/// This area's methods.
pub(super) const METHODS: &[(&str, Handler)] = &[
    ("model.options", |r| {
        let include_unconfigured = flag(&r.params, "include_unconfigured");
        blocking(r.chat, OPTIONS_FAILED, move |chat| {
            Ok(chat.model_options(include_unconfigured))
        })
    }),
    ("model.save_key", |r| {
        let (slug, key) = (text(&r.params, "slug"), text(&r.params, "api_key"));
        blocking(r.chat, SAVE_FAILED, move |chat| chat.save_key(&slug, &key))
    }),
    ("model.disconnect", |r| {
        let slug = text(&r.params, "slug");
        blocking(r.chat, DISCONNECT_FAILED, move |chat| {
            chat.disconnect_provider(&slug)
        })
    }),
    ("reload.env", |r| {
        blocking(r.chat, RELOAD_FAILED, |chat| {
            reload_env(&chat.home).map(|updated| json!({ "updated": updated }))
        })
    }),
    ("config.set", |r| ready(r.chat.config_set(&r.params))),
];

fn text(params: &Map<String, Value>, key: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn flag(params: &Map<String, Value>, key: &str) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Runs `work` on the blocking pool (it reads and writes files); a panic answers `code`.
fn blocking(
    chat: Arc<Chat>,
    code: i64,
    work: impl FnOnce(&Chat) -> Result<Value, RpcError> + Send + 'static,
) -> Reply {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || work(&chat))
            .await
            .unwrap_or_else(|e| Err((code, e.to_string())))
    })
}

/// A set, non-empty environment variable.
fn env_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.trim().is_empty())
}

/// The variables a known provider's key is read from, first to last; `None` for a provider
/// this gateway does not know.
fn key_vars(slug: &str) -> Option<&'static [&'static str]> {
    let configured = slug.trim().to_ascii_lowercase();
    let canonical = canonical_provider_id(&configured);
    KNOWN_PROVIDERS
        .contains(&canonical.as_str())
        .then(|| provider_api_key_env_vars(&configured, &canonical))
}

/// A configured provider's key variable and whether its key is set.
fn configured_key(provider: &LlmProviderConfig) -> (Option<String>, bool) {
    let env = provider
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let literal = provider.api_key.as_deref().map(str::trim).unwrap_or("");
    let from_literal = match literal.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
        Some(var) => env_set(var.trim()),
        None => !literal.is_empty(),
    };
    (
        env.map(str::to_string),
        from_literal || env.is_some_and(env_set),
    )
}

/// One picker row in upstream's `ModelOptionProvider` shape.
fn row(slug: &str, is_current: bool, models: Vec<String>, source: &str) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("slug".into(), slug.into());
    row.insert("name".into(), slug.into());
    row.insert("total_models".into(), models.len().into());
    row.insert("models".into(), models.into());
    row.insert("is_current".into(), is_current.into());
    row.insert("source".into(), source.into());
    row
}

/// The picker rows for `config`: each configured provider, then each known provider with a key
/// set, then (when asked) every other known provider with no models. The current provider is
/// listed even without a key, marked as not authenticated.
fn option_rows(config: &GatewayConfig, current: &str, include_unconfigured: bool) -> Vec<Value> {
    let current_canonical = canonical_provider_id(current);
    let is_current = |slug: &str| {
        slug.eq_ignore_ascii_case(current) || canonical_provider_id(slug) == current_canonical
    };
    let mut rows = Vec::new();
    let mut seen = Vec::new();
    let mut configured: Vec<_> = config.llm_providers.iter().collect();
    configured.sort_by_key(|(slug, _)| slug.to_ascii_lowercase());
    for (slug, provider) in configured {
        let mut models = provider.models.clone();
        if let Some(model) = provider.model.as_deref().filter(|m| !m.trim().is_empty()) {
            if !models.iter().any(|m| m == model) {
                models.insert(0, model.to_string());
            }
        }
        let (key_env, authenticated) = configured_key(provider);
        let mut row = row(slug, is_current(slug), models, "configured");
        row.insert("is_user_defined".into(), true.into());
        row.insert("auth_type".into(), "api_key".into());
        row.insert("authenticated".into(), authenticated.into());
        if let Some(key_env) = key_env {
            row.insert("key_env".into(), key_env.into());
        }
        rows.push(Value::Object(row));
        seen.push(canonical_provider_id(slug));
    }
    for &slug in KNOWN_PROVIDERS {
        if canonical_provider_id(slug) != slug || seen.iter().any(|s| s == slug) {
            continue;
        }
        if matches!(slug, "moa" | "custom") {
            continue;
        }
        let vars = provider_api_key_env_vars(slug, slug);
        let authenticated = vars.iter().any(|v| env_set(v));
        let current = is_current(slug);
        if !(authenticated || current || include_unconfigured) {
            continue;
        }
        let source = if authenticated || current {
            "env"
        } else {
            "canonical"
        };
        let mut row = row(slug, current, Vec::new(), source);
        if let Some(key_env) = vars.first() {
            row.insert("auth_type".into(), "api_key".into());
            row.insert("key_env".into(), (*key_env).into());
            row.insert("authenticated".into(), authenticated.into());
        }
        rows.push(Value::Object(row));
        seen.push(slug.to_string());
    }
    rows
}

/// Writes `key=value` into the `.env` at `path`, replacing the variable's lines, owner-only.
fn save_env(path: &Path, key: &str, value: &str) -> std::io::Result<()> {
    edit_env(path, key, Some(value)).map(|_| ())
}

/// Rewrites the `.env` at `path` without `key`, or with `key=value` once in place of its first
/// line; true when `key` was there. The file is created owner-only and kept so.
fn edit_env(path: &Path, key: &str, value: Option<&str>) -> std::io::Result<bool> {
    let original = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let mut lines = Vec::new();
    let mut found = false;
    for line in original.lines() {
        if hermes_config::dotenv_assignment(line).is_some_and(|(k, _)| k == key) {
            if !found {
                if let Some(value) = value {
                    lines.push(format!("{key}={value}"));
                }
            }
            found = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if value.is_none() && !found {
        return Ok(false);
    }
    if let (Some(value), false) = (value, found) {
        lines.push(format!("{key}={value}"));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    if !path.exists() {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)?;
    }
    #[cfg(unix)]
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    hermes_config::atomic_write_bytes(path, out.as_bytes())
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(found)
}

/// Re-reads `home`'s `.env` into this process's environment, as upstream's `reload_env`; the
/// number of variables changed.
fn reload_env(home: &Path) -> Result<usize, RpcError> {
    let path = home.join(".env");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err((RELOAD_FAILED, err.to_string())),
    };
    let mut updated = 0;
    for (key, value) in text.lines().filter_map(hermes_config::dotenv_assignment) {
        let value = unquote(value.trim());
        if LAUNCH_ONLY.contains(&key) || std::env::var(key).ok().as_deref() == Some(value) {
            continue;
        }
        // SAFETY: as `hermes_config::load_config`, which sets this process's variables from
        // the same file; this process serves only this home.
        unsafe { std::env::set_var(key, value) };
        updated += 1;
    }
    Ok(updated)
}

/// A `.env` value without its surrounding quotes, as the config loader reads it.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// A `config.set model` value: the model and the provider it names with `--provider`; `None`
/// for a switch flag this gateway does not honour (`--global`, `--once` and others).
fn parse_switch(value: &str) -> Option<(String, Option<String>)> {
    let mut words = value.split_whitespace();
    let mut model = Vec::new();
    let mut provider = None;
    while let Some(word) = words.next() {
        match word {
            "--provider" => provider = Some(words.next()?.to_string()),
            "--session" => {}
            flag if flag.starts_with("--") => return None,
            word => model.push(word),
        }
    }
    let model = model.join(" ");
    (!model.is_empty()).then_some((model, provider))
}

impl Chat {
    /// `model.options`: `{providers, model, provider}`.
    fn model_options(&self, include_unconfigured: bool) -> Value {
        let model = self.model().unwrap_or_default().to_string();
        let current = if model.is_empty() {
            String::new()
        } else {
            hermes_provider_runtime::provider_runtime_diagnostic(&self.config, &model)
                .requested_provider
        };
        let providers = option_rows(&self.config, &current, include_unconfigured);
        json!({"providers": providers, "model": model, "provider": current})
    }

    /// `model.save_key {slug, api_key}`: the key goes into this home's `.env` and this
    /// process's environment; answers the provider's refreshed row.
    fn save_key(&self, slug: &str, key: &str) -> Result<Value, RpcError> {
        if slug.is_empty() || key.is_empty() {
            return Err((REQUIRED, "slug and api_key are required".into()));
        }
        let vars = key_vars(slug).ok_or((UNKNOWN, format!("unknown provider: {slug}")))?;
        let Some(var) = vars.first() else {
            let capability = hermes_core::providers::provider_capability_for(slug);
            if capability.is_some_and(|c| c.oauth_supported) {
                return Err((
                    NOT_AN_API_KEY,
                    format!("{slug} signs in another way: run `hermes model` to configure"),
                ));
            }
            return Err((NO_KEY_VARIABLE, format!("no env var defined for {slug}")));
        };
        save_env(&self.home.join(".env"), var, key).map_err(|e| (SAVE_FAILED, e.to_string()))?;
        // SAFETY: as `reload_env`.
        unsafe { std::env::set_var(var, key) };
        let canonical = canonical_provider_id(slug);
        let mut row = option_rows(&self.config, "", false)
            .into_iter()
            .find(|row| row["slug"] == canonical.as_str())
            .unwrap_or_else(|| Value::Object(row(&canonical, false, Vec::new(), "env")));
        row["authenticated"] = true.into();
        Ok(json!({ "provider": row }))
    }

    /// `model.disconnect {slug}`: removes every key variable of the provider from this home's
    /// `.env` and this process's environment.
    fn disconnect_provider(&self, slug: &str) -> Result<Value, RpcError> {
        if slug.is_empty() {
            return Err((REQUIRED, "slug is required".into()));
        }
        let path = self.home.join(".env");
        let mut cleared = false;
        for var in key_vars(slug).unwrap_or_default() {
            let in_file =
                edit_env(&path, var, None).map_err(|e| (DISCONNECT_FAILED, e.to_string()))?;
            let in_env = std::env::var_os(var).is_some();
            // SAFETY: as `reload_env`.
            unsafe { std::env::remove_var(var) };
            cleared |= in_file || in_env;
        }
        if !cleared {
            return Err((NOTHING_SAVED, format!("no credentials found for {slug}")));
        }
        Ok(json!({"slug": slug, "name": slug, "disconnected": true}))
    }

    /// `config.set {session_id, key, value}`: only `model`, for one live session, as upstream
    /// switches a session's model (`methods_config_set.py` `_set_model`).
    fn config_set(&self, params: &Map<String, Value>) -> Result<Value, RpcError> {
        let key = text(params, "key");
        let value = text(params, "value");
        let session_id = text(params, "session_id");
        if key != "model" {
            return Err((UNKNOWN, format!("unknown config key: {key}")));
        }
        let Some(session) = self.lock_sessions().get(&session_id).cloned() else {
            return Err(if session_id.is_empty() {
                (
                    REQUIRED,
                    "config.set model requires a live session; to change the profile default \
                     use Settings -> Models"
                        .into(),
                )
            } else {
                (SESSION_NOT_FOUND, "session not found".into())
            });
        };
        if value.is_empty() {
            return Err((UNKNOWN, "model value required".into()));
        }
        let (model, provider) =
            parse_switch(&value).ok_or((UNKNOWN, format!("unsupported model switch: {value}")))?;
        session.set_model(Some(match &provider {
            Some(provider) => format!("{provider}:{model}"),
            None => model.clone(),
        }));
        Ok(
            json!({"key": "model", "value": model, "warning": "", "confirm_required": false,
                  "confirm_message": "", "scope": "session"}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_value_names_a_model_and_maybe_its_provider() {
        assert_eq!(parse_switch("gpt-5"), Some(("gpt-5".into(), None)));
        assert_eq!(
            parse_switch("example/a --provider nous --session"),
            Some(("example/a".into(), Some("nous".into())))
        );
        for refused in [
            "gpt-5 --global",
            "gpt-5 --once",
            "--provider",
            "gpt-5 --provider",
            "",
        ] {
            assert_eq!(parse_switch(refused), None, "{refused}");
        }
    }

    #[test]
    fn env_edits_replace_remove_and_keep_other_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "# note\nA=1\nB=2\nA=3\n").unwrap();
        save_env(&path, "A", "new").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# note\nA=new\nB=2\n"
        );
        assert!(edit_env(&path, "B", None).unwrap());
        assert!(!edit_env(&path, "B", None).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# note\nA=new\n");
        assert_eq!(unquote("'x'"), "x");
        assert_eq!(unquote("\"x\""), "x");
        assert_eq!(unquote("x'"), "x'");
    }
}
