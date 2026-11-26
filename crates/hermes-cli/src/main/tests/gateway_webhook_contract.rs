#[tokio::test]
async fn gateway_webhook_routes_explicit_and_default_senders_to_their_chats() {
    if !crate::isolated_test_profile::enter(
        "tests::gateway_webhook_routes_explicit_and_default_senders_to_their_chats",
    ) {
        return;
    }

    let mut dm = DmManager::with_ignore_behavior();
    for user in ["webhook-client", "fixture-sender"] {
        dm.authorize_user_for_platform("webhook", user);
    }
    let gateway = Arc::new(Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        dm,
        hermes_gateway::gateway::GatewayConfig::default(),
    ));
    let mut platform = PlatformConfig::default();
    platform
        .extra
        .insert("host".into(), serde_json::json!("127.0.0.1"));
    platform.extra.insert("port".into(), serde_json::json!(0));
    platform
        .extra
        .insert("path".into(), serde_json::json!("/survey"));
    platform
        .extra
        .insert("rate_limit".into(), serde_json::json!(12));
    platform
        .extra
        .insert("max_body_bytes".into(), serde_json::json!(2048));
    platform.extra.insert(
        "routes".into(),
        serde_json::json!({"report": {"chat_id": "survey-chat"}}),
    );
    let config = build_webhook_config(&platform, "fixture-webhook-value".into());
    assert_eq!(config.host, "127.0.0.1");
    assert_eq!(config.port, 0);
    assert_eq!(config.path, "/survey");
    assert_eq!(config.rate_limit, 12);
    assert_eq!(config.max_body_bytes, 2048);
    assert_eq!(
        config.routes["report"].chat_id.as_deref(),
        Some("survey-chat")
    );
    let adapter = Arc::new(WebhookAdapter::new(config));
    gateway.register_adapter("webhook", adapter.clone()).await;
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observations = observed.clone();
    gateway
        .set_message_handler_with_context(Arc::new(move |messages, context| {
            let observations = observations.clone();
            Box::pin(async move {
                assert_eq!(context.platform, "webhook");
                assert!(context.thread_id.is_none());
                let text = messages.last().unwrap().content.as_deref().unwrap();
                observations.lock().unwrap().push(text.to_owned());
                if text == "First count" {
                    return Err(hermes_gateway::GatewayError::Platform(
                        "fixture failure".into(),
                    ));
                }
                Ok(format!("{} received {text}", context.user_id))
            })
        }))
        .await;

    let (send, recv) = mpsc::channel(3);
    for (chat, user, text) in [
        ("survey-chat", Some("fixture-sender"), "First count"),
        ("survey-chat", Some("fixture-sender"), "Bird count"),
        ("weather-chat", None, "Rain count"),
    ] {
        send.send(WebhookPayload {
            chat_id: chat.into(),
            user_id: user.map(String::from),
            text: text.into(),
            metadata: serde_json::Value::Null,
        })
        .await
        .unwrap();
    }
    drop(send);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_webhook_inbound_loop(gateway.clone(), recv),
    )
    .await
    .expect("the webhook loop drains its closed channel");
    assert_eq!(
        std::mem::take(&mut *observed.lock().unwrap()),
        ["First count", "Bird count", "Rain count"]
    );

    let replies = adapter.drain_outbound().await;
    assert_eq!(replies.len(), 3);
    assert_eq!(replies[0].chat_id, "survey-chat");
    assert!(replies[0].text.contains("Platform error: fixture failure"));
    let replies: Vec<_> = replies
        .iter()
        .skip(1)
        .map(|reply| (reply.chat_id.as_str(), reply.text.as_str()))
        .collect();
    assert_eq!(
        replies,
        [
            ("survey-chat", "fixture-sender received Bird count"),
            ("weather-chat", "webhook-client received Rain count"),
        ]
    );

    #[cfg(any(
        feature = "gateway-dingtalk",
        feature = "gateway-homeassistant",
        feature = "gateway-ntfy",
        feature = "gateway-weixin",
    ))]
    {
        let (send, recv) = mpsc::channel(2);
        for (id, text) in [("first", "First count"), ("second", "Cloud count")] {
            send.send(GatewayIncomingMessage {
                platform: "webhook".into(),
                chat_id: "weather-chat".into(),
                user_id: "fixture-sender".into(),
                text: text.into(),
                message_id: Some(id.into()),
                thread_id: None,
                is_dm: true,
            })
            .await
            .unwrap();
        }
        drop(send);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_gateway_incoming_loop(gateway, recv, "webhook"),
        )
        .await
        .expect("the shared inbound loop drains its closed channel");
        assert_eq!(*observed.lock().unwrap(), ["First count", "Cloud count"]);

        let replies = adapter.drain_outbound().await;
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0].chat_id, "weather-chat");
        assert!(replies[0].text.contains("Platform error: fixture failure"));
        assert_eq!(replies[1].chat_id, "weather-chat");
        assert_eq!(replies[1].text, "fixture-sender received Cloud count");
    }
}
