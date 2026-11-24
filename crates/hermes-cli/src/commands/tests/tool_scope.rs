    // The agent reaches only the tools the session plans for the model:
    // `platform_toolsets` and `tools_config.disabled` hold for every call made
    // by name, including a sub-agent's, and follow `/tools` and `/reload`.

    const SCOPE_CHILD_TASK: &str = "scope-probe child task";

    struct ScopeProbe {
        name: String,
        calls: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl hermes_core::ToolHandler for ScopeProbe {
        async fn execute(&self, _params: serde_json::Value) -> Result<String, hermes_core::ToolError> {
            self.calls.lock().unwrap().push(self.name.clone());
            Ok(format!("{} ran", self.name))
        }

        fn schema(&self) -> hermes_core::ToolSchema {
            hermes_core::tool_schema(&self.name, "probe", hermes_core::JsonSchema::new("object"))
        }
    }

    /// Registers `probe_visible`, `probe_disabled` and `probe_outside` and
    /// plans the CLI for `delegate_task`, `probe_visible` and `probe_disabled`,
    /// with `probe_disabled` disabled; `probe_outside` is in no planned toolset.
    fn scope_probe_session(app: &mut App) -> Arc<std::sync::Mutex<Vec<String>>> {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        for name in ["probe_visible", "probe_disabled", "probe_outside"] {
            let handler = Arc::new(ScopeProbe {
                name: name.to_string(),
                calls: calls.clone(),
            });
            app.tool_registry.register(
                name,
                "scope-probe",
                hermes_core::ToolHandler::schema(handler.as_ref()),
                handler,
                Arc::new(|| true),
                Vec::new(),
                true,
                "probe",
                "x",
                None,
            );
        }
        let config = Arc::make_mut(&mut app.config);
        config.platform_toolsets.insert(
            "cli".to_string(),
            vec![
                "delegate_task".to_string(),
                "probe_visible".to_string(),
                "probe_disabled".to_string(),
            ],
        );
        config.tools_config.disabled = vec!["probe_disabled".to_string()];
        app.refresh_agent_tool_snapshot();
        calls
    }

    fn schema_names(schemas: &[hermes_core::ToolSchema]) -> std::collections::BTreeSet<String> {
        schemas.iter().map(|schema| schema.name.clone()).collect()
    }

    fn agent_tool_names(app: &App) -> std::collections::BTreeSet<String> {
        app.agent.tool_registry.names().into_iter().collect()
    }

    fn scope_tool_call(id: &str, name: &str, args: serde_json::Value) -> hermes_core::ToolCall {
        hermes_core::ToolCall {
            id: id.to_string(),
            function: hermes_core::FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
            extra_content: None,
        }
    }

    /// Plays the model: the parent makes `parent_calls`; a sub-agent (given
    /// [`SCOPE_CHILD_TASK`]) calls both hidden probes by name; once a turn has
    /// tool results, it answers.
    struct ScopeModel {
        parent_calls: Vec<hermes_core::ToolCall>,
    }

    #[async_trait::async_trait]
    impl hermes_core::LlmProvider for ScopeModel {
        async fn chat_completion(
            &self,
            messages: &[hermes_core::Message],
            _tools: &[hermes_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let answered = messages
                .last()
                .is_some_and(|m| m.role == hermes_core::MessageRole::Tool);
            let child = messages.iter().any(|m| {
                m.role == hermes_core::MessageRole::User
                    && m.content.as_deref().is_some_and(|c| c.contains(SCOPE_CHILD_TASK))
            });
            let (message, finish) = if answered {
                (hermes_core::Message::assistant("done"), "stop")
            } else if child {
                let calls = vec![
                    scope_tool_call("c1", "probe_disabled", serde_json::json!({})),
                    scope_tool_call("c2", "probe_outside", serde_json::json!({})),
                ];
                (hermes_core::Message::assistant_with_tool_calls(None, calls), "tool_calls")
            } else {
                (
                    hermes_core::Message::assistant_with_tool_calls(None, self.parent_calls.clone()),
                    "tool_calls",
                )
            };
            Ok(hermes_core::LlmResponse {
                message,
                usage: None,
                model: "scope-model".into(),
                finish_reason: Some(finish.into()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[hermes_core::Message],
            _tools: &[hermes_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
            use futures::StreamExt;
            futures::stream::empty().boxed()
        }
    }

    /// Runs one turn on the session's own agent tools (as the CLI built them)
    /// and sub-agent orchestrator, with [`ScopeModel`] as the model.
    async fn run_scope_turn(app: &App, parent_calls: Vec<hermes_core::ToolCall>) {
        let mut config = crate::app::build_agent_config(&app.config, "dynamic");
        config.skip_memory = true;
        config.skip_context_files = true;
        config.background_review_enabled = false;
        config.max_turns = 4;
        config.retry.max_retries = 0;
        config.hermes_home = Some(app.state_root.to_string_lossy().into_owned());
        let agent = hermes_agent::AgentLoop::new(
            config,
            app.agent.tool_registry.clone(),
            Arc::new(ScopeModel { parent_calls }),
        );
        let orchestrator = Arc::new(hermes_agent::sub_agent_orchestrator::SubAgentOrchestrator::from_parent(
            &agent,
            app.state_root.clone(),
        ));
        let agent = agent.with_sub_agent_orchestrator(orchestrator);
        agent
            .run(
                vec![hermes_core::Message::user("scope-probe parent")],
                Some(app.tool_schemas.clone()),
            )
            .await
            .expect("turn runs");
    }

    #[tokio::test]
    async fn the_agent_holds_only_the_tools_planned_for_the_model() {
        let _guard = test_env_lock::lock_async().await;
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        assert_eq!(agent_tool_names(&app), schema_names(&app.tool_schemas));

        scope_probe_session(&mut app);

        let planned = schema_names(&app.tool_schemas);
        assert!(planned.contains("probe_visible"), "{planned:?}");
        assert!(!planned.contains("probe_disabled"), "{planned:?}");
        assert!(!planned.contains("probe_outside"), "{planned:?}");
        assert_eq!(agent_tool_names(&app), planned);
    }

    #[tokio::test]
    async fn a_sub_agent_given_its_own_toolset_cannot_call_a_hidden_tool() {
        let _guard = test_env_lock::lock_async().await;
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        let calls = scope_probe_session(&mut app);

        let delegate = scope_tool_call(
            "p1",
            "delegate_task",
            serde_json::json!({"task": SCOPE_CHILD_TASK, "toolset": "scope-probe"}),
        );
        run_scope_turn(&app, vec![delegate]).await;

        assert_eq!(*calls.lock().unwrap(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_hidden_tool_called_by_name_does_not_run() {
        let _guard = test_env_lock::lock_async().await;
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        let calls = scope_probe_session(&mut app);

        run_scope_turn(
            &app,
            vec![
                scope_tool_call("p1", "probe_disabled", serde_json::json!({})),
                scope_tool_call("p2", "probe_outside", serde_json::json!({})),
            ],
        )
        .await;

        assert_eq!(*calls.lock().unwrap(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn tools_disable_and_enable_change_what_the_agent_can_call() {
        let _guard = test_env_lock::lock_async().await;
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        scope_probe_session(&mut app);
        assert!(app.agent.tool_registry.get("probe_visible").is_some());

        handle_slash_command(&mut app, "/tools", &["disable", "probe_visible"])
            .await
            .expect("disable");
        assert!(!schema_names(&app.tool_schemas).contains("probe_visible"));
        assert_eq!(agent_tool_names(&app), schema_names(&app.tool_schemas));

        handle_slash_command(&mut app, "/tools", &["enable", "probe_visible"])
            .await
            .expect("enable");
        assert!(schema_names(&app.tool_schemas).contains("probe_visible"));
        assert_eq!(agent_tool_names(&app), schema_names(&app.tool_schemas));
    }

    #[tokio::test]
    async fn reload_applies_a_new_tool_config_to_the_agent() {
        let _guard = test_env_lock::lock_async().await;
        let tmp = tempdir().expect("tempdir");
        let _home_guard = TempHomeGuard::new(tmp.path());
        let mut app = build_test_app_with_stream(tmp.path()).await;
        scope_probe_session(&mut app);
        assert!(app.agent.tool_registry.get("probe_visible").is_some());
        std::fs::write(
            app.state_root.join("config.yaml"),
            "platform_toolsets:\n  cli: [delegate_task, probe_visible]\ntools_config:\n  disabled: [probe_visible]\n",
        )
        .expect("write config");

        handle_slash_command(&mut app, "/reload", &[]).await.expect("reload");

        assert!(!schema_names(&app.tool_schemas).contains("probe_visible"));
        assert_eq!(agent_tool_names(&app), schema_names(&app.tool_schemas));
    }
