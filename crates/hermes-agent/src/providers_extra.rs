//! Additional LLM provider implementations for non-standard APIs.
//!
//! - [`QwenProvider`]: Alibaba Tongyi Qianwen (通义千问)
//! - [`KimiProvider`]: Moonshot AI (月之暗面)
//! - [`MiniMaxProvider`]: MiniMax AI
//! - [`NousProvider`]: Nous Research
//! - [`CopilotProvider`]: GitHub Copilot HTTP

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::Value;

use hermes_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema};

use crate::api_bridge::CodexProvider;
use crate::provider::{is_openai_dynamic_model_alias, GenericProvider};
use crate::provider_profiles;

fn openrouter_compatible_extra_body(extra_body: Option<&Value>) -> Option<Value> {
    let Some(Value::Object(map)) = extra_body else {
        return extra_body.cloned();
    };

    let mut cleaned = map.clone();

    // Nous' inference API is OpenRouter-compatible. Use the documented
    // cross-provider reasoning object instead of leaking provider-specific or
    // Hermes-local control keys into the request body.
    if !cleaned.contains_key("reasoning") {
        if let Some(effort) = cleaned.remove("reasoning_effort") {
            cleaned.insert(
                "reasoning".to_string(),
                serde_json::json!({ "effort": effort }),
            );
        }
    } else {
        cleaned.remove("reasoning_effort");
    }

    Some(Value::Object(cleaned))
}

// ---------------------------------------------------------------------------
// QwenProvider — Alibaba Tongyi Qianwen (通义千问)
// ---------------------------------------------------------------------------

/// Alibaba Tongyi Qianwen provider via the DashScope OpenAI-compatible endpoint.
///
/// Default base URL: `https://dashscope.aliyuncs.com/compatible-mode/v1`
/// Default model: `qwen-turbo`
#[derive(Debug, Clone)]
pub struct QwenProvider {
    inner: GenericProvider,
}

impl QwenProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new(
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
                api_key,
                "qwen-turbo",
            )
            .with_provider_profile("qwen-oauth"),
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    pub fn with_base_url(self, url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(url),
        }
    }

    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
        }
    }
}

#[async_trait]
impl LlmProvider for QwenProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.inner
            .chat_completion(messages, tools, max_tokens, temperature, model, extra_body)
            .await
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
        self.inner.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            extra_body,
        )
    }
}

// ---------------------------------------------------------------------------
// KimiProvider — Moonshot AI (月之暗面)
// ---------------------------------------------------------------------------

/// Moonshot AI / Kimi provider.
///
/// Default base URL: `https://api.moonshot.ai/v1`
/// Default model: `kimi-k2-turbo-preview`
#[derive(Debug, Clone)]
pub struct KimiProvider {
    inner: GenericProvider,
}

impl KimiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new(
                provider_profiles::KIMI_LEGACY_BASE_URL,
                api_key,
                "kimi-k2-turbo-preview",
            )
            .with_provider_profile("kimi-coding"),
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    pub fn with_base_url(self, url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(url),
        }
    }

    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
        }
    }
}

#[async_trait]
impl LlmProvider for KimiProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.inner
            .chat_completion(messages, tools, max_tokens, temperature, model, extra_body)
            .await
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
        self.inner.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            extra_body,
        )
    }
}

// ---------------------------------------------------------------------------
// MiniMaxProvider
// ---------------------------------------------------------------------------

/// MiniMax provider.
///
/// Default base URL: `https://api.minimax.chat/v1`
/// Default model: `abab6.5s-chat`
#[derive(Debug, Clone)]
pub struct MiniMaxProvider {
    inner: GenericProvider,
}

impl MiniMaxProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new("https://api.minimax.chat/v1", api_key, "abab6.5s-chat")
                .with_provider_profile("minimax"),
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    pub fn with_base_url(self, url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(url),
        }
    }

    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
        }
    }
}

#[async_trait]
impl LlmProvider for MiniMaxProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.inner
            .chat_completion(messages, tools, max_tokens, temperature, model, extra_body)
            .await
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
        self.inner.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            extra_body,
        )
    }
}

// ---------------------------------------------------------------------------
// NousProvider — Nous Research
// ---------------------------------------------------------------------------

/// Nous Research inference API provider.
///
/// Default base URL: `https://inference-api.nousresearch.com/v1`
/// Default model: `hermes-3-llama-3.1-405b`
#[derive(Debug, Clone)]
pub struct NousProvider {
    inner: GenericProvider,
}

impl NousProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: GenericProvider::new(
                "https://inference-api.nousresearch.com/v1",
                api_key,
                "hermes-3-llama-3.1-405b",
            )
            .with_provider_profile("nous"),
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_model(model),
        }
    }

    pub fn with_base_url(self, url: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_base_url(url),
        }
    }

    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
        }
    }
}

#[async_trait]
impl LlmProvider for NousProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        let extra_body = openrouter_compatible_extra_body(extra_body);
        self.inner
            .chat_completion(
                messages,
                tools,
                max_tokens,
                temperature,
                model,
                extra_body.as_ref(),
            )
            .await
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
        let extra_body = openrouter_compatible_extra_body(extra_body);
        self.inner.chat_completion_stream(
            messages,
            tools,
            max_tokens,
            temperature,
            model,
            extra_body.as_ref(),
        )
    }
}

// ---------------------------------------------------------------------------
// CopilotProvider — GitHub Copilot HTTP
// ---------------------------------------------------------------------------

#[path = "providers_extra/copilot_catalog.rs"]
mod copilot_catalog;

use copilot_catalog::{reconcile_effort, CopilotCatalog, EffortSupport};

/// GitHub Copilot HTTP provider with a configurable inference base URL.
/// GPT-5 and later models go to its Responses API, the rest to Chat
/// Completions.
///
/// Default model: `gpt-4o`
#[derive(Debug, Clone)]
pub struct CopilotProvider {
    inner: GenericProvider,
    responses: CodexProvider,
    catalog: Option<Arc<CopilotCatalog>>,
}

impl CopilotProvider {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let inner = GenericProvider::new(base_url, api_key, "gpt-4o");
        let responses = CodexProvider::new(inner.api_key.clone())
            .with_base_url(inner.base_url.clone())
            .with_model(inner.model.clone());
        Self {
            inner,
            responses,
            catalog: None,
        }
    }

    pub fn with_model(self, model: impl Into<String>) -> Self {
        let model = model.into();
        Self {
            inner: self.inner.with_model(model.clone()),
            responses: self.responses.with_model(model),
            ..self
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.inner = self.inner.with_base_url(url);
        self.responses = self.responses.with_base_url(self.inner.base_url.clone());
        if self.catalog.is_some() {
            self.catalog = CopilotCatalog::for_base_url(&self.inner.base_url);
        }
        self
    }

    /// Reconcile requested reasoning effort against the account's catalogue.
    /// Lookups use the same recognised HTTPS origin and credential as inference.
    /// A disabled, missing or unavailable lookup leaves inference available.
    pub fn with_model_catalog(mut self) -> Self {
        if !matches!(self.inner.api_key.trim(), "" | "local-no-key") {
            self.catalog = CopilotCatalog::for_base_url(&self.inner.base_url);
        }
        self
    }

    pub fn with_optional_request_timeout_seconds(self, seconds: Option<f64>) -> Self {
        Self {
            inner: self.inner.with_optional_request_timeout_seconds(seconds),
            responses: self
                .responses
                .with_optional_request_timeout_seconds(seconds),
            ..self
        }
    }

    /// Whether the request's wire model answers only on the Responses API.
    fn uses_responses_api(&self, model: Option<&str>, extra_body: Option<&Value>) -> bool {
        let wire_model = match extra_body.and_then(|body| body.get("model")) {
            Some(Value::String(model)) => model,
            _ => model.unwrap_or(&self.inner.model),
        };
        requires_responses_api(wire_model)
    }

    async fn reconcile_reasoning<'a>(
        &self,
        model: Option<&str>,
        extra_body: Option<&'a Value>,
    ) -> Option<Cow<'a, Value>> {
        let body = extra_body?;
        let Some((catalog, effort)) = self.catalog.as_ref().zip(requested_catalog_effort(body))
        else {
            return Some(Cow::Borrowed(body));
        };
        let wire_model = match body.get("model") {
            Some(Value::String(model)) => model,
            Some(_) => return Some(Cow::Borrowed(body)),
            None => model.unwrap_or(&self.inner.model).trim(),
        };
        if is_openai_dynamic_model_alias(wire_model) {
            return Some(Cow::Borrowed(body));
        }
        let support = catalog
            .effort_support(&self.inner.api_key, wire_model)
            .await;
        let replacement = match support {
            EffortSupport::Unknown => return Some(Cow::Borrowed(body)),
            EffortSupport::Unsupported => None,
            EffortSupport::Supported(levels) => {
                let Some(value) = reconcile_effort(effort, &levels) else {
                    return Some(Cow::Borrowed(body));
                };
                if body.get("reasoning_effort").and_then(Value::as_str) == Some(value.as_str()) {
                    return Some(Cow::Borrowed(body));
                }
                Some(value)
            }
        };
        let mut adjusted = body.as_object()?.clone();
        if let Some(value) = replacement {
            adjusted.insert("reasoning_effort".into(), Value::String(value));
        } else {
            adjusted.remove("reasoning_effort");
        }
        Some(Cow::Owned(Value::Object(adjusted)))
    }
}

fn requested_catalog_effort(body: &Value) -> Option<&str> {
    if [
        "/reasoning/enabled",
        "/reasoning_config/enabled",
        "/supports_reasoning",
    ]
    .iter()
    .any(|field| body.pointer(field).and_then(Value::as_bool) == Some(false))
    {
        return None;
    }
    body.get("reasoning_effort")?
        .as_str()
        .map(str::trim)
        .filter(|effort| !effort.is_empty() && !effort.eq_ignore_ascii_case("none"))
}

/// GPT-5 and later answer only on Copilot's Responses API, except the
/// `gpt-5-mini` family; other models use Chat Completions.
fn requires_responses_api(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    let name = model.strip_prefix("openai/").unwrap_or(&model);
    let Some(version) = name.strip_prefix("gpt-") else {
        return false;
    };
    let major = &version[..version
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(version.len())];
    major.parse::<u32>().is_ok_and(|major| major >= 5) && !name.starts_with("gpt-5-mini")
}

/// The Chat Completions extra body as the Responses API takes it: the
/// effort, when one is asked for, goes in `reasoning` with a summary, as
/// the reasoning text comes back only with one.
fn responses_extra_body(body: Option<&Value>) -> Option<Value> {
    let Some(Value::Object(map)) = body else {
        return body.cloned();
    };
    let effort = body.and_then(requested_catalog_effort).map(str::to_owned);
    let mut map = map.clone();
    for key in [
        "reasoning_effort",
        "reasoning",
        "reasoning_config",
        "supports_reasoning",
    ] {
        map.remove(key);
    }
    // The Responses API names its output limit `max_output_tokens`.
    for limit in ["max_completion_tokens", "max_tokens"] {
        if let Some(value) = map.remove(limit) {
            map.entry("max_output_tokens").or_insert(value);
        }
    }
    if let Some(effort) = effort {
        map.insert(
            "reasoning".into(),
            serde_json::json!({"effort": effort, "summary": "auto"}),
        );
    }
    Some(Value::Object(map))
}

#[async_trait]
impl LlmProvider for CopilotProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        let adjusted = self.reconcile_reasoning(model, extra_body).await;
        if self.uses_responses_api(model, extra_body) {
            let body = responses_extra_body(adjusted.as_deref());
            return self
                .responses
                .chat_completion(
                    messages,
                    tools,
                    max_tokens,
                    temperature,
                    model,
                    body.as_ref(),
                )
                .await;
        }
        self.inner
            .chat_completion(
                messages,
                tools,
                max_tokens,
                temperature,
                model,
                adjusted.as_deref(),
            )
            .await
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
        let responses = self.uses_responses_api(model, extra_body);
        if !responses
            && (self.catalog.is_none() || extra_body.and_then(requested_catalog_effort).is_none())
        {
            return self.inner.chat_completion_stream(
                messages,
                tools,
                max_tokens,
                temperature,
                model,
                extra_body,
            );
        }

        let provider = self.clone();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let model = model.map(str::to_owned);
        let extra_body = extra_body.cloned();
        Box::pin(async_stream::stream! {
            let adjusted = provider
                .reconcile_reasoning(model.as_deref(), extra_body.as_ref())
                .await;
            let mut stream = if responses {
                let body = responses_extra_body(adjusted.as_deref());
                provider.responses.chat_completion_stream(
                    &messages,
                    &tools,
                    max_tokens,
                    temperature,
                    model.as_deref(),
                    body.as_ref(),
                )
            } else {
                provider.inner.chat_completion_stream(
                    &messages,
                    &tools,
                    max_tokens,
                    temperature,
                    model.as_deref(),
                    adjusted.as_deref(),
                )
            };
            while let Some(chunk) = stream.next().await {
                yield chunk;
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn qwen_provider_defaults() {
        let p = QwenProvider::new("test-key");
        assert_eq!(
            p.inner.base_url,
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
        assert_eq!(p.inner.model, "qwen-turbo");
    }

    #[test]
    fn qwen_provider_with_model() {
        let p = QwenProvider::new("test-key").with_model("qwen-max");
        assert_eq!(p.inner.model, "qwen-max");
    }

    #[test]
    fn kimi_provider_defaults() {
        let p = KimiProvider::new("test-key");
        assert_eq!(p.inner.base_url, provider_profiles::KIMI_LEGACY_BASE_URL);
        assert_eq!(p.inner.model, "kimi-k2-turbo-preview");
    }

    #[test]
    fn kimi_provider_with_model() {
        let p = KimiProvider::new("test-key").with_model("moonshot-v1-128k");
        assert_eq!(p.inner.model, "moonshot-v1-128k");
    }

    #[test]
    fn minimax_provider_defaults() {
        let p = MiniMaxProvider::new("test-key");
        assert_eq!(p.inner.base_url, "https://api.minimax.chat/v1");
        assert_eq!(p.inner.model, "abab6.5s-chat");
    }

    #[test]
    fn nous_provider_defaults() {
        let p = NousProvider::new("test-key");
        assert_eq!(
            p.inner.base_url,
            "https://inference-api.nousresearch.com/v1"
        );
        assert_eq!(p.inner.model, "hermes-3-llama-3.1-405b");
    }

    #[test]
    fn nous_extra_body_uses_openrouter_reasoning_shape_and_preserves_local_controls() {
        let extra = serde_json::json!({
            "strict_api": true,
            "provider_strict": true,
            "reasoning_effort": "xhigh",
            "temperature": 0.1
        });
        let cleaned = openrouter_compatible_extra_body(Some(&extra)).expect("cleaned body");
        assert_eq!(cleaned["strict_api"], true);
        assert_eq!(cleaned["provider_strict"], true);
        assert!(cleaned.get("reasoning_effort").is_none());
        assert_eq!(cleaned["reasoning"]["effort"], "xhigh");
        assert_eq!(cleaned["temperature"], 0.1);
    }

    #[test]
    fn copilot_provider_custom_base_url() {
        let p = CopilotProvider::new("https://copilot.example.com/v1", "token");
        assert_eq!(p.inner.base_url, "https://copilot.example.com/v1");
        assert_eq!(p.inner.model, "gpt-4o");
    }

    #[test]
    fn copilot_provider_with_model() {
        let p = CopilotProvider::new("https://copilot.example.com/v1", "token")
            .with_model("gpt-4o-mini");
        assert_eq!(p.inner.model, "gpt-4o-mini");
    }

    #[test]
    fn openai_compatible_extra_providers_apply_request_timeout_seconds() {
        let timeout = Some(Duration::from_secs(45));

        assert_eq!(
            QwenProvider::new("test-key")
                .with_optional_request_timeout_seconds(Some(45.0))
                .inner
                .configured_request_timeout(),
            timeout
        );
        assert_eq!(
            KimiProvider::new("test-key")
                .with_optional_request_timeout_seconds(Some(45.0))
                .inner
                .configured_request_timeout(),
            timeout
        );
        assert_eq!(
            MiniMaxProvider::new("test-key")
                .with_optional_request_timeout_seconds(Some(45.0))
                .inner
                .configured_request_timeout(),
            timeout
        );
        assert_eq!(
            NousProvider::new("test-key")
                .with_optional_request_timeout_seconds(Some(45.0))
                .inner
                .configured_request_timeout(),
            timeout
        );
        assert_eq!(
            CopilotProvider::new("https://copilot.example.com/v1", "token")
                .with_optional_request_timeout_seconds(Some(45.0))
                .inner
                .configured_request_timeout(),
            timeout
        );
    }
}

#[cfg(test)]
#[path = "providers_extra/copilot_catalog_tests.rs"]
mod copilot_catalog_tests;
