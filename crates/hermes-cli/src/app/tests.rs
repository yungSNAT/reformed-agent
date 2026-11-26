#[cfg(test)]
mod tests {
    use super::*;
    use crate::alpha_runtime::{
        load_quorum_policy, set_objective_contract_behavior_mode, set_quorum_policy,
        upsert_objective_contract,
    };
    use crate::test_env_lock;
    use hermes_agent::plugins::{HookResult, Plugin, PluginContext, PluginManager};
    use hermes_config::LlmProviderConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn env_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
        test_env_lock::lock()
    }

    fn block_on_test<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    struct EnvSnapshot {
        vars: Vec<(&'static str, Option<String>)>,
    }

    impl EnvSnapshot {
        fn capture(keys: &[&'static str]) -> Self {
            Self {
                vars: keys
                    .iter()
                    .map(|key| (*key, std::env::var(key).ok()))
                    .collect(),
            }
        }
    }

    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            for (key, value) in &self.vars {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    struct TestToolHandler {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl hermes_core::ToolHandler for TestToolHandler {
        async fn execute(&self, _params: Value) -> Result<String, hermes_core::ToolError> {
            Ok(format!("{} ok", self.name))
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                self.name,
                "test tool",
                hermes_core::JsonSchema::new("object"),
            )
        }
    }

    fn register_test_tool_in_toolset(
        tools: &ToolRegistry,
        name: &'static str,
        toolset: &'static str,
    ) {
        let handler: Arc<dyn hermes_core::ToolHandler> = Arc::new(TestToolHandler { name });
        tools.register(
            name,
            toolset,
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "test tool",
            "T",
            None,
        );
    }

    fn register_test_tool(tools: &ToolRegistry, name: &'static str) {
        register_test_tool_in_toolset(tools, name, "test");
    }

    fn build_minimal_test_app() -> App {
        build_minimal_test_app_with_state_root(hermes_home_dir())
    }

    fn build_minimal_test_app_with_state_root(state_root: PathBuf) -> App {
        build_minimal_test_app_with_memory(state_root, true)
    }

    fn build_minimal_test_app_with_memory(state_root: PathBuf, memory_enabled: bool) -> App {
        let config = Arc::new(GatewayConfig::default());
        let tool_registry = Arc::new(ToolRegistry::new());
        let cron_dir = state_root.join("cron");
        std::fs::create_dir_all(&cron_dir).expect("create cron test dir");
        let cron_scheduler = Arc::new(build_runtime_cron_scheduler(
            config.as_ref(),
            "dynamic",
            cron_dir,
            &tool_registry,
        ));
        let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
        let session_id = "test-session".to_string();
        let mut agent_config = build_agent_config(config.as_ref(), "dynamic");
        agent_config.session_id = Some(session_id.clone());
        agent_config.skip_memory |= !memory_enabled;
        let provider: Arc<dyn LlmProvider> = Arc::new(hermes_provider_runtime::NoBackendProvider {
            model: "dynamic".to_string(),
        });
        let agent_inner = hermes_agent::attach_discovered_memory(AgentLoop::new(
            agent_config,
            agent_tool_registry,
            provider,
        ))
        .with_callbacks(App::stream_callbacks(Arc::new(StdMutex::new(None))));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            state_root.clone(),
        ));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        App {
            state_root,
            config,
            agent,
            tool_registry,
            cron_scheduler,
            tool_schemas: Vec::new(),
            messages: Vec::new(),
            ui_messages: Vec::new(),
            session_id,
            running: true,
            current_model: "dynamic".to_string(),
            last_usage: None,
            session_usage: None,
            session_cost_usd: 0.0,
            current_personality: None,
            input_history: Vec::new(),
            history_index: 0,
            interrupt_controller: InterruptController::new(),
            stream_handle: None,
            stream_handle_shared: Arc::new(StdMutex::new(None)),
            mouse_enabled: true,
            pending_theme: None,
            pending_image_hint: None,
            session_objective: None,
            pending_input_prefill: None,
            pending_agent_seed: None,
            pending_system_notes: Vec::new(),
            quorum_armed_once: false,
            pet_settings: PetSettings::default(),
            tool_snapshot_sync_error: None,
            fail_model_rebuild_for: None,
        }
    }

    #[test]
    fn app_debug_keeps_conversation_content_out_of_diagnostics() {
        if !crate::isolated_test_profile::enter(
            "app::tests::app_debug_keeps_conversation_content_out_of_diagnostics",
        ) {
            return;
        }
        let mut app = build_minimal_test_app_with_state_root(hermes_home_dir());
        app.messages
            .push(hermes_core::Message::user("raw conversation fixture"));
        app.input_history.push("raw editable input fixture".into());
        app.current_model = "debug-fixture-model".into();
        app.running = false;

        let report = format!("{app:?}");
        assert!(report.starts_with("App {"), "{report}");
        assert!(report.contains(r#"session_id: "test-session""#), "{report}");
        assert!(
            report.contains(r#"current_model: "debug-fixture-model""#),
            "{report}"
        );
        assert!(report.contains("running: false"), "{report}");
        assert!(!report.contains("raw conversation fixture"), "{report}");
        assert!(!report.contains("raw editable input fixture"), "{report}");
    }

    #[test]
    fn refresh_agent_tool_snapshot_adds_late_registered_tool() {
        let mut app = build_minimal_test_app();

        assert!(app.agent.tool_registry.get("mcp_srv_late").is_none());
        assert!(!app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_late"));

        register_test_tool(&app.tool_registry, "mcp_srv_late");
        let refresh = app.refresh_agent_tool_snapshot();

        assert_eq!(refresh.before_count, 0);
        assert_eq!(refresh.after_count, 1);
        assert_eq!(refresh.added, vec!["mcp_srv_late".to_string()]);
        assert!(refresh.removed.is_empty());
        assert!(app.agent.tool_registry.get("mcp_srv_late").is_some());
        assert!(app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_late"));
        assert_eq!(
            app.agent.config.session_id.as_deref(),
            Some(app.session_id.as_str())
        );
    }

    #[test]
    fn refresh_agent_tool_snapshot_detects_equal_size_replacement() {
        let mut app = build_minimal_test_app();
        register_test_tool(&app.tool_registry, "mcp_srv_old");
        let first_refresh = app.refresh_agent_tool_snapshot();
        assert_eq!(first_refresh.added, vec!["mcp_srv_old".to_string()]);
        assert!(app.agent.tool_registry.get("mcp_srv_old").is_some());

        assert!(app.tool_registry.deregister("mcp_srv_old"));
        register_test_tool(&app.tool_registry, "mcp_srv_new");
        let second_refresh = app.refresh_agent_tool_snapshot();

        assert_eq!(second_refresh.before_count, 1);
        assert_eq!(second_refresh.after_count, 1);
        assert_eq!(second_refresh.added, vec!["mcp_srv_new".to_string()]);
        assert_eq!(second_refresh.removed, vec!["mcp_srv_old".to_string()]);
        assert!(app.agent.tool_registry.get("mcp_srv_old").is_none());
        assert!(app.agent.tool_registry.get("mcp_srv_new").is_some());
        assert!(app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_new"));
        assert!(!app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_old"));
    }

    #[test]
    fn refresh_agent_tool_snapshot_reports_no_changes_when_current() {
        let mut app = build_minimal_test_app();
        let refresh = app.refresh_agent_tool_snapshot();

        assert_eq!(refresh.before_count, 0);
        assert_eq!(refresh.after_count, 0);
        assert!(!refresh.changed());
    }

    #[test]
    fn refresh_agent_tool_snapshot_reports_advertised_surface_only() {
        let mut app = build_minimal_test_app();
        Arc::make_mut(&mut app.config)
            .platform_toolsets
            .insert("cli".to_string(), vec!["test".to_string()]);
        register_test_tool(&app.tool_registry, "allowed_tool");
        register_test_tool_in_toolset(&app.tool_registry, "mcp_srv_hidden", "mcp-srv");

        let refresh = app.refresh_agent_tool_snapshot();

        assert_eq!(refresh.before_count, 0);
        assert_eq!(refresh.after_count, 1);
        assert_eq!(refresh.added, vec!["allowed_tool".to_string()]);
        assert!(refresh.removed.is_empty());
        assert!(app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "allowed_tool"));
        assert!(!app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_hidden"));
    }

    #[test]
    fn test_switch_model_updates_existing_session_db_row() {
        let _guard = env_test_lock();
        let env_keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let saved_env: Vec<(&str, Option<String>)> = env_keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();

        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        let persistence = SessionPersistence::new(tmp.path());
        persistence
            .persist_session(
                &app.session_id,
                &[hermes_core::Message::user("hello")],
                Some(&app.current_model),
                Some("cli"),
                None,
                None,
            )
            .unwrap();

        app.switch_model("anthropic:claude-sonnet-4-6");

        assert_eq!(
            persistence.get_session_model(&app.session_id).unwrap(),
            Some("anthropic:claude-sonnet-4-6".to_string())
        );
        assert_eq!(
            app.agent.config.session_id.as_deref(),
            Some(app.session_id.as_str())
        );

        for (key, value) in saved_env {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn try_switch_model_failure_is_noop_for_session_state() {
        let _guard = env_test_lock();
        let env_keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let saved_env: Vec<(&str, Option<String>)> = env_keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();

        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        let persistence = SessionPersistence::new(tmp.path());
        persistence
            .persist_session(
                &app.session_id,
                &[hermes_core::Message::user("hello")],
                Some(&app.current_model),
                Some("cli"),
                None,
                None,
            )
            .unwrap();

        let old_model = "anthropic:claude-sonnet-4-6";
        app.try_switch_model(old_model).expect("baseline switch");
        let old_agent_model = app.agent.config.model.clone();
        assert_eq!(
            persistence.get_session_model(&app.session_id).unwrap(),
            Some(old_model.to_string())
        );
        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some(old_model)
        );

        let broken_model = "openrouter:zai/glm-5.2";
        app.force_model_rebuild_failure_for_test(broken_model);
        let err = app
            .try_switch_model(broken_model)
            .expect_err("forced rebuild failure");

        assert!(err.to_string().contains("test forced rebuild failure"));
        assert_eq!(app.current_model, old_model);
        assert_eq!(app.agent.config.model, old_agent_model);
        assert_eq!(
            persistence.get_session_model(&app.session_id).unwrap(),
            Some(old_model.to_string())
        );
        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some(old_model)
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_MODEL").ok().as_deref(),
            Some(old_model)
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("anthropic")
        );

        for (key, value) in saved_env {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn moa_virtual_model_switch_updates_session_without_rebuilding_agent() {
        let _guard = env_test_lock();
        let env_keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let saved_env: Vec<(&str, Option<String>)> = env_keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();

        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        let original_agent_model = app.agent.config.model.clone();
        let persistence = SessionPersistence::new(tmp.path());
        persistence
            .persist_session(
                &app.session_id,
                &[hermes_core::Message::user("hello")],
                Some(&app.current_model),
                Some("cli"),
                None,
                None,
            )
            .unwrap();

        app.try_switch_model("mixture-of-agents:default")
            .expect("virtual moa switch");

        assert_eq!(app.current_model, "moa:default");
        assert_eq!(
            app.agent.config.model, original_agent_model,
            "virtual model selection should not rebuild the current concrete agent"
        );
        assert_eq!(
            persistence.get_session_model(&app.session_id).unwrap(),
            Some("moa:default".to_string())
        );
        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some("moa:default")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("moa")
        );

        let err = app
            .try_switch_model("moa:unknown")
            .expect_err("unsupported preset should fail closed");
        assert!(err.to_string().contains("unsupported MoA preset"));
        assert_eq!(app.current_model, "moa:default");

        for (key, value) in saved_env {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[tokio::test]
    async fn model_remediation_uses_the_offline_catalogue_for_not_found_errors() {
        if !crate::isolated_test_profile::enter(
            "app::tests::model_remediation_uses_the_offline_catalogue_for_not_found_errors",
        ) {
            return;
        }
        let mut app = build_minimal_test_app_with_state_root(hermes_home_dir());
        let rejected_model = format!("{MOA_PROVIDER}:retired-fixture");
        let replacement_model = format!("{MOA_PROVIDER}:{MOA_DEFAULT_PRESET}");
        app.current_model = rejected_model.clone();
        let unavailable = AgentError::LlmApi("Requested model does not exist".into());

        let (target, notice) = app.model_auto_remediation_target(&unavailable).await.unwrap();
        assert_eq!(target, replacement_model);
        assert!(notice.contains(&rejected_model));
        assert!(notice.contains(&replacement_model));
        assert_eq!(app.current_model, rejected_model);
        assert!(app
            .model_auto_remediation_target(&AgentError::LlmApi("Request timed out".into()))
            .await
            .is_none());

        for current in [replacement_model.as_str(), ":fixture", "unknown-fixture:retired"] {
            app.current_model = current.to_string();
            assert!(app.model_auto_remediation_target(&unavailable).await.is_none());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auth_refresh_reports_missing_state_for_the_active_provider() {
        if !crate::isolated_test_profile::enter(
            "app::tests::auth_refresh_reports_missing_state_for_the_active_provider",
        ) {
            return;
        }

        let profile = tempfile::tempdir().unwrap();
        let home = profile.path();
        std::env::set_var("HOME", home);
        std::env::set_var("HERMES_HOME", home);
        std::env::set_var("HERMES_AUTH_FILE", home.join("missing-auth-store.json"));
        for (key, filename) in [
            ("HERMES_NOUS_OAUTH_FILE", "missing-nous.json"),
            ("HERMES_QWEN_CLI_AUTH_FILE", "missing-qwen.json"),
            ("HERMES_GEMINI_OAUTH_FILE", "missing-gemini.json"),
        ] {
            let path = home.join(filename);
            assert!(!path.exists());
            std::env::set_var(key, path);
        }
        std::env::set_var("HERMES_AUTO_NOUS_REAUTH", "off");

        let mut app = build_minimal_test_app_with_memory(home.to_path_buf(), false);
        let model_messages = app.messages.len();
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        app.set_stream_handle(Some(StreamHandle::from(tx)));
        for (model, notice_prefix, missing_state) in [
            (
                "nous:fixture",
                "Nous auth auto-refresh failed:",
                "not logged into Nous Portal",
            ),
            (
                "qwen-cli:fixture",
                "Qwen OAuth auto-refresh failed:",
                "Qwen CLI credentials not found",
            ),
            (
                "gemini-oauth:fixture",
                "Gemini OAuth auto-refresh failed:",
                "Google OAuth credentials not found",
            ),
        ] {
            app.current_model = model.to_string();
            assert!(!app.force_auth_refresh_after_error().await);
            let notice = app
                .ui_messages
                .last()
                .unwrap()
                .message
                .content
                .as_deref()
                .unwrap();
            assert!(
                notice.starts_with(notice_prefix) && notice.contains(missing_state),
                "{notice}"
            );
            let crate::tui::Event::StreamChunk(chunk) = events.try_recv().unwrap() else {
                panic!("expected a lifecycle stream chunk for {model}");
            };
            let event = chunk.delta.unwrap().extra.unwrap();
            assert_eq!(event["ui_event"], "lifecycle");
            assert_eq!(event["message"], notice);
            assert!(events.try_recv().is_err());
            assert_eq!(app.current_model, model);
        }

        app.current_model = "openai:fixture".into();
        assert!(!app.force_auth_refresh_after_error().await);
        assert!(events.try_recv().is_err());
        assert_eq!(app.ui_messages.len(), 3);
        assert_eq!(app.messages.len(), model_messages);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn contextlattice_preflight_reports_the_loopback_service_state() {
        const TEST: &str =
            "app::tests::contextlattice_preflight_reports_the_loopback_service_state";
        if !crate::isolated_test_profile::enter(TEST) {
            return;
        }
        let server = MockServer::start().await;
        let mut app = build_minimal_test_app_with_state_root(hermes_home_dir());
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        app.set_stream_handle(Some(StreamHandle::from(tx)));
        let mut next_event = || {
            let crate::tui::Event::StreamChunk(chunk) = events.try_recv().expect("preflight event")
            else {
                panic!("expected a preflight stream chunk");
            };
            chunk.delta.unwrap().extra.unwrap()
        };

        std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", "  tests/red-queen  ");
        std::env::set_var("HERMES_CONTEXTLATTICE_PING_TIMEOUT_SECONDS", "5");
        for (prefix, response, result, phase) in [
            (
                "healthy",
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "service": "fixture", "ok": true
                })),
                "contextlattice connected (service=fixture status=200 OK",
                Some("contextlattice connected"),
            ),
            (
                "degraded",
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "service": "fixture", "ok": false
                })),
                "contextlattice degraded (service=fixture status=200 OK",
                Some("contextlattice degraded"),
            ),
            (
                "invalid",
                ResponseTemplate::new(200).set_body_string("status unavailable"),
                "contextlattice connected (service=unknown status=200 OK",
                Some("contextlattice connected"),
            ),
            (
                "unavailable",
                ResponseTemplate::new(503),
                "contextlattice status endpoint returned 503 Service Unavailable",
                None,
            ),
        ] {
            let base = format!("{}/{prefix}", server.uri());
            let resource = format!("/{prefix}/status");
            Mock::given(method("GET"))
                .and(path(resource.as_str()))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            std::env::set_var("CONTEXTLATTICE_ORCHESTRATOR_URL", format!(" {base}/ "));
            app.emit_contextlattice_connectivity_status().await;

            assert_eq!(
                next_event(),
                serde_json::json!({
                    "ui_event": "lifecycle",
                    "message": format!("contextlattice preflight ping {base} (topic=tests/red-queen)"),
                })
            );
            let outcome = next_event();
            assert_eq!(outcome["ui_event"], "lifecycle");
            let message = outcome["message"].as_str().unwrap();
            assert!(message.starts_with(result), "{message}");
            assert!(message.contains(&base), "{message}");
            if let Some(label) = phase {
                assert_eq!(
                    next_event(),
                    serde_json::json!({
                        "ui_event": "phase", "phase": "context", "label": label, "progress_pct": 12,
                    })
                );
            }
        }
        assert!(events.try_recv().is_err());

        std::env::set_var("HERMES_CONTEXTLATTICE_UI_STATUS", "off");
        app.emit_contextlattice_connectivity_status().await;
        assert!(events.try_recv().is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 4);
    }

    #[test]
    fn model_switch_preflight_warning_is_ui_only_and_keeps_messages_clean() {
        let mut app = build_minimal_test_app();
        app.current_model = "anthropic:claude-sonnet-4-6".to_string();
        app.messages = vec![hermes_core::Message::user("abcd".repeat(90_000))];

        let warning = app
            .model_switch_preflight_warning("deepseek-chat")
            .expect("large transcript should warn");

        assert!(warning.contains("Context warning"));
        assert!(warning.contains("preflight compression"));
        assert_eq!(
            app.messages.len(),
            1,
            "warning must not mutate model context"
        );
        assert!(
            app.ui_messages.is_empty(),
            "warning calculation must not add UI transcript rows"
        );
    }

    #[test]
    fn input_history_prev_next_restores_empty_draft_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        app.input_history = vec![
            "first prompt".to_string(),
            "second prompt".to_string(),
            "third prompt".to_string(),
        ];
        app.history_index = app.input_history.len();

        assert_eq!(app.history_prev(), Some("third prompt"));
        assert_eq!(app.history_prev(), Some("second prompt"));
        assert_eq!(app.history_prev(), Some("first prompt"));
        assert_eq!(app.history_prev(), None);

        assert_eq!(app.history_next(), Some("second prompt"));
        assert_eq!(app.history_next(), Some("third prompt"));
        assert_eq!(app.history_next(), None);
        assert_eq!(app.history_index, app.input_history.len());
        assert_eq!(app.history_next(), None);
    }

    #[test]
    fn composer_drafts_persist_per_session_without_touching_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        app.session_id = "session-a".to_string();

        app.persist_current_composer_draft("alpha draft").unwrap();
        app.session_id = "session-b".to_string();
        app.persist_current_composer_draft("beta draft").unwrap();

        assert_eq!(
            app.load_current_composer_draft().unwrap().as_deref(),
            Some("beta draft")
        );
        app.session_id = "session-a".to_string();
        assert_eq!(
            app.load_current_composer_draft().unwrap().as_deref(),
            Some("alpha draft")
        );

        app.clear_current_composer_draft().unwrap();
        assert_eq!(app.load_current_composer_draft().unwrap(), None);
        app.session_id = "session-b".to_string();
        assert_eq!(
            app.load_current_composer_draft().unwrap().as_deref(),
            Some("beta draft")
        );

        let persistence = SessionPersistence::new(tmp.path());
        assert!(persistence.load_session("session-a").unwrap().is_empty());
        assert!(persistence.load_session("session-b").unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn composer_drafts_write_through_a_symlink_and_keep_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().join("state"));
        let link = app.composer_drafts_path();
        let real = tmp.path().join("dotfiles").join("drafts.json");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        app.session_id = "session-a".to_string();

        app.persist_current_composer_draft("alpha draft").unwrap();

        assert!(link.is_symlink(), "the drafts link was replaced");
        assert!(std::fs::read_to_string(&real).unwrap().contains("alpha draft"));
    }

    #[test]
    fn composer_drafts_keep_mru_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());

        for idx in 0..(MAX_COMPOSER_DRAFTS + 3) {
            app.session_id = format!("session-{idx}");
            app.persist_current_composer_draft(&format!("draft-{idx}"))
                .unwrap();
        }

        let raw = std::fs::read_to_string(app.composer_drafts_path()).unwrap();
        let store: ComposerDraftStore = serde_json::from_str(&raw).unwrap();
        assert_eq!(store.drafts.len(), MAX_COMPOSER_DRAFTS);
        assert_eq!(store.drafts.first().unwrap().session_id, "session-3");
        assert_eq!(
            store.drafts.last().unwrap().session_id,
            format!("session-{}", MAX_COMPOSER_DRAFTS + 2)
        );
    }

    #[test]
    fn test_undo_last_n_soft_rewinds_and_sets_prefill() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        app.messages = vec![
            hermes_core::Message::system("sys"),
            hermes_core::Message::user("question 1"),
            hermes_core::Message::assistant("answer 1"),
            hermes_core::Message::user("question 2"),
            hermes_core::Message::assistant("answer 2"),
            hermes_core::Message::user("question 3"),
            hermes_core::Message::assistant("answer 3"),
        ];
        let persistence = SessionPersistence::new(tmp.path());
        persistence
            .persist_session(
                &app.session_id,
                &app.messages,
                None,
                Some("cli"),
                None,
                None,
            )
            .unwrap();

        let prefill = app.undo_last_n(2).expect("undo");

        assert_eq!(prefill, "question 2");
        assert_eq!(
            app.take_pending_input_prefill().as_deref(),
            Some("question 2")
        );
        assert_eq!(
            app.messages
                .iter()
                .filter_map(|m| m.content.as_deref())
                .collect::<Vec<_>>(),
            vec!["sys", "question 1", "answer 1"]
        );
        assert_eq!(persistence.load_session(&app.session_id).unwrap().len(), 3);
        let recent = persistence
            .list_recent_user_messages(&app.session_id, 5)
            .unwrap();
        assert_eq!(
            recent
                .iter()
                .filter_map(|row| row.content.as_deref())
                .collect::<Vec<_>>(),
            vec!["question 1"]
        );
    }

    struct LifecycleHookPlugin {
        seen: Arc<StdMutex<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl Plugin for LifecycleHookPlugin {
        fn meta(&self) -> hermes_agent::plugins::PluginMeta {
            hermes_agent::plugins::PluginMeta {
                name: "lifecycle-recorder".to_string(),
                version: "0.1.0".to_string(),
                description: "Lifecycle recorder".to_string(),
                author: None,
            }
        }

        async fn initialize(&self) -> Result<(), AgentError> {
            Ok(())
        }

        async fn shutdown(&self) -> Result<(), AgentError> {
            Ok(())
        }

        fn register(&self, ctx: &mut PluginContext) {
            let finalize_seen = self.seen.clone();
            ctx.on(
                HookType::OnSessionFinalize,
                Arc::new(move |value| {
                    let session_id = value
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    finalize_seen
                        .lock()
                        .unwrap()
                        .push(("on_session_finalize".to_string(), session_id));
                    HookResult::Ok
                }),
            );
            let reset_seen = self.seen.clone();
            ctx.on(
                HookType::OnSessionReset,
                Arc::new(move |value| {
                    let session_id = value
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    reset_seen
                        .lock()
                        .unwrap()
                        .push(("on_session_reset".to_string(), session_id));
                    HookResult::Ok
                }),
            );
            let end_seen = self.seen.clone();
            ctx.on(
                HookType::OnSessionEnd,
                Arc::new(move |value| {
                    let session_id = value
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let interrupted = value
                        .get("interrupted")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let reason = value
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    end_seen.lock().unwrap().push((
                        format!("on_session_end:interrupted={interrupted}:reason={reason}"),
                        session_id,
                    ));
                    HookResult::Ok
                }),
            );
        }
    }

    fn attach_lifecycle_recorder(app: &mut App, seen: Arc<StdMutex<Vec<(String, String)>>>) {
        let mut plugin_manager = PluginManager::new();
        plugin_manager.register(Arc::new(LifecycleHookPlugin { seen }));
        let agent = AgentLoop::new(
            app.agent.config.clone(),
            app.agent.tool_registry.clone(),
            app.agent.llm_provider.clone(),
        )
        .with_plugins(Arc::new(StdMutex::new(plugin_manager)));
        app.agent = Arc::new(agent);
    }

    #[test]
    fn app_new_session_invokes_session_lifecycle_hooks() {
        let mut app = build_minimal_test_app();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        attach_lifecycle_recorder(&mut app, seen.clone());
        let old_session_id = app.session_id.clone();

        app.new_session();

        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            ("on_session_finalize".to_string(), old_session_id)
        );
        assert_eq!(events[1].0, "on_session_reset");
        assert_eq!(events[1].1, app.session_id);
        assert_ne!(events[0].1, events[1].1);
        assert_eq!(
            app.agent.config.session_id.as_deref(),
            Some(app.session_id.as_str())
        );
    }

    #[test]
    fn app_reset_session_invokes_session_lifecycle_hooks() {
        let mut app = build_minimal_test_app();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        attach_lifecycle_recorder(&mut app, seen.clone());
        let session_id = app.session_id.clone();

        app.reset_session();

        let events = seen.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![
                ("on_session_finalize".to_string(), session_id.clone()),
                ("on_session_reset".to_string(), session_id)
            ]
        );
        assert_eq!(app.session_id, "test-session");
        assert_eq!(app.agent.config.session_id.as_deref(), Some("test-session"));
    }

    #[tokio::test]
    async fn tui_charge_renders_the_key_before_submission() {
        let _guard = test_env_lock::lock_async().await;
        let _env = EnvSnapshot::capture(&[
            "HERMES_HOME",
            "HOME",
            "HERMES_AUTH_FILE",
            "HERMES_NOUS_OAUTH_FILE",
            "HERMES_PORTAL_BASE_URL",
            "NO_PROXY",
        ]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let server = MockServer::start().await;
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HOME", tmp.path());
        std::env::set_var("HERMES_AUTH_FILE", tmp.path().join("auth.json"));
        std::env::set_var("HERMES_NOUS_OAUTH_FILE", tmp.path().join("nous_oauth.json"));
        std::env::set_var("HERMES_PORTAL_BASE_URL", server.uri());
        std::env::set_var("NO_PROXY", "127.0.0.1");
        let state = serde_json::from_value(serde_json::json!({
            "portal_base_url": server.uri(),
            "inference_base_url": server.uri(),
            "client_id": "billing-fixture",
            "token_type": "Bearer",
            "access_token": "fixture-access",
            "obtained_at": "2026-06-25T00:00:00Z",
        }))
        .expect("build fixture auth state");
        assert!(
            crate::auth::save_nous_auth_state(&state).is_ok(),
            "save fixture auth state"
        );
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        for (line, expected) in [
            (
                r#"/billing charge 50 --confirm --idempotency-key="fixture key""#,
                "Unknown or repeated charge option",
            ),
            (
                r#"/billing charge 50 --confirm --idempotency-key="fixture-key""#,
                "without whitespace, quotes or backslashes",
            ),
            (
                "/billing charge 50 --confirm --idempotency-key=first --idempotency-key=second",
                "Unknown or repeated charge option",
            ),
            (
                "/billing charge 50 --yes --confirm",
                "Unknown or repeated charge option",
            ),
        ] {
            assert!(crate::billing::prepare_tui_charge_key(line).is_none());
            assert!(
                app.handle_input_with_charge_key(line, None)
                    .await
                    .is_err_and(|error| error.to_string().contains(expected)),
                "ambiguous raw TUI charge input should be rejected"
            );
        }
        assert!(
            server
                .received_requests()
                .await
                .is_some_and(|requests| requests.is_empty()),
            "rejected TUI charge inputs must not send any requests"
        );

        let command = "/billing charge 50 --confirm";
        let mut state = crate::tui::TuiState::default();
        let theme = crate::skin_engine::resolve_theme("reformation");
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30))
            .expect("create test terminal");
        let key = crate::tui::draw_slash_command_frame(&mut app, &mut state, command, |app, state| {
            terminal
                .draw(|frame| crate::tui::render(frame, app, state, &theme))
                .expect("draw charge frame");
            Ok(())
        })
        .expect("prepare charge frame")
        .expect("generated billing key");
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            screen.contains(&format!("Billing charge idempotency key: {key}"))
                && server
                    .received_requests()
                    .await
                    .is_some_and(|requests| requests.is_empty()),
            "the first rendered frame should display the key before any billing request"
        );

        Mock::given(method("POST"))
            .and(path("/api/billing/charge"))
            .and(wiremock::matchers::header("idempotency-key", key.as_str()))
            .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                "chargeId": "fixture-charge"
            })))
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            app.handle_input_with_charge_key(command, Some(&key))
                .await
                .is_ok(),
            "submit with the key already displayed"
        );
        assert!(
            server
                .received_requests()
                .await
                .is_some_and(|requests| requests.len() == 1),
            "exactly one request should reach the fixture"
        );
        assert!(
            app.input_history.last().is_some_and(|line| line == command)
                && !app.input_history.iter().any(|line| line.contains(&key)),
            "input history should keep the original command without the generated key"
        );
    }

    #[test]
    fn finalize_interrupted_tui_session_invokes_session_end_hook() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "hooked-force-quit".to_string();
        app.messages = vec![hermes_core::Message::user("still save this")];
        let seen = Arc::new(StdMutex::new(Vec::new()));
        attach_lifecycle_recorder(&mut app, seen.clone());

        app.finalize_interrupted_tui_session(None, "shutdown_signal")
            .expect("finalize interrupted session");

        assert_eq!(
            seen.lock().unwrap().clone(),
            vec![(
                "on_session_end:interrupted=true:reason=shutdown_signal".to_string(),
                "hooked-force-quit".to_string()
            )]
        );
    }

    #[test]
    fn active_subagent_count_reads_started_lineage_records() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var("HERMES_HOME").ok();
        // SAFETY: serialized by env_test_lock.
        unsafe { std::env::set_var("HERMES_HOME", tmp.path()) };
        let subagents = tmp.path().join("subagents");
        std::fs::create_dir_all(&subagents).expect("subagents dir");
        std::fs::write(
            subagents.join("a.json"),
            r#"{"sub_agent_id":"a","status":"started"}"#,
        )
        .expect("write active");
        std::fs::write(
            subagents.join("b.json"),
            r#"{"sub_agent_id":"b","status":"completed"}"#,
        )
        .expect("write complete");
        std::fs::write(subagents.join("bad.json"), "{not json").expect("write bad");

        let app = build_minimal_test_app();
        assert_eq!(app.active_subagent_count(), 1);

        // SAFETY: serialized by env_test_lock.
        unsafe {
            match prev_home {
                Some(value) => std::env::set_var("HERMES_HOME", value),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }

    #[test]
    fn test_session_info_serialization() {
        let info = SessionInfo {
            session_id: "test-123".to_string(),
            model: "dynamic".to_string(),
            personality: Some("helpful".to_string()),
            message_count: 5,
            created_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "test-123");
        assert_eq!(back.model, "dynamic");
    }

    #[test]
    fn test_collect_quorum_models_dedup_and_limit() {
        let policy = QuorumPolicy {
            enabled: true,
            voters: 3,
            models: vec![
                "nous:openai/gpt-5.5-pro".to_string(),
                "nous:openai/gpt-5.5-pro".to_string(),
                "nous:anthropic/claude-opus-4.7".to_string(),
                "nous:deepseek/deepseek-v4-pro".to_string(),
            ],
            mode: "balanced".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let models = App::collect_quorum_models(&policy, "nous:openai/gpt-5.5-pro");
        assert_eq!(
            models,
            vec![
                "nous:openai/gpt-5.5-pro".to_string(),
                "nous:anthropic/claude-opus-4.7".to_string(),
                "nous:deepseek/deepseek-v4-pro".to_string()
            ]
        );
    }

    #[test]
    fn test_extract_last_assistant_output_prefers_non_empty_assistant_text() {
        let messages = vec![
            hermes_core::Message::user("hello"),
            hermes_core::Message::assistant(""),
            hermes_core::Message::assistant("final answer"),
        ];
        let output = App::extract_last_assistant_output(&messages);
        assert_eq!(output, "final answer");
    }

    #[test]
    fn test_required_quorum_success_majority() {
        assert_eq!(App::required_quorum_success(1), 1);
        assert_eq!(App::required_quorum_success(2), 2);
        assert_eq!(App::required_quorum_success(3), 2);
        assert_eq!(App::required_quorum_success(4), 3);
        assert_eq!(App::required_quorum_success(5), 3);
    }

    #[test]
    fn test_quorum_mode_armed_once_triggers_without_system_hint() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());

        let _ = set_quorum_policy(
            true,
            Some(3),
            Some(vec![
                "nous:openai/gpt-5.5-pro".to_string(),
                "nous:anthropic/claude-opus-4.7".to_string(),
            ]),
        )
        .expect("set quorum policy");
        let policy = load_quorum_policy().expect("load quorum policy");
        assert!(
            policy.enabled,
            "quorum policy should be enabled in test home"
        );

        let mut app = build_minimal_test_app();
        app.messages = vec![hermes_core::Message::user("run quorum now")];
        app.quorum_armed_once = true;
        let has_hint = app.messages.iter().any(|message| {
            message.role == hermes_core::MessageRole::System
                && message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with(QUORUM_HINT_PREFIX)
        });
        let has_user_turn = app
            .messages
            .iter()
            .any(|m| m.role == hermes_core::MessageRole::User);

        assert!(
            app.quorum_mode_armed_for_turn().is_some(),
            "one-shot quorum arm should trigger fan-out without relying on stale system hints (enabled={}, armed_once={}, has_hint={}, has_user_turn={})",
            policy.enabled,
            app.quorum_armed_once,
            has_hint,
            has_user_turn
        );

        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }

    #[test]
    fn moa_virtual_model_arms_quorum_without_global_policy() {
        let mut app = build_minimal_test_app();
        app.current_model = "moa:default".to_string();
        app.messages = vec![hermes_core::Message::user("solve with moa")];

        let policy = app
            .quorum_mode_armed_for_turn()
            .expect("moa model should arm quorum fan-out");

        assert!(policy.enabled);
        assert_eq!(policy.voters, 2);
        assert_eq!(
            policy.models,
            vec![
                "openai-codex:gpt-5.5".to_string(),
                "openrouter:deepseek/deepseek-v4-pro".to_string()
            ]
        );
        assert_eq!(policy.mode, "moa-default");
        assert_eq!(
            App::quorum_synthesis_model_for_original("moa:default"),
            "openrouter:anthropic/claude-opus-4.7"
        );
        assert_eq!(
            App::quorum_synthesis_model_for_original("anthropic:claude-sonnet-4-6"),
            "anthropic:claude-sonnet-4-6"
        );
    }

    #[test]
    fn moa_oneshot_restores_previous_model_after_failed_turn() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let prev_passes = std::env::var("HERMES_QUORUM_VOTER_PASSES").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HERMES_QUORUM_VOTER_PASSES", "1");

        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        app.current_model = "lm-studio:local-model".to_string();
        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let result = runtime.block_on(app.submit_moa_oneshot("compare these plans"));

        assert!(result.is_err(), "NoBackendProvider should fail the turn");
        assert_eq!(
            app.current_model, "lm-studio:local-model",
            "one-shot /moa must restore the prior model even when inference fails"
        );
        assert!(app
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some("compare these plans")));

        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
        match prev_passes {
            Some(v) => std::env::set_var("HERMES_QUORUM_VOTER_PASSES", v),
            None => std::env::remove_var("HERMES_QUORUM_VOTER_PASSES"),
        }
    }

    #[test]
    fn test_clear_quorum_system_hints_inplace_preserves_other_system_messages() {
        let mut app = build_minimal_test_app();
        app.messages = vec![
            hermes_core::Message::system("[QUORUM_MODE] quorum armed"),
            hermes_core::Message::system("normal system context"),
            hermes_core::Message::user("hello"),
        ];

        app.clear_quorum_system_hints_inplace();

        assert_eq!(app.messages.len(), 2);
        assert!(app.messages.iter().all(|message| !message
            .content
            .as_deref()
            .unwrap_or_default()
            .starts_with("[QUORUM_MODE] ")));
        assert!(app
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some("normal system context")));
    }

    #[test]
    fn test_run_agent_quorum_arm_persists_artifact_even_on_voter_failures() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let prev_passes = std::env::var("HERMES_QUORUM_VOTER_PASSES").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HERMES_QUORUM_VOTER_PASSES", "1");

        let _ = set_quorum_policy(
            true,
            Some(3),
            Some(vec![
                "custom:quorum-voter-a".to_string(),
                "custom:quorum-voter-b".to_string(),
                "custom:quorum-voter-c".to_string(),
            ]),
        )
        .expect("set quorum policy");

        let mut app = build_minimal_test_app();
        app.session_id = "quorum-test-session".to_string();
        app.messages = vec![hermes_core::Message::user(
            "no tools, just verify quorum fan-out branch",
        )];
        app.quorum_armed_once = true;

        let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
        let result = runtime.block_on(app.run_agent());
        assert!(
            result.is_err(),
            "NoBackendProvider should fail voter inference, but quorum artifact must still persist"
        );

        let quorum_dir = app.state_root.join("quorum");
        let artifacts: Vec<_> = std::fs::read_dir(&quorum_dir)
            .expect("read quorum artifact dir")
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|v| v.to_str()) == Some("json"))
            .collect();
        assert!(
            !artifacts.is_empty(),
            "quorum run should write at least one artifact file"
        );
        let latest = artifacts
            .iter()
            .max_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok())
            .expect("latest quorum artifact");
        let raw = std::fs::read_to_string(latest.path()).expect("read quorum artifact");
        let doc: serde_json::Value = serde_json::from_str(&raw).expect("parse quorum artifact");
        assert_eq!(
            doc.get("session_id").and_then(|v| v.as_str()),
            Some("quorum-test-session")
        );
        assert!(
            doc.get("voters")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| !arr.is_empty()),
            "artifact should contain per-voter outcomes"
        );

        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
        match prev_passes {
            Some(v) => std::env::set_var("HERMES_QUORUM_VOTER_PASSES", v),
            None => std::env::remove_var("HERMES_QUORUM_VOTER_PASSES"),
        }
    }

    include!("tests/quorum_contract.rs");
    include!("tests/session_runtime.rs");
    include!("tests/tool_scope.rs");

}
