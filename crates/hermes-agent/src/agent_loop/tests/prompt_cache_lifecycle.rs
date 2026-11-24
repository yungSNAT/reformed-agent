/// Answers "done" with a cached-prefix usage and records each request's
/// message count and output cap.
struct LifecycleProvider {
    seen: Mutex<Vec<(usize, Option<u32>)>>,
}

#[async_trait::async_trait]
impl LlmProvider for LifecycleProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.seen
            .lock()
            .expect("seen")
            .push((messages.len(), max_tokens));
        Ok(LlmResponse {
            message: Message::assistant("done"),
            usage: Some(hermes_core::UsageStats {
                prompt_tokens: 40_010,
                completion_tokens: 2,
                total_tokens: 40_012,
                estimated_cost: None,
                cache_read_tokens: 40_000,
                cache_write_tokens: 0,
            }),
            model: "claude-test".to_string(),
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

fn lifecycle_agent(
    session: &str,
    prompt_caching: hermes_config::PromptCachingConfig,
) -> (AgentLoop, Arc<LifecycleProvider>, tempfile::TempDir) {
    let provider = Arc::new(LifecycleProvider {
        seen: Mutex::new(Vec::new()),
    });
    let mut config = AgentConfig {
        model: "anthropic:claude-test".to_string(),
        skip_memory: true,
        skip_context_files: true,
        session_id: Some(session.to_string()),
        // A keep-alive needs a known price; the test model has none of its own.
        prompt_cost_per_million_usd: Some(5.0),
        completion_cost_per_million_usd: Some(25.0),
        prompt_caching,
        ..AgentConfig::default()
    };
    let home = isolate_route_learning_home(&mut config);
    let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider.clone());
    (agent, provider, home)
}

#[tokio::test(start_paused = true)]
async fn keep_alive_replays_the_last_request_after_a_run_only_when_opted_in() {
    let (agent, provider, _home) =
        lifecycle_agent("lifecycle-off", hermes_config::PromptCachingConfig::default());
    agent
        .run(vec![Message::user("hi")], Some(Vec::new()))
        .await
        .expect("run");
    tokio::time::sleep(Duration::from_secs(7_200)).await;
    assert_eq!(provider.seen.lock().expect("seen").len(), 1, "off by default");

    let on = hermes_config::PromptCachingConfig {
        keep_alive: hermes_config::PromptCacheKeepAliveConfig {
            max_pings: 2,
            max_idle_minutes: 60,
        },
        ..Default::default()
    };
    let (agent, provider, _home) = lifecycle_agent("lifecycle-on", on);
    agent
        .run(vec![Message::user("hi")], Some(Vec::new()))
        .await
        .expect("run");
    tokio::time::sleep(Duration::from_secs(7_200)).await;
    let seen = provider.seen.lock().expect("seen").clone();
    assert_eq!(seen.len(), 3, "the turn, then 2 keep-alives: {seen:?}");
    assert_eq!(seen[1], (seen[0].0, Some(1)));
    assert_eq!(seen[2], (seen[0].0, Some(1)));
    crate::context::cache_keep_alive::forget("lifecycle-on");
}

#[tokio::test(start_paused = true)]
async fn a_background_review_leaves_the_sessions_keep_alive_alone() {
    let on = hermes_config::PromptCachingConfig {
        keep_alive: hermes_config::PromptCacheKeepAliveConfig {
            max_pings: 2,
            max_idle_minutes: 60,
        },
        ..Default::default()
    };
    let (agent, provider, _home) = lifecycle_agent("lifecycle-review", on);
    agent
        .run(vec![Message::user("hi")], Some(Vec::new()))
        .await
        .expect("run");
    let review_provider = Arc::new(LifecycleProvider {
        seen: Mutex::new(Vec::new()),
    });
    let review = AgentLoop::new(
        agent.background_review_config(),
        Arc::new(ToolRegistry::new()),
        review_provider.clone(),
    );
    review
        .run(
            vec![Message::user("hi"), Message::user("review")],
            Some(Vec::new()),
        )
        .await
        .expect("review run");
    tokio::time::sleep(Duration::from_secs(7_200)).await;
    let seen = provider.seen.lock().expect("seen").clone();
    assert_eq!(seen.len(), 3, "the turn, then its 2 keep-alives: {seen:?}");
    assert_eq!(seen[1], (seen[0].0, Some(1)));
    let review_seen = review_provider.seen.lock().expect("seen").clone();
    assert_eq!(review_seen.len(), 1, "the review only: {review_seen:?}");
    crate::context::cache_keep_alive::forget("lifecycle-review");
}

#[test]
fn the_one_hour_tier_applies_only_on_the_anthropic_provider() {
    let one_hour = hermes_config::PromptCachingConfig {
        cache_ttl: hermes_config::PromptCacheTtl::OneHour,
        ..Default::default()
    };
    for (model, tier) in [
        ("anthropic:claude-test", hermes_config::PromptCacheTtl::OneHour),
        ("bedrock:anthropic.claude-test", hermes_config::PromptCacheTtl::FiveMinutes),
        ("openrouter:anthropic/claude-test", hermes_config::PromptCacheTtl::FiveMinutes),
    ] {
        let config = AgentConfig {
            model: model.to_string(),
            prompt_caching: one_hour,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(LifecycleProvider {
                seen: Mutex::new(Vec::new()),
            }),
        );
        assert_eq!(agent.prompt_cache_ttl(), tier, "{model}");
        let body = agent
            .extra_body_for_api_mode(&agent.config.api_mode)
            .unwrap_or_default();
        assert_eq!(
            body.get(crate::provider::PROMPT_CACHE_TTL_KEY).is_some(),
            tier == hermes_config::PromptCacheTtl::OneHour,
            "{model}: {body}"
        );
    }
}

#[tokio::test]
async fn a_cold_return_compacts_a_half_full_history_before_the_first_request_when_opted_in() {
    let history = || {
        let mut messages = Vec::new();
        for i in 0..20 {
            messages.push(Message::user(format!("question {i} {}", "x".repeat(3_000))));
            messages.push(Message::assistant(format!("answer {i} {}", "y".repeat(3_000))));
        }
        messages.push(Message::user("and now?"));
        messages
    };
    let cold = Instant::now()
        .checked_sub(Duration::from_secs(7_200))
        .expect("2 hours ago");
    let mut sent = Vec::new();
    for (session, compact_when_cold, cold_clock) in [
        ("lifecycle-cold-off", false, true),
        ("lifecycle-warm-on", true, false),
        ("lifecycle-cold-on", true, true),
    ] {
        let (agent, provider, _home) = lifecycle_agent(
            session,
            hermes_config::PromptCachingConfig {
                compact_when_cold,
                ..Default::default()
            },
        );
        let at = if cold_clock { cold } else { Instant::now() };
        crate::context::tool_result_prune::record_cache_touch_at(session, at);
        agent.run(history(), Some(Vec::new())).await.expect("run");
        sent.push(provider.seen.lock().expect("seen")[0].0);
    }
    // About 120,000 characters of a 200,000 budget is over half the 80%
    // threshold but under the threshold itself.
    assert_eq!(sent[0], sent[1], "no compaction unless opted in and cold");
    assert!(sent[2] < sent[0] / 2, "compacted first: {sent:?}");
}
