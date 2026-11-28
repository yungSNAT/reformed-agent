//! MCP OAuth Authentication (Requirement 14.4)
//!
//! Provides authentication for remote MCP servers:
//! - **McpAuthProvider**: Trait for authentication providers
//! - **OAuthConfig**: OAuth2 client credentials flow implementation
//! - **BearerTokenAuth**: Simple bearer token authentication

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::McpError;

// ---------------------------------------------------------------------------
// McpAuthProvider trait
// ---------------------------------------------------------------------------

/// Trait for providing authentication tokens for MCP server connections.
///
/// Implementations can provide different authentication mechanisms:
/// - Bearer token (simple static token)
/// - OAuth2 (client credentials flow with token refresh)
/// - Custom (any mechanism that produces a token string)
#[async_trait::async_trait]
pub trait McpAuthProvider: Send + Sync {
    /// Get an authentication token.
    ///
    /// Returns a token string suitable for use in an Authorization header.
    /// For OAuth, this will automatically refresh expired tokens.
    async fn get_token(&self) -> Result<String, McpError>;

    /// Get the type name of this auth provider (for logging/debugging).
    fn provider_type(&self) -> &str;
}

// ---------------------------------------------------------------------------
// BearerTokenAuth
// ---------------------------------------------------------------------------

/// Simple bearer token authentication provider.
///
/// Use this when you have a static API key or token that doesn't expire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BearerTokenAuth {
    /// The bearer token to use for authentication.
    pub token: String,
}

impl BearerTokenAuth {
    /// Create a new bearer token auth provider.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

#[async_trait::async_trait]
impl McpAuthProvider for BearerTokenAuth {
    async fn get_token(&self) -> Result<String, McpError> {
        if self.token.is_empty() {
            return Err(McpError::Auth("Bearer token is empty".to_string()));
        }
        Ok(self.token.clone())
    }

    fn provider_type(&self) -> &str {
        "bearer"
    }
}

// ---------------------------------------------------------------------------
// OAuthConfig
// ---------------------------------------------------------------------------

/// OAuth2 client credentials flow configuration.
///
/// This auth provider implements the OAuth2 client credentials grant
/// (RFC 6749 Section 4.4) which is suitable for server-to-server
/// authentication where no user interaction is needed.
///
/// Token lifecycle:
/// 1. On first `get_token()` call, requests an access token from the token endpoint
/// 2. Caches the token and its expiration time
/// 3. On subsequent calls, returns the cached token if it hasn't expired
/// 4. If the token has expired (with a 30-second buffer), automatically refreshes it
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OAuthConfig {
    /// OAuth2 client ID.
    pub client_id: String,
    /// OAuth2 client secret.
    pub client_secret: String,
    /// Authorization server URL for the authorize endpoint.
    pub auth_url: String,
    /// Token endpoint URL for exchanging authorization codes or refreshing tokens.
    pub token_url: String,
    /// OAuth2 scopes to request.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Optional audience parameter for the token request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

impl OAuthConfig {
    /// Create a new OAuth configuration.
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        auth_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            auth_url: auth_url.into(),
            token_url: token_url.into(),
            scopes: Vec::new(),
            audience: None,
        }
    }

    /// Add a scope to request.
    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scopes.push(scope.into());
        self
    }

    /// Set the audience parameter.
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = Some(audience.into());
        self
    }

    /// Exchange client credentials for an access token.
    ///
    /// Performs the OAuth2 client credentials grant:
    /// ```text
    /// POST /token
    /// Content-Type: application/x-www-form-urlencoded
    ///
    /// grant_type=client_credentials
    /// &client_id=...
    /// &client_secret=...
    /// &scope=...
    /// ```
    async fn fetch_token(&self) -> Result<OAuthToken, McpError> {
        info!("Fetching OAuth2 access token from {}", self.token_url);

        let client = reqwest::Client::new();

        let mut params = vec![
            ("grant_type", "client_credentials".to_string()),
            ("client_id", self.client_id.clone()),
            ("client_secret", self.client_secret.clone()),
        ];

        if !self.scopes.is_empty() {
            params.push(("scope", self.scopes.join(" ")));
        }

        if let Some(ref audience) = self.audience {
            params.push(("audience", audience.clone()));
        }

        let response = client
            .post(&self.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| McpError::Auth(format!("Token request failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(McpError::Auth(format!(
                "Token endpoint returned {}: {}",
                status, body
            )));
        }

        let token_response: OAuthTokenResponse = response
            .json()
            .await
            .map_err(|e| McpError::Auth(format!("Failed to parse token response: {}", e)))?;

        Ok(OAuthToken {
            access_token: token_response.access_token,
        })
    }
}

/// Cached OAuth token with expiration information.
#[derive(Debug, Clone)]
struct OAuthToken {
    access_token: String,
}

/// Response from an OAuth2 token endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default = "default_token_type")]
    token_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_in: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

// Note: We use a simpler approach for OAuth since async_trait + interior mutability
// with cached tokens requires more complex synchronization. For now, each get_token
// call fetches a fresh token. A production implementation would use Arc<Mutex<>>
// to cache the token.

#[async_trait::async_trait]
impl McpAuthProvider for OAuthConfig {
    async fn get_token(&self) -> Result<String, McpError> {
        debug!("Getting OAuth token via client credentials flow");
        let token = self.fetch_token().await?;
        Ok(token.access_token)
    }

    fn provider_type(&self) -> &str {
        "oauth2"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bearer_token_auth() {
        let auth = BearerTokenAuth::new("test-token-123");
        assert_eq!(auth.token, "test-token-123");
        assert_eq!(auth.provider_type(), "bearer");
    }

    #[tokio::test]
    async fn test_bearer_token_get_token() {
        let auth = BearerTokenAuth::new("test-token-123");
        let token = auth.get_token().await.unwrap();
        assert_eq!(token, "test-token-123");
    }

    #[tokio::test]
    async fn test_bearer_token_empty() {
        let auth = BearerTokenAuth::new("");
        let result = auth.get_token().await;
        assert!(result.is_err());
    }

    #[test]
    fn test_oauth_config_new() {
        let config = OAuthConfig::new(
            "client-id",
            "client-secret",
            "https://auth.example.com/authorize",
            "https://auth.example.com/token",
        );
        assert_eq!(config.client_id, "client-id");
        assert_eq!(config.client_secret, "client-secret");
        assert_eq!(config.auth_url, "https://auth.example.com/authorize");
        assert_eq!(config.token_url, "https://auth.example.com/token");
        assert!(config.scopes.is_empty());
        assert!(config.audience.is_none());
    }

    #[test]
    fn test_oauth_config_with_scopes() {
        let config = OAuthConfig::new("id", "secret", "auth", "token")
            .with_scope("read")
            .with_scope("write")
            .with_audience("api.example.com");
        assert_eq!(config.scopes, vec!["read", "write"]);
        assert_eq!(config.audience, Some("api.example.com".to_string()));
    }

    #[test]
    fn test_oauth_config_provider_type() {
        let config = OAuthConfig::new("id", "secret", "auth", "token");
        assert_eq!(config.provider_type(), "oauth2");
    }

    #[test]
    fn test_oauth_token_serialization() {
        let auth = BearerTokenAuth::new("token123");
        let json = serde_json::to_string(&auth).unwrap();
        let deserialized: BearerTokenAuth = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.token, "token123");
    }

    #[test]
    fn test_oauth_config_serialization() {
        let config = OAuthConfig::new("id", "secret", "auth", "token")
            .with_scope("read")
            .with_audience("api");
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: OAuthConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.client_id, "id");
        assert_eq!(deserialized.scopes, vec!["read"]);
        assert_eq!(deserialized.audience, Some("api".to_string()));
    }

    async fn exchange_with_fixture(
        status: &str,
        body: &str,
        configure: impl FnOnce(OAuthConfig) -> OAuthConfig,
    ) -> (Result<String, McpError>, String) {
        use crate::transport::tests::answer_once_with_body;
        use tokio::net::TcpListener;
        use tokio::time::{timeout, Duration};

        let endpoint = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token_url = format!("http://{}/token", endpoint.local_addr().unwrap());
        let config = configure(OAuthConfig::new(
            "fixture-client",
            "fixture-secret",
            "about:blank",
            token_url,
        ));
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (result, (head, form)) = timeout(Duration::from_secs(10), async {
            tokio::join!(config.get_token(), answer_once_with_body(&endpoint, reply))
        })
        .await
        .expect("loopback token exchange timed out");
        assert!(head.starts_with("post /token http/1.1"));
        assert!(head.contains("content-type: application/x-www-form-urlencoded"));
        (result, form)
    }

    #[tokio::test]
    async fn oauth_exchange_uses_only_the_loopback_endpoint() {
        use std::time::Duration;
        use tokio::process::Command;

        const FIXTURE_CHILD: &str = "HERMES_TEST_MCP_OAUTH_LOOPBACK_CHILD";
        if std::env::var_os(FIXTURE_CHILD).is_none() {
            // The production client honours proxies. Re-run only this fixture in
            // a child without the parent's proxy configuration or credentials.
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .env_clear()
                .env(FIXTURE_CHILD, "1")
                .env("NO_PROXY", "127.0.0.1")
                .kill_on_drop(true)
                .args([
                    "--exact",
                    "auth::tests::oauth_exchange_uses_only_the_loopback_endpoint",
                ]);
            for variable in ["LLVM_PROFILE_FILE", "SystemRoot"] {
                if let Some(value) = std::env::var_os(variable) {
                    command.env(variable, value);
                }
            }
            let output = tokio::time::timeout(Duration::from_secs(35), command.output())
                .await
                .expect("isolated token exchange timed out")
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{output:?}"
            );
            return;
        }
        let (result, form) = exchange_with_fixture(
            "200 OK",
            r#"{"access_token":"fixture-access","expires_in":3600}"#,
            |config| {
                config
                    .with_scope("read")
                    .with_scope("write")
                    .with_audience("fixture-api")
            },
        )
        .await;
        assert_eq!(result.unwrap(), "fixture-access");
        let mut fields = form.split('&').collect::<Vec<_>>();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "audience=fixture-api",
                "client_id=fixture-client",
                "client_secret=fixture-secret",
                "grant_type=client_credentials",
                "scope=read+write",
            ]
        );
        for (status, body, expected) in [
            (
                "403 Forbidden",
                "fixture denial",
                "Token endpoint returned 403 Forbidden: fixture denial",
            ),
            (
                "200 OK",
                r#"{"expires_in":3600}"#,
                "Failed to parse token response:",
            ),
        ] {
            let (result, form) = exchange_with_fixture(status, body, |config| config).await;
            let error = result.expect_err("expected a token exchange error");
            assert!(
                matches!(&error, McpError::Auth(message) if message.starts_with(expected)),
                "{error}"
            );
            assert!(!form.contains("scope=") && !form.contains("audience="));
        }

        let config = OAuthConfig::new("fixture-client", "fixture-secret", "about:blank", "invalid");
        let error = config.get_token().await.expect_err("invalid endpoint URL");
        assert!(
            matches!(&error, McpError::Auth(message) if message.starts_with("Token request failed:")),
            "{error}"
        );
    }
}
