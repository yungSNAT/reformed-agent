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

    #[tokio::test]
    async fn text_retries_a_missing_reply_once_then_suppresses_later_references() {
        let server = MockServer::start().await;
        let mut adapter = adapter_for_mock(&server, None);
        adapter.config.reply_to_mode = "all".to_string();
        let metadata = DiscordSendMetadata::with_thread_and_reply("thread", "original")
            .with_non_conversational(true);
        let starter = format!("{}\n", "x".repeat(MAX_MESSAGE_LENGTH - 1));
        Mock::given(method("POST"))
            .and(path("/channels/thread/messages"))
            .and(header("Authorization", "Bot test-token"))
            .respond_with(|request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                if body.get("message_reference").is_some() {
                    return ResponseTemplate::new(400).set_body_string("Unknown Message");
                }
                let id = if body["content"] == "follow-up" {
                    "second"
                } else {
                    "first"
                };
                ResponseTemplate::new(200).set_body_json(json!({"id": id, "channel_id": "thread"}))
            })
            .expect(3)
            .mount(&server)
            .await;

        let ids = adapter
            .send_text_with_metadata("parent", &format!("{starter}follow-up"), Some(&metadata))
            .await
            .unwrap();
        assert_eq!(ids, ["first", "second"]);
        assert!(adapter.non_conversational_message_contains("first"));
        assert!(adapter.non_conversational_message_contains("second"));
        assert!(!adapter.non_conversational_message_contains("original"));

        let requests = server.received_requests().await.unwrap();
        let bodies: Vec<serde_json::Value> = requests
            .iter()
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect();
        let [initial, retry, follow_up] = bodies.as_slice() else {
            panic!("expected one rejected reference, its retry and one follow-up");
        };
        assert_eq!(initial["content"], starter);
        assert_eq!(
            initial["message_reference"],
            json!({"message_id": "original", "fail_if_not_exists": false}),
        );
        let mut expected_retry = initial.clone();
        expected_retry
            .as_object_mut()
            .unwrap()
            .remove("message_reference");
        assert_eq!(retry, &expected_retry);
        assert_eq!(follow_up["content"], "follow-up");
        assert!(follow_up.get("message_reference").is_none());
    }

    #[tokio::test]
    async fn message_edits_and_thread_renames_send_bounded_changes_and_return_api_errors() {
        let server = MockServer::start().await;
        let adapter = adapter_for_mock(&server, None);
        let text = "é".repeat(MAX_MESSAGE_LENGTH);
        let title = "🦀".repeat(48) + "...";
        for (route, body, status) in [
            (
                "/channels/thread/messages/first",
                json!({"content": text}),
                200,
            ),
            (
                "/channels/thread/messages/blocked",
                json!({"content": "edit"}),
                403,
            ),
            ("/channels/thread", json!({"name": title}), 200),
            ("/channels/blocked", json!({"name": "Hermes"}), 403),
        ] {
            Mock::given(method("PATCH"))
                .and(path(route))
                .and(header("Authorization", "Bot test-token"))
                .and(body_partial_json(body))
                .respond_with(ResponseTemplate::new(status).set_body_string("fixture permission"))
                .expect(1)
                .mount(&server)
                .await;
        }
        adapter
            .edit_text("thread", "first", &(text + "extra"))
            .await
            .unwrap();
        assert!(matches!(
            adapter.edit_text("thread", "blocked", "edit").await,
            Err(GatewayError::SendFailed(error)) if error.contains("fixture permission")
        ));
        adapter
            .rename_thread_channel(" thread ", &format!("  {}  ", "🦀".repeat(60)))
            .await
            .unwrap();
        assert!(matches!(
            adapter.rename_thread_channel("blocked", " ").await,
            Err(GatewayError::SendFailed(error)) if error.contains("fixture permission")
        ));
        assert!(matches!(
            adapter.rename_thread_channel(" ", "fixture").await,
            Err(GatewayError::SendFailed(error)) if error.contains("requires a thread id")
        ));
        assert_eq!(server.received_requests().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn attachments_and_image_embeds_route_to_the_thread() {
        let server = MockServer::start().await;
        let adapter = adapter_for_mock(&server, None);
        let metadata = DiscordSendMetadata::with_thread_id("thread").with_non_conversational(true);
        let directory = tempfile::tempdir().unwrap();
        let local_file = directory.path().join("fixture.txt");
        let bytes = "café audio fixture";
        tokio::fs::write(&local_file, bytes).await.unwrap();
        let local_file = local_file.to_str().unwrap();
        let image_file = directory.path().join("poster.svg");
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1" fill="blue"/></svg>"#;
        tokio::fs::write(&image_file, svg).await.unwrap();
        Mock::given(method("POST"))
            .and(path("/channels/thread/messages"))
            .and(header("Authorization", "Bot test-token"))
            .respond_with(|request: &wiremock::Request| {
                if request.headers["content-type"]
                    .as_bytes()
                    .starts_with(b"multipart/form-data")
                {
                    let filename = b"filename=\"poster.svg\"";
                    let id = if request.body.windows(filename.len()).any(|part| part == filename) {
                        "image"
                    } else {
                        "attachment"
                    };
                    return ResponseTemplate::new(201)
                        .set_body_json(json!({"id": id, "channel_id": "thread"}));
                }
                ResponseTemplate::new(201)
                    .set_body_json(json!({"id": "embed", "channel_id": "thread"}))
            })
            .expect(3)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/channels/blocked/messages"))
            .and(header("Authorization", "Bot test-token"))
            .respond_with(ResponseTemplate::new(413).set_body_string("fixture upload limit"))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            adapter
                .send_voice("parent", local_file, Some("caption"), Some(&metadata))
                .await
                .unwrap(),
            "attachment",
        );
        assert_eq!(
            adapter
                .send_image(
                    "parent",
                    "https://example.invalid/fixture.png",
                    Some("image caption"),
                    Some(&metadata),
                )
                .await
                .unwrap(),
            "embed",
        );
        assert_eq!(
            adapter
                .send_image_file(
                    "parent",
                    image_file.to_str().unwrap(),
                    Some("poster attachment"),
                    Some(&metadata),
                )
                .await
                .unwrap(),
            "image",
        );
        assert!(adapter.non_conversational_message_contains("attachment"));
        assert!(adapter.non_conversational_message_contains("embed"));
        assert!(adapter.non_conversational_message_contains("image"));
        assert!(matches!(
            adapter.upload_file("blocked", local_file, None).await,
            Err(GatewayError::SendFailed(error)) if error.contains("fixture upload limit")
        ));
        assert!(matches!(
            adapter.upload_file("blocked", directory.path().join("missing").to_str().unwrap(), None).await,
            Err(GatewayError::SendFailed(error)) if error.contains("Failed to read file")
        ));

        let requests = server.received_requests().await.unwrap();
        let [attachment, embed, image, _rejected] = requests.as_slice() else {
            panic!("expected three thread messages and one rejected upload");
        };
        for (request, filename, expected_file, caption) in [
            (attachment, "fixture.txt", bytes, "caption"),
            (image, "poster.svg", svg, "poster attachment"),
        ] {
            let content_type = request.headers["content-type"].to_str().unwrap();
            let boundary = content_type
                .strip_prefix("multipart/form-data; boundary=")
                .unwrap();
            let form = std::str::from_utf8(&request.body).unwrap();
            let delimiter = format!("--{boundary}");
            let parts: Vec<_> = form
                .split(&delimiter)
                .filter_map(|part| part.split_once("\r\n\r\n"))
                .collect();
            let [(file_header, file_body), (payload_header, payload_body)] = parts.as_slice() else {
                panic!("expected an attachment and its metadata");
            };
            let filename = format!("filename=\"{filename}\"");
            for (headers, expected) in [
                (file_header, &["name=\"files[0]\"", filename.as_str()][..]),
                (payload_header, &["name=\"payload_json\""][..]),
            ] {
                let disposition = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("Content-Disposition"))
                    .expect("multipart part has a content disposition")
                    .1;
                let mut fields = disposition.split(';').map(str::trim);
                assert_eq!(fields.next(), Some("form-data"));
                let fields: Vec<_> = fields.collect();
                assert!(expected.iter().all(|field| fields.contains(field)));
            }
            assert_eq!(file_body.strip_suffix("\r\n"), Some(expected_file));
            let payload: serde_json::Value = serde_json::from_str(payload_body).unwrap();
            assert_eq!(payload["content"], caption);
            assert!(payload["allowed_mentions"].is_object());
        }
        assert_eq!(embed.headers["content-type"], "application/json");
        let payload: serde_json::Value = serde_json::from_slice(&embed.body).unwrap();
        assert_eq!(payload["content"], "image caption");
        assert_eq!(
            payload["embeds"][0]["image"]["url"],
            "https://example.invalid/fixture.png"
        );
    }

    #[tokio::test]
    async fn rest_write_errors_keep_the_operation_and_provider_message() {
        let server = MockServer::start().await;
        let adapter = adapter_for_mock(&server, Some("fixture-app"));
        let maintenance = "fixture REST service unavailable";
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(503).set_body_string(maintenance))
            .expect(8)
            .mount(&server)
            .await;

        let errors = [
            (
                "forum post",
                adapter.send_forum_post("forum", "Fixture summary", None).await.unwrap_err(),
            ),
            (
                "embed",
                adapter.send_embed("channel", Some("Fixture embed"), &[]).await.unwrap_err(),
            ),
            (
                "add_reaction",
                adapter.add_reaction("channel", "message", "%E2%9C%85").await.unwrap_err(),
            ),
            (
                "remove_reaction",
                adapter.remove_reaction("channel", "message", "%E2%9C%85").await.unwrap_err(),
            ),
            (
                "create_thread",
                adapter.create_thread("channel", "message", "Fixture topic", None).await.unwrap_err(),
            ),
            (
                "register_commands",
                adapter.register_slash_commands(&[]).await.unwrap_err(),
            ),
            (
                "register_guild_commands",
                adapter.register_guild_slash_commands("fixture-guild", &[]).await.unwrap_err(),
            ),
            (
                "interaction response",
                adapter.respond_to_interaction("fixture-interaction", "fixture-callback-value", "Fixture response")
                    .await
                    .unwrap_err(),
            ),
        ];
        for (operation, error) in errors {
            assert!(matches!(error, GatewayError::SendFailed(ref message)
                if message == &format!("Discord {operation} API error: {maintenance}")),
                "{operation}: {error}");
        }
    }

    #[tokio::test]
    async fn forum_posts_fall_back_to_the_thread_id_without_a_starter() {
        let server = MockServer::start().await;
        let adapter = adapter_for_mock(&server, None);
        Mock::given(method("POST"))
            .and(path("/channels/fixture-forum/threads"))
            .and(body_partial_json(json!({"message": {"content": "Fixture summary"}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "fixture-thread"})))
            .expect(1)
            .mount(&server)
            .await;
        let outcome = adapter.send_forum_post("fixture-forum", "Fixture summary", None).await.unwrap();
        assert_eq!(outcome.thread_id, "fixture-thread");
        assert_eq!(outcome.message_id, "fixture-thread");
        assert!(outcome.warnings.is_empty());
    }
}
