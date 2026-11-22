// Coverage tests for the private helpers in `model_switch::live_catalog`.
//
// Not covered here: the bodies of `fetch_openai_compatible_live_models` and
// `fetch_nous_live_models` return early when `cfg!(test)` is set, and
// `resolve_nous_catalog_endpoint_and_token` needs real Nous credentials
// (home directory or network). None of those lines can be reached safely.

use super::*;

// Provider names that no lookup table recognises.
const UNKNOWN_PROVIDERS: [&str; 3] = ["definitely-not-a-provider", "", "   "];

#[allow(unused_unsafe)]
fn set_env(name: &str, value: &str) {
    // `set_var` is unsafe in edition 2024 and safe in earlier editions.
    unsafe {
        std::env::set_var(name, value);
    }
}

#[allow(unused_unsafe)]
fn remove_env(name: &str) {
    // `remove_var` is unsafe in edition 2024 and safe in earlier editions.
    unsafe {
        std::env::remove_var(name);
    }
}

fn restore_env(name: &str, previous: Option<String>) {
    match previous {
        Some(value) => set_env(name, &value),
        None => remove_env(name),
    }
}

#[test]
fn unknown_provider_has_no_default_base_url() {
    if run_isolated("unknown_provider_has_no_default_base_url") {
        return;
    }

    for provider in UNKNOWN_PROVIDERS {
        assert_eq!(local_provider_default_base_url(provider), None);
    }
}

#[test]
fn unknown_provider_has_no_base_url_env_var() {
    if run_isolated("unknown_provider_has_no_base_url_env_var") {
        return;
    }

    for provider in UNKNOWN_PROVIDERS {
        assert_eq!(local_provider_base_url_env_var(provider), None);
    }
}

#[test]
fn unknown_provider_has_no_api_key() {
    if run_isolated("unknown_provider_has_no_api_key") {
        return;
    }

    for provider in UNKNOWN_PROVIDERS {
        assert_eq!(local_provider_api_key(provider), None);
    }
}

#[test]
fn unknown_provider_has_no_resolved_base_url() {
    if run_isolated("unknown_provider_has_no_resolved_base_url") {
        return;
    }

    assert_eq!(
        local_provider_resolved_base_url("definitely-not-a-provider"),
        None
    );
    assert!(!is_local_openai_compatible_provider(
        "definitely-not-a-provider"
    ));
}

#[test]
fn unknown_provider_has_no_catalog_credentials() {
    if run_isolated("unknown_provider_has_no_catalog_credentials") {
        return;
    }

    assert_eq!(
        openai_compatible_catalog_credentials("definitely-not-a-provider"),
        None
    );
    assert_eq!(openai_compatible_catalog_credentials(""), None);
}

#[test]
fn deepinfra_catalog_credentials_use_env_token_and_base_url() {
    if run_isolated("deepinfra_catalog_credentials_use_env_token_and_base_url") {
        return;
    }

    let prev_key = std::env::var("DEEPINFRA_API_KEY").ok();
    let prev_base = std::env::var("DEEPINFRA_BASE_URL").ok();

    // Token set, base URL unset: the default DeepInfra endpoint is used and
    // the token is trimmed.
    set_env("DEEPINFRA_API_KEY", "  deepinfra-token  ");
    remove_env("DEEPINFRA_BASE_URL");
    let default_result = openai_compatible_catalog_credentials("deepinfra");

    // An explicit base URL overrides the default and is trimmed.
    set_env("DEEPINFRA_BASE_URL", "  https://example.invalid/v1  ");
    let override_result = openai_compatible_catalog_credentials("deepinfra");

    // A whitespace-only base URL falls back to the default.
    set_env("DEEPINFRA_BASE_URL", "   ");
    let blank_base_result = openai_compatible_catalog_credentials("deepinfra");

    // Without a token there are no credentials, whatever the base URL.
    remove_env("DEEPINFRA_API_KEY");
    let missing_token_result = openai_compatible_catalog_credentials("deepinfra");

    // Restore before asserting so a failure cannot leak env changes.
    restore_env("DEEPINFRA_API_KEY", prev_key);
    restore_env("DEEPINFRA_BASE_URL", prev_base);

    assert_eq!(
        default_result,
        Some((
            "https://api.deepinfra.com/v1/openai".to_string(),
            "deepinfra-token".to_string()
        ))
    );
    assert_eq!(
        override_result,
        Some((
            "https://example.invalid/v1".to_string(),
            "deepinfra-token".to_string()
        ))
    );
    assert_eq!(
        blank_base_result,
        Some((
            "https://api.deepinfra.com/v1/openai".to_string(),
            "deepinfra-token".to_string()
        ))
    );
    assert_eq!(missing_token_result, None);
}

#[test]
fn openai_compatible_models_url_trims_trailing_slashes() {
    if run_isolated("openai_compatible_models_url_trims_trailing_slashes") {
        return;
    }

    assert_eq!(
        openai_compatible_models_url("https://api.deepinfra.com/v1/openai///"),
        "https://api.deepinfra.com/v1/openai/models?output_modalities=all"
    );
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path.split_once("::").map_or(path.as_str(), |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([name, "--exact", "--test-threads=1", "--nocapture"])
        .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
        .output()
        .expect("run the isolated test");
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "isolated test {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    true
}
