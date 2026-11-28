use super::*;
use serde_json::json;
use wiremock::matchers::{body_string, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn encode_keeps_unreserved_characters_unchanged() {
    let input = "ABCXYZabcxyz0123456789-_.~";
    assert_eq!(super::urlencoding::encode_query_component(input), input);
}

#[test]
fn encode_empty_string_is_empty() {
    assert_eq!(super::urlencoding::encode_query_component(""), "");
}

#[test]
fn encode_escapes_reserved_and_space_characters() {
    assert_eq!(super::urlencoding::encode_query_component(" "), "%20");
    assert_eq!(super::urlencoding::encode_query_component(":"), "%3A");
    assert_eq!(super::urlencoding::encode_query_component("/"), "%2F");
    assert_eq!(super::urlencoding::encode_query_component("&="), "%26%3D");
    assert_eq!(super::urlencoding::encode_query_component("+"), "%2B");
    assert_eq!(
        super::urlencoding::encode_query_component("a b:c"),
        "a%20b%3Ac"
    );
}

#[test]
fn encode_escapes_multibyte_utf8_as_uppercase_hex_bytes() {
    assert_eq!(super::urlencoding::encode_query_component("é"), "%C3%A9");
    assert_eq!(
        super::urlencoding::encode_query_component("\u{1F600}"),
        "%F0%9F%98%80"
    );
}

#[test]
fn encode_escapes_control_bytes_with_two_hex_digits() {
    assert_eq!(super::urlencoding::encode_query_component("\n"), "%0A");
    assert_eq!(super::urlencoding::encode_query_component("\0"), "%00");
}

#[test]
fn form_body_empty_pairs_is_empty_string() {
    assert_eq!(form_body(&[]), "");
}

#[test]
fn form_body_single_pair() {
    assert_eq!(form_body(&[("client_id", "abc123")]), "client_id=abc123");
}

#[test]
fn form_body_joins_pairs_with_ampersand_and_encodes_values() {
    let body = form_body(&[
        ("client_id", DEFAULT_DEVICE_CLIENT_ID),
        ("scope", "read:org read:user repo"),
    ]);
    assert_eq!(
        body,
        "client_id=Iv23liCLQ87g8RVSjNGp&scope=read%3Aorg%20read%3Auser%20repo"
    );
}

#[test]
fn form_body_encodes_keys_and_grant_type() {
    let body = form_body(&[
        ("device code", "x&y"),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ]);
    assert_eq!(
        body,
        "device%20code=x%26y&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"
    );
}

#[test]
fn form_body_preserves_pair_order() {
    let body = form_body(&[("b", "2"), ("a", "1"), ("c", "3")]);
    assert_eq!(body, "b=2&a=1&c=3");
}

async fn mount_device_response(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .and(header("accept", "application/json"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string(
            "client_id=fixture-client&scope=fixture%20scope",
        ))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_poll_response(server: &MockServer, priority: u8, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(header("accept", "application/json"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string(
            "client_id=fixture-client&device_code=fixture%26device&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .respond_with(response)
        .with_priority(priority)
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
}

fn device_response(server: &MockServer) -> serde_json::Value {
    json!({
        "device_code": "fixture&device",
        "user_code": "FIXTURE-1",
        "verification_uri": format!("{}/verify", server.uri())
    })
}

async fn request_device_flow(server: &MockServer) -> Result<String, AgentError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("loopback client");
    tokio::time::timeout(
        Duration::from_secs(35),
        complete_device_flow(&client, &server.uri(), "fixture-client", "fixture scope"),
    )
    .await
    .expect("device flow fixture completed")
}

#[tokio::test]
async fn device_flow_retries_pending_and_slow_down_until_approved() {
    let server = MockServer::start().await;
    mount_device_response(
        &server,
        ResponseTemplate::new(200).set_body_json(device_response(&server)),
    )
    .await;
    for (priority, status, body) in [
        (1, 400, json!({"error": "authorization_pending"})),
        (2, 400, json!({"error": "slow_down"})),
        (
            3,
            200,
            json!({"access_token": "fixture-value", "scope": "fixture-scope"}),
        ),
    ] {
        mount_poll_response(
            &server,
            priority,
            ResponseTemplate::new(status).set_body_json(body),
        )
        .await;
    }

    let started = std::time::Instant::now();
    let result = request_device_flow(&server).await.expect("approved device");
    let elapsed = started.elapsed();
    assert_eq!(result, "fixture-value");
    assert!(
        elapsed >= Duration::from_secs(20),
        "three polls and slow_down backoff took only {elapsed:?}"
    );
    server.verify().await;
}

#[tokio::test]
async fn device_flow_requires_all_authorization_fields_before_polling() {
    for required in ["device_code", "user_code", "verification_uri"] {
        let server = MockServer::start().await;
        let mut body = device_response(&server);
        body.as_object_mut()
            .expect("device object")
            .remove(required);
        mount_device_response(&server, ResponseTemplate::new(200).set_body_json(body)).await;

        let error = request_device_flow(&server)
            .await
            .expect_err("incomplete device response");
        assert!(matches!(
            error,
            AgentError::AuthFailed(message)
                if message == format!("GitHub device/code: missing {required}")
        ));
        assert_eq!(
            server.received_requests().await.expect("request log").len(),
            1
        );
    }
}

#[tokio::test]
async fn device_flow_reports_provider_and_invalid_json_errors() {
    let server = MockServer::start().await;
    mount_device_response(
        &server,
        ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_client",
            "error_description": "Unknown fixture client"
        })),
    )
    .await;
    let error = request_device_flow(&server)
        .await
        .expect_err("device rejected");
    assert!(matches!(
        error,
        AgentError::AuthFailed(message)
            if message == "GitHub device/code: invalid_client: Unknown fixture client"
    ));
    assert_eq!(
        server.received_requests().await.expect("request log").len(),
        1
    );

    let server = MockServer::start().await;
    mount_device_response(
        &server,
        ResponseTemplate::new(200).set_body_string("invalid JSON"),
    )
    .await;
    let error = request_device_flow(&server)
        .await
        .expect_err("malformed device body");
    assert!(matches!(
        error,
        AgentError::AuthFailed(message) if message.starts_with("device/code JSON: ")
    ));
    assert_eq!(
        server.received_requests().await.expect("request log").len(),
        1
    );
}

#[tokio::test]
async fn device_flow_reports_poll_errors_and_rejects_empty_values() {
    for (response, expected) in [
        (
            ResponseTemplate::new(403).set_body_json(json!({
                "error": "access_denied",
                "error_description": "Fixture declined"
            })),
            "GitHub OAuth error: access_denied: Fixture declined",
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({"access_token": ""})),
            "GitHub OAuth: empty response without error field",
        ),
        (
            ResponseTemplate::new(200).set_body_string("invalid JSON"),
            "Unexpected token response (not JSON): invalid JSON",
        ),
    ] {
        let server = MockServer::start().await;
        mount_device_response(
            &server,
            ResponseTemplate::new(200).set_body_json(device_response(&server)),
        )
        .await;
        mount_poll_response(&server, 1, response).await;

        let error = request_device_flow(&server).await.expect_err("poll failed");
        assert!(matches!(error, AgentError::AuthFailed(message) if message == expected));
        assert_eq!(
            server.received_requests().await.expect("request log").len(),
            2
        );
    }
}
