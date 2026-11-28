#[test]
fn decrypt_xml_rejects_bad_ciphertext() {
    let app = make_app("a", "ww_test", 3);
    let e = decrypt_err(&app, "@@@@");
    assert!(e.contains("invalid encrypted payload"), "{e}");
    let ten = base64::engine::general_purpose::STANDARD.encode([1u8; 10]);
    let e = decrypt_err(&app, &ten);
    assert!(e.contains("invalid encrypted block size"), "{e}");
    let e = decrypt_err(&app, "");
    assert!(e.contains("invalid encrypted block size"), "{e}");
}

#[test]
fn decrypt_xml_rejects_bad_plaintext_layout() {
    let app = make_app("a", "ww_test", 3);
    let key = key_bytes(3);

    let mut bad_pad = vec![5u8; 31];
    bad_pad.push(0);
    let e = decrypt_err(&app, &cbc_encrypt(&key, &bad_pad));
    assert!(e.contains("invalid pkcs7 padding"), "{e}");

    let mut short = vec![1u8; 16];
    short.extend_from_slice(&[16u8; 16]);
    let e = decrypt_err(&app, &cbc_encrypt(&key, &short));
    assert!(e.contains("too short"), "{e}");

    let plain = build_plain(b"<xml/>", 1000, b"ww_test");
    let e = decrypt_err(&app, &cbc_encrypt(&key, &plain));
    assert!(e.contains("invalid xml length"), "{e}");

    let plain = build_plain(&[0xff, 0xfe], 2, b"ww_test");
    let e = decrypt_err(&app, &cbc_encrypt(&key, &plain));
    assert!(e.contains("xml utf8 decode failed"), "{e}");

    let plain = build_plain(b"<x/>", 4, &[0xff]);
    let e = decrypt_err(&app, &cbc_encrypt(&key, &plain));
    assert!(e.contains("receive_id decode failed"), "{e}");

    let plain = build_plain(b"<x/>", 4, b"other");
    let e = decrypt_err(&app, &cbc_encrypt(&key, &plain));
    assert!(e.contains("receive_id mismatch"), "{e}");
}

#[test]
fn resolve_app_for_chat_matches_corp_prefix() {
    let adapter = WeComCallbackAdapter::new(make_config(
        "127.0.0.1",
        0,
        vec![make_app("a", "ww_a", 1), make_app("b", "ww_b", 2)],
    ))
    .expect("adapter");
    assert_eq!(adapter.resolve_app_for_chat("ww_b:user").name, "b");
    assert_eq!(adapter.resolve_app_for_chat("ww_a:user").name, "a");
    assert_eq!(adapter.resolve_app_for_chat("unknown").name, "a");
    assert_eq!(adapter.platform_name(), "wecom_callback");
    assert!(!adapter.is_running());
}

async fn adapter_with_cached_token() -> WeComCallbackAdapter {
    let adapter = WeComCallbackAdapter::new(make_config(
        "127.0.0.1",
        0,
        vec![make_app("a", "ww_test", 1)],
    ))
    .expect("adapter");
    adapter.access_tokens.write().await.insert(
        "a".to_string(),
        (
            "cached-token".to_string(),
            Instant::now() + Duration::from_secs(600),
        ),
    );
    adapter
}

#[tokio::test]
async fn cached_access_token_is_reused() {
    let adapter = adapter_with_cached_token().await;
    let app = adapter.config.apps[0].clone();
    let token = adapter.get_access_token_for_app(&app).await.unwrap();
    assert_eq!(token, "cached-token");
}

#[tokio::test]
async fn callback_sends_text_and_uploaded_media_with_the_selected_app_token() {
    let server = MockServer::start().await;
    let mut selected = make_app("support", "ww_support", 2);
    selected.agent_id = "42".to_string();
    let session = uuid::Uuid::new_v4().simple().to_string();
    Mock::given(method("GET"))
        .and(path("/gettoken"))
        .and(query_param("corpid", selected.corp_id.as_str()))
        .and(query_param("corpsecret", selected.corp_secret.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": session,
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut adapter = WeComCallbackAdapter::new(make_config(
        "127.0.0.1",
        0,
        vec![make_app("default", "ww_default", 1), selected],
    ))
    .unwrap();
    adapter.api_base = server.uri();
    adapter.client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();

    Mock::given(method("POST"))
        .and(path("/message/send"))
        .and(query_param("access_token", session.as_str()))
        .and(body_json(serde_json::json!({
            "touser": "alice",
            "msgtype": "text",
            "agentid": 42,
            "text": { "content": "Your attachments are ready" },
            "safe": 0
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    adapter
        .send_message("ww_support:alice", "Your attachments are ready", None)
        .await
        .unwrap();

    let directory = tempfile::tempdir().unwrap();
    for (extension, media_type) in [
        ("png", "image"),
        ("wav", "voice"),
        ("mp4", "video"),
        ("txt", "file"),
    ] {
        let file_name = format!("attachment.{extension}");
        let file = directory.path().join(&file_name);
        let contents = format!("Fixture bytes for {file_name}");
        tokio::fs::write(&file, &contents).await.unwrap();
        let media_id = format!("uploaded-{media_type}");
        Mock::given(method("POST"))
            .and(path("/media/upload"))
            .and(query_param("access_token", session.as_str()))
            .and(query_param("type", media_type))
            .and(body_string_contains(format!(
                "name=\"media\"; filename=\"{file_name}\""
            )))
            .and(body_string_contains(format!("\r\n\r\n{contents}\r\n")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "media_id": media_id })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let media = if media_type == "video" {
            serde_json::json!({ "media_id": media_id, "title": "Fixture clip" })
        } else {
            serde_json::json!({ "media_id": media_id })
        };
        Mock::given(method("POST"))
            .and(path("/message/send"))
            .and(query_param("access_token", session.as_str()))
            .and(body_json(serde_json::json!({
                "touser": "alice",
                "msgtype": media_type,
                "agentid": 42,
                (media_type): media
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        adapter
            .send_file(
                "ww_support:alice",
                &file.to_string_lossy(),
                Some("Fixture clip"),
            )
            .await
            .unwrap();
    }
    server.verify().await;
}

#[tokio::test]
async fn send_file_and_image_url_fail_on_missing_file() {
    let adapter = adapter_with_cached_token().await;
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.png");
    let e = err_text(
        adapter
            .send_file("ww_test:alice", missing.to_str().unwrap(), None)
            .await,
    );
    assert!(e.contains("Failed to read file"), "{e}");

    let url = format!("file://{}/missing%20file.png", dir.path().display());
    let e = err_text(
        adapter
            .send_image_url("ww_test:alice", &url, Some("cap"))
            .await,
    );
    assert!(e.contains("Failed to read file"), "{e}");

    let url = format!("file://{}/bad%FF.png", dir.path().display());
    let e = err_text(adapter.send_image_url("ww_test:alice", &url, None).await);
    assert!(e.contains("Failed to read file"), "{e}");
}

#[tokio::test]
async fn edit_message_is_noop() {
    let adapter = adapter_with_cached_token().await;
    assert!(adapter.edit_message("c", "m", "t").await.is_ok());
}

#[tokio::test]
async fn start_and_stop_toggle_running() {
    let adapter = WeComCallbackAdapter::new(make_config(
        "127.0.0.1",
        0,
        vec![make_app("a", "ww_test", 1)],
    ))
    .expect("adapter");
    adapter.start().await.expect("start");
    assert!(adapter.is_running());
    assert!(adapter.shutdown_tx.read().await.is_some());
    adapter.stop().await.expect("stop");
    assert!(!adapter.is_running());
    assert!(adapter.shutdown_tx.read().await.is_none());
}

#[tokio::test]
async fn start_rejects_invalid_address() {
    let adapter = WeComCallbackAdapter::new(make_config(
        "not a host",
        0,
        vec![make_app("a", "ww_test", 1)],
    ))
    .expect("adapter");
    let e = err_text(adapter.start().await);
    assert!(e.contains("invalid address"), "{e}");
    assert!(!adapter.is_running());
}

#[tokio::test]
async fn handler_returns_ok_on_empty_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(addr).await.unwrap();
    let (server, peer) = listener.accept().await.unwrap();
    client.shutdown().await.unwrap();
    let (inbound, _rx, seen) = new_state();
    let apps = vec![make_app("a", "ww_test", 1)];
    handle_callback_request(server, peer, &apps, PATH, inbound, seen)
        .await
        .expect("ok");
    let mut out = Vec::new();
    client.read_to_end(&mut out).await.unwrap();
    assert!(out.is_empty());
}

#[tokio::test]
async fn handler_404_405_413() {
    let apps = vec![make_app("a", "ww_test", 1)];
    let (inbound, _rx, seen) = new_state();
    let r = exchange(
        &apps,
        b"GET /other HTTP/1.1\r\n\r\n",
        inbound.clone(),
        seen.clone(),
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 404"), "{r}");

    let r = exchange(
        &apps,
        format!("PUT {PATH} HTTP/1.1\r\n\r\n").as_bytes(),
        inbound.clone(),
        seen.clone(),
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 405"), "{r}");

    let req = format!(
        "POST {PATH} HTTP/1.1\r\nContent-Length: {}\r\n\r\n<xml/>",
        WECOM_CALLBACK_MAX_BODY_BYTES + 10
    );
    let r = exchange(&apps, req.as_bytes(), inbound, seen).await;
    assert!(r.starts_with("HTTP/1.1 413"), "{r}");
}

#[tokio::test]
async fn handler_get_verifies_echostr() {
    let apps = vec![
        make_app("other", "ww_other", 9),
        make_app("a", "ww_test", 1),
    ];
    let enc = encrypt_xml(1, "echo-12345", "ww_test");
    let sig = sign(&apps[1], &enc);
    let req = format!(
        "GET {PATH}?msg_signature={sig}&timestamp={TS}&nonce={NONCE}&echostr={} HTTP/1.1\r\nHost: x\r\n\r\n",
        urlencoding::encode(&enc)
    );
    let (inbound, _rx, seen) = new_state();
    let r = exchange(&apps, req.as_bytes(), inbound.clone(), seen.clone()).await;
    assert!(r.starts_with("HTTP/1.1 200 OK"), "{r}");
    assert!(r.ends_with("echo-12345"), "{r}");

    let bad = format!(
        "GET {PATH}?msg_signature=bad&timestamp={TS}&nonce={NONCE}&echostr=abc HTTP/1.1\r\n\r\n"
    );
    let r = exchange(&apps, bad.as_bytes(), inbound, seen).await;
    assert!(r.starts_with("HTTP/1.1 403"), "{r}");
}

#[tokio::test]
async fn handler_post_text_message_and_dedup() {
    let app = make_app("a", "ww_test", 1);
    let apps = vec![app.clone()];
    let (inbound, mut rx, seen) = new_state();
    let xml = "<xml><ToUserName>ww_test</ToUserName><FromUserName>alice</FromUserName><CreateTime>1700000000</CreateTime><MsgType>text</MsgType><Content>hello there</Content><MsgId>m-1</MsgId></xml>";
    let req = post_request(&app, 1, xml);
    let r = exchange(&apps, &req, inbound.clone(), seen.clone()).await;
    assert!(r.starts_with("HTTP/1.1 200 OK"), "{r}");
    assert!(r.ends_with("success"), "{r}");
    let msg = rx.try_recv().expect("inbound message");
    assert_eq!(msg.platform, "wecom_callback");
    assert_eq!(msg.chat_id, "ww_test:alice");
    assert_eq!(msg.user_id, "alice");
    assert_eq!(msg.text, "hello there");
    assert_eq!(msg.message_id.as_deref(), Some("m-1"));
    assert!(msg.is_dm);
    assert!(seen.read().await.contains_key("m-1"));

    let r = exchange(&apps, &req, inbound, seen).await;
    assert!(r.ends_with("success"), "{r}");
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn handler_post_group_mention_is_stripped() {
    let app = make_app("a", "ww_test", 1);
    let apps = vec![app.clone()];
    let (inbound, mut rx, seen) = new_state();
    let xml = "<xml><FromUserName>bob</FromUserName><ChatId>grp1</ChatId><MsgType>TEXT</MsgType><Content>@Bot  /approve x</Content><MsgId>g-1</MsgId></xml>";
    let r = exchange(&apps, &post_request(&app, 1, xml), inbound, seen).await;
    assert!(r.ends_with("success"), "{r}");
    let msg = rx.try_recv().expect("inbound message");
    assert_eq!(msg.chat_id, "ww_test:grp1");
    assert_eq!(msg.text, "/approve x");
    assert!(!msg.is_dm);
}

#[tokio::test]
async fn handler_post_subscribe_event_becomes_start() {
    let app = make_app("a", "ww_test", 1);
    let apps = vec![app.clone()];
    let (inbound, mut rx, seen) = new_state();
    let xml = "<xml><FromUserName>bob</FromUserName><CreateTime>42</CreateTime><MsgType>event</MsgType><Event>subscribe</Event></xml>";
    let r = exchange(&apps, &post_request(&app, 1, xml), inbound, seen).await;
    assert!(r.ends_with("success"), "{r}");
    let msg = rx.try_recv().expect("inbound message");
    assert_eq!(msg.text, "/start");
    assert_eq!(msg.message_id.as_deref(), Some("bob:42"));
}

#[tokio::test]
async fn handler_post_non_text_is_acknowledged_without_message() {
    let app = make_app("a", "ww_test", 1);
    let apps = vec![app.clone()];
    let (inbound, mut rx, seen) = new_state();
    let xml = "<xml><FromUserName>carol</FromUserName><MsgType>image</MsgType></xml>";
    let r = exchange(&apps, &post_request(&app, 1, xml), inbound, seen.clone()).await;
    assert!(r.ends_with("success"), "{r}");
    assert!(rx.try_recv().is_err());
    assert!(seen.read().await.contains_key("carol:0"));
}

#[tokio::test]
async fn handler_post_bad_requests_return_400() {
    let app = make_app("a", "ww_test", 1);
    let apps = vec![app.clone()];
    let (inbound, mut rx, seen) = new_state();
    let xml = "<xml><MsgType>text</MsgType><Content>hi</Content></xml>";
    let r = exchange(
        &apps,
        &post_request(&app, 1, xml),
        inbound.clone(),
        seen.clone(),
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 400"), "{r}");

    let body = "<xml><Encrypt>garbage</Encrypt></xml>";
    let req = format!(
        "POST {PATH}?msg_signature=bad&timestamp={TS}&nonce={NONCE} HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let r = exchange(&apps, req.as_bytes(), inbound, seen).await;
    assert!(r.starts_with("HTTP/1.1 400"), "{r}");
    assert!(rx.try_recv().is_err());
}
