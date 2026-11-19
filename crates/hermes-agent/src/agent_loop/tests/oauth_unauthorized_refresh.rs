    /// A provider that answers 401 to every key but `accepted`, sending the
    /// key it holds the way a real provider's `replace_credential` does.
    struct UnauthorizedUntilRefreshed {
        key: Mutex<String>,
        accepted: &'static str,
        refusal: &'static str,
        /// Streamed before a refusal, as output already delivered.
        lead: Option<StreamChunk>,
        sent_keys: Mutex<Vec<String>>,
    }

    impl UnauthorizedUntilRefreshed {
        fn new(key: &str, accepted: &'static str) -> Arc<Self> {
            Self::refusing(key, accepted, "API error 401 Unauthorized: invalid x-api-key")
        }

        fn refusing(key: &str, accepted: &'static str, refusal: &'static str) -> Arc<Self> {
            Self::build(key, accepted, refusal, None)
        }

        fn streaming_before_refusal(key: &str, accepted: &'static str, lead: StreamChunk) -> Arc<Self> {
            Self::build(key, accepted, "API error 401 Unauthorized: invalid x-api-key", Some(lead))
        }

        fn build(
            key: &str,
            accepted: &'static str,
            refusal: &'static str,
            lead: Option<StreamChunk>,
        ) -> Arc<Self> {
            Arc::new(Self {
                key: Mutex::new(key.to_string()),
                accepted,
                refusal,
                lead,
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
            _: &[Message],
            _: &[ToolSchema],
            _: Option<u32>,
            _: Option<f64>,
            _: Option<&str>,
            _: Option<&serde_json::Value>,
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
            _: &[Message],
            _: &[ToolSchema],
            _: Option<u32>,
            _: Option<f64>,
            _: Option<&str>,
            _: Option<&serde_json::Value>,
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
                Err(e) => self.lead.clone().map(Ok).into_iter().chain([Err(e)]).collect(),
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
        match out {
            Err(e) => assert!(e.to_string().contains("403"), "{e}"),
            Ok(_) => panic!("the stream should fail"),
        }

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

        match out {
            Err(e) => assert!(e.to_string().contains("401"), "{e}"),
            Ok(_) => panic!("the stream should fail"),
        }
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

        // Neither a write in place nor a new file beside it can succeed.
        let store = auth_dir.join("tokens.json");
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o400))
            .expect("make the store read-only");
        std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o500))
            .expect("make the auth folder read-only");
        let result = agent.call_llm_with_retry(&user_context(), &[], None, None).await;
        std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o700))
            .expect("make the auth folder writable again");
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600))
            .expect("make the store writable again");

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

    /// Refreshes after a 401 whilst the store cannot be written, has another
    /// process write `later`, then runs the next turn's store step.
    #[cfg(unix)]
    async fn kept_token_meets_a_writer(later: serde_json::Value) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;

        let (home, _server, cfg) = anthropic_oauth_fixture("old-access").await;
        let auth_dir = home.path().join("auth");
        let provider = UnauthorizedUntilRefreshed::new("old-access", "new-access");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o500))
            .expect("make the auth folder read-only");
        let result = agent.call_llm_with_retry(&user_context(), &[], None, None).await;
        std::fs::write(auth_dir.join("tokens.json"), later.to_string()).expect("other writer");
        std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o700))
            .expect("make the auth folder writable again");
        result.expect("the refreshed token is used although it was not stored");

        agent.refresh_oauth_store_tokens_if_needed().await;
        home
    }

    #[cfg(unix)]
    async fn kept_token_gives_way_to_a_writer(access: &str, refresh: &str) {
        let home = kept_token_meets_a_writer(serde_json::json!({
            "anthropic": {
                "provider": "anthropic",
                "access_token": access,
                "refresh_token": refresh,
                "expires_at": "2199-01-01T00:00:00Z"
            }
        }))
        .await;
        assert_eq!(
            stored_anthropic_token(&home),
            (access.to_string(), refresh.to_string())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kept_token_gives_way_to_one_another_writer_stored() {
        kept_token_gives_way_to_a_writer("other-access", "other-refresh").await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kept_token_gives_way_to_a_writer_that_rotated_only_the_refresh_token() {
        kept_token_gives_way_to_a_writer("new-access", "writer-refresh").await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kept_token_is_not_written_back_after_another_writer_removed_its_entry() {
        let home = kept_token_meets_a_writer(serde_json::json!({})).await;
        let raw = std::fs::read_to_string(home.path().join("auth").join("tokens.json"))
            .expect("read token store");
        let store: serde_json::Value = serde_json::from_str(&raw).expect("token store json");
        assert!(store.get("anthropic").is_none(), "{store}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn second_exchange_that_cannot_be_stored_is_kept_too() {
        use std::os::unix::fs::PermissionsExt;
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, ResponseTemplate};

        let (home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        Mock::given(method("POST"))
            .and(body_string_contains("refresh_token=new-refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "newer-access",
                "refresh_token": "newer-refresh",
                "token_type": "Bearer",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;
        let auth_dir = home.path().join("auth");
        let provider = UnauthorizedUntilRefreshed::new("old-access", "never-accepted");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o500))
            .expect("make the auth folder read-only");
        // Each call refreshes once: old to new, then new to newer.
        let first = agent.call_llm_with_retry(&user_context(), &[], None, None).await;
        let second = agent.call_llm_with_retry(&user_context(), &[], None, None).await;
        std::fs::set_permissions(&auth_dir, std::fs::Permissions::from_mode(0o700))
            .expect("make the auth folder writable again");
        assert!(first.is_err() && second.is_err());
        assert_eq!(
            provider.sent_keys(),
            ["old-access", "new-access", "new-access", "newer-access"]
        );
        assert_eq!(token_requests(&server).await, 2);

        agent.refresh_oauth_store_tokens_if_needed().await;
        assert_eq!(
            stored_anthropic_token(&home),
            ("newer-access".to_string(), "newer-refresh".to_string())
        );
    }

    #[tokio::test]
    async fn concurrent_unauthorized_calls_exchange_the_refresh_token_once() {
        let (home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        let provider = UnauthorizedUntilRefreshed::new("old-access", "new-access");
        let first = AgentLoop::new(cfg.clone(), Arc::new(ToolRegistry::new()), provider.clone());
        let second = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let (ctx_a, ctx_b) = (user_context(), user_context());

        let (a, b) = tokio::join!(
            first.call_llm_with_retry(&ctx_a, &[], None, None),
            second.call_llm_with_retry(&ctx_b, &[], None, None)
        );

        a.expect("the first call succeeds after the refresh");
        b.expect("the second call succeeds with the token the first one got");
        assert_eq!(token_requests(&server).await, 1);
        assert_eq!(stored_anthropic_token(&home).0, "new-access");
    }

    #[tokio::test]
    async fn refresh_waits_while_another_process_holds_the_store_lock() {
        let (home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        // Another process's handle on the lock file beside the store.
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(home.path().join("auth").join("tokens.json.lock"))
            .expect("lock file");
        holder.lock().expect("hold the store lock");
        let provider = UnauthorizedUntilRefreshed::new("old-access", "new-access");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        let release = async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let before = token_requests(&server).await;
            holder.unlock().expect("release the store lock");
            before
        };
        let ctx = user_context();
        let (result, exchanged_while_held) =
            tokio::join!(agent.call_llm_with_retry(&ctx, &[], None, None), release);

        assert_eq!(exchanged_while_held, 0);
        result.expect("the refresh goes ahead once the lock is released");
        assert_eq!(token_requests(&server).await, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn token_endpoint_that_never_answers_ends_the_refresh() {
        use wiremock::matchers::method;
        use wiremock::{Mock, ResponseTemplate};

        let (home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(3600)))
            .with_priority(1)
            .mount(&server)
            .await;
        let provider = UnauthorizedUntilRefreshed::new("old-access", "new-access");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        let err = agent
            .call_llm_with_retry(&user_context(), &[], None, None)
            .await
            .expect_err("the 401 stands when the refresh times out");

        assert!(err.to_string().contains("401"), "{err}");
        assert_eq!(provider.sent_keys(), ["old-access"]);
        assert_eq!(stored_anthropic_token(&home).0, "old-access");
    }

    #[tokio::test]
    async fn bad_request_whose_text_contains_401_is_not_refreshed() {
        let (home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        let provider = UnauthorizedUntilRefreshed::refusing(
            "old-access",
            "new-access",
            "API error 400 Bad Request: prompt is too long: 240100 tokens > 200000 maximum",
        );
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        let err = agent
            .call_llm_with_retry(&user_context(), &[], None, None)
            .await
            .expect_err("a 400 keeps its refusal");

        assert!(err.to_string().contains("400"), "{err}");
        assert_eq!(provider.sent_keys(), ["old-access"]);
        assert_eq!(token_requests(&server).await, 0);
        assert_eq!(stored_anthropic_token(&home).0, "old-access");
    }

    #[tokio::test]
    async fn unauthorized_stream_after_delivered_output_is_not_replayed() {
        let thinking = StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: None,
                tool_calls: None,
                extra: Some(serde_json::json!({ "thinking": "planning" })),
            }),
            finish_reason: None,
            usage: None,
        };
        let (_home, server, cfg) = anthropic_oauth_fixture("old-access").await;
        let provider =
            UnauthorizedUntilRefreshed::streaming_before_refusal("old-access", "new-access", thinking);
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());

        let out = agent
            .collect_stream_llm_response(&user_context(), &[], None, "anthropic:claude-test", None, &|_| {})
            .await;

        match out {
            Err(e) => assert!(e.to_string().contains("401"), "{e}"),
            Ok(_) => panic!("the stream should fail"),
        }
        assert_eq!(provider.sent_keys(), ["old-access"]);
        assert_eq!(token_requests(&server).await, 0);
    }

    #[tokio::test]
    async fn routed_provider_replays_with_the_refreshed_token_not_its_configured_key() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, ResponseTemplate};

        let (home, server, mut cfg) = anthropic_oauth_fixture("old-access").await;
        // The route's configured key is the stored token, so a rebuilt route
        // would send it again.
        if let Some(anthropic) = cfg.runtime_providers.get_mut("anthropic") {
            anthropic.api_key = Some("old-access".to_string());
        }
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "new-access"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-test",
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid x-api-key"))
            .with_priority(2)
            .mount(&server)
            .await;
        let primary = UnauthorizedUntilRefreshed::new("primary-key", "primary-key");
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), primary.clone());
        let mut route = agent.turn_route_cost_guard("claude-test".to_string());
        route.provider = Some("anthropic".to_string());
        route.base_url = Some(server.uri());

        let resp = agent
            .call_llm_with_retry(&user_context(), &[], Some(&route), None)
            .await
            .expect("the replay sends the refreshed token");

        assert_eq!(resp.message.content.as_deref(), Some("ok"));
        let sent: Vec<String> = server
            .received_requests()
            .await
            .expect("recorded requests")
            .iter()
            .filter(|r| r.url.path() == "/v1/messages")
            .filter_map(|r| r.headers.get("x-api-key").and_then(|v| v.to_str().ok()).map(str::to_string))
            .collect();
        assert_eq!(sent, ["old-access", "new-access"]);
        assert_eq!(stored_anthropic_token(&home).0, "new-access");
        assert!(primary.sent_keys().is_empty());
    }
