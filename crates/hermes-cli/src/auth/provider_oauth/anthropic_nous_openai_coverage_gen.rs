use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn extract_error_message_handles_all_shapes() {
    assert_eq!(extract_error_message("not json"), None);
    assert_eq!(extract_error_message("{}"), None);
    assert_eq!(
        extract_error_message(r#"{"error":"","error_description":""}"#),
        None
    );
    assert_eq!(
        extract_error_message(r#"{"error_description":"only description"}"#),
        Some("only description".to_string())
    );
    assert_eq!(
        extract_error_message(r#"{"error":"invalid_grant"}"#),
        Some("invalid_grant".to_string())
    );
    assert_eq!(
        extract_error_message(r#"{"error":"invalid_grant","error_description":"code expired"}"#),
        Some("invalid_grant: code expired".to_string())
    );
    // Non-string error values are treated as absent.
    assert_eq!(
        extract_error_message(r#"{"error":42,"error_description":"desc"}"#),
        Some("desc".to_string())
    );
}

#[tokio::test]
async fn login_returns_trimmed_provider_message() {
    let msg = login("  openai  ").await.expect("login ok");
    assert_eq!(msg, "LLM API key stored for provider 'openai'.");
}

#[tokio::test]
async fn logout_returns_trimmed_provider_message() {
    let msg = logout("\tanthropic\n").await.expect("logout ok");
    assert_eq!(msg, "Removed stored credential for provider 'anthropic'.");
}

#[tokio::test]
async fn nous_device_code_request_failure_reports_status_and_detail() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/device/code"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string(r#"{"error":"invalid_client","error_description":"bad client"}"#),
        )
        .mount(&server)
        .await;

    let options = NousDeviceCodeOptions {
        portal_base_url: Some(format!("{}/", server.uri())),
        inference_base_url: Some("https://inference.example.invalid/".to_string()),
        client_id: Some("test-client".to_string()),
        scope: Some("test-scope".to_string()),
        timeout_seconds: 10.0,
        open_browser: false,
        min_key_ttl_seconds: Default::default(),
    };
    let err = login_nous_device_code(options)
        .await
        .expect_err("device code request should fail");
    let text = err.to_string();
    assert!(
        text.contains("Nous device code request failed"),
        "unexpected error: {text}"
    );
    assert!(text.contains("400"), "unexpected error: {text}");
    assert!(
        text.contains("invalid_client: bad client"),
        "unexpected error: {text}"
    );
}

#[tokio::test]
async fn nous_device_code_failure_without_json_uses_raw_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/device/code"))
        .respond_with(ResponseTemplate::new(503).set_body_string("service down"))
        .mount(&server)
        .await;

    let options = NousDeviceCodeOptions {
        portal_base_url: Some(server.uri()),
        inference_base_url: None,
        client_id: None,
        scope: None,
        timeout_seconds: f64::INFINITY,
        open_browser: false,
        min_key_ttl_seconds: Default::default(),
    };
    let err = login_nous_device_code(options)
        .await
        .expect_err("device code request should fail");
    let text = err.to_string();
    assert!(text.contains("503"), "unexpected error: {text}");
    assert!(text.contains("service down"), "unexpected error: {text}");
}

#[tokio::test]
async fn nous_device_code_missing_verification_uri_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/oauth/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"device_code":"dev-123","user_code":"USER-1","verification_uri_complete":"  ","verification_uri":"   ","expires_in":900,"interval":5}"#,
        ))
        .mount(&server)
        .await;

    let options = NousDeviceCodeOptions {
        portal_base_url: Some(server.uri()),
        inference_base_url: None,
        client_id: Some("client".to_string()),
        scope: Some("scope".to_string()),
        timeout_seconds: f64::NAN,
        open_browser: false,
        min_key_ttl_seconds: Default::default(),
    };
    let err = login_nous_device_code(options)
        .await
        .expect_err("missing verification uri should fail");
    let text = err.to_string();
    assert!(
        text.contains("device code response missing verification_uri"),
        "unexpected error: {text}"
    );
}
