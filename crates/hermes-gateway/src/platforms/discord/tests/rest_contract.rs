mod rest_contract {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::body_partial_json;

    fn adapter_for_mock(server: &MockServer, app_id: Option<&str>) -> DiscordAdapter {
        let mut config = test_config();
        config.api_base_url = Some(server.uri());
        config.application_id = app_id.map(str::to_string);
        let mut adapter =
            DiscordAdapter::new(config).expect("initialise adapter for loopback server");
        adapter.client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build loopback-only client");
        adapter
    }

    #[tokio::test]
    async fn forum_post_preserves_starter_ids_when_a_follow_up_fails() {
        let server = MockServer::start().await;
        let adapter = adapter_for_mock(&server, None);
        let starter = format!("{}\n", "x".repeat(MAX_MESSAGE_LENGTH - 1));
        Mock::given(method("POST"))
            .and(path("/channels/forum/threads"))
            .and(body_partial_json(json!({
                "auto_archive_duration": 60,
                "message": {"content": starter}
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "thread",
                "message": {"id": "starter", "channel_id": "thread"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/channels/thread/messages"))
            .and(body_partial_json(json!({"content": "follow-up"})))
            .respond_with(ResponseTemplate::new(403).set_body_string("missing channel permission"))
            .expect(1)
            .mount(&server)
            .await;

        let send = adapter
            .send_forum_post("forum", &format!("{starter}follow-up"), Some(60))
            .await
            .expect("the starter was accepted");
        assert_eq!(send.thread_id, "thread");
        assert_eq!(send.message_id, "starter");
        server.verify().await;
        assert_eq!(send.warnings.len(), 1);
        assert!(
            send.warnings[0].contains("missing channel permission"),
            "{:?}",
            send.warnings
        );
    }

    #[tokio::test]
    async fn create_thread_and_reactions_use_the_message_and_thread_ids() {
        let server = MockServer::start().await;
        let adapter = adapter_for_mock(&server, None);
        Mock::given(method("POST"))
            .and(path("/channels/channel/messages/message/threads"))
            .and(body_json(
                json!({"name": "Review", "auto_archive_duration": 1440}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": "created-thread", "name": "Review", "type": 11,
                "guild_id": "guild", "parent_id": "channel"
            })))
            .expect(1)
            .mount(&server)
            .await;
        for verb in ["PUT", "DELETE"] {
            Mock::given(method(verb))
                .and(path(
                    "/channels/created-thread/messages/message/reactions/check/@me",
                ))
                .and(header("Authorization", "Bot test-token"))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }

        let thread = adapter
            .create_thread("channel", "message", "Review", Some(1440))
            .await
            .expect("create thread");
        assert_eq!(thread.id, "created-thread");
        assert_eq!(thread.parent_id.as_deref(), Some("channel"));
        adapter
            .add_reaction(&thread.id, "message", "check")
            .await
            .unwrap();
        adapter
            .remove_reaction(&thread.id, "message", "check")
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let methods: Vec<_> = requests
            .iter()
            .map(|request| request.method.as_str())
            .collect();
        assert_eq!(methods, ["POST", "PUT", "DELETE"]);
        server.verify().await;
    }

    #[tokio::test]
    async fn command_registration_routes_globally_or_by_guild() {
        let server = MockServer::start().await;
        let command = SlashCommand {
            name: "status".into(),
            description: "Show status".into(),
            options: None,
            default_member_permissions: None,
            dm_permission: None,
            nsfw: None,
            contexts: None,
            integration_types: None,
            command_type: 1,
        };
        let commands = [command];
        let unconfigured = adapter_for_mock(&server, None);
        for result in [
            unconfigured.register_slash_commands(&commands).await,
            unconfigured
                .register_guild_slash_commands("guild", &commands)
                .await,
        ] {
            assert!(matches!(
                result,
                Err(GatewayError::Platform(message)) if message.contains("application_id required")
            ));
        }
        assert!(server.received_requests().await.unwrap().is_empty());

        let adapter = adapter_for_mock(&server, Some("app"));
        for endpoint in [
            "/applications/app/commands",
            "/applications/app/guilds/guild/commands",
        ] {
            Mock::given(method("PUT"))
                .and(path(endpoint))
                .and(body_json(
                    json!([{"name":"status","description":"Show status","type":1}]),
                ))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
        }
        adapter.register_slash_commands(&commands).await.unwrap();
        adapter
            .register_guild_slash_commands("guild", &commands)
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let paths: Vec<_> = requests.iter().map(|request| request.url.path()).collect();
        assert_eq!(
            paths,
            [
                "/applications/app/commands",
                "/applications/app/guilds/guild/commands"
            ]
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn interaction_callback_distinguishes_immediate_and_deferred_responses() {
        let server = MockServer::start().await;
        let adapter = adapter_for_mock(&server, None);
        Mock::given(method("POST"))
            .and(path("/interactions/message/interaction-fixture/callback"))
            .and(body_json(json!({"type":4,"data":{"content":"done"}})))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/interactions/pending/interaction-fixture/callback"))
            .and(body_json(json!({"type":5})))
            .respond_with(ResponseTemplate::new(409).set_body_string("already acknowledged"))
            .expect(1)
            .mount(&server)
            .await;

        adapter
            .respond_to_interaction("message", "interaction-fixture", "done")
            .await
            .unwrap();
        let deferred = adapter
            .defer_interaction("pending", "interaction-fixture")
            .await;
        assert!(matches!(
            deferred,
            Err(GatewayError::SendFailed(message)) if message.contains("already acknowledged")
        ));
        server.verify().await;
    }
}
