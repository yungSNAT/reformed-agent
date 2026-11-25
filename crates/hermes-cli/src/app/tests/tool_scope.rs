    /// Records the tool names each request offers the model.
    #[derive(Default)]
    struct OfferedToolsRecorder {
        offered: StdMutex<Vec<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for OfferedToolsRecorder {
        async fn chat_completion(
            &self,
            _messages: &[hermes_core::Message],
            tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let mut names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
            names.sort();
            self.offered.lock().unwrap().push(names);
            Ok(hermes_core::LlmResponse {
                message: hermes_core::Message::assistant("answer"),
                usage: None,
                model: "recorder".into(),
                finish_reason: Some("stop".into()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[hermes_core::Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
            use futures::StreamExt;
            futures::stream::empty().boxed()
        }
    }

    /// A quorum pass run without tools (as after a provider rejects the tool
    /// schemas) offers the model no tools, not every tool the agent holds.
    #[test]
    fn a_turn_run_without_tools_offers_none() {
        let mut app = build_minimal_test_app();
        register_test_tool(&app.tool_registry, "planned_tool");
        register_test_tool(&app.tool_registry, "other_tool");
        app.tool_schemas = vec![ToolSchema::new(
            "planned_tool",
            "test tool",
            hermes_core::JsonSchema::new("object"),
        )];
        let recorder = Arc::new(OfferedToolsRecorder::default());
        let mut config = build_agent_config(app.config.as_ref(), "dynamic");
        config.skip_memory = true;
        config.skip_context_files = true;
        config.background_review_enabled = false;
        app.agent = Arc::new(AgentLoop::new(
            config,
            Arc::new(bridge_tool_registry(&app.tool_registry)),
            recorder.clone(),
        ));

        block_on_test(app.run_messages_with_current_agent_tools(
            vec![hermes_core::Message::user("hi")],
            false,
            false,
        ))
        .expect("tool-less turn");
        block_on_test(app.run_messages_with_current_agent_tools(
            vec![hermes_core::Message::user("hi")],
            false,
            true,
        ))
        .expect("turn with tools");

        let offered = recorder.offered.lock().unwrap().clone();
        assert_eq!(offered.first(), Some(&Vec::<String>::new()));
        assert_eq!(offered.last(), Some(&vec!["planned_tool".to_string()]));
    }

    /// Every request costs $0.004.
    #[derive(Default)]
    struct FourMilliDollarProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for FourMilliDollarProvider {
        async fn chat_completion(
            &self,
            _messages: &[hermes_core::Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(hermes_core::LlmResponse {
                message: hermes_core::Message::assistant("answer"),
                usage: Some(hermes_core::UsageStats {
                    prompt_tokens: 10,
                    total_tokens: 10,
                    estimated_cost: Some(0.004),
                    ..Default::default()
                }),
                model: "priced".into(),
                finish_reason: Some("stop".into()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[hermes_core::Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
            use futures::StreamExt;
            futures::stream::empty().boxed()
        }
    }

    /// Runs whose results are never applied (objective continuations, quorum
    /// voters) still count towards the session's spend cap.
    #[test]
    fn runs_whose_results_are_not_applied_count_towards_the_spend_cap() {
        let mut app = build_minimal_test_app();
        let provider = Arc::new(FourMilliDollarProvider::default());
        let mut config = build_agent_config(app.config.as_ref(), "dynamic");
        config.skip_memory = true;
        config.skip_context_files = true;
        config.background_review_enabled = false;
        config.max_cost_usd = Some(0.01);
        app.agent = Arc::new(AgentLoop::new(
            config,
            Arc::new(bridge_tool_registry(&app.tool_registry)),
            provider.clone(),
        ));

        for _ in 0..2 {
            let run = block_on_test(app.run_messages_with_current_agent_tools(
                vec![hermes_core::Message::user("hi")],
                false,
                false,
            ))
            .expect("run under the cap");
            assert!(run.finished_naturally);
        }
        assert!((app.session_cost_usd - 0.008).abs() < 1e-9, "{}", app.session_cost_usd);

        let third = block_on_test(app.run_messages_with_current_agent_tools(
            vec![hermes_core::Message::user("hi")],
            false,
            false,
        ))
        .expect("third run");
        assert!(!third.finished_naturally, "$0.008 + $0.004 passes $0.01");
        let fourth = block_on_test(app.run_messages_with_current_agent_tools(
            vec![hermes_core::Message::user("hi")],
            false,
            false,
        ))
        .expect("refused run");
        assert_eq!(fourth.total_turns, 0);
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }
