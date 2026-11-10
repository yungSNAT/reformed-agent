//! Google Vertex AI provider: Gemini through Vertex's OpenAI-compatible
//! endpoint.
//!
//! Port of upstream `agent/vertex_adapter.py`, the `vertex` model-provider
//! plugin and the Vertex branch of `hermes_cli/runtime_provider.py`. Vertex has
//! no static API key: each request carries a short-lived OAuth2 access token
//! minted from a service-account key or Application Default Credentials (ADC)
//! (see [`credentials`]). Tokens are cached per credential source and re-minted
//! when they are within 5 minutes of expiry, and once more after a 401.
//!
//! Settings, all optional:
//! - `VERTEX_CREDENTIALS_PATH`, then `GOOGLE_APPLICATION_CREDENTIALS`: path to
//!   a service-account key file (a secret pointer; lives in `.env`);
//! - `VERTEX_PROJECT_ID` and `VERTEX_REGION`: routing overrides. The
//!   `vertex.project_id` and `vertex.region` keys in `config.yaml` are bridged
//!   to these variables by `hermes-config`, so the environment wins.

mod credentials;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use hermes_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::provider::GenericProvider;
use credentials::{GoogleCredentials, GOOGLE_OAUTH2_TOKEN_ENDPOINT};

/// Canonical provider id, as upstream registers it.
pub const VERTEX_PROVIDER_ID: &str = "vertex";
/// Upstream's runtime `source` label for Vertex credentials.
pub const VERTEX_AUTH_SOURCE: &str = "vertex-oauth";
pub const VERTEX_DEFAULT_REGION: &str = "global";

/// Upstream's `AuthError` text for unresolved Vertex credentials, without its
/// `pip install` hint (the Rust build needs no extra package).
pub const VERTEX_CREDENTIALS_UNRESOLVED: &str = "Vertex AI credentials could not be resolved. \
     Vertex uses OAuth2 (not a static API key): provide a service-account JSON via \
     GOOGLE_APPLICATION_CREDENTIALS (or VERTEX_CREDENTIALS_PATH) in ~/.hermes/.env, or run \
     'gcloud auth application-default login' for ADC. Set the GCP project/region under vertex: \
     in config.yaml if they aren't embedded in the credentials.";

const REFRESH_MARGIN: Duration = Duration::from_secs(300);
const ADC_CACHE_KEY: &str = "__adc__";

/// Environment lookup, injectable so tests never touch the process
/// environment.
pub(crate) type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

fn env_value(env: &EnvLookup, key: &str) -> Option<String> {
    env(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// OpenAI-compatible base URL for a project and region. The `global`
/// location uses the bare `aiplatform.googleapis.com` host; other regions use
/// `{region}-aiplatform.googleapis.com`.
pub fn build_vertex_base_url(project_id: &str, region: &str) -> String {
    build_base_url_for_origin(&vertex_origin(region), project_id, region)
}

fn vertex_origin(region: &str) -> String {
    if region == VERTEX_DEFAULT_REGION {
        "https://aiplatform.googleapis.com".to_string()
    } else {
        format!("https://{region}-aiplatform.googleapis.com")
    }
}

fn build_base_url_for_origin(origin: &str, project_id: &str, region: &str) -> String {
    format!("{origin}/v1beta1/projects/{project_id}/locations/{region}/endpoints/openapi")
}

/// Fast check, with no network, for whether Vertex credentials appear
/// configured: a resolvable service-account path or an explicit project ID
/// (which implies ADC is intended).
pub fn has_vertex_credentials() -> bool {
    VertexAuth::shared().has_credentials()
}

/// A fresh access token and the base URL it is valid for. Not `Debug`, so the
/// token cannot reach a log through formatting.
pub(crate) struct VertexAccess {
    token: String,
    base_url: String,
}

struct CachedCredentials {
    credentials: GoogleCredentials,
    project_id: Option<String>,
    token: Option<credentials::AccessToken>,
}

impl CachedCredentials {
    fn needs_refresh(&self) -> bool {
        match &self.token {
            None => true,
            Some(token) => token
                .expires_at
                .is_some_and(|at| at.saturating_duration_since(Instant::now()) < REFRESH_MARGIN),
        }
    }
}

/// Resolves Vertex credentials and caches minted tokens per credential source
/// (the service-account path, or ADC).
pub(crate) struct VertexAuth {
    env: EnvLookup,
    http: reqwest::Client,
    google_token_endpoint: String,
    api_origin_override: Option<String>,
    cache: Mutex<HashMap<String, CachedCredentials>>,
}

impl VertexAuth {
    fn with_env(env: EnvLookup) -> Self {
        Self {
            env,
            http: reqwest::Client::new(),
            google_token_endpoint: GOOGLE_OAUTH2_TOKEN_ENDPOINT.to_string(),
            api_origin_override: None,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The process-wide instance, so every provider built for Vertex shares
    /// one token cache, as upstream's module-level cache does.
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<VertexAuth>> = OnceLock::new();
        SHARED
            .get_or_init(|| Arc::new(Self::with_env(Arc::new(|key| std::env::var(key).ok()))))
            .clone()
    }

    fn region(&self) -> String {
        env_value(&self.env, "VERTEX_REGION").unwrap_or_else(|| VERTEX_DEFAULT_REGION.to_string())
    }

    fn project_override(&self) -> Option<String> {
        env_value(&self.env, "VERTEX_PROJECT_ID")
    }

    /// `VERTEX_CREDENTIALS_PATH`, then `GOOGLE_APPLICATION_CREDENTIALS`; each
    /// counts only when the file exists.
    fn credentials_path(&self) -> Option<PathBuf> {
        ["VERTEX_CREDENTIALS_PATH", "GOOGLE_APPLICATION_CREDENTIALS"]
            .into_iter()
            .filter_map(|key| env_value(&self.env, key))
            .map(PathBuf::from)
            .find(|path| path.exists())
    }

    fn has_credentials(&self) -> bool {
        self.credentials_path().is_some() || self.project_override().is_some()
    }

    fn base_url(&self, project_id: &str, region: &str) -> String {
        match &self.api_origin_override {
            Some(origin) => build_base_url_for_origin(origin, project_id, region),
            None => build_vertex_base_url(project_id, region),
        }
    }

    /// Return a token that is not within 5 minutes of expiry, and the base URL
    /// for the resolved project and region.
    pub(crate) async fn access(&self) -> Result<VertexAccess, AgentError> {
        let path = self.credentials_path();
        let cache_key = path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| ADC_CACHE_KEY.to_string());
        let mut cache = self.cache.lock().await;
        let (token, embedded_project) = match self.fresh_token(&mut cache, &cache_key, path).await {
            Ok(resolved) => resolved,
            Err(detail) => {
                cache.remove(&cache_key);
                tracing::error!("Failed to resolve Vertex AI credentials: {detail}");
                return Err(AgentError::AuthFailed(format!(
                    "{VERTEX_CREDENTIALS_UNRESOLVED} Cause: {detail}"
                )));
            }
        };
        let Some(project_id) = self.project_override().or(embedded_project) else {
            return Err(AgentError::AuthFailed(format!(
                "{VERTEX_CREDENTIALS_UNRESOLVED} Cause: no GCP project ID was found; set \
                 vertex.project_id in config.yaml or VERTEX_PROJECT_ID."
            )));
        };
        Ok(VertexAccess {
            token,
            base_url: self.base_url(&project_id, &self.region()),
        })
    }

    async fn fresh_token(
        &self,
        cache: &mut HashMap<String, CachedCredentials>,
        cache_key: &str,
        path: Option<PathBuf>,
    ) -> Result<(String, Option<String>), String> {
        if !cache.contains_key(cache_key) {
            let (credentials, project_id) = match &path {
                Some(path) => credentials::load_service_account_file(path)?,
                None => {
                    credentials::application_default(
                        &self.env,
                        &self.http,
                        self.project_override().is_some(),
                    )
                    .await?
                }
            };
            cache.insert(
                cache_key.to_string(),
                CachedCredentials {
                    credentials,
                    project_id,
                    token: None,
                },
            );
        }
        let entry = cache
            .get_mut(cache_key)
            .ok_or_else(|| "credential cache entry vanished".to_string())?;
        if entry.needs_refresh() {
            let token = entry
                .credentials
                .mint(&self.http, &self.google_token_endpoint)
                .await?;
            tracing::debug!("minted a Vertex AI access token");
            entry.token = Some(token);
        }
        let token = entry
            .token
            .as_ref()
            .map(|token| token.value.clone())
            .ok_or_else(|| "no access token after refresh".to_string())?;
        Ok((token, entry.project_id.clone()))
    }

    /// Drop cached tokens so the next [`Self::access`] mints a new one.
    async fn invalidate_tokens(&self) {
        for entry in self.cache.lock().await.values_mut() {
            entry.token = None;
        }
    }
}

/// Gemini on Vertex AI. Each request mints or reuses a cached OAuth2 token,
/// then sends an OpenAI-compatible chat completion to the project's Vertex
/// endpoint. A 401 re-mints the token and retries once.
#[derive(Clone)]
pub struct VertexProvider {
    auth: Arc<VertexAuth>,
    template: GenericProvider,
}

impl VertexProvider {
    pub fn new(model: impl Into<String>) -> Self {
        Self::with_auth(VertexAuth::shared(), model)
    }

    fn with_auth(auth: Arc<VertexAuth>, model: impl Into<String>) -> Self {
        Self {
            auth,
            template: GenericProvider::new(String::new(), String::new(), model)
                .with_provider_profile(VERTEX_PROVIDER_ID),
        }
    }

    pub fn with_optional_request_timeout_seconds(mut self, seconds: Option<f64>) -> Self {
        self.template = self.template.with_optional_request_timeout_seconds(seconds);
        self
    }

    async fn authorised(&self) -> Result<GenericProvider, AgentError> {
        let access = self.auth.access().await?;
        let mut provider = self.template.clone();
        provider.base_url = access.base_url;
        provider.api_key = access.token;
        Ok(provider)
    }
}

/// The OpenAI-compatible provider reports HTTP failures as `API error <status>`.
fn is_unauthorized(err: &AgentError) -> bool {
    matches!(err, AgentError::LlmApi(message) if message.starts_with("API error 401"))
}

#[async_trait]
impl LlmProvider for VertexProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        let provider = self.authorised().await?;
        let result = provider
            .chat_completion(messages, tools, max_tokens, temperature, model, extra_body)
            .await;
        match result {
            Err(err) if is_unauthorized(&err) => {
                tracing::info!("Vertex AI returned 401; re-minting the OAuth2 token and retrying");
                self.auth.invalidate_tokens().await;
                self.authorised()
                    .await?
                    .chat_completion(messages, tools, max_tokens, temperature, model, extra_body)
                    .await
            }
            other => other,
        }
    }

    fn chat_completion_stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        let this = self.clone();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let model = model.map(str::to_string);
        let extra_body = extra_body.cloned();
        async_stream::stream! {
            let open = |provider: GenericProvider| {
                provider.chat_completion_stream(
                    &messages,
                    &tools,
                    max_tokens,
                    temperature,
                    model.as_deref(),
                    extra_body.as_ref(),
                )
            };
            let mut inner = match this.authorised().await {
                Ok(provider) => open(provider),
                Err(err) => {
                    yield Err(err);
                    return;
                }
            };
            let mut first = inner.next().await;
            if matches!(&first, Some(Err(err)) if is_unauthorized(err)) {
                tracing::info!("Vertex AI returned 401; re-minting the OAuth2 token and retrying");
                this.auth.invalidate_tokens().await;
                inner = match this.authorised().await {
                    Ok(provider) => open(provider),
                    Err(err) => {
                        yield Err(err);
                        return;
                    }
                };
                first = inner.next().await;
            }
            if let Some(item) = first {
                yield item;
            }
            while let Some(item) = inner.next().await {
                yield item;
            }
        }
        .boxed()
    }
}
