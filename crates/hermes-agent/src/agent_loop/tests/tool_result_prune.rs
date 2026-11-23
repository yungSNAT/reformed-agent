#[derive(Default)]
struct RecordingProvider {
    seen: Mutex<Vec<Vec<Message>>>,
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
        futures::stream::empty().boxed()
    }
}

/// An earlier turn whose tool returned `size` characters, then enough short
/// turns to age it out of the protected tail, then the new question.
fn pruneable_history(size: usize) -> Vec<Message> {
    let mut call = Message::assistant("");
    call.tool_calls = Some(vec![hermes_core::ToolCall {
        id: "old_call".into(),
        function: hermes_core::FunctionCall {
            name: "terminal".into(),
            arguments: r#"{"command":"cat big.log"}"#.into(),
        },
        extra_content: None,
    }]);
    let mut out = vec![
        Message::user("show the log"),
        call,
        Message::tool_result("old_call", "L".repeat(size)),
        Message::assistant("shown"),
    ];
    for i in 0..12 {
        out.push(Message::user(format!("q{i}")));
        out.push(Message::assistant(format!("a{i}")));
    }
    out.push(Message::user("next question"));
    out
}

fn prune_agent(session_id: &str) -> (AgentLoop, Arc<RecordingProvider>, tempfile::TempDir) {
    let mut registry = ToolRegistry::new();
    registry.register(
        "read_file",
        ToolSchema::new("read_file", "Read a file", JsonSchema::new("object")),
        Arc::new(|_args| Ok("ok".to_string())),
    );
    let mut config = AgentConfig {
        skip_memory: true,
        skip_context_files: true,
        session_id: Some(session_id.to_string()),
        model: "anthropic/claude-test".into(),
        ..AgentConfig::default()
    };
    let home = isolate_route_learning_home(&mut config);
    let provider = Arc::new(RecordingProvider::default());
    let agent = AgentLoop::new(config, Arc::new(registry), provider.clone());
    (agent, provider, home)
}

fn old_result_sent(provider: &RecordingProvider) -> String {
    let seen = provider.seen.lock().expect("seen lock");
    seen[0]
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("old_call"))
        .and_then(|m| m.content.clone())
        .expect("the old result was sent")
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

#[test]
fn a_turn_after_the_prompt_cache_expired_archives_old_tool_results() {
    let (agent, provider, home) = prune_agent("prune-cold-session");
    let six_minutes_ago = Instant::now()
        .checked_sub(Duration::from_secs(6 * 60))
        .expect("instant");
    tool_result_prune::record_cache_touch_at("prune-cold-session", six_minutes_ago);

    let result = block_on(agent.run(pruneable_history(70_000), None)).expect("run");

    let sent = old_result_sent(&provider);
    assert!(sent.starts_with("[terminal] command=cat big.log (70000 chars result)"));
    let archive = tool_result_prune::archive_pointer(&sent).expect("the stub names its archive");
    assert!(std::path::Path::new(archive).starts_with(home.path()));
    assert_eq!(std::fs::read_to_string(archive).expect("archive"), "L".repeat(70_000));
    let kept = result
        .messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("old_call"))
        .and_then(|m| m.content.as_deref());
    assert_eq!(kept, Some(sent.as_str()), "the returned history keeps the stub");
}

#[test]
fn a_turn_on_a_warm_or_unknown_cache_sends_old_tool_results_whole() {
    for (session, touched) in [("prune-warm-session", true), ("prune-unknown-session", false)] {
        let (agent, provider, _home) = prune_agent(session);
        if touched {
            tool_result_prune::record_cache_touch(session);
        }
        block_on(agent.run(pruneable_history(70_000), None)).expect("run");
        assert_eq!(old_result_sent(&provider), "L".repeat(70_000), "{session}");
    }
}

#[test]
fn a_cold_prune_needs_a_reclaim_worth_the_rewrite() {
    let (agent, provider, _home) = prune_agent("prune-small-session");
    let past = Instant::now()
        .checked_sub(Duration::from_secs(6 * 60))
        .expect("instant");
    tool_result_prune::record_cache_touch_at("prune-small-session", past);

    block_on(agent.run(pruneable_history(20_000), None)).expect("run");

    assert_eq!(old_result_sent(&provider), "L".repeat(20_000));
}

#[test]
fn a_run_starts_the_prompt_cache_clock_again() {
    let (agent, _provider, _home) = prune_agent("prune-clock-session");
    let past = Instant::now()
        .checked_sub(Duration::from_secs(6 * 60))
        .expect("instant");
    tool_result_prune::record_cache_touch_at("prune-clock-session", past);
    let ttl = Duration::from_secs(5 * 60);
    assert!(tool_result_prune::cache_expired("prune-clock-session", ttl));

    block_on(agent.run(vec![Message::user("hi")], None)).expect("run");

    assert!(!tool_result_prune::cache_expired("prune-clock-session", ttl));
}

#[test]
fn compaction_archives_old_tool_results_before_it_summarises() {
    let (agent, _provider, _home) = prune_agent("prune-compact-session");
    let mut ctx = ContextManager::new(100_000);
    for msg in pruneable_history(85_000) {
        ctx.add_message(msg);
    }

    agent.auto_compress_if_over_threshold(&mut ctx);

    let messages = ctx.get_messages();
    assert!(ctx.compressed_transcripts().is_empty(), "nothing was summarised");
    assert_eq!(messages.len(), pruneable_history(0).len());
    let stub = messages[2].content.as_deref().expect("stub");
    assert!(tool_result_prune::archive_pointer(stub).is_some(), "{stub}");
}

#[test]
fn without_read_file_nothing_is_archived() {
    let mut config = AgentConfig {
        skip_memory: true,
        skip_context_files: true,
        session_id: Some("prune-no-reader".into()),
        ..AgentConfig::default()
    };
    let home = isolate_route_learning_home(&mut config);
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(RecordingProvider::default()),
    );
    let mut ctx = ContextManager::new(100_000);
    for msg in pruneable_history(85_000) {
        ctx.add_message(msg);
    }

    agent.auto_compress_if_over_threshold(&mut ctx);

    assert_eq!(ctx.compressed_transcripts().len(), 1, "it summarised instead");
    assert!(!home.path().join("cache").exists());
}
