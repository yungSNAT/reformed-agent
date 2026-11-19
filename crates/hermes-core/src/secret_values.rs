//! The secret values this process holds, matched exactly.
//!
//! Pattern redaction finds text shaped like a credential, so a secret with no
//! known shape passes it. This module knows the values themselves: those of
//! environment variables with credential names, where the active profile's
//! `.env`, a vault hydration or the shell put them, and those registered when
//! the configuration loads. Redaction masks them, and the tool registry refuses
//! a call whose arguments carry one.
//!
//! shortcut: only the exact text is matched. A copy that was encoded (base64,
//! percent-encoding of reserved characters) or split across arguments is not
//! found; catching those needs a decoder per encoding. Tokens held only in the
//! auth stores (the token store, credential pool and OAuth files) are not
//! known here; pattern redaction alone covers them.
//!
//! Other credential-name rules serve other purposes: the import filter in
//! `hermes-cli` (`agent_import.rs`), the gateway's agent cache key and the
//! redactor's key sets in `hermes-intelligence`.

use std::borrow::Cow;
use std::sync::{LazyLock, PoisonError, RwLock};

use serde_json::Value;

/// Shorter values are never treated as secrets: masking or refusing a short
/// common string would break ordinary text more often than it caught a leak.
const MIN_SECRET_CHARS: usize = 8;

/// A value of only letters must be at least this long: a word such as
/// `required` or `disabled` under a credential-named key is a setting.
const MIN_LETTERS_ONLY_CHARS: usize = 16;

/// Keys that local OpenAI-compatible servers document as throwaway values,
/// compared without case. They appear in ordinary paths and commands.
const PLACEHOLDER_VALUES: &[&str] = &[
    "not-needed",
    "lm-studio",
    "no-key-required",
    "sk-no-key-required",
    "dummy-key",
];

/// What masked text shows in place of a secret value.
pub const MASK: &str = "***";

/// Values registered from loaded configuration, with the label each is
/// reported under.
static REGISTERED: LazyLock<RwLock<Vec<(String, String)>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Whether a variable or field name names a credential. Words are split on
/// anything but ASCII letters and digits; case is ignored. A name counts when
/// a word ends with `KEY`, `TOKEN`, `SECRET` or `PASSWORD` (`APIKEY`,
/// `PGPASSWORD`), or is `PASSWD`, `PASSPHRASE`, `CREDENTIAL`, `CREDENTIALS` or
/// `PAT` (a personal access token), or the last word is `PASS`, `AUTH`,
/// `AUTHORIZATION`, `COOKIE` or, after another word, `PWD`. It does not count
/// when the last word is `PATH`, `FILE`, `DIR`, `ENV`, `URL`, `ID` or
/// `ENDPOINT`, which name where a secret is kept or which one it is, or for
/// names that hold identifiers: Git's indexed `GIT_CONFIG_KEY_<n>` option
/// names and `HERMES_SESSION_KEY`, the gateway session.
pub fn is_secret_name(name: &str) -> bool {
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    let Some(last) = words.last() else {
        return false;
    };
    let joined = words.join("_");
    let names_an_identifier = joined == "HERMES_SESSION_KEY"
        || joined
            .strip_prefix("GIT_CONFIG_KEY_")
            .is_some_and(|index| index.bytes().all(|b| b.is_ascii_digit()));
    if names_an_identifier
        || matches!(
            last.as_str(),
            "PATH" | "FILE" | "DIR" | "ENV" | "URL" | "ID" | "ENDPOINT"
        )
    {
        return false;
    }
    matches!(last.as_str(), "PASS" | "AUTH" | "AUTHORIZATION" | "COOKIE")
        || (last == "PWD" && words.len() > 1)
        || words.iter().any(|word| {
            matches!(
                word.as_str(),
                "PASSWD" | "PASSPHRASE" | "CREDENTIAL" | "CREDENTIALS" | "PAT"
            ) || ["KEY", "TOKEN", "SECRET", "PASSWORD"]
                .iter()
                .any(|suffix| word.ends_with(suffix))
        })
}

/// Whether a value under a credential name is worth matching: long enough,
/// not only a short word, not a documented placeholder, and not a path or a
/// `${VAR}` reference.
fn is_secret_value(value: &str) -> bool {
    let chars = value.chars().count();
    let letters_only = value.chars().all(|c| c.is_ascii_alphabetic());
    let bytes = value.as_bytes();
    let is_path = value.starts_with(['/', '~'])
        || value.starts_with("./")
        || value.starts_with("../")
        || (bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'));
    chars >= MIN_SECRET_CHARS
        && (!letters_only || chars >= MIN_LETTERS_ONLY_CHARS)
        && !is_path
        && !value.starts_with("${")
        && !PLACEHOLDER_VALUES
            .iter()
            .any(|placeholder| value.eq_ignore_ascii_case(placeholder))
}

/// Adds `value` to `known` as reported by `label`, with the credential of a
/// `Bearer` or `Basic` value without its scheme too, since a model would send
/// the token alone. Values too short or plain to be secrets, and values
/// already known, are skipped.
fn add_value(known: &mut Vec<(String, String)>, label: &str, value: &str) {
    let value = value.trim();
    let bare = ["Bearer ", "Basic "].iter().find_map(|scheme| {
        value
            .get(..scheme.len())
            .filter(|head| head.eq_ignore_ascii_case(scheme))
            .map(|_| value[scheme.len()..].trim())
    });
    for form in std::iter::once(value).chain(bare) {
        if is_secret_value(form) && !known.iter().any(|(_, seen)| seen == form) {
            known.push((label.to_string(), form.to_string()));
        }
    }
}

/// Records `value` as a secret reported as `label`, unless it is too short or
/// plain to be one.
pub fn register(label: &str, value: &str) {
    let mut registered = REGISTERED.write().unwrap_or_else(PoisonError::into_inner);
    add_value(&mut registered, label, value);
}

/// Registers every string or number held under a credential-named key
/// anywhere in `value`, each labelled with its dotted path from `prefix`.
pub fn register_json(prefix: &str, value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                let label = format!("{prefix}.{key}");
                match item {
                    Value::String(text) if is_secret_name(key) => register(&label, text),
                    Value::Number(number) if is_secret_name(key) => {
                        register(&label, &number.to_string())
                    }
                    _ => register_json(&label, item),
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                register_json(&format!("{prefix}[{index}]"), item);
            }
        }
        _ => {}
    }
}

/// The secrets known now, longest value first, so a value holding another is
/// masked whole.
fn known() -> Vec<(String, String)> {
    let mut known = Vec::new();
    for (name, value) in std::env::vars_os() {
        if let (Ok(name), Ok(value)) = (name.into_string(), value.into_string()) {
            if is_secret_name(&name) {
                add_value(&mut known, &name, &value);
            }
        }
    }
    known.extend(
        REGISTERED
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned(),
    );
    known.sort_by(|(_, left), (_, right)| right.len().cmp(&left.len()).then(left.cmp(right)));
    known.dedup_by(|(_, left), (_, right)| left == right);
    known
}

/// The label of a secret value that `text` contains, if any.
pub fn find(text: &str) -> Option<String> {
    find_with(&known(), text)
}

fn find_with(known: &[(String, String)], text: &str) -> Option<String> {
    known
        .iter()
        .find(|(_, value)| text.contains(value.as_str()))
        .map(|(label, _)| label.clone())
}

/// The label of a secret value that any string, number or key in `value`
/// contains.
pub fn find_in_json(value: &Value) -> Option<String> {
    fn walk(known: &[(String, String)], value: &Value) -> Option<String> {
        match value {
            Value::String(text) => find_with(known, text),
            Value::Number(number) => find_with(known, &number.to_string()),
            Value::Array(items) => items.iter().find_map(|item| walk(known, item)),
            Value::Object(map) => map
                .iter()
                .find_map(|(key, item)| find_with(known, key).or_else(|| walk(known, item))),
            _ => None,
        }
    }
    walk(&known(), value)
}

/// `text` with every known secret value replaced by [`MASK`].
pub fn mask(text: &str) -> Cow<'_, str> {
    let mut masked = Cow::Borrowed(text);
    for (_, value) in known() {
        if masked.contains(value.as_str()) {
            masked = Cow::Owned(masked.replace(value.as_str(), MASK));
        }
    }
    masked
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credential_names_are_recognised_and_locations_are_not() {
        for name in [
            "OPENAI_API_KEY",
            "TELEGRAM_BOT_TOKEN",
            "SLACK_SIGNING_SECRET",
            "EMAIL_PASSWORD",
            "OPENAI_APIKEY",
            "api_key",
            "client-secret",
            "db.passphrase",
            "AWS_CREDENTIALS",
            "GITHUB_PAT",
        ] {
            assert!(
                is_secret_name(name),
                "{name} was not taken for a credential"
            );
        }
        for name in [
            "GITHUB_APP_PRIVATE_KEY_PATH",
            "GOOGLE_APPLICATION_CREDENTIALS_FILE",
            "api_key_env",
            "MODAL_TOKEN_ID",
            "TOKENIZERS_PARALLELISM",
            "SSH_AUTH_SOCK",
            "HERMES_HOME",
            "",
        ] {
            assert!(!is_secret_name(name), "{name} was taken for a credential");
        }
    }

    #[test]
    fn identifier_names_shaped_like_credentials_are_not_secrets() {
        for name in [
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_KEY_12",
            "HERMES_SESSION_KEY",
            "HERMES_DASHBOARD_OIDC_TOKEN_ENDPOINT",
        ] {
            assert!(!is_secret_name(name), "{name} was taken for a credential");
        }
        assert!(is_secret_name("AWS_SESSION_TOKEN"));
    }

    #[test]
    fn other_common_credential_names_are_recognised() {
        for name in [
            "PGPASSWORD",
            "MYSQL_PWD",
            "SMTP_PASS",
            "DB_PASS",
            "NPM_AUTH",
            "Authorization",
            "Cookie",
            "Proxy-Authorization",
        ] {
            assert!(
                is_secret_name(name),
                "{name} was not taken for a credential"
            );
        }
        for name in [
            "PGPASSWORD_FILE",
            "PWD",
            "OLDPWD",
            "PASS_THROUGH_ENV",
            "HERMES_AUTH_DEFAULT_PROVIDER",
            "HERMES_DASHBOARD_COOKIE_SECURE",
        ] {
            assert!(!is_secret_name(name), "{name} was taken for a credential");
        }
    }

    #[test]
    fn documented_placeholder_keys_are_not_secrets() {
        for value in [
            "not-needed",
            "lm-studio",
            "LM-Studio",
            "no-key-required",
            "sk-no-key-required",
            "dummy-key",
        ] {
            assert!(!is_secret_value(value), "{value} was taken for a secret");
        }
    }

    #[test]
    fn settings_about_a_credential_are_not_secrets() {
        for name in [
            "OPENVIKING_API_KEY_TYPE",
            "HERMES_CHRONOS_TOKEN_LEEWAY_SECONDS",
            "HERMES_ALLOW_SECRET_STDOUT",
        ] {
            assert!(!is_secret_name(name), "{name} was taken for a credential");
        }
    }

    #[test]
    fn a_scheme_is_stripped_only_from_an_authorization_value_with_a_real_credential() {
        // Made up.
        register("config.mcp.headers.Authorization", "Bearer not-needed");
        register("config.proxy.Authorization", "basic abc");
        register("config.db.password", "Basic ordinary-settings-2");
        register("config.api.Authorization", "bEaReR made-up-mixed-case-71");

        assert_eq!(find("Bearer not-needed"), None);
        assert_eq!(find("basic abc"), None);
        assert_eq!(find("ordinary-settings-2"), None);
        assert_eq!(
            find("Basic ordinary-settings-2").as_deref(),
            Some("config.db.password")
        );
        assert_eq!(
            find("made-up-mixed-case-71").as_deref(),
            Some("config.api.Authorization")
        );
    }

    #[test]
    fn the_token_in_an_authorization_value_is_found_without_its_scheme() {
        // Made up.
        register_json(
            "config",
            &json!({"mcp": {"headers": {"Authorization": "Bearer made-up-bearer-ochre-58"}}}),
        );

        assert_eq!(
            find("token=made-up-bearer-ochre-58").as_deref(),
            Some("config.mcp.headers.Authorization")
        );
        assert_eq!(mask("Bearer made-up-bearer-ochre-58"), "***");
    }

    #[test]
    fn a_number_under_a_credential_key_is_registered_and_found() {
        // Made up.
        register_json(
            "config",
            &json!({"vault": {"pin_password": 27_182_818_284_u64}}),
        );

        assert_eq!(
            find_in_json(&json!({"body": 27_182_818_284_u64})).as_deref(),
            Some("config.vault.pin_password")
        );
        assert_eq!(mask("pin 27182818284"), "pin ***");
    }

    #[test]
    fn short_plain_path_and_reference_values_are_not_secrets() {
        for value in [
            "hunter2",
            "required",
            "disabled",
            "/etc/key.pem",
            "~/.ssh/id",
            "C:\\keys\\a",
            "${OPENAI_API_KEY}",
        ] {
            assert!(!is_secret_value(value), "{value} was taken for a secret");
        }
        for value in [
            "hunter2!x",
            "correcthorsebatterystaple",
            "made-up-0123456789",
        ] {
            assert!(is_secret_value(value), "{value} was not taken for a secret");
        }
    }

    #[test]
    fn registered_config_values_are_found_and_masked() {
        // Made up.
        let config = json!({
            "llm_providers": {"custom": {"api_key": "made-up-registered-heron-77", "api_key_env": "SOME_VAR_NAME_LONG"}},
            "platforms": [{"token": "short"}],
        });
        register_json("config", &config);

        assert_eq!(
            find("send made-up-registered-heron-77 out").as_deref(),
            Some("config.llm_providers.custom.api_key")
        );
        assert_eq!(find("SOME_VAR_NAME_LONG"), None);
        assert_eq!(
            find_in_json(&json!({"args": ["x", {"made-up-registered-heron-77": 1}]})).as_deref(),
            Some("config.llm_providers.custom.api_key")
        );
        assert_eq!(mask("a made-up-registered-heron-77 b"), "a *** b");
        assert!(matches!(mask("nothing here"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_value_holding_another_is_masked_whole() {
        register("outer", "made-up-outer-made-up-inner-1");
        register("inner", "made-up-inner-1");

        assert_eq!(mask("x made-up-outer-made-up-inner-1 y"), "x *** y");
        assert_eq!(mask("x made-up-inner-1 y"), "x *** y");
    }
}
