    use super::*;
    use futures::stream::BoxStream;
    use hermes_core::JsonSchema;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    struct DummyProvider;
    #[async_trait::async_trait]
    impl LlmProvider for DummyProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            Ok(hermes_core::LlmResponse {
                message: Message::assistant("dummy"),
                usage: None,
                model: "dummy".into(),
                finish_reason: Some("stop".into()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    pub(crate) fn env_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env test lock poisoned")
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn isolate_route_learning_home(config: &mut AgentConfig) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        config.hermes_home = Some(tmp.path().to_string_lossy().to_string());
        tmp
    }

    #[test]
    fn tool_name_repair_preserves_call_identity_and_arguments() {
        let mut registry = ToolRegistry::new();
        registry.register(
            "fixture_count",
            ToolSchema::new(
                "fixture_count",
                "Count fixture items",
                JsonSchema::new("object"),
            ),
            Arc::new(|_| panic!("resolving a name must not run the tool")),
        );
        let mut config = AgentConfig::default();
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));

        for (name, expected, repaired) in [
            ("fixture_count", "fixture_count", false),
            ("FIXTURE_COUNT", "fixture_count", true),
            ("fixture_count_items", "fixture_count", true),
            ("other_tool", "other_tool", false),
        ] {
            let mut call = ToolCall {
                id: "fixture-call".into(),
                function: hermes_core::FunctionCall {
                    name: name.into(),
                    arguments: r#"{"limit":5}"#.into(),
                },
                extra_content: None,
            };
            assert_eq!(agent.repair_tool_call(&mut call), repaired);
            assert_eq!(call.function.name, expected);
            assert_eq!(call.id, "fixture-call");
            assert_eq!(call.function.arguments, r#"{"limit":5}"#);
        }
    }

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_turns, 250);
        assert_eq!(config.model, "gpt-5.5");
        assert!(!config.stream);
        assert_eq!(config.max_concurrent_delegates, 1);
        assert_eq!(config.max_delegate_depth, 4);
        assert_eq!(config.memory_flush_interval, 5);
        assert_eq!(config.api_mode, ApiMode::ChatCompletions);
        assert_eq!(config.retry.max_retries, 3);
        assert!(config.session_id.is_none());
        assert!(!config.skip_memory);
        assert!(!config.skip_context_files);
        assert!(config.platform.is_none());
        assert!(!config.pass_session_id);
        assert!(config.max_cost_usd.is_none());
        assert_eq!(config.cost_guard_degrade_at_ratio, 0.8);
        assert!(config.cost_guard_degrade_model.is_none());
        assert_eq!(config.checkpoint_interval_turns, 3);
        assert_eq!(config.rollback_on_tool_error_threshold, 3);
        assert!(!config.smart_model_routing.enabled);
        assert!(config.background_review_metrics_enabled);
        assert_eq!(config.stream_read_max_retries, 2);
        assert!(config.prefill_messages.is_empty());
    }

    #[tokio::test]
    async fn prefill_messages_are_model_visible_but_not_returned_for_persistence() {
        #[derive(Default)]
        struct CapturingProvider {
            seen_messages: Mutex<Vec<Message>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for CapturingProvider {
            async fn chat_completion(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                *self.seen_messages.lock().expect("seen messages lock") = messages.to_vec();
                Ok(LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "test".to_string(),
                    finish_reason: Some("stop".to_string()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let provider = std::sync::Arc::new(CapturingProvider::default());
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            prefill_messages: vec![
                Message::system("prefill system"),
                Message::user("prefill user example"),
            ],
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(
            config,
            std::sync::Arc::new(ToolRegistry::new()),
            provider.clone(),
        );

        let result = agent
            .run(vec![Message::user("real question")], Some(Vec::new()))
            .await
            .expect("agent run");

        let seen = provider.seen_messages.lock().expect("seen messages lock");
        assert!(seen
            .iter()
            .any(|m| m.content.as_deref() == Some("prefill system")));
        assert!(seen
            .iter()
            .any(|m| m.content.as_deref() == Some("prefill user example")));
        assert!(seen
            .iter()
            .any(|m| m.content.as_deref() == Some("real question")));

        assert!(!result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("prefill system")));
        assert!(!result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("prefill user example")));
        assert!(result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("real question")));
        assert!(result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("done")));
    }

    #[tokio::test]
    async fn ephemeral_system_prompt_is_model_visible_but_not_returned_for_persistence() {
        #[derive(Default)]
        struct CapturingProvider {
            seen_messages: Mutex<Vec<Message>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for CapturingProvider {
            async fn chat_completion(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                *self.seen_messages.lock().expect("seen messages lock") = messages.to_vec();
                Ok(LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "test".to_string(),
                    finish_reason: Some("stop".to_string()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let provider = std::sync::Arc::new(CapturingProvider::default());
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            ephemeral_system_prompt: Some("ephemeral test-only system prompt".to_string()),
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(
            config,
            std::sync::Arc::new(ToolRegistry::new()),
            provider.clone(),
        );

        let result = agent
            .run(vec![Message::user("real question")], Some(Vec::new()))
            .await
            .expect("agent run");

        let seen = provider.seen_messages.lock().expect("seen messages lock");
        let question = seen.iter().position(|m| m.content.as_deref() == Some("real question"));
        let extra = seen.iter().position(|m| m.content.as_deref() == Some("ephemeral test-only system prompt"));
        assert!(question.expect("user question") < extra.expect("ephemeral system prompt"));

        assert!(!result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("ephemeral test-only system prompt")));
        assert!(result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("real question")));
        assert!(result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("done")));
    }

    #[test]
    fn delegate_depth_parser_floors_without_legacy_ceiling() {
        assert_eq!(parse_delegate_depth("99"), Some(99));
        assert_eq!(parse_delegate_depth(" 12 "), Some(12));
        assert_eq!(parse_delegate_depth("1"), Some(1));
        assert_eq!(parse_delegate_depth("0"), Some(1));
        assert_eq!(parse_delegate_depth("-7"), Some(1));
        assert_eq!(parse_delegate_depth(""), None);
        assert_eq!(parse_delegate_depth("not-a-number"), None);
    }

    #[test]
    fn default_state_paths_use_the_own_home_not_upstreams() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let upstream = tmp.path().join(".hermes");
        std::fs::create_dir_all(upstream.join("memories")).expect("upstream memories");
        std::fs::write(upstream.join("memories").join("MEMORY.md"), "upstream fact")
            .expect("upstream memory");
        std::fs::create_dir_all(upstream.join("alpha")).expect("upstream alpha");
        std::fs::write(
            upstream.join("alpha").join("claim_verifier_policy.json"),
            r#"{"enabled": false}"#,
        )
        .expect("upstream policy");
        let _home = EnvVarGuard::set("HOME", &tmp.path().to_string_lossy());
        let _hermes_home = EnvVarGuard::remove("HERMES_HOME");
        let _ultra_home = EnvVarGuard::remove("HERMES_AGENT_ULTRA_HOME");
        let _verifier = EnvVarGuard::remove("HERMES_CLAIM_VERIFIER_ENABLED");
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        let own = tmp.path().join(".hermes-agent-ultra");
        let checks = [
            (
                "hook spills",
                agent.hook_context_spill_dir() == own.join("hooks").join("spills"),
            ),
            (
                "auth tokens",
                agent.auth_tokens_path() == own.join("auth").join("tokens.json"),
            ),
            (
                "runtime ledger",
                agent.objective_runtime_ledger_path()
                    == own.join("alpha").join("objective_runtime_ledger.jsonl"),
            ),
            (
                "eval trend",
                agent.objective_eval_trend_path()
                    == own.join("alpha").join("objective_eval_trend.json"),
            ),
            (
                "memory snapshot",
                crate::context::load_builtin_memory_snapshot(None) == (None, None),
            ),
            ("claim verifier policy", claim_verifier_enabled_runtime()),
        ];
        let wrong: Vec<_> = checks.iter().filter(|(_, ok)| !ok).map(|(name, _)| *name).collect();
        assert!(wrong.is_empty(), "read or written outside the own home: {wrong:?}");
    }

    #[test]
    fn max_delegate_depth_resolves_env_then_config_with_floor() {
        let _guard = env_test_lock();
        let _env = EnvVarGuard::set("HERMES_MAX_DELEGATE_DEPTH", "99");
        let config = AgentConfig {
            max_delegate_depth: 2,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config.clone(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        assert_eq!(agent.resolve_max_delegate_depth(), 99);
        drop(_env);

        let _env = EnvVarGuard::set("HERMES_MAX_DELEGATE_DEPTH", "0");
        assert_eq!(agent.resolve_max_delegate_depth(), 1);
        drop(_env);

        let _env = EnvVarGuard::remove("HERMES_MAX_DELEGATE_DEPTH");
        let config = AgentConfig {
            max_delegate_depth: 0,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        assert_eq!(agent.resolve_max_delegate_depth(), 1);
    }

    #[test]
    fn delegation_spawning_paused_honors_env_toggle() {
        std::env::remove_var("HERMES_DELEGATION_PAUSED");
        assert!(!delegation_spawning_paused());
        std::env::set_var("HERMES_DELEGATION_PAUSED", "1");
        assert!(delegation_spawning_paused());
        std::env::set_var("HERMES_DELEGATION_PAUSED", "true");
        assert!(delegation_spawning_paused());
        std::env::set_var("HERMES_DELEGATION_PAUSED", "0");
        assert!(!delegation_spawning_paused());
    }

    #[test]
    fn tool_enforcement_prompt_gate_applies_to_non_openai_model_names() {
        assert!(should_inject_tool_enforcement_for_model(
            "nous:nousresearch/hermes-4-70b"
        ));
        assert!(should_inject_tool_enforcement_for_model(
            "openrouter:moonshotai/kimi-k2.6"
        ));
        assert!(should_inject_tool_enforcement_for_model(
            "anthropic:claude-3-7-sonnet"
        ));
    }

    #[test]
    fn summarize_background_review_nothing_to_save() {
        let msgs = vec![Message::assistant("Nothing to save.")];
        let out = summarize_background_review_result(&msgs);
        assert!(out.is_none());
    }

    #[test]
    fn classify_error_404_generic_is_retryable() {
        assert_eq!(classify_error("HTTP 404 Not Found"), ErrorClass::Retryable);
        assert_eq!(
            classify_error("gateway returned not found"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_404_model_not_found_is_fatal() {
        assert_eq!(
            classify_error("404 model not found: foo/bar"),
            ErrorClass::Fatal
        );
        assert_eq!(
            classify_error("invalid model: gpt-unknown"),
            ErrorClass::Fatal
        );
    }

    #[test]
    fn classify_error_openrouter_privacy_guardrail_is_fatal() {
        assert_eq!(
            classify_error("HTTP 404: OpenRouter privacy guardrail blocked this endpoint"),
            ErrorClass::Fatal
        );
    }

    #[test]
    fn classify_error_ssl_bad_record_mac_is_retryable() {
        assert_eq!(
            classify_error("[SSL: BAD_RECORD_MAC] sslv3 alert bad record mac (_ssl.c:2580)"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_ssl_openssl_token_form_is_retryable() {
        assert_eq!(
            classify_error("ERR_SSL_SSL/TLS_ALERT_BAD_RECORD_MAC during streaming"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_plain_disconnect_stays_retryable() {
        assert_eq!(
            classify_error("Server disconnected without sending a response"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_context_overflow_contracts_include_413_and_generic_400_text() {
        for err in [
            "HTTP 413 payload too large",
            "API error 400: request too large: max tokens per request is 200000",
            "prompt is too long: context length 300000 exceeds max of 200000",
            "Please reduce the length of the messages",
            "The input exceeds the context window",
        ] {
            assert_eq!(classify_error(err), ErrorClass::ContextOverflow, "{err}");
        }
    }

    #[test]
    fn tool_payload_validation_error_detector_matches_known_provider_signatures() {
        let strict_shape = "API error 400 Bad Request: Invalid input: expected \"function\"";
        assert!(is_tool_payload_validation_error(strict_shape));
        let provider_generic = "API error 400 Bad Request: This request is not valid. Check the model name and other parameters. Additional info: Provider returned error";
        assert!(is_tool_payload_validation_error(provider_generic));
        let no_choices_provider_shape = "No choices in response (status=400; message=This request is not valid. Check the model name and other parameters. Additional info: Provider returned error)";
        assert!(is_tool_payload_validation_error(no_choices_provider_shape));
        let unprocessable_payload =
            "API error 422 Unprocessable Entity: Check that you're sending a valid payload";
        assert!(is_tool_payload_validation_error(unprocessable_payload));
        assert!(!is_tool_payload_validation_error(
            "API error 400 Bad Request: max_tokens must be positive"
        ));
    }

    #[test]
    fn preferred_tool_payload_fallback_model_defaults_and_override() {
        assert_eq!(
            preferred_tool_payload_fallback_model("nous", "openai/gpt-5.5"),
            Some("nousresearch/hermes-4-70b".to_string())
        );
        assert_eq!(
            preferred_tool_payload_fallback_model("nous-api", "openai/gpt-5.5"),
            Some("nousresearch/hermes-4-70b".to_string())
        );
        assert_eq!(
            preferred_tool_payload_fallback_model("openrouter", "openai/gpt-5.5"),
            None
        );
        std::env::set_var(
            "HERMES_TOOL_PAYLOAD_FALLBACK_MODEL",
            "nousresearch/hermes-4-405b",
        );
        assert_eq!(
            preferred_tool_payload_fallback_model("nous-portal-api", "openai/gpt-5.5"),
            Some("nousresearch/hermes-4-405b".to_string())
        );
        std::env::remove_var("HERMES_TOOL_PAYLOAD_FALLBACK_MODEL");
    }

    #[test]
    fn maybe_nous_401_diagnostic_returns_hint_for_nous_auth_failures() {
        let diag = maybe_nous_401_diagnostic(
            "nous",
            "HTTP 401 Unauthorized: token expired",
            Some("/tmp/hermes-home"),
        )
        .expect("nous 401 should produce diagnostics");
        assert!(diag.contains("Nous 401 — Portal authentication failed."));
        assert!(diag.contains("hermes auth add nous"));
        assert!(diag.contains("portal.nousresearch.com"));
        assert!(diag.contains("/tmp/hermes-home/auth.json"));
    }

    #[test]
    fn maybe_nous_401_diagnostic_ignores_non_nous_provider() {
        let diag = maybe_nous_401_diagnostic(
            "openrouter",
            "HTTP 401 Unauthorized: token expired",
            Some("/tmp/hermes-home"),
        );
        assert!(diag.is_none());
    }

    #[test]
    fn maybe_nous_401_diagnostic_ignores_non_auth_errors() {
        let diag = maybe_nous_401_diagnostic("nous", "HTTP 500 upstream timeout", None);
        assert!(diag.is_none());
    }

    #[test]
    fn summarize_background_review_counts_tool_calls() {
        let msgs = vec![
            Message::tool_result(
                "tc_mem",
                "{\"success\":true,\"message\":\"Skill 'prospect-scanner' created.\"}",
            ),
            Message::tool_result(
                "tc_skill",
                "{\"success\":true,\"message\":\"Entry added\",\"target\":\"memory\"}",
            ),
            Message::tool_result("tc_skip", "{\"success\":false,\"message\":\"failed\"}"),
        ];
        let out = summarize_background_review_result(&msgs).expect("summary should exist");
        assert!(out.starts_with("💾 "));
        assert!(out.contains("Skill 'prospect-scanner' created."));
        assert!(out.contains("Memory updated"));
    }

    #[test]
    fn summarize_background_review_filters_status_and_secret_like_text() {
        let msgs = vec![
            Message::tool_result(
                "tc_safe",
                "{\"success\":true,\"message\":\"created docs/repo-review-notes.md\"}",
            ),
            Message::tool_result(
                "tc_status",
                "{\"success\":true,\"message\":\"status=ok token refreshed\"}",
            ),
            Message::tool_result(
                "tc_obj",
                "{\"success\":true,\"message\":\"{\\\"message\\\":\\\"updated config\\\"}\"}",
            ),
        ];
        let out = summarize_background_review_result(&msgs).expect("summary should exist");
        assert!(out.contains("created docs/repo-review-notes.md"));
        assert!(!out.contains("status=ok token refreshed"));
        assert!(!out.contains("{\"message\""));
    }

    #[test]
    fn exploratory_hint_enabled_for_repo_exploration_intent() {
        let msgs = vec![Message::user(
            "Deeply audit /Users/sheawinkler/Documents/Projects/hermes-agent-ultra/crates/hermes-agent/src/agent_loop.rs and diagnose root cause.",
        )];
        let hint = exploratory_problem_solving_system_hint(&msgs).expect("hint should exist");
        assert!(hint.contains("Exploratory problem-solving protocol active"));
        assert!(hint.contains("workstream=<name>"));
    }

    #[test]
    fn exploratory_hint_disabled_for_non_exploratory_repo_intent() {
        let msgs = vec![Message::user(
            "Implement this fix directly in /Users/sheawinkler/Documents/Projects/hermes-agent-ultra/src/main.rs.",
        )];
        assert!(exploratory_problem_solving_system_hint(&msgs).is_none());
    }

    #[test]
    fn hook_context_spill_writes_file_for_oversized_payload() {
        let prev = std::env::var("HERMES_HOOK_CONTEXT_SPILL_CHARS").ok();
        std::env::set_var("HERMES_HOOK_CONTEXT_SPILL_CHARS", "1024");
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = AgentConfig {
            hermes_home: Some(tmp.path().display().to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider));
        let large = "x".repeat(2_048);
        let spilled = agent
            .spill_hook_context_if_oversized(&large)
            .expect("spill should write file");
        assert!(spilled.exists(), "spill file should exist");
        let read_back = std::fs::read_to_string(&spilled).expect("read spill file");
        assert_eq!(read_back.len(), large.len());
        assert!(
            agent
                .spill_hook_context_if_oversized("small payload")
                .is_none(),
            "small payload must not spill"
        );
        if let Some(v) = prev {
            std::env::set_var("HERMES_HOOK_CONTEXT_SPILL_CHARS", v);
        } else {
            std::env::remove_var("HERMES_HOOK_CONTEXT_SPILL_CHARS");
        }
    }

    #[test]
    fn post_llm_transform_hook_rewrites_assistant_content() {
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut content = Some("before".to_string());
        agent.apply_hook_output_transforms(
            &[HookResult::TransformLlmOutput("after".to_string())],
            &mut content,
        );
        assert_eq!(content.as_deref(), Some("after"));
    }

    #[test]
    fn preflight_compression_status_reports_skipped_when_under_threshold() {
        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("small"));
        agent.preflight_context_compress_with_status(&mut ctx);

        let rows = captured.lock().expect("captured lock");
        assert!(rows
            .iter()
            .any(|(kind, msg)| { kind == "lifecycle" && msg.contains("no compression needed") }));
    }

    #[test]
    fn preflight_compression_status_reports_when_compressing() {
        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(95)));
        agent.preflight_context_compress_with_status(&mut ctx);

        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("compressing before first turn")
        }));
        assert!(rows
            .iter()
            .any(|(kind, msg)| kind == "lifecycle" && msg.contains("compression complete")));
    }

    #[test]
    fn status_callback_receives_context_pressure_messages() {
        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_callbacks(callbacks);

        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx);

        let rows = captured.lock().expect("captured lock");
        assert!(rows
            .iter()
            .any(|(kind, msg)| kind == "lifecycle" && msg.contains("triggering compression")));
    }

    #[test]
    fn compressing_runs_return_the_turns_they_replaced() {
        // Compression writes a checkpoint over HTTP; send it to a closed
        // local port so the test stays offline.
        let _lock = env_test_lock();
        let _url = EnvVarGuard::set("CONTEXTLATTICE_ORCHESTRATOR_URL", "http://127.0.0.1:9");
        let _key = EnvVarGuard::remove("CONTEXTLATTICE_API_KEY");
        let _mode = EnvVarGuard::set("HERMES_CONTEXTLATTICE_COMPACTION_GOVERNANCE", "advisory");
        struct StopProvider;
        #[async_trait::async_trait]
        impl LlmProvider for StopProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("by the mill"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::iter(vec![
                    Ok(stream_chunk_content("by the mill")),
                    Ok(stream_chunk_finish("stop")),
                ])
                .boxed()
            }
        }

        // Over 80% of the default 200,000-character budget, so each run
        // compresses before its first model call.
        let mut history: Vec<Message> = (0..20)
            .flat_map(|turn| {
                [
                    Message::user(format!("heron question {turn} {}", "x".repeat(5_000))),
                    Message::assistant(format!("willow answer {turn} {}", "y".repeat(4_000))),
                ]
            })
            .collect();
        history.push(Message::user("and the kingfisher"));
        let config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), Arc::new(StopProvider));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let results = [
            rt.block_on(agent.run(history.clone(), None)).expect("run"),
            rt.block_on(agent.run_stream(history.clone(), None, Some(Box::new(|_| {}))))
                .expect("streamed run"),
        ];
        for result in results {
            assert_eq!(result.compressed_transcripts.len(), 1);
            let replaced = &result.compressed_transcripts[0];
            assert!(replaced.contains(&history[0]), "the replaced turns are returned");
            assert!(!result.messages.contains(&history[0]), "the run's context was compressed");
        }
    }

    #[test]
    fn compressing_runs_keep_the_prefill_and_the_persisted_user_message() {
        // Compression writes a checkpoint over HTTP; send it to a closed
        // local port so the test stays offline.
        let _lock = env_test_lock();
        let _url = EnvVarGuard::set("CONTEXTLATTICE_ORCHESTRATOR_URL", "http://127.0.0.1:9");
        let _key = EnvVarGuard::remove("CONTEXTLATTICE_API_KEY");
        let _mode = EnvVarGuard::set("HERMES_CONTEXTLATTICE_COMPACTION_GOVERNANCE", "advisory");
        #[derive(Default)]
        struct CapturingProvider {
            seen: Mutex<Vec<Vec<Message>>>,
        }
        #[async_trait::async_trait]
        impl LlmProvider for CapturingProvider {
            async fn chat_completion(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                self.seen.lock().expect("seen lock").push(messages.to_vec());
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("by the mill"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                self.seen.lock().expect("seen lock").push(messages.to_vec());
                futures::stream::iter(vec![
                    Ok(stream_chunk_content("by the mill")),
                    Ok(stream_chunk_finish("stop")),
                ])
                .boxed()
            }
        }
        fn has(messages: &[Message], text: &str) -> bool {
            messages.iter().any(|m| m.content.as_deref() == Some(text))
        }

        // Over 80% of the default 200,000-character budget, so each run
        // compresses before its first model call.
        let mut history: Vec<Message> = (0..20)
            .flat_map(|turn| {
                [
                    Message::user(format!("heron question {turn} {}", "x".repeat(5_000))),
                    Message::assistant(format!("willow answer {turn} {}", "y".repeat(4_000))),
                ]
            })
            .collect();
        history.push(Message::user("and the kingfisher"));
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            prefill_messages: vec![
                Message::user("prefill question"),
                Message::assistant("prefill answer"),
            ],
            persist_user_message: Some("kingfisher, as typed".to_string()),
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(CapturingProvider::default());
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone());

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let results = [
            rt.block_on(agent.run(history.clone(), None)).expect("run"),
            rt.block_on(agent.run_stream(history.clone(), None, Some(Box::new(|_| {}))))
                .expect("streamed run"),
        ];
        let seen = provider.seen.lock().expect("seen lock");
        assert_eq!(seen.len(), 2, "one model call per run");
        for (sent, result) in seen.iter().zip(&results) {
            assert_eq!(result.compressed_transcripts.len(), 1, "the run compressed");
            for prefill in ["prefill question", "prefill answer"] {
                assert!(has(sent, prefill), "the model still sees {prefill:?}");
                assert!(!has(&result.messages, prefill), "{prefill:?} is not returned");
                assert!(
                    !result.compressed_transcripts.iter().any(|t| has(t, prefill)),
                    "{prefill:?} is not returned as a compressed turn"
                );
            }
            assert!(
                result.compressed_transcripts[0].contains(&history[0]),
                "the replaced turns are returned"
            );
            assert!(
                result.messages.iter().any(|m| m
                    .content
                    .as_deref()
                    .is_some_and(|c| c.starts_with("[Conversation summary]"))),
                "the summary is returned"
            );
            assert!(has(&result.messages, "by the mill"));
            let last_user = result
                .messages
                .iter()
                .rfind(|m| m.role == MessageRole::User)
                .and_then(|m| m.content.as_deref());
            assert_eq!(last_user, Some("kingfisher, as typed"));
        }

        // Once the run is saved, session search does not find the prefill.
        let store = tempfile::tempdir().expect("tempdir");
        let sp = crate::session_persistence::SessionPersistence::new(store.path());
        sp.persist_agent_result("live", &results[0], None, None)
            .expect("persist");
        let conn = rusqlite::Connection::open(sp.db_path()).expect("open db");
        let hits = |phrase: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1",
                [format!("\"{phrase}\"")],
                |row| row.get(0),
            )
            .expect("search")
        };
        assert!(hits("heron question") > 0, "the replaced turns are found");
        for prefill in ["prefill question", "prefill answer"] {
            assert_eq!(hits(prefill), 0, "session search finds {prefill:?}");
        }
    }

    #[test]
    fn runs_store_only_the_tool_calls_they_answer() {
        // The model repeats a call. The run executes it once, so the stored
        // assistant message, and its replayed Anthropic blocks, keep one call.
        fn repeated_call(id: &str) -> hermes_core::ToolCall {
            hermes_core::ToolCall {
                id: id.to_string(),
                function: hermes_core::FunctionCall {
                    name: "echo".to_string(),
                    arguments: "{}".to_string(),
                },
                extra_content: None,
            }
        }
        fn answered(messages: &[Message]) -> bool {
            messages.iter().any(|m| m.role == MessageRole::Tool)
        }
        #[derive(Default)]
        struct RepeatingProvider {
            seen: Mutex<Vec<Vec<Message>>>,
        }
        #[async_trait::async_trait]
        impl LlmProvider for RepeatingProvider {
            async fn chat_completion(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                self.seen.lock().expect("seen lock").push(messages.to_vec());
                let message = if answered(messages) {
                    Message::assistant("done")
                } else {
                    let mut message = Message::assistant_with_tool_calls(
                        None,
                        vec![repeated_call("call_a"), repeated_call("call_b")],
                    );
                    message.anthropic_content_blocks = Some(vec![
                        serde_json::json!({"type": "thinking", "thinking": "echo", "signature": "sig"}),
                        serde_json::json!({"type": "tool_use", "id": "call_a", "name": "echo", "input": {}}),
                        serde_json::json!({"type": "tool_use", "id": "call_b", "name": "echo", "input": {}}),
                    ]);
                    message
                };
                Ok(hermes_core::LlmResponse {
                    message,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                self.seen.lock().expect("seen lock").push(messages.to_vec());
                let chunks = if answered(messages) {
                    vec![stream_chunk_content("done"), stream_chunk_finish("stop")]
                } else {
                    vec![
                        stream_chunk_tool_name(0, "call_a", "echo"),
                        stream_chunk_tool_args(0, "{}"),
                        stream_chunk_tool_name(1, "call_b", "echo"),
                        stream_chunk_tool_args(1, "{}"),
                        stream_chunk_finish("tool_calls"),
                    ]
                };
                futures::stream::iter(chunks.into_iter().map(Ok)).boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "echo",
            ToolSchema::new("echo", "Echo", JsonSchema::new("object")),
            Arc::new(|_args| Ok("ok".to_string())),
        );
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(RepeatingProvider::default());
        let agent = AgentLoop::new(config, Arc::new(registry), provider.clone());

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let results = [
            rt.block_on(agent.run(vec![Message::user("echo twice")], None))
                .expect("run"),
            rt.block_on(agent.run_stream(
                vec![Message::user("echo twice")],
                None,
                Some(Box::new(|_| {})),
            ))
            .expect("streamed run"),
        ];
        let seen = provider.seen.lock().expect("seen lock");
        let sent = seen.iter().map(Vec::as_slice);
        for messages in sent.chain(results.iter().map(|r| r.messages.as_slice())) {
            let calls: Vec<&str> = messages
                .iter()
                .flat_map(|m| m.tool_calls.iter().flatten())
                .map(|c| c.id.as_str())
                .collect();
            let answers: Vec<&str> = messages
                .iter()
                .filter(|m| m.role == MessageRole::Tool)
                .filter_map(|m| m.tool_call_id.as_deref())
                .collect();
            assert_eq!(calls, answers, "each stored call has its result: {messages:?}");
        }
        assert!(results.iter().all(|r| answered(&r.messages)), "the call ran");
        let replayed: Vec<&str> = results[0]
            .messages
            .iter()
            .flat_map(|m| m.anthropic_content_blocks.iter().flatten())
            .filter(|block| block["type"] == "tool_use")
            .filter_map(|block| block["id"].as_str())
            .collect();
        assert_eq!(replayed, ["call_a"]);
    }

    #[tokio::test]
    async fn an_interrupt_after_the_pre_model_hook_keeps_the_user_message() {
        use crate::plugins::{Plugin, PluginContext, PluginMeta};

        struct InterruptBeforeModel(crate::interrupt::InterruptController);

        #[async_trait::async_trait]
        impl Plugin for InterruptBeforeModel {
            fn meta(&self) -> PluginMeta {
                PluginMeta {
                    name: "interrupt-before-model".into(),
                    version: "0.1.0".into(),
                    description: "Requests cancellation from the pre-model hook".into(),
                    author: None,
                }
            }

            async fn initialize(&self) -> Result<(), AgentError> {
                Ok(())
            }

            async fn shutdown(&self) -> Result<(), AgentError> {
                Ok(())
            }

            fn register(&self, context: &mut PluginContext) {
                let interrupt = self.0.clone();
                context.on(
                    HookType::PreLlmCall,
                    Arc::new(move |_| {
                        interrupt.interrupt(None);
                        HookResult::Ok
                    }),
                );
            }
        }

        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider));
        let mut plugins = PluginManager::new();
        plugins.register(Arc::new(InterruptBeforeModel(agent.interrupt.clone())));
        let agent = agent.with_plugins(Arc::new(Mutex::new(plugins)));
        let user = Message::user("Summarise the field notes");
        let result = agent.run(vec![user.clone()], None).await.unwrap();

        assert!(result.interrupted);
        assert!(!result.finished_naturally);
        assert!(result.messages.contains(&user));
        assert!(!result.messages.iter().any(|message| {
            matches!(message.role, MessageRole::Assistant | MessageRole::Tool)
        }));
    }

    #[test]
    fn an_interrupt_before_the_tool_batch_keeps_no_unanswered_call() {
        // The user interrupts whilst the model answers with a call, so the
        // run stops before the batch runs.
        #[derive(Default)]
        struct InterruptingProvider {
            interrupt: OnceLock<crate::interrupt::InterruptController>,
        }
        #[async_trait::async_trait]
        impl LlmProvider for InterruptingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                self.interrupt.get().expect("interrupt set").interrupt(None);
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant_with_tool_calls(
                        None,
                        vec![hermes_core::ToolCall {
                            id: "call_echo".to_string(),
                            function: hermes_core::FunctionCall {
                                name: "echo".to_string(),
                                arguments: "{}".to_string(),
                            },
                            extra_content: None,
                        }],
                    ),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("tool_calls".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "echo",
            ToolSchema::new("echo", "Echo", JsonSchema::new("object")),
            Arc::new(|_args| Ok("ok".to_string())),
        );
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(InterruptingProvider::default());
        let agent = AgentLoop::new(config, Arc::new(registry), provider.clone());
        provider
            .interrupt
            .set(agent.interrupt.clone())
            .expect("interrupt unset");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let result = rt
            .block_on(agent.run(vec![Message::user("echo")], None))
            .expect("run");
        assert!(result.interrupted);
        let stored_calls = result
            .messages
            .iter()
            .flat_map(|m| m.tool_calls.iter().flatten())
            .count();
        assert_eq!(stored_calls, 0, "{:?}", result.messages);
        assert!(!result.messages.iter().any(|m| m.role == MessageRole::Tool));
    }

    #[test]
    fn a_tool_that_panics_still_answers_its_call() {
        struct PanicToolProvider;
        #[async_trait::async_trait]
        impl LlmProvider for PanicToolProvider {
            async fn chat_completion(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let message = if messages.iter().any(|m| m.role == MessageRole::Tool) {
                    Message::assistant("done")
                } else {
                    Message::assistant_with_tool_calls(
                        None,
                        vec![hermes_core::ToolCall {
                            id: "call_boom".to_string(),
                            function: hermes_core::FunctionCall {
                                name: "boom".to_string(),
                                arguments: "{}".to_string(),
                            },
                            extra_content: None,
                        }],
                    )
                };
                Ok(hermes_core::LlmResponse {
                    message,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "boom",
            ToolSchema::new("boom", "Panics", JsonSchema::new("object")),
            Arc::new(|_args| -> Result<String, ToolError> { panic!("the boom tool panicked") }),
        );
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            max_turns: 4,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(PanicToolProvider));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let result = rt
            .block_on(agent.run(vec![Message::user("boom")], None))
            .expect("run");
        let answers: Vec<&str> = result
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect();
        assert_eq!(answers, ["call_boom"], "{:?}", result.messages);
        let last = result.messages.last().and_then(|m| m.content.as_deref());
        assert_eq!(last, Some("done"));
    }

    #[test]
    fn a_turn_whose_calls_are_all_dropped_keeps_only_its_text() {
        let call = hermes_core::ToolCall {
            id: "call_a".to_string(),
            function: hermes_core::FunctionCall {
                name: "echo".to_string(),
                arguments: "{}".to_string(),
            },
            extra_content: None,
        };
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::user("echo"));
        ctx.add_message(Message::assistant_with_tool_calls(None, vec![call.clone()]));
        ctx.add_message(Message::system("policy note"));
        AgentLoop::sync_stored_tool_calls(&mut ctx, 1, &[]);
        let roles: Vec<_> = ctx.get_messages().iter().map(|m| m.role).collect();
        assert_eq!(roles, [MessageRole::User, MessageRole::System]);

        ctx.add_message(Message::assistant_with_tool_calls(
            Some("checking".to_string()),
            vec![call],
        ));
        AgentLoop::sync_stored_tool_calls(&mut ctx, 2, &[]);
        let kept = &ctx.get_messages()[2];
        assert_eq!(kept.content.as_deref(), Some("checking"));
        assert_eq!(kept.tool_calls, None);
    }

    #[test]
    fn replayed_tool_use_blocks_follow_the_calls_that_run() {
        let call = |id: &str, name: &str, arguments: &str| hermes_core::ToolCall {
            id: id.to_string(),
            function: hermes_core::FunctionCall {
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
            extra_content: None,
        };
        let mut stored = Message::assistant_with_tool_calls(
            None,
            vec![call("call_a", "Echo", r#"{"n":1}"#), call("call_b", "echo", "{}")],
        );
        let thinking = serde_json::json!({"type": "thinking", "thinking": "t", "signature": "s"});
        stored.anthropic_content_blocks = Some(vec![
            thinking.clone(),
            serde_json::json!({"type": "tool_use", "id": "call_a", "name": "Echo", "input": {"n": 1}}),
            serde_json::json!({"type": "tool_use", "id": "call_b", "name": "echo", "input": {}}),
        ]);
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::user("echo"));
        ctx.add_message(stored);

        // call_a ran with a repaired name and rewritten arguments; call_b was dropped.
        let ran = call("call_a", "echo", r#"{"n":2}"#);
        AgentLoop::sync_stored_tool_calls(&mut ctx, 1, std::slice::from_ref(&ran));
        let stored = &ctx.get_messages()[1];
        assert_eq!(stored.tool_calls, Some(vec![ran]));
        assert_eq!(
            stored.anthropic_content_blocks,
            Some(vec![
                thinking,
                serde_json::json!({"type": "tool_use", "id": "call_a", "name": "echo", "input": {"n": 2}}),
            ])
        );
    }

    #[test]
    fn runs_finish_when_every_call_is_dropped() {
        // Delegation is paused, so the model's only call is dropped; a call to
        // a missing tool ends the run after the retries; a streamed text-format
        // call has no name, so it never runs.
        let _lock = env_test_lock();
        let _paused = EnvVarGuard::set("HERMES_DELEGATION_PAUSED", "1");
        const NOTE: &str = "delegation is unavailable";
        const MISSING: &str = "use a missing tool";
        fn has(messages: &[Message], text: &str) -> bool {
            messages.iter().any(|m| m.content.as_deref() == Some(text))
        }
        struct DroppedCallProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DroppedCallProvider {
            async fn chat_completion(
                &self,
                messages: &[Message],
                tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                // With no tools offered (the max-turns summary), answer in text.
                let noted = tools.is_empty()
                    || messages
                    .iter()
                    .any(|m| m.content.as_deref().is_some_and(|c| c.contains(NOTE)));
                let tool = if has(messages, MISSING) {
                    "missing_tool"
                } else {
                    "delegate_task"
                };
                let message = if noted {
                    Message::assistant("done")
                } else {
                    Message::assistant_with_tool_calls(
                        None,
                        vec![hermes_core::ToolCall {
                            id: format!("call_{tool}"),
                            function: hermes_core::FunctionCall {
                                name: tool.to_string(),
                                arguments: r#"{"goal":"count the herons"}"#.to_string(),
                            },
                            extra_content: None,
                        }],
                    )
                };
                Ok(hermes_core::LlmResponse {
                    message,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::iter(vec![
                    Ok(stream_chunk_content(
                        r#"done <tool_call>{"name": "", "arguments": {}}</tool_call>"#,
                    )),
                    Ok(stream_chunk_finish("stop")),
                ])
                .boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "delegate_task",
            ToolSchema::new("delegate_task", "Delegate", JsonSchema::new("object")),
            Arc::new(|_args| Ok("delegated".to_string())),
        );
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            max_turns: 4,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DroppedCallProvider));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let delegated = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("run");
        let missing = rt
            .block_on(agent.run(vec![Message::user(MISSING)], None))
            .expect("run with a missing tool");
        let streamed = rt
            .block_on(agent.run_stream(
                vec![Message::user("hello")],
                None,
                Some(Box::new(|_| {})),
            ))
            .expect("streamed run");
        for result in [&delegated, &missing, &streamed] {
            let calls: Vec<&str> = result
                .messages
                .iter()
                .flat_map(|m| m.tool_calls.iter().flatten())
                .map(|c| c.id.as_str())
                .collect();
            let answers: Vec<&str> = result
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::Tool)
                .filter_map(|m| m.tool_call_id.as_deref())
                .collect();
            assert_eq!(calls, answers, "{:?}", result.messages);
        }
        assert!(!missing.finished_naturally, "the retries ran out");
        for result in [&delegated, &streamed] {
            assert!(result.finished_naturally, "{:?}", result.messages);
            let last = result.messages.last().and_then(|m| m.content.as_deref());
            assert_eq!(last, Some("done"));
        }
    }

    include!("tests/llm_retry_stream.rs");
    include!("tests/oauth_unauthorized_refresh.rs");
    include!("tests/prompt_cache_prefix.rs");
    include!("tests/prompt_cache_lifecycle.rs");

    #[test]
    fn quiet_mode_suppresses_status_callback() {
        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            quiet_mode: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider))
            .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx);

        assert!(captured.lock().expect("captured lock").is_empty());
    }

    #[test]
    fn test_builtin_personality_injected_into_system_prompt() {
        // An empty home, so a real SOUL.md or personalities/coder.md cannot change the prompt.
        let home = tempfile::tempdir().expect("tempdir");
        let config = AgentConfig {
            personality: Some("coder".to_string()),
            hermes_home: Some(home.path().to_string_lossy().into_owned()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(prompt.contains("## Active Personality (coder)"));
        assert!(prompt.contains("`coder` persona"));
    }

    #[test]
    fn test_unknown_personality_name_does_not_add_overlay_block() {
        // An empty home, so a real SOUL.md cannot replace the default identity.
        let home = tempfile::tempdir().expect("tempdir");
        let config = AgentConfig {
            personality: Some("unknown_persona".to_string()),
            hermes_home: Some(home.path().to_string_lossy().into_owned()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(!prompt.contains("## Active Personality (unknown_persona)"));
        assert!(prompt.contains("You are Hermes Agent"));
    }

    #[test]
    fn test_default_personality_name_does_not_add_overlay_block() {
        // An empty home, so a real SOUL.md cannot replace the default identity.
        let home = tempfile::tempdir().expect("tempdir");
        let config = AgentConfig {
            personality: Some("default".to_string()),
            hermes_home: Some(home.path().to_string_lossy().into_owned()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(!prompt.contains("## Active Personality (default)"));
        assert!(prompt.contains("You are Hermes Agent"));
    }

    #[test]
    fn blank_stored_prompt_builds_from_local_settings_and_available_tools() {
        let _lock = env_test_lock();
        let _enforcement = EnvVarGuard::remove("HERMES_DISABLE_TOOL_ENFORCEMENT_PROMPT");
        let home = tempfile::tempdir().unwrap();
        let memories = home.path().join("memories");
        std::fs::create_dir(&memories).unwrap();
        std::fs::write(memories.join("USER.md"), "Prefers a weekly summary.").unwrap();
        let config = AgentConfig {
            model: "alibaba/qwen-fixture".to_string(),
            provider: Some("alibaba".into()),
            personality: Some("Patient field archivist".into()),
            platform: Some("email".into()),
            session_id: Some("  weekly-summary  ".into()),
            pass_session_id: true,
            stored_system_prompt: Some(" \t\n".into()),
            code_index_enabled: false,
            hermes_home: Some(home.path().to_str().unwrap().to_string()),
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let schemas = [ToolSchema::new(
            "contextlattice_search",
            "Look up local notes",
            JsonSchema::new("object"),
        )];
        let (prompt, restored) = agent.resolve_initial_system_prompt("", &schemas);

        assert!(!restored);
        for expected in [
            "Personality: Patient field archivist",
            "Prefers a weekly summary.",
            "Session ID: weekly-summary",
            "You are powered by the model named qwen-fixture.",
            "The exact model ID is alibaba/qwen-fixture.",
            "You are responding over email.",
            CONTEXTLATTICE_OPERATIONAL_GUIDANCE,
        ] {
            assert!(
                prompt.contains(expected),
                "missing prompt section: {expected}"
            );
        }
        assert!(!agent
            .build_system_prompt("", &[], "alibaba/qwen-fixture")
            .contains(CONTEXTLATTICE_OPERATIONAL_GUIDANCE));
        assert!(agent
            .build_system_prompt("", &schemas, "gemini-fixture")
            .contains(GOOGLE_MODEL_OPERATIONAL_GUIDANCE));
    }

    #[test]
    fn steer_channel_note_is_gated_on_tool_availability() {
        use hermes_core::JsonSchema;

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let without_tools = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(!without_tools.contains("## Mid-turn user steering"));

        let tool_schemas = vec![ToolSchema::new(
            "terminal",
            "Terminal",
            JsonSchema::new("object"),
        )];
        let with_tools = agent.build_system_prompt("", &tool_schemas, "gpt-4o");
        assert!(with_tools.contains(STEER_CHANNEL_NOTE));
        assert!(with_tools.contains(crate::steer::STEER_MARKER_OPEN));
        assert!(with_tools.contains(crate::steer::STEER_MARKER_CLOSE));
    }

    #[test]
    fn coding_context_prompt_injected_for_cli_workspace_with_tools() {
        use hermes_core::JsonSchema;

        let _lock = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .expect("write manifest");
        let _cwd = EnvVarGuard::set("TERMINAL_CWD", tmp.path().to_str().unwrap());

        let config = AgentConfig {
            platform: Some("cli".to_string()),
            coding_context: "auto".to_string(),
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let tools = vec![
            ToolSchema::new("terminal", "Terminal", JsonSchema::new("object")),
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("write_file", "Write", JsonSchema::new("object")),
            ToolSchema::new("patch", "Patch", JsonSchema::new("object")),
            ToolSchema::new("search_files", "Search", JsonSchema::new("object")),
        ];
        let prompt = agent.build_system_prompt("", &tools, "openai/gpt-5.4");
        assert!(
            prompt.contains("You are a coding agent pairing with the user inside their codebase")
        );
        assert!(prompt.contains("Workspace"));
        assert!(prompt.contains("Cargo.toml"));
        assert!(prompt.contains("cargo test"));
        assert!(prompt.contains("mode='patch'"));

        let without_tools = agent.build_system_prompt("", &[], "openai/gpt-5.4");
        assert!(!without_tools.contains("You are a coding agent pairing"));
    }

    #[test]
    fn coding_context_prompt_respects_platform_and_off_mode() {
        use hermes_core::JsonSchema;

        let _lock = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .expect("write manifest");
        let _cwd = EnvVarGuard::set("TERMINAL_CWD", tmp.path().to_str().unwrap());

        let tools = vec![ToolSchema::new(
            "terminal",
            "Terminal",
            JsonSchema::new("object"),
        )];
        for config in [
            AgentConfig {
                platform: Some("telegram".to_string()),
                coding_context: "auto".to_string(),
                skip_memory: true,
                skip_context_files: true,
                ..AgentConfig::default()
            },
            AgentConfig {
                platform: Some("cli".to_string()),
                coding_context: "off".to_string(),
                skip_memory: true,
                skip_context_files: true,
                ..AgentConfig::default()
            },
        ] {
            let agent = AgentLoop::new(
                config,
                Arc::new(ToolRegistry::new()),
                Arc::new(DummyProvider),
            );
            let prompt = agent.build_system_prompt("", &tools, "openai/gpt-5.4");
            assert!(!prompt.contains("You are a coding agent pairing"));
        }
    }

    #[tokio::test]
    async fn code_index_scan_waits_for_run_not_the_constructor() {
        #[derive(Default)]
        struct CapturingProvider {
            seen_messages: Mutex<Vec<Message>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for CapturingProvider {
            async fn chat_completion(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                *self.seen_messages.lock().expect("seen messages lock") = messages.to_vec();
                Ok(LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "test".to_string(),
                    finish_reason: Some("stop".to_string()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let workspace = tempfile::tempdir().expect("tempdir");
        let marker = workspace.path().join("marker.rs");
        std::fs::write(&marker, "pub fn repo_map_marker_fn() {}\n").expect("write marker");
        let provider = Arc::new(CapturingProvider::default());
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = {
            let _lock = env_test_lock();
            let _cwd = EnvVarGuard::set("TERMINAL_CWD", workspace.path().to_str().unwrap());
            AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone())
        };

        // Constructors run on async workers, where a workspace scan would stall
        // every task sharing the worker; the scan belongs to `run`.
        let index = agent.code_index.clone().expect("code index enabled by default");
        assert!(index
            .lock()
            .expect("code index lock")
            .list_file_symbols(&marker, 8)
            .is_empty());

        agent
            .run(vec![Message::user("hello")], Some(Vec::new()))
            .await
            .expect("agent run");

        let seen = provider.seen_messages.lock().expect("seen messages lock");
        assert!(seen.iter().any(|m| m.role == MessageRole::System
            && m.content
                .as_deref()
                .is_some_and(|c| c.contains("repo_map_marker_fn"))));
    }

    /// Records the messages each model call is given and answers "read it".
    #[derive(Default)]
    struct RecordingProvider {
        seen: Mutex<Vec<Vec<Message>>>,
        housekeeping_first: AtomicBool,
    }
    #[async_trait::async_trait]
    impl LlmProvider for RecordingProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            self.seen.lock().expect("seen lock").push(messages.to_vec());
            Ok(hermes_core::LlmResponse {
                message: Message::assistant("read it"),
                usage: None,
                model: "dummy".into(),
                finish_reason: Some("stop".into()),
            })
        }

        fn chat_completion_stream(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            self.seen.lock().expect("seen lock").push(messages.to_vec());
            if self.housekeeping_first.swap(false, Ordering::AcqRel) {
                return futures::stream::iter([
                    Ok(stream_chunk_content("read it")),
                    Ok(stream_chunk_tool_name(0, "cleanup", "todo")),
                    Ok(stream_chunk_tool_args(0, "{}")),
                    Ok(stream_chunk_finish("tool_calls")),
                ])
                .boxed();
            }
            futures::stream::iter(vec![
                Ok(stream_chunk_content("read it")),
                Ok(stream_chunk_finish("stop")),
            ])
            .boxed()
        }
    }

    #[test]
    fn fallback_chain_walks_configured_providers_and_restores_primary() {
        let primary: Arc<dyn LlmProvider> = Arc::new(RecordingProvider::default());
        let first: Arc<dyn LlmProvider> = Arc::new(RecordingProvider::default());
        let second: Arc<dyn LlmProvider> = Arc::new(RecordingProvider::default());
        let chain = crate::FallbackChain::new(primary.clone())
            .with_fallback(first.clone())
            .with_fallback(second.clone())
            .with_cooldown(Duration::MAX);

        assert!(chain.try_restore_primary());
        assert!(Arc::ptr_eq(&chain.active_provider(), &primary));
        assert!(!chain.is_on_fallback());
        assert!(chain.activate_fallback());
        assert!(chain.is_on_fallback());
        assert!(Arc::ptr_eq(&chain.active_provider(), &first));
        assert!(!chain.try_restore_primary());
        assert!(chain.activate_fallback());
        assert!(Arc::ptr_eq(&chain.active_provider(), &second));
        assert!(!chain.activate_fallback());
        assert!(Arc::ptr_eq(&chain.active_provider(), &second));

        let chain = chain.with_cooldown(Duration::ZERO);
        assert!(chain.try_restore_primary());
        assert!(!chain.is_on_fallback());
        assert!(Arc::ptr_eq(&chain.active_provider(), &primary));
        assert!(chain.activate_fallback());
        assert!(Arc::ptr_eq(&chain.active_provider(), &first));
    }

    #[tokio::test]
    async fn plain_and_streamed_runs_wait_for_their_session_end_hook() {
        struct FlushRecorder(Arc<Mutex<Vec<Value>>>);

        impl crate::memory_manager::MemoryProviderPlugin for FlushRecorder {
            fn name(&self) -> &str {
                "test-flush"
            }

            fn prepare_session_end(
                &self,
                messages: &[Value],
            ) -> Option<futures::future::BoxFuture<'static, ()>> {
                let recorded = self.0.clone();
                let messages = messages.to_vec();
                Some(Box::pin(async move {
                    tokio::task::yield_now().await;
                    recorded.lock().unwrap().extend(messages);
                }))
            }
        }

        for (streamed, input) in [(false, "plain session"), (true, "streamed session")] {
            let observed = Arc::new(Mutex::new(Vec::new()));
            let mut memory = MemoryManager::new();
            memory.add_provider(Arc::new(FlushRecorder(observed.clone())));
            let mut config = AgentConfig {
                skip_context_files: true,
                ..AgentConfig::default()
            };
            let _home = isolate_route_learning_home(&mut config);
            let mut registry = ToolRegistry::new();
            registry.register(
                "todo",
                ToolSchema::new("todo", "Update the list", JsonSchema::new("object")),
                Arc::new(|_| Ok("updated".into())),
            );
            let provider = RecordingProvider {
                housekeeping_first: AtomicBool::new(streamed),
                ..RecordingProvider::default()
            };
            let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(provider))
                .with_memory(Arc::new(Mutex::new(memory)));
            let messages = vec![Message::user(input)];
            if streamed {
                let callback_transcript = observed.clone();
                let unmuted = Arc::new(AtomicBool::new(false));
                let callback_unmuted = unmuted.clone();
                agent
                    .run_stream(messages, None, Some(Box::new(move |chunk| {
                        let extra = chunk.delta.and_then(|delta| delta.extra);
                        if extra.is_some_and(|value| {
                            value["control"] == "mute_post_response" && value["enabled"] == false
                        }) {
                            assert!(!callback_transcript.lock().unwrap().is_empty());
                            callback_unmuted.store(true, Ordering::Release);
                        }
                    })))
                    .await
                    .unwrap();
                assert!(unmuted.load(Ordering::Acquire));
            } else {
                agent.run(messages, None).await.unwrap();
            }
            let observed = observed.lock().unwrap();
            assert!(observed.iter().any(|message| message["content"] == input));
        }
    }

    #[test]
    fn streamed_and_plain_runs_both_expand_file_references() {
        let sent = user_messages_sent(vec![Message::user("what is this package? @file:Cargo.toml")]);
        for (users, run) in sent.iter().zip(["run", "run_stream"]) {
            assert!(
                users
                    .last()
                    .is_some_and(|asked| asked.contains("name = \"hermes-agent\"")),
                "{run} sends the file's text to the model"
            );
        }
    }

    /// Runs `messages` through `run` and then `run_stream` and returns the user messages each
    /// sent to the model, in order.
    fn user_messages_sent(messages: Vec<Message>) -> Vec<Vec<String>> {
        // Tests run in the crate's folder, which holds this Cargo.toml.
        let _lock = env_test_lock();
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(RecordingProvider::default());
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(agent.run(messages.clone(), None)).expect("run");
        rt.block_on(agent.run_stream(messages, None, Some(Box::new(|_| {}))))
            .expect("streamed run");
        let seen = provider.seen.lock().expect("seen lock");
        assert_eq!(seen.len(), 2, "one model call per run");
        seen.iter()
            .map(|sent| {
                sent.iter()
                    .filter(|m| m.role == MessageRole::User)
                    .map(|m| m.content.clone().unwrap_or_default())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn only_the_turns_own_message_has_its_references_expanded() {
        // An earlier turn's message as an expansion left it: the attached text names a file.
        let earlier =
            "what is in my notes?\n\n--- Attached Context ---\n\nnotes.md: see @file:Cargo.toml";
        let sent = user_messages_sent(vec![
            Message::user(earlier),
            Message::assistant("They point at the manifest."),
            Message::user("and this? @file:Cargo.toml"),
        ]);
        for (users, run) in sent.iter().zip(["run", "run_stream"]) {
            assert_eq!(users[0], earlier, "{run} expanded an earlier turn's text");
            assert!(
                users[1].contains("name = \"hermes-agent\""),
                "{run} did not expand the turn's own message"
            );
        }
    }

    #[test]
    fn a_reference_in_an_acp_image_prompt_keeps_its_parts() {
        let image = json!({
            "type": "image_url",
            "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="},
        });
        let parts = json!([
            {"type": "text", "text": "what is this package? @file:Cargo.toml"},
            image.clone(),
        ]);
        let prompt = format!("__hermes_acp_parts_json__:{parts}");
        let sent = user_messages_sent(vec![Message::user(prompt)]);
        for (users, run) in sent.iter().zip(["run", "run_stream"]) {
            let parts = crate::provider::parse_acp_multimodal_parts(&users[0])
                .unwrap_or_else(|| panic!("{run} broke the parts: {}", users[0]));
            assert_eq!(parts.len(), 2, "{run}");
            assert!(
                parts[0]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("name = \"hermes-agent\"")),
                "{run} did not expand the text part: {}",
                parts[0]
            );
            assert_eq!(parts[1], image, "{run}");
        }
    }

    include!("tests/coding_route_learning.rs");
    include!("tests/session_end_flush.rs");

#[cfg(test)]
#[path = "tests_core_coverage_gen.rs"]
mod coverage_gen;
