#[tokio::test]
async fn multipart_media_delivery_preserves_attachment_fields_and_caption_limits() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {"message_id": 456}
        })))
        .expect(8)
        .mount(&server)
        .await;
    let mut adapter = test_adapter(test_config());
    adapter.api_base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("fixture.txt");
    std::fs::write(&file, b"fixture attachment").unwrap();
    let file = file.to_str().unwrap();
    let long_caption = format!("  {}  ", "é".repeat(MAX_CAPTION_LENGTH + 2));

    let sent = [
        adapter
            .send_document_with_options("123:73", file, Some(&long_caption), Some(17), Some(74))
            .await
            .unwrap(),
        adapter.send_photo("123", file, Some("  ")).await.unwrap(),
        adapter.send_audio("123", file, None).await.unwrap(),
        adapter.send_video("123", file, None).await.unwrap(),
        adapter.send_voice("123", file, None).await.unwrap(),
        adapter.send_animation("123", file, None).await.unwrap(),
        adapter.send_sticker("123", file).await.unwrap(),
        adapter
            .send_sticker_by_id("123", "fixture-sticker-id")
            .await
            .unwrap(),
    ];
    assert_eq!(sent, [456; 8]);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 8);
    let endpoints = [
        ("/sendDocument", "document"),
        ("/sendPhoto", "photo"),
        ("/sendAudio", "audio"),
        ("/sendVideo", "video"),
        ("/sendVoice", "voice"),
        ("/sendAnimation", "animation"),
        ("/sendSticker", "sticker"),
    ];
    let caption = "é".repeat(MAX_CAPTION_LENGTH);
    for (request, (endpoint, field)) in requests.iter().zip(endpoints) {
        assert_eq!(request.url.path(), endpoint);
        let body = std::str::from_utf8(&request.body).unwrap();
        assert!(body.contains("name=\"chat_id\"\r\n\r\n123\r\n"));
        assert!(body.contains(&format!("name=\"{field}\"; filename=\"fixture.txt\"")));
        assert!(body.contains("\r\n\r\nfixture attachment\r\n"));
        if endpoint == "/sendDocument" {
            assert!(body.contains(&format!("name=\"caption\"\r\n\r\n{caption}\r\n")));
            assert!(body.contains("name=\"reply_to_message_id\"\r\n\r\n17\r\n"));
            assert!(body.contains("name=\"message_thread_id\"\r\n\r\n74\r\n"));
        } else {
            assert!(!body.contains("name=\"caption\""));
            assert!(!body.contains("name=\"reply_to_message_id\""));
            assert!(!body.contains("name=\"message_thread_id\""));
        }
    }
    let sticker = &requests[7];
    assert_eq!(sticker.url.path(), "/sendSticker");
    assert_eq!(
        sticker.body_json::<Value>().unwrap(),
        serde_json::json!({"chat_id": "123", "sticker": "fixture-sticker-id"})
    );
}

#[tokio::test]
async fn multipart_media_delivery_retries_stale_topic_once_and_reports_other_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/sendPhoto"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "ok": false,
            "description": "Bad Request: message thread not found"
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sendPhoto"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {"message_id": 567}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sendAudio"))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "ok": false,
            "parameters": {"retry_after": 7}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sendVideo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .expect(1)
        .mount(&server)
        .await;
    let mut adapter = test_adapter(test_config());
    adapter.api_base = server.uri();
    adapter.bind_dm_topic("123", "73", "fixture-session", "fixture-user", None, false);
    assert!(adapter.dm_topic_binding("123", "73").is_some());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("fixture.txt");
    std::fs::write(&file, b"fixture attachment").unwrap();
    let file = file.to_str().unwrap();

    let id = adapter
        .send_photo_with_options("123:73", file, Some("keep caption"), Some(17), None)
        .await
        .unwrap();
    assert_eq!(id, 567);
    assert!(adapter.dm_topic_binding("123", "73").is_none());
    assert!(matches!(
        adapter.send_audio("123", file, None).await,
        Err(GatewayError::RateLimited {
            retry_after_secs: Some(7)
        })
    ));
    assert!(matches!(
        adapter.send_video("123", file, None).await,
        Err(GatewayError::SendFailed(message)) if message == "sendVideo returned no message"
    ));
    let missing = dir.path().join("missing.txt");
    assert!(matches!(
        adapter.send_document("123", missing.to_str().unwrap(), None).await,
        Err(GatewayError::SendFailed(message)) if message.starts_with("Failed to read file")
    ));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    let first = std::str::from_utf8(&requests[0].body).unwrap();
    let retry = std::str::from_utf8(&requests[1].body).unwrap();
    assert!(first.contains("name=\"message_thread_id\"\r\n\r\n73\r\n"));
    assert!(first.contains("name=\"reply_to_message_id\"\r\n\r\n17\r\n"));
    assert!(!retry.contains("name=\"message_thread_id\""));
    assert!(!retry.contains("name=\"reply_to_message_id\""));
    for body in [first, retry] {
        assert!(body.contains("name=\"chat_id\"\r\n\r\n123\r\n"));
        assert!(body.contains("name=\"caption\"\r\n\r\nkeep caption\r\n"));
        assert!(body.contains("\r\n\r\nfixture attachment\r\n"));
    }
}
