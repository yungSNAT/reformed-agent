#[tokio::test]
async fn expired_tenant_token_triggers_refresh() {
    let adapter = adapter_without_token();
    if let Some(old) = Instant::now().checked_sub(Duration::from_secs(TOKEN_EXPIRY_SECS)) {
        *adapter.tenant_token.write().await = Some(CachedToken {
            value: "old".into(),
            obtained_at: old,
        });
    }
    match adapter.get_tenant_token().await {
        Err(GatewayError::Auth(m)) => assert!(m.contains("Feishu auth failed"), "{m}"),
        other => panic!("expected auth error, got {other:?}"),
    }
}

#[tokio::test]
async fn messaging_requests_report_transport_errors() {
    let adapter = adapter_with_token().await;
    let card = FeishuCard::new()
        .with_header("T", Some("blue"))
        .add_markdown("**x**");
    let post = vec![vec![PostElement::text("hello")]];

    assert_send_failed(
        adapter.send_text("oc_1", " hi ").await.unwrap_err(),
        "Feishu send failed",
    );
    assert_send_failed(
        adapter.edit_text("om_1", "hi").await.unwrap_err(),
        "Feishu edit failed",
    );
    assert_send_failed(
        adapter
            .send_post("oc_1", "T", post.clone())
            .await
            .unwrap_err(),
        "Feishu send_post failed",
    );
    assert_send_failed(
        adapter.send_card("oc_1", &card).await.unwrap_err(),
        "Feishu send_card failed",
    );
    assert_send_failed(
        adapter.update_card("om_1", &card).await.unwrap_err(),
        "Feishu update_card failed",
    );
    assert_send_failed(
        adapter
            .send_image_by_key("oc_1", "img_1")
            .await
            .unwrap_err(),
        "Feishu send_image failed",
    );
    assert_send_failed(
        adapter.reply_message("om_1", "hi").await.unwrap_err(),
        "Feishu reply failed",
    );
    assert_send_failed(
        adapter.reply_post("om_1", "T", post).await.unwrap_err(),
        "Feishu reply_post failed",
    );
    assert_send_failed(
        adapter.reply_card("om_1", &card).await.unwrap_err(),
        "Feishu reply_card failed",
    );
    assert_send_failed(
        adapter.add_reaction("om_1", "THUMBSUP").await.unwrap_err(),
        "Feishu add_reaction failed",
    );
    assert_send_failed(
        adapter.delete_message("om_1").await.unwrap_err(),
        "Feishu delete_message failed",
    );
    assert_platform(
        adapter.get_chat_info("oc_1").await.unwrap_err(),
        "Feishu get_chat_info failed",
    );
    assert_platform(
        adapter.get_message("om_1").await.unwrap_err(),
        "Feishu get_message failed",
    );
}

#[tokio::test]
async fn image_and_audio_uploads_report_errors() {
    let adapter = adapter_with_token().await;
    let dir = tempfile::tempdir().expect("tempdir");

    assert_send_failed(
        adapter
            .upload_image(vec![1, 2, 3], "pic.png")
            .await
            .unwrap_err(),
        "Feishu image upload failed",
    );

    let missing = dir.path().join("missing.png");
    assert_send_failed(
        adapter
            .send_image("oc_1", missing.to_str().expect("utf8"))
            .await
            .unwrap_err(),
        "Failed to read image",
    );

    let img = dir.path().join("real.png");
    std::fs::write(&img, [0u8, 1, 2, 3]).expect("write");
    assert_send_failed(
        adapter
            .send_image("oc_1", img.to_str().expect("utf8"))
            .await
            .unwrap_err(),
        "Feishu image upload failed",
    );

    let missing_audio = dir.path().join("missing.opus");
    assert_send_failed(
        adapter
            .send_audio("oc_1", missing_audio.to_str().expect("utf8"), None)
            .await
            .unwrap_err(),
        "Failed to read audio file",
    );

    let audio = dir.path().join("voice.opus");
    std::fs::write(&audio, [9u8, 8, 7]).expect("write");
    assert_send_failed(
        adapter
            .send_audio("oc_1", audio.to_str().expect("utf8"), Some(1500))
            .await
            .unwrap_err(),
        "Feishu audio upload failed",
    );
}

#[tokio::test]
async fn check_api_response_variants() {
    let server = MockServer::start().await;
    Mock::given(path("/ok"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"code": 0, "data": {"message_id": "om_42"}})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/nodata"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code": 0})))
        .mount(&server)
        .await;
    Mock::given(path("/coded"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"code": 99, "msg": "bad things"})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/garbage"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    Mock::given(path("/fail"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let client = no_proxy_client();
    let get = |p: &str| client.get(format!("{}{}", server.uri(), p)).send();

    let resp = get("/ok").await.expect("resp");
    assert_eq!(
        FeishuAdapter::check_api_response("send_text", resp)
            .await
            .expect("ok"),
        "om_42"
    );

    let resp = get("/nodata").await.expect("resp");
    assert_eq!(
        FeishuAdapter::check_api_response("send_text", resp)
            .await
            .expect("ok"),
        ""
    );

    let resp = get("/coded").await.expect("resp");
    assert_send_failed(
        FeishuAdapter::check_api_response("send_post", resp)
            .await
            .unwrap_err(),
        "Feishu send_post error (code=99): bad things",
    );

    let resp = get("/garbage").await.expect("resp");
    assert_send_failed(
        FeishuAdapter::check_api_response("send_card", resp)
            .await
            .unwrap_err(),
        "Feishu send_card parse failed",
    );

    let resp = get("/fail").await.expect("resp");
    assert_send_failed(
        FeishuAdapter::check_api_response("reply_card", resp)
            .await
            .unwrap_err(),
        "Feishu reply_card API error: boom",
    );
}

#[tokio::test]
async fn trait_start_stop_and_metadata() {
    let adapter = adapter_with_token().await;
    assert_eq!(adapter.platform_name(), "feishu");
    assert!(!adapter.is_running());

    adapter.start().await.expect("start with cached token");
    assert!(adapter.is_running());

    adapter.stop().await.expect("stop");
    assert!(!adapter.is_running());
    assert!(adapter.get_tenant_token().await.is_err());
}

#[tokio::test]
async fn trait_message_methods_propagate_errors() {
    let adapter = adapter_with_token().await;

    assert_send_failed(
        adapter
            .send_message("oc_1", "hello", Some(ParseMode::Plain))
            .await
            .unwrap_err(),
        "Feishu send failed",
    );
    assert_send_failed(
        adapter
            .edit_message("oc_1", "om_1", "edited")
            .await
            .unwrap_err(),
        "Feishu edit failed",
    );
}

#[tokio::test]
async fn trait_send_file_paths() {
    let adapter = adapter_with_token().await;
    let dir = tempfile::tempdir().expect("tempdir");

    let missing = dir.path().join("nope.txt");
    assert_send_failed(
        adapter
            .send_file("oc_1", missing.to_str().expect("utf8"), None)
            .await
            .unwrap_err(),
        "Failed to read file",
    );

    let doc = dir.path().join("notes.txt");
    std::fs::write(&doc, "hello").expect("write");
    let err = adapter
        .send_file("oc_1", doc.to_str().expect("utf8"), Some("cap"))
        .await
        .unwrap_err();
    assert_send_failed(err, "failed");

    let img = dir.path().join("pic.png");
    std::fs::write(&img, [1u8, 2, 3]).expect("write");
    assert_send_failed(
        adapter
            .send_file("oc_1", img.to_str().expect("utf8"), None)
            .await
            .unwrap_err(),
        "Feishu image upload failed",
    );
}

#[tokio::test]
async fn trait_send_image_url_falls_back_and_errors() {
    let adapter = adapter_with_token().await;
    let url = closed_local_url();
    let result = adapter.send_image_url("oc_1", &url, Some("caption")).await;
    assert!(result.is_err());
}
