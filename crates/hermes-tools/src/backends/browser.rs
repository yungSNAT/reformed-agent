//! Real browser backend: CDP (Chrome DevTools Protocol) via WebSocket.
//!
//! This backend connects to a running Chrome/Chromium instance via CDP
//! and provides browser automation capabilities.

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use hermes_config::{
    cli_config_path, config_path, is_managed_tool_gateway_ready, managed_nous_tools_enabled,
    resolve_managed_tool_gateway, ManagedToolGatewayConfig, ResolveOptions,
};
use regex::Regex;
use serde_json::{json, Value};
use std::net::IpAddr;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use url::Url;
use uuid::Uuid;

use crate::tools::browser::BrowserBackend;
use hermes_core::ToolError;

const BROWSERBASE_BASE_URL_DEFAULT: &str = "https://api.browserbase.com";
const BROWSERBASE_MAX_SESSION_TIMEOUT_SECS: u64 = 21_600;
const BROWSER_USE_BASE_URL_DEFAULT: &str = "https://api.browser-use.com/api/v3";
const BROWSER_USE_MANAGED_TIMEOUT_MINUTES: u64 = 5;
const BROWSER_USE_MANAGED_PROXY_COUNTRY_CODE: &str = "us";
const FIRECRAWL_BROWSER_BASE_URL_DEFAULT: &str = "https://api.firecrawl.dev";
const FIRECRAWL_BROWSER_DEFAULT_TTL_SECS: u64 = 300;
const DEFAULT_CDP_COMMAND_TIMEOUT_SECS: u64 = 30;
const MIN_CDP_OPEN_TIMEOUT_SECS: u64 = 60;
const MIN_FIRST_CDP_OPEN_TIMEOUT_SECS: u64 = 120;
/// Age past which a saved browser screenshot is deleted.
const SCREENSHOT_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
#[cfg(test)]
const CAMOFOX_STATE_DIR_NAME: &str = "browser_auth";
#[cfg(test)]
const CAMOFOX_STATE_SUBDIR: &str = "camofox";

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct CamofoxIdentity {
    user_id: String,
    session_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CamofoxLoopbackRewrite {
    from: String,
    to: String,
    original_url: String,
    rewritten_url: String,
}

/// Resolve the default browser backend from environment.
///
/// Selection order:
/// - `HERMES_BROWSER_BACKEND=browser-use` / `BROWSER_CLOUD_PROVIDER=browser-use`
/// - `HERMES_BROWSER_BACKEND=browserbase` / `BROWSER_CLOUD_PROVIDER=browserbase`
/// - `HERMES_BROWSER_BACKEND=firecrawl` / `BROWSER_CLOUD_PROVIDER=firecrawl`
/// - Browserbase credentials (`BROWSERBASE_API_KEY` + `BROWSERBASE_PROJECT_ID`)
/// - Browser Use direct or managed gateway configuration
/// - `HERMES_BROWSER_BACKEND=camofox`
/// - local CDP (`CHROME_CDP_URL` or localhost default)
pub fn browser_backend_from_env() -> Arc<dyn BrowserBackend> {
    match browser_backend_choice_from_env() {
        "browser-use" => match BrowserUseBrowserBackend::from_env() {
            Ok(backend) => Arc::new(CloudFallbackBrowserBackend::new(
                "BrowserUseProvider",
                backend,
            )),
            Err(err) if explicit_browser_use_requested_from_env_or_config() => {
                Arc::new(UnavailableBrowserBackend::new(err.to_string()))
            }
            Err(_) => Arc::new(CdpBrowserBackend::from_env()),
        },
        "browserbase" => match BrowserbaseBrowserBackend::from_env() {
            Ok(backend) => Arc::new(CloudFallbackBrowserBackend::new(
                "BrowserbaseProvider",
                backend,
            )),
            Err(err) if explicit_browserbase_requested_from_env_or_config() => {
                Arc::new(UnavailableBrowserBackend::new(err.to_string()))
            }
            Err(_) => Arc::new(CdpBrowserBackend::from_env()),
        },
        "firecrawl" => match FirecrawlBrowserBackend::from_env() {
            Ok(backend) => Arc::new(CloudFallbackBrowserBackend::new(
                "FirecrawlBrowserProvider",
                backend,
            )),
            Err(err) if explicit_firecrawl_requested_from_env_or_config() => {
                Arc::new(UnavailableBrowserBackend::new(err.to_string()))
            }
            Err(_) => Arc::new(CdpBrowserBackend::from_env()),
        },
        "camofox" => Arc::new(CamoFoxBrowserBackend::from_env()),
        _ => Arc::new(CdpBrowserBackend::from_env()),
    }
}

fn explicit_browser_use_requested_from_env_or_config() -> bool {
    for key in [
        "HERMES_BROWSER_BACKEND",
        "BROWSER_CLOUD_PROVIDER",
        "BROWSER_PROVIDER",
    ] {
        if let Some(value) = env_optional_nonempty(key) {
            if normalize_browser_provider(&value) == Some("browser-use") {
                return true;
            }
        }
    }
    matches!(configured_browser_cloud_provider(), Some("browser-use"))
}

fn explicit_browserbase_requested_from_env_or_config() -> bool {
    for key in [
        "HERMES_BROWSER_BACKEND",
        "BROWSER_CLOUD_PROVIDER",
        "BROWSER_PROVIDER",
    ] {
        if let Some(value) = env_optional_nonempty(key) {
            if normalize_browser_provider(&value) == Some("browserbase") {
                return true;
            }
        }
    }
    matches!(configured_browser_cloud_provider(), Some("browserbase"))
}

fn explicit_firecrawl_requested_from_env_or_config() -> bool {
    for key in [
        "HERMES_BROWSER_BACKEND",
        "BROWSER_CLOUD_PROVIDER",
        "BROWSER_PROVIDER",
    ] {
        if let Some(value) = env_optional_nonempty(key) {
            if normalize_browser_provider(&value) == Some("firecrawl") {
                return true;
            }
        }
    }
    matches!(configured_browser_cloud_provider(), Some("firecrawl"))
}

fn browser_backend_choice_from_env() -> &'static str {
    for key in [
        "HERMES_BROWSER_BACKEND",
        "BROWSER_CLOUD_PROVIDER",
        "BROWSER_PROVIDER",
    ] {
        if let Some(value) = env_optional_nonempty(key) {
            if let Some(provider) = normalize_browser_provider(&value) {
                return provider;
            }
        }
    }

    if cdp_override_from_env() {
        return "cdp";
    }

    if let Some(provider) = configured_browser_cloud_provider() {
        return provider;
    }

    if BrowserbaseConfig::is_configured_from_env() {
        "browserbase"
    } else if BrowserUseConfig::is_configured_from_env_or_managed() {
        "browser-use"
    } else if camofox_mode_enabled_from_env() {
        "camofox"
    } else {
        "cdp"
    }
}

fn normalize_browser_provider(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "browser-use" | "browser_use" | "browseruse" | "managed-browser" | "managed_browser" => {
            Some("browser-use")
        }
        "browserbase" | "browser-base" => Some("browserbase"),
        "firecrawl" | "fire-crawl" | "firecrawl-browser" | "firecrawl_browser" => Some("firecrawl"),
        "camofox" | "camo" => Some("camofox"),
        "cdp" | "chrome" | "chromium" | "local" => Some("cdp"),
        _ => None,
    }
}

fn env_optional_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_bool(name: &str, default: bool) -> bool {
    env_optional_nonempty(name)
        .map(|v| {
            !matches!(
                v.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(default)
}

fn env_duration_seconds(name: &str) -> Option<Duration> {
    let seconds = env_optional_nonempty(name)?.parse::<f64>().ok()?;
    if seconds.is_finite() && seconds > 0.0 {
        Some(Duration::from_secs_f64(seconds))
    } else {
        None
    }
}

fn cdp_command_timeout() -> Duration {
    env_duration_seconds("HERMES_BROWSER_COMMAND_TIMEOUT_SECONDS")
        .or_else(|| env_duration_seconds("BROWSER_COMMAND_TIMEOUT"))
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_CDP_COMMAND_TIMEOUT_SECS))
}

fn cdp_open_timeout(first_open: bool) -> Duration {
    let floor_secs = if first_open {
        MIN_FIRST_CDP_OPEN_TIMEOUT_SECS
    } else {
        MIN_CDP_OPEN_TIMEOUT_SECS
    };
    cdp_command_timeout().max(Duration::from_secs(floor_secs))
}

fn duration_label(duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if (seconds.fract()).abs() < f64::EPSILON {
        format!("{} seconds", seconds as u64)
    } else {
        format!("{seconds:.3} seconds")
    }
}

fn browser_cdp_hint(method: &str, detail: &str) -> Vec<&'static str> {
    let combined = detail.to_ascii_lowercase();
    let mut hints = vec![
        "Start Chrome/Chromium with --remote-debugging-port=9222 or set CHROME_CDP_URL/BROWSER_CDP_URL.",
    ];
    if method == "Page.navigate" {
        hints.push("If this was the first browser open, Chrome may still be cold-starting; retry after the daemon is ready.");
    }
    if combined.contains("sandbox") || combined.contains("no usable sandbox") {
        hints.push(
            "For Docker/root/AppArmor launches, start Chromium with --no-sandbox,--disable-dev-shm-usage.",
        );
    }
    hints
}

/// Scheme, host and port of a CDP endpoint. Messages that reach the model and
/// the logs print only these: the endpoint's user info, path or query can carry
/// a key.
fn cdp_endpoint_origin(endpoint: &str) -> String {
    match Url::parse(endpoint.trim()).map(|url| url.origin()) {
        Ok(origin) if origin.is_tuple() => origin.ascii_serialization(),
        _ => "the configured CDP endpoint".to_string(),
    }
}

fn format_cdp_timeout_error(method: &str, endpoint: &str, timeout_duration: Duration) -> String {
    let mut parts = vec![format!(
        "Browser CDP command '{method}' timed out after {} while contacting {}.",
        duration_label(timeout_duration),
        cdp_endpoint_origin(endpoint)
    )];
    parts.extend(browser_cdp_hint(method, "").into_iter().map(str::to_string));
    parts.join("\n")
}

fn tool_error_message(err: ToolError) -> String {
    match err {
        ToolError::ExecutionFailed(message)
        | ToolError::InvalidParams(message)
        | ToolError::NotFound(message)
        | ToolError::Timeout(message)
        | ToolError::SchemaViolation(message) => message,
    }
}

fn format_cdp_command_error(method: &str, endpoint: &str, err: ToolError) -> ToolError {
    let detail = tool_error_message(err);
    let mut parts = vec![format!(
        "Browser CDP command '{method}' failed while contacting {}: {detail}",
        cdp_endpoint_origin(endpoint)
    )];
    parts.extend(
        browser_cdp_hint(method, &detail)
            .into_iter()
            .map(str::to_string),
    );
    ToolError::ExecutionFailed(parts.join("\n"))
}

fn cdp_override_from_env() -> bool {
    env_optional_nonempty("CHROME_CDP_URL")
        .or_else(|| env_optional_nonempty("BROWSER_CDP_URL"))
        .is_some()
}

fn camofox_mode_enabled_from_env() -> bool {
    env_optional_nonempty("CAMOFOX_URL").is_some() && !cdp_override_from_env()
}

#[cfg(test)]
fn camofox_state_dir_for_home(home: &Path) -> std::path::PathBuf {
    home.join(CAMOFOX_STATE_DIR_NAME).join(CAMOFOX_STATE_SUBDIR)
}

#[cfg(test)]
fn camofox_identity_for_home(home: &Path, task_id: Option<&str>) -> CamofoxIdentity {
    use sha2::{Digest, Sha256};

    let state_dir = camofox_state_dir_for_home(home);
    let scope_root = state_dir.to_string_lossy();
    let logical_scope = task_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("default");
    let user_digest =
        hex::encode(Sha256::digest(format!("camofox-user:{scope_root}")))[..10].to_string();
    let session_digest = hex::encode(Sha256::digest(format!(
        "camofox-session:{scope_root}:{logical_scope}"
    )))[..16]
        .to_string();
    CamofoxIdentity {
        user_id: format!("hermes_{user_digest}"),
        session_key: format!("task_{session_digest}"),
    }
}

fn camofox_loopback_rewrite_enabled_from_env() -> bool {
    env_bool("CAMOFOX_REWRITE_LOOPBACK_URLS", false)
}

fn camofox_loopback_alias_from_env() -> String {
    env_optional_nonempty("CAMOFOX_LOOPBACK_HOST_ALIAS")
        .unwrap_or_else(|| "host.docker.internal".to_string())
}

fn is_loopback_hostname(host: &str) -> bool {
    let normalized = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    matches!(normalized.as_str(), "localhost" | "localhost.localdomain")
        || normalized
            .parse::<IpAddr>()
            .map(|addr| addr.is_loopback())
            .unwrap_or(false)
}

fn rewrite_loopback_url_for_camofox(
    input: &str,
    enabled: bool,
    alias: &str,
) -> (String, Option<CamofoxLoopbackRewrite>) {
    if !enabled || alias.trim().is_empty() {
        return (input.to_string(), None);
    }

    let Ok(mut parsed) = Url::parse(input) else {
        return (input.to_string(), None);
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return (input.to_string(), None);
    }
    let Some(host) = parsed
        .host_str()
        .map(|host| host.trim_matches(['[', ']']).to_string())
    else {
        return (input.to_string(), None);
    };
    if !is_loopback_hostname(&host) {
        return (input.to_string(), None);
    }
    if parsed.set_host(Some(alias.trim())).is_err() {
        return (input.to_string(), None);
    }
    let rewritten = parsed.to_string();
    (
        rewritten.clone(),
        Some(CamofoxLoopbackRewrite {
            from: host,
            to: alias.trim().to_string(),
            original_url: input.to_string(),
            rewritten_url: rewritten,
        }),
    )
}

fn secret_url_param(key: &str) -> bool {
    let key = key
        .trim()
        .trim_matches(|c: char| c == '-' || c == '_' || c.is_ascii_whitespace())
        .to_ascii_lowercase();
    key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("authorization")
        || key.contains("credential")
        || (key.contains("api") && key.contains("key"))
        || key == "key"
}

fn validate_url_does_not_exfiltrate_secret(input: &str) -> Result<(), ToolError> {
    let Ok(parsed) = Url::parse(input) else {
        return Ok(());
    };
    for (key, value) in parsed.query_pairs() {
        if secret_url_param(&key) && !value.trim().is_empty() {
            return Err(ToolError::InvalidParams(format!(
                "Blocked URL: query parameter '{key}' looks like an API key or token; pass secrets via local env/vault, not browser or web URLs."
            )));
        }
    }
    Ok(())
}

fn secret_assignment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(api[_-]?key|token|secret|password|authorization|credential)\b\s*[:=]\s*["']?([A-Za-z0-9_\-./]{8,})"#,
        )
        .expect("secret assignment regex")
    })
}

fn bearer_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)\bBearer\s+[A-Za-z0-9_\-./]{12,}"#).expect("bearer token regex")
    })
}

fn provider_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\b(?:sk|ghp|github_pat|or)-[A-Za-z0-9_\-]{12,}"#)
            .expect("provider token regex")
    })
}

fn redact_browser_observation(input: &str) -> String {
    let redacted = secret_assignment_re().replace_all(input, "$1=[REDACTED]");
    let redacted = bearer_token_re().replace_all(&redacted, "Bearer [REDACTED]");
    provider_token_re()
        .replace_all(&redacted, "[REDACTED_SECRET]")
        .to_string()
}

fn add_fallback_metadata(mut value: Value, provider: &str, reason: &str) -> Value {
    if let Some(obj) = value.as_object_mut() {
        obj.insert("fallback_from_cloud".into(), Value::Bool(true));
        obj.insert("fallback_provider".into(), provider.into());
        obj.insert("fallback_reason".into(), reason.into());
        return value;
    }
    json!({
        "fallback_from_cloud": true,
        "fallback_provider": provider,
        "fallback_reason": reason,
        "local_result": value,
    })
}

fn browser_fallback_response(local_result: String, provider: &str, reason: &str) -> String {
    let value = serde_json::from_str::<Value>(&local_result).unwrap_or(Value::String(local_result));
    add_fallback_metadata(value, provider, reason).to_string()
}

fn normalize_base_url(raw: &str) -> String {
    normalize_base_url_with_default(raw, BROWSERBASE_BASE_URL_DEFAULT)
}

fn normalize_base_url_with_default(raw: &str, default: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn configured_browser_cloud_provider() -> Option<&'static str> {
    for path in [cli_config_path(), config_path()] {
        if let Some(provider) = browser_provider_from_yaml_path(&path) {
            return Some(provider);
        }
    }
    None
}

fn browser_provider_from_yaml_path(path: &Path) -> Option<&'static str> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let provider = value
        .get("browser")
        .and_then(|browser| browser.get("cloud_provider"))
        .and_then(yaml_scalar_as_string)?;
    normalize_browser_provider(&provider)
}

fn browser_use_prefers_gateway_from_config() -> bool {
    for path in [cli_config_path(), config_path()] {
        if let Some(prefer) = browser_use_gateway_preference_from_yaml_path(&path) {
            return prefer;
        }
    }
    false
}

fn browser_use_gateway_preference_from_yaml_path(path: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;

    if let Some(raw) = value
        .get("browser")
        .and_then(|browser| browser.get("use_gateway"))
    {
        return Some(yaml_truthy(raw));
    }
    if let Some(raw) = value
        .get("tool_gateway")
        .and_then(|gateway| gateway.get("browser"))
    {
        return Some(yaml_gateway_route(raw));
    }
    None
}

fn yaml_scalar_as_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn yaml_truthy(value: &serde_yaml::Value) -> bool {
    match value {
        serde_yaml::Value::Bool(b) => *b,
        serde_yaml::Value::Number(n) => n.as_i64().map(|v| v != 0).unwrap_or(false),
        serde_yaml::Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "gateway" | "managed"
        ),
        _ => false,
    }
}

fn yaml_gateway_route(value: &serde_yaml::Value) -> bool {
    match value {
        serde_yaml::Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "gateway" | "managed" | "nous" | "true" | "1" | "yes" | "on"
        ),
        _ => yaml_truthy(value),
    }
}

/// Delete browser screenshots in `dir` older than a day, as upstream's
/// `_cleanup_old_screenshots` does, so the cache does not grow without bound.
/// A file that cannot be checked or removed is logged and left; pruning never
/// stops a capture. Upstream also skips the scan if it ran within the hour;
/// this scans on every capture, which costs one directory listing.
async fn prune_old_screenshots(dir: &Path) {
    let Some(cutoff) = std::time::SystemTime::now().checked_sub(SCREENSHOT_MAX_AGE) else {
        return;
    };
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!("listing {} for old screenshots failed: {e}", dir.display());
            return;
        }
    };
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(e) => {
                tracing::debug!("listing {} for old screenshots failed: {e}", dir.display());
                break;
            }
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("browser_screenshot_") && name.ends_with(".png")) {
            continue;
        }
        let path = entry.path();
        let outcome = match entry.metadata().await.and_then(|meta| meta.modified()) {
            Ok(modified) if modified < cutoff => tokio::fs::remove_file(&path).await,
            Ok(_) => continue,
            Err(e) => Err(e),
        };
        if let Err(e) = outcome {
            tracing::debug!("removing old screenshot {} failed: {e}", path.display());
        }
    }
}

/// Save a browser screenshot under `cache/screenshots` in the Hermes home,
/// with upstream's file name, and return its path.
async fn save_browser_screenshot(png: &[u8]) -> Result<std::path::PathBuf, ToolError> {
    let dir = hermes_config::hermes_home()
        .join("cache")
        .join("screenshots");
    tokio::fs::create_dir_all(&dir).await.map_err(|e| {
        ToolError::ExecutionFailed(format!("Failed to create {}: {e}", dir.display()))
    })?;
    prune_old_screenshots(&dir).await;
    let path = dir.join(format!(
        "browser_screenshot_{}.png",
        Uuid::new_v4().simple()
    ));
    tokio::fs::write(&path, png).await.map_err(|e| {
        ToolError::ExecutionFailed(format!("Failed to save {}: {e}", path.display()))
    })?;
    Ok(path)
}

/// Browser backend using Chrome DevTools Protocol.
/// Connects to Chrome via WebSocket for automation.
pub struct CdpBrowserBackend {
    /// DevTools endpoint: an HTTP discovery URL (`http://localhost:9222`), a
    /// bare `ws://host:port` or a browser WebSocket URL.
    endpoint: String,
    client: reqwest::Client,
    first_navigation: AtomicBool,
}

/// CamoFox anti-detection browser backend (compat layer).
///
/// Currently routes through CDP endpoint while exposing a dedicated type so
/// higher layers can opt into anti-detection profile selection.
pub struct CamoFoxBrowserBackend {
    inner: CdpBrowserBackend,
    profile: String,
}

struct CloudFallbackBrowserBackend<C> {
    provider: &'static str,
    cloud: C,
    local: CdpBrowserBackend,
    /// Why the cloud browser was unavailable, once an action has fallen back.
    fallback_reason: OnceLock<String>,
}

struct UnavailableBrowserBackend {
    message: String,
}

include!("browser/cloud_config.rs");

include!("browser/cloud_sessions.rs");
#[async_trait]
impl BrowserBackend for UnavailableBrowserBackend {
    async fn navigate(&self, _url: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn click(&self, _selector: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn r#type(&self, _selector: &str, _text: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn scroll(&self, _direction: &str, _amount: Option<u32>) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn press(&self, _key: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn get_images(&self, _selector: Option<&str>) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn screenshot(&self) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn console(&self, _action: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }
}

/// Falls back to the local browser only when the cloud provider is
/// unavailable: no session, or no CDP connection to it. A command the cloud
/// browser ran keeps its own result or error, since the local browser holds a
/// different page. This matches upstream, which falls back when creating the
/// cloud session fails and keeps the local session for the rest of the task.
#[async_trait]
impl<C: CloudBrowserSession + BrowserBackend> BrowserBackend for CloudFallbackBrowserBackend<C> {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        // Reject the URL before a cloud session is created for it.
        validate_url_does_not_exfiltrate_secret(url)?;
        match self.connect_cloud().await {
            Ok(()) => self.cloud.navigate(url).await,
            Err(reason) => self.fall_back(&reason, self.local.navigate(url)).await,
        }
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        match self.connect_cloud().await {
            Ok(()) => self.cloud.snapshot().await,
            Err(reason) => self.fall_back(&reason, self.local.snapshot()).await,
        }
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        match self.connect_cloud().await {
            Ok(()) => self.cloud.click(selector).await,
            Err(reason) => self.fall_back(&reason, self.local.click(selector)).await,
        }
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        match self.connect_cloud().await {
            Ok(()) => self.cloud.r#type(selector, text).await,
            Err(reason) => {
                self.fall_back(&reason, self.local.r#type(selector, text))
                    .await
            }
        }
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        match self.connect_cloud().await {
            Ok(()) => self.cloud.scroll(direction, amount).await,
            Err(reason) => {
                self.fall_back(&reason, self.local.scroll(direction, amount))
                    .await
            }
        }
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        match self.connect_cloud().await {
            Ok(()) => self.cloud.go_back().await,
            Err(reason) => self.fall_back(&reason, self.local.go_back()).await,
        }
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        match self.connect_cloud().await {
            Ok(()) => self.cloud.press(key).await,
            Err(reason) => self.fall_back(&reason, self.local.press(key)).await,
        }
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        match self.connect_cloud().await {
            Ok(()) => self.cloud.get_images(selector).await,
            Err(reason) => {
                self.fall_back(&reason, self.local.get_images(selector))
                    .await
            }
        }
    }

    async fn screenshot(&self) -> Result<String, ToolError> {
        match self.connect_cloud().await {
            Ok(()) => self.cloud.screenshot().await,
            Err(reason) => self.fall_back(&reason, self.local.screenshot()).await,
        }
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match self.connect_cloud().await {
            Ok(()) => self.cloud.console(action).await,
            Err(reason) => self.fall_back(&reason, self.local.console(action)).await,
        }
    }
}

include!("browser/cdp_client.rs");

include!("browser/cloud_backend_impls.rs");

include!("browser/local_backends.rs");
#[cfg(test)]
mod tests;
