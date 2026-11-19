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
//! found; catching those needs a decoder per encoding.

use std::borrow::Cow;
use std::sync::{LazyLock, PoisonError, RwLock};

use serde_json::Value;

/// Shorter values are never treated as secrets: masking or refusing a short
/// common string would break ordinary text more often than it caught a leak.
const MIN_SECRET_CHARS: usize = 8;

/// A value of only letters must be at least this long: a word such as
/// `required` or `disabled` under a credential-named key is a setting.
const MIN_LETTERS_ONLY_CHARS: usize = 16;

/// What masked text shows in place of a secret value.
pub const MASK: &str = "***";

/// Values registered from loaded configuration, with the label each is
/// reported under.
static REGISTERED: LazyLock<RwLock<Vec<(String, String)>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Whether a variable or field name names a credential: one of its words is
/// `PASSWORD`, `PASSWD`, `PASSPHRASE`, `CREDENTIAL`, `CREDENTIALS` or `PAT` (a
/// personal access token), or ends
/// with `KEY`, `TOKEN` or `SECRET` (`APIKEY` too), and its last word is not
/// `PATH`, `FILE`, `DIR`, `ENV`, `URL` or `ID`, which name where a secret is
/// kept or which one it is, not the secret. Words are split on anything but
/// ASCII letters and digits; case is ignored.
pub fn is_secret_name(name: &str) -> bool {
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    let Some(last) = words.last() else {
        return false;
    };
    if matches!(
        last.as_str(),
        "PATH" | "FILE" | "DIR" | "ENV" | "URL" | "ID"
    ) {
        return false;
    }
    words.iter().any(|word| {
        matches!(
            word.as_str(),
            "PASSWORD" | "PASSWD" | "PASSPHRASE" | "CREDENTIAL" | "CREDENTIALS" | "PAT"
        ) || ["KEY", "TOKEN", "SECRET"]
            .iter()
            .any(|suffix| word.ends_with(suffix))
    })
}

/// Whether a value under a credential name is worth matching: long enough,
/// not only a short word, and not a path or a `${VAR}` reference.
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
}

/// Records `value` as a secret reported as `label`, unless it is too short or
/// plain to be one.
pub fn register(label: &str, value: &str) {
    let value = value.trim();
    if !is_secret_value(value) {
        return;
    }
    let mut registered = REGISTERED.write().unwrap_or_else(PoisonError::into_inner);
    if !registered.iter().any(|(_, known)| known == value) {
        registered.push((label.to_string(), value.to_string()));
    }
}

/// Registers every string held under a credential-named key anywhere in
/// `value`, each labelled with its dotted path from `prefix`.
pub fn register_json(prefix: &str, value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                let label = format!("{prefix}.{key}");
                match item {
                    Value::String(text) if is_secret_name(key) => register(&label, text),
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
    let mut known: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| is_secret_name(name))
        .map(|(name, value)| (name, value.trim().to_string()))
        .filter(|(_, value)| is_secret_value(value))
        .collect();
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

/// The label of a secret value that any string or key in `value` contains.
pub fn find_in_json(value: &Value) -> Option<String> {
    fn walk(known: &[(String, String)], value: &Value) -> Option<String> {
        match value {
            Value::String(text) => find_with(known, text),
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
