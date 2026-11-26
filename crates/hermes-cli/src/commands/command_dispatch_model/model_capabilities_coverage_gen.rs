use super::*;

fn caps(tools: bool, vision: bool, reasoning: bool, ctx: u64) -> ResolvedModelCapabilities {
    ResolvedModelCapabilities {
        supports_tools: tools,
        supports_vision: vision,
        supports_reasoning: reasoning,
        context_window: ctx,
    }
}

#[test]
fn effective_min_context_long_context_with_explicit_value() {
    let low = ModelCapabilityRequirements {
        require_long_context: true,
        min_context_window: Some(1_000),
        ..Default::default()
    };
    assert_eq!(low.effective_min_context(), Some(128_000));

    let high = ModelCapabilityRequirements {
        require_long_context: true,
        min_context_window: Some(300_000),
        ..Default::default()
    };
    assert_eq!(high.effective_min_context(), Some(300_000));
}

#[test]
fn summary_lists_all_parts_or_none() {
    assert_eq!(ModelCapabilityRequirements::default().summary(), "none");

    let all = ModelCapabilityRequirements {
        require_tools: true,
        require_vision: true,
        require_reasoning: true,
        require_long_context: true,
        min_context_window: Some(200_000),
    };
    assert_eq!(all.summary(), "tools, vision, reasoning, context>=200000");

    let only_ctx = ModelCapabilityRequirements {
        min_context_window: Some(5_000),
        ..Default::default()
    };
    assert_eq!(only_ctx.summary(), "context>=5000");
}

#[test]
fn openai_codex_oauth_false_without_config_for_other_provider() {
    assert!(!provider_is_openai_codex_oauth("anthropic", None));
}

#[test]
fn resolve_display_context_window_uses_models_dev_then_fallback() {
    assert_eq!(
        resolve_display_context_window("zzz-prov", "unknown-model", None, Some(5_000)),
        5_000
    );
    let expected = get_model_context_length("zzz-prov:unknown-model");
    assert_eq!(
        resolve_display_context_window(" zzz-prov ", " unknown-model ", None, Some(0)),
        expected
    );
    assert_eq!(
        resolve_display_context_window("zzz-prov", "unknown-model", None, None),
        expected
    );
}

#[test]
fn parse_args_skips_empty_tokens_and_empty_cap_entries() {
    let (positional, req, provider) =
        parse_model_command_args(&["", "  ", "--cap", "tools,,vision", "gpt-x"]).unwrap();
    assert_eq!(positional, vec!["gpt-x".to_string()]);
    assert!(req.require_tools);
    assert!(req.require_vision);
    assert!(!req.require_reasoning);
    assert!(provider.is_none());
}

#[test]
fn parse_args_rejects_empty_provider() {
    let err = parse_model_command_args(&["--provider", "   "]).unwrap_err();
    assert!(matches!(
        err,
        AgentError::Config(ref msg) if msg.contains("provider override cannot be empty")
    ));
}

#[test]
fn parse_args_min_context_missing_value() {
    let err = parse_model_command_args(&["--min-context"]).unwrap_err();
    assert!(matches!(
        err,
        AgentError::Config(ref msg) if msg.contains("requires a numeric value")
    ));
}

#[test]
fn parse_args_min_context_invalid_value() {
    let err = parse_model_command_args(&["--min_context", "abc"]).unwrap_err();
    assert!(matches!(
        err,
        AgentError::Config(ref msg) if msg.contains("Invalid --min-context value 'abc'")
    ));
}

#[test]
fn model_meets_requirements_branches() {
    let tools = ModelCapabilityRequirements {
        require_tools: true,
        ..Default::default()
    };
    assert!(!model_meets_requirements(
        caps(false, true, true, 1_000_000),
        tools
    ));

    let reasoning = ModelCapabilityRequirements {
        require_reasoning: true,
        ..Default::default()
    };
    assert!(!model_meets_requirements(
        caps(true, true, false, 1_000_000),
        reasoning
    ));

    let ctx = ModelCapabilityRequirements {
        min_context_window: Some(1_000),
        ..Default::default()
    };
    assert!(!model_meets_requirements(caps(true, true, true, 500), ctx));
    assert!(model_meets_requirements(caps(true, true, true, 2_000), ctx));
}

#[test]
fn unmet_requirements_reports_tools_and_skips_satisfied_context() {
    let req = ModelCapabilityRequirements {
        require_tools: true,
        min_context_window: Some(1_000),
        ..Default::default()
    };
    let missing = unmet_model_requirements(caps(false, false, false, 2_000), req);
    assert_eq!(missing, vec!["tools".to_string()]);

    let missing = unmet_model_requirements(caps(false, false, false, 10), req);
    assert_eq!(
        missing,
        vec!["tools".to_string(), "context>=1000 (actual=10)".to_string()]
    );
}

#[test]
fn switch_request_whitespace_only_picks_provider() {
    let providers = ["openai"];
    assert_eq!(
        parse_model_switch_request(&["  ", " "], &providers),
        ModelSwitchRequest::PickProviderThenModel
    );
}

#[test]
fn nested_model_block_detection_variants() {
    let dir = tempfile::tempdir().unwrap();

    let blank = dir.path().join("blank.yaml");
    std::fs::write(&blank, "   \n\n").unwrap();
    assert!(!config_uses_nested_model_block(&blank).unwrap());

    let seq = dir.path().join("seq.yaml");
    std::fs::write(&seq, "- a\n- b\n").unwrap();
    assert!(!config_uses_nested_model_block(&seq).unwrap());

    let flat = dir.path().join("flat.yaml");
    std::fs::write(&flat, "model: openai:gpt-4o\n").unwrap();
    assert!(!config_uses_nested_model_block(&flat).unwrap());

    let nested = dir.path().join("nested.yaml");
    std::fs::write(&nested, "model:\n  default: gpt-4o\n  provider: openai\n").unwrap();
    assert!(config_uses_nested_model_block(&nested).unwrap());
}

#[test]
fn parse_failover_chain_dedupes_and_skips_empty() {
    let chain = parse_failover_chain("openai:gpt-4o, ,openai:gpt-4o,").unwrap();
    assert_eq!(chain.len(), 1);
    assert!(chain[0].contains("gpt-4o"));

    let empty = parse_failover_chain(" , ,").unwrap();
    assert!(empty.is_empty());
}
