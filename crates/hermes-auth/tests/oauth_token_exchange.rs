use base64::Engine;
use hermes_auth::{
    exchange_authorization_code, exchange_refresh_token, OAuth2ClientAuthMethod, OAuth2Endpoints,
};
use hermes_core::AgentError;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn exchange_authorization_code_parses_token_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-ok",
            "token_type": "Bearer",
            "expires_in": 120,
            "refresh_token": "rt-ok",
            "scope": "openid"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let endpoints = OAuth2Endpoints {
        authorize_url: "https://id.example/authorize".to_string(),
        token_url: format!("{}/oauth/token", server.uri()),
        client_id: "public-client".to_string(),
        redirect_uri: "http://127.0.0.1:9999/cb".to_string(),
        scopes: vec!["openid".to_string()],
        client_secret: None,
        client_auth_method: OAuth2ClientAuthMethod::default(),
    };

    let cred =
        exchange_authorization_code("acme", &endpoints, "auth-code-xyz", "pkce-verifier-secret")
            .await
            .expect("token exchange");

    assert_eq!(cred.provider, "acme");
    assert_eq!(cred.access_token, "at-ok");
    assert_eq!(cred.refresh_token.as_deref(), Some("rt-ok"));
    assert_eq!(cred.token_type, "Bearer");
    assert!(cred.expires_at.is_some());
}

#[tokio::test]
async fn exchange_refresh_token_keeps_old_refresh_if_omitted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-new",
            "token_type": "Bearer",
            "expires_in": 60
        })))
        .expect(1)
        .mount(&server)
        .await;

    let endpoints = OAuth2Endpoints {
        authorize_url: "https://id.example/authorize".to_string(),
        token_url: format!("{}/oauth/token", server.uri()),
        client_id: "public-client".to_string(),
        redirect_uri: "http://127.0.0.1:9999/cb".to_string(),
        scopes: vec![],
        client_secret: None,
        client_auth_method: OAuth2ClientAuthMethod::default(),
    };

    let cred = exchange_refresh_token("acme", &endpoints, "old-refresh")
        .await
        .expect("refresh");

    assert_eq!(cred.access_token, "at-new");
    assert_eq!(cred.refresh_token.as_deref(), Some("old-refresh"));
}

#[tokio::test]
async fn exchange_authorization_code_supports_confidential_client_basic_auth() {
    let server = MockServer::start().await;
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("client%3Awith+space:secret%3A%40%2F")
    );
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("Authorization", basic))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier=pkce-verifier-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-confidential",
            "token_type": "Bearer",
            "expires_in": 120,
            "refresh_token": "rt-confidential"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let endpoints = OAuth2Endpoints {
        authorize_url: "https://id.example/authorize".to_string(),
        token_url: format!("{}/oauth/token", server.uri()),
        client_id: "client:with space".to_string(),
        redirect_uri: "http://127.0.0.1:9999/cb".to_string(),
        scopes: vec!["openid".to_string()],
        client_secret: Some("secret:@/".to_string()),
        client_auth_method: OAuth2ClientAuthMethod::ClientSecretBasic,
    };

    let cred =
        exchange_authorization_code("acme", &endpoints, "auth-code-xyz", "pkce-verifier-secret")
            .await
            .expect("token exchange");

    assert_eq!(cred.access_token, "at-confidential");
    assert_eq!(cred.refresh_token.as_deref(), Some("rt-confidential"));
}

#[tokio::test]
async fn exchange_refresh_token_supports_confidential_client_secret_post() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=old-refresh"))
        .and(body_string_contains("client_secret=post-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-post",
            "token_type": "Bearer",
            "expires_in": 60,
            "refresh_token": "rt-post"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let endpoints = OAuth2Endpoints {
        authorize_url: "https://id.example/authorize".to_string(),
        token_url: format!("{}/oauth/token", server.uri()),
        client_id: "post-client".to_string(),
        redirect_uri: "http://127.0.0.1:9999/cb".to_string(),
        scopes: vec![],
        client_secret: Some("post-secret".to_string()),
        client_auth_method: OAuth2ClientAuthMethod::ClientSecretPost,
    };

    let cred = exchange_refresh_token("acme", &endpoints, "old-refresh")
        .await
        .expect("refresh");

    assert_eq!(cred.access_token, "at-post");
    assert_eq!(cred.refresh_token.as_deref(), Some("rt-post"));
}

#[tokio::test]
async fn token_exchanges_refuse_an_expiry_past_year_9999() {
    // About 9,500 years, then the largest integer: neither may read as a
    // credential that never expires.
    for expires_in in [300_000_000_000_i64, i64::MAX] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-forever",
                "token_type": "Bearer",
                "expires_in": expires_in
            })))
            .expect(2)
            .mount(&server)
            .await;
        let endpoints = OAuth2Endpoints {
            authorize_url: "https://id.example/authorize".to_string(),
            token_url: format!("{}/oauth/token", server.uri()),
            client_id: "public-client".to_string(),
            redirect_uri: "http://127.0.0.1:9999/cb".to_string(),
            scopes: vec![],
            client_secret: None,
            client_auth_method: OAuth2ClientAuthMethod::default(),
        };

        let code = exchange_authorization_code("acme", &endpoints, "code", "verifier").await;
        let refresh = exchange_refresh_token("acme", &endpoints, "old-refresh").await;

        assert!(code.is_err(), "{expires_in}: {code:?}");
        assert!(refresh.is_err(), "{expires_in}: {refresh:?}");
    }
}

#[tokio::test]
async fn token_exchange_errors_keep_only_the_status_and_standard_code() {
    let cases = [
        (
            400,
            r#"{"error":"invalid_grant","error_description":"Croft Manor fixture response"}"#
                .to_string(),
            Some("invalid_grant"),
        ),
        (
            401,
            r#"{"error":"invalid_client","error_uri":"https://id.example/private-fixture"}"#
                .to_string(),
            Some("invalid_client"),
        ),
        (
            400,
            r#"{"error":"provider-specific fixture value"}"#.to_string(),
            None,
        ),
        (502, "Upstream fixture response".to_string(), None),
        (
            400,
            format!(
                r#"{{"error":"invalid_grant","error_description":"{}"}}"#,
                "a".repeat(16_384)
            ),
            None,
        ),
    ];

    for (status, body, code) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .expect(2)
            .mount(&server)
            .await;
        let endpoints = OAuth2Endpoints {
            authorize_url: "https://id.example/authorize".to_string(),
            token_url: format!("{}/oauth/token", server.uri()),
            client_id: "public-client".to_string(),
            redirect_uri: "http://127.0.0.1:9999/cb".to_string(),
            scopes: vec![],
            client_secret: None,
            client_auth_method: OAuth2ClientAuthMethod::default(),
        };

        let exchange = exchange_authorization_code("acme", &endpoints, "code", "verifier").await;
        let refresh = exchange_refresh_token("acme", &endpoints, "old-refresh").await;
        for (flow, result) in [("token exchange", exchange), ("token refresh", refresh)] {
            let Err(AgentError::AuthFailed(message)) = result else {
                panic!("{flow} should report authentication failure with HTTP {status}");
            };
            let mut expected = format!("{flow} failed (HTTP {status})");
            if let Some(code) = code {
                expected.push_str(": ");
                expected.push_str(code);
            }
            assert_eq!(message, expected);
        }
    }
}
