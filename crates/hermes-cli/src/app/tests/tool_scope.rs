    /// Records the tool names each request offers the model; each request
    /// reports `cost_usd` when set.
    #[derive(Default)]
    struct OfferedToolsRecorder {
        offered: StdMutex<Vec<Vec<String>>>,
        cost_usd: Option<f64>,
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
                usage: self.cost_usd.map(|cost| hermes_core::UsageStats {
                    estimated_cost: Some(cost),
                    ..Default::default()
                }),
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

    #[test]
    fn resuming_a_capped_app_reads_spend_and_reset_clears_it_while_the_cap_is_off() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut first = build_minimal_test_app_with_state_root(tmp.path().into());
        let id = first.session_id.clone();
        let provider = Arc::new(OfferedToolsRecorder { cost_usd: Some(0.004), ..Default::default() });
        let mut config = build_agent_config(first.config.as_ref(), "dynamic");
        config.skip_memory = true;
        config.skip_context_files = true;
        config.background_review_enabled = false;
        config.max_cost_usd = Some(0.01);
        config.session_id = Some(id.clone());
        first.agent = Arc::new(AgentLoop::new(config, Arc::new(bridge_tool_registry(&first.tool_registry)), provider.clone())
            .with_session_budget_root(tmp.path().into()));
        let run = block_on_test(first.run_messages_with_current_agent_tools(
            vec![hermes_core::Message::user("fixture")], false, false)).unwrap();
        assert!(run.finished_naturally);
        drop(first);

        let mut resumed = build_minimal_test_app_with_state_root(tmp.path().into());
        Arc::make_mut(&mut resumed.config).agent.max_session_cost_usd = Some(0.01);
        resumed.resume_session_id(id.clone());
        assert!((resumed.session_cost_usd - 0.004).abs() < 1e-9);
        // Keep the production resume builder's session id and state root, replacing only its provider.
        let registry = Arc::clone(&resumed.agent.tool_registry);
        let mut recovered = resumed.agent.clone_with_tool_registry(registry);
        recovered.llm_provider = provider;
        recovered.config.skip_memory = true;
        recovered.config.skip_context_files = true;
        recovered.config.background_review_enabled = false;
        resumed.agent = Arc::new(recovered);
        let second = block_on_test(resumed.run_messages_with_current_agent_tools(
            vec![hermes_core::Message::user("after restart")], false, false)).unwrap();
        assert!(second.finished_naturally);
        let store = SessionPersistence::new(tmp.path());
        assert!((store.session_spend_usd(&id).unwrap() - 0.008).abs() < 1e-9);
        Arc::make_mut(&mut resumed.config).agent.max_session_cost_usd = None;
        resumed.reset_session();
        assert_eq!(store.session_spend_usd(&id).unwrap(), 0.0);
        resumed.new_session();
        assert_ne!(resumed.session_id, id);
        assert_eq!(store.session_spend_usd(&resumed.session_id).unwrap(), 0.0);
    }

    /// Runs whose results are never applied (objective continuations, quorum
    /// voters) still count towards the session's spend cap.
    #[test]
    fn runs_whose_results_are_not_applied_count_towards_the_spend_cap() {
        let mut app = build_minimal_test_app();
        let provider = Arc::new(OfferedToolsRecorder {
            cost_usd: Some(0.004),
            ..Default::default()
        });
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
        assert_eq!(provider.offered.lock().unwrap().len(), 3);

        // The refusal keeps its note when applied, and counts nothing.
        app.apply_agent_result(fourth);
        assert!(app
            .cost_guard_note()
            .is_some_and(|note| note.contains("before this run")));
        assert!((app.session_cost_usd - 0.012).abs() < 1e-9, "{}", app.session_cost_usd);
    }
