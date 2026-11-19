fn direct_provider_body(profile: &str, model: &str, extra_body: Option<&Value>) -> Value {
    let provider = GenericProvider::new("https://example.invalid/v1", "key", model)
        .with_provider_profile(profile);
    let messages = vec![Message::user("hello")];
    provider.chat_request_body(ChatRequestParams {
        messages: &messages,
        tools: &[],
        max_tokens: None,
        temperature: None,
        effective_model: model,
        extra_body,
        stream: false,
    })
}

#[test]
fn deepinfra_fireworks_and_upstage_resolve_to_their_own_endpoints() {
    use crate::provider_profiles::{normalize_runtime_provider_name, provider_default_base_url};
    const DEEPINFRA_BASE_URL: &str = "https://api.deepinfra.com/v1/openai";
    const FIREWORKS_BASE_URL: &str = "https://api.fireworks.ai/inference/v1";
    const UPSTAGE_BASE_URL: &str = "https://api.upstage.ai/v1";
    let cases = [
        ("deepinfra", "deepinfra", DEEPINFRA_BASE_URL, "DEEPINFRA_API_KEY"),
        ("deep-infra", "deepinfra", DEEPINFRA_BASE_URL, "DEEPINFRA_API_KEY"),
        ("fireworks", "fireworks", FIREWORKS_BASE_URL, "FIREWORKS_API_KEY"),
        ("fw", "fireworks", FIREWORKS_BASE_URL, "FIREWORKS_API_KEY"),
        ("upstage", "upstage", UPSTAGE_BASE_URL, "UPSTAGE_API_KEY"),
        ("solar", "upstage", UPSTAGE_BASE_URL, "UPSTAGE_API_KEY"),
    ];
    for (alias, canonical, base_url, key_var) in cases {
        assert_eq!(normalize_runtime_provider_name(alias), canonical, "{alias}");
        assert_eq!(hermes_core::providers::canonical_provider_id(alias), canonical);
        assert_eq!(provider_default_base_url(alias), Some(base_url), "{alias}");
        assert_eq!(
            hermes_core::providers::provider_api_key_env_vars(alias, canonical),
            &[key_var],
            "{alias}"
        );
    }
    assert_eq!(
        crate::provider_profiles::canonical_provider_profile_id("fireworks-ai"),
        Some("fireworks")
    );
}

#[test]
fn deepinfra_sends_top_level_effort_and_none_to_switch_off() {
    let body = direct_provider_body(
        "deepinfra",
        "deepseek-ai/DeepSeek-V4-Flash",
        Some(&serde_json::json!({"reasoning": {"effort": "ultra"}})),
    );
    assert_eq!(body["reasoning_effort"], "max");
    assert!(body.get("reasoning").is_none());

    let off = direct_provider_body(
        "deepinfra",
        "zai-org/GLM-5.2",
        Some(&serde_json::json!({"reasoning_config": {"enabled": false}})),
    );
    assert_eq!(off["reasoning_effort"], "none");

    let unset = direct_provider_body("deepinfra", "zai-org/GLM-5.2", None);
    assert!(unset.get("reasoning_effort").is_none());
    assert!(unset.get("reasoning").is_none());
}

#[test]
fn fireworks_never_sends_a_nested_reasoning_object() {
    let body = direct_provider_body(
        "fireworks",
        "accounts/fireworks/models/kimi-k2p6",
        Some(&serde_json::json!({"reasoning": {"effort": "high"}})),
    );
    assert_eq!(body["reasoning_effort"], "high");
    assert!(body.get("reasoning").is_none());

    let off = direct_provider_body(
        "fireworks",
        "accounts/fireworks/models/glm-5p2",
        Some(&serde_json::json!({"reasoning": {"enabled": false, "effort": "high"}})),
    );
    assert_eq!(off["reasoning_effort"], "none");
    assert!(off.get("reasoning_config").is_none());

    // Upstream passes Fireworks the effort as written.
    let verbatim = direct_provider_body(
        "fireworks",
        "accounts/fireworks/models/kimi-k2p6",
        Some(&serde_json::json!({"reasoning_config": {"effort": " Future "}})),
    );
    assert_eq!(verbatim["reasoning_effort"], " Future ");
    assert!(verbatim.get("reasoning_config").is_none());

    let empty = direct_provider_body(
        "fireworks",
        "accounts/fireworks/models/kimi-k2p6",
        Some(&serde_json::json!({"reasoning_config": {"effort": ""}})),
    );
    assert!(empty.get("reasoning_effort").is_none());

    // The port-level `reasoning_effort` extra still applies when the
    // nested config sets no effort.
    let top_level = direct_provider_body(
        "fireworks",
        "accounts/fireworks/models/kimi-k2p6",
        Some(&serde_json::json!({"reasoning_config": {}, "reasoning_effort": "high"})),
    );
    assert_eq!(top_level["reasoning_effort"], "high");
    assert!(top_level.get("reasoning_config").is_none());
}

#[test]
fn upstage_defaults_reasoning_on_and_skips_models_without_it() {
    let unset = direct_provider_body("upstage", "solar-pro3", None);
    assert_eq!(unset["reasoning_effort"], "medium");

    let strongest = direct_provider_body(
        "solar",
        "solar-pro3",
        Some(&serde_json::json!({"reasoning": {"effort": "xhigh"}})),
    );
    assert_eq!(strongest["reasoning_effort"], "high");
    assert!(strongest.get("reasoning").is_none());

    let unknown = direct_provider_body(
        "upstage",
        "solar-pro3",
        Some(&serde_json::json!({"reasoning_effort": "turbo"})),
    );
    assert_eq!(unknown["reasoning_effort"], "high");

    for extra in [
        serde_json::json!({"reasoning": {"effort": "minimal"}}),
        serde_json::json!({"reasoning_config": {"enabled": false}}),
    ] {
        let body = direct_provider_body("upstage", "solar-pro3", Some(&extra));
        assert!(body.get("reasoning_effort").is_none(), "{extra}");
    }
    for model in ["solar-mini", "solar-mini-250127", "SYN-PRO"] {
        let body = direct_provider_body("upstage", model, None);
        assert!(body.get("reasoning_effort").is_none(), "{model}");
    }
    let not_mini = direct_provider_body("upstage", "solar-minix", None);
    assert_eq!(not_mini["reasoning_effort"], "medium");
}

#[test]
fn fireworks_requests_carry_upstreams_attribution_headers() {
    let header_of = |profile: &str, name: &str| {
        let provider = GenericProvider::new("https://example.invalid/v1", "key", "m")
            .with_provider_profile(profile);
        let request = provider
            .build_request(
                &Client::new(),
                "https://example.invalid/v1/chat/completions",
                "key",
                &serde_json::json!({}),
            )
            .build()
            .expect("request");
        request
            .headers()
            .get(name)
            .map(|value| value.to_str().unwrap().to_string())
    };
    for profile in ["fireworks", "fw"] {
        assert_eq!(
            header_of(profile, "HTTP-Referer").as_deref(),
            Some("https://hermes-agent.nousresearch.com")
        );
        assert_eq!(header_of(profile, "X-Title").as_deref(), Some("Hermes Agent"));
        assert_eq!(
            header_of(profile, "User-Agent"),
            Some(format!("HermesAgent/{}", env!("CARGO_PKG_VERSION")))
        );
    }
    for profile in ["deepinfra", "upstage", "openrouter"] {
        assert_eq!(header_of(profile, "X-Title"), None, "{profile}");
    }
}

#[test]
fn gmi_and_xai_requests_carry_upstreams_user_agents() {
    let user_agent = |profile: &str| {
        let provider = GenericProvider::new("https://example.invalid/v1", "key", "m")
            .with_provider_profile(profile);
        let request = provider
            .build_request(
                &Client::new(),
                "https://example.invalid/v1/chat/completions",
                "key",
                &serde_json::json!({}),
            )
            .build()
            .expect("request");
        request
            .headers()
            .get("User-Agent")
            .map(|value| value.to_str().unwrap().to_string())
    };
    for profile in ["gmi", "gmi-cloud", "gmicloud"] {
        assert_eq!(
            user_agent(profile),
            Some(format!("HermesAgent/{}", env!("CARGO_PKG_VERSION"))),
            "{profile}"
        );
    }
    for profile in ["xai", "grok", "x-ai", "x.ai"] {
        assert_eq!(
            user_agent(profile),
            Some(format!("Hermes-Agent/{}", env!("CARGO_PKG_VERSION"))),
            "{profile}"
        );
    }
    assert_eq!(user_agent("deepinfra"), None);
}

#[test]
fn nvidia_tool_messages_drop_name_and_tool_name() {
    let tool_result = Message {
        name: Some("terminal".to_string()),
        ..Message::tool_result("call-1", "done")
    };
    let messages = vec![
        Message {
            name: Some("ryan".to_string()),
            ..Message::user("hello")
        },
        tool_result,
    ];
    let body_for = |profile: &str| {
        GenericProvider::new("https://example.invalid/v1", "key", "m")
            .with_provider_profile(profile)
            .chat_request_body(ChatRequestParams {
                messages: &messages,
                tools: &[],
                max_tokens: None,
                temperature: None,
                effective_model: "nvidia/llama-3.3-70b-instruct",
                extra_body: None,
                stream: false,
            })
    };
    for profile in ["nvidia", "nvidia-nim", "nim", "build-nvidia", "nemotron"] {
        let body = body_for(profile);
        let tool = &body["messages"][1];
        assert_eq!(tool["role"], "tool", "{profile}");
        assert_eq!(tool["tool_call_id"], "call-1", "{profile}");
        assert!(tool.get("name").is_none(), "{profile}: {tool}");
        assert!(tool.get("tool_name").is_none(), "{profile}: {tool}");
        assert_eq!(body["messages"][0]["name"], "ryan", "{profile}");
    }
    assert_eq!(body_for("openrouter")["messages"][1]["name"], "terminal");
}
