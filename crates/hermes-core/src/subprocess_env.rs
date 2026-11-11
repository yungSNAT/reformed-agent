//! Credential-safe environment construction for spawned subprocesses.
//!
//! Terminal execution paths may opt specific keys through a configured
//! passthrough. Non-terminal helper processes should use this module so they do
//! not blindly inherit high-value operator credentials.

use std::collections::{BTreeMap, BTreeSet};

pub const SUBPROCESS_ENV_FORCE_PREFIX: &str = "_HERMES_FORCE_";
pub const SUBPROCESS_ENV_PASSTHROUGH_VAR: &str = "HERMES_SUBPROCESS_ENV_PASSTHROUGH";

pub fn normalize_env_passthrough_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn parse_subprocess_env_passthrough(value: &str) -> BTreeSet<String> {
    value
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ':' | ';'))
        .filter_map(normalize_env_passthrough_name)
        .collect()
}

/// `hermes_subprocess_env(false)` plus the keys in `passthrough` (the
/// configured and session lists) and in `HERMES_SUBPROCESS_ENV_PASSTHROUGH`,
/// and the `_HERMES_FORCE_<KEY>` overrides, as the terminal tool applies them.
/// For model-facing processes outside the terminal tool, such as `execute_code`.
pub fn hermes_subprocess_env_with_passthrough(
    passthrough: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    let mut env = hermes_subprocess_env(false);
    let mut keys = passthrough.clone();
    if let Ok(raw) = std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR) {
        keys.extend(parse_subprocess_env_passthrough(&raw));
    }
    for key in keys {
        if let Ok(value) = std::env::var(&key) {
            env.insert(key, value);
        }
    }
    for (key, value) in std::env::vars_os() {
        let (Some(key), Some(value)) = (key.to_str(), value.to_str()) else {
            continue;
        };
        if let Some(target) = key.strip_prefix(SUBPROCESS_ENV_FORCE_PREFIX) {
            if !target.is_empty() && should_strip_subprocess_env_key(target, false) {
                env.insert(target.to_string(), value.to_string());
            }
        }
    }
    env
}

/// Tier-1 secrets stripped from every managed subprocess, even when provider
/// credentials are intentionally inherited for a model-driving child.
pub const SUBPROCESS_ALWAYS_STRIP_KEYS: &[&str] = &[
    "DAYTONA_API_KEY",
    "DISCORD_BOT_TOKEN",
    "EMAIL_PASSWORD",
    "GATEWAY_ALLOW_ALL_USERS",
    "GATEWAY_ALLOWED_USERS",
    "GH_TOKEN",
    "GITHUB_APP_ID",
    "GITHUB_APP_INSTALLATION_ID",
    "GITHUB_APP_PRIVATE_KEY_PATH",
    "GITHUB_TOKEN",
    "HASS_TOKEN",
    "HERMES_DASHBOARD_SESSION_TOKEN",
    "HERMES_POLICY_ADMIN_TOKEN",
    "MODAL_TOKEN_ID",
    "MODAL_TOKEN_SECRET",
    "SLACK_APP_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_SIGNING_SECRET",
    "TELEGRAM_BOT_TOKEN",
];

/// Credential-like env vars stripped by default. Callers that legitimately
/// spawn a model-driving child can opt into retaining these while Tier-1 keys
/// remain stripped.
pub const SUBPROCESS_ENV_BLOCKLIST_EXACT: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "BROWSERBASE_PROJECT_ID",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "COHERE_API_KEY",
    "DAYTONA_API_KEY",
    "DEEPSEEK_API_KEY",
    "DISCORD_BOT_TOKEN",
    "DISCORD_FREE_RESPONSE_CHANNELS",
    "DISCORD_HOME_CHANNEL",
    "DISCORD_HOME_CHANNEL_NAME",
    "DISCORD_REQUIRE_MENTION",
    "EMAIL_ADDRESS",
    "EMAIL_HOME_ADDRESS",
    "EMAIL_HOME_ADDRESS_NAME",
    "EMAIL_IMAP_HOST",
    "EMAIL_PASSWORD",
    "EMAIL_SMTP_HOST",
    "ELEVENLABS_API_KEY",
    "FIRECRAWL_API_KEY",
    "FIREWORKS_API_KEY",
    "GATEWAY_ALLOW_ALL_USERS",
    "GATEWAY_ALLOWED_USERS",
    "GH_TOKEN",
    "GITHUB_APP_ID",
    "GITHUB_APP_INSTALLATION_ID",
    "GITHUB_APP_PRIVATE_KEY_PATH",
    "GITHUB_TOKEN",
    "GLM_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GROQ_API_KEY",
    "HASS_TOKEN",
    "HASS_URL",
    "HELICONE_API_KEY",
    "HERMES_DASHBOARD_SESSION_TOKEN",
    "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
    "HERMES_OPENAI_API_KEY",
    "HERMES_POLICY_ADMIN_TOKEN",
    "KIMI_API_KEY",
    "LLM_MODEL",
    "MINIMAX_API_KEY",
    "MINIMAX_CN_API_KEY",
    "MISTRAL_API_KEY",
    "MODAL_TOKEN_ID",
    "MODAL_TOKEN_SECRET",
    "NVIDIA_API_KEY",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENROUTER_API_KEY",
    "PERPLEXITY_API_KEY",
    "SIGNAL_ACCOUNT",
    "SIGNAL_ALLOWED_USERS",
    "SIGNAL_GROUP_ALLOWED_USERS",
    "SIGNAL_HOME_CHANNEL",
    "SIGNAL_HOME_CHANNEL_NAME",
    "SIGNAL_HTTP_URL",
    "SIGNAL_IGNORE_STORIES",
    "SLACK_ALLOWED_USERS",
    "SLACK_APP_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_HOME_CHANNEL",
    "SLACK_HOME_CHANNEL_NAME",
    "SLACK_SIGNING_SECRET",
    "TELEGRAM_BOT_TOKEN",
    "TELEGRAM_HOME_CHANNEL",
    "TELEGRAM_HOME_CHANNEL_NAME",
    "TOGETHER_API_KEY",
    "VERTEX_CREDENTIALS_PATH",
    "WHATSAPP_ALLOWED_USERS",
    "WHATSAPP_ENABLED",
    "WHATSAPP_MODE",
    "XAI_API_KEY",
    "ZAI_API_KEY",
    "Z_AI_API_KEY",
];

pub const SUBPROCESS_ENV_BLOCKLIST_PREFIXES: &[&str] = &[
    "TOOL_GATEWAY_",
    "HERMES_MANAGED_TOOL_GATEWAY_",
    "HERMES_GATEWAY_",
    "HERMES_HTTP_",
];

#[cfg(unix)]
const SANE_PATH_ENTRIES: &[&str] = &[
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
];

pub fn should_strip_subprocess_env_key(key: &str, inherit_credentials: bool) -> bool {
    SUBPROCESS_ALWAYS_STRIP_KEYS.contains(&key)
        || SUBPROCESS_ENV_BLOCKLIST_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
        || (!inherit_credentials && SUBPROCESS_ENV_BLOCKLIST_EXACT.contains(&key))
        || key.starts_with(SUBPROCESS_ENV_FORCE_PREFIX)
        || key == SUBPROCESS_ENV_PASSTHROUGH_VAR
}

#[cfg(unix)]
pub fn normalize_subprocess_path(path: Option<&str>) -> String {
    let Some(path) = path.filter(|value| !value.trim().is_empty()) else {
        return SANE_PATH_ENTRIES.join(":");
    };
    if std::env::split_paths(path).any(|entry| entry == std::path::Path::new("/usr/bin")) {
        return path.to_string();
    }

    let mut entries: Vec<String> = std::env::split_paths(path)
        .map(|entry| entry.to_string_lossy().to_string())
        .filter(|entry| !entry.is_empty())
        .collect();
    for sane in SANE_PATH_ENTRIES {
        if !entries.iter().any(|entry| entry == sane) {
            entries.push((*sane).to_string());
        }
    }
    entries.join(":")
}

pub fn hermes_subprocess_env(inherit_credentials: bool) -> BTreeMap<String, String> {
    hermes_subprocess_env_from(std::env::vars(), inherit_credentials)
}

pub fn hermes_subprocess_env_from<I, K, V>(
    vars: I,
    inherit_credentials: bool,
) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let mut env = BTreeMap::new();
    for (key, value) in vars {
        let key = key.into();
        if should_strip_subprocess_env_key(&key, inherit_credentials) {
            continue;
        }
        env.insert(key, value.into());
    }
    // Windows keeps the inherited PATH. The sane entries are POSIX
    // directories (`/usr/bin` would search `C:\usr\bin`), and a `:`-joined
    // list hides every Windows directory from the child.
    #[cfg(unix)]
    {
        let path = normalize_subprocess_path(env.get("PATH").map(String::as_str));
        env.insert("PATH".to_string(), path);
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(vars: &[(&str, &str)], inherit_credentials: bool) -> BTreeMap<String, String> {
        hermes_subprocess_env_from(
            vars.iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
            inherit_credentials,
        )
    }

    #[test]
    fn provider_keys_are_stripped_by_default() {
        let env = build(
            &[
                ("PATH", "/custom/bin"),
                ("HOME", "/home/user"),
                ("OPENAI_API_KEY", "sk-test"),
                ("HERMES_OPENAI_API_KEY", "sk-hermes"),
                ("ANTHROPIC_API_KEY", "ant-test"),
                ("SAFE_VAR", "keep"),
            ],
            false,
        );
        assert_eq!(env.get("SAFE_VAR").map(String::as_str), Some("keep"));
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("HERMES_OPENAI_API_KEY"));
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn vertex_credential_paths_are_stripped_by_default() {
        let env = build(
            &[
                ("VERTEX_CREDENTIALS_PATH", "/secrets/sa.json"),
                ("GOOGLE_APPLICATION_CREDENTIALS", "/secrets/sa.json"),
                ("VERTEX_REGION", "global"),
            ],
            false,
        );
        assert!(!env.contains_key("VERTEX_CREDENTIALS_PATH"));
        assert!(!env.contains_key("GOOGLE_APPLICATION_CREDENTIALS"));
        assert_eq!(env.get("VERTEX_REGION").map(String::as_str), Some("global"));
    }

    #[test]
    fn provider_keys_can_be_inherited_but_tier1_stays_stripped() {
        let env = build(
            &[
                ("OPENAI_API_KEY", "sk-test"),
                ("ANTHROPIC_API_KEY", "ant-test"),
                ("GH_TOKEN", "ghp-secret"),
                ("TELEGRAM_BOT_TOKEN", "bot-secret"),
                ("HERMES_GATEWAY_TOKEN", "gateway-secret"),
            ],
            true,
        );
        assert_eq!(
            env.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-test")
        );
        assert_eq!(
            env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("ant-test")
        );
        assert!(!env.contains_key("GH_TOKEN"));
        assert!(!env.contains_key("TELEGRAM_BOT_TOKEN"));
        assert!(!env.contains_key("HERMES_GATEWAY_TOKEN"));
    }

    #[test]
    fn force_and_passthrough_control_vars_never_leak() {
        let env = build(
            &[
                ("_HERMES_FORCE_OPENAI_API_KEY", "sk-force"),
                ("HERMES_SUBPROCESS_ENV_PASSTHROUGH", "OPENAI_API_KEY"),
            ],
            true,
        );
        assert!(!env.contains_key("_HERMES_FORCE_OPENAI_API_KEY"));
        assert!(!env.contains_key("HERMES_SUBPROCESS_ENV_PASSTHROUGH"));
    }

    #[cfg(unix)]
    #[test]
    fn path_keeps_its_entries_and_gains_the_sane_ones() {
        let env = build(&[("PATH", "/custom/bin")], false);
        let path = env.get("PATH").expect("PATH is always set");
        assert!(path.starts_with("/custom/bin:"), "{path}");
        assert!(path.split(':').any(|entry| entry == "/usr/bin"), "{path}");

        let env = build(&[("HOME", "/home/user")], false);
        assert_eq!(
            env.get("PATH").map(String::as_str),
            Some(SANE_PATH_ENTRIES.join(":").as_str())
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_is_inherited_unchanged_under_either_spelling() {
        let inherited = r"C:\Windows\System32;C:\Program Files\Git\cmd";
        for name in ["PATH", "Path"] {
            let env = build(&[(name, inherited)], false);
            let paths: Vec<(&str, &str)> = env
                .iter()
                .filter(|(key, _)| key.eq_ignore_ascii_case("PATH"))
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            assert_eq!(paths, [(name, inherited)]);
        }
    }
}
