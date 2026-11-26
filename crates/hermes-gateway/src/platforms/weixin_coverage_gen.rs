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

include!("weixin_part2_coverage_gen.rs");
