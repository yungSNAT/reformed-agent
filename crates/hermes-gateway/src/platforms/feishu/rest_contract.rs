use super::*;
use serde_json::json;
use wiremock::matchers::{body_json, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const AUTH_PATH: &str = "/open-apis/auth/v3/tenant_access_token/internal";
const APP_ID: &str = "fixture-app";
const APP_SECRET: &str = "fixture-value";
const TENANT: &str = "fixture-tenant";

fn loopback_adapter(server: &MockServer) -> FeishuAdapter {
    assert!(server.address().ip().is_loopback());
    let mut adapter = FeishuAdapter::new(FeishuConfig {
        app_id: APP_ID.to_string(),
        app_secret: APP_SECRET.to_string(),
        verification_token: None,
        encrypt_key: None,
        proxy: AdapterProxyConfig::default(),
    })
    .unwrap();
    assert_eq!(adapter.api_base, FEISHU_API_BASE);
    adapter.api_base = format!("{}/open-apis", server.uri());
    adapter.client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    adapter
}

fn tenant_request() -> wiremock::MockBuilder {
    Mock::given(method("POST"))
        .and(path(AUTH_PATH))
        .and(body_json(
            json!({"app_id": APP_ID, "app_secret": APP_SECRET}),
        ))
}

fn tenant_success() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"code": 0, "tenant_access_token": TENANT}))
}

fn authorised_request(verb: &str, route: &str) -> wiremock::MockBuilder {
    Mock::given(method(verb))
        .and(path(route))
        .and(header("Authorization", bearer(TENANT).as_str()))
}

async fn mount_single_tenant_request(server: &MockServer) {
    tenant_request()
        .respond_with(tenant_success())
        .expect(1)
        .mount(server)
        .await;
}

fn chat_message_request(kind: &str, content: serde_json::Value) -> wiremock::MockBuilder {
    authorised_request("POST", "/open-apis/im/v1/messages")
        .and(query_param("receive_id_type", "chat_id"))
        .and(body_json(json!({
            "receive_id": "office",
            "msg_type": kind,
            "content": content.to_string(),
        })))
}

fn accepted_message() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(json!({"code": 0, "data": {"message_id": "delivered"}}))
}

#[tokio::test]
async fn tenant_token_is_reused_until_explicit_invalidation() {
    let server = MockServer::start().await;
    let adapter = loopback_adapter(&server);
    tenant_request()
        .respond_with(tenant_success())
        .expect(2)
        .mount(&server)
        .await;
    assert_eq!(adapter.get_tenant_token().await.unwrap(), TENANT);
    assert_eq!(adapter.get_tenant_token().await.unwrap(), TENANT);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    adapter.invalidate_token().await;
    assert_eq!(adapter.get_tenant_token().await.unwrap(), TENANT);
    server.verify().await;
}

#[tokio::test]
async fn tenant_token_distinguishes_invalid_json_rejection_and_missing_value() {
    let server = MockServer::start().await;
    let adapter = loopback_adapter(&server);
    for (response, expected) in [
        (
            ResponseTemplate::new(200).set_body_string("not JSON"),
            "Feishu auth parse failed",
        ),
        (
            ResponseTemplate::new(403).set_body_json(json!({"code": 123, "msg": "denied"})),
            "Feishu token request failed (code=123): denied",
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({"code": 0})),
            "No tenant_access_token in response",
        ),
    ] {
        tenant_request()
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let error = adapter.get_tenant_token().await.unwrap_err();
        assert!(
            matches!(&error, GatewayError::Auth(message) if message.contains(expected)),
            "{error}"
        );
        assert!(adapter.tenant_token.read().await.is_none());
        server.verify().await;
        server.reset().await;
    }
}

#[tokio::test]
async fn chat_lookup_returns_group_metadata_and_service_errors() {
    let server = MockServer::start().await;
    let adapter = loopback_adapter(&server);
    tenant_request()
        .respond_with(tenant_success())
        .expect(1)
        .mount(&server)
        .await;
    authorised_request("GET", "/open-apis/im/v1/chats/office")
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "name": "Office", "description": "Shift notes", "chat_mode": "group",
                "owner_id": "fixture-owner", "member_count": 4,
            },
        })))
        .expect(1)
        .mount(&server)
        .await;
    authorised_request("GET", "/open-apis/im/v1/chats/missing")
        .respond_with(ResponseTemplate::new(404).set_body_string("unknown chat"))
        .expect(1)
        .mount(&server)
        .await;

    let chat = adapter.get_chat_info("office").await.unwrap();
    assert_eq!(chat.chat_id, "office");
    assert_eq!(chat.name.as_deref(), Some("Office"));
    assert_eq!(chat.description.as_deref(), Some("Shift notes"));
    assert_eq!(chat.chat_type.as_deref(), Some("group"));
    assert_eq!(chat.owner_id.as_deref(), Some("fixture-owner"));
    assert_eq!(chat.member_count, Some(4));
    let error = adapter.get_chat_info("missing").await.unwrap_err();
    assert!(matches!(error, GatewayError::Platform(message) if message.contains("unknown chat")));
    server.verify().await;
}

#[tokio::test]
async fn message_lookup_preserves_response_and_distinguishes_bad_replies() {
    let server = MockServer::start().await;
    let adapter = loopback_adapter(&server);
    tenant_request()
        .respond_with(tenant_success())
        .expect(1)
        .mount(&server)
        .await;
    let expected = json!({"data": {"items": [{"message_id": "message", "msg_type": "text"}]}});
    for (id, response) in [
        (
            "message",
            ResponseTemplate::new(200).set_body_json(&expected),
        ),
        (
            "broken",
            ResponseTemplate::new(200).set_body_string("not JSON"),
        ),
        (
            "missing",
            ResponseTemplate::new(404).set_body_string("unknown message"),
        ),
    ] {
        authorised_request("GET", &format!("/open-apis/im/v1/messages/{id}"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
    }
    assert_eq!(adapter.get_message("message").await.unwrap(), expected);
    for (id, expected) in [
        ("broken", "Feishu get_message parse failed"),
        ("missing", "Feishu get_message error: unknown message"),
    ] {
        let error = adapter.get_message(id).await.unwrap_err();
        assert!(
            matches!(&error, GatewayError::Platform(message) if message.contains(expected)),
            "{error}"
        );
    }
    server.verify().await;
}

#[tokio::test]
async fn message_mutations_distinguish_fatal_and_nonfatal_service_errors() {
    let server = MockServer::start().await;
    let adapter = loopback_adapter(&server);
    tenant_request()
        .respond_with(tenant_success())
        .expect(1)
        .mount(&server)
        .await;
    let message = "/open-apis/im/v1/messages/message";
    authorised_request("PATCH", message)
        .and(body_json(json!({
            "msg_type": "text",
            "content": json!({"text": "updated notes"}).to_string()
        })))
        .respond_with(ResponseTemplate::new(403).set_body_string("edit denied"))
        .expect(1)
        .mount(&server)
        .await;
    authorised_request("DELETE", message)
        .respond_with(ResponseTemplate::new(403).set_body_string("delete denied"))
        .expect(1)
        .mount(&server)
        .await;
    authorised_request("POST", &format!("{message}/reactions"))
        .and(body_json(
            json!({"reaction_type": {"emoji_type": "THUMBSUP"}}),
        ))
        .respond_with(ResponseTemplate::new(403).set_body_string("reaction ignored"))
        .expect(1)
        .mount(&server)
        .await;

    let edit = adapter
        .edit_text("message", "  updated notes  ")
        .await
        .unwrap_err();
    assert!(
        matches!(edit, GatewayError::SendFailed(message) if message.contains("Feishu edit error: edit denied"))
    );
    let delete = adapter.delete_message("message").await.unwrap_err();
    assert!(
        matches!(delete, GatewayError::SendFailed(message) if message.contains("Feishu delete_message error: delete denied"))
    );
    adapter.add_reaction("message", "THUMBSUP").await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn audio_delivery_uploads_opus_with_duration_and_sends_its_key() {
    let server = MockServer::start().await;
    let adapter = loopback_adapter(&server);
    mount_single_tenant_request(&server).await;
    let root = tempfile::tempdir().unwrap();
    let clip = root.path().join("shift-notes.opus");
    std::fs::write(&clip, b"synthetic audio bytes").unwrap();
    authorised_request("POST", "/open-apis/im/v1/files")
        .and(body_string_contains("name=\"file_type\"\r\n\r\nopus"))
        .and(body_string_contains("name=\"duration\"\r\n\r\n900"))
        .and(body_string_contains("filename=\"shift-notes.opus\""))
        .and(body_string_contains("synthetic audio bytes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"file_key": "audio-key"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    chat_message_request("audio", json!({"file_key": "audio-key"}))
        .respond_with(accepted_message())
        .expect(1)
        .mount(&server)
        .await;

    let id = adapter
        .send_audio("office", clip.to_str().unwrap(), Some(900))
        .await
        .unwrap();
    assert_eq!(id, "delivered");
    server.verify().await;
}

#[tokio::test]
async fn generic_file_delivery_uploads_document_then_sends_its_caption() {
    let server = MockServer::start().await;
    let adapter = loopback_adapter(&server);
    mount_single_tenant_request(&server).await;
    let root = tempfile::tempdir().unwrap();
    let document = root.path().join("notes.txt");
    std::fs::write(&document, b"synthetic shift notes").unwrap();
    authorised_request("POST", "/open-apis/im/v1/files")
        .and(body_string_contains("name=\"file_type\"\r\n\r\nstream"))
        .and(body_string_contains("filename=\"notes.txt\""))
        .and(body_string_contains("synthetic shift notes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"file_key": "document-key"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    chat_message_request("file", json!({"file_key": "document-key"}))
        .respond_with(accepted_message())
        .expect(1)
        .mount(&server)
        .await;
    chat_message_request("text", json!({"text": "Shift handover"}))
        .respond_with(accepted_message())
        .expect(1)
        .mount(&server)
        .await;

    adapter
        .send_file("office", document.to_str().unwrap(), Some("Shift handover"))
        .await
        .unwrap();
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 4);
    assert_eq!(received[1].url.path(), "/open-apis/im/v1/files");
    let file_message: serde_json::Value = serde_json::from_slice(&received[2].body).unwrap();
    assert_eq!(file_message["msg_type"], "file");
    let caption_message: serde_json::Value = serde_json::from_slice(&received[3].body).unwrap();
    assert_eq!(caption_message["msg_type"], "text");
    server.verify().await;
}

#[tokio::test]
async fn image_delivery_stops_when_upload_does_not_produce_a_key() {
    let root = tempfile::tempdir().unwrap();
    let image = root.path().join("diagram.png");
    std::fs::write(&image, b"synthetic image bytes").unwrap();
    for (response, expected) in [
        (
            ResponseTemplate::new(503).set_body_string("images unavailable"),
            "Feishu image upload error: images unavailable",
        ),
        (
            ResponseTemplate::new(200).set_body_string("not JSON"),
            "Feishu image upload parse failed",
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({"data": {}})),
            "No image_key in upload response",
        ),
    ] {
        let server = MockServer::start().await;
        let adapter = loopback_adapter(&server);
        mount_single_tenant_request(&server).await;
        authorised_request("POST", "/open-apis/im/v1/images")
            .and(body_string_contains("name=\"image_type\"\r\n\r\nmessage"))
            .and(body_string_contains("filename=\"diagram.png\""))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let error = adapter
            .send_image("office", image.to_str().unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, GatewayError::SendFailed(message) if message.contains(expected)),
            "{error}"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        server.verify().await;
    }
}
