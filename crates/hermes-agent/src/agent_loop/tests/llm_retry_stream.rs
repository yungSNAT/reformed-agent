    #[tokio::test]
    async fn call_llm_with_retry_strips_provider_prefix_for_primary_and_fallback_models() {
        use futures::stream::BoxStream;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct RecordingProvider {
            seen_models: Arc<std::sync::Mutex<Vec<String>>>,
            call_count: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RecordingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                self.seen_models
                    .lock()
                    .expect("seen model lock")
                    .push(model.unwrap_or_default().to_string());
                let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
                if idx == 0 {
                    return Err(AgentError::LlmApi(
                        "API error 429: synthetic retry".to_string(),
                    ));
                }
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model: "ok".to_string(),
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

        let seen_models = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut cfg = AgentConfig {
            model: "nous:primary-model".to_string(),
            ..AgentConfig::default()
        };
        cfg.retry.max_retries = 0;
        cfg.retry.fallback_model = Some("openrouter:backup-model".to_string());

        let provider = Arc::new(RecordingProvider {
            seen_models: seen_models.clone(),
            call_count: AtomicUsize::new(0),
        });
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider);
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user("hello"));

        let resp = agent
            .call_llm_with_retry_inner(&ctx, &[], None, None)
            .await
            .expect("fallback should recover");
        assert_eq!(resp.message.content.as_deref(), Some("ok"));

        let seen = seen_models.lock().expect("seen model lock").clone();
        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "backup-model".to_string()]
        );
    }

    struct RateLimitFallbackProvider {
        seen_models: Arc<std::sync::Mutex<Vec<String>>>,
        primary_calls_before_success: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RateLimitFallbackProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let model = model.unwrap_or_default().to_string();
            self.seen_models
                .lock()
                .expect("seen model lock")
                .push(model.clone());
            if model == "backup-model" {
                return Ok(hermes_core::LlmResponse {
                    message: Message::assistant("fallback-ok"),
                    usage: None,
                    model: "backup-model".to_string(),
                    finish_reason: Some("stop".to_string()),
                });
            }
            let remaining = self
                .primary_calls_before_success
                .try_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |value| value.checked_sub(1),
                )
                .unwrap_or(0);
            if remaining > 0 {
                return Err(AgentError::LlmApi(
                    "API error 429: synthetic rate limit".to_string(),
                ));
            }
            Ok(hermes_core::LlmResponse {
                message: Message::assistant("primary-ok"),
                usage: None,
                model: "primary-model".to_string(),
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

    async fn run_rate_limit_fallback_probe(
        provider: &str,
        base_url: Option<&str>,
        pool: Arc<CredentialPool>,
        primary_failures_before_success: usize,
    ) -> Vec<String> {
        let seen_models = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut cfg = AgentConfig {
            model: "primary-model".to_string(),
            provider: Some(provider.to_string()),
            ..AgentConfig::default()
        };
        cfg.retry.max_retries = 3;
        cfg.retry.base_delay_ms = 0;
        cfg.retry.max_delay_ms = 0;
        cfg.retry.fallback_model = Some("openrouter:backup-model".to_string());
        if let Some(base_url) = base_url {
            cfg.runtime_providers.insert(
                provider.to_string(),
                RuntimeProviderConfig {
                    base_url: Some(base_url.to_string()),
                    request_timeout_seconds: None,
                    api_mode: None,
                    ..RuntimeProviderConfig::default()
                },
            );
        }

        let provider = Arc::new(RateLimitFallbackProvider {
            seen_models: seen_models.clone(),
            primary_calls_before_success: std::sync::atomic::AtomicUsize::new(
                primary_failures_before_success,
            ),
        });
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider)
            .with_primary_credential_pool(pool);
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user("hello"));

        let _ = agent
            .call_llm_with_retry_inner(&ctx, &[], None, None)
            .await
            .expect("probe should recover");
        let seen = seen_models.lock().expect("seen model lock").clone();
        seen
    }

    #[tokio::test]
    async fn cloudcode_rate_limit_uses_fallback_without_pool_retry() {
        let seen = run_rate_limit_fallback_probe(
            "google-gemini-cli",
            None,
            Arc::new(CredentialPool::new(vec![
                "oauth-a".to_string(),
                "oauth-b".to_string(),
                "oauth-c".to_string(),
            ])),
            10,
        )
        .await;

        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "backup-model".to_string()]
        );
    }

    #[tokio::test]
    async fn cloudcode_base_url_uses_fallback_even_for_alias_provider() {
        let seen = run_rate_limit_fallback_probe(
            "custom-provider",
            Some("cloudcode-pa://google"),
            Arc::new(CredentialPool::new(vec![
                "oauth-a".to_string(),
                "oauth-b".to_string(),
                "oauth-c".to_string(),
            ])),
            10,
        )
        .await;

        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "backup-model".to_string()]
        );
    }

    #[tokio::test]
    async fn non_cloudcode_multi_key_pool_retries_primary_before_fallback() {
        let seen = run_rate_limit_fallback_probe(
            "openrouter",
            Some("https://openrouter.ai/api/v1"),
            Arc::new(CredentialPool::new(vec![
                "key-a".to_string(),
                "key-b".to_string(),
                "key-c".to_string(),
            ])),
            1,
        )
        .await;

        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "primary-model".to_string()]
        );
    }

    #[tokio::test]
    async fn single_entry_pool_rate_limit_uses_fallback_without_retry() {
        let seen = run_rate_limit_fallback_probe(
            "openrouter",
            Some("https://openrouter.ai/api/v1"),
            Arc::new(CredentialPool::single("key-a")),
            10,
        )
        .await;

        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "backup-model".to_string()]
        );
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    struct ChaosHarnessStep {
        kind: String,
        message: Option<String>,
    }

    #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
    struct ChaosHarnessExpectation {
        outcome: String,
        attempts: usize,
        fallback_calls: usize,
        error_contains: Option<String>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    struct ChaosHarnessScenario {
        id: String,
        seed: u64,
        max_retries: u32,
        fallback_model: Option<String>,
        #[serde(default)]
        include_tool_schema: bool,
        steps: Vec<ChaosHarnessStep>,
        expected: ChaosHarnessExpectation,
    }

    #[derive(Debug, serde::Deserialize)]
    struct ChaosHarnessFixture {
        schema_version: u32,
        scenarios: Vec<ChaosHarnessScenario>,
    }

    #[derive(Debug)]
    struct ChaosHarnessRun {
        outcome: &'static str,
        attempts: usize,
        fallback_calls: usize,
        error: Option<String>,
    }

    fn load_chaos_harness_fixture() -> ChaosHarnessFixture {
        serde_json::from_str(include_str!("../../testdata/adapter_chaos_profiles.json"))
            .expect("parse adapter chaos fixture")
    }

    struct ChaosHarnessProvider {
        scenario_id: String,
        steps: Vec<ChaosHarnessStep>,
        call_index: std::sync::atomic::AtomicUsize,
        seen_models: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl ChaosHarnessProvider {
        fn new(scenario: &ChaosHarnessScenario) -> Self {
            Self {
                scenario_id: scenario.id.clone(),
                steps: scenario.steps.clone(),
                call_index: std::sync::atomic::AtomicUsize::new(0),
                seen_models: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn attempts(&self) -> usize {
            self.call_index.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn fallback_calls(&self, fallback_model: Option<&str>) -> usize {
            let Some(fallback) = fallback_model else {
                return 0;
            };
            let fallback_name = fallback
                .split_once(':')
                .map(|(_, model)| model)
                .unwrap_or(fallback);
            self.seen_models
                .lock()
                .expect("seen model lock")
                .iter()
                .filter(|m| m.as_str() == fallback_name)
                .count()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for ChaosHarnessProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let idx = self
                .call_index
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.seen_models
                .lock()
                .expect("seen model lock")
                .push(model.unwrap_or_default().to_string());

            let step = self
                .steps
                .get(idx)
                .cloned()
                .or_else(|| self.steps.last().cloned())
                .unwrap_or(ChaosHarnessStep {
                    kind: "success".to_string(),
                    message: Some("ok-default".to_string()),
                });
            match step.kind.as_str() {
                "success" => Ok(hermes_core::LlmResponse {
                    message: Message::assistant(
                        step.message
                            .unwrap_or_else(|| format!("ok-{}", self.scenario_id)),
                    ),
                    usage: None,
                    model: "chaos".to_string(),
                    finish_reason: Some("stop".to_string()),
                }),
                "timeout" => Err(AgentError::LlmApi(
                    step.message
                        .unwrap_or_else(|| "request timeout".to_string()),
                )),
                "http_5xx" => {
                    Err(AgentError::LlmApi(step.message.unwrap_or_else(|| {
                        "API error 500: synthetic upstream fault".to_string()
                    })))
                }
                "rate_limit" => {
                    Err(AgentError::LlmApi(step.message.unwrap_or_else(|| {
                        "API error 429: synthetic rate limit".to_string()
                    })))
                }
                "connection_reset" => Err(AgentError::LlmApi(
                    step.message
                        .unwrap_or_else(|| "connection reset by peer".to_string()),
                )),
                "auth_expired" => Err(AgentError::AuthFailed(
                    step.message
                        .unwrap_or_else(|| "HTTP 401 Unauthorized: token expired".to_string()),
                )),
                "malformed_tool_payload" => Err(AgentError::LlmApi(
                    step.message.unwrap_or_else(|| {
                        "Provider returned error: This request is not valid because tools payload is invalid"
                            .to_string()
                    }),
                )),
                other => Err(AgentError::LlmApi(format!(
                    "unsupported chaos step '{}' in scenario '{}'",
                    other, self.scenario_id
                ))),
            }
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

    async fn run_chaos_harness_scenario(scenario: &ChaosHarnessScenario) -> ChaosHarnessRun {
        let mut cfg = AgentConfig {
            model: "nous:primary-model".to_string(),
            ..AgentConfig::default()
        };
        cfg.retry.max_retries = scenario.max_retries;
        cfg.retry.base_delay_ms = 0;
        cfg.retry.max_delay_ms = 0;
        cfg.retry.fallback_model = scenario.fallback_model.clone();

        let provider = Arc::new(ChaosHarnessProvider::new(scenario));
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user(format!(
            "chaos scenario {} seed {}",
            scenario.id, scenario.seed
        )));
        let tool_schemas = if scenario.include_tool_schema {
            vec![ToolSchema::new(
                "sota_fault_probe",
                "SOTA fault-injection probe tool",
                hermes_core::JsonSchema::new("object"),
            )]
        } else {
            Vec::new()
        };

        match agent
            .call_llm_with_retry_inner(&ctx, &tool_schemas, None, None)
            .await
        {
            Ok(_) => ChaosHarnessRun {
                outcome: "success",
                attempts: provider.attempts(),
                fallback_calls: provider.fallback_calls(scenario.fallback_model.as_deref()),
                error: None,
            },
            Err(err) => ChaosHarnessRun {
                outcome: "error",
                attempts: provider.attempts(),
                fallback_calls: provider.fallback_calls(scenario.fallback_model.as_deref()),
                error: Some(err.to_string()),
            },
        }
    }

    #[tokio::test]
    async fn rejected_tools_try_a_compatible_model_then_retry_the_original_without_tools() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        if run_isolated_agent_test(
            "agent_loop::tests::rejected_tools_try_a_compatible_model_then_retry_the_original_without_tools",
        ) {
            return;
        }

        const PRIMARY: &str = "openai/fixture-primary";
        const ROUTED: &str = "openai/fixture-routed";
        const FALLBACK: &str = "fixture/compatible-model";
        const TOOL: &str = "lookup_weekly_summary";
        let _fallback = EnvVarGuard::set("HERMES_TOOL_PAYLOAD_FALLBACK_MODEL", FALLBACK);

        let home = tempfile::tempdir().unwrap();
        let default_server = MockServer::start().await;
        let routed_server = MockServer::start().await;
        let default_url = format!("{}/v1", default_server.uri());
        let routed_url = format!("{}/v1", routed_server.uri());
        let primary = Arc::new(
            NousProvider::new("fixture-only")
                .with_base_url(&default_url)
                .with_optional_request_timeout_seconds(Some(5.0)),
        );
        let agent = AgentLoop::new(
            AgentConfig {
                model: format!("nous:{PRIMARY}"),
                provider: Some("nous".into()),
                hermes_home: Some(home.path().to_str().unwrap().into()),
                isolated: true,
                code_index_enabled: false,
                runtime_providers: HashMap::from([(
                    "nous".into(),
                    RuntimeProviderConfig {
                        api_key: Some("fixture-only".into()),
                        base_url: Some(default_url),
                        request_timeout_seconds: Some(5.0),
                        ..RuntimeProviderConfig::default()
                    },
                )]),
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            primary,
        );
        let mut route = agent.turn_route_reliability_guard(format!("nous:{ROUTED}"));
        route.provider = Some("nous".into());
        route.base_url = Some(routed_url);
        let mut ctx = ContextManager::new(1_000);
        ctx.add_message(Message::user("Find the weekly summary."));
        let schemas = [ToolSchema::new(
            TOOL,
            "Look up the weekly summary",
            hermes_core::JsonSchema::new("object"),
        )];

        for (use_route, accept_fallback, accept_without_tools) in [
            (false, true, false),
            (true, false, true),
            (false, false, false),
        ] {
            let (server, unused) = if use_route {
                (&routed_server, &default_server)
            } else {
                (&default_server, &routed_server)
            };
            let requested = if use_route { ROUTED } else { PRIMARY };
            let count = if accept_fallback { 2 } else { 3 };
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .respond_with(move |request: &wiremock::Request| {
                    let body: Value = request.body_json().unwrap();
                    let model = body["model"].as_str().unwrap();
                    let has_tools = body["tools"].as_array().is_some_and(|tools| !tools.is_empty());
                    let (status, text) = match (model, has_tools) {
                        (model, true) if model == requested => (400, "Invalid tools payload"),
                        (FALLBACK, true) if accept_fallback => (200, "Compatible model handled the tools"),
                        (FALLBACK, true) => (400, "Compatible model refused the tools"),
                        (model, false) if model == requested && accept_without_tools => {
                            (200, "Original model replied without tools")
                        }
                        (model, false) if model == requested => {
                            (503, "Original model unavailable without tools")
                        }
                        _ => (422, "Unexpected model or tools"),
                    };
                    if status == 200 {
                        ResponseTemplate::new(status).set_body_json(json!({
                            "id": "fixture-completion",
                            "model": model,
                            "choices": [{
                                "message": {"role": "assistant", "content": text},
                                "finish_reason": "stop"
                            }]
                        }))
                    } else {
                        ResponseTemplate::new(status).set_body_json(json!({"error": {"message": text}}))
                    }
                })
                .expect(count as u64)
                .mount(server)
                .await;

            let result = agent
                .call_llm_with_retry_inner(&ctx, &schemas, use_route.then_some(&route), None)
                .await;
            if accept_fallback || accept_without_tools {
                let expected = if accept_fallback {
                    "Compatible model handled the tools"
                } else {
                    "Original model replied without tools"
                };
                assert_eq!(result.unwrap().message.content.as_deref(), Some(expected));
            } else {
                let error = result.unwrap_err();
                assert!(matches!(&error, AgentError::LlmApi(message) if message.contains("503") && message.contains("Original model unavailable without tools")), "{error}");
            }

            let requests = server.received_requests().await.unwrap();
            assert_eq!(requests.len(), count);
            assert!(unused.received_requests().await.unwrap().is_empty());
            let bodies: Vec<Value> = requests
                .iter()
                .map(|request| request.body_json().unwrap())
                .collect();
            assert_eq!(bodies[0]["model"], requested);
            assert_eq!(bodies[1]["model"], FALLBACK);
            for body in &bodies[..2] {
                assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
                assert_eq!(body["tools"][0]["function"]["name"], TOOL);
            }
            if let Some(last) = bodies.get(2) {
                assert_eq!(last["model"], requested);
                assert!(last["tools"].is_null() || last["tools"].as_array().is_some_and(Vec::is_empty));
            }
            server.reset().await;
        }
    }

    #[test]
    fn chaos_harness_fixture_is_seeded_and_unique() {
        let fixture = load_chaos_harness_fixture();
        assert_eq!(fixture.schema_version, 1, "unexpected fixture schema");
        assert!(
            !fixture.scenarios.is_empty(),
            "chaos fixture must not be empty"
        );
        let mut ids = std::collections::HashSet::new();
        let mut seeds = std::collections::HashSet::new();
        for scenario in fixture.scenarios {
            assert!(
                ids.insert(scenario.id.clone()),
                "duplicate chaos scenario id: {}",
                scenario.id
            );
            assert!(
                seeds.insert(scenario.seed),
                "duplicate chaos scenario seed: {}",
                scenario.seed
            );
        }
    }

    #[tokio::test]
    async fn chaos_harness_profiles_verify_retry_and_fallback() {
        let fixture = load_chaos_harness_fixture();
        let mut diagnostics = Vec::new();
        let mut runs = Vec::new();
        for scenario in fixture.scenarios {
            let run = run_chaos_harness_scenario(&scenario).await;
            runs.push(serde_json::json!({
                "scenario": scenario.id,
                "seed": scenario.seed,
                "actual": {
                    "outcome": run.outcome,
                    "attempts": run.attempts,
                    "fallback_calls": run.fallback_calls,
                    "error": run.error,
                }
            }));
            let mut mismatches = Vec::new();

            if run.outcome != scenario.expected.outcome {
                mismatches.push(format!(
                    "outcome mismatch expected={} actual={}",
                    scenario.expected.outcome, run.outcome
                ));
            }
            if run.attempts != scenario.expected.attempts {
                mismatches.push(format!(
                    "attempt mismatch expected={} actual={}",
                    scenario.expected.attempts, run.attempts
                ));
            }
            if run.fallback_calls != scenario.expected.fallback_calls {
                mismatches.push(format!(
                    "fallback_calls mismatch expected={} actual={}",
                    scenario.expected.fallback_calls, run.fallback_calls
                ));
            }
            if let Some(expect_fragment) = scenario.expected.error_contains.as_ref() {
                let got_error = run.error.as_deref().unwrap_or("");
                if !got_error.contains(expect_fragment) {
                    mismatches.push(format!(
                        "error fragment missing expected='{}' actual='{}'",
                        expect_fragment, got_error
                    ));
                }
            }

            if !mismatches.is_empty() {
                diagnostics.push(serde_json::json!({
                    "scenario": scenario.id,
                    "seed": scenario.seed,
                    "expected": scenario.expected,
                    "actual": {
                        "outcome": run.outcome,
                        "attempts": run.attempts,
                        "fallback_calls": run.fallback_calls,
                        "error": run.error,
                    },
                    "mismatches": mismatches,
                }));
            }
        }

        println!(
            "adapter chaos harness results: {}",
            serde_json::to_string(&runs).expect("serialize chaos runs")
        );

        assert!(
            diagnostics.is_empty(),
            "adapter chaos harness mismatches:\n{}",
            serde_json::to_string_pretty(&diagnostics).expect("serialize diagnostics")
        );
    }

    #[tokio::test]
    async fn handle_max_iterations_uses_provider_native_model_id() {
        use futures::stream::BoxStream;

        struct RecordingProvider {
            seen_model: Arc<std::sync::Mutex<Option<String>>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RecordingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                *self.seen_model.lock().expect("seen model lock") =
                    Some(model.unwrap_or_default().to_string());
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("summary"),
                    usage: None,
                    model: "ok".to_string(),
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

        let seen_model = Arc::new(std::sync::Mutex::new(None::<String>));
        let cfg = AgentConfig {
            model: "nous:moonshotai/kimi-k2.6".to_string(),
            ..AgentConfig::default()
        };

        let provider = Arc::new(RecordingProvider {
            seen_model: seen_model.clone(),
        });
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider);
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user("hit turn limit"));

        let _ = agent
            .handle_max_iterations(&mut ctx)
            .await
            .expect("max iterations summary should succeed");
        let seen = seen_model.lock().expect("seen model lock").clone();
        assert_eq!(seen.as_deref(), Some("moonshotai/kimi-k2.6"));
    }

    #[tokio::test]
    async fn status_callback_receives_empty_response_retry_notice() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct RetryDummyProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RetryDummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                let msg = if *n == 1 {
                    Message::assistant("")
                } else {
                    Message::assistant("ok")
                };
                let finish_reason = if *n == 1 {
                    None
                } else {
                    Some("stop".to_string())
                };
                Ok(hermes_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason,
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
            max_turns: 1,
            empty_content_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(RetryDummyProvider::default()),
        )
        .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response — retrying")
        }));
    }

    #[tokio::test]
    async fn empty_stop_response_is_accepted_without_retry() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct EmptyStopProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for EmptyStopProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant(""),
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

        let provider = Arc::new(EmptyStopProvider::default());
        let cfg = AgentConfig {
            max_turns: 1,
            empty_content_max_retries: 3,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone())
            .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
        let rows = captured.lock().expect("captured lock");
        assert!(!rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response — retrying")
        }));
    }

    fn stream_chunk_content(text: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: Some(text.to_string()),
                tool_calls: None,
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_tool_name(index: u32, id: &str, name: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: None,
                tool_calls: Some(vec![hermes_core::ToolCallDelta {
                    index,
                    id: Some(id.to_string()),
                    function: Some(hermes_core::FunctionCallDelta {
                        name: Some(name.to_string()),
                        arguments: None,
                    }),
                }]),
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_tool_args(index: u32, args: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: None,
                tool_calls: Some(vec![hermes_core::ToolCallDelta {
                    index,
                    id: None,
                    function: Some(hermes_core::FunctionCallDelta {
                        name: None,
                        arguments: Some(args.to_string()),
                    }),
                }]),
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_finish(reason: &str) -> StreamChunk {
        StreamChunk {
            delta: None,
            finish_reason: Some(reason.to_string()),
            usage: None,
        }
    }

    const STREAM_UNREPRESENTABLE_ARGUMENT: &str = r#"{"value":1e999}"#;

    #[derive(Clone, Copy)]
    enum StreamRetryScenario {
        CostLimit,
        UnknownTool,
        SemanticRetry,
        FinalizerRewrite,
        EmptyStop,
        PartialInterrupt,
        RecoverOnSecondAttempt,
        AlwaysFailMidToolCall,
        TextOnlyDrop,
        MemoryContextLeak,
        BufferedContextTail { text: &'static str, finish: bool },
        MalformedToolArgs,
        TruncatedToolArguments,
        InvalidArgumentBatch,
        SlowStream,
        SlowRetry,
    }

    struct StreamRetryProvider {
        scenario: StreamRetryScenario,
        calls: std::sync::Mutex<u32>,
        semantic_retries: std::sync::Mutex<u32>,
        pending_stream_polled: Arc<tokio::sync::Notify>,
    }

    impl StreamRetryProvider {
        fn new(scenario: StreamRetryScenario) -> Self {
            Self {
                scenario,
                calls: std::sync::Mutex::new(0),
                semantic_retries: std::sync::Mutex::new(0),
                pending_stream_polled: Arc::new(tokio::sync::Notify::new()),
            }
        }

        fn cost_limit_usage() -> UsageStats {
            UsageStats {
                prompt_tokens: 3,
                completion_tokens: 2,
                total_tokens: 5,
                estimated_cost: Some(0.5),
                ..Default::default()
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for StreamRetryProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            if matches!(self.scenario, StreamRetryScenario::CostLimit) {
                *self.calls.lock().expect("calls lock") += 1;
                return Ok(hermes_core::LlmResponse {
                    message: Message::assistant("Fixture draft"),
                    usage: Some(Self::cost_limit_usage()),
                    model: "fixture-model".into(),
                    finish_reason: Some("stop".into()),
                });
            }
            if !matches!(self.scenario, StreamRetryScenario::SemanticRetry) {
                return Err(AgentError::LlmApi("unused".to_string()));
            }
            assert!(messages.iter().any(|message| {
                message.role == MessageRole::Assistant
                    && message.reasoning_content.as_deref() == Some("Fixture draft reasoning")
            }));
            let mut retries = self.semantic_retries.lock().unwrap();
            *retries += 1;
            let (text, finish_reason, prompt_tokens, completion_tokens) = if *retries == 1 {
                ("", "length", 4, 2)
            } else {
                ("Recovered answer", "stop", 16, 3)
            };
            Ok(hermes_core::LlmResponse {
                message: Message::assistant(text),
                usage: Some(hermes_core::UsageStats {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens: prompt_tokens + completion_tokens,
                    ..Default::default()
                }),
                model: "fixture-model".into(),
                finish_reason: Some(finish_reason.into()),
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
            let mut calls = self.calls.lock().expect("calls lock");
            *calls += 1;
            let attempt = *calls;

            let events: Vec<Result<StreamChunk, AgentError>> = match self.scenario {
                StreamRetryScenario::CostLimit => vec![
                    Ok(stream_chunk_content("Fixture draft")),
                    Ok(StreamChunk {
                        usage: Some(Self::cost_limit_usage()),
                        ..stream_chunk_finish("stop")
                    }),
                ],
                StreamRetryScenario::UnknownTool => vec![
                    Ok(stream_chunk_tool_name(
                        0,
                        &format!("missing-fixture-{attempt}"),
                        "missing_fixture_tool",
                    )),
                    Ok(stream_chunk_tool_args(0, "{}")),
                    Ok(stream_chunk_finish("tool_calls")),
                ],
                StreamRetryScenario::SemanticRetry => vec![
                    Ok(StreamChunk {
                        delta: Some(hermes_core::StreamDelta {
                            content: None,
                            tool_calls: None,
                            extra: Some(serde_json::json!({"thinking":"Fixture draft reasoning"})),
                        }),
                        usage: Some(hermes_core::UsageStats {
                            prompt_tokens: 1,
                            completion_tokens: 1,
                            total_tokens: 2,
                            ..Default::default()
                        }),
                        finish_reason: None,
                    }),
                    Ok(stream_chunk_finish("length")),
                ],
                StreamRetryScenario::FinalizerRewrite => {
                    let text = if attempt == 1 {
                        "Read the [fixture details](URL)."
                    } else {
                        assert_eq!(attempt, 2, "the corrected answer ends the turn");
                        assert!(messages.iter().any(|message| {
                            message.role == MessageRole::System
                                && message.content.as_deref().is_some_and(|content| {
                                    content.contains("Output quality contract")
                                        && content.contains("no unresolved placeholders")
                                })
                        }));
                        "The fixture contains two records."
                    };
                    vec![
                        Ok(stream_chunk_content(text)),
                        Ok(stream_chunk_finish("stop")),
                    ]
                }
                StreamRetryScenario::EmptyStop => vec![Ok(stream_chunk_finish("stop"))],
                StreamRetryScenario::PartialInterrupt => vec![Ok(StreamChunk {
                    usage: Some(hermes_core::UsageStats {
                        prompt_tokens: 2,
                        completion_tokens: 1,
                        total_tokens: 3,
                        estimated_cost: Some(0.125),
                        ..Default::default()
                    }),
                    ..stream_chunk_content("Partial answer")
                })],
                StreamRetryScenario::SlowStream => vec![
                    Ok(stream_chunk_content("first ")),
                    Ok(stream_chunk_content("second ")),
                    Ok(stream_chunk_content("third")),
                    Ok(stream_chunk_finish("stop")),
                ],
                StreamRetryScenario::SlowRetry if attempt == 1 => vec![Err(AgentError::LlmApi(
                    "Stream read error: connection lost".into(),
                ))],
                StreamRetryScenario::SlowRetry => vec![
                    Ok(stream_chunk_content("retried response")),
                    Ok(stream_chunk_finish("stop")),
                ],
                StreamRetryScenario::RecoverOnSecondAttempt => {
                    if attempt == 1 {
                        vec![
                            Ok(stream_chunk_content("Let me write the audit: ")),
                            Ok(stream_chunk_tool_name(0, "call_1", "write_file")),
                            Ok(stream_chunk_tool_args(0, "{\"path\":\"/tmp/x\",")),
                            Err(AgentError::LlmApi(
                                "Stream read error: peer closed connection".to_string(),
                            )),
                        ]
                    } else {
                        vec![
                            Ok(stream_chunk_content("Let me write the audit: ")),
                            Ok(stream_chunk_tool_name(0, "call_1", "write_file")),
                            Ok(stream_chunk_tool_args(
                                0,
                                "{\"path\":\"/tmp/x\",\"content\":\"hi\"}",
                            )),
                            Ok(stream_chunk_finish("tool_calls")),
                        ]
                    }
                }
                StreamRetryScenario::AlwaysFailMidToolCall => vec![
                    Ok(stream_chunk_content("Working...")),
                    Ok(stream_chunk_tool_name(0, "call_2", "write_file")),
                    Ok(stream_chunk_tool_args(0, "{\"path\":\"/tmp/y\",")),
                    Err(AgentError::LlmApi(
                        "Stream read error: connection reset by peer".to_string(),
                    )),
                ],
                StreamRetryScenario::TextOnlyDrop => vec![
                    Ok(stream_chunk_content("Partial text")),
                    Err(AgentError::LlmApi(
                        "Stream read error: connection lost".to_string(),
                    )),
                ],
                StreamRetryScenario::MemoryContextLeak => vec![
                    Ok(stream_chunk_content("Hello\n")),
                    Ok(stream_chunk_content("<memory-context>\nsecret ")),
                    Ok(stream_chunk_content("payload\n")),
                    Ok(stream_chunk_content("</memory-context> world")),
                    Ok(stream_chunk_finish("stop")),
                ],
                StreamRetryScenario::BufferedContextTail { text, finish } => {
                    let mut events = vec![Ok(stream_chunk_content(text))];
                    if finish {
                        events.push(Ok(stream_chunk_finish("stop")));
                    }
                    events
                }
                StreamRetryScenario::MalformedToolArgs => vec![
                    Ok(stream_chunk_tool_name(0, "call_repair", "terminal")),
                    Ok(stream_chunk_tool_args(
                        0,
                        "{\"command\":\"ls -la\",\"timeout\":30",
                    )),
                    Ok(stream_chunk_finish("tool_calls")),
                ],
                StreamRetryScenario::TruncatedToolArguments if attempt == 1 => vec![
                    Ok(stream_chunk_tool_name(
                        0,
                        "truncated-fixture",
                        "fixture_tool",
                    )),
                    Ok(stream_chunk_tool_args(0, STREAM_UNREPRESENTABLE_ARGUMENT)),
                    Ok(stream_chunk_finish("tool_calls")),
                ],
                StreamRetryScenario::TruncatedToolArguments if attempt == 2 => vec![
                    Ok(stream_chunk_tool_name(
                        0,
                        "recovered-fixture",
                        "fixture_tool",
                    )),
                    Ok(stream_chunk_tool_args(0, r#"{"value":"complete"}"#)),
                    Ok(stream_chunk_finish("tool_calls")),
                ],
                StreamRetryScenario::TruncatedToolArguments => vec![
                    Ok(stream_chunk_content("Recorded the complete fixture.")),
                    Ok(stream_chunk_finish("stop")),
                ],
                StreamRetryScenario::InvalidArgumentBatch if attempt <= 2 => vec![
                    Ok(stream_chunk_tool_name(
                        0,
                        &format!("invalid-fixture-{attempt}"),
                        "fixture_invalid",
                    )),
                    Ok(stream_chunk_tool_args(0, STREAM_UNREPRESENTABLE_ARGUMENT)),
                    Ok(stream_chunk_tool_name(
                        1,
                        &format!("skipped-fixture-{attempt}"),
                        "fixture_valid",
                    )),
                    Ok(stream_chunk_tool_args(1, r#"{"value":"skipped"}"#)),
                    Ok(stream_chunk_finish("tool_calls")),
                ],
                StreamRetryScenario::InvalidArgumentBatch if attempt == 3 => vec![
                    Ok(stream_chunk_tool_name(0, "valid-fixture-3", "fixture_valid")),
                    Ok(stream_chunk_tool_args(0, r#"{"value":"recovered"}"#)),
                    Ok(stream_chunk_finish("tool_calls")),
                ],
                StreamRetryScenario::InvalidArgumentBatch => vec![
                    Ok(stream_chunk_content("Recovered after the tool result")),
                    Ok(stream_chunk_finish("stop")),
                ],
            };

            let events = futures::stream::iter(events);
            if matches!(self.scenario, StreamRetryScenario::PartialInterrupt) {
                let pending_stream_polled = Arc::clone(&self.pending_stream_polled);
                return events
                    .chain(futures::stream::poll_fn(move |_| {
                        pending_stream_polled.notify_one();
                        std::task::Poll::Pending
                    }))
                    .boxed();
            }
            if matches!(self.scenario, StreamRetryScenario::SlowStream | StreamRetryScenario::SlowRetry) {
                return events
                    .then(|event| async {
                        tokio::time::sleep(Duration::from_millis(40)).await;
                        event
                    })
                    .boxed();
            }
            events.boxed()
        }
    }

    #[tokio::test]
    async fn stream_semantic_retries_keep_reasoning_and_accumulate_all_usage() {
        let mut config = AgentConfig {
            model: "openai:fixture-model".into(),
            thinking_prefill_max_retries: 1,
            empty_content_max_retries: 1,
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::SemanticRetry));
        let notices = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&notices);
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone())
            .with_callbacks(AgentCallbacks {
                status_callback: Some(Arc::new(move |kind, message| {
                    if kind == "lifecycle" && message.contains("retrying") {
                        captured.lock().unwrap().push(message.to_owned());
                    }
                })),
                ..Default::default()
            });
        let result = agent
            .run_stream(vec![Message::user("Show the answer")], None, Some(Box::new(|_| {})))
            .await
            .unwrap();
        assert!(result.finished_naturally);
        assert_eq!(result.total_turns, 1);
        assert_eq!(*provider.calls.lock().unwrap(), 1);
        assert_eq!(*provider.semantic_retries.lock().unwrap(), 2);
        let usage = result.usage.unwrap();
        assert_eq!((usage.prompt_tokens, usage.completion_tokens, usage.total_tokens), (21, 6, 27));
        assert_eq!(
            result.messages.last().and_then(|message| message.content.as_deref()),
            Some("Recovered answer")
        );
        let notices = notices.lock().unwrap();
        assert_eq!(notices.len(), 2);
        assert!(notices[0].starts_with("Reasoning-only response"));
        assert!(notices[1].starts_with("Empty assistant response"));
    }

    #[tokio::test]
    async fn stream_finalizer_requests_a_rewrite_before_finishing() {
        let mut config = AgentConfig {
            model: "openai:fixture-model".into(),
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::FinalizerRewrite));
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone());
        let result = agent
            .run_stream(
                vec![Message::user("Summarise the fixture")],
                None,
                Some(Box::new(|_| {})),
            )
            .await
            .unwrap();
        assert!(result.finished_naturally);
        assert_eq!(result.total_turns, 2);
        assert_eq!(*provider.calls.lock().unwrap(), 2);
        assert_eq!(
            result
                .messages
                .last()
                .and_then(|message| message.content.as_deref()),
            Some("The fixture contains two records.")
        );
    }

    #[tokio::test]
    async fn stream_semantic_empty_stop_finishes_without_retry() {
        let mut config = AgentConfig {
            model: "openai:fixture-model".into(),
            empty_content_max_retries: 2,
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::EmptyStop));
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone());
        let result = agent
            .run_stream(vec![Message::user("Use an explicit stop")], None, Some(Box::new(|_| {})))
            .await
            .unwrap();
        assert!(result.finished_naturally);
        assert_eq!(result.total_turns, 1);
        assert_eq!(*provider.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn stream_semantic_interrupt_returns_partial_text_and_usage() {
        let mut config = AgentConfig {
            model: "openai:fixture-model".into(),
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::PartialInterrupt));
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone());
        let interrupt = agent.interrupt.clone();
        let pending_stream_polled = Arc::clone(&provider.pending_stream_polled);
        let cancellation = tokio::spawn(async move {
            pending_stream_polled.notified().await;
            interrupt.interrupt(None);
        });
        let delivered = Arc::new(Mutex::new(String::new()));
        let stream_text = Arc::clone(&delivered);
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            agent.run_stream(
                vec![Message::user("Start a response")],
                None,
                Some(Box::new(move |chunk| {
                    if let Some(text) = chunk.delta.and_then(|delta| delta.content) {
                        stream_text.lock().unwrap().push_str(&text);
                    }
                })),
            ),
        )
        .await
        .expect("interrupt ends the stalled fixture stream")
        .unwrap();
        assert!(result.interrupted);
        assert!(!result.finished_naturally);
        cancellation.await.unwrap();
        assert_eq!(*delivered.lock().unwrap(), "Partial answer");
        assert_eq!(*provider.calls.lock().unwrap(), 1);
        assert_eq!(result.usage.unwrap().total_tokens, 3);
        assert_eq!(result.session_cost_usd, Some(0.125));
        assert_eq!(
            result.messages.last().and_then(|message| message.content.as_deref()),
            Some("Partial answer")
        );
    }

    #[tokio::test]
    async fn nonstreaming_usage_stops_the_turn_at_the_session_cost_limit() {
        for (degrade_model, warning) in [
            (
                Some(" anthropic:fixture-small "),
                "Switching to cheaper model `anthropic:fixture-small`.",
            ),
            (None, "Cost guard warning: session spend is now"),
        ] {
            let home = tempfile::tempdir().expect("fixture home");
            let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::CostLimit));
            let agent = AgentLoop::new(
                AgentConfig {
                    model: "anthropic:fixture-model".into(),
                    hermes_home: Some(home.path().to_string_lossy().into_owned()),
                    max_cost_usd: Some(0.25),
                    cost_guard_degrade_model: degrade_model.map(str::to_owned),
                    skip_memory: true,
                    skip_context_files: true,
                    code_index_enabled: false,
                    ..AgentConfig::default()
                },
                Arc::new(ToolRegistry::new()),
                provider.clone(),
            );
            let result = agent
                .run(vec![Message::user("Fixture question")], None)
                .await
                .expect("cost guard returns the session state");

            assert!(!result.finished_naturally);
            assert!(!result.interrupted);
            assert_eq!(result.total_turns, 1);
            assert_eq!(result.session_cost_usd, Some(0.5));
            assert_eq!(result.usage.expect("reported usage").total_tokens, 5);
            assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
            let notices: Vec<_> = result
                .messages
                .iter()
                .filter(|message| message.role == MessageRole::System)
                .filter_map(|message| message.content.as_deref())
                .collect();
            assert!(notices.iter().any(|text| text.contains(warning)));
            assert!(notices
                .iter()
                .any(|text| text.contains("Cost guard tripped")));
            assert!(!result
                .messages
                .iter()
                .any(|message| message.role == MessageRole::Assistant));
        }
    }

    #[tokio::test]
    async fn streamed_usage_stops_the_turn_at_the_session_cost_limit() {
        let home = tempfile::tempdir().expect("fixture home");
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::CostLimit));
        let agent = AgentLoop::new(
            AgentConfig {
                model: "openai:fixture-model".into(),
                hermes_home: Some(home.path().to_string_lossy().into_owned()),
                max_cost_usd: Some(0.25),
                cost_guard_degrade_model: Some("openai:fixture-small".into()),
                skip_memory: true,
                skip_context_files: true,
                code_index_enabled: false,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            provider.clone(),
        );
        let result = agent
            .run_stream(
                vec![Message::user("Fixture question")],
                None,
                Some(Box::new(|_| {})),
            )
            .await
            .expect("cost guard returns the session state");
        assert!(!result.finished_naturally);
        assert!(!result.interrupted);
        assert_eq!(result.total_turns, 1);
        assert_eq!(result.session_cost_usd, Some(0.5));
        assert_eq!(result.usage.expect("stream usage").total_tokens, 5);
        assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
        assert!(result.messages.iter().any(|row| {
            row.role == MessageRole::System
                && row
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("Cost guard tripped"))
        }));
        assert!(!result
            .messages
            .iter()
            .any(|row| row.role == MessageRole::Assistant));
    }

    #[tokio::test]
    async fn streamed_unknown_tool_calls_are_rejected_then_capped() {
        let home = tempfile::tempdir().expect("fixture home");
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::UnknownTool));
        let agent = AgentLoop::new(
            AgentConfig {
                model: "openai:fixture-model".into(),
                hermes_home: Some(home.path().to_string_lossy().into_owned()),
                max_turns: 5,
                invalid_tool_call_max_retries: 2,
                skip_memory: true,
                skip_context_files: true,
                code_index_enabled: false,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            provider.clone(),
        );
        let result = agent
            .run_stream(
                vec![Message::user("Fixture question")],
                None,
                Some(Box::new(|_| {})),
            )
            .await
            .expect("unknown tool is reported to the agent");
        assert!(!result.finished_naturally);
        assert!(!result.interrupted);
        assert_eq!(result.total_turns, 2);
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
        assert!(result.tool_errors.is_empty());
        assert!(result.messages.iter().any(|row| {
            row.role == MessageRole::Tool
                && row
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("Tool 'missing_fixture_tool' does not exist."))
        }));
        assert!(result.messages.iter().any(|row| {
            row.role == MessageRole::System
                && row
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("Max invalid tool retries reached (2)"))
        }));
        assert!(result.messages.iter().all(|row| {
            row.tool_call_id.as_deref() != Some("missing-fixture-2")
                && row
                    .tool_calls
                    .iter()
                    .flatten()
                    .all(|call| call.id != "missing-fixture-2")
        }));
    }

    #[tokio::test(start_paused = true)]
    async fn stream_lifetime_caps_a_trickling_response_without_retry() {
        for cap in [Some(0.1), Some(0.0), None] {
            let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::SlowStream));
            let config = AgentConfig {
                stream_max_lifetime: cap,
                skip_memory: true,
                skip_context_files: true,
                ..AgentConfig::default()
            };
            let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone());
            let delivered = Arc::new(Mutex::new(String::new()));
            let captured = delivered.clone();
            let result = agent
                .run_stream(vec![Message::user("run")], None, Some(Box::new(move |chunk| {
                    if let Some(content) = chunk.delta.and_then(|delta| delta.content) {
                        captured.lock().expect("delivered stream").push_str(&content);
                    }
                })))
                .await;

            if cap == Some(0.1) {
                assert!(matches!(
                    result,
                    Err(AgentError::Timeout(message))
                        if message.contains("agent.stream_max_lifetime")
                ), "expected the configured stream timeout");
                assert_eq!(*delivered.lock().expect("delivered stream"), "first second ");
            } else {
                let result = result.expect("uncapped stream should finish");
                assert!(result.messages.iter().any(|message| {
                    message.content.as_deref() == Some("first second third")
                }));
            }
            assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stream_lifetime_resets_on_transport_retry() {
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::SlowRetry));
        let config = AgentConfig {
            stream_max_lifetime: Some(0.1),
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone());
        let result = agent
            .run_stream(vec![Message::user("run")], None, Some(Box::new(|_| {})))
            .await
            .expect("retry has its own deadline");
        assert!(result.messages.iter().any(|message| {
            message.content.as_deref() == Some("retried response")
        }));
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
    }

    #[tokio::test]
    async fn zero_memory_flush_interval_runs_both_loops() {
        struct DoneProvider;

        #[async_trait::async_trait]
        impl LlmProvider for DoneProvider {
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
                    message: Message::assistant("done"),
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
                    Ok(stream_chunk_content("done")),
                    Ok(stream_chunk_finish("stop")),
                ])
                .boxed()
            }
        }

        // An interval of 0 turns the periodic memory sync off rather than
        // dividing the turn count by zero.
        let agent = AgentLoop::new(
            AgentConfig {
                memory_flush_interval: 0,
                skip_memory: true,
                skip_context_files: true,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(DoneProvider),
        );
        let ran = agent
            .run(vec![Message::user("run")], None)
            .await
            .expect("run with memory_flush_interval 0");
        let said_done = |messages: &[Message]| {
            messages
                .iter()
                .any(|m| m.content.as_deref() == Some("done"))
        };
        assert!(said_done(&ran.messages));
        let streamed = agent
            .run_stream(vec![Message::user("run")], None, Some(Box::new(|_| {})))
            .await
            .expect("run_stream with memory_flush_interval 0");
        assert!(said_done(&streamed.messages));
    }

    #[test]
    fn normalize_tool_call_arguments_repairs_malformed_json_without_retry_error() {
        let mut tc = ToolCall {
            id: "call_1".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: "{\"command\":\"ls -la\",\"timeout\":30".to_string(),
            },
            extra_content: None,
        };

        AgentLoop::normalize_tool_call_arguments(&mut tc).expect("repairable arguments");
        let parsed: Value = serde_json::from_str(&tc.function.arguments).expect("valid JSON");
        assert_eq!(parsed["command"], "ls -la");
        assert_eq!(parsed["timeout"], 30);
    }

    #[tokio::test]
    async fn stream_tool_call_arguments_are_repaired_before_truncation_retry_gate() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::MalformedToolArgs,
        ));
        let cfg = AgentConfig {
            stream_read_max_retries: 0,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider);
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &|_| {})
            .await
            .expect("stream should collect");

        let StreamCollectOutcome::Complete(resp) = out else {
            panic!("expected complete response");
        };
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
        let tc = resp
            .message
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .expect("tool call");
        let parsed: Value = serde_json::from_str(&tc.function.arguments).expect("valid JSON");
        assert_eq!(parsed["command"], "ls -la");
        assert_eq!(parsed["timeout"], 30);
    }

    #[tokio::test]
    async fn streaming_retries_truncated_arguments_before_running_the_tool() {
        let executed = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&executed);
        let mut registry = ToolRegistry::new();
        registry.register(
            "fixture_tool",
            ToolSchema::new(
                "fixture_tool",
                "Record fixture input",
                hermes_core::JsonSchema::new("object"),
            ),
            Arc::new(move |args| {
                captured.lock().unwrap().push(args.clone());
                Ok("Fixture input recorded".into())
            }),
        );
        let mut config = AgentConfig {
            model: "openai:fixture-model".into(),
            max_turns: 5,
            truncated_tool_call_max_retries: 1,
            invalid_tool_json_max_retries: 0,
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::TruncatedToolArguments,
        ));
        let notices = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&notices);
        let agent = AgentLoop::new(config, Arc::new(registry), provider.clone()).with_callbacks(
            AgentCallbacks {
                status_callback: Some(Arc::new(move |kind, message| {
                    if kind == "lifecycle" && message.starts_with("Truncated tool arguments") {
                        captured.lock().unwrap().push(message.to_owned());
                    }
                })),
                ..AgentCallbacks::default()
            },
        );

        let result = agent
            .run_stream(
                vec![Message::user("Use the local fixture tool")],
                None,
                Some(Box::new(|_| {})),
            )
            .await
            .unwrap();
        assert!(result.finished_naturally && !result.interrupted);
        assert_eq!(*provider.calls.lock().unwrap(), 3);
        assert_eq!(
            executed.lock().unwrap().as_slice(),
            [serde_json::json!({"value": "complete"})]
        );
        assert_eq!(
            notices.lock().unwrap().as_slice(),
            ["Truncated tool arguments — retrying (1/1)"]
        );
        assert!(result.messages.iter().all(|row| {
            row.tool_call_id.as_deref() != Some("truncated-fixture")
                && row
                    .tool_calls
                    .iter()
                    .flatten()
                    .all(|call| call.id != "truncated-fixture")
        }));
        assert!(result.messages.iter().any(|row| {
            row.tool_call_id.as_deref() == Some("recovered-fixture")
                && row.content.as_deref() == Some("Fixture input recorded")
        }));
        assert_eq!(
            result
                .messages
                .last()
                .and_then(|row| row.content.as_deref()),
            Some("Recorded the complete fixture.")
        );
    }

    #[tokio::test]
    async fn streamed_tool_batches_retry_invalid_arguments_before_executing_siblings() {
        assert!(serde_json::from_str::<serde::de::IgnoredAny>(STREAM_UNREPRESENTABLE_ARGUMENT)
            .is_ok());
        assert!(serde_json::from_str::<Value>(STREAM_UNREPRESENTABLE_ARGUMENT).is_err());

        let executed = Arc::new(Mutex::new(Vec::new()));
        let mut registry = ToolRegistry::new();
        for name in ["fixture_invalid", "fixture_valid"] {
            let executed = Arc::clone(&executed);
            registry.register(
                name,
                ToolSchema::new(name, "Record fixture input", hermes_core::JsonSchema::new("object")),
                Arc::new(move |args| {
                    executed.lock().unwrap().push((name, args.clone()));
                    Ok("recorded fixture input".to_string())
                }),
            );
        }
        let mut config = AgentConfig {
            model: "openai:fixture-model".into(),
            max_turns: 6,
            invalid_tool_json_max_retries: 2,
            truncated_tool_call_max_retries: 0,
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::InvalidArgumentBatch));
        let notices = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&notices);
        let agent = AgentLoop::new(config, Arc::new(registry), provider.clone())
            .with_callbacks(AgentCallbacks {
                status_callback: Some(Arc::new(move |kind, message| {
                    if kind == "lifecycle"
                        && (message.starts_with("Invalid tool JSON")
                            || message.starts_with("Max invalid JSON"))
                    {
                        captured.lock().unwrap().push(message.to_owned());
                    }
                })),
                ..AgentCallbacks::default()
            });
        let result = agent
            .run_stream(
                vec![Message::user("Use the local fixture tool")],
                None,
                Some(Box::new(|_| {})),
            )
            .await
            .unwrap();
        assert!(result.finished_naturally && !result.interrupted);
        assert_eq!(*provider.calls.lock().unwrap(), 4);
        assert_eq!(
            executed.lock().unwrap().as_slice(),
            [("fixture_valid", serde_json::json!({"value":"recovered"}))]
        );
        assert_eq!(
            notices.lock().unwrap().as_slice(),
            [
                "Invalid tool JSON arguments — retrying (1/2)",
                "Max invalid JSON retries reached (2); returning tool errors",
            ]
        );
        assert!(result.messages.iter().all(|row| {
            !matches!(
                row.tool_call_id.as_deref(),
                Some("invalid-fixture-1" | "skipped-fixture-1")
            ) && row.tool_calls.iter().flatten().all(|call| {
                !matches!(call.id.as_str(), "invalid-fixture-1" | "skipped-fixture-1")
            })
        }));
        for (id, expected) in [
            ("invalid-fixture-2", "Error: Invalid JSON arguments"),
            ("skipped-fixture-2", "Skipped: other tool call"),
            ("valid-fixture-3", "recorded fixture input"),
        ] {
            assert!(result.messages.iter().any(|row| {
                row.role == MessageRole::Tool
                    && row.tool_call_id.as_deref() == Some(id)
                    && row.content.as_deref().is_some_and(|text| text.contains(expected))
            }));
        }
        assert_eq!(
            result.messages.last().and_then(|row| row.content.as_deref()),
            Some("Recovered after the tool result")
        );
    }

    #[tokio::test]
    async fn a_stream_that_falls_back_from_its_route_is_labelled_with_the_model_streamed() {
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::SlowStream));
        let cfg = AgentConfig {
            model: "primary-model".to_string(),
            stream_read_max_retries: 0,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider);
        // A provider with no key and no local endpoint: the route cannot be built.
        let mut route = agent.turn_route_cost_guard("routed-model".to_string());
        route.provider = Some("no-such-provider-for-this-test".to_string());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));

        let out = agent
            .collect_stream_llm_response(&ctx, &[], Some(&route), "routed-model", None, &|_| {})
            .await
            .expect("stream should collect");

        let StreamCollectOutcome::Complete(resp) = out else {
            panic!("expected complete response");
        };
        assert_eq!(resp.model, "primary-model");
    }

    #[tokio::test]
    async fn primary_model_is_preserved_when_falling_back_or_retrying_without_tools() {
        use futures::stream::BoxStream;

        struct EchoProvider {
            seen_calls: std::sync::Mutex<Vec<(String, usize)>>,
            retry_error: Option<&'static str>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for EchoProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let model = model.unwrap_or_default().to_string();
                self.seen_calls
                    .lock()
                    .expect("seen calls lock")
                    .push((model.clone(), tools.len()));
                if !tools.is_empty() {
                    return Err(AgentError::LlmApi("Invalid tools schema".into()));
                }
                if let Some(error) = self.retry_error {
                    return Err(AgentError::LlmApi(error.into()));
                }
                // A server labels its reply with the model it was asked for.
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model,
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

        let tool = ToolSchema::new(
            "fixture",
            "Fixture tool",
            hermes_core::JsonSchema::new("object"),
        );
        for (routed, tools, retry_error, expected_tool_counts) in [
            (true, &[][..], None, &[0][..]),
            (true, std::slice::from_ref(&tool), None, &[1, 0]),
            (false, std::slice::from_ref(&tool), Some("Invalid request"), &[1, 0]),
        ] {
            let provider = Arc::new(EchoProvider {
                seen_calls: std::sync::Mutex::new(Vec::new()),
                retry_error,
            });
            let mut cfg = AgentConfig {
                model: "primary-model".to_string(),
                ..AgentConfig::default()
            };
            let _home = isolate_route_learning_home(&mut cfg);
            let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
            let route = routed.then(|| {
                let mut route = agent.turn_route_cost_guard("routed-model".to_string());
                route.provider = Some("no-such-provider-for-this-test".to_string());
                route
            });
            let mut ctx = ContextManager::default_budget();
            ctx.add_message(Message::system("system"));
            ctx.add_message(Message::user("run"));

            let result = agent
                .call_llm_with_retry_inner(&ctx, tools, route.as_ref(), None)
                .await;
            if let Some(error) = retry_error {
                assert!(
                    matches!(result, Err(AgentError::LlmApi(message)) if message.contains(error))
                );
            } else {
                assert_eq!(result.expect("the primary should answer").model, "primary-model");
            }
            let calls = provider.seen_calls.lock().expect("seen calls lock");
            assert!(calls.iter().all(|(model, _)| model == "primary-model"));
            assert_eq!(
                calls.iter().map(|(_, count)| *count).collect::<Vec<_>>(),
                expected_tool_counts
            );
        }
    }

    #[tokio::test]
    async fn stream_mid_tool_call_silent_retry_recovers_tool_call() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::RecoverOnSecondAttempt,
        ));
        let cfg = AgentConfig {
            stream_read_max_retries: 2,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await;

        let StreamCollectOutcome::Complete(resp) = out.expect("stream should recover") else {
            panic!("expected complete response");
        };
        let tc = resp
            .message
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .expect("missing tool call");
        assert_eq!(tc.function.name, "write_file");
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
        assert!(seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
    }

    #[tokio::test]
    async fn stream_mid_tool_call_exhausted_retries_returns_error() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::AlwaysFailMidToolCall,
        ));
        let cfg = AgentConfig {
            stream_read_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await;

        let err = match out {
            Err(err) => err,
            Ok(_) => panic!("should fail after retries"),
        };
        assert!(err.to_string().contains("Stream read error"));
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
        assert!(seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
    }

    #[tokio::test]
    async fn stream_text_only_drop_does_not_retry() {
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::TextOnlyDrop));
        let cfg = AgentConfig {
            stream_read_max_retries: 2,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await;

        let err = match out {
            Err(err) => err,
            Ok(_) => panic!("text-only stream drops should not retry silently"),
        };
        assert!(err.to_string().contains("Stream read error"));
        assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
        assert!(!seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
    }

    #[tokio::test]
    async fn stream_memory_context_blocks_are_scrubbed_from_callbacks_and_final_message() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::MemoryContextLeak,
        ));
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            provider.clone(),
        );
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await
            .expect("stream should complete");

        let StreamCollectOutcome::Complete(resp) = out else {
            panic!("expected complete response");
        };
        assert_eq!(resp.message.content.as_deref(), Some("Hello\n world"));
        let joined = seen.lock().expect("seen lock").join("");
        assert_eq!(joined, "Hello\n world");
        assert!(!joined.contains("secret"));
        assert!(!joined.contains("memory-context"));
    }

    #[tokio::test]
    async fn stream_flushes_visible_context_prefixes_and_discards_unclosed_blocks() {
        for finish in [false, true] {
            for (text, expected, deltas) in [
                (
                    "visible <memory-con",
                    "visible <memory-con",
                    &["visible ", "<memory-con"][..],
                ),
                (
                    "visible\n<memory-context>\nprivate fixture",
                    "visible\n",
                    &["visible\n"][..],
                ),
            ] {
                let mut config = AgentConfig::default();
                let _home = isolate_route_learning_home(&mut config);
                let provider = Arc::new(StreamRetryProvider::new(
                    StreamRetryScenario::BufferedContextTail { text, finish },
                ));
                let callbacks = Arc::new(std::sync::Mutex::new(Vec::new()));
                let captured = callbacks.clone();
                let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone())
                    .with_callbacks(AgentCallbacks {
                        on_stream_delta: Some(Box::new(move |delta: &str| {
                            captured.lock().unwrap().push(delta.to_string());
                        })),
                        ..AgentCallbacks::default()
                    });
                let mut ctx = ContextManager::default_budget();
                ctx.add_message(Message::user("fixture request"));
                let chunks = std::sync::Mutex::new(Vec::new());
                let result = agent
                    .collect_stream_llm_response(
                        &ctx,
                        &[],
                        None,
                        "fixture-model",
                        None,
                        &|chunk| chunks.lock().unwrap().push(chunk),
                    )
                    .await
                    .unwrap();
                let StreamCollectOutcome::Complete(response) = result else {
                    panic!("expected completed stream");
                };
                assert_eq!(response.message.content.as_deref(), Some(expected));
                assert_eq!(response.finish_reason.as_deref(), finish.then_some("stop"));
                assert_eq!(*callbacks.lock().unwrap(), deltas);
                assert_eq!(*provider.calls.lock().unwrap(), 1);
                let chunks = chunks.into_inner().unwrap();
                assert_eq!(chunks.len(), deltas.len() + usize::from(finish));
                for (chunk, expected) in chunks.iter().zip(deltas) {
                    assert_eq!(
                        chunk.delta.as_ref().and_then(|delta| delta.content.as_deref()),
                        Some(*expected)
                    );
                }
                if finish {
                    assert_eq!(chunks.last().unwrap().finish_reason.as_deref(), Some("stop"));
                }
            }
        }
    }
