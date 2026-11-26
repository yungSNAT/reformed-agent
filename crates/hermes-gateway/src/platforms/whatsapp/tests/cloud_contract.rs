use super::*;
use serde_json::json;

fn loopback_adapter(settings: WhatsAppConfig) -> WhatsAppAdapter {
    let mut adapter = WhatsAppAdapter::new(settings).unwrap();
    adapter.client = Client::builder().no_proxy().build().unwrap();
    adapter
}

#[test]
fn webhook_checks_the_subscription_and_extracts_messages_across_changes() {
    for (mode, token, expected) in [
        (
            "subscribe",
            "fixture-verify",
            Some("challenge-4".to_string()),
        ),
        ("subscribe", "wrong", None),
        ("unsubscribe", "fixture-verify", None),
    ] {
        assert_eq!(
            WhatsAppAdapter::verify_webhook(mode, token, "challenge-4", "fixture-verify"),
            expected
        );
    }
    assert!(WhatsAppAdapter::parse_webhook_event(&json!({"entry":{}})).is_empty());

    let events = WhatsAppAdapter::parse_webhook_event(&json!({"entry":[
        {"changes":null},
        {"changes":[
            {"field":"messages"},
            {"value":{"statuses":[{"id":"ignored"}]}},
            {"value":{"messages":[
                {"id":"wamid.text","from":"15551234567","type":"text","timestamp":"123",
                 "text":{"body":"Hello"}},
                {"id":"wamid.image","from":"15551234568","type":"image","timestamp":"124",
                 "image":{"id":"file"}}
            ]}}
        ]},
        {"changes":[{"value":{"messages":[{"text":{"body":"Later"}}]}}]}
    ]}));
    let actual: Vec<_> = events
        .iter()
        .map(|event| {
            (
                event.message_id.as_str(),
                event.from.as_str(),
                event.message_type.as_str(),
                event.timestamp.as_str(),
                event.text.as_str(),
            )
        })
        .collect();
    assert_eq!(
        actual,
        [
            ("wamid.text", "15551234567", "text", "123", "Hello"),
            ("wamid.image", "15551234568", "image", "124", ""),
            ("", "", "text", "", "Later"),
        ]
    );
}

#[tokio::test]
async fn cloud_requests_use_the_encoded_phone_id_and_normalised_recipient() {
    let server = MockServer::start().await;
    let adapter = loopback_adapter(WhatsAppConfig {
        phone_number_id: Some("phone/1".into()),
        api_base_url: Some(format!("{}/", server.uri())),
        reply_prefix: Some("Agent\n".into()),
        ..config()
    });
    let first_chunk = "é".repeat(MAX_WHATSAPP_MESSAGE_LENGTH);
    let long_message = format!("{first_chunk}next");
    for body in [
        json!({"messaging_product":"whatsapp","to":"15551234567","type":"text",
            "text":{"body":"Agent"}}),
        json!({"messaging_product":"whatsapp","to":"15551234567","type":"text",
            "text":{"body":first_chunk}}),
        json!({"messaging_product":"whatsapp","to":"15551234567","type":"text",
            "text":{"body":"next"}}),
        json!({"messaging_product":"whatsapp","status":"read","message_id":"wamid.inbound"}),
        json!({"messaging_product":"whatsapp","to":"15551234567","type":"reaction",
            "reaction":{"message_id":"wamid.inbound","emoji":"👍"}}),
        json!({"messaging_product":"whatsapp","to":"15551234567","type":"image",
            "image":{"link":"https://example.invalid/photo.png","caption":"A photo"}}),
    ] {
        Mock::given(method("POST"))
            .and(path("/phone%2F1/messages"))
            .and(header("authorization", "Bearer token"))
            .and(body_json(body))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }

    assert!(!adapter.is_running());
    adapter.start().await.unwrap();
    assert!(adapter.is_running());
    assert!(adapter.splits_long_messages());
    assert_eq!(adapter.platform_name(), "whatsapp");
    assert_eq!(adapter.config().phone_number_id.as_deref(), Some("phone/1"));
    let recipient = "+1 (555) 123-4567";
    adapter
        .send_message(recipient, &long_message, None)
        .await
        .unwrap();
    adapter.mark_as_read("wamid.inbound").await.unwrap();
    adapter
        .send_reaction(recipient, "wamid.inbound", "👍")
        .await
        .unwrap();
    adapter
        .send_image_url(
            recipient,
            "https://example.invalid/photo.png",
            Some(" A photo "),
        )
        .await
        .unwrap();
    adapter
        .edit_message(recipient, "wamid.inbound", "Ignored")
        .await
        .unwrap();
    adapter.stop().await.unwrap();
    assert!(!adapter.is_running());

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 6);
    let text_chunks: Vec<String> = requests[..3]
        .iter()
        .map(|request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            body["text"]["body"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(
        text_chunks,
        ["Agent".to_string(), first_chunk, "next".to_string()]
    );
    server.verify().await;
}

#[tokio::test]
async fn cloud_errors_include_the_response_body_and_missing_phone_id_stays_offline() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/phone-1/messages"))
        .respond_with(ResponseTemplate::new(403).set_body_string("fixture request denied"))
        .expect(5)
        .mount(&server)
        .await;

    let mut settings = WhatsAppConfig {
        api_base_url: Some(server.uri()),
        ..config()
    };
    let adapter = loopback_adapter(settings.clone());
    let recipient = "15551234567";
    let image_url = "https://example.invalid/photo.png";
    let first_chunk = "a".repeat(MAX_WHATSAPP_MESSAGE_LENGTH);
    let result = adapter
        .send_message(recipient, &format!("{first_chunk}next"), None)
        .await;
    assert!(matches!(result, Err(GatewayError::SendFailed(message))
        if message == "WhatsApp API error: fixture request denied"));
    let requests = server.received_requests().await.unwrap();
    let [request] = requests.as_slice() else {
        panic!("expected the first rejected chunk only");
    };
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["text"]["body"], first_chunk);

    for (result, operation) in [
        (
            adapter.send_text(recipient, "Hello").await,
            "WhatsApp API error",
        ),
        (
            adapter.mark_as_read("inbound").await,
            "WhatsApp mark_as_read error",
        ),
        (
            adapter.send_reaction(recipient, "inbound", "👍").await,
            "WhatsApp reaction error",
        ),
        (
            adapter
                .send_media(recipient, "image", image_url, None)
                .await,
            "WhatsApp API error",
        ),
    ] {
        assert!(matches!(result, Err(GatewayError::SendFailed(message))
            if message == format!("{operation}: fixture request denied")));
    }

    settings.phone_number_id = None;
    let adapter = loopback_adapter(settings);
    for result in [
        adapter.send_text(recipient, "Hello").await,
        adapter.mark_as_read("inbound").await,
        adapter.send_reaction(recipient, "inbound", "👍").await,
        adapter
            .send_media(recipient, "image", image_url, None)
            .await,
    ] {
        assert!(matches!(result, Err(GatewayError::SendFailed(message))
            if message == "phone_number_id not configured"));
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
    server.verify().await;
}
