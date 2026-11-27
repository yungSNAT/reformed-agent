//! The model picker's methods and a session's model switch, as upstream's tui_gateway answers
//! them (`methods_complete.py` for `model.options`, `model.save_key` and `model.disconnect`;
//! `methods_tools.py` for `reload.env`; `methods_config_set.py` and `model_switch.py` for
//! `config.set model`).
//!
//! Each acts on this gateway's own home: its `config.yaml` and its `.env`, read and written
//! through `hermes_config`'s `.env` code (another profile is served by its own process). A key
//! changed after start-up is kept in [`Keys`], never set in the process environment, which other
//! threads and C libraries read whilst turns run. The key methods serve only clients on this
//! machine. The picker lists what the configuration names and which keys are set; it fetches
//! no live model catalogue, prices or usage.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use hermes_config::loader::{
    dotenv_assignment, dotenv_values_in, env_name_key, env_name_writable, env_value_round_trips,
    set_env_file_value,
};
use hermes_config::{GatewayConfig, LlmProviderConfig};
use hermes_core::providers::{canonical_provider_id, provider_api_key_env_vars, KNOWN_PROVIDERS};
use serde_json::{json, Map, Value};

use super::{ready, Chat, Handler, Origin, Reply, Request, RpcError, SESSION_NOT_FOUND};

/// Upstream's codes.
const REQUIRED: i64 = 4001;
const UNKNOWN: i64 = 4002;
const CONFIGURED_IN_FILE: i64 = 4003;
const NO_KEY_VARIABLE: i64 = 4004;
const NOTHING_SAVED: i64 = 4005;
const OPTIONS_FAILED: i64 = 5033;
const SAVE_FAILED: i64 = 5034;
const DISCONNECT_FAILED: i64 = 5035;
const RELOAD_FAILED: i64 = 5015;
/// A key method asked by a client that is not on this machine.
const LOCAL_ONLY: i64 = 4031;

/// This area's methods.
pub(super) const METHODS: &[(&str, Handler)] = &[
    ("model.options", |r| {
        let include_unconfigured = flag(&r.params, "include_unconfigured");
        local(r, OPTIONS_FAILED, move |chat| {
            Ok(chat.model_options(include_unconfigured))
        })
    }),
    ("model.save_key", |r| {
        let (slug, key) = (text(&r.params, "slug"), text(&r.params, "api_key"));
        local(r, SAVE_FAILED, move |chat| chat.save_key(&slug, &key))
    }),
    ("model.disconnect", |r| {
        let slug = text(&r.params, "slug");
        local(r, DISCONNECT_FAILED, move |chat| {
            chat.disconnect_provider(&slug)
        })
    }),
    ("reload.env", |r| {
        local(r, RELOAD_FAILED, |chat| {
            Ok(json!({ "updated": chat.reload_env() }))
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

/// Runs `work` on the blocking pool (it reads and writes files) for a client on this machine;
/// any other is refused. A panic answers `code`.
fn local(
    r: Request,
    code: i64,
    work: impl FnOnce(&Chat) -> Result<Value, RpcError> + Send + 'static,
) -> Reply {
    if r.conn.origin != Origin::Local {
        return ready(Err((
            LOCAL_ONLY,
            "model keys are managed only from this machine".into(),
        )));
    }
    Box::pin(super::profiles::off_thread(r.chat, code, work))
}

/// Key variables changed since start-up by `model.save_key`, `model.disconnect` or
/// `reload.env`; `None` for a removed one. A turn and the picker read these before the process
/// environment, which only start-up sets.
#[derive(Default)]
pub(super) struct Keys {
    changed: Mutex<HashMap<String, Option<String>>>,
    /// The names this gateway has seen in the home `.env` (at start-up, on a save or a reload),
    /// as [`env_name_key`] gives them. Not the project `.env`: a reload reads that one only to
    /// fill gaps, so a key it holds may be missing from what a reload reads.
    from_file: Mutex<BTreeSet<String>>,
}

/// The names in `home`'s `.env` as [`env_name_key`] gives them: none for a missing file,
/// `None` for one that cannot be read.
fn home_env_names(home: &Path) -> Option<BTreeSet<String>> {
    match std::fs::read_to_string(home.join(".env")) {
        Ok(contents) => Some(
            contents
                .lines()
                .filter_map(dotenv_assignment)
                .map(|(name, _)| env_name_key(name))
                .collect(),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(BTreeSet::new()),
        Err(_) => None,
    }
}

impl Keys {
    /// No changes yet; the names in `home`'s `.env` as it stands.
    pub(super) fn at_start_up(home: &Path) -> Self {
        Self {
            changed: Mutex::default(),
            from_file: Mutex::new(home_env_names(home).unwrap_or_default()),
        }
    }

    fn names_from_file(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.from_file.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records `names` as seen in the home `.env`.
    fn saw_in_home_file(&self, names: impl IntoIterator<Item = String>) {
        self.names_from_file().extend(names);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Option<String>>> {
        // A map updated one entry at a time: a poisoned lock is still safe to use.
        self.changed.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether this gateway has seen `var` in the home `.env`.
    fn came_from_home_file(&self, var: &str) -> bool {
        self.names_from_file().contains(&env_name_key(var))
    }

    /// `var`'s value now, if set and not blank: a change since start-up, else the environment's.
    fn value(&self, var: &str) -> Option<String> {
        let value = match self.lock().get(var) {
            Some(changed) => changed.clone(),
            None => std::env::var(var).ok(),
        };
        value.filter(|v| !v.trim().is_empty())
    }

    /// Whether `var` was changed since start-up.
    fn changed(&self, var: &str) -> bool {
        self.lock().contains_key(var)
    }

    /// Sets `var` to `value` (`None` removes it); whether its value changed.
    fn set(&self, var: &str, value: Option<String>) -> bool {
        let value = value.filter(|v| !v.trim().is_empty());
        let changed = self.value(var) != value;
        self.lock().insert(var.to_string(), value);
        changed
    }
}

/// Held across each `.env` change in this process, so 2 key methods at once cannot lose one's
/// change.
fn env_write() -> MutexGuard<'static, ()> {
    static WRITE: Mutex<()> = Mutex::new(());
    WRITE.lock().unwrap_or_else(|e| e.into_inner())
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

/// The variable a provider's `config.yaml` key comes from, as start-up leaves it (it expands
/// `${VAR}`): the text of a whole `${...}` it could not expand, untrimmed as the provider build
/// looks it up (it may not be a variable name), or the start-up value of one of `vars`. `None`
/// for a key written out.
fn literal_source<'a>(literal: &'a str, vars: &[&'a str]) -> Option<&'a str> {
    if let Some(var) = literal.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
        return Some(var);
    }
    vars.iter()
        .copied()
        .find(|var| std::env::var(var).is_ok_and(|value| value.trim() == literal))
}

/// A configured provider's key variable and whether its key is set.
fn configured_key(slug: &str, provider: &LlmProviderConfig, keys: &Keys) -> (Option<String>, bool) {
    let env = provider
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let literal = provider.api_key.as_deref().map(str::trim).unwrap_or("");
    let vars: Vec<&str> = env
        .into_iter()
        .chain(key_vars(slug).unwrap_or_default().iter().copied())
        .collect();
    let from_literal = !literal.is_empty()
        && literal_source(literal, &vars).is_none_or(|var| keys.value(var).is_some());
    (
        env.map(str::to_string),
        from_literal || env.is_some_and(|var| keys.value(var).is_some()),
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
fn option_rows(
    config: &GatewayConfig,
    keys: &Keys,
    current: &str,
    include_unconfigured: bool,
) -> Vec<Value> {
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
        let (key_env, authenticated) = configured_key(slug, provider, keys);
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
        let authenticated = vars.iter().any(|v| keys.value(v).is_some());
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
    /// The variables a key is read from: each known provider's, each configured `key_env` and
    /// each `${VAR}` a configured `api_key` still holds.
    fn key_variables(&self) -> Vec<String> {
        let mut vars: Vec<String> = KNOWN_PROVIDERS
            .iter()
            .flat_map(|slug| provider_api_key_env_vars(slug, &canonical_provider_id(slug)))
            .map(|var| (*var).to_string())
            .chain(
                self.config
                    .llm_providers
                    .values()
                    .filter_map(|provider| provider.api_key_env.as_deref())
                    .map(|var| var.trim().to_string())
                    .filter(|var| !var.is_empty()),
            )
            .chain(self.config.llm_providers.values().filter_map(|provider| {
                let literal = provider.api_key.as_deref()?.trim();
                literal_source(literal, &[]).map(str::to_string)
            }))
            .collect();
        vars.sort();
        vars.dedup();
        vars
    }

    /// The configuration a turn on `model` runs with: the gateway's, with the provider's key
    /// from a change since start-up; an error when that change removed the key it had.
    pub(super) fn turn_config(&self, model: &str) -> Result<Cow<'_, GatewayConfig>, String> {
        let (provider, _) =
            hermes_provider_runtime::resolve_provider_and_model(&self.config, model);
        let entry = self
            .config
            .llm_providers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&provider));
        let mut vars: Vec<&str> = entry
            .and_then(|(_, c)| c.api_key_env.as_deref())
            .map(str::trim)
            .filter(|var| !var.is_empty())
            .into_iter()
            .chain(key_vars(&provider).unwrap_or_default().iter().copied())
            .collect();
        // A key in `config.yaml` that start-up took from a variable follows that variable;
        // one written out comes first, as the provider build reads it.
        let literal = entry
            .and_then(|(_, c)| c.api_key.as_deref())
            .map(str::trim)
            .unwrap_or("");
        if !literal.is_empty() {
            match literal_source(literal, &vars) {
                Some(var) => vars = vec![var],
                None => return Ok(Cow::Borrowed(&self.config)),
            }
        }
        if !vars.iter().any(|var| self.keys.changed(var)) {
            return Ok(Cow::Borrowed(&self.config));
        }
        let Some(key) = vars.iter().find_map(|var| self.keys.value(var)) else {
            if vars.iter().any(|var| std::env::var_os(var).is_some()) {
                return Err(format!(
                    "No API key for provider '{provider}': it was disconnected."
                ));
            }
            return Ok(Cow::Borrowed(&self.config));
        };
        let mut config = self.config.clone();
        let name = entry.map_or_else(|| provider.clone(), |(name, _)| name.clone());
        let slot = config.llm_providers.entry(name).or_insert_with(|| {
            serde_json::from_value(json!({})).expect("every provider field has a default")
        });
        slot.api_key = Some(key);
        Ok(Cow::Owned(config))
    }

    /// `model.options`: `{providers, model, provider}`.
    fn model_options(&self, include_unconfigured: bool) -> Value {
        // The configured model may carry its provider (`provider:model`); the picker shows them
        // apart, as upstream does.
        let (current, model) = match self.model() {
            Some(model) => hermes_provider_runtime::resolve_provider_and_model(&self.config, model),
            None => (String::new(), String::new()),
        };
        let providers = option_rows(&self.config, &self.keys, &current, include_unconfigured);
        json!({"providers": providers, "model": model, "provider": current})
    }

    /// The `config.yaml` entries for `slug`: the one named as the slug first, then by name.
    fn slug_entries(&self, slug: &str) -> Vec<(&String, &LlmProviderConfig)> {
        let slug = slug.trim();
        let canonical = canonical_provider_id(&slug.to_ascii_lowercase());
        let mut entries: Vec<_> = self
            .config
            .llm_providers
            .iter()
            .filter(|(name, _)| {
                name.eq_ignore_ascii_case(slug) || name.eq_ignore_ascii_case(&canonical)
            })
            .collect();
        entries.sort_by_key(|(name, _)| (!name.eq_ignore_ascii_case(slug), name.as_str()));
        entries
    }

    /// A known provider's key variables, first to last: the `${VAR}` its configured `api_key`
    /// still holds, the `key_env` it is configured with, then its own; `None` for a provider
    /// this gateway does not know.
    fn slug_key_vars(&self, slug: &str) -> Option<Vec<&str>> {
        let known = key_vars(slug)?;
        let configured = self.slug_entries(slug).into_iter().flat_map(|(_, c)| {
            let reference = c
                .api_key
                .as_deref()
                .and_then(|literal| literal_source(literal.trim(), &[]));
            // An empty `${}` stays, to be refused as no variable name.
            reference.into_iter().chain(
                c.api_key_env
                    .as_deref()
                    .map(str::trim)
                    .filter(|var| !var.is_empty()),
            )
        });
        let mut vars: Vec<&str> = configured.chain(known.iter().copied()).collect();
        let mut seen = std::collections::HashSet::new();
        vars.retain(|var| seen.insert(*var));
        Some(vars)
    }

    /// `model.save_key {slug, api_key}`: the key goes into this home's `.env` and the next
    /// turns; answers the provider's refreshed row. Refused before anything is written: a key
    /// that a `.env` file would not read back as itself, a key variable from `config.yaml` that
    /// is not a variable name, and a provider whose key `config.yaml` writes out (it would win
    /// over the saved one).
    fn save_key(&self, slug: &str, key: &str) -> Result<Value, RpcError> {
        if slug.is_empty() || key.is_empty() {
            return Err((REQUIRED, "slug and api_key are required".into()));
        }
        let vars = self
            .slug_key_vars(slug)
            .ok_or((UNKNOWN, format!("unknown provider: {slug}")))?;
        let Some(var) = vars.first().copied() else {
            return Err((NO_KEY_VARIABLE, format!("no env var defined for {slug}")));
        };
        if !env_name_writable(var) {
            return Err((
                NO_KEY_VARIABLE,
                format!(
                    "config.yaml names a key variable for {slug} that is not a variable name or \
                     that steers the next process"
                ),
            ));
        }
        let written_out = self.slug_entries(slug).into_iter().any(|(_, c)| {
            let literal = c.api_key.as_deref().map_or("", str::trim);
            !literal.is_empty() && literal_source(literal, &vars) != Some(var)
        });
        if written_out {
            return Err((
                CONFIGURED_IN_FILE,
                format!(
                    "config.yaml sets the key for {slug} (written out or from another variable): \
                     change it there"
                ),
            ));
        }
        if !env_value_round_trips(var, key) {
            return Err((
                UNKNOWN,
                "api_key cannot be stored: it holds a control character, surrounding quotes \
                 or another assignment"
                    .into(),
            ));
        }
        let _write = env_write();
        set_env_file_value(&self.home.join(".env"), var, Some(key)).map_err(|_| {
            (
                SAVE_FAILED,
                "could not write this profile's .env".to_string(),
            )
        })?;
        self.keys.set(var, Some(key.to_string()));
        self.keys.saw_in_home_file([env_name_key(var)]);
        drop(_write);
        // The picker's row for the slug it sent, as upstream answers.
        let mut row = option_rows(&self.config, &self.keys, "", false)
            .into_iter()
            .find(|row| {
                row["slug"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(slug))
            })
            .unwrap_or_else(|| Value::Object(row(slug, false, Vec::new(), "env")));
        row["authenticated"] = true.into();
        Ok(json!({ "provider": row }))
    }

    /// `model.disconnect {slug}`: removes every key variable of the provider from this home's
    /// `.env` and from the next turns.
    fn disconnect_provider(&self, slug: &str) -> Result<Value, RpcError> {
        if slug.is_empty() {
            return Err((REQUIRED, "slug is required".into()));
        }
        let path = self.home.join(".env");
        let mut cleared = false;
        let _write = env_write();
        // A name the writer refuses was never saved.
        for var in self
            .slug_key_vars(slug)
            .unwrap_or_default()
            .into_iter()
            .filter(|var| env_name_writable(var))
        {
            let in_file = set_env_file_value(&path, var, None).map_err(|_| {
                (
                    DISCONNECT_FAILED,
                    "could not write this profile's .env".to_string(),
                )
            })?;
            let was_set = self.keys.set(var, None);
            cleared |= in_file || was_set;
        }
        if !cleared {
            return Err((NOTHING_SAVED, format!("no credentials found for {slug}")));
        }
        Ok(json!({"slug": slug, "name": slug, "disconnected": true}))
    }

    /// `reload.env`: re-reads the key variables as start-up does (`hermes_config`'s order, the
    /// managed `.env` last, names compared as [`env_name_key`]); the number changed. A key this
    /// gateway has seen in the home `.env` and now gone from it and from what a reload reads is
    /// removed, as upstream removes a known key gone from a readable home `.env`; one only the
    /// shell or the project `.env` set stays. Other variables and a value that would not read
    /// back as itself take effect at the next start.
    fn reload_env(&self) -> usize {
        let _write = env_write();
        let home_names = home_env_names(&self.home);
        let values: HashMap<String, String> = dotenv_values_in(&self.home)
            .into_iter()
            .map(|(name, value)| (env_name_key(&name), value))
            .collect();
        let changed = self
            .key_variables()
            .iter()
            .filter(|var| match values.get(&env_name_key(var)) {
                Some(value) => {
                    env_value_round_trips(var, value) && self.keys.set(var, Some(value.clone()))
                }
                None => {
                    home_names
                        .as_ref()
                        .is_some_and(|names| !names.contains(&env_name_key(var)))
                        && self.keys.came_from_home_file(var)
                        && self.keys.set(var, None)
                }
            })
            .count();
        self.keys.saw_in_home_file(home_names.unwrap_or_default());
        changed
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
    use std::sync::Arc;

    use super::*;
    use crate::chat::Conn;

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
    fn a_key_changed_after_start_up_stays_out_of_the_process_environment() {
        let keys = Keys::default();
        let var = "DESKTOP_MODEL_KEYS_UNIT_TEST_KEY";
        assert_eq!(keys.value(var), None);
        assert!(!keys.changed(var));
        assert!(keys.set(var, Some("k".into())));
        assert_eq!(keys.value(var).as_deref(), Some("k"));
        assert!(std::env::var_os(var).is_none());
        assert!(!keys.set(var, Some("k".into())), "the same value again");
        assert!(keys.set(var, None));
        assert_eq!(keys.value(var), None);
        assert!(keys.changed(var));
    }

    /// A gateway whose one provider, `envkey`, has `api_key` (and `key_env` when given) in
    /// `config.yaml`.
    fn chat_with_key(api_key: &str, key_env: Option<&str>) -> Chat {
        let provider: LlmProviderConfig =
            serde_json::from_value(json!({"api_key": api_key, "api_key_env": key_env})).unwrap();
        let mut config = GatewayConfig::default();
        config.llm_providers.insert("envkey".into(), provider);
        Chat::new(config)
    }

    fn turn_key(chat: &Chat) -> Result<Option<String>, String> {
        let config = chat.turn_config("envkey:m")?;
        Ok(config.llm_providers["envkey"].api_key.clone())
    }

    #[test]
    fn a_config_key_from_the_environment_follows_later_key_changes() {
        // Start-up expanded `${PATH}`: the key is the variable's start-up value.
        let start_up = std::env::var("PATH").unwrap();
        let chat = chat_with_key(&start_up, Some("PATH"));
        assert_eq!(turn_key(&chat).unwrap().as_deref(), Some(start_up.as_str()));
        let (_, authenticated) =
            configured_key("envkey", &chat.config.llm_providers["envkey"], &chat.keys);
        assert!(authenticated);
        chat.keys.set("PATH", Some("saved-key".into()));
        assert_eq!(turn_key(&chat).unwrap().as_deref(), Some("saved-key"));
        chat.keys.set("PATH", None);
        assert!(turn_key(&chat).unwrap_err().contains("disconnected"));
        let (_, authenticated) =
            configured_key("envkey", &chat.config.llm_providers["envkey"], &chat.keys);
        assert!(!authenticated, "a removed key is not configured");

        // A reference start-up could not expand stays `${VAR}`; a key saved since is used.
        let var = "DESKTOP_MODEL_UNIT_TEST_REFERENCE_KEY";
        let chat = chat_with_key(&format!("${{{var}}}"), None);
        chat.keys.set(var, Some("saved-key".into()));
        assert_eq!(turn_key(&chat).unwrap().as_deref(), Some("saved-key"));

        // A key written out in `config.yaml` comes first.
        let chat = chat_with_key("written-key", Some("PATH"));
        chat.keys.set("PATH", Some("saved-key".into()));
        assert_eq!(turn_key(&chat).unwrap().as_deref(), Some("written-key"));
    }

    #[test]
    fn a_saved_key_goes_to_the_key_env_the_provider_is_configured_with() {
        let dir = tempfile::tempdir().unwrap();
        let var = "DESKTOP_MODEL_UNIT_TEST_CONFIGURED_KEY";
        let provider: LlmProviderConfig =
            serde_json::from_value(json!({ "api_key_env": var })).unwrap();
        let mut config = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().into_owned()),
            ..GatewayConfig::default()
        };
        config.llm_providers.insert("xiaomi".into(), provider);
        let chat = Chat::new(config);
        let env = || std::fs::read_to_string(dir.path().join(".env")).unwrap();

        chat.save_key("xiaomi", "saved-key").unwrap();
        assert_eq!(env(), format!("{var}=saved-key\n"));
        let config = chat.turn_config("xiaomi:m").unwrap();
        assert_eq!(
            config.llm_providers["xiaomi"].api_key.as_deref(),
            Some("saved-key")
        );
        drop(config);
        chat.disconnect_provider("xiaomi").unwrap();
        assert!(!env().contains(var), "{}", env());
        assert_eq!(chat.keys.value(var), None);
    }

    #[test]
    fn a_saved_key_goes_to_the_variable_an_unexpanded_config_key_names() {
        let dir = tempfile::tempdir().unwrap();
        let var = "DESKTOP_MODEL_UNIT_TEST_UNEXPANDED_KEY";
        let provider: LlmProviderConfig =
            serde_json::from_value(json!({ "api_key": format!("${{{var}}}") })).unwrap();
        let mut config = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().into_owned()),
            ..GatewayConfig::default()
        };
        config.llm_providers.insert("xiaomi".into(), provider);
        let chat = Chat::new(config);
        let path = dir.path().join(".env");
        std::fs::write(&path, "OTHER_KEY=keep-me\n").unwrap();
        let env = || std::fs::read_to_string(&path).unwrap();
        chat.save_key("xiaomi", "saved-key").unwrap();
        assert_eq!(env(), format!("OTHER_KEY=keep-me\n{var}=saved-key\n"));
        let config = chat.turn_config("xiaomi:m").unwrap();
        assert_eq!(
            config.llm_providers["xiaomi"].api_key.as_deref(),
            Some("saved-key")
        );
        drop(config);
        chat.disconnect_provider("xiaomi").unwrap();
        assert_eq!(env(), "OTHER_KEY=keep-me\n");
        assert_eq!(chat.keys.value(var), None);
        assert!(
            !chat.keys.changed("OTHER_KEY"),
            "only the saved key was held"
        );
    }

    /// A gateway whose `xiaomi` provider has `fields` in `config.yaml`, with a `.env` holding
    /// one other key.
    fn xiaomi_chat(dir: &std::path::Path, fields: Value) -> Chat {
        let provider: LlmProviderConfig = serde_json::from_value(fields).unwrap();
        let mut config = GatewayConfig {
            home_dir: Some(dir.to_string_lossy().into_owned()),
            ..GatewayConfig::default()
        };
        config.llm_providers.insert("xiaomi".into(), provider);
        std::fs::write(dir.join(".env"), "OTHER_KEY=keep-me\n").unwrap();
        Chat::new(config)
    }

    #[test]
    fn a_config_key_variable_that_is_not_a_name_is_refused_and_nothing_written() {
        for fields in [
            json!({"api_key": "${X\nOPENAI_API_KEY}"}),
            json!({"api_key": "${ DESKTOP_MODEL_UNIT_TEST_SPACED }"}),
            json!({"api_key": "${A}${B}"}),
            json!({"api_key_env": "X\nOPENAI_API_KEY"}),
            json!({"api_key": "${}"}),
            json!({"api_key_env": "PATH"}),
            json!({"api_key_env": "LD_PRELOAD"}),
            json!({"api_key": "${HERMES_HOME}"}),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let chat = xiaomi_chat(dir.path(), fields.clone());
            let (code, _) = chat.save_key("xiaomi", "k").unwrap_err();
            assert_eq!(code, NO_KEY_VARIABLE, "{fields}");
            let env = std::fs::read_to_string(dir.path().join(".env")).unwrap();
            assert_eq!(env, "OTHER_KEY=keep-me\n", "{fields}");
            for var in [
                "OPENAI_API_KEY",
                "XIAOMI_API_KEY",
                "PATH",
                "LD_PRELOAD",
                "HERMES_HOME",
            ] {
                assert!(!chat.keys.changed(var), "{fields} {var}");
            }
        }
    }

    #[test]
    fn a_key_written_out_in_config_yaml_is_not_shadowed_by_a_saved_one() {
        let dir = tempfile::tempdir().unwrap();
        let chat = xiaomi_chat(dir.path(), json!({"api_key": "written-key"}));
        let (code, message) = chat.save_key("xiaomi", "saved-key").unwrap_err();
        assert_eq!(code, CONFIGURED_IN_FILE);
        assert!(message.contains("config.yaml"), "{message}");
        let env = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert_eq!(env, "OTHER_KEY=keep-me\n");
        let config = chat.turn_config("xiaomi:m").unwrap();
        assert_eq!(
            config.llm_providers["xiaomi"].api_key.as_deref(),
            Some("written-key")
        );
    }

    #[test]
    fn a_key_config_yaml_takes_from_another_variable_is_not_shadowed() {
        let dir = tempfile::tempdir().unwrap();
        let chat = xiaomi_chat(dir.path(), json!({}));
        let mut config = chat.config.clone();
        // `mimo` saves to its own `key_env`; `xiaomi`'s key is `PATH`'s start-up value.
        let start_up = std::env::var("PATH").unwrap();
        for (name, fields) in [
            (
                "mimo",
                json!({"api_key_env": "DESKTOP_MODEL_UNIT_TEST_WRITE_KEY"}),
            ),
            (
                "xiaomi",
                json!({"api_key_env": "PATH", "api_key": start_up}),
            ),
        ] {
            config
                .llm_providers
                .insert(name.into(), serde_json::from_value(fields).unwrap());
        }
        let chat = Chat::new(config);
        let (code, _) = chat.save_key("mimo", "saved-key").unwrap_err();
        assert_eq!(code, CONFIGURED_IN_FILE);
        let env = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert_eq!(env, "OTHER_KEY=keep-me\n");
        assert!(!chat.keys.changed("DESKTOP_MODEL_UNIT_TEST_WRITE_KEY"));
    }

    #[test]
    fn reload_env_drops_a_saved_key_removed_from_a_readable_file() {
        let dir = tempfile::tempdir().unwrap();
        let var = "DESKTOP_MODEL_UNIT_TEST_RELOADED_KEY";
        let chat = xiaomi_chat(dir.path(), json!({ "api_key_env": var }));
        let path = dir.path().join(".env");
        chat.save_key("xiaomi", "saved-key").unwrap();
        std::fs::write(&path, "OTHER_KEY=keep-me\n").unwrap();
        assert_eq!(chat.reload_env(), 1);
        assert_eq!(chat.keys.value(var), None);
        assert!(!chat.keys.changed("OTHER_KEY"));
        // A file that cannot be read removes nothing.
        chat.save_key("xiaomi", "saved-key").unwrap();
        std::fs::write(&path, b"\xff\n").unwrap();
        assert_eq!(chat.reload_env(), 0);
        assert_eq!(chat.keys.value(var).as_deref(), Some("saved-key"));
    }

    #[test]
    fn reload_env_drops_a_key_start_up_read_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let var = "DESKTOP_MODEL_UNIT_TEST_START_UP_KEY";
        let config = xiaomi_chat(dir.path(), json!({ "api_key_env": var })).config;
        let path = dir.path().join(".env");
        std::fs::write(&path, format!("OTHER_KEY=keep-me\n{var}=start-up-key\n")).unwrap();
        let chat = Chat::new(config);
        std::fs::write(&path, "OTHER_KEY=keep-me\n").unwrap();
        // The process environment never held `var`, so the count cannot show the removal.
        chat.reload_env();
        assert!(chat.keys.changed(var));
        assert_eq!(chat.keys.value(var), None);
        assert!(!chat.keys.changed("OTHER_KEY"));
    }

    #[cfg(windows)]
    #[test]
    fn reload_env_drops_a_start_up_key_whose_line_differs_in_case() {
        let dir = tempfile::tempdir().unwrap();
        let var = "DESKTOP_MODEL_UNIT_TEST_CASE_KEY";
        let config = xiaomi_chat(dir.path(), json!({ "api_key_env": var })).config;
        let path = dir.path().join(".env");
        let line = var.to_ascii_lowercase();
        std::fs::write(&path, format!("OTHER_KEY=keep-me\n{line}=start-up-key\n")).unwrap();
        let chat = Chat::new(config);
        std::fs::write(&path, "OTHER_KEY=keep-me\n").unwrap();
        chat.reload_env();
        assert!(chat.keys.changed(var));
        assert_eq!(chat.keys.value(var), None);
    }

    #[test]
    fn a_saved_key_goes_to_the_entry_named_as_the_slug_first() {
        // `llm_providers` is a `HashMap`: each new map iterates in its own order.
        for _ in 0..32 {
            let mut config = GatewayConfig::default();
            for (name, var) in [("xiaomi", "CANONICAL_KEY"), ("mimo", "EXACT_KEY")] {
                let provider: LlmProviderConfig =
                    serde_json::from_value(json!({ "api_key_env": var })).unwrap();
                config.llm_providers.insert(name.into(), provider);
            }
            let chat = Chat::new(config);
            assert_eq!(
                chat.slug_key_vars("mimo").unwrap()[..2],
                ["EXACT_KEY", "CANONICAL_KEY"]
            );
        }
    }

    #[tokio::test]
    async fn the_key_methods_serve_only_clients_on_this_machine() {
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().into_owned()),
            ..GatewayConfig::default()
        };
        let chat = Arc::new(Chat::new(config));
        let call = |origin: Origin, method: &str, params: Value| {
            let handler = METHODS.iter().find(|(m, _)| *m == method).unwrap().1;
            let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
            let mut conn = Conn::new(1, outbox, false);
            conn.origin = origin;
            let params = params.as_object().unwrap().clone();
            handler(Request {
                chat: Arc::clone(&chat),
                conn,
                params,
            })
        };
        let requests = [
            ("model.options", json!({})),
            (
                "model.save_key",
                json!({"slug": "xiaomi", "api_key": "remote-key"}),
            ),
            ("model.disconnect", json!({"slug": "xiaomi"})),
            ("reload.env", json!({})),
        ];
        for origin in [Origin::Remote, Origin::Webapp] {
            for (method, params) in &requests {
                let reply = call(origin, method, params.clone()).await;
                assert_eq!(reply.unwrap_err().0, LOCAL_ONLY, "{method} from {origin:?}");
            }
        }
        assert!(!dir.path().join(".env").exists());
        assert!(!chat.keys.changed("XIAOMI_API_KEY"));
        // The same request from this machine is served.
        assert!(call(Origin::Local, "model.options", json!({}))
            .await
            .is_ok());
    }

    #[test]
    fn a_env_file_that_cannot_be_written_fails_the_save_and_the_disconnect() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".env")).unwrap();
        let config = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().into_owned()),
            ..GatewayConfig::default()
        };
        let chat = Chat::new(config);
        let failed = (
            SAVE_FAILED,
            "could not write this profile's .env".to_string(),
        );
        assert_eq!(chat.save_key("xiaomi", "saved-key").unwrap_err(), failed);
        assert!(
            !chat.keys.changed("XIAOMI_API_KEY"),
            "a failed save is not used"
        );
        let failed = (DISCONNECT_FAILED, failed.1);
        assert_eq!(chat.disconnect_provider("xiaomi").unwrap_err(), failed);
    }

    #[test]
    fn a_key_saved_for_a_provider_config_yaml_does_not_list_passes_the_set_up_check() {
        assert!(
            std::env::var_os("XIAOMI_API_KEY").is_none(),
            "this test needs XIAOMI_API_KEY unset"
        );
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().into_owned()),
            model: Some("xiaomi:mimo-test".into()),
            ..GatewayConfig::default()
        };
        let chat = Chat::new(config);
        let before = chat.runtime_check();
        assert_eq!(before["ok"], false, "{before}");

        let saved = chat.save_key("xiaomi", "saved-key").unwrap();
        assert_eq!(saved["provider"]["slug"], "xiaomi", "{saved}");
        assert_eq!(saved["provider"]["authenticated"], true, "{saved}");
        let check = chat.runtime_check();
        assert_eq!(check["ok"], true, "{check}");

        // Disconnected, the provider is as it was before the save: start-up never had its key.
        chat.disconnect_provider("xiaomi").unwrap();
        assert_eq!(chat.runtime_check(), before);
    }

    #[test]
    fn a_saved_key_answers_the_picker_row_of_the_slug_sent() {
        let dir = tempfile::tempdir().unwrap();
        let var = "DESKTOP_MODEL_UNIT_TEST_ALIAS_KEY";
        let provider: LlmProviderConfig =
            serde_json::from_value(json!({ "api_key_env": var })).unwrap();
        let mut config = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().into_owned()),
            ..GatewayConfig::default()
        };
        config.llm_providers.insert("mimo".into(), provider);
        let chat = Chat::new(config);
        // The picker lists the alias entry as `mimo`: the reply is that row.
        let saved = chat.save_key("MiMo", "saved-key").unwrap();
        assert_eq!(saved["provider"]["slug"], "mimo", "{saved}");
        assert_eq!(saved["provider"]["source"], "configured", "{saved}");
        assert_eq!(saved["provider"]["key_env"], var, "{saved}");
        assert_eq!(saved["provider"]["authenticated"], true, "{saved}");
        assert_eq!(chat.keys.value(var).as_deref(), Some("saved-key"));

        // With no entry, the picker lists the key as `xiaomi`; the reply still names `mimo`.
        let dir = tempfile::tempdir().unwrap();
        let config = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().into_owned()),
            ..GatewayConfig::default()
        };
        let saved = Chat::new(config).save_key("mimo", "saved-key").unwrap();
        assert_eq!(
            saved["provider"],
            json!({"slug": "mimo", "name": "mimo", "total_models": 0, "models": [],
                   "is_current": false, "source": "env", "authenticated": true})
        );
    }

    #[test]
    fn the_set_up_check_reports_a_disconnected_start_up_key() {
        // Start-up read `PATH` from the environment; the gateway has since removed it.
        let provider: LlmProviderConfig =
            serde_json::from_value(json!({ "api_key_env": "PATH" })).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut config = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().into_owned()),
            model: Some("envkey:m".into()),
            ..GatewayConfig::default()
        };
        config.llm_providers.insert("envkey".into(), provider);
        let chat = Chat::new(config);
        chat.keys.set("PATH", None);
        let check = chat.runtime_check();
        assert_eq!(check["ok"], false, "{check}");
        let error = check["error"].as_str().unwrap();
        assert!(error.contains("it was disconnected"), "{check}");
    }

    #[test]
    fn a_providers_own_model_lists_first_among_its_models() {
        let dir = tempfile::tempdir().unwrap();
        let chat = xiaomi_chat(
            dir.path(),
            json!({ "model": "mimo-own", "models": ["mimo-a", "mimo-b"] }),
        );
        let options = chat.model_options(false);
        let row = options["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["slug"] == "xiaomi")
            .unwrap()
            .clone();
        assert_eq!(
            row["models"],
            json!(["mimo-own", "mimo-a", "mimo-b"]),
            "{row}"
        );
        let listed = xiaomi_chat(
            dir.path(),
            json!({ "model": "mimo-b", "models": ["mimo-a", "mimo-b"] }),
        )
        .model_options(false);
        assert_eq!(
            listed["providers"][0]["models"],
            json!(["mimo-a", "mimo-b"])
        );
    }
}
