use super::*;
use crate::test_env_lock;
use std::env;
use std::ffi::{OsStr, OsString};

#[derive(Default)]
struct ScopedEnv {
    prior: Vec<(&'static str, Option<OsString>)>,
}

impl ScopedEnv {
    fn set(&mut self, name: &'static str, value: impl AsRef<OsStr>) {
        self.prior.push((name, env::var_os(name)));
        env::set_var(name, value);
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        while let Some((name, value)) = self.prior.pop() {
            if let Some(value) = value {
                env::set_var(name, value);
            } else {
                env::remove_var(name);
            }
        }
    }
}

#[test]
fn whitespace_model_is_rejected_as_empty() {
    if run_isolated("whitespace_model_is_rejected_as_empty") {
        return;
    }

    assert!(matches!(
        normalize_provider_model(" \t\n "),
        Err(AgentError::Config(message)) if message == "Model cannot be empty"
    ));
}

#[test]
fn picker_describes_alibaba_coding_plan_and_azure_foundry() {
    if run_isolated("picker_describes_alibaba_coding_plan_and_azure_foundry") {
        return;
    }

    assert_eq!(
        provider_picker_description(" ALIBABA-CODING-PLAN "),
        "Alibaba Coding Plan (coding models via DashScope Coding Plan API)"
    );
    assert_eq!(
        provider_picker_description(" Azure-Foundry "),
        "Azure Foundry (OpenAI-style or Anthropic-style endpoint, your Azure AI deployment)"
    );
}

#[test]
fn picker_describes_text_generation_webui_consistently_across_aliases() {
    if run_isolated("picker_describes_text_generation_webui_consistently_across_aliases") {
        return;
    }

    let expected = "text-generation-webui / oobabooga (local OpenAI-compatible server)";
    assert_eq!(
        provider_picker_description("text-generation-webui"),
        expected
    );
    assert_eq!(provider_picker_description("oobabooga"), expected);

    for spelling in [
        "text_generation_webui",
        "text_generation_web_ui",
        "textgen_webui",
        "textgen",
    ] {
        if canonical_provider_id(spelling) == "text-generation-webui" {
            assert_eq!(
                provider_picker_description(spelling),
                expected,
                "{spelling}"
            );
        }
    }
}

#[test]
fn nous_api_shares_the_nonempty_nous_curated_catalog() {
    if run_isolated("nous_api_shares_the_nonempty_nous_curated_catalog") {
        return;
    }

    let models = provider_curated_models("nous-api");
    assert!(models.contains(&"moonshotai/kimi-k2.6"));
    assert_eq!(models, provider_curated_models("nous"));
}

#[test]
fn configured_provider_lookup_rejects_blank_names_even_with_an_empty_key() {
    if run_isolated("configured_provider_lookup_rejects_blank_names_even_with_an_empty_key") {
        return;
    }

    let mut config = GatewayConfig::default();
    config
        .llm_providers
        .insert(String::new(), LlmProviderConfig::default());

    assert!(configured_llm_provider(&config, " \t\n ").is_none());
}

#[test]
fn configured_api_key_prefers_named_environment_then_uses_the_inline_key() {
    if run_isolated("configured_api_key_prefers_named_environment_then_uses_the_inline_key") {
        return;
    }

    const NAMED: &str = "HERMES_TEST_MODEL_SWITCH_COVERAGE_NAMED_KEY";
    const INLINE: &str = "HERMES_TEST_MODEL_SWITCH_COVERAGE_INLINE_KEY";

    let _lock = test_env_lock::lock();
    let mut env = ScopedEnv::default();
    env.set(NAMED, "  named-fixture-key \n");
    env.set(INLINE, " \tinline-fixture-key ");

    let mut provider = LlmProviderConfig {
        api_key_env: Some(format!(" {NAMED} ")),
        api_key: Some(format!(" ${{ {INLINE} }} ")),
        ..LlmProviderConfig::default()
    };

    assert_eq!(
        resolve_configured_api_key(&provider).as_deref(),
        Some("named-fixture-key")
    );

    env.set(NAMED, " \t ");
    assert_eq!(
        resolve_configured_api_key(&provider).as_deref(),
        Some("inline-fixture-key")
    );

    env.set(INLINE, " \n ");
    assert_eq!(resolve_configured_api_key(&provider), None);

    provider.api_key = Some("  literal-fixture-key  ".to_string());
    assert_eq!(
        resolve_configured_api_key(&provider).as_deref(),
        Some("literal-fixture-key")
    );

    provider.api_key = Some(" \t ".to_string());
    assert_eq!(resolve_configured_api_key(&provider), None);
}

#[tokio::test]
async fn configured_discovery_falls_back_to_nonblank_unique_models() {
    if run_isolated("configured_discovery_falls_back_to_nonblank_unique_models") {
        return;
    }

    let mut config = GatewayConfig::default();
    const PROVIDER: &str = "coverage-invalid-discovery-endpoint";

    config.llm_providers.insert(
        PROVIDER.to_string(),
        LlmProviderConfig {
            // The unmatched IPv6 bracket makes the URL invalid before any I/O.
            base_url: Some("http://[".to_string()),
            api_key: Some("fixture-key".to_string()),
            discover_models: true,
            models: ["  First  ", " \t ", "FIRST", "Second", ""]
                .map(str::to_string)
                .to_vec(),
            ..LlmProviderConfig::default()
        },
    );

    assert_eq!(
        provider_model_ids_for_config(PROVIDER, &config).await,
        vec!["First", "Second"]
    );
}

#[tokio::test]
async fn configured_catalog_omits_providers_without_models() {
    if run_isolated("configured_catalog_omits_providers_without_models") {
        return;
    }

    const EMPTY_PROVIDER: &str = "coverage-empty-custom-catalog";

    let _lock = test_env_lock::lock_async().await;
    let home = tempfile::tempdir().expect("temporary Hermes home");
    let mut env = ScopedEnv::default();
    env.set("HERMES_HOME", home.path());

    let mut config = GatewayConfig::default();
    config.llm_providers.clear();
    // Override every built-in catalog so listing them needs no live discovery.
    for provider in curated_provider_slugs() {
        config.llm_providers.insert(
            provider.to_string(),
            LlmProviderConfig {
                models: vec![format!("{provider}-fixture")],
                discover_models: false,
                ..LlmProviderConfig::default()
            },
        );
    }
    let expected_entries = provider_slugs_for_config(&config).len();
    config.llm_providers.insert(
        EMPTY_PROVIDER.to_string(),
        LlmProviderConfig {
            models: vec![" \t ".to_string()],
            discover_models: false,
            ..LlmProviderConfig::default()
        },
    );
    assert!(provider_slugs_for_config(&config)
        .iter()
        .any(|provider| provider == EMPTY_PROVIDER));

    let entries = provider_catalog_entries_for_config(&config).await;
    assert_eq!(entries.len(), expected_entries);
    for entry in entries {
        assert_ne!(entry.provider, EMPTY_PROVIDER);
        assert_eq!(entry.models, vec![format!("{}-fixture", entry.provider)]);
        assert_eq!(entry.total_models, 1);
    }
}

#[tokio::test]
async fn catalog_entries_preserve_cached_models_and_requested_provider_names() {
    if run_isolated("catalog_entries_preserve_cached_models_and_requested_provider_names") {
        return;
    }

    let _lock = test_env_lock::lock_async().await;
    let home = tempfile::tempdir().expect("temporary Hermes home");
    let mut env = ScopedEnv::default();
    env.set("HERMES_HOME", home.path());
    env.set(
        "HERMES_PROVENANCE_SIGNING_KEY",
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
    );

    let cached_models = vec![
        "coverage/cached-second".to_string(),
        "coverage/cached-first".to_string(),
    ];
    persist_provider_catalog_cache("openrouter", &cached_models);

    let entries =
        provider_catalog_entries(&["openrouter", "coverage-unknown-provider", "mixture"]).await;

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].provider, "openrouter");
    assert_eq!(entries[0].models, cached_models);
    assert_eq!(entries[0].total_models, cached_models.len());
    assert_eq!(entries[1].provider, "mixture");
    assert_eq!(entries[1].models, vec!["default"]);
    assert_eq!(entries[1].total_models, 1);
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path
        .split_once("::")
        .map_or(path.as_str(), |(_, rest)| rest);
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
