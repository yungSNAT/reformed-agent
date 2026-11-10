    #[test]
    fn test_runtime_provider_command_args_override_primary_acp_metadata() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
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

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: Some("https://api.openai.com/v1".to_string()),
                request_timeout_seconds: Some(45.0),
                api_mode: None,
                command: Some("copilot-language-server".to_string()),
                args: vec![
                    "--stdio".to_string(),
                    "--model".to_string(),
                    "gpt-4o-mini".to_string(),
                ],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );
        runtime_providers.insert(
            "openai-codex".to_string(),
            RuntimeProviderConfig {
                request_timeout_seconds: Some(75.0),
                ..RuntimeProviderConfig::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            provider: Some("openai".to_string()),
            runtime_providers,
            acp_command: Some("global-acp".to_string()),
            acp_args: vec!["--global".to_string()],
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let primary = agent.primary_runtime_snapshot();
        assert_eq!(
            agent.resolve_runtime_request_timeout_seconds("openai"),
            Some(45.0)
        );
        assert_eq!(
            agent.resolve_runtime_request_timeout_seconds("codex"),
            Some(75.0)
        );
        assert_eq!(primary.command.as_deref(), Some("copilot-language-server"));
        assert_eq!(
            primary.args,
            vec![
                "--stdio".to_string(),
                "--model".to_string(),
                "gpt-4o-mini".to_string()
            ]
        );
    }

    #[test]
    fn test_runtime_wire_model_remaps_openai_dynamic_only() {
        assert_eq!(
            AgentLoop::runtime_wire_model_for_provider("openai", "dynamic"),
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL
        );
        assert_eq!(
            AgentLoop::runtime_wire_model_for_provider("codex", "openai:dynamic"),
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL
        );
        assert_eq!(
            AgentLoop::runtime_wire_model_for_provider("openai-codex", "codex:dynamic"),
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL
        );
        assert_eq!(
            AgentLoop::runtime_wire_model_for_provider("openrouter", "dynamic"),
            "dynamic"
        );
        assert_eq!(
            AgentLoop::runtime_wire_model_for_provider("openai", "gpt-5.5"),
            "gpt-5.5"
        );
    }

    #[test]
    fn test_smart_model_routing_codex_provider_alias_builds_runtime() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
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

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "codex".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: Some("https://api.openai.com/v1".to_string()),
                request_timeout_seconds: None,
                api_mode: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let mut config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("codex".to_string()),
                    model: Some("gpt-5-mini".to_string()),
                    base_url: Some("https://api.openai.com/v1".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("总结一下这个需求")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-5-mini")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("codex")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.api_mode.as_ref()),
            Some(&ApiMode::CodexResponses)
        );
    }

    #[test]
    fn test_smart_model_routing_qwen_oauth_alias_builds_runtime() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
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

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "qwen-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-qwen-oauth".to_string()),
                api_key_env: None,
                base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
                request_timeout_seconds: None,
                api_mode: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let mut config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("qwen-oauth".to_string()),
                    model: Some("qwen3-coder-plus".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("给我一段简短总结")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("qwen-oauth")
        );
    }

    #[test]
    fn test_runtime_provider_stepfun_build_supported() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
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

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "stepfun".to_string(),
            RuntimeProviderConfig {
                api_key: Some("stepfun-test-key".to_string()),
                api_key_env: None,
                base_url: None,
                request_timeout_seconds: None,
                api_mode: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        let built =
            agent.build_runtime_provider("stepfun", "step-3.5-flash", None, None, None, None, None);
        assert!(built.is_ok(), "stepfun runtime provider should build");
    }

    #[test]
    fn test_smart_model_routing_openai_codex_reads_auth_store_token() {
        use futures::stream::BoxStream;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
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

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("hermes-auth-fixture-{}", nonce));
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        std::fs::write(
            auth_dir.join("tokens.json"),
            r#"{
  "openai-codex": {
    "provider": "openai-codex",
    "access_token": "codex-oauth-token",
    "token_type": "bearer",
    "refresh_token": null,
    "scope": null,
    "expires_at": "2099-01-01T00:00:00Z"
  }
}"#,
        )
        .expect("write token store");

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai-codex".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                request_timeout_seconds: None,
                api_mode: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            hermes_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai-codex".to_string()),
                    model: Some("gpt-5-codex".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("openai-codex")
        );
        // Best effort cleanup.
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn test_openai_runtime_reads_openai_oauth_token_store_entry() {
        use futures::stream::BoxStream;
        use std::time::{SystemTime, UNIX_EPOCH};

        // The OAuth token is the last source, so no OpenAI key may be set.
        let _env_lock = env_test_lock();
        let _hermes_openai_key = EnvVarGuard::remove("HERMES_OPENAI_API_KEY");
        let _openai_key = EnvVarGuard::remove("OPENAI_API_KEY");

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
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

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("hermes-openai-auth-fixture-{}", nonce));
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        std::fs::write(
            auth_dir.join("tokens.json"),
            r#"{
  "openai": {
    "provider": "openai",
    "access_token": "openai-oauth-token",
    "token_type": "bearer",
    "refresh_token": null,
    "scope": null,
    "expires_at": "2099-01-01T00:00:00Z"
  }
}"#,
        )
        .expect("write token store");

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                request_timeout_seconds: None,
                api_mode: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            hermes_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let resolved = agent.resolve_runtime_api_key("openai", None, None);
        assert_eq!(resolved.as_deref(), Some("openai-oauth-token"));
        let _ = std::fs::remove_dir_all(home);
    }

    /// Primary runtime for the reliability route tests. A reply of "primary"
    /// means the turn fell back to it instead of using the route.
    struct ReliabilityRoutePrimary;

    #[async_trait::async_trait]
    impl LlmProvider for ReliabilityRoutePrimary {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            Ok(hermes_core::LlmResponse {
                message: Message::assistant("primary"),
                usage: None,
                model: "primary".into(),
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
        ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    #[test]
    fn test_reliability_route_keeps_provider_base_url_from_env() {
        use std::io::{Read, Write};

        let _env_lock = env_test_lock();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback fixture");
        let base_url = format!("http://{}/v1", listener.local_addr().expect("fixture address"));
        let _base_url_env = EnvVarGuard::set("DEEPSEEK_BASE_URL", &base_url);
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                if let Some(header_end) = text.find("\r\n\r\n") {
                    let body_len = text[..header_end]
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + body_len {
                        break;
                    }
                }
            }
            let body = r#"{"id":"fixture","object":"chat.completion","created":1,"model":"deepseek-chat","choices":[{"index":0,"message":{"role":"assistant","content":"loopback"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(reply.as_bytes());
            let _ = request_tx.send(String::from_utf8_lossy(&request).to_string());
        });

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "deepseek".to_string(),
            RuntimeProviderConfig {
                api_key: Some("deepseek-fixture-key".to_string()),
                request_timeout_seconds: Some(5.0),
                ..RuntimeProviderConfig::default()
            },
        );
        let mut config = AgentConfig {
            model: "deepseek:deepseek-reasoner".to_string(),
            provider: Some("deepseek".to_string()),
            runtime_providers,
            retry: RetryConfig {
                max_retries: 0,
                fallback_model: Some("deepseek:deepseek-chat".to_string()),
                ..RetryConfig::default()
            },
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(ReliabilityRoutePrimary),
        );

        // The same two steps the run loops take once the governor degrades.
        let model = agent
            .resolve_reliability_degrade_model(agent.config.model.as_str(), None)
            .expect("the configured fallback model is the degrade target");
        let route = agent.turn_route_reliability_guard(model);
        let mut ctx = ContextManager::new(100_000);
        ctx.add_message(Message::user("hello"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let response = runtime.block_on(agent.call_llm_with_retry(&ctx, &[], Some(&route), None));

        let request = request_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the reliability route must address the DEEPSEEK_BASE_URL endpoint");
        assert!(
            request.starts_with("POST /v1/chat/completions "),
            "unexpected request line: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer deepseek-fixture-key"),
            "the route must send the provider's own credential: {request}"
        );
        let response = response.expect("routed reply");
        assert_eq!(response.message.content.as_deref(), Some("loopback"));
    }

    #[test]
    fn test_degrade_guards_do_not_leave_custom_provider() {
        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "custom".to_string(),
            RuntimeProviderConfig {
                api_key: Some("custom-fixture-key".to_string()),
                base_url: Some("http://127.0.0.1:9/v1".to_string()),
                ..RuntimeProviderConfig::default()
            },
        );
        let custom = AgentLoop::new(
            AgentConfig {
                model: "custom:local-model".to_string(),
                provider: Some("custom".to_string()),
                runtime_providers,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(ReliabilityRoutePrimary),
        );
        assert_eq!(
            custom.resolve_reliability_degrade_model("custom:local-model", None),
            None,
            "without a configured fallback the reliability guard must stay on the custom endpoint"
        );
        assert_eq!(
            custom.resolve_cost_degrade_model(),
            None,
            "without a configured fallback the cost guard must stay on the custom endpoint"
        );

        let openai = AgentLoop::new(
            AgentConfig {
                model: "openai:gpt-4o".to_string(),
                provider: Some("openai".to_string()),
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(ReliabilityRoutePrimary),
        );
        assert_eq!(
            openai
                .resolve_reliability_degrade_model("openai:gpt-4o", None)
                .as_deref(),
            Some("openai:gpt-4o-mini")
        );
        assert_eq!(
            openai.resolve_cost_degrade_model().as_deref(),
            Some("openai:gpt-4o-mini")
        );
    }

    #[test]
    fn test_primary_credential_pool_stays_with_the_primary_provider() {
        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "deepseek".to_string(),
            RuntimeProviderConfig {
                api_key: Some("deepseek-fixture-key".to_string()),
                ..RuntimeProviderConfig::default()
            },
        );
        let mut config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            provider: Some("openai".to_string()),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("deepseek".to_string()),
                    model: Some("deepseek-chat".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(ReliabilityRoutePrimary),
        )
        .with_primary_credential_pool(Arc::new(CredentialPool::single("primary-pool-key")));

        let same_provider = agent.turn_route_reliability_guard("openai:gpt-4o-mini".to_string());
        assert!(
            agent.credential_pool_for_route(&same_provider).is_some(),
            "a route on the primary's provider keeps the primary pool"
        );
        let guard = agent.turn_route_reliability_guard("deepseek:deepseek-chat".to_string());
        assert!(
            agent.credential_pool_for_route(&guard).is_none(),
            "a guard route to another provider must not carry the primary's keys"
        );
        let cheap = agent
            .resolve_smart_runtime_route(&[Message::user("hi")])
            .expect("cheap route");
        assert_eq!(cheap.provider.as_deref(), Some("deepseek"));
        assert!(
            agent.credential_pool_for_route(&cheap).is_none(),
            "a cheap route to another provider must not carry the primary's keys"
        );
    }

    /// Writes an unexpired OAuth store entry for `provider` under `home`.
    fn write_oauth_store_entry(home: &std::path::Path, provider: &str, token: &str) {
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        let store = serde_json::json!({
            provider: {
                "provider": provider,
                "access_token": token,
                "token_type": "bearer",
                "refresh_token": null,
                "scope": null,
                "expires_at": "2099-01-01T00:00:00Z"
            }
        });
        std::fs::write(auth_dir.join("tokens.json"), store.to_string()).expect("write token store");
    }

    #[test]
    fn test_route_prefers_configured_key_over_stored_oauth_token() {
        let home = tempfile::tempdir().expect("tempdir");
        write_oauth_store_entry(home.path(), "openai", "stored-chatgpt-oauth-token");
        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("configured-openai-key".to_string()),
                base_url: Some("http://127.0.0.1:9/v1".to_string()),
                ..RuntimeProviderConfig::default()
            },
        );
        let agent = AgentLoop::new(
            AgentConfig {
                model: "openai:gpt-4o".to_string(),
                hermes_home: Some(home.path().to_string_lossy().to_string()),
                runtime_providers,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(ReliabilityRoutePrimary),
        );
        // The primary builder uses the configured key first, so a route to
        // the same provider (and its custom base URL) must too.
        assert_eq!(
            agent.resolve_runtime_api_key("openai", None, None).as_deref(),
            Some("configured-openai-key")
        );
    }

    #[test]
    fn test_route_prefers_provider_env_key_over_stored_oauth_token() {
        let _env_lock = env_test_lock();
        let _qwen_key = EnvVarGuard::set("HERMES_QWEN_OAUTH_API_KEY", "qwen-env-key");
        let home = tempfile::tempdir().expect("tempdir");
        write_oauth_store_entry(home.path(), "qwen-oauth", "stored-qwen-oauth-token");
        let agent = AgentLoop::new(
            AgentConfig {
                hermes_home: Some(home.path().to_string_lossy().to_string()),
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(ReliabilityRoutePrimary),
        );
        assert_eq!(
            agent.resolve_runtime_api_key("qwen-oauth", None, None).as_deref(),
            Some("qwen-env-key")
        );
    }

    #[test]
    fn test_runtime_provider_vertex_aliases_build_without_static_key() {
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(crate::vertex::VertexProvider::new(
                "google/gemini-3-flash-preview",
            )),
        );
        for provider in [
            "vertex",
            "google-vertex",
            "vertex-ai",
            "gcp-vertex",
            "vertexai",
        ] {
            let built = agent.build_runtime_provider(
                provider,
                "google/gemini-3-flash-preview",
                None,
                None,
                None,
                None,
                None,
            );
            assert!(built.is_ok(), "{provider} runtime provider should build");
        }
    }
