use super::*;
use crate::platforms::telegram::ChatKind;

fn telegram_text_fixture() -> IncomingMessage {
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

    let direct = telegram_gateway_message(IncomingMessage {
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
            IncomingMessage {
                is_voice: true,
                ..fixture.clone()
            },
            "[voice message]",
        ),
        (
            IncomingMessage {
                is_photo: true,
                ..fixture.clone()
            },
            "[photo message]",
        ),
        (
            IncomingMessage {
                is_video: true,
                ..fixture.clone()
            },
            "[video message]",
        ),
        (
            IncomingMessage {
                is_sticker: true,
                ..fixture.clone()
            },
            "[unsupported message]",
        ),
        (
            IncomingMessage {
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
    assert!(!telegram_should_batch_text(&IncomingMessage {
        callback_query_id: Some("fixture-query".into()),
        ..fixture.clone()
    }));
    assert!(!telegram_should_batch_text(&IncomingMessage {
        callback_data: Some("fixture-data".into()),
        ..fixture.clone()
    }));
    assert!(!telegram_should_batch_text(&IncomingMessage {
        text: Some("   ".into()),
        ..fixture
    }));
}

#[test]
fn telegram_gateway_message_preserves_group_topic_in_chat_id() {
    let incoming = IncomingMessage {
        chat_id: -1001,
        user_id: Some(42),
        username: Some("alice".to_string()),
        text: Some("topic hello".to_string()),
        message_id: 77,
        is_voice: false,
        is_photo: false,
        is_sticker: false,
        is_document: false,
        is_video: false,
        voice_file_id: None,
        photo_file_id: None,
        sticker_file_id: None,
        document_file_id: None,
        document_file_name: None,
        document_mime_type: None,
        document_file_size: None,
        video_file_id: None,
        video_file_name: None,
        video_mime_type: None,
        video_file_size: None,
        reply_to_message_id: None,
        message_thread_id: Some(17585),
        chat_type: ChatKind::Supergroup,
        is_group: true,
        callback_query_id: None,
        callback_data: None,
    };

    let routed = telegram_gateway_message(incoming);
    assert_eq!(routed.chat_id, "-1001:17585");
    assert_eq!(routed.user_id, "42");
    assert!(!routed.is_dm);
}

#[test]
fn telegram_gateway_message_preserves_private_topic_in_chat_id() {
    let incoming = IncomingMessage {
        chat_id: 208214988,
        user_id: Some(42),
        username: Some("alice".to_string()),
        text: Some("topic hello".to_string()),
        message_id: 77,
        is_voice: false,
        is_photo: false,
        is_sticker: false,
        is_document: false,
        is_video: false,
        voice_file_id: None,
        photo_file_id: None,
        sticker_file_id: None,
        document_file_id: None,
        document_file_name: None,
        document_mime_type: None,
        document_file_size: None,
        video_file_id: None,
        video_file_name: None,
        video_mime_type: None,
        video_file_size: None,
        reply_to_message_id: None,
        message_thread_id: Some(17585),
        chat_type: ChatKind::Private,
        is_group: false,
        callback_query_id: None,
        callback_data: None,
    };

    let routed = telegram_gateway_message(incoming);
    assert_eq!(routed.chat_id, "208214988:17585");
    assert_eq!(routed.user_id, "42");
    assert!(routed.is_dm);
}

#[test]
fn telegram_gateway_message_treats_general_topic_as_root_lobby() {
    let incoming = IncomingMessage {
        chat_id: 208214988,
        user_id: Some(42),
        username: Some("alice".to_string()),
        text: Some("root lobby".to_string()),
        message_id: 77,
        is_voice: false,
        is_photo: false,
        is_sticker: false,
        is_document: false,
        is_video: false,
        voice_file_id: None,
        photo_file_id: None,
        sticker_file_id: None,
        document_file_id: None,
        document_file_name: None,
        document_mime_type: None,
        document_file_size: None,
        video_file_id: None,
        video_file_name: None,
        video_mime_type: None,
        video_file_size: None,
        reply_to_message_id: None,
        message_thread_id: Some(1),
        chat_type: ChatKind::Private,
        is_group: false,
        callback_query_id: None,
        callback_data: None,
    };

    let routed = telegram_gateway_message(incoming);
    assert_eq!(routed.chat_id, "208214988");
    assert!(routed.is_dm);
}

#[test]
fn telegram_polling_pause_threshold_matches_reconnect_policy() {
    assert!(!telegram_polling_should_pause_for_reconnect(0));
    assert!(!telegram_polling_should_pause_for_reconnect(
        TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD - 1
    ));
    assert!(telegram_polling_should_pause_for_reconnect(
        TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD
    ));
    assert!(telegram_polling_should_pause_for_reconnect(
        TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD + 1
    ));
}

#[tokio::test]
async fn gateway_polling_batches_updates_and_pauses_after_repeated_failures() {
    use std::sync::Arc;

    use hermes_config::session::SessionConfig;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::dm::DmManager;
    use crate::gateway::GatewayConfig;
    use crate::platforms::telegram::TelegramConfig;
    use crate::SessionManager;

    const CHILD_HOME: &str = "HERMES_TELEGRAM_POLL_TEST_HOME";
    if std::env::var_os(CHILD_HOME).is_none() {
        let profile = tempfile::tempdir().expect("fixture home");
        let mut child = tokio::process::Command::new(std::env::current_exe().expect("test binary"));
        child
            .env_clear()
            .env(CHILD_HOME, profile.path())
            .env("HOME", profile.path())
            .env("USERPROFILE", profile.path())
            .env("HERMES_HOME", profile.path())
            .env("HERMES_MANAGED_DIR", profile.path().join("managed"))
            .envs(Some("SystemRoot").zip(std::env::var_os("SystemRoot")))
            .envs(Some("LLVM_PROFILE_FILE").zip(std::env::var_os("LLVM_PROFILE_FILE")))
            .kill_on_drop(true)
            .args([
                "--exact",
                "platforms::telegram::gateway_polling::tests::gateway_polling_batches_updates_and_pauses_after_repeated_failures",
            ]);
        let output = tokio::time::timeout(Duration::from_secs(20), child.output())
            .await
            .expect("isolated polling test timeout")
            .expect("isolated polling test");
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
            "{output:?}"
        );
        return;
    }

    let server = MockServer::start().await;
    let unavailable = ResponseTemplate::new(503)
        .set_body_json(json!({"ok": false, "description": "Fixture unavailable"}));
    Mock::given(method("POST"))
        .and(path("/deleteWebhook"))
        .and(body_partial_json(json!({"drop_pending_updates": false})))
        .respond_with(unavailable.clone())
        .expect(1)
        .mount(&server)
        .await;

    let user = |id| json!({"id": id, "is_bot": false, "first_name": "Fixture"});
    let update = |id, sender, text| {
        json!({"update_id": id, "message": {
            "message_id": id, "date": 0, "chat": {"id": sender, "type": "private"},
            "from": user(sender), "text": text
        }})
    };
    Mock::given(method("POST"))
        .and(path("/getUpdates"))
        .and(body_partial_json(json!({"offset": 0})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "result": [
            update(10, 42, " First update "),
            update(11, 42, "Second update"),
            json!({"update_id": 12}),
            update(13, 7, "Ignored update"),
            json!({"update_id": 14, "message": {
                "message_id": 14, "date": 0, "chat": {"id": 42, "type": "private"}, "from": user(42),
                "photo": [{"file_id": "fixture-photo", "file_unique_id": "fixture-unique", "width": 1, "height": 1}]
            }}),
            json!({"update_id": 15, "callback_query": {
                "id": "expired-card", "from": user(42), "data": "approval:deny:42"
            }})
        ]})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/getUpdates"))
        .and(body_partial_json(json!({"offset": 16})))
        .respond_with(unavailable.clone())
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/answerCallbackQuery"))
        .and(body_partial_json(
            json!({"callback_query_id": "expired-card"}),
        ))
        .respond_with(unavailable)
        .expect(1)
        .mount(&server)
        .await;
    for reply in [
        "Received [photo message]",
        "Received First update\nSecond update",
    ] {
        Mock::given(method("POST"))
            .and(path("/sendMessage"))
            .and(body_partial_json(json!({"chat_id": "42", "text": reply})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true, "result": {"message_id": 99}
            })))
            .expect(1)
            .mount(&server)
            .await;
    }

    let config: TelegramConfig = serde_json::from_value(json!({
        "token": "fixture", "allowed_users": ["42"], "text_batch_delay_ms": 10,
        "command_menu_enabled": false, "polling": false
    }))
    .expect("fixture bot config");
    let mut adapter = TelegramAdapter::new(config).expect("fixture bot");
    adapter.api_base = server.uri();
    adapter.client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("fixture HTTP client");

    let mut dm = DmManager::with_ignore_behavior();
    dm.authorize_user_for_platform("telegram", "42");
    let gateway = Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        dm,
        GatewayConfig::default(),
    );
    adapter.start().await.expect("start fixture bot");
    assert!(
        tokio::time::timeout(Duration::from_millis(25), adapter.poll_gateway(&gateway))
            .await
            .is_err()
    );
    assert!(server
        .received_requests()
        .await
        .expect("recorded HTTP requests")
        .is_empty());

    adapter.config.polling = true;
    let adapter = Arc::new(adapter);
    gateway.register_adapter("telegram", adapter.clone()).await;
    gateway
        .set_message_handler_with_context(Arc::new(|messages, context| {
            Box::pin(async move {
                assert_eq!(context.platform, "telegram");
                assert_eq!(context.chat_id, "42");
                assert_eq!(context.user_id, "42");
                let text = messages
                    .last()
                    .and_then(|message| message.content.as_deref())
                    .expect("incoming Telegram text");
                Ok(format!("Received {text}"))
            })
        }))
        .await;

    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            biased;
            () = adapter.poll_gateway(&gateway) => panic!("polling loop returned"),
            () = async {
                while adapter.is_running() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            } => {}
        }
    })
    .await
    .expect("polling should pause at the reconnect threshold");
    assert_eq!(adapter.consecutive_error_count(), 3);
    server.verify().await;
}
