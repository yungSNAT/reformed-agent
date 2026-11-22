use super::*;

use std::mem::{size_of, size_of_val};

fn config_with_key(provider: &str, base_url: Option<&str>) -> GatewayConfig {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        provider.to_string(),
        LlmProviderConfig {
            api_key: Some("cov-test-key".to_string()),
            base_url: base_url.map(str::to_string),
            ..LlmProviderConfig::default()
        },
    );
    config
}

fn assert_real_backend(p: &Arc<dyn LlmProvider>) {
    assert_ne!(size_of_val(&**p), size_of::<NoBackendProvider>());
}

fn fallback_env_present() -> bool {
    std::env::var_os("HERMES_FALLBACK_MODELS").is_some()
        || std::env::var_os("HERMES_FALLBACK_MODEL").is_some()
}

#[test]
fn local_backend_resolved_base_url_prefers_trimmed_config_value() {
    let spec = &local_backend_specs()[0];
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        spec.provider.to_string(),
        LlmProviderConfig {
            base_url: Some("  http://127.0.0.1:9/cov/v1  ".to_string()),
            ..LlmProviderConfig::default()
        },
    );
    assert_eq!(
        local_backend_resolved_base_url(spec.provider, Some(&config)),
        Some("http://127.0.0.1:9/cov/v1".to_string())
    );
}

#[test]
fn local_backend_resolved_base_url_blank_config_falls_back() {
    let spec = &local_backend_specs()[0];
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        spec.provider.to_string(),
        LlmProviderConfig {
            base_url: Some("   ".to_string()),
            ..LlmProviderConfig::default()
        },
    );
    let resolved = local_backend_resolved_base_url(spec.provider, Some(&config))
        .expect("local backend always resolves a base url");
    assert!(!resolved.trim().is_empty());
    let without_config = local_backend_resolved_base_url(spec.provider, None)
        .expect("local backend always resolves a base url");
    assert!(!without_config.trim().is_empty());
}

#[test]
fn local_backend_resolved_base_url_unknown_provider_is_none() {
    assert_eq!(
        local_backend_resolved_base_url("zz-cov-not-a-local-backend", None),
        None
    );
}

#[test]
fn smoke_matrix_reports_all_mismatch_kinds() {
    let config = GatewayConfig::default();
    let cases = vec![provider_auth_smoke_case(
        "cov_mismatch",
        "moa:default",
        "zz-wrong-provider",
        false,
        None,
        false,
        true,
    )];
    let matrix = provider_auth_smoke_matrix_with_auth_resolver(&config, &cases, None);
    assert!(!matrix.pass);
    assert_eq!(matrix.summary.total, 1);
    assert_eq!(matrix.summary.passed, 0);
    assert_eq!(matrix.summary.failed, 1);
    let result = &matrix.results[0];
    assert!(!result.pass);
    assert!(result
        .failures
        .iter()
        .any(|f| f == "runtime_provider expected=zz-wrong-provider actual=moa"));
    assert!(result
        .failures
        .iter()
        .any(|f| f == "local_no_key_allowed expected=false actual=true"));
    assert!(result
        .failures
        .iter()
        .any(|f| f == "uses_openai_pro_backend expected=true actual=false"));
}

#[test]
fn api_key_source_reports_configured_env_var() {
    let cfg = LlmProviderConfig {
        api_key_env: Some(" PATH ".to_string()),
        ..LlmProviderConfig::default()
    };
    let (present, source) =
        provider_runtime_api_key_source(Some(&cfg), "zz-cov-alias-a", "zz-cov-runtime-a", None);
    assert!(present);
    assert_eq!(source, Some("env:PATH".to_string()));
}

#[test]
fn api_key_source_uses_resolver_for_runtime_provider() {
    let resolver = |name: &str| {
        if name == "zz-cov-runtime-b" {
            Some("tok".to_string())
        } else {
            None
        }
    };
    let (present, source) = provider_runtime_api_key_source(
        None,
        "zz-cov-alias-b",
        "zz-cov-runtime-b",
        Some(&resolver),
    );
    assert!(present);
    assert_eq!(source, Some("oauth_resolver:zz-cov-runtime-b".to_string()));
}

#[test]
fn fallback_chain_dedupes_and_appends_single_fallback() {
    if fallback_env_present() {
        return;
    }
    let config = GatewayConfig {
        fallback_models: vec![
            "alpha".to_string(),
            "   ".to_string(),
            "ALPHA".to_string(),
            " beta ".to_string(),
        ],
        fallback_model: Some("gamma".to_string()),
        ..GatewayConfig::default()
    };
    assert_eq!(
        configured_model_fallback_chain(&config),
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
    );
}

#[test]
fn startup_selection_defaults_empty_request_to_gpt55() {
    let selection = select_startup_model_with_fallback(&GatewayConfig::default(), "   ");
    assert_eq!(selection.requested_model, "gpt-5.5");
    assert!(!selection.selected_model.is_empty());
    if let Some(first) = selection.skipped_unavailable_models.first() {
        assert_eq!(first, "gpt-5.5");
    }
}

#[test]
fn startup_selection_skips_fallback_equal_to_request() {
    if fallback_env_present() {
        return;
    }
    let config = GatewayConfig {
        fallback_models: vec!["ZZ-COV-NONE:MODEL".to_string(), "moa:default".to_string()],
        ..GatewayConfig::default()
    };
    let selection = select_startup_model_with_fallback(&config, "zz-cov-none:model");
    assert_eq!(selection.requested_model, "zz-cov-none:model");
    assert_eq!(selection.selected_model, "moa:default");
    assert!(selection.fallback_used);
    assert_eq!(
        selection.skipped_unavailable_models,
        vec!["zz-cov-none:model".to_string()]
    );
}

#[test]
fn active_config_matches_case_insensitively() {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        "MyCovProv".to_string(),
        LlmProviderConfig {
            base_url: Some("http://127.0.0.1:9/case".to_string()),
            ..LlmProviderConfig::default()
        },
    );
    let found = active_llm_provider_config(&config, "mycovprov", "mycovprov-runtime")
        .expect("case-insensitive match");
    assert_eq!(found.base_url.as_deref(), Some("http://127.0.0.1:9/case"));
    assert!(active_llm_provider_config(&config, "other", "other-runtime").is_none());
}

#[test]
fn resolve_provider_uses_single_configured_provider() {
    let mut config = GatewayConfig::default();
    config
        .llm_providers
        .insert("zz-solo".to_string(), LlmProviderConfig::default());
    assert_eq!(
        resolve_provider_and_model(&config, "  some-model  "),
        ("zz-solo".to_string(), "some-model".to_string())
    );
}

#[tokio::test]
async fn build_provider_moa_returns_no_backend() {
    let p = build_provider(&GatewayConfig::default(), "moa:default");
    assert_eq!(size_of_val(&*p), size_of::<NoBackendProvider>());
    let result = p.chat_completion(&[], &[], None, None, None, None).await;
    match result {
        Err(AgentError::LlmApi(msg)) => {
            assert!(msg.contains("NoBackendProvider"));
            assert!(msg.contains("moa:default"));
        }
        _ => panic!("expected LlmApi error from NoBackendProvider"),
    }
}

#[tokio::test]
async fn no_backend_stream_yields_single_error() {
    let p = NoBackendProvider {
        model: "cov-model".to_string(),
    };
    let mut stream = p.chat_completion_stream(&[], &[], None, None, None, None);
    match futures::StreamExt::next(&mut stream).await {
        Some(Err(AgentError::LlmApi(msg))) => {
            assert!(msg.contains("no LLM backend configured for streaming"));
        }
        _ => panic!("expected streaming LlmApi error"),
    }
    assert!(futures::StreamExt::next(&mut stream).await.is_none());
}

#[test]
fn build_provider_vertex_constructs_vertex_backend() {
    let model = format!("{VERTEX_PROVIDER_ID}:gemini-2.5-pro");
    let p = build_provider(&GatewayConfig::default(), &model);
    assert_real_backend(&p);
    assert_eq!(size_of_val(&*p), size_of::<VertexProvider>());
}

#[test]
fn build_provider_codex_with_base_url() {
    let config = config_with_key("codex", Some("http://127.0.0.1:9/codex"));
    let p = build_provider(&config, "codex:gpt-5.5");
    assert_real_backend(&p);
    assert_eq!(size_of_val(&*p), size_of::<CodexProvider>());
}

#[test]
fn build_provider_anthropic_with_base_url() {
    let config = config_with_key("anthropic", Some("http://127.0.0.1:9/anthropic"));
    let p = build_provider(&config, "anthropic:claude-sonnet-4-6");
    assert_real_backend(&p);
    assert_eq!(size_of_val(&*p), size_of::<AnthropicProvider>());
}

#[test]
fn build_provider_bedrock_without_key() {
    let p = build_provider(&GatewayConfig::default(), "bedrock:anthropic.claude-v2");
    assert_real_backend(&p);
}

#[test]
fn build_provider_openrouter() {
    let config = config_with_key("openrouter", None);
    let p = build_provider(&config, "openrouter:anthropic/claude-sonnet-4.6");
    assert_real_backend(&p);
    assert_eq!(size_of_val(&*p), size_of::<OpenRouterProvider>());
}

#[test]
fn build_provider_qwen_oauth_with_base_url() {
    let config = config_with_key("qwen-cli", Some("http://127.0.0.1:9/qwen"));
    let p = build_provider(&config, "qwen-cli:qwen3-coder");
    assert_real_backend(&p);
    assert_eq!(size_of_val(&*p), size_of::<QwenProvider>());
}

#[test]
fn build_provider_kimi_and_moonshot() {
    for provider in ["kimi", "moonshot"] {
        let config = config_with_key(provider, Some("http://127.0.0.1:9/kimi"));
        let p = build_provider(&config, &format!("{provider}:kimi-k2"));
        assert_real_backend(&p);
    }
}

#[test]
fn build_provider_minimax_with_base_url() {
    let config = config_with_key("minimax", Some("http://127.0.0.1:9/minimax"));
    let p = build_provider(&config, "minimax:MiniMax-M2");
    assert_real_backend(&p);
}

#[test]
fn build_provider_gemini_with_base_url() {
    let config = config_with_key("gemini", Some("http://127.0.0.1:9/gemini"));
    let p = build_provider(&config, "gemini:gemini-2.5-pro");
    assert_real_backend(&p);
}

#[test]
fn build_provider_stepfun() {
    let config = config_with_key("stepfun", None);
    let p = build_provider(&config, "stepfun:step-2");
    assert_real_backend(&p);
}

#[test]
fn build_provider_nous_with_base_url() {
    let config = config_with_key("nous", Some("http://127.0.0.1:9/nous"));
    let p = build_provider(&config, "nous:Hermes-4");
    assert_real_backend(&p);
    assert_eq!(size_of_val(&*p), size_of::<NousProvider>());
}

#[test]
fn build_provider_copilot() {
    let config = config_with_key("copilot", None);
    let p = build_provider(&config, "copilot:gpt-4o");
    assert_real_backend(&p);
}
