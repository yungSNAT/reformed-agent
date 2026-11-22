use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_config(base: &str) -> WeixinConfig {
    WeixinConfig {
        account_id: "bot-acc".into(),
        token: "tok-123".into(),
        base_url: base.into(),
        cdn_base_url: base.into(),
        dm_policy: "open".into(),
        group_policy: "open".into(),
        allow_from: vec![],
        group_allow_from: vec![],
        proxy: AdapterProxyConfig::default(),
    }
}

fn make_adapter(base: &str) -> WeChatAdapter {
    WeChatAdapter::new(test_config(base)).expect("adapter")
}

fn text_items(t: &str) -> Value {
    serde_json::json!([{ "type": 1, "text_item": { "text": t } }])
}

async fn bodies_for(server: &MockServer, p: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path() == p)
        .map(|r| serde_json::from_slice(&r.body).expect("json body"))
        .collect()
}

async fn deliver(config: WeixinConfig, msg: Value) -> Option<IncomingMessage> {
    let a = WeChatAdapter::new(config).expect("adapter");
    let (tx, mut rx) = mpsc::channel(4);
    a.set_inbound_sender(tx).await;
    WeChatAdapter::process_inbound(a.inner.clone(), msg).await;
    drop(a);
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("recv timed out")
}

#[test]
fn pkcs7_unpad_empty_and_invalid_padding() {
    assert_eq!(pkcs7_unpad(&[]).unwrap(), Vec::<u8>::new());
    let zero_pad = [1u8, 2, 3, 0];
    assert_eq!(pkcs7_unpad(&zero_pad).unwrap(), zero_pad.to_vec());
    let mismatched = [5u8, 9, 9, 3];
    assert_eq!(pkcs7_unpad(&mismatched).unwrap(), mismatched.to_vec());
}

#[test]
fn parse_aes_key_hex_and_non_hex_32_bytes() {
    let hex = "00112233445566778899aabbccddeeff";
    let b64 = base64::engine::general_purpose::STANDARD.encode(hex);
    let k = parse_aes_key(&b64).unwrap();
    assert_eq!(
        k,
        [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff
        ]
    );
    let nonhex = "zz112233445566778899aabbccddeeff";
    let b64 = base64::engine::general_purpose::STANDARD.encode(nonhex);
    let err = parse_aes_key(&b64).unwrap_err();
    assert!(matches!(err, GatewayError::Platform(ref m) if m.contains("32 decoded bytes")));
}

#[test]
fn cdn_download_url_builds_and_rejects_invalid_base() {
    let u = cdn_download_url("https://cdn.example.com/c2c/", "abc").unwrap();
    assert_eq!(
        u,
        "https://cdn.example.com/c2c/download?encrypted_query_param=abc"
    );
    let err = cdn_download_url("not a url", "abc").unwrap_err();
    assert!(matches!(err, GatewayError::ConnectionFailed(_)));
}

#[test]
fn from_platform_config_reads_extra_values() {
    let mut p = hermes_config::PlatformConfig::default();
    p.extra
        .insert("account_id".into(), serde_json::json!(" acc1 "));
    p.extra.insert("token".into(), serde_json::json!("tok-x"));
    p.extra
        .insert("base_url".into(), serde_json::json!("https://base.example"));
    p.extra.insert(
        "cdn_base_url".into(),
        serde_json::json!("https://cdn.example"),
    );
    p.extra
        .insert("dm_policy".into(), serde_json::json!("allowlist"));
    p.extra
        .insert("group_policy".into(), serde_json::json!("open"));
    p.extra
        .insert("allow_from".into(), serde_json::json!("a, b,,c"));
    p.extra.insert(
        "group_allow_from".into(),
        serde_json::json!(["g1", " ", 3, "g2 "]),
    );
    let c = WeixinConfig::from_platform_config(&p);
    assert_eq!(c.account_id, "acc1");
    if p.token.clone().unwrap_or_default().is_empty() {
        assert_eq!(c.token, "tok-x");
    }
    assert_eq!(c.base_url, "https://base.example");
    assert_eq!(c.cdn_base_url, "https://cdn.example");
    assert_eq!(c.dm_policy, "allowlist");
    assert_eq!(c.group_policy, "open");
    assert_eq!(c.allow_from, vec!["a", "b", "c"]);
    assert_eq!(c.group_allow_from, vec!["g1", "g2"]);
}

#[test]
fn new_requires_account_id() {
    let mut c = test_config("https://ilink.invalid");
    c.account_id = String::new();
    let err = WeChatAdapter::new(c).err().expect("should fail");
    assert!(matches!(err, GatewayError::Platform(ref m) if m.contains("account_id")));
}

#[test]
fn account_paths_have_expected_names() {
    let a = WeChatAdapter::account_json_path("acc9");
    assert_eq!(a.file_name().unwrap().to_str().unwrap(), "acc9.json");
    let parent = a.parent().unwrap();
    assert_eq!(parent.file_name().unwrap().to_str().unwrap(), "accounts");
    assert_eq!(
        parent
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap(),
        "weixin"
    );
    let s = WeChatAdapter::sync_buf_path("acc9");
    assert_eq!(s.file_name().unwrap().to_str().unwrap(), "acc9.sync.json");
    let c = WeChatAdapter::context_path("acc9");
    assert_eq!(
        c.file_name().unwrap().to_str().unwrap(),
        "acc9.context-tokens.json"
    );
}

#[tokio::test]
async fn adapter_trait_basics_and_stop() {
    let a = make_adapter("https://ilink.invalid");
    assert_eq!(a.config().account_id, "bot-acc");
    assert_eq!(a.platform_name(), "weixin");
    assert!(a.splits_long_messages());
    assert!(a.edit_message("c", "m", "t").await.is_ok());
    assert!(a.stop().await.is_ok());
    assert!(!a.is_running());
}

#[tokio::test]
async fn download_and_decrypt_media_requires_source() {
    let a = make_adapter("https://ilink.invalid");
    let err = WeChatAdapter::download_and_decrypt_media(
        &a.inner,
        Some(""),
        None,
        None,
        Duration::from_secs(1),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, GatewayError::Platform(ref m) if m.contains("neither")));
}

#[tokio::test]
async fn download_and_decrypt_media_via_mock_cdn() {
    let server = MockServer::start().await;
    let key: [u8; 16] = rand::random();
    let plaintext = b"hello weixin media";
    let ct = aes128_ecb_encrypt(plaintext, &key);
    Mock::given(method("GET"))
        .and(path("/download"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(ct.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/raw"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"raw".to_vec()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;
    let a = make_adapter(&server.uri());
    let key_b64 = base64::engine::general_purpose::STANDARD.encode(key);
    let out = WeChatAdapter::download_and_decrypt_media(
        &a.inner,
        Some("q1"),
        Some(&key_b64),
        None,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(out, plaintext.to_vec());

    let raw_url = format!("{}/raw", server.uri());
    let out = WeChatAdapter::download_and_decrypt_media(
        &a.inner,
        None,
        None,
        Some(&raw_url),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(out, b"raw".to_vec());

    let missing = format!("{}/missing", server.uri());
    let err = WeChatAdapter::download_and_decrypt_media(
        &a.inner,
        None,
        None,
        Some(&missing),
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, GatewayError::ConnectionFailed(ref m) if m.contains("404")));
}

#[test]
fn image_aes_key_and_media_map() {
    let item = serde_json::json!({
        "image_item": {
            "aeskey": "00112233445566778899aabbccddeeff",
            "media": { "aes_key": "fallback" }
        }
    });
    let expected = base64::engine::general_purpose::STANDARD.encode([
        0x00u8, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ]);
    assert_eq!(WeChatAdapter::image_aes_key_b64(&item), Some(expected));

    let item = serde_json::json!({
        "image_item": { "aeskey": "xyz", "media": { "aes_key": "fallback" } }
    });
    assert_eq!(
        WeChatAdapter::image_aes_key_b64(&item),
        Some("fallback".to_string())
    );
    let item = serde_json::json!({ "image_item": {} });
    assert_eq!(WeChatAdapter::image_aes_key_b64(&item), None);

    let v = serde_json::json!({ "video_item": { "media": { "a": 1 } } });
    assert_eq!(
        WeChatAdapter::media_map(&v, "video_item"),
        Some(&serde_json::json!({ "a": 1 }))
    );
    assert!(WeChatAdapter::media_map(&v, "image_item").is_none());
}

#[tokio::test]
async fn media_line_for_item_failure_paths_return_none() {
    let a = make_adapter("https://ilink.invalid");
    let items = [
        serde_json::json!({ "type": 2, "image_item": { "media": {} } }),
        serde_json::json!({ "type": 2 }),
        serde_json::json!({ "type": 5, "video_item": { "media": {} } }),
        serde_json::json!({ "type": 4, "file_item": { "media": {}, "file_name": "a.txt" } }),
        serde_json::json!({ "type": 3, "voice_item": { "text": "hi", "media": {} } }),
        serde_json::json!({ "type": 3, "voice_item": { "text": "", "media": {} } }),
        serde_json::json!({ "type": 99 }),
        serde_json::json!({ "no_type": true }),
    ];
    for item in &items {
        assert!(WeChatAdapter::media_line_for_item(&a.inner, item)
            .await
            .is_none());
    }
    let list = vec![serde_json::json!({
        "type": 2,
        "image_item": { "media": {} },
        "ref_msg": { "message_item": { "type": 5, "video_item": { "media": {} } } }
    })];
    assert!(WeChatAdapter::collect_media_lines(&a.inner, &list)
        .await
        .is_empty());
}

#[tokio::test]
async fn upload_ciphertext_missing_header_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let a = make_adapter(&server.uri());
    let err = WeChatAdapter::upload_ciphertext_to_cdn(&a.inner, b"ct", "p", "fk")
        .await
        .unwrap_err();
    assert!(
        matches!(err, GatewayError::ConnectionFailed(ref m) if m.contains("missing x-encrypted-param"))
    );
}

#[test]
fn outbound_media_item_image_and_video() {
    let v = WeChatAdapter::outbound_media_item(MEDIA_IMAGE, "eq", "k", 32, 20, "a.png");
    assert_eq!(v["type"], ITEM_IMAGE);
    assert_eq!(v["image_item"]["mid_size"], 32);
    assert_eq!(v["image_item"]["media"]["encrypt_query_param"], "eq");
    assert_eq!(v["image_item"]["media"]["aes_key"], "k");
    let v = WeChatAdapter::outbound_media_item(MEDIA_VIDEO, "eq2", "k2", 48, 40, "a.mp4");
    assert_eq!(v["type"], ITEM_VIDEO);
    assert_eq!(v["video_item"]["video_size"], 48);
    assert_eq!(v["video_item"]["media"]["encrypt_type"], 1);
}

#[test]
fn ilink_headers_authorization_variants() {
    let h = WeChatAdapter::ilink_headers(None, "{}");
    assert!(!h.contains_key("authorization"));
    assert_eq!(h.get("content-length").unwrap().to_str().unwrap(), "2");
    let h = WeChatAdapter::ilink_headers(Some("bad\nvalue"), "{}");
    assert!(!h.contains_key("authorization"));
    let h = WeChatAdapter::ilink_headers(Some("ok"), "{}");
    assert_eq!(
        h.get("authorization").unwrap().to_str().unwrap(),
        "Bearer ok"
    );
}

#[tokio::test]
async fn get_updates_ok_and_invalid_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ilink/bot/getupdates"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ret": 0, "msgs": [] })),
        )
        .mount(&server)
        .await;
    let a = make_adapter(&server.uri());
    let v = WeChatAdapter::get_updates(&a.inner, "buf-1", 10)
        .await
        .unwrap();
    assert_eq!(v["ret"], 0);
    let bodies = bodies_for(&server, "/ilink/bot/getupdates").await;
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["get_updates_buf"], "buf-1");
    assert_eq!(bodies[0]["base_info"]["channel_version"], CHANNEL_VERSION);

    let bad = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ilink/bot/getupdates"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&bad)
        .await;
    let b = make_adapter(&bad.uri());
    let err = WeChatAdapter::get_updates(&b.inner, "", 10)
        .await
        .unwrap_err();
    assert!(matches!(err, GatewayError::ConnectionFailed(ref m) if m.contains("weixin JSON")));
}

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
    let (tx, mut rx) = mpsc::channel(4);
    a.set_inbound_sender(tx).await;
    let msg = serde_json::json!({
        "from_user_id": "u1", "message_id": "dup1", "item_list": text_items("once")
    });
    WeChatAdapter::process_inbound(a.inner.clone(), msg.clone()).await;
    let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout");
    assert_eq!(first.expect("first").text, "once");
    WeChatAdapter::process_inbound(a.inner.clone(), msg).await;
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
