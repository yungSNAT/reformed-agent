//! Normalize Python Hermes `config.yaml` into shapes `GatewayConfig` can deserialize.
//!
//! Python 版常用：嵌套 `model:`、`toolsets:`、`agent.max_turns`、`session_reset`、`providers`、
//! 根级 `telegram` / `discord` / `weixin` 等平台块（而非 `platforms:` 下）等；
//! Rust 字段能直接对齐的保留，其余在归一化层映射；平台专有键经 [`crate::platform::PlatformConfig`]
//! 的 `#[serde(flatten)] extra` 保留。

use serde_yaml::{Mapping, Value};
use std::collections::HashSet;

fn key(s: &str) -> Value {
    Value::String(s.to_string())
}

fn as_str(v: &Value) -> Option<&str> {
    v.as_str()
}

fn trimmed_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_i64().map(|i| i as u64))
}

fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn drop_null_top_level_values(map: &mut Mapping) {
    let null_keys: Vec<Value> = map
        .iter()
        .filter_map(|(k, v)| matches!(v, Value::Null).then_some(k.clone()))
        .collect();
    for key in null_keys {
        if let Some(name) = key.as_str() {
            tracing::warn!("Ignoring null top-level config key `{name}`; using default");
        }
        map.remove(&key);
    }
}

fn normalized_base_url(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
}

fn canonical_custom_provider_name(provider: &str) -> String {
    provider
        .trim()
        .strip_prefix("custom:")
        .unwrap_or_else(|| provider.trim())
        .trim()
        .to_string()
}

/// Lift `agent.max_turns` to root `max_turns` when the latter is absent.
fn lift_agent_max_turns(map: &mut Mapping) {
    let max_key = key("max_turns");
    if map.contains_key(&max_key) {
        return;
    }
    let Some(Value::Mapping(agent)) = map.get(key("agent")) else {
        return;
    };
    let Some(mt) = agent.get(key("max_turns")) else {
        return;
    };
    map.insert(max_key, mt.clone());
}

/// `toolsets: [a, b]` → `tools: [a, b]` when root `tools` is absent or empty.
fn lift_toolsets_to_tools(map: &mut Mapping) {
    let tools_key = key("tools");
    let keep_existing = match map.get(&tools_key) {
        Some(Value::Sequence(s)) => !s.is_empty(),
        Some(Value::String(st)) => !st.is_empty(),
        _ => false,
    };
    if keep_existing {
        return;
    }
    let Some(Value::Sequence(ts)) = map.remove(key("toolsets")) else {
        return;
    };
    let out: Vec<Value> = ts
        .iter()
        .filter_map(|v| v.as_str().map(|s| Value::String(s.to_string())))
        .collect();
    if !out.is_empty() {
        map.insert(tools_key, Value::Sequence(out));
    }
}

/// Python `model: { default|name|model, provider, base_url, ... }` →
/// `model: "provider:default"` + `llm_providers`.
fn normalize_model_block(map: &mut Mapping) {
    let model_key = key("model");
    let Some(raw) = map.remove(&model_key) else {
        return;
    };

    match raw {
        Value::String(s) => {
            map.insert(model_key, Value::String(s));
        }
        Value::Mapping(m) => {
            if let Some(persist) = m.get(key("persist_switch_by_default")).cloned() {
                let switch_key = key("model_switch");
                let mut switch_map = match map.remove(&switch_key) {
                    Some(Value::Mapping(existing)) => existing,
                    Some(other) => {
                        let mut existing = Mapping::new();
                        existing.insert(key("value"), other);
                        existing
                    }
                    None => Mapping::new(),
                };
                switch_map.insert(key("persist_switch_by_default"), persist);
                map.insert(switch_key, Value::Mapping(switch_map));
            }
            // A mapping-valued default pairs the model with the provider it is
            // routed through; that provider outranks the outer one (often `auto`).
            let (default, nested_provider) = ["default", "name", "model"]
                .iter()
                .find_map(|field| match m.get(key(field))? {
                    // A mapping that names no model is skipped, as an empty scalar is.
                    Value::Mapping(nested) => {
                        let model = trimmed_str(nested.get(key("model")))
                            .or_else(|| trimmed_str(nested.get(key("default"))))?;
                        Some((Some(model), trimmed_str(nested.get(key("provider")))))
                    }
                    scalar => trimmed_str(Some(scalar)).map(|model| (Some(model), None)),
                })
                .unwrap_or((None, None));
            let outer_provider = trimmed_str(m.get(key("provider")));
            let provider = nested_provider.or(outer_provider);
            let base_url = m.get(key("base_url")).and_then(normalized_base_url);
            let api_key = m
                .get(key("api_key"))
                .and_then(as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let api_key_env = m
                .get(key("api_key_env"))
                .and_then(as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());

            let model_str = match (provider, default) {
                (Some(p), Some(d)) => format!("{}:{d}", canonical_custom_provider_name(p)),
                (None, Some(d)) => d.to_string(),
                (Some(p), None) => format!("{}:", canonical_custom_provider_name(p)),
                (None, None) => {
                    map.insert(model_key, Value::Mapping(m));
                    return;
                }
            };
            map.insert(model_key, Value::String(model_str));

            // Credentials written beside the outer provider belong to it, not to
            // a different provider nested in the default; with no outer provider
            // they belong to the nested one.
            if let Some(p) = outer_provider
                .or(nested_provider)
                .filter(|_| base_url.is_some() || api_key.is_some() || api_key_env.is_some())
            {
                let provider_name = canonical_custom_provider_name(p);
                let llm_key = key("llm_providers");
                let mut llm = match map.get(&llm_key).cloned() {
                    Some(Value::Mapping(x)) => x,
                    _ => Mapping::new(),
                };
                let prov_entry = llm
                    .entry(Value::String(provider_name))
                    .or_insert_with(|| Value::Mapping(Mapping::new()));
                if let Value::Mapping(ref mut em) = prov_entry {
                    if let Some(bu) = base_url {
                        em.insert(key("base_url"), Value::String(bu));
                    }
                    if let Some(key_value) = api_key {
                        em.insert(key("api_key"), Value::String(key_value.to_string()));
                    }
                    if let Some(env_name) = api_key_env {
                        em.insert(key("api_key_env"), Value::String(env_name.to_string()));
                    }
                }
                map.insert(llm_key, Value::Mapping(llm));
            }
        }
        other => {
            map.insert(model_key, other);
        }
    }
}

/// `custom_providers: [{ name, base_url, ... }]` → merge into `llm_providers`.
fn merge_custom_providers_into_llm(map: &mut Mapping) {
    let Some(Value::Sequence(providers)) = map.remove(key("custom_providers")) else {
        return;
    };
    if providers.is_empty() {
        return;
    }

    let llm_key = key("llm_providers");
    let mut llm = match map.get(&llm_key).cloned() {
        Some(Value::Mapping(x)) => x,
        _ => Mapping::new(),
    };

    for entry in providers {
        let Value::Mapping(src) = entry else {
            continue;
        };
        let Some(provider_name) = src
            .get(key("name"))
            .and_then(as_str)
            .map(canonical_custom_provider_name)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let slot = llm
            .entry(Value::String(provider_name))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if let Value::Mapping(provider_cfg) = slot {
            for field in [
                "api_key",
                "api_key_env",
                "base_url",
                "command",
                "args",
                "model",
                "models",
                "discover_models",
                "api_mode",
                "max_tokens",
                "temperature",
                "extra_body",
                "rate_limit",
                "credential_pool",
                "oauth_token_url",
                "oauth_client_id",
            ] {
                let field_key = key(field);
                let Some(value) = src.get(&field_key) else {
                    continue;
                };
                let normalized = if field == "base_url" {
                    normalized_base_url(value).map(Value::String)
                } else {
                    Some(value.clone())
                };
                if let Some(value) = normalized {
                    provider_cfg.insert(field_key, value);
                }
            }
        }
    }

    map.insert(llm_key, Value::Mapping(llm));
}

/// `providers: { openai: { api_key: ... } }` → merge into `llm_providers`. Upstream's
/// spellings are read as it reads them: the endpoint is the first of `base_url`, `url` and
/// `api`, and the key's variable is `key_env`, else `api_key_env`.
fn merge_providers_into_llm(map: &mut Mapping) {
    let Some(Value::Mapping(providers)) = map.remove(key("providers")) else {
        return;
    };
    if providers.is_empty() {
        return;
    }
    let llm_key = key("llm_providers");
    let mut llm = match map.get(&llm_key).cloned() {
        Some(Value::Mapping(x)) => x,
        _ => Mapping::new(),
    };
    for (pk, pv) in providers {
        let pname = match pk.as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let slot = llm
            .entry(Value::String(pname))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if let Value::Mapping(em) = slot {
            if let Value::Mapping(src) = pv {
                let endpoint = (trimmed_str(src.get(key("base_url"))).is_none())
                    .then(|| {
                        ["url", "api"]
                            .iter()
                            .find_map(|field| src.get(key(field)).and_then(normalized_base_url))
                    })
                    .flatten();
                let key_env = trimmed_str(src.get(key("key_env"))).map(str::to_string);
                for (k, v) in src {
                    em.insert(k, v);
                }
                if let Some(url) = endpoint {
                    em.insert(key("base_url"), Value::String(url));
                }
                if let Some(env_name) = key_env {
                    em.insert(key("api_key_env"), Value::String(env_name));
                }
            }
        }
    }
    map.insert(llm_key, Value::Mapping(llm));
}

fn merge_fallback_provider_metadata(map: &mut Mapping, provider: &str, entry: &Mapping) {
    let provider = provider.trim();
    if provider.is_empty() {
        return;
    }

    let llm_key = key("llm_providers");
    let mut llm = match map.get(&llm_key).cloned() {
        Some(Value::Mapping(x)) => x,
        _ => Mapping::new(),
    };
    let slot = llm
        .entry(Value::String(provider.to_string()))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if let Value::Mapping(provider_cfg) = slot {
        for field in [
            "api_key",
            "api_key_env",
            "base_url",
            "command",
            "args",
            "oauth_token_url",
            "oauth_client_id",
        ] {
            let field_key = key(field);
            let Some(value) = entry.get(&field_key) else {
                continue;
            };
            let normalized = if field == "base_url" {
                normalized_base_url(value).map(Value::String)
            } else {
                Some(value.clone())
            };
            if let Some(value) = normalized {
                provider_cfg.insert(field_key, value);
            }
        }
    }
    map.insert(llm_key, Value::Mapping(llm));
}

fn push_fallback_spec(spec: String, chain: &mut Vec<Value>, seen: &mut HashSet<String>) {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return;
    }
    let identity = trimmed.to_ascii_lowercase();
    if seen.insert(identity) {
        chain.push(Value::String(trimmed.to_string()));
    }
}

fn collect_fallback_entries(
    raw: Value,
    map: &mut Mapping,
    chain: &mut Vec<Value>,
    seen: &mut HashSet<String>,
) {
    match raw {
        Value::String(spec) => push_fallback_spec(spec, chain, seen),
        Value::Sequence(seq) => {
            for entry in seq {
                collect_fallback_entries(entry, map, chain, seen);
            }
        }
        Value::Mapping(entry) => {
            let provider = entry
                .get(key("provider"))
                .and_then(as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let model = entry
                .get(key("model"))
                .and_then(as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());

            let Some(model) = model else {
                return;
            };

            let spec = match provider {
                Some(provider) if !model.contains(':') => {
                    merge_fallback_provider_metadata(map, provider, &entry);
                    format!("{provider}:{model}")
                }
                Some(provider) => {
                    merge_fallback_provider_metadata(map, provider, &entry);
                    model.to_string()
                }
                None => model.to_string(),
            };
            push_fallback_spec(spec, chain, seen);
        }
        _ => {}
    }
}

/// Python supports `fallback_providers` plus legacy `fallback_model`; Rust uses
/// string model specs in `fallback_models`, so normalize and merge the effective
/// chain before deserializing.
fn normalize_fallback_chain(map: &mut Mapping) {
    let fallback_models_key = key("fallback_models");
    let mut chain: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    if let Some(existing) = map.remove(&fallback_models_key) {
        collect_fallback_entries(existing, map, &mut chain, &mut seen);
    }
    if let Some(providers) = map.remove(key("fallback_providers")) {
        collect_fallback_entries(providers, map, &mut chain, &mut seen);
    }
    if let Some(legacy) = map.remove(key("fallback_model")) {
        collect_fallback_entries(legacy, map, &mut chain, &mut seen);
    }

    if let Some(Value::String(first)) = chain.first().cloned() {
        map.insert(key("fallback_model"), Value::String(first));
    }
    if !chain.is_empty() {
        map.insert(fallback_models_key, Value::Sequence(chain));
    }
}

/// Python `session_reset: { mode, idle_minutes, at_hour }` → `session.reset_policy` (tagged enum shape).
fn normalize_session_reset(map: &mut Mapping) {
    let Some(Value::Mapping(sr)) = map.get(key("session_reset")).cloned() else {
        return;
    };
    let mode = sr.get(key("mode")).and_then(as_str).map(str::to_lowercase);
    let idle_minutes = sr.get(key("idle_minutes")).and_then(as_u64);
    let at_hour = sr
        .get(key("at_hour"))
        .and_then(as_u64)
        .map(|h| h.min(23) as u8);

    let reset_policy = match mode.as_deref() {
        Some("daily") => {
            let h = at_hour.unwrap_or(0);
            let mut m = Mapping::new();
            m.insert(key("type"), Value::String("daily".into()));
            m.insert(key("at_hour"), Value::Number(serde_yaml::Number::from(h)));
            Value::Mapping(m)
        }
        Some("idle") => {
            let tm = idle_minutes.unwrap_or(30);
            let mut m = Mapping::new();
            m.insert(key("type"), Value::String("idle".into()));
            m.insert(
                key("timeout_minutes"),
                Value::Number(serde_yaml::Number::from(tm)),
            );
            Value::Mapping(m)
        }
        Some("both") => {
            let mut daily = Mapping::new();
            daily.insert(
                key("at_hour"),
                Value::Number(serde_yaml::Number::from(at_hour.unwrap_or(0))),
            );
            let mut idle = Mapping::new();
            idle.insert(
                key("timeout_minutes"),
                Value::Number(serde_yaml::Number::from(idle_minutes.unwrap_or(30))),
            );
            let mut m = Mapping::new();
            m.insert(key("type"), Value::String("both".into()));
            m.insert(key("daily"), Value::Mapping(daily));
            m.insert(key("idle"), Value::Mapping(idle));
            Value::Mapping(m)
        }
        Some("none") => {
            let mut m = Mapping::new();
            m.insert(key("type"), Value::String("none".into()));
            Value::Mapping(m)
        }
        _ => return,
    };

    map.remove(key("session_reset"));
    let session_key = key("session");
    let mut session = match map.get(&session_key).cloned() {
        Some(Value::Mapping(x)) => x,
        _ => Mapping::new(),
    };
    session.insert(key("reset_policy"), reset_policy);
    map.insert(session_key, Value::Mapping(session));
}

/// Normalize `platform_toolsets` entries so mixed scalar YAML values (e.g.
/// bare numbers like `12306`) deserialize into `Vec<String>` safely. A `null`
/// entry is no entry.
fn normalize_platform_toolsets(map: &mut Mapping) {
    let ptk = key("platform_toolsets");
    let Some(Value::Mapping(existing)) = map.remove(&ptk) else {
        return;
    };

    let mut normalized = Mapping::new();
    for (platform, raw_values) in existing {
        let Some(platform_name) = platform.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        if raw_values.is_null() {
            continue;
        }

        let mut out = Vec::new();
        match raw_values {
            Value::Sequence(seq) => {
                for entry in seq {
                    if let Some(s) = scalar_to_string(&entry) {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            out.push(Value::String(trimmed.to_string()));
                        }
                    }
                }
            }
            other => {
                if let Some(s) = scalar_to_string(&other) {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        out.push(Value::String(trimmed.to_string()));
                    }
                }
            }
        }

        normalized.insert(key(platform_name), Value::Sequence(out));
    }

    map.insert(ptk, Value::Mapping(normalized));
}

/// Earlier releases wrote every default entry into the user's file, so a
/// user map holding the entries each of them wrote is read as theirs: its
/// entries equal to the default were never chosen and are dropped. The
/// managed scope's map is never read this way: its pins bind as written.
pub(crate) fn drop_written_default_platform_toolsets(
    toolsets: &mut std::collections::HashMap<String, Vec<String>>,
) {
    // Every release that wrote the defaults wrote at least these.
    let written_by_every_release = ["cli", "telegram", "discord", "whatsapp", "slack"];
    if !written_by_every_release
        .iter()
        .all(|platform| toolsets.contains_key(*platform))
    {
        return;
    }
    for (platform, defaults) in crate::config::default_platform_toolsets() {
        if toolsets.get(&platform) == Some(&defaults) {
            toolsets.remove(&platform);
        }
    }
}

/// Python often declares `telegram:` / `discord:` at the **root** instead of under `platforms:`.
fn lift_root_platform_blocks(map: &mut Mapping) {
    const NAMES: &[&str] = &[
        "telegram",
        "discord",
        "slack",
        "whatsapp",
        "signal",
        "weixin",
        "matrix",
        "mattermost",
        "dingtalk",
        "feishu",
        "wecom",
        "email",
        "sms",
        "homeassistant",
        "bluebubbles",
    ];

    let plat_key = key("platforms");
    let mut platforms = match map.get(&plat_key).cloned() {
        Some(Value::Mapping(x)) => x,
        _ => Mapping::new(),
    };

    for name in NAMES {
        let nk = key(name);
        if platforms.contains_key(&nk) {
            continue;
        }
        if let Some(block) = map.remove(&nk) {
            platforms.insert(nk, block);
        }
    }

    if !platforms.is_empty() {
        map.insert(plat_key, Value::Mapping(platforms));
    }
}

/// Apply in-place transforms so Python-style Hermes YAML deserializes into [`crate::config::GatewayConfig`].
pub(crate) fn normalize_config_yaml_root(map: &mut Mapping) {
    drop_null_top_level_values(map);
    // Order matters: model before merge_providers (may touch llm_providers)
    normalize_model_block(map);
    merge_custom_providers_into_llm(map);
    merge_providers_into_llm(map);
    normalize_fallback_chain(map);
    lift_agent_max_turns(map);
    lift_toolsets_to_tools(map);
    normalize_platform_toolsets(map);
    normalize_session_reset(map);
    lift_root_platform_blocks(map);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_fixture(raw: &str) -> crate::config::GatewayConfig {
        let mut root = serde_yaml::from_str(raw).expect("YAML fixture must be a mapping");
        normalize_config_yaml_root(&mut root);
        serde_yaml::from_value(Value::Mapping(root)).expect("valid config fixture")
    }

    #[test]
    fn legacy_empty_providers_preserve_native_settings_and_lift_agent_limits() {
        let raw = r#"
agent: {max_turns: 17}
tools: terminal
toolsets: [web]
custom_providers: []
providers: {}
llm_providers:
  office: {base_url: 'https://office.invalid/v1'}
discord: {enabled: false}
platforms:
  discord: {enabled: true, require_mention: true}
"#;
        let mut root: Mapping = serde_yaml::from_str(raw).unwrap();
        normalize_config_yaml_root(&mut root);

        assert_eq!(root[key("max_turns")].as_u64(), Some(17));
        assert_eq!(root[key("tools")].as_str(), Some("terminal"));
        assert!(!root.contains_key(key("custom_providers")));
        assert!(!root.contains_key(key("providers")));
        let providers = root[key("llm_providers")].as_mapping().unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(
            providers[key("office")]["base_url"].as_str(),
            Some("https://office.invalid/v1")
        );
        let platforms = root[key("platforms")].as_mapping().unwrap();
        assert_eq!(platforms[key("discord")]["enabled"].as_bool(), Some(true));
        assert_eq!(
            platforms[key("discord")]["require_mention"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn legacy_sections_extend_native_provider_and_switch_settings() {
        let source = r#"
model: {default: plain-model, persist_switch_by_default: true}
model_switch: {future_option: keep}
llm_providers:
  office: {model: office-default}
  lab: {model: lab-default}
custom_providers:
  - {name: office, base_url: 'https://office.invalid/v1'}
providers:
  office: {request_timeout_seconds: 12}
"#;
        let expected = r#"
model: plain-model
model_switch: {future_option: keep, persist_switch_by_default: true}
llm_providers:
  office: {model: office-default, base_url: 'https://office.invalid/v1', request_timeout_seconds: 12}
  lab: {model: lab-default}
"#;
        let mut root: Mapping = serde_yaml::from_str(source).unwrap();
        normalize_config_yaml_root(&mut root);

        assert_eq!(root, serde_yaml::from_str::<Mapping>(expected).unwrap());
    }

    #[test]
    fn legacy_provider_scalar_preserves_native_provider_metadata() {
        let raw = r#"
llm_providers:
  office: {model: office-default, base_url: 'https://office.invalid/v1'}
providers:
  office: false
"#;
        let cfg = parse_fixture(raw);
        let office = &cfg.llm_providers["office"];
        assert_eq!(office.model.as_deref(), Some("office-default"));
        assert_eq!(
            office.base_url.as_deref(),
            Some("https://office.invalid/v1")
        );
    }

    #[test]
    fn legacy_unknown_reset_mode_and_model_value_survive_normalisation() {
        let raw = r#"
model: true
session_reset: {mode: future}
session: {max_context_messages: 40}
"#;
        let expected: Mapping = serde_yaml::from_str(raw).unwrap();
        let mut root = expected.clone();
        normalize_config_yaml_root(&mut root);

        assert_eq!(root, expected);
    }

    #[test]
    fn python_provider_only_model_preserves_scalar_switch_value() {
        let raw = r#"
model:
  provider: 'custom:office'
  persist_switch_by_default: false
model_switch: legacy
"#;
        let expected = r#"
model: 'office:'
model_switch:
  value: legacy
  persist_switch_by_default: false
"#;
        let mut root: Mapping = serde_yaml::from_str(raw).unwrap();
        normalize_config_yaml_root(&mut root);
        assert_eq!(root, serde_yaml::from_str::<Mapping>(expected).unwrap());
    }

    #[test]
    fn python_model_without_route_keeps_mapping_and_lifts_switch_flag() {
        let raw = r#"
model:
  default: { provider: office }
  persist_switch_by_default: true
"#;
        let mut root: Mapping = serde_yaml::from_str(raw).unwrap();
        let original_model = root[key("model")].clone();
        normalize_config_yaml_root(&mut root);
        assert_eq!(root[key("model")], original_model);
        assert_eq!(root[key("model_switch")]["persist_switch_by_default"], true);
        assert!(!root.contains_key(key("llm_providers")));
    }

    #[test]
    fn python_model_block_to_string_and_llm() {
        let raw = r#"
model:
  default: z-ai/glm-5.1
  provider: openrouter
  base_url: https://openrouter.ai/api/v1
max_turns: 99
"#;
        let cfg = parse_fixture(raw);
        assert_eq!(cfg.model.as_deref(), Some("openrouter:z-ai/glm-5.1"));
        assert!(cfg.model_switch.persist_switch_by_default);
        assert_eq!(cfg.max_turns, 99);
        let or = cfg.llm_providers.get("openrouter").expect("openrouter");
        assert_eq!(or.base_url.as_deref(), Some("https://openrouter.ai/api/v1"));
    }

    #[test]
    fn python_model_block_lifts_persist_default_flag() {
        let raw = r#"
model:
  default: zai/glm-5.2
  provider: openrouter
  persist_switch_by_default: "false"
"#;
        let cfg = parse_fixture(raw);
        assert_eq!(cfg.model.as_deref(), Some("openrouter:zai/glm-5.2"));
        assert!(!cfg.model_switch.persist_switch_by_default);
    }

    #[test]
    fn python_model_block_accepts_name_and_model_aliases_for_default() {
        for (field, expected) in [
            ("name", "openrouter:zai/glm-5.2"),
            ("model", "openrouter:anthropic/claude-sonnet-4.6"),
        ] {
            let raw = format!(
                r#"
model:
  {field}: {model}
  provider: openrouter
"#,
                model = expected.split_once(':').unwrap().1
            );
            let cfg = parse_fixture(&raw);
            assert_eq!(cfg.model.as_deref(), Some(expected));
        }
    }

    #[test]
    fn python_model_block_prefers_default_over_aliases() {
        let raw = r#"
model:
  default: canonical/model
  name: stale/name
  model: stale/model
  provider: openrouter
"#;
        let cfg = parse_fixture(raw);
        assert_eq!(cfg.model.as_deref(), Some("openrouter:canonical/model"));
    }

    #[test]
    fn python_model_block_keeps_the_provider_nested_in_a_mapping_default() {
        for (model_block, expected) in [
            // The nested provider stays with its model instead of the outer `auto`.
            (
                "default: {provider: anthropic, model: claude-sonnet-4-6}\n  provider: auto",
                "anthropic:claude-sonnet-4-6",
            ),
            (
                "default: {provider: anthropic, default: claude-sonnet-4-6}",
                "anthropic:claude-sonnet-4-6",
            ),
            (
                "model: {provider: deepseek, model: deepseek-chat}\n  provider: openrouter",
                "deepseek:deepseek-chat",
            ),
            // A nested default without a provider keeps the outer one.
            (
                "default: {model: z-ai/glm-5.1}\n  provider: openrouter",
                "openrouter:z-ai/glm-5.1",
            ),
            // An empty mapping names nothing, so the next alias is used.
            (
                "default: {}\n  model: z-ai/glm-5.1\n  provider: openrouter",
                "openrouter:z-ai/glm-5.1",
            ),
            // So is a mapping that names only a provider, which is dropped with it.
            (
                "default: {provider: anthropic}\n  name: z-ai/glm-5.1\n  provider: openrouter",
                "openrouter:z-ai/glm-5.1",
            ),
        ] {
            let raw = format!("model:\n  {model_block}\n");
            let cfg = parse_fixture(&raw);
            assert_eq!(cfg.model.as_deref(), Some(expected), "{raw}");
        }

        // The outer provider keeps the credentials written beside it.
        let raw = "model:\n  default: {provider: anthropic, model: claude-sonnet-4-6}\n  \
                   provider: openrouter\n  api_key: sk-outer\n";
        let cfg = parse_fixture(raw);
        assert_eq!(cfg.model.as_deref(), Some("anthropic:claude-sonnet-4-6"));
        assert_eq!(
            cfg.llm_providers
                .get("openrouter")
                .and_then(|provider| provider.api_key.as_deref()),
            Some("sk-outer")
        );
        assert!(!cfg.llm_providers.contains_key("anthropic"));

        // With no outer provider they go to the nested one.
        let raw = "model:\n  default: {provider: deepseek, model: deepseek-chat}\n  \
                   base_url: https://api.deepseek.com/\n";
        let cfg = parse_fixture(raw);
        assert_eq!(cfg.model.as_deref(), Some("deepseek:deepseek-chat"));
        assert_eq!(
            cfg.llm_providers
                .get("deepseek")
                .and_then(|provider| provider.base_url.as_deref()),
            Some("https://api.deepseek.com")
        );
    }

    #[test]
    fn python_model_block_lifts_provider_credentials() {
        let raw = r#"
model:
  default: deepseek-chat
  provider: deepseek
  base_url: https://api.deepseek.com/
  api_key: sk-local
  api_key_env: DEEPSEEK_API_KEY
"#;
        let cfg = parse_fixture(raw);

        assert_eq!(cfg.model.as_deref(), Some("deepseek:deepseek-chat"));
        let provider = cfg.llm_providers.get("deepseek").expect("deepseek");
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.deepseek.com")
        );
        assert_eq!(provider.api_key.as_deref(), Some("sk-local"));
        assert_eq!(provider.api_key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
    }

    /// Upstream's `providers:` entries name the endpoint `api` or `url` and the key's
    /// variable `key_env` (`hermes_cli/config_providers.py`); Hermes Desktop's own tests
    /// write that shape.
    #[test]
    fn python_providers_entry_reads_api_url_and_key_env() {
        let raw = r#"
model:
  default: mock-model
  provider: mock
providers:
  mock:
    api: http://127.0.0.1:40107/v1/
    key_env: MOCK_API_KEY
  byurl:
    url: http://127.0.0.1:40108/v1
    api: http://ignored.example/v1
  both:
    base_url: http://127.0.0.1:40109/v1
    api: http://ignored.example/v1
    key_env: FIRST_KEY
    api_key_env: SECOND_KEY
"#;
        let cfg = parse_fixture(raw);

        assert_eq!(cfg.model.as_deref(), Some("mock:mock-model"));
        let mock = cfg.llm_providers.get("mock").expect("mock provider");
        assert_eq!(mock.base_url.as_deref(), Some("http://127.0.0.1:40107/v1"));
        assert_eq!(mock.api_key_env.as_deref(), Some("MOCK_API_KEY"));
        let byurl = cfg.llm_providers.get("byurl").expect("byurl provider");
        assert_eq!(byurl.base_url.as_deref(), Some("http://127.0.0.1:40108/v1"));
        // Upstream's order: base_url, url, api; key_env before api_key_env.
        let both = cfg.llm_providers.get("both").expect("both provider");
        assert_eq!(both.base_url.as_deref(), Some("http://127.0.0.1:40109/v1"));
        assert_eq!(both.api_key_env.as_deref(), Some("FIRST_KEY"));
    }

    #[test]
    fn python_providers_replace_scalar_root_and_skip_empty_names() {
        let raw = r#"
llm_providers: legacy
providers:
  7: { base_url: http://127.0.0.1:8123/v1 }
  '': { base_url: http://127.0.0.1:8124/v1 }
  office:
    url: ' '
    api: ' http://127.0.0.1:8125/v1/ '
"#;
        let mut root: Mapping = serde_yaml::from_str(raw).unwrap();
        normalize_config_yaml_root(&mut root);
        assert!(!root.contains_key(key("providers")));
        let cfg: crate::config::GatewayConfig =
            serde_yaml::from_value(Value::Mapping(root)).unwrap();
        assert_eq!(cfg.llm_providers.len(), 1);
        assert_eq!(
            cfg.llm_providers["office"].base_url.as_deref(),
            Some("http://127.0.0.1:8125/v1"),
        );
    }

    #[test]
    fn python_custom_providers_merge_into_llm_provider_config() {
        let raw = r#"
model:
  default: my-model
  provider: custom:beans
custom_providers:
  - name: beans
    base_url: http://beans.local/v1/
    api_key: sk-beans
    api_key_env: BEANS_API_KEY
    model: fallback-beans-model
    discover_models: "false"
    models:
      kimi-k2.5: {}
      glm-5: {}
    api_mode: codex_responses
  - name: local
    base_url: http://localhost:8080/v1/
"#;
        let cfg = parse_fixture(raw);

        assert_eq!(cfg.model.as_deref(), Some("beans:my-model"));
        let beans = cfg.llm_providers.get("beans").expect("beans provider");
        assert_eq!(beans.base_url.as_deref(), Some("http://beans.local/v1"));
        assert_eq!(beans.api_key.as_deref(), Some("sk-beans"));
        assert_eq!(beans.api_key_env.as_deref(), Some("BEANS_API_KEY"));
        assert_eq!(beans.model.as_deref(), Some("fallback-beans-model"));
        assert!(!beans.discover_models);
        assert_eq!(beans.models, vec!["kimi-k2.5", "glm-5"]);
        assert_eq!(beans.api_mode.as_deref(), Some("codex_responses"));

        let local = cfg.llm_providers.get("local").expect("local provider");
        assert_eq!(local.base_url.as_deref(), Some("http://localhost:8080/v1"));
        assert!(local.api_key.is_none());
    }

    #[test]
    fn toolsets_lifted_to_tools() {
        let raw = r#"
toolsets:
  - hermes-cli
  - web
"#;
        let cfg = parse_fixture(raw);
        assert_eq!(cfg.tools, vec!["hermes-cli", "web"]);
    }

    #[test]
    fn null_top_level_config_values_fall_back_to_defaults() {
        let raw = r#"
model: null
personality: null
max_turns: null
tools: null
platforms: null
platform_toolsets: null
llm_providers: null
display: null
terminal: null
web: null
sessions: null
agent: null
personalities: null
42: null
"#;
        let mut root = serde_yaml::from_str(raw).expect("YAML fixture must be a mapping");
        normalize_config_yaml_root(&mut root);
        assert!(!root.contains_key(key("personalities")));
        assert!(!root.contains_key(Value::from(42)));
        let cfg: crate::config::GatewayConfig =
            serde_yaml::from_value(Value::Mapping(root)).unwrap();
        let default = crate::config::GatewayConfig::default();
        assert_eq!(cfg.model, default.model);
        assert_eq!(cfg.personality, default.personality);
        assert_eq!(cfg.max_turns, default.max_turns);
        assert_eq!(cfg.tools, default.tools);
        assert_eq!(cfg.display, default.display);
        assert_eq!(cfg.terminal, default.terminal);
        assert_eq!(cfg.web, default.web);
        assert_eq!(cfg.sessions, default.sessions);
        assert_eq!(cfg.agent, default.agent);
    }

    #[test]
    fn root_discord_lifted_under_platforms() {
        let raw = r#"
discord:
  require_mention: true
  free_response_channels: ""
  auto_thread: true
"#;
        let cfg = parse_fixture(raw);
        let d = cfg.platforms.get("discord").expect("discord");
        assert_eq!(d.require_mention, Some(true));
        assert!(d.extra.contains_key("free_response_channels"));
        assert!(d.extra.contains_key("auto_thread"));
    }

    #[test]
    fn root_platform_typing_indicator_lifted_under_platforms() {
        let raw = r#"
slack:
  enabled: true
  typing_indicator: false
"#;
        let cfg = parse_fixture(raw);
        let slack = cfg.platforms.get("slack").expect("slack");
        assert!(slack.enabled);
        assert!(!slack.typing_indicator_enabled());
    }

    #[test]
    fn session_reset_both_to_session() {
        let raw = r#"
session_reset:
  mode: both
  idle_minutes: 1440
  at_hour: 4
"#;
        let cfg = parse_fixture(raw);
        match cfg.session.reset_policy {
            crate::session::SessionResetPolicy::Both { daily, idle } => {
                assert_eq!(daily.at_hour, 4);
                assert_eq!(idle.timeout_minutes, 1440);
            }
            other => panic!("expected Both, got {:?}", other),
        }
    }

    #[test]
    fn legacy_session_reset_modes_preserve_other_session_settings() {
        use crate::session::SessionResetPolicy;

        let cases = [
            (
                "{mode: DAILY, at_hour: 26}",
                SessionResetPolicy::Daily { at_hour: 23 },
            ),
            ("{mode: daily}", SessionResetPolicy::Daily { at_hour: 0 }),
            (
                "{mode: idle, idle_minutes: 45}",
                SessionResetPolicy::Idle {
                    timeout_minutes: 45,
                },
            ),
            (
                "{mode: idle}",
                SessionResetPolicy::Idle {
                    timeout_minutes: 30,
                },
            ),
            ("{mode: none}", SessionResetPolicy::None),
        ];
        for (reset, expected) in cases {
            let mut root: Mapping =
                serde_yaml::from_str(&format!("session_reset: {reset}\nsession: {{max_context_messages: 40, compression_enabled: true}}\n")).unwrap();
            normalize_config_yaml_root(&mut root);
            assert!(!root.contains_key(key("session_reset")));
            let config: crate::config::GatewayConfig =
                serde_yaml::from_value(Value::Mapping(root)).unwrap();
            assert_eq!(config.session.reset_policy, expected, "legacy {reset}");
            assert_eq!(config.session.max_context_messages, Some(40));
            assert!(config.session.compression_enabled);
        }
    }

    #[test]
    fn platform_toolsets_numeric_entries_normalized_to_strings() {
        let raw = r#"
platform_toolsets:
  cli:
    - hermes-cli
    - 12306
    - true
  cron: 42
"#;
        let cfg = parse_fixture(raw);

        let cli = cfg.platform_toolsets.get("cli").expect("cli");
        assert!(cli.contains(&"hermes-cli".to_string()));
        assert!(cli.contains(&"12306".to_string()));
        assert!(cli.contains(&"true".to_string()));

        let cron = cfg.platform_toolsets.get("cron").expect("cron");
        assert_eq!(cron, &vec!["42".to_string()]);
    }

    fn load(raw: &str) -> crate::config::GatewayConfig {
        let mut cfg = parse_fixture(raw);
        drop_written_default_platform_toolsets(&mut cfg.platform_toolsets);
        cfg
    }

    #[test]
    fn platform_toolsets_hold_only_the_entries_the_config_sets() {
        assert!(load("model: x\n").platform_toolsets.is_empty());
        assert!(crate::config::GatewayConfig::default()
            .platform_toolsets
            .is_empty());
        let saved = serde_yaml::to_string(&crate::config::GatewayConfig::default()).unwrap();
        assert!(!saved.contains("platform_toolsets"), "{saved}");

        let cfg = load("platform_toolsets:\n  cli: []\n  cron: null\n  acp:\n    - hermes-acp\n");
        assert_eq!(cfg.platform_toolsets.get("cli"), Some(&Vec::new()));
        assert_eq!(cfg.platform_toolsets.get("cron"), None);
        assert_eq!(
            cfg.platform_toolsets.get("acp"),
            Some(&vec!["hermes-acp".to_string()])
        );
        assert_eq!(cfg.platform_toolsets.len(), 2);
    }

    #[test]
    fn a_map_carrying_every_default_entry_keeps_only_the_changed_ones() {
        // Earlier releases wrote every default entry into config.yaml.
        let mut raw = String::from("platform_toolsets:\n");
        for (platform, toolsets) in crate::config::default_platform_toolsets() {
            let toolsets = match platform.as_str() {
                "telegram" => vec!["web".to_string()],
                _ => toolsets,
            };
            raw.push_str(&format!("  {platform}: [{}]\n", toolsets.join(", ")));
        }
        raw.push_str("  matrix: [hermes-cli]\n");
        let cfg = load(&raw);

        let mut kept: Vec<_> = cfg.platform_toolsets.into_iter().collect();
        kept.sort();
        assert_eq!(
            kept,
            [
                ("matrix".to_string(), vec!["hermes-cli".to_string()]),
                ("telegram".to_string(), vec!["web".to_string()]),
            ]
        );

        // The first release to write them wrote 5.
        let april = "platform_toolsets:\n  cli: [hermes-cli]\n  telegram: [hermes-telegram]\n  \
                     discord: [hermes-discord]\n  whatsapp: [hermes-whatsapp]\n  slack: [hermes-slack]\n";
        assert!(load(april).platform_toolsets.is_empty());

        // Without all 5, an entry equal to its default was the user's.
        let chosen =
            load("platform_toolsets:\n  cli: [hermes-cli]\n  telegram: [hermes-telegram]\n");
        assert_eq!(chosen.platform_toolsets.len(), 2);
    }

    #[test]
    fn legacy_route_lists_skip_empty_entries_and_keep_explicit_model_prefixes() {
        let raw = r#"
model: { provider: fixture }
custom_providers:
  - ignored
  - { base_url: "https://unused.invalid/v1" }
  - { name: fixture, base_url: " https://first.invalid/v1/ " }
fallback_models:
  - "primary:stable"
  - "  "
fallback_providers:
  - - { provider: fixture, model: "secondary:routed", base_url: "https://second.invalid/v1/" }
    - { provider: ignored }
    - { model: "standalone" }
    - false
  - "PRIMARY:STABLE"
fallback_model: "legacy:final"
platform_toolsets:
  " ": discarded
  " cli ": [" tools ", "", null, { invalid: true }]
  cron: { invalid: true }
"#;
        let mut root: Mapping = serde_yaml::from_str(raw).unwrap();
        normalize_config_yaml_root(&mut root);
        let cfg: crate::config::GatewayConfig =
            serde_yaml::from_value(Value::Mapping(root)).unwrap();

        assert_eq!(cfg.model.as_deref(), Some("fixture:"));
        assert_eq!(
            cfg.fallback_models,
            [
                "primary:stable",
                "secondary:routed",
                "standalone",
                "legacy:final"
            ]
        );
        assert_eq!(cfg.fallback_model.as_deref(), Some("primary:stable"));
        assert_eq!(
            cfg.llm_providers["fixture"].base_url.as_deref(),
            Some("https://second.invalid/v1")
        );
        assert!(!cfg.llm_providers.contains_key("ignored"));
        assert_eq!(cfg.platform_toolsets["cli"], ["tools"]);
        assert!(cfg.platform_toolsets["cron"].is_empty());
        assert!(!cfg.platform_toolsets.contains_key(" "));
    }

    #[test]
    fn fallback_providers_and_legacy_model_are_merged() {
        let raw = r#"
fallback_providers:
  - provider: openrouter
    model: anthropic/claude-sonnet-4.6
    base_url: https://openrouter.ai/api/v1/
  - provider: openrouter
    model: anthropic/claude-sonnet-4.6
fallback_model:
  provider: nous
  model: Hermes-4
  api_key_env: NOUS_API_KEY
"#;
        let cfg = parse_fixture(raw);

        assert_eq!(
            cfg.fallback_models,
            vec![
                "openrouter:anthropic/claude-sonnet-4.6".to_string(),
                "nous:Hermes-4".to_string()
            ]
        );
        assert_eq!(
            cfg.fallback_model.as_deref(),
            Some("openrouter:anthropic/claude-sonnet-4.6")
        );
        assert_eq!(
            cfg.llm_providers
                .get("openrouter")
                .and_then(|p| p.base_url.as_deref()),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(
            cfg.llm_providers
                .get("nous")
                .and_then(|p| p.api_key_env.as_deref()),
            Some("NOUS_API_KEY")
        );
    }
}
