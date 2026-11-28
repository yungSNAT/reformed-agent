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

mod keys;
mod options;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_reload;

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

use hermes_config::loader::{
    dotenv_values_in, env_name_key, env_name_writable, env_value_round_trips, set_env_file_value,
};
use hermes_config::{GatewayConfig, LlmProviderConfig};
use hermes_core::providers::{canonical_provider_id, provider_api_key_env_vars, KNOWN_PROVIDERS};
use serde_json::{json, Map, Value};

pub(super) use keys::Keys;
use keys::{env_file_values, env_write, home_env_names, PROJECT};
use options::{key_vars, literal_source, option_rows, parse_switch, row};

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
        let entry = self.slug_entry(&provider);
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

    /// The `config.yaml` entry a turn on provider `slug` reads, as the provider build picks it,
    /// so a key is saved to, and a turn sends, the variable that turn's build reads.
    fn slug_entry(&self, slug: &str) -> Option<(&String, &LlmProviderConfig)> {
        let slug = slug.trim();
        hermes_provider_runtime::active_llm_provider_entry(
            &self.config,
            slug,
            &hermes_provider_runtime::normalize_runtime_provider_name(slug),
        )
    }

    /// A known provider's key variables, first to last: the `${VAR}` its configured `api_key`
    /// still holds, the `key_env` it is configured with, then its own; `None` for a provider
    /// this gateway does not know.
    fn slug_key_vars(&self, slug: &str) -> Option<Vec<&str>> {
        let known = key_vars(slug)?;
        let configured = self.slug_entry(slug).into_iter().flat_map(|(_, c)| {
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

    /// Resolves the variable `slug` saves to and verifies `config.yaml` allows writing it.
    fn save_target(&self, slug: &str) -> Result<&str, RpcError> {
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
        let written_out = self.slug_entry(slug).into_iter().any(|(_, c)| {
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
        Ok(var)
    }

    /// The picker's row of the entry a save wrote to, else of the slug sent, as upstream answers;
    /// one named exactly first.
    fn save_reply_row(&self, slug: &str) -> Value {
        let name = self
            .slug_entry(slug)
            .map_or(slug, |(name, _)| name.as_str());
        let mut rows = option_rows(&self.config, &self.keys, "", false);
        rows.sort_by_key(|row| row["slug"] != name);
        let mut row = rows
            .into_iter()
            .find(|row| {
                row["slug"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(name))
            })
            .unwrap_or_else(|| Value::Object(row(slug, false, Vec::new(), "env")));
        row["authenticated"] = true.into();
        row
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
        let var = self.save_target(slug)?;
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
        Ok(json!({ "provider": self.save_reply_row(slug) }))
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
    /// removed, as upstream removes a known key gone from a readable home `.env`, or takes the
    /// value a fresh start would give it from the shell or the project `.env` (upstream drops
    /// it); one only the shell or the project `.env` set stays. Other variables and a value
    /// that would not read back as itself take effect at the next start.
    fn reload_env(&self) -> usize {
        let _write = env_write();
        let home_names = home_env_names(&self.home);
        // After the read that sanitises the project file, as at start-up.
        let values: HashMap<String, String> = dotenv_values_in(&self.home)
            .into_iter()
            .map(|(name, value)| (env_name_key(&name), value))
            .collect();
        let project = env_file_values(Path::new(PROJECT));
        let changed = self
            .key_variables()
            .iter()
            .filter(|var| match values.get(&env_name_key(var)) {
                Some(value) if env_value_round_trips(var, value) => {
                    self.keys.set(var, Some(value.clone()))
                }
                // Gone from the home `.env`, or read only as a value that would not read back
                // as itself: what a fresh start would read.
                _ => {
                    home_names
                        .as_ref()
                        .is_some_and(|names| !names.contains(&env_name_key(var)))
                        && self.keys.came_from_home_file(var)
                        && self.keys.drop_home_value(var, project.as_ref())
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
