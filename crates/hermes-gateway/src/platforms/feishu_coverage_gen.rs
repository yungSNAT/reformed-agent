use super::*;

use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_config() -> FeishuConfig {
    FeishuConfig {
        app_id: "cli_coverage_app".to_string(),
        app_secret: "secret".to_string(),
        verification_token: None,
        encrypt_key: None,
        proxy: AdapterProxyConfig::default(),
    }
}

/// A client whose every request is routed through a proxy on a closed local
/// port, so all requests fail fast without touching the real network.
fn failing_client() -> Client {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    Client::builder()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).expect("proxy"))
        .timeout(Duration::from_secs(5))
        .build()
        .expect("client")
}

fn closed_local_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}/pic.png")
}

fn adapter_without_token() -> FeishuAdapter {
    let mut adapter = FeishuAdapter::new(test_config()).expect("adapter");
    adapter.client = failing_client();
    adapter
}

async fn adapter_with_token() -> FeishuAdapter {
    let adapter = adapter_without_token();
    *adapter.tenant_token.write().await = Some(CachedToken {
        value: "cached-token".to_string(),
        obtained_at: Instant::now(),
    });
    adapter
}

fn assert_send_failed(err: GatewayError, needle: &str) {
    match err {
        GatewayError::SendFailed(m) => assert!(m.contains(needle), "unexpected message: {m}"),
        other => panic!("expected SendFailed, got {other:?}"),
    }
}

fn assert_platform(err: GatewayError, needle: &str) {
    match err {
        GatewayError::Platform(m) => assert!(m.contains(needle), "unexpected message: {m}"),
        other => panic!("expected Platform, got {other:?}"),
    }
}

fn no_proxy_client() -> Client {
    Client::builder().no_proxy().build().expect("client")
}

#[test]
fn cached_token_expiry() {
    let fresh = CachedToken {
        value: "a".into(),
        obtained_at: Instant::now(),
    };
    assert!(!fresh.is_expired());
    if let Some(old) = Instant::now().checked_sub(Duration::from_secs(TOKEN_EXPIRY_SECS)) {
        let stale = CachedToken {
            value: "b".into(),
            obtained_at: old,
        };
        assert!(stale.is_expired());
    }
}

#[test]
fn bearer_formats_header() {
    assert_eq!(bearer("abc"), "Bearer abc");
}

#[test]
fn config_accessor_returns_config() {
    let adapter = FeishuAdapter::new(test_config()).expect("adapter");
    assert_eq!(adapter.config().app_id, "cli_coverage_app");
    assert_eq!(adapter.config().app_secret, "secret");
}

#[test]
fn verify_event_without_any_token_is_false() {
    let event = FeishuEvent {
        schema: None,
        header: None,
        event: None,
        challenge: None,
        token: None,
        event_type: None,
    };
    assert!(!FeishuAdapter::verify_event(&event, "tok"));
}

#[test]
fn extract_text_content_edge_cases() {
    let no_content = serde_json::json!({});
    assert_eq!(FeishuAdapter::extract_text_content(&no_content, "text"), "");

    let not_json = serde_json::json!({ "content": "plain words" });
    assert_eq!(
        FeishuAdapter::extract_text_content(&not_json, "text"),
        "plain words"
    );

    let image = serde_json::json!({ "content": "{\"image_key\":\"img_1\"}" });
    assert_eq!(
        FeishuAdapter::extract_text_content(&image, "image"),
        "{\"image_key\":\"img_1\"}"
    );

    let post_content = serde_json::json!({
        "title": "Title",
        "content": [
            [
                {"tag": "text", "text": "hi "},
                {"tag": "at", "user_name": "Bob"},
                {"tag": "img", "image_key": "k"}
            ],
            "not-an-array",
            [
                {"tag": "a", "text": "link", "href": "https://example.com"}
            ]
        ]
    })
    .to_string();
    let post = serde_json::json!({ "content": post_content });
    assert_eq!(
        FeishuAdapter::extract_text_content(&post, "post"),
        "Title\nhi @Bob\nlink"
    );
}

#[test]
fn flatten_post_content_without_title_or_body() {
    assert_eq!(
        FeishuAdapter::flatten_post_content(&serde_json::json!({})),
        ""
    );
    let empty_title =
        serde_json::json!({ "title": "", "content": [[{"tag": "text", "text": "x"}]] });
    assert_eq!(FeishuAdapter::flatten_post_content(&empty_title), "x");
}

#[test]
fn parse_post_message_event_with_mention() {
    let content = serde_json::json!({
        "content": [[{"tag": "at", "user_name": "Ann"}, {"tag": "text", "text": " hello"}]]
    })
    .to_string();
    let event = serde_json::json!({
        "sender": {"sender_id": {"open_id": "ou_1"}},
        "message": {
            "message_id": "om_1",
            "chat_id": "oc_1",
            "chat_type": "group",
            "message_type": "post",
            "content": content,
            "mentions": [{"key": "@_user_1"}]
        }
    });
    let msg = FeishuAdapter::parse_message_event(&event).expect("parsed");
    assert_eq!(msg.text, "@Ann hello");
    assert!(msg.is_mention);
    assert!(FeishuAdapter::is_group_chat(&msg));
    assert_eq!(msg.sender_id.as_deref(), Some("ou_1"));
}

#[test]
fn card_builder_default_and_button_grouping() {
    let card = FeishuCard::default();
    assert!(card.header.is_none());
    assert!(card.elements.is_empty());

    let card = FeishuCard::new().add_button("One", None, None).add_button(
        "Two",
        Some(serde_json::json!({"k": 1})),
        Some("primary"),
    );
    assert_eq!(card.elements.len(), 1);
    match &card.elements[0] {
        CardElement::Action { actions } => assert_eq!(actions.len(), 2),
        other => panic!("unexpected element {other:?}"),
    }
}

#[test]
fn post_element_at_and_image_serialize() {
    let at = serde_json::to_value(PostElement::at("ou_9")).expect("ser");
    assert_eq!(at, serde_json::json!({"tag": "at", "user_id": "ou_9"}));
    let img = serde_json::to_value(PostElement::image("img_k")).expect("ser");
    assert_eq!(img, serde_json::json!({"tag": "img", "image_key": "img_k"}));
}

#[test]
fn image_content_type_helpers() {
    assert_eq!(normalized_image_content_type(Some("text/html")), None);
    assert_eq!(
        normalized_image_content_type(Some("IMAGE/PNG; charset=x")).as_deref(),
        Some("image/png")
    );
    let cases = [
        ("image/jpeg", Some("jpg")),
        ("image/png", Some("png")),
        ("image/gif", Some("gif")),
        ("image/webp", Some("webp")),
        ("image/bmp", Some("bmp")),
        ("image/tiff", Some("tiff")),
        ("image/svg+xml", Some("svg")),
        ("image/heic", Some("heic")),
        ("image/heif", Some("heif")),
        ("image/avif", Some("avif")),
        ("image/x-unknown", None),
    ];
    for (ct, expected) in cases {
        assert_eq!(
            image_extension_from_content_type(Some(ct)),
            expected,
            "{ct}"
        );
    }
}

#[test]
fn remote_image_file_name_and_fallback_text() {
    assert_eq!(remote_image_file_name("", Some("image/webp")), "image.webp");
    assert_eq!(remote_image_file_name("", None), "image.png");
    assert_eq!(
        image_fallback_text("https://x/y.png", None),
        "https://x/y.png"
    );
    assert_eq!(
        image_fallback_text("https://x/y.png", Some("   ")),
        "https://x/y.png"
    );
    assert_eq!(
        image_fallback_text("https://x/y.png", Some(" cap ")),
        "cap\nhttps://x/y.png"
    );
}

#[tokio::test]
async fn tenant_token_cached_and_invalidated() {
    let adapter = adapter_with_token().await;
    assert_eq!(
        adapter.get_tenant_token().await.expect("cached"),
        "cached-token"
    );

    adapter.invalidate_token().await;
    assert!(adapter.tenant_token.read().await.is_none());

    match adapter.get_tenant_token().await {
        Err(GatewayError::Auth(m)) => assert!(m.contains("Feishu auth failed"), "{m}"),
        other => panic!("expected auth error, got {other:?}"),
    }
}

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
