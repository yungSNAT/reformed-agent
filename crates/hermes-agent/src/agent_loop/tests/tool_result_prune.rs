#[derive(Default)]
struct PromptRecorder {
    seen: Mutex<Vec<Vec<Message>>>,
}

#[async_trait::async_trait]
impl LlmProvider for PromptRecorder {
    async fn chat_completion(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        _: Option<u32>,
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
        _: Option<u32>,
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

fn prune_agent(session_id: &str) -> (AgentLoop, Arc<PromptRecorder>, tempfile::TempDir) {
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
    let provider = Arc::new(PromptRecorder::default());
    let agent = AgentLoop::new(config, Arc::new(registry), provider.clone());
    (agent, provider, home)
}

fn old_result_sent(provider: &PromptRecorder) -> String {
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
    // Needs the local terminal backend; other tests set these variables.
    let _env = env_test_lock();
    let saved: Vec<_> = ["TERMINAL_ENV", "TERMINAL_BACKEND"]
        .map(|key| (key, std::env::var_os(key)))
        .into();
    for (key, _) in &saved {
        std::env::remove_var(key);
    }
    let (agent, provider, home) = prune_agent("prune-cold-session");
    let six_minutes_ago = Instant::now()
        .checked_sub(Duration::from_secs(6 * 60))
        .expect("instant");
    tool_result_prune::record_cache_touch_at("prune-cold-session", six_minutes_ago);

    let result = block_on(agent.run(pruneable_history(70_000), None));
    for (key, value) in saved {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    let result = result.expect("run");

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
    let (agent, _provider, home) = prune_agent("prune-compact-session");
    let mut ctx = ContextManager::new(100_000);
    ctx.set_tool_result_archive(home.path().join(tool_result_prune::PRUNED_SUBDIR));
    for msg in pruneable_history(85_000) {
        ctx.add_message(msg);
    }

    agent.auto_compress_if_over_threshold(&mut ctx);

    let messages = ctx.get_messages();
    assert_eq!(messages.len(), pruneable_history(0).len(), "nothing was summarised");
    let stub = messages[2].content.as_deref().expect("stub");
    assert!(tool_result_prune::archive_pointer(stub).is_some(), "{stub}");
    assert!(ctx.compressed_transcripts().is_empty(), "nothing was compressed");
    let kept = ctx.take_compressed_transcripts();
    assert_eq!(kept.len(), 1, "the replaced result is kept");
    assert_eq!(kept[0].len(), 2, "only the call and its original result");
    assert_eq!(kept[0][0].tool_calls.as_ref().map(Vec::len), Some(1));
    assert_eq!(kept[0][1].content.as_deref(), Some("L".repeat(85_000).as_str()));
}

#[test]
fn compaction_keeps_a_nonempty_memory_note_in_the_prompt() {
    struct CompressionMemory {
        note: &'static str,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    impl crate::memory_manager::MemoryProviderPlugin for CompressionMemory {
        fn name(&self) -> &str {
            "test-compression-memory"
        }

        fn on_pre_compress(&self, messages: &[Value]) -> String {
            *self.seen.lock().unwrap() = messages.to_vec();
            self.note.to_owned()
        }
    }

    let history = [
        Message::user("Describe the outline for the essay."),
        Message::assistant("Start with a short introduction and cover two examples."),
        Message::user("Use the first example as the opening."),
        Message::assistant("The outline now starts with the first example."),
    ];
    let expected = serde_json::to_value(&history).unwrap();

    for note in ["Keep the essay's first example.", " \n"] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut manager = crate::memory_manager::MemoryManager::new();
        manager.add_provider(Arc::new(CompressionMemory {
            note,
            seen: seen.clone(),
        }));
        let agent = AgentLoop::new(
            AgentConfig {
                isolated: true,
                ..Default::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(PromptRecorder::default()),
        )
        .with_memory(Arc::new(Mutex::new(manager)));
        let mut ctx =
            ContextManager::with_compressor(100, crate::context::ContextCompressor::new(2));
        for message in &history {
            ctx.add_message(message.clone());
        }

        agent.auto_compress_if_over_threshold(&mut ctx);

        assert_eq!(
            seen.lock().unwrap().as_slice(),
            expected.as_array().unwrap()
        );
        let mut archived = history.to_vec();
        if !note.trim().is_empty() {
            let note = Message::system(note);
            assert!(ctx.get_messages().contains(&note));
            archived.push(note);
        }
        assert_eq!(ctx.compressed_transcripts(), &[archived]);
    }
}

#[test]
fn a_cold_prune_keeps_the_persisted_user_message_override() {
    let _env = env_test_lock();
    let saved: Vec<_> = ["TERMINAL_ENV", "TERMINAL_BACKEND"]
        .map(|key| (key, std::env::var_os(key)))
        .into();
    for (key, _) in &saved {
        std::env::remove_var(key);
    }
    let (mut agent, provider, _home) = prune_agent("prune-override-session");
    agent.config.persist_user_message = Some("as typed".into());
    let past = Instant::now()
        .checked_sub(Duration::from_secs(6 * 60))
        .expect("instant");
    tool_result_prune::record_cache_touch_at("prune-override-session", past);

    let result = block_on(agent.run(pruneable_history(70_000), None));
    for (key, value) in saved {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    let result = result.expect("run");

    assert!(tool_result_prune::archive_pointer(&old_result_sent(&provider)).is_some());
    let last_user = result
        .messages
        .iter()
        .rfind(|m| m.role == MessageRole::User)
        .and_then(|m| m.content.as_deref());
    assert_eq!(last_user, Some("as typed"));
}

#[test]
fn compaction_without_a_session_id_summarises_instead_of_archiving() {
    let (mut agent, _provider, home) = prune_agent("unused");
    agent.config.session_id = None;
    let mut ctx = ContextManager::new(100_000);
    ctx.set_tool_result_archive(home.path().join(tool_result_prune::PRUNED_SUBDIR));
    for msg in pruneable_history(85_000) {
        ctx.add_message(msg);
    }

    agent.auto_compress_if_over_threshold(&mut ctx);

    assert!(ctx.get_messages().len() < pruneable_history(0).len(), "it summarised");
    assert!(!home.path().join(tool_result_prune::PRUNED_SUBDIR).exists());
}

#[test]
fn compaction_without_an_archive_summarises() {
    let (agent, _provider, home) = prune_agent("prune-no-archive");
    let mut ctx = ContextManager::new(100_000);
    for msg in pruneable_history(85_000) {
        ctx.add_message(msg);
    }

    agent.auto_compress_if_over_threshold(&mut ctx);

    assert!(ctx.get_messages().len() < pruneable_history(0).len(), "it summarised");
    assert!(!home.path().join("cache").exists());
}

#[test]
fn a_turn_that_does_not_offer_read_file_archives_nothing() {
    let (agent, provider, home) = prune_agent("prune-no-reader-offered");
    let past = Instant::now()
        .checked_sub(Duration::from_secs(6 * 60))
        .expect("instant");
    tool_result_prune::record_cache_touch_at("prune-no-reader-offered", past);
    let other = ToolSchema::new("web_search", "Search", JsonSchema::new("object"));

    block_on(agent.run(pruneable_history(70_000), Some(vec![other]))).expect("run");

    assert_eq!(old_result_sent(&provider), "L".repeat(70_000));
    assert!(!home.path().join("cache").exists());
}

#[test]
fn a_remote_terminal_backend_archives_nothing() {
    let _env = env_test_lock();
    let saved = std::env::var_os("TERMINAL_ENV");
    std::env::set_var("TERMINAL_ENV", "docker");
    let (agent, provider, home) = prune_agent("prune-docker-session");
    let past = Instant::now()
        .checked_sub(Duration::from_secs(6 * 60))
        .expect("instant");
    tool_result_prune::record_cache_touch_at("prune-docker-session", past);

    let result = block_on(agent.run(pruneable_history(70_000), None));
    match saved {
        Some(value) => std::env::set_var("TERMINAL_ENV", value),
        None => std::env::remove_var("TERMINAL_ENV"),
    }

    result.expect("run");
    assert_eq!(old_result_sent(&provider), "L".repeat(70_000));
    assert!(!home.path().join("cache").exists());
}

struct HangingProvider;

#[async_trait::async_trait]
impl LlmProvider for HangingProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        futures::future::pending().await
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        futures::stream::pending().boxed()
    }
}

#[test]
fn an_abandoned_run_still_restarts_the_clock() {
    let mut config = AgentConfig {
        skip_memory: true,
        skip_context_files: true,
        session_id: Some("prune-abandoned-session".into()),
        ..AgentConfig::default()
    };
    let _home = isolate_route_learning_home(&mut config);
    let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), Arc::new(HangingProvider));
    let past = Instant::now()
        .checked_sub(Duration::from_secs(6 * 60))
        .expect("instant");
    tool_result_prune::record_cache_touch_at("prune-abandoned-session", past);
    let ttl = Duration::from_secs(5 * 60);

    let outcome = block_on(async {
        tokio::time::timeout(
            Duration::from_millis(300),
            agent.run(vec![Message::user("hi")], None),
        )
        .await
    });

    assert!(outcome.is_err(), "the run was cut off");
    assert!(!tool_result_prune::cache_expired("prune-abandoned-session", ttl));
}
