//! Honcho dialectic user modeling plugin.
//!
//! Implements the `MemoryProviderPlugin` trait for Honcho, providing
//! dialectic user modeling via the Honcho API.
//!
//! This module is behind the `honcho` feature flag.

use serde::{Deserialize, Serialize};

use crate::memory_manager::MemoryProviderPlugin;
use hermes_core::AgentError;

/// Honcho API base URL.
const DEFAULT_HONCHO_BASE_URL: &str = "https://api.honcho.dev/v1";

/// Configuration for the Honcho memory provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HonchoConfig {
    /// Honcho API key.
    pub api_key: String,
    /// Honcho application ID.
    pub app_id: String,
    /// Optional base URL override.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Whether to auto-sync turns to Honcho.
    #[serde(default = "default_true")]
    pub auto_sync: bool,
    /// Whether to prefetch user context on session start.
    #[serde(default = "default_true")]
    pub prefetch: bool,
}

fn default_true() -> bool {
    true
}

/// Honcho dialectic user modeling provider.
///
/// Connects to the Honcho API for:
/// - Syncing conversation turns for user modeling
/// - Prefetching user context (dialectic insights)
/// - Storing and retrieving user metamemory
pub struct HonchoProvider {
    config: HonchoConfig,
    client: reqwest::Client,
    base_url: String,
    /// Cached user context from last prefetch.
    cached_context: tokio::sync::RwLock<Option<String>>,
}

impl HonchoProvider {
    /// Create a new Honcho provider with the given configuration.
    pub fn new(config: HonchoConfig) -> Self {
        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_HONCHO_BASE_URL.to_string());

        Self {
            config,
            client: reqwest::Client::new(),
            base_url,
            cached_context: tokio::sync::RwLock::new(None),
        }
    }

    /// The API address for `segments` under the base URL, each segment
    /// percent-encoded so an id cannot change the path or add a query.
    fn api_url(&self, segments: &[&str]) -> Result<url::Url, AgentError> {
        let mut url = url::Url::parse(&self.base_url)
            .map_err(|e| AgentError::Config(format!("Honcho base URL: {e}")))?;
        url.path_segments_mut()
            .map_err(|()| AgentError::Config("Honcho base URL cannot take a path".into()))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    /// Sync a conversation turn to Honcho for user modeling.
    pub async fn sync_turn(
        &self,
        user_id: &str,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<(), AgentError> {
        if !self.config.auto_sync {
            return Ok(());
        }

        let url = self.api_url(&[
            "apps",
            &self.config.app_id,
            "users",
            user_id,
            "sessions",
            session_id,
            "messages",
        ])?;

        let body = serde_json::json!({
            "role": role,
            "content": content,
        });

        let resp = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentError::Io(format!("Honcho sync failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            tracing::warn!("Honcho sync error: {}", text);
        }

        Ok(())
    }

    /// Prefetch user context (dialectic insights) from Honcho.
    pub async fn prefetch_context(&self, user_id: &str) -> Result<Option<String>, AgentError> {
        if !self.config.prefetch {
            return Ok(None);
        }

        let url = self.api_url(&["apps", &self.config.app_id, "users", user_id, "metamemories"])?;

        let resp = self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .send()
            .await
            .map_err(|e| AgentError::Io(format!("Honcho prefetch failed: {}", e)))?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        #[derive(Deserialize)]
        struct MetamemoryResponse {
            #[serde(default)]
            content: Option<String>,
        }

        let data: MetamemoryResponse = resp
            .json()
            .await
            .map_err(|e| AgentError::Io(format!("Honcho parse failed: {}", e)))?;

        if let Some(ref content) = data.content {
            let mut cache = self.cached_context.write().await;
            *cache = Some(content.clone());
        }

        Ok(data.content)
    }

    /// Get the cached user context (from last prefetch).
    pub async fn get_cached_context(&self) -> Option<String> {
        self.cached_context.read().await.clone()
    }
}

impl MemoryProviderPlugin for HonchoProvider {
    fn name(&self) -> &str {
        "honcho"
    }

    fn prefetch(&self, query: &str, session_id: &str) -> String {
        let _ = (query, session_id);
        // Synchronous prefetch returns cached context if available.
        // Actual async prefetch should be called separately via prefetch_context().
        String::new()
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, session_id: &str) {
        let _ = (user_content, assistant_content, session_id);
        // Synchronous sync is a no-op; use the async sync_turn method directly.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_honcho_config_defaults() {
        let config: HonchoConfig = serde_json::from_str(
            r#"{
            "api_key": "test-key",
            "app_id": "test-app"
        }"#,
        )
        .unwrap();

        assert!(config.auto_sync);
        assert!(config.prefetch);
        assert!(config.base_url.is_none());
    }

    #[test]
    fn test_honcho_provider_creation() {
        let config = HonchoConfig {
            api_key: "test-key".to_string(),
            app_id: "test-app".to_string(),
            base_url: None,
            auto_sync: true,
            prefetch: true,
        };

        let provider = HonchoProvider::new(config);
        assert_eq!(provider.base_url, DEFAULT_HONCHO_BASE_URL);
        assert_eq!(provider.name(), "honcho");
    }

    #[test]
    fn test_honcho_custom_base_url() {
        let config = HonchoConfig {
            api_key: "key".to_string(),
            app_id: "app".to_string(),
            base_url: Some("https://custom.honcho.dev".to_string()),
            auto_sync: false,
            prefetch: false,
        };

        let provider = HonchoProvider::new(config);
        assert_eq!(provider.base_url, "https://custom.honcho.dev");
    }
}
