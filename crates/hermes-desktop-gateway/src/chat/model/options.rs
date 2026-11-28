//! Provider key-variable resolution, picker row formatting and model-switch parsing.

use hermes_config::{GatewayConfig, LlmProviderConfig};
use hermes_core::providers::{canonical_provider_id, provider_api_key_env_vars, KNOWN_PROVIDERS};
use serde_json::{Map, Value};

use super::keys::Keys;

/// The variables a known provider's key is read from, first to last; `None` for a provider
/// this gateway does not know.
pub(super) fn key_vars(slug: &str) -> Option<&'static [&'static str]> {
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
pub(super) fn literal_source<'a>(literal: &'a str, vars: &[&'a str]) -> Option<&'a str> {
    if let Some(var) = literal.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
        return Some(var);
    }
    vars.iter()
        .copied()
        .find(|var| std::env::var(var).is_ok_and(|value| value.trim() == literal))
}

/// A configured provider's key variable and whether its key is set.
pub(super) fn configured_key(
    slug: &str,
    provider: &LlmProviderConfig,
    keys: &Keys,
) -> (Option<String>, bool) {
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
pub(super) fn row(
    slug: &str,
    is_current: bool,
    models: Vec<String>,
    source: &str,
) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("slug".into(), slug.into());
    row.insert("name".into(), slug.into());
    row.insert("total_models".into(), models.len().into());
    row.insert("models".into(), models.into());
    row.insert("is_current".into(), is_current.into());
    row.insert("source".into(), source.into());
    row
}

fn configured_rows(
    config: &GatewayConfig,
    keys: &Keys,
    is_current: impl Fn(&str) -> bool,
) -> (Vec<Value>, Vec<String>) {
    let mut rows = Vec::new();
    let mut seen = Vec::new();
    let mut configured: Vec<_> = config.llm_providers.iter().collect();
    // By name in any case, then by name.
    configured.sort_by(|(a, _), (b, _)| {
        a.to_ascii_lowercase()
            .cmp(&b.to_ascii_lowercase())
            .then_with(|| a.cmp(b))
    });
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
    (rows, seen)
}

fn known_rows(
    keys: &Keys,
    is_current: impl Fn(&str) -> bool,
    include_unconfigured: bool,
    mut seen: Vec<String>,
) -> Vec<Value> {
    let mut rows = Vec::new();
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

/// The picker rows for `config`: each configured provider, then each known provider with a key
/// set, then (when asked) every other known provider with no models. The current provider is
/// listed even without a key, marked as not authenticated.
pub(super) fn option_rows(
    config: &GatewayConfig,
    keys: &Keys,
    current: &str,
    include_unconfigured: bool,
) -> Vec<Value> {
    let current_canonical = canonical_provider_id(current);
    let is_current = |slug: &str| {
        slug.eq_ignore_ascii_case(current) || canonical_provider_id(slug) == current_canonical
    };
    let (mut rows, seen) = configured_rows(config, keys, is_current);
    rows.extend(known_rows(keys, is_current, include_unconfigured, seen));
    rows
}

/// A `config.set model` value: the model and the provider it names with `--provider`; `None`
/// for a switch flag this gateway does not honour (`--global`, `--once` and others).
pub(super) fn parse_switch(value: &str) -> Option<(String, Option<String>)> {
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
