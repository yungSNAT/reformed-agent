//! Browser automation tools
//!
//! All browser tools delegate to an injectable `BrowserBackend` trait,
//! allowing different browser implementations (Playwright, Chromium, etc.)
//! to be plugged in by the caller.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::tools::url_safety::{check_website_blocklist, is_always_blocked_url, is_safe_url};
use crate::tools::vision::VisionBackend;

// ---------------------------------------------------------------------------
// BrowserBackend trait
// ---------------------------------------------------------------------------

/// Injected backend for browser automation operations.
#[async_trait]
pub trait BrowserBackend: Send + Sync {
    /// Open `url`. A JSON object result names the page the browser landed
    /// on, after any redirects, in its `url` field.
    async fn navigate(&self, url: &str) -> Result<String, ToolError>;
    /// Replace the current page with `about:blank` and drop it from the
    /// history, so going back cannot reopen it.
    async fn blank_page(&self) -> Result<String, ToolError>;
    /// Whether the browser runs on this machine beside a local terminal, which
    /// already reaches the local network, so `browser_navigate` skips the
    /// private-address check (upstream `_is_local_backend`). A backend that
    /// does not say keeps the check.
    fn is_local_backend(&self) -> bool {
        false
    }
    async fn snapshot(&self) -> Result<String, ToolError>;
    async fn click(&self, selector: &str) -> Result<String, ToolError>;
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError>;
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError>;
    async fn go_back(&self) -> Result<String, ToolError>;
    async fn press(&self, key: &str) -> Result<String, ToolError>;
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError>;
    /// Capture the current page as a PNG file and return JSON with its
    /// `screenshot_path`, never the image bytes.
    async fn screenshot(&self) -> Result<String, ToolError>;
    async fn console(&self, action: &str) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// BrowserNavigateHandler
// ---------------------------------------------------------------------------

pub struct BrowserNavigateHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserNavigateHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserNavigateHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'url' parameter".into()))?;
        let local = self.backend.is_local_backend();
        check_navigation(url, local, false).await?;
        let result = self.backend.navigate(url).await?;
        let landed = serde_json::from_str::<Value>(&result)
            .ok()
            .and_then(|value| value.get("url")?.as_str().map(str::to_string));
        if let Some(landed) = landed.filter(|landed| landed != url) {
            if let Err(err) = check_navigation(&landed, local, true).await {
                // Leave the refused page neither open nor in the history, so
                // neither a snapshot nor going back reaches it.
                if let Err(blank) = self.backend.blank_page().await {
                    tracing::warn!("Refused page left open; blanking failed: {blank}");
                }
                return Err(err);
            }
        }
        Ok(result)
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "url".into(),
            json!({
                "type": "string",
                "description": "The URL to navigate to"
            }),
        );
        tool_schema(
            "browser_navigate",
            "Navigate the browser to a URL.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

/// Refuses a page `browser_navigate` must not open, or stay on after a
/// redirect (`landed`), as upstream does: a cloud metadata endpoint on every
/// backend, a private or internal address unless the browser is local, and a
/// host on `security.website_blocklist`. Runs on the blocking pool, since the
/// checks resolve the host.
async fn check_navigation(url: &str, local: bool, landed: bool) -> Result<(), ToolError> {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        let (target, private) = if landed {
            ("redirect landed on", "private/internal address")
        } else {
            ("URL targets", "private or internal address")
        };
        if is_always_blocked_url(&url) {
            return Err(ToolError::ExecutionFailed(format!(
                "Blocked: {target} a cloud metadata endpoint"
            )));
        }
        if !local && !is_safe_url(&url) {
            return Err(ToolError::ExecutionFailed(format!(
                "Blocked: {target} a {private}"
            )));
        }
        check_website_blocklist(&url)
    })
    .await
    .unwrap_or_else(|e| {
        Err(ToolError::ExecutionFailed(format!(
            "Blocked: URL check did not complete: {e}"
        )))
    })
}

// ---------------------------------------------------------------------------
// BrowserSnapshotHandler
// ---------------------------------------------------------------------------

pub struct BrowserSnapshotHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserSnapshotHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserSnapshotHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        self.backend.snapshot().await
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "browser_snapshot",
            "Take a snapshot of the current page state (accessibility tree).",
            JsonSchema::new("object"),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserClickHandler
// ---------------------------------------------------------------------------

pub struct BrowserClickHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserClickHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserClickHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'selector' parameter".into()))?;
        self.backend.click(selector).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "CSS selector or accessibility label to click"
            }),
        );
        tool_schema(
            "browser_click",
            "Click an element on the page.",
            JsonSchema::object(props, vec!["selector".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserTypeHandler
// ---------------------------------------------------------------------------

pub struct BrowserTypeHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserTypeHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserTypeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'selector' parameter".into()))?;
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'text' parameter".into()))?;
        self.backend.r#type(selector, text).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "CSS selector or accessibility label of the input field"
            }),
        );
        props.insert(
            "text".into(),
            json!({
                "type": "string",
                "description": "Text to type into the element"
            }),
        );
        tool_schema(
            "browser_type",
            "Type text into an element on the page.",
            JsonSchema::object(props, vec!["selector".into(), "text".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserScrollHandler
// ---------------------------------------------------------------------------

pub struct BrowserScrollHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserScrollHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserScrollHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let direction = params
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("down");
        let amount = params
            .get("amount")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        self.backend.scroll(direction, amount).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "direction".into(),
            json!({
                "type": "string",
                "description": "Scroll direction: up, down, left, right",
                "enum": ["up", "down", "left", "right"],
                "default": "down"
            }),
        );
        props.insert(
            "amount".into(),
            json!({
                "type": "integer",
                "description": "Number of pixels to scroll (default: 500)"
            }),
        );
        tool_schema(
            "browser_scroll",
            "Scroll the page in a direction.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserBackHandler
// ---------------------------------------------------------------------------

pub struct BrowserBackHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserBackHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserBackHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        self.backend.go_back().await
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "browser_back",
            "Navigate back in browser history.",
            JsonSchema::new("object"),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserPressHandler
// ---------------------------------------------------------------------------

pub struct BrowserPressHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserPressHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserPressHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'key' parameter".into()))?;
        self.backend.press(key).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "key".into(),
            json!({
                "type": "string",
                "description": "Key to press (e.g. 'Enter', 'Tab', 'Escape', 'ArrowDown')"
            }),
        );
        tool_schema(
            "browser_press",
            "Press a keyboard key.",
            JsonSchema::object(props, vec!["key".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserGetImagesHandler
// ---------------------------------------------------------------------------

pub struct BrowserGetImagesHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserGetImagesHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserGetImagesHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let selector = params.get("selector").and_then(|v| v.as_str());
        self.backend.get_images(selector).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "selector".into(),
            json!({
                "type": "string",
                "description": "Optional CSS selector to filter images"
            }),
        );
        tool_schema(
            "browser_get_images",
            "Get images from the current page.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserVisionHandler
// ---------------------------------------------------------------------------

pub struct BrowserVisionHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
    /// The vision model, or why none is configured.
    vision: Result<std::sync::Arc<dyn VisionBackend>, String>,
}

impl BrowserVisionHandler {
    pub fn new(
        backend: std::sync::Arc<dyn BrowserBackend>,
        vision: Result<std::sync::Arc<dyn VisionBackend>, String>,
    ) -> Self {
        Self { backend, vision }
    }
}

#[async_trait]
impl ToolHandler for BrowserVisionHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let instruction = params
            .get("instruction")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'instruction' parameter".into()))?;
        let raw = self.backend.screenshot().await?;
        let mut result: Value = serde_json::from_str(&raw).map_err(|e| {
            ToolError::ExecutionFailed(format!("Invalid browser screenshot result: {e}"))
        })?;
        let path = result["screenshot_path"]
            .as_str()
            .ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "Browser screenshot result has no screenshot_path".into(),
                )
            })?
            .to_string();
        // Upstream's prompt for `browser_vision`.
        let prompt = format!(
            "You are analyzing a screenshot of a web browser.\n\n\
             User's question: {instruction}\n\n\
             Provide a detailed and helpful answer based on what you see in the screenshot. \
             If there are interactive elements, describe them. If there are verification challenges \
             or CAPTCHAs, describe what type they are and what action might be needed. \
             Focus on answering the user's specific question."
        );
        let analysis = match &self.vision {
            Ok(vision) => vision
                .analyze(&path, &prompt)
                .await
                .map_err(|e| e.to_string()),
            Err(not_configured) => Err(not_configured.clone()),
        };
        match analysis {
            Ok(analysis) => {
                // The vision model may have read secrets off the page.
                let analysis = hermes_intelligence::redact_sensitive_text(analysis.trim());
                result["success"] = json!(true);
                result["analysis"] = json!(if analysis.is_empty() {
                    "Vision analysis returned no content.".to_string()
                } else {
                    analysis
                });
            }
            Err(error) => {
                // A provider error can quote the request's key.
                let error = hermes_intelligence::redact_sensitive_text(error);
                tracing::warn!("browser_vision analysis failed: {error}");
                result["success"] = json!(false);
                result["error"] = json!(format!("Error during vision analysis: {error}"));
                result["note"] = json!(
                    "Screenshot was captured but vision analysis failed. You can still share it via MEDIA:<path>."
                );
            }
        }
        Ok(result.to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "instruction".into(),
            json!({
                "type": "string",
                "description": "What to look for or analyze in the current page screenshot"
            }),
        );
        tool_schema(
            "browser_vision",
            "Use vision to analyze the current browser page.",
            JsonSchema::object(props, vec!["instruction".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// BrowserConsoleHandler
// ---------------------------------------------------------------------------

pub struct BrowserConsoleHandler {
    backend: std::sync::Arc<dyn BrowserBackend>,
}

impl BrowserConsoleHandler {
    pub fn new(backend: std::sync::Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for BrowserConsoleHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = if params
            .get("clear")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            "clear"
        } else {
            params
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("read")
        };
        self.backend.console(action).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("action".into(), json!({
            "type": "string",
            "description": "Console action: 'read' to get console output, 'clear' to clear console",
            "enum": ["read", "clear"],
            "default": "read"
        }));
        props.insert(
            "clear".into(),
            json!({
                "type": "boolean",
                "description": "Clear console output after reading; compatibility alias for action='clear'",
                "default": false
            }),
        );
        tool_schema(
            "browser_console",
            "Read or clear the browser console.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBrowserBackend;
    #[async_trait]
    impl BrowserBackend for MockBrowserBackend {
        async fn navigate(&self, url: &str) -> Result<String, ToolError> {
            Ok(format!("Navigated to {}", url))
        }
        async fn blank_page(&self) -> Result<String, ToolError> {
            Ok("Blanked".into())
        }
        async fn snapshot(&self) -> Result<String, ToolError> {
            Ok("Page snapshot".into())
        }
        async fn click(&self, sel: &str) -> Result<String, ToolError> {
            Ok(format!("Clicked {}", sel))
        }
        async fn r#type(&self, sel: &str, text: &str) -> Result<String, ToolError> {
            Ok(format!("Typed '{}' into {}", text, sel))
        }
        async fn scroll(&self, dir: &str, _amt: Option<u32>) -> Result<String, ToolError> {
            Ok(format!("Scrolled {}", dir))
        }
        async fn go_back(&self) -> Result<String, ToolError> {
            Ok("Went back".into())
        }
        async fn press(&self, key: &str) -> Result<String, ToolError> {
            Ok(format!("Pressed {}", key))
        }
        async fn get_images(&self, sel: Option<&str>) -> Result<String, ToolError> {
            Ok(format!("Images: {:?}", sel))
        }
        async fn screenshot(&self) -> Result<String, ToolError> {
            Ok(json!({"screenshot_path": "/tmp/page.png"}).to_string())
        }
        async fn console(&self, action: &str) -> Result<String, ToolError> {
            Ok(format!("Console: {}", action))
        }
    }

    fn backend() -> std::sync::Arc<dyn BrowserBackend> {
        std::sync::Arc::new(MockBrowserBackend)
    }

    #[tokio::test]
    async fn test_browser_navigate() {
        let _env = CheckEnv::new("");
        let handler = BrowserNavigateHandler::new(backend());
        let result = handler
            .execute(json!({"url": "https://93.184.215.14/"}))
            .await
            .unwrap();
        assert!(result.contains("93.184.215.14"));
    }

    /// Holds the locks the URL checks read under, and points `HERMES_HOME` at
    /// a directory whose `config.yaml`, if any, is `config`, so no real
    /// config applies.
    struct CheckEnv {
        _home: tempfile::TempDir,
        original: Option<String>,
        _private_urls: std::sync::MutexGuard<'static, ()>,
        _global: std::sync::MutexGuard<'static, ()>,
    }

    impl CheckEnv {
        fn new(config: &str) -> Self {
            let global = hermes_config::managed_gateway::test_lock::lock();
            let private_urls = crate::tools::url_safety::ENV_TEST_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let home = tempfile::tempdir().unwrap();
            if !config.is_empty() {
                std::fs::write(home.path().join("config.yaml"), config).unwrap();
            }
            let original = std::env::var("HERMES_HOME").ok();
            std::env::set_var("HERMES_HOME", home.path());
            Self {
                _home: home,
                original,
                _private_urls: private_urls,
                _global: global,
            }
        }
    }

    impl Drop for CheckEnv {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }

    /// Lands on `landed` when set, else where it was sent, and records each
    /// URL it opens.
    struct NavBackend {
        local: bool,
        landed: Option<&'static str>,
        opened: std::sync::Mutex<Vec<String>>,
    }

    impl NavBackend {
        fn new(local: bool, landed: Option<&'static str>) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                local,
                landed,
                opened: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn opened(&self) -> Vec<String> {
            self.opened.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl BrowserBackend for NavBackend {
        async fn navigate(&self, url: &str) -> Result<String, ToolError> {
            self.opened.lock().unwrap().push(url.to_string());
            Ok(json!({"status": "navigated", "url": self.landed.unwrap_or(url)}).to_string())
        }
        async fn blank_page(&self) -> Result<String, ToolError> {
            self.opened.lock().unwrap().push("blank_page".into());
            Ok(json!({"status": "blank"}).to_string())
        }
        fn is_local_backend(&self) -> bool {
            self.local
        }
        async fn snapshot(&self) -> Result<String, ToolError> {
            Err(ToolError::ExecutionFailed("unused".into()))
        }
        async fn click(&self, _: &str) -> Result<String, ToolError> {
            Err(ToolError::ExecutionFailed("unused".into()))
        }
        async fn r#type(&self, _: &str, _: &str) -> Result<String, ToolError> {
            Err(ToolError::ExecutionFailed("unused".into()))
        }
        async fn scroll(&self, _: &str, _: Option<u32>) -> Result<String, ToolError> {
            Err(ToolError::ExecutionFailed("unused".into()))
        }
        async fn go_back(&self) -> Result<String, ToolError> {
            Err(ToolError::ExecutionFailed("unused".into()))
        }
        async fn press(&self, _: &str) -> Result<String, ToolError> {
            Err(ToolError::ExecutionFailed("unused".into()))
        }
        async fn get_images(&self, _: Option<&str>) -> Result<String, ToolError> {
            Err(ToolError::ExecutionFailed("unused".into()))
        }
        async fn screenshot(&self) -> Result<String, ToolError> {
            Err(ToolError::ExecutionFailed("unused".into()))
        }
        async fn console(&self, _: &str) -> Result<String, ToolError> {
            Err(ToolError::ExecutionFailed("unused".into()))
        }
    }

    async fn open(backend: &std::sync::Arc<NavBackend>, url: &str) -> Result<String, ToolError> {
        BrowserNavigateHandler::new(backend.clone())
            .execute(json!({"url": url}))
            .await
    }

    /// Cloud metadata endpoints, from upstream's `test_browser_ssrf_local.py`.
    const IMDS_URLS: [&str; 5] = [
        "http://169.254.169.254/latest/meta-data/",
        "http://169.254.169.253/metadata/instance",
        "http://169.254.170.2/v2/credentials",
        "http://100.100.100.200/latest/meta-data/",
        "http://metadata.google.internal/computeMetadata/v1/",
    ];

    fn assert_refused(outcome: Result<String, ToolError>, reason: &str) {
        let err = outcome.expect_err(reason).to_string();
        assert!(err.contains(reason), "{err}");
    }

    #[tokio::test]
    async fn cloud_browser_refuses_private_and_metadata_urls_before_opening_them() {
        let _env = CheckEnv::new("");
        let cloud = NavBackend::new(false, None);

        assert_refused(
            open(&cloud, "http://127.0.0.1:8080/dashboard").await,
            "URL targets a private or internal address",
        );
        for url in IMDS_URLS {
            assert_refused(
                open(&cloud, url).await,
                "URL targets a cloud metadata endpoint",
            );
        }
        assert!(cloud.opened().is_empty());

        open(&cloud, "https://93.184.215.14/")
            .await
            .expect("public URL");
    }

    #[tokio::test]
    async fn local_browser_opens_private_urls_but_never_metadata_endpoints() {
        let _env = CheckEnv::new("");
        let local = NavBackend::new(true, None);

        for url in [
            "http://127.0.0.1:8080/dashboard",
            "http://192.168.1.1/admin",
            "http://10.0.0.5/",
        ] {
            open(&local, url).await.expect(url);
        }
        for url in IMDS_URLS {
            assert_refused(
                open(&local, url).await,
                "URL targets a cloud metadata endpoint",
            );
        }
        assert_eq!(local.opened().len(), 3);
    }

    #[tokio::test]
    async fn redirect_to_a_refused_address_is_blanked_and_reported() {
        let _env = CheckEnv::new("");
        let start = "https://93.184.215.14/redirect";

        let cloud = NavBackend::new(false, Some("http://192.168.1.1/internal"));
        assert_refused(
            open(&cloud, start).await,
            "redirect landed on a private/internal address",
        );
        assert_eq!(cloud.opened(), [start, "blank_page"]);

        for local in [false, true] {
            let backend = NavBackend::new(local, Some("http://169.254.169.254/latest/meta-data/"));
            assert_refused(
                open(&backend, start).await,
                "redirect landed on a cloud metadata endpoint",
            );
            assert_eq!(backend.opened(), [start, "blank_page"]);
        }
    }

    #[tokio::test]
    async fn redirect_to_an_allowed_address_reports_where_it_landed() {
        let _env = CheckEnv::new("");
        let start = "https://93.184.215.14/redirect";
        for (local, landed) in [
            (true, "http://192.168.1.1/internal"),
            (false, "https://93.184.215.14/final"),
        ] {
            let backend = NavBackend::new(local, Some(landed));
            let result: Value =
                serde_json::from_str(&open(&backend, start).await.expect(landed)).unwrap();
            assert_eq!(result["url"], landed);
            assert_eq!(backend.opened(), [start]);
        }
    }

    #[tokio::test]
    async fn website_blocklist_applies_to_every_backend_before_and_after_opening() {
        let _env = CheckEnv::new(
            "security:\n  website_blocklist:\n    enabled: true\n    domains:\n      - \"93.184.215.14\"\n",
        );
        for local in [false, true] {
            let backend = NavBackend::new(local, None);
            assert_refused(
                open(&backend, "https://93.184.215.14/").await,
                "matches website blocklist rule",
            );
            assert!(backend.opened().is_empty());
        }

        let local = NavBackend::new(true, Some("https://93.184.215.14/"));
        assert_refused(
            open(&local, "http://10.0.0.5/").await,
            "matches website blocklist rule",
        );
        assert_eq!(local.opened(), ["http://10.0.0.5/", "blank_page"]);
    }

    #[tokio::test]
    async fn test_browser_click() {
        let handler = BrowserClickHandler::new(backend());
        let result = handler.execute(json!({"selector": "#btn"})).await.unwrap();
        assert!(result.contains("#btn"));
    }

    #[tokio::test]
    async fn test_browser_snapshot() {
        let handler = BrowserSnapshotHandler::new(backend());
        let result = handler.execute(json!({})).await.unwrap();
        assert!(result.contains("snapshot"));
    }

    #[tokio::test]
    async fn browser_interaction_handlers_forward_params_to_backend() {
        assert_eq!(
            BrowserTypeHandler::new(backend())
                .execute(json!({"selector": "#search", "text": "hello"}))
                .await
                .unwrap(),
            "Typed 'hello' into #search"
        );
        assert_eq!(
            BrowserScrollHandler::new(backend())
                .execute(json!({"direction": "up", "amount": 250}))
                .await
                .unwrap(),
            "Scrolled up"
        );
        assert_eq!(
            BrowserBackHandler::new(backend())
                .execute(json!({}))
                .await
                .unwrap(),
            "Went back"
        );
        assert_eq!(
            BrowserPressHandler::new(backend())
                .execute(json!({"key": "Enter"}))
                .await
                .unwrap(),
            "Pressed Enter"
        );
        assert_eq!(
            BrowserGetImagesHandler::new(backend())
                .execute(json!({"selector": "img.product"}))
                .await
                .unwrap(),
            "Images: Some(\"img.product\")"
        );
        let vision: Value = serde_json::from_str(
            &BrowserVisionHandler::new(backend(), Err("no vision model".into()))
                .execute(json!({"instruction": "summarize the page"}))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(vision["screenshot_path"], "/tmp/page.png");
        assert_eq!(
            vision["error"],
            "Error during vision analysis: no vision model"
        );
    }

    struct FailingVision;
    #[async_trait]
    impl VisionBackend for FailingVision {
        async fn analyze(&self, _image_url: &str, _question: &str) -> Result<String, ToolError> {
            Err(ToolError::ExecutionFailed(
                "401 Incorrect API key provided: sk-proj-abc123def456ghi789jkl012".into(),
            ))
        }
    }

    #[tokio::test]
    async fn browser_vision_redacts_the_provider_error() {
        let vision: Value = serde_json::from_str(
            &BrowserVisionHandler::new(backend(), Ok(std::sync::Arc::new(FailingVision)))
                .execute(json!({"instruction": "summarize the page"}))
                .await
                .unwrap(),
        )
        .unwrap();

        let error = vision["error"].as_str().expect("error");
        assert!(
            error.starts_with("Error during vision analysis: "),
            "{error}"
        );
        assert!(!error.contains("def456ghi789jkl012"), "{error}");
    }

    #[tokio::test]
    async fn browser_handlers_reject_missing_required_params() {
        assert!(BrowserTypeHandler::new(backend())
            .execute(json!({"selector": "#search"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("Missing 'text'"));
        assert!(BrowserPressHandler::new(backend())
            .execute(json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("Missing 'key'"));
        assert!(
            BrowserVisionHandler::new(backend(), Err("no vision model".into()))
                .execute(json!({}))
                .await
                .unwrap_err()
                .to_string()
                .contains("Missing 'instruction'")
        );
    }

    #[tokio::test]
    async fn browser_console_clear_bool_is_compat_alias_for_action_clear() {
        let handler = BrowserConsoleHandler::new(backend());
        let result = handler.execute(json!({"clear": true})).await.unwrap();
        assert_eq!(result, "Console: clear");

        let schema = handler.schema();
        let props = schema
            .parameters
            .properties
            .expect("browser_console properties");
        assert!(props.contains_key("clear"));
    }
}
