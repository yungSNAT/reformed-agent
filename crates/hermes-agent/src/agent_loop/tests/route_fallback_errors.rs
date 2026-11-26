/// Primary runtime that fails every request with a retryable error and
/// records the model each request asked for.
#[derive(Default)]
struct FailingPrimary {
    models: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl LlmProvider for FailingPrimary {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        self.models
            .lock()
            .unwrap()
            .push(model.unwrap_or_default().to_string());
        Err(AgentError::LlmApi("HTTP 404 Not Found".into()))
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
        Box::pin(futures::stream::empty())
    }
}

/// When the turn's route cannot be built, the primary answers, so the retry
/// failover chain starts from the primary's model: it must not resend the
/// primary model that just failed.
#[test]
fn after_a_route_falls_back_the_failover_chain_starts_from_the_primary() {
    let _env_lock = env_test_lock();
    let _no_key = EnvVarGuard::remove("DEEPSEEK_API_KEY");
    let mut config = AgentConfig {
        model: "openai:primary-model".to_string(),
        provider: Some("openai".to_string()),
        retry: RetryConfig {
            max_retries: 0,
            fallback_models: vec![
                "openai:primary-model".to_string(),
                "openai:spare-model".to_string(),
            ],
            ..RetryConfig::default()
        },
        ..AgentConfig::default()
    };
    let _home = isolate_route_learning_home(&mut config);
    let primary = Arc::new(FailingPrimary::default());
    let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), primary.clone());
    // A route to a provider with no key: it cannot be built, so the primary answers.
    let mut route = agent.turn_route_cost_guard("deepseek:route-model".to_string());
    route.provider = Some("deepseek".to_string());
    route.api_key_env = Some("HERMES_TEST_ABSENT_ROUTE_KEY".to_string());
    route.credential_pool = None;
    route.credential_pool_fallback = false;
    let mut ctx = ContextManager::new(100_000);
    ctx.add_message(Message::user("hello"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let result = runtime.block_on(agent.call_llm_with_retry(&ctx, &[], Some(&route), None));

    assert!(result.is_err());
    let models = primary.models.lock().unwrap().clone();
    assert_eq!(
        models
            .iter()
            .filter(|m| m.contains("primary-model"))
            .count(),
        1,
        "{models:?}"
    );
    assert!(
        models.iter().any(|m| m.contains("spare-model")),
        "{models:?}"
    );
}
