use super::*;
use serde_json::json;

#[tokio::test]
async fn model_diagnostics_report_capability_gaps_and_suggest_configured_alternatives() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::model_diagnostics_contract::model_diagnostics_report_capability_gaps_and_suggest_configured_alternatives",
    ) {
        return;
    }
    let catalog = hermes_intelligence::models_dev::default_client();
    catalog.seed_cache(json!({
        "fixture-local": {
            "models": {
                "acme/brief": {"limit":{"context":8000}},
                "acme/full": {"tool_call":true,"attachment":true,"reasoning":true,
                    "limit":{"context":32000}}
            }
        }
    }));
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    Arc::make_mut(&mut app.config).llm_providers.insert(
        "fixture-local".into(),
        hermes_config::LlmProviderConfig {
            models: vec!["acme/brief".into(), "acme/full".into()],
            model_context_windows: [("ACME/FULL".to_string(), 64_000)].into(),
            ..Default::default()
        },
    );
    let requirements = ["--cap", "tools,vision,reasoning", "--min-context", "48000"];
    let mut args = vec!["explain", "another:brief", "--provider", "FIXTURE-LOCAL"];
    args.extend(requirements);
    let output = command_output(&mut app, "/model", &args).await;
    for expected in [
        "target: fixture-local:acme/brief",
        "catalog_guard: Model catalog guard remapped",
        "acp_multimodal_parts: text-only fallback",
        "missing: tools, vision, reasoning, context>=48000 (actual=8000)",
        "alternatives: fixture-local:acme/full",
    ] {
        assert!(output.contains(expected), "{output}");
    }

    args[0] = "why-not";
    assert!(matches!(
        handle_slash_command(&mut app, "/model", &args).await,
        Err(AgentError::Config(detail)) if detail == output
    ));
    let mut args = vec!["explain", "fixture-local:acme/full"];
    args.extend(requirements);
    let output = command_output(&mut app, "/model", &args).await;
    assert!(output.contains("context_window: 64000"), "{output}");
    assert!(
        output.contains("acp_multimodal_parts: supported"),
        "{output}"
    );
    assert!(output.contains("requirements: satisfied"), "{output}");
    assert!(!output.contains("alternatives:"), "{output}");

    let output = command_output(&mut app, "/model", &["why-not", "fixture-local:acme/full"]).await;
    assert!(
        output.contains("why-not mode requires constraints"),
        "{output}"
    );
    let output = command_output(
        &mut app,
        "/model",
        &[
            "explain",
            "fixture-local:acme/brief",
            "--min-context",
            "80000",
        ],
    )
    .await;
    assert!(
        output.contains("alternatives: none in provider catalog"),
        "{output}"
    );
}

#[tokio::test]
async fn model_failover_keeps_priority_order_and_clears_legacy_state() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::model_diagnostics_contract::model_failover_keeps_priority_order_and_clears_legacy_state",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    let current_model = app.current_model.clone();
    let empty = command_output(&mut app, "/model", &["failover"]).await;
    assert!(empty.contains("chain: (none)"), "{empty}");

    std::env::set_var("HERMES_FALLBACK_MODEL", "openai:legacy-fixture");
    let legacy = command_output(&mut app, "/model", &["failover", "show"]).await;
    assert!(
        legacy.contains("primary_fallback: openai:legacy-fixture"),
        "{legacy}"
    );
    let chain = [
        "openai:fixture-a",
        "anthropic:fixture-b",
        "openai:fixture-c",
    ];
    let set = command_output(
        &mut app,
        "/model",
        &[
            "failover",
            "set",
            "fixture-a, anthropic:fixture-b, OPENAI:FIXTURE-A, , fixture-c",
        ],
    )
    .await;
    assert!(
        set.contains(&format!("Failover chain set: {}", chain.join(", "))),
        "{set}"
    );
    assert_eq!(std::env::var("HERMES_FALLBACK_MODEL").unwrap(), chain[0]);
    assert_eq!(
        std::env::var("HERMES_FALLBACK_MODELS").unwrap(),
        chain.join(",")
    );
    assert_eq!(app.agent.config.retry.fallback_models, chain);
    assert_eq!(
        app.agent.config.retry.fallback_model.as_deref(),
        Some(chain[0])
    );
    let status = command_output(&mut app, "/model", &["failover", "status"]).await;
    assert!(
        status.contains(&format!("chain: {}", chain.join(", "))),
        "{status}"
    );
    assert_eq!(app.current_model, current_model);

    for (args, expected) in [
        (&["failover", "set"][..], "Usage: /model failover set"),
        (
            &["failover", "set", "  ,  "][..],
            "Failover chain cannot be empty",
        ),
    ] {
        assert!(matches!(
            handle_slash_command(&mut app, "/model", args).await,
            Err(AgentError::Config(detail)) if detail.contains(expected)
        ));
        assert_eq!(read_failover_chain_from_env(), chain);
    }
    let usage = command_output(&mut app, "/model", &["failover", "unknown"]).await;
    assert!(
        usage.contains("Usage: /model failover [status|set"),
        "{usage}"
    );
    let clear = command_output(&mut app, "/model", &["failover", "reset"]).await;
    assert!(clear.contains("Cleared retry failover chain."), "{clear}");
    assert!(std::env::var_os("HERMES_FALLBACK_MODEL").is_none());
    assert!(std::env::var_os("HERMES_FALLBACK_MODELS").is_none());
    assert!(app.agent.config.retry.fallback_model.is_none());
    assert!(app.agent.config.retry.fallback_models.is_empty());
    assert_eq!(app.current_model, current_model);
}

#[tokio::test]
async fn model_harness_and_refresh_report_offline_catalogues() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::model_diagnostics_contract::model_harness_and_refresh_report_offline_catalogues",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    app.current_model = "moa:default".into();
    let output = command_output(&mut app, "/model", &["harness"]).await;
    for expected in [
        "provider: moa",
        "selected_model: default",
        "credentials: api_key=no oauth_state=no",
        "catalog_total: 1",
        "selected_in_catalog: yes",
    ] {
        assert!(output.contains(expected), "{output}");
    }
    assert!(!output.contains("remediation:"), "{output}");

    let output = command_output(&mut app, "/model", &["harness", "MOA:missing"]).await;
    assert!(output.contains("selected_in_catalog: no"), "{output}");
    assert!(output.contains("catalog_sample: default"), "{output}");
    assert!(output.contains("/model missing --provider moa"), "{output}");
    let output = command_output(&mut app, "/model", &["harness", "fixture-unknown"]).await;
    assert!(output.contains("provider: fixture-unknown"), "{output}");
    assert!(output.contains("catalog_total: 0"), "{output}");
    assert!(!output.contains("catalog_sample:"), "{output}");

    let output = command_output(&mut app, "/model", &["refresh"]).await;
    assert!(output.contains("provider: moa"), "{output}");
    assert!(output.contains("models_sample: default"), "{output}");
    Arc::make_mut(&mut app.config).llm_providers.insert(
        "fixture-local".into(),
        hermes_config::LlmProviderConfig {
            models: vec!["fixture-a".into(), "fixture-b".into()],
            discover_models: false,
            ..Default::default()
        },
    );
    let output = command_output(&mut app, "/model", &["refresh", "FIXTURE-LOCAL:fixture-b"]).await;
    assert!(output.contains("known_provider: yes"), "{output}");
    assert!(output.contains("catalog_total: 2"), "{output}");
    assert!(
        output.contains("models_sample: fixture-a, fixture-b"),
        "{output}"
    );
    let output = command_output(&mut app, "/model", &["refresh", "fixture-unknown"]).await;
    assert!(output.contains("known_provider: no"), "{output}");
    assert!(output.contains("models_sample: (none)"), "{output}");
    assert!(output.contains("provider is not registered"), "{output}");
    assert_eq!(app.current_model, "moa:default");
}

#[tokio::test]
async fn model_backend_lists_profiles_and_applies_only_the_requested_local_preset() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::model_diagnostics_contract::model_backend_lists_profiles_and_applies_only_the_requested_local_preset",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let current = app.current_model.clone();
    for (args, expected) in [
        (&["backend"][..], "Backend best-practice profiles"),
        (&["backend", "status", "ollvm"][..], "vllm:throughput"),
        (
            &["backend", "list", "fixture-unknown"][..],
            "No backend profile presets found",
        ),
        (&["backend", "show"][..], "Usage: /model backend show"),
        (
            &["backend", "show", "vllm", "unknown"][..],
            "No backend profile found",
        ),
        (
            &["backend", "show", "vllm"][..],
            "VLLM_GPU_MEMORY_UTILIZATION=0.88",
        ),
        (&["backend", "apply"][..], "Usage: /model backend apply"),
        (
            &["backend", "apply", "vllm", "unknown"][..],
            "No backend profile found",
        ),
        (&["backend", "unknown"][..], "Usage: /model backend [list"),
    ] {
        let output = command_output(&mut app, "/model", args).await;
        assert!(output.contains(expected), "{args:?}: {output}");
    }
    assert!(std::env::var_os("HERMES_LOCAL_BACKEND_PROFILE").is_none());
    let output = command_output(
        &mut app,
        "/model",
        &["backend", "apply", "ollvm", "throughput"],
    )
    .await;
    assert!(
        output.contains("Applied backend profile vllm:throughput"),
        "{output}"
    );
    assert_eq!(
        std::env::var("HERMES_LOCAL_BACKEND_PROVIDER").unwrap(),
        "vllm"
    );
    assert_eq!(
        std::env::var("HERMES_LOCAL_BACKEND_PROFILE").unwrap(),
        "throughput"
    );
    assert_eq!(std::env::var("VLLM_MAX_NUM_SEQS").unwrap(), "256");
    let export = home.join("runtime/backend_profiles/vllm-throughput.env");
    assert!(export.is_file());
    assert!(
        output.contains(export.to_string_lossy().as_ref()),
        "{output}"
    );
    assert_eq!(app.current_model, current);
}
