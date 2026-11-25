fn telegram_text_fixture() -> TelegramIncomingMessage {
    let update = serde_json::from_value(serde_json::json!({
        "update_id": 42,
        "message": {
            "message_id": 7,
            "message_thread_id": 9,
            "date": 0,
            "chat": {"id": -10001, "type": "supergroup"},
            "from": {"id": 23, "is_bot": false, "first_name": "Fixture", "username": "fixture"},
            "text": "  Topic message  "
        }
    }))
    .unwrap();
    TelegramAdapter::parse_update(&update).expect("valid fixture update")
}

#[test]
fn telegram_inbound_conversion_keeps_topic_and_sender_identity() {
    let fixture = telegram_text_fixture();
    assert!(telegram_should_batch_text(&fixture));
    let routed = telegram_gateway_message(fixture.clone());
    assert_eq!(routed.platform, "telegram");
    assert_eq!(routed.chat_id, "-10001:9");
    assert_eq!(routed.thread_id.as_deref(), Some("9"));
    assert_eq!(routed.user_id, "23");
    assert_eq!(routed.text, "  Topic message  ");
    assert_eq!(routed.message_id.as_deref(), Some("7"));
    assert!(!routed.is_dm);

    let direct = telegram_gateway_message(TelegramIncomingMessage {
        chat_id: 42,
        message_thread_id: Some(1),
        user_id: None,
        ..fixture
    });
    assert_eq!(direct.chat_id, "42");
    assert!(direct.thread_id.is_none());
    assert_eq!(direct.user_id, "fixture");
    assert!(direct.is_dm);
}

#[test]
fn telegram_inbound_media_and_callbacks_are_never_text_batched() {
    let fixture = telegram_text_fixture();
    let media = [
        (
            TelegramIncomingMessage {
                is_voice: true,
                ..fixture.clone()
            },
            "[voice message]",
        ),
        (
            TelegramIncomingMessage {
                is_photo: true,
                ..fixture.clone()
            },
            "[photo message]",
        ),
        (
            TelegramIncomingMessage {
                is_video: true,
                ..fixture.clone()
            },
            "[video message]",
        ),
        (
            TelegramIncomingMessage {
                is_sticker: true,
                ..fixture.clone()
            },
            "[unsupported message]",
        ),
        (
            TelegramIncomingMessage {
                is_document: true,
                ..fixture.clone()
            },
            "[unsupported message]",
        ),
    ];
    for (mut message, placeholder) in media {
        assert!(!telegram_should_batch_text(&message));
        message.text = None;
        message.user_id = None;
        message.username = None;
        let routed = telegram_gateway_message(message);
        assert_eq!(routed.text, placeholder);
        assert_eq!(routed.user_id, "unknown");
    }
    assert!(!telegram_should_batch_text(&TelegramIncomingMessage {
        callback_query_id: Some("fixture-query".into()),
        ..fixture.clone()
    }));
    assert!(!telegram_should_batch_text(&TelegramIncomingMessage {
        callback_data: Some("fixture-data".into()),
        ..fixture.clone()
    }));
    assert!(!telegram_should_batch_text(&TelegramIncomingMessage {
        text: Some("   ".into()),
        ..fixture
    }));
}
