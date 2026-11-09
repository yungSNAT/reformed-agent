//! Rust-native request-profile contracts for OpenAI-compatible providers.
//!
//! Upstream Python keeps this behavior in provider plugin profiles. Hermes
//! Ultra runs the agent loop in Rust, so the request-shaping contracts live
//! here instead of importing Python provider plugins at runtime.

use serde_json::{Map, Value};

use crate::local_backends;

pub const NOUS_PRODUCT_TAG: &str = "product=hermes-agent";
pub const NOUS_CLIENT_TAG_PREFIX: &str = "client=hermes-client-v";
pub const KIMI_CODE_BASE_URL: &str = "https://api.kimi.com/coding/v1";
pub const KIMI_LEGACY_BASE_URL: &str = "https://api.moonshot.ai/v1";
pub const KIMI_CN_BASE_URL: &str = "https://api.moonshot.cn/v1";
pub const KIMI_CODE_USER_AGENT: &str = "KimiCLI/1.0";
pub const GEMINI_NATIVE_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const GEMINI_OPENAI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
pub const DEFAULT_NOUS_INFERENCE_URL: &str = "https://inference-api.nousresearch.com/v1";
pub const STEPFUN_BASE_URL: &str = "https://api.stepfun.ai/step_plan/v1";
pub const QWEN_BASE_URL: &str = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1";
pub const ALIBABA_CODING_PLAN_BASE_URL: &str = "https://coding-intl.dashscope.aliyuncs.com/v1";
pub const GOOGLE_GEMINI_CLI_BASE_URL: &str = "cloudcode-pa://google";
pub const AI_GATEWAY_BASE_URL: &str = "https://ai-gateway.vercel.sh/v1";
pub const MINIMAX_CN_BASE_URL: &str = "https://api.minimaxi.com/anthropic";
pub const NOVITA_BASE_URL: &str = "https://api.novita.ai/openai/v1";
pub const XAI_BASE_URL: &str = "https://api.x.ai/v1";
pub const NVIDIA_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
pub const COPILOT_BASE_URL: &str = "https://api.githubcopilot.com";
pub const OPENCODE_GO_BASE_URL: &str = "https://opencode.ai/zen/go/v1";
pub const OPENCODE_ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";
pub const KILOCODE_BASE_URL: &str = "https://api.kilo.ai/api/gateway";
pub const HUGGINGFACE_BASE_URL: &str = "https://router.huggingface.co/v1";
pub const GMI_BASE_URL: &str = "https://api.gmi-serving.com/v1";
pub const XIAOMI_BASE_URL: &str = "https://api.xiaomimimo.com/v1";
pub const ZAI_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
pub const ARCEE_BASE_URL: &str = "https://api.arcee.ai/api/v1";
pub const TENCENT_TOKENHUB_BASE_URL: &str = "https://tokenhub.tencentmaas.com/v1";
pub const OLLAMA_CLOUD_BASE_URL: &str = "https://ollama.com/v1";
pub const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1";

pub fn is_kimi_code_base_url(base_url: &str) -> bool {
    base_url
        .trim()
        .to_ascii_lowercase()
        .contains("api.kimi.com")
}

pub fn is_native_gemini_base_url(base_url: &str) -> bool {
    let normalized = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    normalized.contains("generativelanguage.googleapis.com") && !normalized.contains("/openai")
}

pub fn gemini_openai_compatible_base_url(base_url: &str) -> String {
    if is_native_gemini_base_url(base_url) {
        format!("{}/openai", base_url.trim().trim_end_matches('/'))
    } else {
        base_url.trim().trim_end_matches('/').to_string()
    }
}

pub fn kimi_base_url_for_api_key(api_key: &str, legacy_default: &str) -> &'static str {
    if api_key.trim().starts_with("sk-kimi-") {
        KIMI_CODE_BASE_URL
    } else if legacy_default.trim().is_empty() {
        KIMI_LEGACY_BASE_URL
    } else if legacy_default.trim() == KIMI_CN_BASE_URL {
        KIMI_CN_BASE_URL
    } else {
        KIMI_LEGACY_BASE_URL
    }
}

pub fn canonical_provider_profile_id(provider: &str) -> Option<&'static str> {
    let normalized = provider.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    match normalized.as_str() {
        "nvidia" | "nvidia-nim" => Some("nvidia"),
        "kimi" | "moonshot" | "moonshot-ai" | "kimi-coding" => Some("kimi-coding"),
        "kimi-coding-cn" | "kimi-cn" | "moonshot-cn" => Some("kimi-coding-cn"),
        "openrouter" | "or" => Some("openrouter"),
        "minimax" | "mini-max" | "minimax-cn" | "minimax_cn" | "minimax-china"
        | "minimax-oauth" | "minimax_oauth" => Some("minimax"),
        "nous" | "nous-portal" | "nous-api" | "nous_api" | "nousapi" | "nous-portal-api" => {
            Some("nous")
        }
        "qwen" | "qwen-oauth" | "qwen-portal" | "qwen-cli" => Some("qwen-oauth"),
        "xiaomi" | "mimo" | "xiaomi-mimo" => Some("xiaomi"),
        "custom"
        | "ollama"
        | "ollama-local"
        | "llama-cpp"
        | "llamafile"
        | "vllm"
        | "mlx"
        | "mlx-lm"
        | "apple-mlx"
        | "vmlx"
        | "omlx"
        | "mlx-vlm"
        | "mlxvlm"
        | "mlx-openai-server"
        | "apple-ane"
        | "sglang"
        | "tgi"
        | "lmstudio"
        | "lm-studio"
        | "lm_studio"
        | "lmdeploy"
        | "lm-deploy"
        | "localai"
        | "local-ai"
        | "koboldcpp"
        | "kobold-cpp"
        | "text-generation-webui"
        | "text-generation-web-ui"
        | "oobabooga"
        | "textgen-webui"
        | "tabbyapi"
        | "tabby-api"
        | "exllama"
        | "exllamav2" => Some("custom"),
        _ => None,
    }
}

pub fn default_max_tokens(profile: &str) -> Option<u32> {
    match canonical_provider_profile_id(profile)? {
        "nvidia" => Some(16_384),
        "kimi-coding" | "kimi-coding-cn" => Some(32_000),
        "qwen-oauth" => Some(65_536),
        "custom" => Some(65_536),
        _ => None,
    }
}

pub fn omit_temperature(profile: &str) -> bool {
    matches!(
        canonical_provider_profile_id(profile),
        Some("kimi-coding" | "kimi-coding-cn")
    )
}

pub fn supports_vision(profile: &str) -> bool {
    matches!(
        canonical_provider_profile_id(profile),
        Some("anthropic" | "nous" | "openrouter" | "qwen-oauth" | "xiaomi")
    )
}

pub fn supports_vision_tool_messages(profile: &str) -> bool {
    !matches!(canonical_provider_profile_id(profile), Some("xiaomi"))
}

pub fn profile_auth_type(profile: &str) -> Option<&'static str> {
    match canonical_provider_profile_id(profile)? {
        "nous" => Some("oauth_device_code"),
        "qwen-oauth" => Some("oauth_external"),
        "nvidia" | "kimi-coding" | "kimi-coding-cn" | "openrouter" | "minimax" | "xiaomi"
        | "custom" => Some("api_key"),
        _ => None,
    }
}

pub fn profile_base_url(profile: &str) -> Option<&'static str> {
    match canonical_provider_profile_id(profile)? {
        "nvidia" => Some("https://integrate.api.nvidia.com/v1"),
        "kimi-coding" => Some(KIMI_CODE_BASE_URL),
        "kimi-coding-cn" => Some(KIMI_CN_BASE_URL),
        "openrouter" => Some("https://openrouter.ai/api/v1"),
        "minimax" => Some("https://api.minimax.io/anthropic"),
        "nous" => Some("https://inference-api.nousresearch.com/v1"),
        "qwen-oauth" => Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
        "xiaomi" => Some("https://api.xiaomimimo.com/v1"),
        _ => None,
    }
}

/// Canonical runtime name for a provider alias, shared by the primary provider
/// builder and the agent loop's runtime routes.
pub fn normalize_runtime_provider_name(provider: &str) -> String {
    let normalized = provider.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "codex" => "openai-codex".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "mixture" | "mixture-of-agents" | "mixture_of_agents" => "moa".to_string(),
        "nous_api" | "nousapi" | "nous-portal-api" => "nous-api".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
        "azure" | "azure-ai-foundry" | "azure_ai_foundry" => "azure-foundry".to_string(),
        "step" | "step-plan" => "stepfun".to_string(),
        "moonshot" | "kimi-coding" | "kimi-coding-cn" => "kimi".to_string(),
        "alibaba" | "alibaba-coding-plan" => "qwen".to_string(),
        "minimax-cn" => "minimax".to_string(),
        "novita-ai" | "novitaai" => "novita".to_string(),
        "glm" | "z-ai" | "z_ai" | "zhipu" => "zai".to_string(),
        "aigateway" | "vercel" => "ai-gateway".to_string(),
        "github-copilot" | "github-models" => "copilot".to_string(),
        "github-copilot-acp" | "copilot-acp-agent" => "copilot-acp".to_string(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".to_string(),
        "gmi-cloud" | "gmicloud" => "gmi".to_string(),
        "arcee-ai" | "arceeai" => "arcee".to_string(),
        "mimo" | "xiaomi-mimo" => "xiaomi".to_string(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".to_string(),
        "aws" | "aws-bedrock" | "amazon-bedrock" | "amazon" => "bedrock".to_string(),
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "opencode" | "opencode-zen" | "zen" => "opencode-zen".to_string(),
        "go" => "opencode-go".to_string(),
        local if local_backends::local_backend_spec(local).is_some() => {
            local_backends::local_backend_spec(local)
                .expect("local backend spec checked")
                .provider
                .to_string()
        }
        _ => normalized,
    }
}

/// Base URL that the provider's environment variable selects, if set. The
/// primary provider builder and the agent loop's runtime routes both read it,
/// so a rerouted turn addresses the same endpoint as the primary.
pub fn provider_base_url_from_env(provider: &str) -> Option<String> {
    let raw_provider = provider.trim().to_ascii_lowercase();
    let normalized_provider = normalize_runtime_provider_name(raw_provider.as_str());
    if let Some(url) = local_backends::local_backend_base_url_from_env(raw_provider.as_str())
        .or_else(|| local_backends::local_backend_base_url_from_env(normalized_provider.as_str()))
    {
        return Some(url);
    }
    let env_var = match raw_provider.as_str() {
        "minimax-cn" | "minimax_cn" => "MINIMAX_CN_BASE_URL",
        _ => match normalized_provider.as_str() {
            "openai" => "OPENAI_BASE_URL",
            "openai-codex" | "codex" => "HERMES_OPENAI_CODEX_BASE_URL",
            "nous-api" => "NOUS_BASE_URL",
            "anthropic" => "ANTHROPIC_BASE_URL",
            "bedrock" => "BEDROCK_BASE_URL",
            "google-gemini-cli" => "HERMES_GEMINI_BASE_URL",
            "gemini" | "google" => "GEMINI_BASE_URL",
            "qwen" => "DASHSCOPE_BASE_URL",
            "qwen-oauth" => "HERMES_QWEN_BASE_URL",
            "stepfun" => "STEPFUN_BASE_URL",
            "ai-gateway" => "AI_GATEWAY_BASE_URL",
            "kimi" => "KIMI_BASE_URL",
            "minimax" => "MINIMAX_BASE_URL",
            "novita" => "NOVITA_BASE_URL",
            "xai" => "XAI_BASE_URL",
            "nvidia" => "NVIDIA_BASE_URL",
            "copilot" => "COPILOT_API_BASE_URL",
            "opencode-go" => "OPENCODE_GO_BASE_URL",
            "opencode-zen" => "OPENCODE_ZEN_BASE_URL",
            "kilocode" => "KILOCODE_BASE_URL",
            "huggingface" => "HF_BASE_URL",
            "gmi" => "GMI_BASE_URL",
            "xiaomi" => "XIAOMI_BASE_URL",
            "zai" => "GLM_BASE_URL",
            "arcee" => "ARCEE_BASE_URL",
            "tencent-tokenhub" => "TOKENHUB_BASE_URL",
            "deepseek" => "DEEPSEEK_BASE_URL",
            _ => return None,
        },
    };
    std::env::var(env_var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Default base URL for a provider with no configured or environment URL.
/// The primary provider builder and the agent loop's runtime routes share it,
/// so a route never falls back to another provider's host.
pub fn provider_default_base_url(provider: &str) -> Option<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai-codex" | "codex" => Some(crate::provider::OPENAI_CODEX_BASE_URL),
        "nous-api" | "nous_api" | "nousapi" | "nous-portal-api" => Some(DEFAULT_NOUS_INFERENCE_URL),
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => Some(GOOGLE_GEMINI_CLI_BASE_URL),
        "gemini" | "google" | "google-gemini" | "google-ai-studio" => Some(GEMINI_OPENAI_BASE_URL),
        "qwen" | "alibaba" => Some(QWEN_BASE_URL),
        "alibaba-coding-plan" => Some(ALIBABA_CODING_PLAN_BASE_URL),
        "stepfun" | "step" | "step-plan" => Some(STEPFUN_BASE_URL),
        "ai-gateway" | "aigateway" | "vercel" => Some(AI_GATEWAY_BASE_URL),
        "kimi-coding" => Some(KIMI_CODE_BASE_URL),
        "kimi" | "moonshot" => Some(KIMI_LEGACY_BASE_URL),
        "kimi-coding-cn" => Some(KIMI_CN_BASE_URL),
        "minimax-cn" | "minimax_cn" => Some(MINIMAX_CN_BASE_URL),
        "novita" | "novita-ai" | "novitaai" => Some(NOVITA_BASE_URL),
        "xai" => Some(XAI_BASE_URL),
        "nvidia" => Some(NVIDIA_BASE_URL),
        "copilot" | "github-copilot" | "github-models" => Some(COPILOT_BASE_URL),
        "opencode-go" => Some(OPENCODE_GO_BASE_URL),
        "opencode-zen" | "opencode" => Some(OPENCODE_ZEN_BASE_URL),
        "kilocode" | "kilo" => Some(KILOCODE_BASE_URL),
        "huggingface" | "hf" | "hugging-face" | "huggingface-hub" => Some(HUGGINGFACE_BASE_URL),
        "gmi" | "gmi-cloud" | "gmicloud" => Some(GMI_BASE_URL),
        "xiaomi" | "mimo" | "xiaomi-mimo" => Some(XIAOMI_BASE_URL),
        "zai" | "glm" | "z-ai" | "z_ai" | "zhipu" => Some(ZAI_BASE_URL),
        "arcee" | "arcee-ai" | "arceeai" => Some(ARCEE_BASE_URL),
        "tencent-tokenhub" | "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => {
            Some(TENCENT_TOKENHUB_BASE_URL)
        }
        "ollama-cloud" => Some(OLLAMA_CLOUD_BASE_URL),
        "deepseek" => Some(DEEPSEEK_BASE_URL),
        local if local_backends::local_backend_spec(local).is_some() => {
            local_backends::local_backend_default_base_url(local)
        }
        _ => None,
    }
}

/// A configured key: a literal, or a `${ENV}` reference to an environment variable.
pub fn resolve_api_key_literal_or_env_ref(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(env_ref) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return std::env::var(env_ref).ok().filter(|v| !v.trim().is_empty());
    }
    Some(trimmed.to_string())
}

/// Resolve API key / token for a named LLM provider from well-known environment variables.
pub fn provider_api_key_from_env(provider: &str) -> Option<String> {
    let raw_provider = provider.trim().to_ascii_lowercase();
    if raw_provider == "kimi-coding-cn" {
        return ["KIMI_CN_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"]
            .iter()
            .find_map(|env_var| {
                std::env::var(env_var)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });
    }
    if matches!(raw_provider.as_str(), "minimax-cn" | "minimax_cn") {
        return std::env::var("MINIMAX_CN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("MINIMAX_API_KEY").ok())
            .filter(|s| !s.trim().is_empty());
    }
    let provider = normalize_runtime_provider_name(raw_provider.as_str());
    if let Some(api_key) = local_backends::local_backend_api_key_from_env(raw_provider.as_str())
        .or_else(|| local_backends::local_backend_api_key_from_env(provider.as_str()))
    {
        return Some(api_key);
    }
    match provider.as_str() {
        "openai" => std::env::var("HERMES_OPENAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "openai-codex" | "codex" => std::env::var("HERMES_OPENAI_CODEX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "anthropic" | "claude" | "claude-code" => std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ANTHROPIC_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok())
            .filter(|s| !s.trim().is_empty()),
        "bedrock" => Some(crate::bedrock::BEDROCK_AUTH_MARKER.to_string()),
        // Upstream `hermes_cli/auth.py` PROVIDER_REGISTRY["gemini"] reads these in this order.
        "gemini" => std::env::var("GOOGLE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("GEMINI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
            std::env::var("HERMES_GEMINI_OAUTH_API_KEY")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
                .filter(|s| !s.trim().is_empty())
                .or_else(|| std::env::var("GEMINI_API_KEY").ok())
                .filter(|s| !s.trim().is_empty())
        }
        "openrouter" => std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "qwen" => std::env::var("DASHSCOPE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "qwen-oauth" => std::env::var("HERMES_QWEN_OAUTH_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("DASHSCOPE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "kimi" | "moonshot" => {
            let env_vars: &[&str] = if raw_provider == "kimi-coding" {
                &["KIMI_CODING_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"]
            } else {
                &[
                    "KIMI_API_KEY",
                    "KIMI_CODING_API_KEY",
                    "MOONSHOT_API_KEY",
                    "KIMI_CN_API_KEY",
                ]
            };
            env_vars.iter().find_map(|env_var| {
                std::env::var(env_var)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
        }
        "minimax" => std::env::var("MINIMAX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("MINIMAX_CN_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "stepfun" => std::env::var("HERMES_STEPFUN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("STEPFUN_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "novita" => std::env::var("NOVITA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "nous" | "nous-api" => std::env::var("NOUS_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "copilot" => std::env::var("COPILOT_GITHUB_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("GH_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("GITHUB_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("GITHUB_COPILOT_TOKEN").ok())
            .filter(|s| !s.trim().is_empty()),
        "ai-gateway" => std::env::var("AI_GATEWAY_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "arcee" => std::env::var("ARCEEAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ARCEE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "deepseek" => std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "huggingface" => std::env::var("HF_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "gmi" => std::env::var("GMI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "kilocode" => std::env::var("KILOCODE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "nvidia" => std::env::var("NVIDIA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "ollama-cloud" => std::env::var("OLLAMA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "opencode-go" => std::env::var("OPENCODE_GO_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "opencode-zen" => std::env::var("OPENCODE_ZEN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "xai" => std::env::var("XAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "xiaomi" => std::env::var("XIAOMI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "tencent-tokenhub" => std::env::var("TOKENHUB_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "zai" => std::env::var("GLM_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ZAI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("Z_AI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        _ => None,
    }
}

/// Credential order shared by the primary provider builder and the agent
/// loop's runtime routes, so a route sends the credential the primary would:
/// the configured key, the configured key variable, the provider's environment
/// variables, then a stored OAuth token. Each step tries every name in
/// `provider_names` (the configured name, then its canonical runtime name).
pub fn resolve_provider_api_key(
    provider_names: &[&str],
    configured_api_key: Option<&str>,
    configured_api_key_env: Option<&str>,
    oauth_token: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    configured_api_key
        .and_then(resolve_api_key_literal_or_env_ref)
        .or_else(|| {
            configured_api_key_env
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .and_then(|name| std::env::var(name).ok())
                .filter(|v| !v.trim().is_empty())
        })
        .or_else(|| {
            provider_names
                .iter()
                .find_map(|name| provider_api_key_from_env(name))
        })
        .or_else(|| provider_names.iter().find_map(|name| oauth_token(name)))
}

pub fn hermes_client_tag() -> String {
    format!("{NOUS_CLIENT_TAG_PREFIX}{}", env!("CARGO_PKG_VERSION"))
}

pub fn nous_portal_tags() -> Value {
    Value::Array(vec![
        Value::String(NOUS_PRODUCT_TAG.to_string()),
        Value::String(hermes_client_tag()),
    ])
}

pub fn local_control_key_for_profile(profile: Option<&str>, key: &str) -> bool {
    if matches!(
        key,
        "provider_profile"
            | "supports_reasoning"
            | "reasoning_config"
            | "provider_preferences"
            | "openrouter_min_coding_score"
            | "supports_vision"
            | "session_id"
            | "qwen_session_metadata"
            | "ollama_num_ctx"
    ) {
        return true;
    }

    match profile.and_then(canonical_provider_profile_id) {
        Some("kimi-coding" | "kimi-coding-cn" | "nous" | "openrouter") => {
            matches!(key, "reasoning" | "reasoning_effort")
        }
        _ => false,
    }
}

pub fn clean_extra_body_for_profile(
    profile: Option<&str>,
    extra_body: Option<&Value>,
) -> Option<Value> {
    let Some(Value::Object(map)) = extra_body else {
        return extra_body.cloned();
    };
    let mut cleaned = Map::new();
    for (key, value) in map {
        if !local_control_key_for_profile(profile, key) {
            cleaned.insert(key.clone(), value.clone());
        }
    }
    Some(Value::Object(cleaned))
}

pub fn clean_extra_body_for_native_gemini(extra_body: Option<&Value>) -> Option<Value> {
    let Some(Value::Object(map)) = extra_body else {
        return None;
    };
    let mut cleaned = Map::new();
    for key in ["thinking_config", "thinkingConfig"] {
        if let Some(value) = map.get(key) {
            cleaned.insert(key.to_string(), value.clone());
        }
    }
    if cleaned.is_empty() {
        None
    } else {
        Some(Value::Object(cleaned))
    }
}

pub fn normalize_messages_for_profile(profile: Option<&str>, messages: &mut Value) {
    let Some("qwen-oauth") = profile.and_then(canonical_provider_profile_id) else {
        return;
    };
    let Some(items) = messages.as_array_mut() else {
        return;
    };
    for item in items {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        let is_system = obj
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role.eq_ignore_ascii_case("system"));
        match obj.get_mut("content") {
            Some(Value::String(text)) => {
                let mut part = Map::new();
                part.insert("type".to_string(), Value::String("text".to_string()));
                part.insert("text".to_string(), Value::String(text.clone()));
                if is_system {
                    part.insert(
                        "cache_control".to_string(),
                        serde_json::json!({"type": "ephemeral"}),
                    );
                }
                *obj.get_mut("content").expect("content exists") =
                    Value::Array(vec![Value::Object(part)]);
            }
            Some(Value::Array(parts)) if is_system => {
                if let Some(last) = parts.last_mut().and_then(Value::as_object_mut) {
                    last.entry("cache_control".to_string())
                        .or_insert_with(|| serde_json::json!({"type": "ephemeral"}));
                }
            }
            _ => {}
        }
    }
}

pub fn apply_profile_to_body(
    profile: Option<&str>,
    body: &mut Value,
    effective_model: &str,
    base_url: &str,
    extra_body: Option<&Value>,
) {
    let Some(profile) = profile.and_then(canonical_provider_profile_id) else {
        return;
    };
    match profile {
        "kimi-coding" | "kimi-coding-cn" => apply_kimi_profile(body, extra_body),
        "minimax" => apply_minimax_profile(body, effective_model, base_url, extra_body),
        "openrouter" => apply_openrouter_profile(body, effective_model, extra_body),
        "nous" => apply_nous_profile(body, extra_body),
        "qwen-oauth" => apply_qwen_profile(body, extra_body),
        "custom" => apply_custom_profile(body, extra_body),
        "nvidia" => {}
        _ => {}
    }
}

pub fn extra_headers_for_profile(
    profile: Option<&str>,
    effective_model: &str,
    extra_body: Option<&Value>,
) -> Vec<(String, String)> {
    let Some("openrouter") = profile.and_then(canonical_provider_profile_id) else {
        return Vec::new();
    };
    let Some(session_id) = string_field(extra_body, "session_id") else {
        return Vec::new();
    };
    if is_grok_model(effective_model) {
        vec![("x-grok-conv-id".to_string(), session_id.to_string())]
    } else {
        Vec::new()
    }
}

fn apply_kimi_profile(body: &mut Value, extra_body: Option<&Value>) {
    remove_key(body, "temperature");
    remove_key(body, "reasoning");
    let reasoning = reasoning_config(extra_body);
    let enabled = reasoning
        .and_then(|cfg| cfg.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    body["thinking"] = serde_json::json!({
        "type": if enabled { "enabled" } else { "disabled" },
    });
    if enabled {
        let effort = reasoning
            .and_then(|cfg| cfg.get("effort"))
            .and_then(Value::as_str)
            .or_else(|| string_field(extra_body, "reasoning_effort"))
            .unwrap_or("medium");
        body["reasoning_effort"] = Value::String(effort.to_string());
    } else {
        remove_key(body, "reasoning_effort");
    }
}

fn apply_minimax_profile(
    body: &mut Value,
    effective_model: &str,
    base_url: &str,
    extra_body: Option<&Value>,
) {
    if !is_minimax_global_openai_base_url(base_url) || !is_minimax_m3(effective_model) {
        return;
    }

    remove_key(body, "reasoning");
    body["reasoning_split"] = Value::Bool(true);
    let reasoning = reasoning_config(extra_body);
    let enabled = reasoning
        .and_then(|cfg| cfg.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    body["thinking"] = serde_json::json!({
        "type": if enabled { "adaptive" } else { "disabled" },
    });
}

fn apply_openrouter_profile(body: &mut Value, effective_model: &str, extra_body: Option<&Value>) {
    if let Some(preferences) = extra_body.and_then(|v| v.get("provider_preferences")) {
        body["provider"] = preferences.clone();
    }
    if let Some(session_id) = string_field(extra_body, "session_id") {
        body["session_id"] = Value::String(session_id.to_string());
    }
    if is_pareto_code_model(effective_model) {
        if let Some(score) = extra_body
            .and_then(|v| v.get("openrouter_min_coding_score"))
            .and_then(parse_score)
        {
            body["plugins"] =
                serde_json::json!([{"id": "pareto-router", "min_coding_score": score}]);
        }
    }

    let supports_reasoning = bool_field(extra_body, "supports_reasoning").unwrap_or(false);
    if openrouter_anthropic_reasoning_is_mandatory(effective_model) {
        remove_key(body, "reasoning");
        let reasoning = reasoning_config(extra_body);
        let enabled = reasoning
            .and_then(|cfg| cfg.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let effort = reasoning
            .and_then(|cfg| cfg.get("effort"))
            .and_then(Value::as_str)
            .or_else(|| string_field(extra_body, "reasoning_effort"))
            .map(str::trim)
            .filter(|v| !v.is_empty() && *v != "none");
        if enabled {
            if let Some(effort) = effort {
                body["verbosity"] = Value::String(effort.to_string());
            }
        }
    } else if let Some(reasoning) = reasoning_config(extra_body) {
        body["reasoning"] = reasoning.clone();
    } else if let Some(effort) = string_field(extra_body, "reasoning_effort") {
        body["reasoning"] = serde_json::json!({"enabled": true, "effort": effort});
    } else if supports_reasoning {
        body["reasoning"] = serde_json::json!({"enabled": true, "effort": "medium"});
    }
}

fn apply_nous_profile(body: &mut Value, extra_body: Option<&Value>) {
    body["tags"] = nous_portal_tags();

    let supports_reasoning = bool_field(extra_body, "supports_reasoning").unwrap_or(false);
    let reasoning = reasoning_config(extra_body);
    let enabled = reasoning
        .and_then(|cfg| cfg.get("enabled"))
        .and_then(Value::as_bool);

    match (supports_reasoning, reasoning, enabled) {
        (_, Some(_), Some(false)) => remove_key(body, "reasoning"),
        (true, Some(cfg), _) => body["reasoning"] = cfg.clone(),
        (true, None, _) => {
            if let Some(effort) = string_field(extra_body, "reasoning_effort") {
                body["reasoning"] = serde_json::json!({"effort": effort});
            }
        }
        _ => remove_key(body, "reasoning"),
    }
}

fn apply_qwen_profile(body: &mut Value, extra_body: Option<&Value>) {
    body["vl_high_resolution_images"] = Value::Bool(true);
    if let Some(metadata) = extra_body.and_then(|v| v.get("qwen_session_metadata")) {
        body["metadata"] = metadata.clone();
    }
}

fn apply_custom_profile(body: &mut Value, extra_body: Option<&Value>) {
    let Some(num_ctx) = extra_body
        .and_then(|v| v.get("ollama_num_ctx"))
        .and_then(|v| v.as_u64())
    else {
        return;
    };
    let options = body
        .as_object_mut()
        .expect("request body should be an object")
        .entry("options".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !options.is_object() {
        *options = Value::Object(Map::new());
    }
    options["num_ctx"] = Value::Number(num_ctx.into());
}

fn reasoning_config(extra_body: Option<&Value>) -> Option<&Value> {
    extra_body
        .and_then(|v| v.get("reasoning_config"))
        .or_else(|| extra_body.and_then(|v| v.get("reasoning")))
}

fn string_field<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn bool_field(value: Option<&Value>, key: &str) -> Option<bool> {
    value.and_then(|v| v.get(key)).and_then(Value::as_bool)
}

fn parse_score(value: &Value) -> Option<f64> {
    let score = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|v| v.trim().parse::<f64>().ok()))?;
    if (0.0..=1.0).contains(&score) {
        Some(score)
    } else {
        None
    }
}

fn is_grok_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower.starts_with("x-ai/grok")
        || lower.starts_with("xai/grok")
        || lower.starts_with("grok")
        || lower.contains("/grok")
}

fn is_minimax_global_openai_base_url(base_url: &str) -> bool {
    let normalized = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    normalized == "https://api.minimax.io/v1"
}

fn is_minimax_m3(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "minimax-m3" | "minimax/minimax-m3"
    )
}

fn openrouter_anthropic_reasoning_is_mandatory(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    if !m.starts_with("anthropic/") && !m.starts_with("claude") && !m.contains("claude") {
        return false;
    }
    const OPTIONAL_SUBSTRINGS: &[&str] = &[
        "claude-3",
        "claude-opus-4-0",
        "claude-opus-4.0",
        "claude-opus-4-1",
        "claude-opus-4.1",
        "claude-sonnet-4-0",
        "claude-sonnet-4.0",
        "claude-opus-4-2025",
        "claude-sonnet-4-2025",
        "claude-opus-4-5",
        "claude-opus-4.5",
        "claude-sonnet-4-5",
        "claude-sonnet-4.5",
        "claude-haiku-4-5",
        "claude-haiku-4.5",
    ];
    !OPTIONAL_SUBSTRINGS.iter().any(|needle| m.contains(needle))
}

fn is_pareto_code_model(model: &str) -> bool {
    let lower = model.trim().to_ascii_lowercase();
    lower == "openrouter/pareto-code" || lower.ends_with("/pareto-code")
}

fn remove_key(body: &mut Value, key: &str) {
    if let Some(obj) = body.as_object_mut() {
        obj.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_match_upstream_provider_profiles() {
        assert_eq!(canonical_provider_profile_id("kimi"), Some("kimi-coding"));
        assert_eq!(
            canonical_provider_profile_id("moonshot"),
            Some("kimi-coding")
        );
        assert_eq!(
            canonical_provider_profile_id("kimi-coding-cn"),
            Some("kimi-coding-cn")
        );
        assert_eq!(canonical_provider_profile_id("or"), Some("openrouter"));
        assert_eq!(canonical_provider_profile_id("mini-max"), Some("minimax"));
        assert_eq!(canonical_provider_profile_id("nous-portal"), Some("nous"));
        assert_eq!(canonical_provider_profile_id("nous-api"), Some("nous"));
        assert_eq!(canonical_provider_profile_id("nousapi"), Some("nous"));
        assert_eq!(canonical_provider_profile_id("qwen"), Some("qwen-oauth"));
        assert_eq!(canonical_provider_profile_id("mimo"), Some("xiaomi"));
        assert_eq!(canonical_provider_profile_id("xiaomi-mimo"), Some("xiaomi"));
        assert_eq!(canonical_provider_profile_id("lm-studio"), Some("custom"));
        assert_eq!(canonical_provider_profile_id("llamafile"), Some("custom"));
        assert_eq!(canonical_provider_profile_id("vmlx"), Some("custom"));
        assert_eq!(canonical_provider_profile_id("omlx"), Some("custom"));
        assert_eq!(canonical_provider_profile_id("kobold-cpp"), Some("custom"));
        assert_eq!(canonical_provider_profile_id("oobabooga"), Some("custom"));
        assert_eq!(canonical_provider_profile_id("exllamav2"), Some("custom"));
        assert_eq!(
            canonical_provider_profile_id("qwen-portal"),
            Some("qwen-oauth")
        );
        assert_eq!(canonical_provider_profile_id("missing"), None);
    }

    #[test]
    fn profile_static_metadata_matches_runtime_contracts() {
        assert_eq!(default_max_tokens("nvidia"), Some(16_384));
        assert_eq!(default_max_tokens("kimi"), Some(32_000));
        assert_eq!(default_max_tokens("qwen-oauth"), Some(65_536));
        assert_eq!(default_max_tokens("ollama-local"), Some(65_536));
        assert!(omit_temperature("kimi"));
        assert_eq!(profile_auth_type("nous"), Some("oauth_device_code"));
        assert_eq!(profile_auth_type("qwen-oauth"), Some("oauth_external"));
        assert!(profile_base_url("nvidia").unwrap().contains("nvidia.com"));
        assert_eq!(profile_base_url("kimi-coding"), Some(KIMI_CODE_BASE_URL));
        assert!(profile_base_url("kimi-coding-cn")
            .unwrap()
            .contains("moonshot.cn"));
        assert!(profile_base_url("mimo").unwrap().contains("xiaomimimo.com"));
        assert!(supports_vision("xiaomi"));
        assert!(!supports_vision_tool_messages("xiaomi"));
        assert!(!supports_vision("kimi"));
        assert!(is_kimi_code_base_url(KIMI_CODE_BASE_URL));
        assert_eq!(
            gemini_openai_compatible_base_url(GEMINI_NATIVE_BASE_URL),
            GEMINI_OPENAI_BASE_URL
        );
        assert_eq!(
            gemini_openai_compatible_base_url(GEMINI_OPENAI_BASE_URL),
            GEMINI_OPENAI_BASE_URL
        );
        assert!(is_native_gemini_base_url(GEMINI_NATIVE_BASE_URL));
        assert!(!is_native_gemini_base_url(GEMINI_OPENAI_BASE_URL));
        assert_eq!(
            kimi_base_url_for_api_key("sk-kimi-test", KIMI_LEGACY_BASE_URL),
            KIMI_CODE_BASE_URL
        );
        assert_eq!(
            kimi_base_url_for_api_key("sk-legacy-test", KIMI_LEGACY_BASE_URL),
            KIMI_LEGACY_BASE_URL
        );
    }

    #[test]
    fn nous_portal_tags_include_product_and_versioned_client() {
        assert_eq!(
            hermes_client_tag(),
            format!("{NOUS_CLIENT_TAG_PREFIX}{}", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            nous_portal_tags(),
            serde_json::json!([NOUS_PRODUCT_TAG, hermes_client_tag()])
        );
        let mut mutated = nous_portal_tags();
        mutated
            .as_array_mut()
            .expect("portal tags array")
            .push(Value::String("caller=mutated".to_string()));
        assert_eq!(
            nous_portal_tags(),
            serde_json::json!([NOUS_PRODUCT_TAG, hermes_client_tag()])
        );
    }

    #[test]
    fn qwen_message_normalization_adds_parts_and_cache_control() {
        let mut messages = serde_json::json!([
            {"role": "system", "content": "Be helpful"},
            {"role": "user", "content": "hello"}
        ]);
        normalize_messages_for_profile(Some("qwen-oauth"), &mut messages);
        assert!(messages[0]["content"].is_array());
        assert_eq!(messages[0]["content"][0]["text"], "Be helpful");
        assert_eq!(
            messages[0]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
        assert_eq!(
            messages[1]["content"][0],
            serde_json::json!({"type": "text", "text": "hello"})
        );
    }

    #[test]
    fn openrouter_grok_session_affinity_header_is_profile_scoped() {
        let extra = serde_json::json!({"session_id": "sess-abc"});
        assert_eq!(
            extra_headers_for_profile(Some("openrouter"), "x-ai/grok-4", Some(&extra)),
            vec![("x-grok-conv-id".to_string(), "sess-abc".to_string())]
        );
        assert!(extra_headers_for_profile(
            Some("openrouter"),
            "anthropic/claude-sonnet-4.6",
            Some(&extra)
        )
        .is_empty());
    }
}
