/// Each request: 60,000 Haiku prompt tokens, $0.006.
struct SixMilliDollarHaiku {
    calls: std::sync::atomic::AtomicUsize,
}

fn six_milli_dollar_usage() -> UsageStats {
    UsageStats {
        prompt_tokens: 60_000,
        total_tokens: 60_000,
        ..UsageStats::default()
    }
}

#[async_trait::async_trait]
impl LlmProvider for SixMilliDollarHaiku {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(hermes_core::LlmResponse {
            message: Message::assistant("done"),
            usage: Some(six_milli_dollar_usage()),
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
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        futures::StreamExt::boxed(futures::stream::iter(vec![
            Ok(StreamChunk {
                delta: Some(hermes_core::StreamDelta {
                    content: Some("done".to_string()),
                    tool_calls: None,
                    extra: None,
                }),
                finish_reason: None,
                usage: None,
            }),
            Ok(StreamChunk {
                delta: None,
                finish_reason: Some("stop".into()),
                usage: Some(six_milli_dollar_usage()),
            }),
        ]))
    }
}

fn capped_haiku_agent(cap: f64) -> (AgentLoop, Arc<SixMilliDollarHaiku>, tempfile::TempDir) {
    let mut config = AgentConfig {
        model: "anthropic:claude-haiku-5-5".to_string(),
        skip_memory: true,
        skip_context_files: true,
        max_cost_usd: Some(cap),
        ..AgentConfig::default()
    };
    let home = isolate_route_learning_home(&mut config);
    let provider = Arc::new(SixMilliDollarHaiku {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone());
    (agent, provider, home)
}

fn tripped(result: &AgentResult) -> bool {
    result.messages.iter().any(|m| {
        m.content
            .as_deref()
            .is_some_and(|text| text.starts_with("Cost guard tripped"))
    })
}

#[tokio::test]
async fn the_cost_cap_counts_what_earlier_runs_of_the_session_spent() {
    let (agent, _provider, _home) = capped_haiku_agent(0.01);
    let first = agent
        .run(vec![Message::user("one")], None)
        .await
        .expect("first run");
    assert!(first.finished_naturally, "$0.006 is under the $0.01 cap");
    let first_cost = first.session_cost_usd.expect("priced");

    agent.set_session_spent_usd(first_cost);
    let second = agent
        .run(vec![Message::user("two")], None)
        .await
        .expect("second run");
    assert!(
        !second.finished_naturally,
        "$0.006 earlier plus $0.006 now passes the $0.01 cap"
    );
    assert!(tripped(&second));
    let second_cost = second.session_cost_usd.expect("priced");
    assert!(
        (second_cost - 0.006).abs() < 1e-9,
        "a run reports its own spend, not the session's: {second_cost}"
    );
}

#[tokio::test]
async fn a_streamed_run_counts_what_earlier_runs_of_the_session_spent() {
    let (agent, _provider, _home) = capped_haiku_agent(0.01);
    agent.set_session_spent_usd(0.006);
    let result = agent
        .run_stream(vec![Message::user("two")], None, None)
        .await
        .expect("streamed run");
    assert!(!result.finished_naturally);
    assert!(tripped(&result));
}

#[tokio::test]
async fn a_session_already_at_its_cap_makes_no_request() {
    use std::sync::atomic::Ordering;

    let (agent, provider, _home) = capped_haiku_agent(0.01);
    agent.set_session_spent_usd(0.01);
    let result = agent
        .run(vec![Message::user("three")], None)
        .await
        .expect("refused run");
    let streamed = agent
        .run_stream(vec![Message::user("four")], None, None)
        .await
        .expect("refused streamed run");

    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    for refused in [&result, &streamed] {
        assert!(!refused.finished_naturally);
        assert_eq!(refused.total_turns, 0);
        assert!(tripped(refused));
        assert_eq!(refused.session_cost_usd, Some(0.0));
    }
}
