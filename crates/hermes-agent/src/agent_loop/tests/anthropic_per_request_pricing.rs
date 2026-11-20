struct RetryingHaiku {
    attempts: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl LlmProvider for RetryingHaiku {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        use std::sync::atomic::Ordering;

        let message = if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut reasoning = Message::assistant("");
            reasoning.reasoning_content = Some("reasoning-only response".to_string());
            reasoning
        } else {
            Message::assistant("done")
        };
        Ok(hermes_core::LlmResponse {
            message,
            usage: Some(UsageStats {
                prompt_tokens: 60_000,
                total_tokens: 60_000,
                ..UsageStats::default()
            }),
            model: "claude-haiku-5-5".to_string(),
            finish_reason: Some("end_turn".to_string()),
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
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        unreachable!("this fixture exercises non-streaming semantic retries")
    }
}

#[tokio::test]
async fn haiku_price_tier_is_applied_to_each_semantic_retry() {
    use std::sync::atomic::Ordering;

    let mut config = AgentConfig {
        model: "anthropic:claude-haiku-5-5".to_string(),
        skip_memory: true,
        skip_context_files: true,
        thinking_prefill_max_retries: 1,
        max_cost_usd: Some(0.02),
        ..AgentConfig::default()
    };
    let _home = isolate_route_learning_home(&mut config);
    let provider = Arc::new(RetryingHaiku {
        attempts: std::sync::atomic::AtomicUsize::new(0),
    });
    let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone());
    let result = agent
        .run(vec![Message::user("answer briefly")], None)
        .await
        .expect("Haiku completes the turn");

    assert_eq!(provider.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(result.usage.expect("usage").prompt_tokens, 120_000);
    let cost = result.session_cost_usd.expect("price for both requests");
    assert!((cost - 0.012).abs() < 1e-9, "{cost}");
    assert!(result.finished_naturally);
}
