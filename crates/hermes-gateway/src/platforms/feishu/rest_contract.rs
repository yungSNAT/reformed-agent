use super::*;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
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
