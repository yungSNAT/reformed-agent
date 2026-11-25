    /// An isolated agent posts no compaction checkpoint; the same agent without isolation
    /// does, so the listener below would see it.
    #[test]
    fn an_isolated_agent_posts_no_compaction_checkpoint() {
        struct Unused;
        #[async_trait::async_trait]
        impl LlmProvider for Unused {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                unreachable!("no model call in this test")
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
                unreachable!("no model call in this test")
            }
        }

        let _lock = env_test_lock();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let _url = EnvVarGuard::set("CONTEXTLATTICE_ORCHESTRATOR_URL", &url);
        let _key = EnvVarGuard::remove("CONTEXTLATTICE_API_KEY");
        let _mode = EnvVarGuard::set("HERMES_CONTEXTLATTICE_COMPACTION_GOVERNANCE", "advisory");
        let _timeout = EnvVarGuard::set("HERMES_CONTEXTLATTICE_TIMEOUT_SECS", "1");
        let connections = |isolated: bool| {
            let agent = AgentLoop::new(
                AgentConfig {
                    isolated,
                    ..AgentConfig::default()
                },
                Arc::new(ToolRegistry::new()),
                Arc::new(Unused),
            );
            agent.emit_compaction_contextlattice_checkpoint(150_000, 40_000, 200_000);
            std::iter::from_fn(|| listener.accept().ok()).count()
        };

        assert_eq!(connections(true), 0);
        assert_eq!(connections(false), 1);
    }
