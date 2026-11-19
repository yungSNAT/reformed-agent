    /// A provider that answers 401 to every key but `accepted`, sending the
    /// key it holds the way a real provider's `replace_credential` does.
    struct UnauthorizedUntilRefreshed {
        key: Mutex<String>,
        accepted: &'static str,
        refusal: &'static str,
        sent_keys: Mutex<Vec<String>>,
    }

    impl UnauthorizedUntilRefreshed {
        fn new(key: &str, accepted: &'static str) -> Arc<Self> {
            Self::refusing(key, accepted, "API error 401 Unauthorized: invalid x-api-key")
        }

        fn refusing(key: &str, accepted: &'static str, refusal: &'static str) -> Arc<Self> {
            Arc::new(Self {
                key: Mutex::new(key.to_string()),
                accepted,
                refusal,
                sent_keys: Mutex::new(Vec::new()),
            })
        }

        fn send(&self) -> Result<(), AgentError> {
            let key = self.key.lock().expect("key lock").clone();
            self.sent_keys.lock().expect("sent lock").push(key.clone());
            if key == self.accepted {
                Ok(())
            } else {
                Err(AgentError::LlmApi(self.refusal.to_string()))
            }
        }

        fn sent_keys(&self) -> Vec<String> {
            self.sent_keys.lock().expect("sent lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for UnauthorizedUntilRefreshed {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            self.send()?;
            Ok(hermes_core::LlmResponse {
                message: Message::assistant("ok"),
                usage: None,
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
            let chunks = match self.send() {
                Ok(()) => vec![
                    Ok(stream_chunk_content("ok")),
                    Ok(StreamChunk {
                        delta: None,
                        finish_reason: Some("stop".to_string()),
                        usage: None,
                    }),
                ],
                Err(e) => vec![Err(e)],
            };
            futures::stream::iter(chunks).boxed()
        }

        fn active_credential(&self) -> Option<String> {
            Some(self.key.lock().expect("key lock").clone())
        }

        fn replace_credential(&self, credential: &str) -> bool {
            *self.key.lock().expect("key lock") = credential.to_string();
            true
        }
    }

    /// A Hermes home whose `tokens.json` holds an unexpired Anthropic OAuth
    /// token, and a token endpoint that mints `new-access` for it.
    async fn anthropic_oauth_fixture(
        stored_access: &str,
    ) -> (tempfile::TempDir, wiremock::MockServer, AgentConfig) {
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let home = tempfile::tempdir().expect("hermes home");
        let auth_dir = home.path().join("auth");
        std::fs::create_dir_all(&auth_dir).expect("auth dir");
        let store = serde_json::json!({
            "anthropic": {
                "provider": "anthropic",
                "access_token": stored_access,
                "refresh_token": "old-refresh",
                "token_type": "Bearer",
                "expires_at": "2099-01-01T00:00:00Z"
            }
        });
        std::fs::write(auth_dir.join("tokens.json"), store.to_string()).expect("token store");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("refresh_token=old-refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh",
                "token_type": "Bearer",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let mut cfg = AgentConfig {
            model: "anthropic:claude-test".to_string(),
            hermes_home: Some(home.path().to_string_lossy().to_string()),
            stream_read_max_retries: 0,
            ..AgentConfig::default()
        };
        cfg.retry.max_retries = 0;
        cfg.runtime_providers.insert(
            "anthropic".to_string(),
            RuntimeProviderConfig {
                oauth_token_url: Some(format!("{}/v1/oauth/token", server.uri())),
                oauth_client_id: Some("test-client".to_string()),
                ..RuntimeProviderConfig::default()
            },
        );
        (home, server, cfg)
    }

    fn stored_anthropic_token(home: &tempfile::TempDir) -> (String, String) {
        let raw = std::fs::read_to_string(home.path().join("auth").join("tokens.json"))
            .expect("read token store");
        let store: serde_json::Value = serde_json::from_str(&raw).expect("token store json");
        let entry = &store["anthropic"];
        (
            entry["access_token"].as_str().unwrap_or_default().to_string(),
            entry["refresh_token"].as_str().unwrap_or_default().to_string(),
        )
    }

    async fn token_requests(server: &wiremock::MockServer) -> usize {
        server
            .received_requests()
            .await
            .map(|requests| requests.len())
            .unwrap_or(0)
    }

    fn user_context() -> ContextManager {
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::user("hello"));
        ctx
    }

    #[tokio::test]
    async fn unauthorized_stored_oauth_token_is_refreshed_once_and_retried() {
        let (home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        let provider = UnauthorizedUntilRefreshed::new("old-access", "new-access");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        let resp = agent
            .call_llm_with_retry(&user_context(), &[], None, None)
            .await
            .expect("the retry with the refreshed token succeeds");

        assert_eq!(resp.message.content.as_deref(), Some("ok"));
        assert_eq!(provider.sent_keys(), ["old-access", "new-access"]);
        assert_eq!(token_requests(&server).await, 1);
        assert_eq!(
            stored_anthropic_token(&home),
            ("new-access".to_string(), "new-refresh".to_string())
        );

        // The provider keeps the new token for the next request.
        agent
            .call_llm_with_retry(&user_context(), &[], None, None)
            .await
            .expect("later requests send the refreshed token");
        assert_eq!(provider.sent_keys().last().map(String::as_str), Some("new-access"));
        assert_eq!(token_requests(&server).await, 1);
    }

    #[tokio::test]
    async fn unauthorized_stream_with_stored_oauth_token_is_refreshed_and_retried() {
        let (home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        let provider = UnauthorizedUntilRefreshed::new("old-access", "new-access");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        let out = agent
            .collect_stream_llm_response(
                &user_context(),
                &[],
                None,
                "anthropic:claude-test",
                None,
                &|_| {},
            )
            .await
            .expect("the stream retried with the refreshed token completes");

        let StreamCollectOutcome::Complete(resp) = out else {
            panic!("expected a complete stream");
        };
        assert_eq!(resp.message.content.as_deref(), Some("ok"));
        assert_eq!(provider.sent_keys(), ["old-access", "new-access"]);
        assert_eq!(token_requests(&server).await, 1);
        assert_eq!(stored_anthropic_token(&home).0, "new-access");
    }

    #[tokio::test]
    async fn unauthorized_key_that_is_not_the_stored_token_is_not_refreshed() {
        let (home, server, cfg) = anthropic_oauth_fixture("stored-access").await;
        let provider = UnauthorizedUntilRefreshed::new("sk-ant-env-key", "new-access");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        let err = agent
            .call_llm_with_retry(&user_context(), &[], None, None)
            .await
            .expect_err("a key from elsewhere keeps its 401");

        assert!(err.to_string().contains("401"), "{err}");
        assert_eq!(provider.sent_keys(), ["sk-ant-env-key"]);
        assert_eq!(token_requests(&server).await, 0);
        assert_eq!(stored_anthropic_token(&home).0, "stored-access");
    }

    #[tokio::test]
    async fn unauthorized_after_refresh_ends_the_call_without_a_second_refresh() {
        let (_home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        let provider = UnauthorizedUntilRefreshed::new("old-access", "never-accepted");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        let err = agent
            .call_llm_with_retry(&user_context(), &[], None, None)
            .await
            .expect_err("the refreshed token is refused too");

        assert!(err.to_string().contains("401"), "{err}");
        assert_eq!(provider.sent_keys(), ["old-access", "new-access"]);
        assert_eq!(token_requests(&server).await, 1);
    }

    #[tokio::test]
    async fn forbidden_that_mentions_unauthorized_is_not_refreshed() {
        let (home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        let provider = UnauthorizedUntilRefreshed::refusing(
            "old-access",
            "new-access",
            "API error 403 Forbidden: unauthorized",
        );
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        let err = agent
            .call_llm_with_retry(&user_context(), &[], None, None)
            .await
            .expect_err("a 403 keeps its refusal");
        assert!(err.to_string().contains("403"), "{err}");
        let out = agent
            .collect_stream_llm_response(&user_context(), &[], None, "anthropic:claude-test", None, &|_| {})
            .await;
        assert!(matches!(out, Err(ref e) if e.to_string().contains("403")), "{out:?}");

        assert_eq!(provider.sent_keys(), ["old-access", "old-access"]);
        assert_eq!(token_requests(&server).await, 0);
        assert_eq!(stored_anthropic_token(&home).0, "old-access");
    }

    #[tokio::test]
    async fn unauthorized_stream_after_refresh_ends_without_a_second_refresh() {
        let (_home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        let provider = UnauthorizedUntilRefreshed::new("old-access", "never-accepted");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        let out = agent
            .collect_stream_llm_response(&user_context(), &[], None, "anthropic:claude-test", None, &|_| {})
            .await;

        assert!(matches!(out, Err(ref e) if e.to_string().contains("401")), "{out:?}");
        assert_eq!(provider.sent_keys(), ["old-access", "new-access"]);
        assert_eq!(token_requests(&server).await, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refreshed_token_that_cannot_be_stored_is_kept_and_stored_later() {
        use std::os::unix::fs::PermissionsExt;

        let (home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        let auth_dir = home.path().join("auth");
        let provider = UnauthorizedUntilRefreshed::new("old-access", "new-access");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o500))
            .expect("make the auth folder read-only");
        let result = agent.call_llm_with_retry(&user_context(), &[], None, None).await;
        std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o700))
            .expect("make the auth folder writable again");

        result.expect("the refreshed token is used although it was not stored");
        assert_eq!(stored_anthropic_token(&home).0, "old-access");

        // The next turn writes the kept credential, without another exchange.
        agent.refresh_oauth_store_tokens_if_needed().await;
        assert_eq!(
            stored_anthropic_token(&home),
            ("new-access".to_string(), "new-refresh".to_string())
        );
        assert_eq!(token_requests(&server).await, 1);
    }
