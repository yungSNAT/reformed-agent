use super::*;
use serde_json::json;
use wiremock::matchers::{body_json, body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture_config() -> QqBotConfig {
    QqBotConfig {
        app_id: "fixture-app".into(),
        client_secret: "fixture-value".into(),
        markdown_support: false,
        proxy: AdapterProxyConfig::default(),
    }
}

fn fixture_adapter(server: &MockServer) -> QqBotAdapter {
    let mut adapter = QqBotAdapter::new(fixture_config()).unwrap();
    adapter.client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    adapter.api_base = server.uri();
    adapter.token_url = format!("{}/app/getAppAccessToken", adapter.api_base);
    adapter
}

async fn mock_token(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/app/getAppAccessToken"))
        .and(body_json(
            json!({"appId": "fixture-app", "clientSecret": "fixture-value"}),
        ))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn messages_use_one_access_token_across_user_and_group_chats() {
    let server = MockServer::start().await;
    mock_token(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({"access_token": "fixture-access"})),
    )
    .await;
    let mut adapter = fixture_adapter(&server);
    *adapter.access_token.write().await = Some(("expired-fixture".into(), Instant::now()));
    assert_eq!(adapter.platform_name(), "qqbot");
    assert!(!adapter.is_running());
    adapter.start().await.unwrap();
    assert!(adapter.is_running());

    Mock::given(method("POST"))
        .and(path("/v2/users/user-1/messages"))
        .and(header("authorization", "QQBot fixture-access"))
        .and(body_partial_json(
            json!({"msg_type": 0, "content": "Plain text"}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    adapter
        .send_message("user-1", "Plain text", None)
        .await
        .unwrap();

    Mock::given(method("POST"))
        .and(path("/v2/groups/group_1/messages"))
        .and(header("authorization", "QQBot fixture-access"))
        .and(body_partial_json(
            json!({"msg_type": 2, "markdown": {"content": "[Attachment: notes.txt] Summary"}}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    adapter.config.markdown_support = true;
    adapter
        .send_file("group_1", "fixtures/notes.txt", Some("Summary"))
        .await
        .unwrap();
    adapter
        .edit_message("group_1", "fixture", "unused")
        .await
        .unwrap();
    adapter.stop().await.unwrap();
    assert!(!adapter.is_running());
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn image_uploads_pass_file_info_and_a_rejected_media_message_falls_back() {
    let server = MockServer::start().await;
    mock_token(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fixture-access", "expires_in": 7200
        })),
    )
    .await;
    let adapter = fixture_adapter(&server);
    let image_url = "https://media.example.invalid/chart.png";
    Mock::given(method("POST"))
        .and(path("/v2/groups/group_1/files"))
        .and(header("authorization", "QQBot fixture-access"))
        .and(body_json(
            json!({"file_type": 1, "srv_send_msg": false, "url": image_url}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"file_info": "remote-info"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v2/groups/group_1/messages"))
        .and(header("authorization", "QQBot fixture-access"))
        .and(body_partial_json(json!({
            "msg_type": 7, "media": {"file_info": "remote-info"}, "content": "Chart"
        })))
        .respond_with(ResponseTemplate::new(503).set_body_string("media unavailable"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v2/groups/group_1/messages"))
        .and(header("authorization", "QQBot fixture-access"))
        .and(body_partial_json(
            json!({"msg_type": 0, "content": format!("Chart\n{image_url}")}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    adapter
        .send_image_url("group_1", image_url, Some("  Chart  "))
        .await
        .unwrap();

    let root = tempfile::tempdir().unwrap();
    let image = root.path().join("sample image.png");
    std::fs::write(&image, b"image").unwrap();
    let image_url = format!("file://{}", urlencoding::encode(image.to_str().unwrap()));
    Mock::given(method("POST"))
        .and(path("/v2/users/user-1/files"))
        .and(header("authorization", "QQBot fixture-access"))
        .and(body_json(
            json!({"file_type": 1, "srv_send_msg": false, "file_data": "aW1hZ2U="}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"file_info": "local-info"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v2/users/user-1/messages"))
        .and(header("authorization", "QQBot fixture-access"))
        .and(body_partial_json(
            json!({"msg_type": 7, "media": {"file_info": "local-info"}}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    adapter
        .send_image_url("user-1", &image_url, None)
        .await
        .unwrap();
    std::fs::write(&image, b"").unwrap();
    assert!(matches!(
        adapter.send_image_url("user-1", &image_url, None).await,
        Err(GatewayError::SendFailed(reason)) if reason.contains("file is empty")
    ));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 6);
    let local_message: serde_json::Value = serde_json::from_slice(&requests[5].body).unwrap();
    assert!(local_message.get("content").is_none());
}

#[test]
fn both_fields_are_required_before_an_adapter_is_created() {
    let mut config = fixture_config();
    config.app_id = " ".into();
    assert!(matches!(
        QqBotAdapter::new(config),
        Err(GatewayError::Platform(reason)) if reason.contains("requires app_id")
    ));
    let mut config = fixture_config();
    config.client_secret.clear();
    assert!(matches!(
        QqBotAdapter::new(config),
        Err(GatewayError::Platform(reason)) if reason.contains("requires client_secret")
    ));
}

#[tokio::test]
async fn auth_refusal_and_invalid_tokens_do_not_send_or_populate_the_cache() {
    for (response, expected) in [
        (
            ResponseTemplate::new(403).set_body_string("fixture refusal"),
            "fixture refusal",
        ),
        (
            ResponseTemplate::new(200).set_body_string("not json"),
            "auth parse failed",
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({"access_token": " "})),
            "missing access_token",
        ),
    ] {
        let server = MockServer::start().await;
        mock_token(&server, response).await;
        let adapter = fixture_adapter(&server);
        assert!(matches!(
            adapter.send_message("user-1", "hello", None).await,
            Err(GatewayError::Auth(reason)) if reason.contains(expected)
        ));
        assert!(adapter.access_token.read().await.is_none());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn tokens_with_no_lifetime_beyond_the_refresh_margin_are_refetched() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/app/getAppAccessToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fixture-short-lived", "expires_in": 60
        })))
        .expect(2)
        .mount(&server)
        .await;
    let adapter = fixture_adapter(&server);
    for _ in 0..2 {
        assert_eq!(
            adapter.get_access_token().await.unwrap(),
            "fixture-short-lived"
        );
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
