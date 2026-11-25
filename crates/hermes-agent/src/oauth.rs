//! OAuth credential management for provider integrations.
//!
//! This module provides a lightweight token cache with auto-refresh support.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use hermes_core::AgentError;

/// A cached OAuth access token with expiration metadata.
#[derive(Debug, Clone)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in_secs: u64,
}

impl OAuthToken {
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.access_token)
    }
}

/// A callback used to fetch/refresh OAuth tokens.
pub type TokenFetcher = Arc<
    dyn Fn(Option<String>) -> futures::future::BoxFuture<'static, Result<OAuthToken, AgentError>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
struct CachedToken {
    token: OAuthToken,
    issued_at: Instant,
}

impl CachedToken {
    fn is_expired(&self) -> bool {
        // Refresh 30 seconds before hard expiry.
        let ttl = Duration::from_secs(self.token.expires_in_secs.saturating_sub(30));
        self.issued_at.elapsed() >= ttl
    }
}

/// OAuth manager with auto-refresh and in-memory caching.
#[derive(Clone)]
pub struct OAuthManager {
    fetcher: TokenFetcher,
    cache: Arc<Mutex<Option<CachedToken>>>,
}

impl OAuthManager {
    pub fn new(fetcher: TokenFetcher) -> Self {
        Self {
            fetcher,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Return a valid access token, refreshing when needed.
    pub async fn access_token(&self) -> Result<String, AgentError> {
        let mut guard = self.cache.lock().await;

        if let Some(cached) = guard.as_ref() {
            if !cached.is_expired() {
                return Ok(cached.token.access_token.clone());
            }
        }

        let refresh = guard.as_ref().and_then(|c| c.token.refresh_token.clone());

        let token = (self.fetcher)(refresh).await?;
        *guard = Some(CachedToken {
            token: token.clone(),
            issued_at: Instant::now(),
        });

        Ok(token.access_token)
    }

    /// Return the Authorization header value.
    pub async fn authorization_header(&self) -> Result<String, AgentError> {
        let token = self.access_token().await?;
        Ok(format!("Bearer {}", token))
    }

    /// Force clear token cache.
    pub async fn clear(&self) {
        let mut guard = self.cache.lock().await;
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cached_credentials_refresh_early_and_survive_failed_refreshes() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::clone(&calls);
        let manager = OAuthManager::new(Arc::new(move |refresh| {
            let received = Arc::clone(&received);
            Box::pin(async move {
                let count = {
                    let mut calls = received.lock().await;
                    calls.push(refresh);
                    calls.len()
                };
                tokio::task::yield_now().await;
                match count {
                    1 => Ok(OAuthToken {
                        access_token: "initial".into(),
                        refresh_token: Some("renewal".into()),
                        expires_in_secs: 5,
                    }),
                    2 => Err(AgentError::LlmApi("fixture refresh failed".into())),
                    3 | 4 => Ok(OAuthToken {
                        access_token: format!("credential-{count}"),
                        refresh_token: None,
                        expires_in_secs: 120,
                    }),
                    _ => panic!("unexpected fetch {count}"),
                }
            })
        }));

        assert_eq!(manager.access_token().await.unwrap(), "initial");
        assert!(matches!(
            manager.access_token().await,
            Err(AgentError::LlmApi(message)) if message == "fixture refresh failed"
        ));
        assert_eq!(
            manager.authorization_header().await.unwrap(),
            "Bearer credential-3"
        );
        assert_eq!(manager.access_token().await.unwrap(), "credential-3");
        manager.clear().await;
        let (access, header) = tokio::join!(manager.access_token(), manager.authorization_header());
        assert_eq!(access.unwrap(), "credential-4");
        assert_eq!(header.unwrap(), "Bearer credential-4");
        assert_eq!(
            calls.lock().await.as_slice(),
            &[None, Some("renewal".into()), Some("renewal".into()), None]
        );
    }

    #[test]
    fn bearer_header_preserves_the_access_value() {
        let credential = OAuthToken {
            access_token: "fixture-value".into(),
            refresh_token: None,
            expires_in_secs: 120,
        };
        assert_eq!(credential.bearer(), "Bearer fixture-value");
    }
}
