impl CamoFoxBrowserBackend {
    pub fn new(endpoint: String, profile: String) -> Self {
        Self {
            inner: CdpBrowserBackend::new(endpoint),
            profile,
        }
    }

    pub fn from_env() -> Self {
        let endpoint = std::env::var("CAMOFOX_CDP_URL")
            .or_else(|_| std::env::var("CHROME_CDP_URL"))
            .or_else(|_| std::env::var("BROWSER_CDP_URL"))
            .unwrap_or_else(|_| "http://localhost:9222".to_string());
        let profile = std::env::var("CAMOFOX_PROFILE").unwrap_or_else(|_| "default".to_string());
        Self::new(endpoint, profile)
    }
}

impl CdpBrowserBackend {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            client: reqwest::Client::new(),
            first_navigation: AtomicBool::new(true),
        }
    }

    /// Create from environment variable `CHROME_CDP_URL` or default localhost.
    pub fn from_env() -> Self {
        let endpoint = std::env::var("CHROME_CDP_URL")
            .or_else(|_| std::env::var("BROWSER_CDP_URL"))
            .unwrap_or_else(|_| "http://localhost:9222".to_string());
        Self::new(endpoint)
    }

    /// Send one CDP command to the first page target. Each call opens its own
    /// WebSocket to the page; a local Chrome keeps the page whether or not a
    /// client is connected, so no state has to outlive a call.
    async fn page_call(
        &self,
        method: &str,
        params: Value,
        collect: &[&str],
    ) -> Result<(Value, Vec<Value>), ToolError> {
        let targets_resp = self
            .client
            .get(format!("{}/json", self.endpoint))
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Failed to connect to Chrome CDP at {}: {}",
                    self.endpoint, e
                ))
            })?;

        let targets: Vec<Value> = targets_resp.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse CDP targets: {}", e))
        })?;

        // Chrome also lists service workers, iframes and extension pages here.
        let ws_url = targets
            .iter()
            .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
            .find_map(|t| t.get("webSocketDebuggerUrl").and_then(Value::as_str))
            .ok_or_else(|| ToolError::ExecutionFailed("No Chrome page target found. Is Chrome running with --remote-debugging-port=9222?".into()))?;

        let mut connection = CdpConnection::open(ws_url)
            .await
            .map_err(|failure| ToolError::ExecutionFailed(failure.message()))?;
        let outcome = connection.call(method, params, collect).await;
        connection.close().await;
        outcome.map_err(|failure| ToolError::ExecutionFailed(failure.message()))
    }
}

impl CdpChannel for CdpBrowserBackend {
    async fn cdp_call(
        &self,
        method: &str,
        params: Value,
        collect: &[&str],
        timeout: Duration,
    ) -> Result<(Value, Vec<Value>), ToolError> {
        match tokio::time::timeout(timeout, self.page_call(method, params, collect)).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(err)) => Err(format_cdp_command_error(method, &self.endpoint, err)),
            Err(_) => Err(ToolError::ExecutionFailed(format_cdp_timeout_error(
                method,
                &self.endpoint,
                timeout,
            ))),
        }
    }

    /// The first navigation gets a longer allowance for Chrome's cold start.
    fn next_navigate_timeout(&self) -> Duration {
        cdp_open_timeout(self.first_navigation.swap(false, Ordering::SeqCst))
    }
}

#[async_trait]
impl BrowserBackend for CdpBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        cdp_navigate(self, url).await
    }
    async fn snapshot(&self) -> Result<String, ToolError> {
        cdp_snapshot(self).await
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        cdp_click(self, selector).await
    }
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        cdp_type(self, selector, text).await
    }
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        cdp_scroll(self, direction, amount).await
    }
    async fn go_back(&self) -> Result<String, ToolError> {
        cdp_go_back(self).await
    }
    async fn press(&self, key: &str) -> Result<String, ToolError> {
        cdp_press(self, key).await
    }
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        cdp_get_images(self, selector).await
    }
    async fn screenshot(&self) -> Result<String, ToolError> {
        cdp_screenshot(self).await
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        cdp_console(self, action).await
    }
}

#[async_trait]
impl BrowserBackend for CamoFoxBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        let alias = camofox_loopback_alias_from_env();
        let (browser_url, rewrite) = rewrite_loopback_url_for_camofox(
            url,
            camofox_loopback_rewrite_enabled_from_env(),
            &alias,
        );
        let result = self.inner.navigate(&browser_url).await?;
        let mut value =
            serde_json::from_str::<Value>(&result).unwrap_or_else(|_| json!({"result": result}));
        if let Some(obj) = value.as_object_mut() {
            obj.insert("camofox_profile".into(), self.profile.clone().into());
            if let Some(rewrite) = rewrite {
                obj.insert("requested_url".into(), rewrite.original_url.clone().into());
                obj.insert(
                    "url_rewrite".into(),
                    json!({
                        "from": rewrite.from,
                        "to": rewrite.to,
                        "original_url": rewrite.original_url,
                        "rewritten_url": rewrite.rewritten_url,
                    }),
                );
                obj.insert(
                    "warning".into(),
                    "Rewrote loopback URL for Docker-hosted Camofox".into(),
                );
            }
            Ok(value.to_string())
        } else {
            Ok(value.to_string())
        }
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        self.inner.snapshot().await
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        self.inner.click(selector).await
    }
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        self.inner.r#type(selector, text).await
    }
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        self.inner.scroll(direction, amount).await
    }
    async fn go_back(&self) -> Result<String, ToolError> {
        self.inner.go_back().await
    }
    async fn press(&self, key: &str) -> Result<String, ToolError> {
        self.inner.press(key).await
    }
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        self.inner.get_images(selector).await
    }
    async fn screenshot(&self) -> Result<String, ToolError> {
        self.inner.screenshot().await
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        self.inner.console(action).await
    }
}
