use super::*;

use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_config() -> FeishuConfig {
    FeishuConfig {
        app_id: "cli_coverage_app".to_string(),
        app_secret: "secret".to_string(),
        verification_token: None, // nosemgrep: codacy.yaml.security.hard-coded-tokens
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

include!("feishu_part2_coverage_gen.rs");
