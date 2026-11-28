#[test]
fn guess_chat_variants() {
    let acc = "bot-acc";
    let m = serde_json::json!({ "room_id": "r1", "from_user_id": "u" });
    assert_eq!(
        WeChatAdapter::guess_chat(&m, acc),
        ("group", "r1".to_string())
    );
    let m = serde_json::json!({ "chat_room_id": "r2", "from_user_id": "u" });
    assert_eq!(
        WeChatAdapter::guess_chat(&m, acc),
        ("group", "r2".to_string())
    );
    let m = serde_json::json!({ "to_user_id": "t1", "from_user_id": "u", "msg_type": 1 });
    assert_eq!(
        WeChatAdapter::guess_chat(&m, acc),
        ("group", "t1".to_string())
    );
    let m = serde_json::json!({ "to_user_id": "bot-acc", "from_user_id": "u", "msg_type": 1 });
    assert_eq!(WeChatAdapter::guess_chat(&m, acc), ("dm", "u".to_string()));
    let m = serde_json::json!({ "to_user_id": "t1", "from_user_id": "u", "msg_type": 2 });
    assert_eq!(WeChatAdapter::guess_chat(&m, acc), ("dm", "u".to_string()));
}

#[test]
fn extract_text_variants() {
    let items = vec![
        serde_json::json!({ "type": 2, "image_item": {} }),
        serde_json::json!({ "type": 1 }),
        serde_json::json!({ "type": 1, "text_item": { "text": "hello" } }),
    ];
    assert_eq!(WeChatAdapter::extract_text(&items), "hello");
    let items = vec![serde_json::json!({ "type": 2 })];
    assert_eq!(WeChatAdapter::extract_text(&items), "");
    assert_eq!(WeChatAdapter::extract_text(&[]), "");
}

#[tokio::test]
async fn is_dup_tracks_ids() {
    let a = make_adapter("https://ilink.invalid");
    assert!(!WeChatAdapter::is_dup(&a.inner, "").await);
    assert!(!WeChatAdapter::is_dup(&a.inner, "").await);
    assert!(!WeChatAdapter::is_dup(&a.inner, "m1").await);
    assert!(WeChatAdapter::is_dup(&a.inner, "m1").await);
    assert!(!WeChatAdapter::is_dup(&a.inner, "m2").await);
}

#[tokio::test]
async fn process_inbound_dm_open_delivers() {
    let msg = serde_json::json!({
        "from_user_id": "u1",
        "message_id": "m1",
        "item_list": [
            { "type": 1, "text_item": { "text": "hi" } },
            { "type": 2, "image_item": { "media": {} } }
        ]
    });
    let m = deliver(test_config("https://ilink.invalid"), msg)
        .await
        .expect("message delivered");
    assert_eq!(m.platform, "weixin");
    assert_eq!(m.chat_id, "u1");
    assert_eq!(m.user_id, "u1");
    assert_eq!(m.text, "hi");
    assert_eq!(m.message_id.as_deref(), Some("m1"));
    assert!(m.thread_id.is_none());
    assert!(m.is_dm);

    let msg = serde_json::json!({ "from_user_id": "u2", "item_list": text_items("yo") });
    let m = deliver(test_config("https://ilink.invalid"), msg)
        .await
        .expect("delivered");
    assert!(m.message_id.is_none());
    assert_eq!(m.text, "yo");
}

#[tokio::test]
async fn process_inbound_filters() {
    let base = "https://ilink.invalid";
    let msg = serde_json::json!({ "from_user_id": "", "item_list": text_items("x") });
    assert!(deliver(test_config(base), msg).await.is_none());
    let msg = serde_json::json!({ "from_user_id": "bot-acc", "item_list": text_items("x") });
    assert!(deliver(test_config(base), msg).await.is_none());
    let msg = serde_json::json!({ "from_user_id": "u1", "item_list": [] });
    assert!(deliver(test_config(base), msg).await.is_none());

    let group_msg = serde_json::json!({
        "from_user_id": "u1", "room_id": "r1", "message_id": "g1", "item_list": text_items("g")
    });
    let mut c = test_config(base);
    c.group_policy = "disabled".into();
    assert!(deliver(c, group_msg.clone()).await.is_none());
    let mut c = test_config(base);
    c.group_policy = "allowlist".into();
    c.group_allow_from = vec!["other".into()];
    assert!(deliver(c, group_msg.clone()).await.is_none());
    let mut c = test_config(base);
    c.group_policy = "allowlist".into();
    c.group_allow_from = vec!["r1".into()];
    let m = deliver(c, group_msg).await.expect("group delivered");
    assert_eq!(m.chat_id, "r1");
    assert!(!m.is_dm);

    let dm_msg = serde_json::json!({ "from_user_id": "u1", "item_list": text_items("d") });
    let mut c = test_config(base);
    c.dm_policy = "disabled".into();
    assert!(deliver(c, dm_msg.clone()).await.is_none());
    let mut c = test_config(base);
    c.dm_policy = "allowlist".into();
    c.allow_from = vec!["u9".into()];
    assert!(deliver(c, dm_msg.clone()).await.is_none());
    let mut c = test_config(base);
    c.dm_policy = "allowlist".into();
    c.allow_from = vec!["u1".into()];
    assert_eq!(deliver(c, dm_msg).await.expect("dm").text, "d");
}

#[tokio::test]
async fn process_inbound_drops_duplicates() {
    let a = make_adapter("https://ilink.invalid");
    let dir = tempfile::tempdir().expect("temporary cache");
    let (tx, mut rx) = mpsc::channel(4);
    a.set_inbound_sender(tx).await;
    let msg = serde_json::json!({
        "from_user_id": "u1", "message_id": "dup1", "item_list": text_items("once")
    });
    WeChatAdapter::process_inbound(a.inner.clone(), msg.clone(), dir.path()).await;
    let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout");
    assert_eq!(first.expect("first").text, "once");
    WeChatAdapter::process_inbound(a.inner.clone(), msg, dir.path()).await;
    drop(a);
    let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout");
    assert!(second.is_none());
}

#[tokio::test]
async fn send_text_with_context_token_and_trait_send_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ilink/bot/sendmessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ret": 0 })))
        .mount(&server)
        .await;
    let a = make_adapter(&server.uri());
    a.inner
        .context_tokens
        .lock()
        .await
        .insert("bot-acc:u1".into(), "ctx-1".into());
    a.send_ilink_text("u1", "hello").await.unwrap();
    a.send_message("u2", "second", None).await.unwrap();
    let bodies = bodies_for(&server, "/ilink/bot/sendmessage").await;
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["msg"]["context_token"], "ctx-1");
    assert_eq!(
        bodies[0]["msg"]["item_list"][0]["text_item"]["text"],
        "hello"
    );
    assert_eq!(bodies[0]["msg"]["to_user_id"], "u1");
    assert!(bodies[1]["msg"].get("context_token").is_none());
    assert_eq!(
        bodies[1]["msg"]["item_list"][0]["text_item"]["text"],
        "second"
    );
}

#[tokio::test]
async fn send_text_propagates_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ilink/bot/sendmessage"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&server)
        .await;
    let a = make_adapter(&server.uri());
    let err = a.send_ilink_text("u1", "hello").await.unwrap_err();
    assert!(matches!(err, GatewayError::ConnectionFailed(_)));
}

async fn mount_upload_full_url(server: &MockServer, with_header: bool) {
    Mock::given(method("POST"))
        .and(path("/ilink/bot/getuploadurl"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "upload_full_url": format!("{}/put", server.uri()) }),
        ))
        .mount(server)
        .await;
    let put = if with_header {
        ResponseTemplate::new(200).insert_header("x-encrypted-param", "enc-xyz")
    } else {
        ResponseTemplate::new(200)
    };
    Mock::given(method("PUT"))
        .and(path("/put"))
        .respond_with(put)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/ilink/bot/sendmessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ret": 0 })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn send_image_file_via_upload_full_url_with_caption_and_ctx() {
    let server = MockServer::start().await;
    mount_upload_full_url(&server, true).await;
    let a = make_adapter(&server.uri());
    a.inner
        .context_tokens
        .lock()
        .await
        .insert("bot-acc:u1".into(), "ctx-9".into());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pic.png");
    std::fs::write(&file, b"png-data").unwrap();
    a.send_ilink_file("u1", &file, Some(" cap ")).await.unwrap();

    let up = bodies_for(&server, "/ilink/bot/getuploadurl").await;
    assert_eq!(up.len(), 1);
    assert_eq!(up[0]["media_type"], MEDIA_IMAGE);
    assert_eq!(up[0]["rawsize"], 8);

    let sends = bodies_for(&server, "/ilink/bot/sendmessage").await;
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0]["msg"]["item_list"][0]["text_item"]["text"], "cap");
    let media = &sends[1]["msg"];
    assert_eq!(media["context_token"], "ctx-9");
    assert_eq!(media["item_list"][0]["type"], ITEM_IMAGE);
    assert_eq!(
        media["item_list"][0]["image_item"]["media"]["encrypt_query_param"],
        "enc-xyz"
    );
}

#[tokio::test]
async fn send_file_without_header_falls_back_to_filekey() {
    let server = MockServer::start().await;
    mount_upload_full_url(&server, false).await;
    let a = make_adapter(&server.uri());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.bin");
    std::fs::write(&file, b"xyz").unwrap();
    a.send_ilink_file("u1", &file, None).await.unwrap();
    let up = bodies_for(&server, "/ilink/bot/getuploadurl").await;
    assert_eq!(up[0]["media_type"], MEDIA_FILE);
    let filekey = up[0]["filekey"].as_str().unwrap().to_string();
    let sends = bodies_for(&server, "/ilink/bot/sendmessage").await;
    assert_eq!(sends.len(), 1);
    let item = &sends[0]["msg"]["item_list"][0];
    assert_eq!(item["type"], ITEM_FILE);
    assert_eq!(
        item["file_item"]["media"]["encrypt_query_param"],
        filekey.as_str()
    );
    assert_eq!(item["file_item"]["file_name"], "data.bin");
    assert!(sends[0]["msg"].get("context_token").is_none());
}

#[tokio::test]
async fn send_video_file_missing_upload_params_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ilink/bot/getuploadurl"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    let a = make_adapter(&server.uri());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("clip.mp4");
    std::fs::write(&file, b"video").unwrap();
    let err = a.send_ilink_file("u1", &file, None).await.unwrap_err();
    assert!(matches!(err, GatewayError::SendFailed(ref m) if m.contains("missing upload_param")));
    let up = bodies_for(&server, "/ilink/bot/getuploadurl").await;
    assert_eq!(up[0]["media_type"], MEDIA_VIDEO);
}

#[tokio::test]
async fn send_file_and_image_url_missing_local_file_errors() {
    let a = make_adapter("https://ilink.invalid");
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.png");
    let err = a
        .send_file("u1", missing.to_str().unwrap(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, GatewayError::SendFailed(_)));
    let url = format!("file://{}", dir.path().join("bad%FF.png").to_string_lossy());
    let err = a.send_image_url("u1", &url, None).await.unwrap_err();
    assert!(matches!(err, GatewayError::SendFailed(_)));
}

#[tokio::test]
async fn send_image_url_file_scheme_decodes_path() {
    let server = MockServer::start().await;
    mount_upload_full_url(&server, true).await;
    let a = make_adapter(&server.uri());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("my pic.png");
    std::fs::write(&file, b"png-bytes").unwrap();
    let url = format!("file://{}", file.to_string_lossy().replace(' ', "%20"));
    a.send_image_url("u1", &url, None).await.unwrap();
    let up = bodies_for(&server, "/ilink/bot/getuploadurl").await;
    assert_eq!(up.len(), 1);
    assert_eq!(up[0]["rawsize"], 9);
    assert_eq!(up[0]["media_type"], MEDIA_IMAGE);
}

#[test]
fn image_content_type_helpers() {
    assert_eq!(normalized_image_content_type(Some("text/html")), None);
    assert_eq!(
        normalized_image_content_type(Some("image/PNG; charset=x")),
        Some("image/png".to_string())
    );
    let cases = [
        ("image/png", "png"),
        ("image/gif", "gif"),
        ("image/webp", "webp"),
        ("image/bmp", "bmp"),
        ("image/tiff", "tiff"),
        ("image/svg+xml", "svg"),
        ("image/heic", "heic"),
        ("image/heif", "heif"),
        ("image/avif", "avif"),
    ];
    for (ct, ext) in cases {
        assert_eq!(image_extension_from_content_type(Some(ct)), Some(ext));
    }
    assert_eq!(
        image_extension_from_content_type(Some("image/x-unknown")),
        None
    );
}

#[test]
fn remote_image_file_name_and_fallback_text() {
    assert_eq!(
        remote_image_file_name("?x=1", Some("image/gif")),
        "image.gif"
    );
    assert_eq!(remote_image_file_name("", None), "image.png");
    assert_eq!(
        image_fallback_text("https://x/y.png", None),
        "https://x/y.png"
    );
    assert_eq!(
        image_fallback_text("https://x/y.png", Some("   ")),
        "https://x/y.png"
    );
}
