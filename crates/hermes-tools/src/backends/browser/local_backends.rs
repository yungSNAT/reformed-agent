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
        let mut backend = Self::new(endpoint, profile);
        backend.inner.local = !cdp_override_from_env();
        backend
    }
}

impl CdpBrowserBackend {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            client: reqwest::Client::new(),
            first_navigation: AtomicBool::new(true),
            local: false,
            retired: AtomicBool::new(false),
            isolated: false,
            tab: Mutex::new(None),
        }
    }

    /// Create from environment variable `CHROME_CDP_URL` or default localhost.
    pub fn from_env() -> Self {
        let endpoint = std::env::var("CHROME_CDP_URL")
            .or_else(|_| std::env::var("BROWSER_CDP_URL"))
            .unwrap_or_else(|_| "http://localhost:9222".to_string());
        let mut backend = Self::new(endpoint);
        backend.local = !cdp_override_from_env();
        backend
    }

    /// Send one CDP command to the browser's first page within `timeout`.
    /// Each call opens its own WebSocket and attaches to the page; a local
    /// Chrome keeps the page whether or not a client is connected, so no state
    /// has to outlive a call. The flag on an error is false only when the
    /// command was not sent; a timeout counts as sent.
    async fn page_call(
        &self,
        method: &str,
        params: Value,
        collect: &[&str],
        timeout: Duration,
    ) -> Result<(Value, Vec<Value>), (ToolError, bool)> {
        let attempt = async {
            let mut connection = self.open_page().await.map_err(|err| (err, false))?;
            let outcome = connection.call(method, params, collect).await;
            connection.close().await;
            outcome.map_err(|failure| (ToolError::ExecutionFailed(failure.message()), true))
        };
        match tokio::time::timeout(timeout, attempt).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err((err, sent))) => {
                Err((format_cdp_command_error(method, &self.endpoint, err), sent))
            }
            Err(_) => Err((
                ToolError::ExecutionFailed(format_cdp_timeout_error(
                    method,
                    &self.endpoint,
                    timeout,
                )),
                true,
            )),
        }
    }

    /// A connection attached to the browser's first page, or to this
    /// backend's own tab when it is isolated, unless the browser was retired.
    async fn open_page(&self) -> Result<CdpConnection, ToolError> {
        if self.retired.load(Ordering::SeqCst) {
            return Err(ToolError::ExecutionFailed(
                BROWSER_RETIRED_MESSAGE.into(),
            ));
        }
        let failed = |failure: CdpFailure| ToolError::ExecutionFailed(failure.message());
        if !self.isolated {
            return CdpConnection::open_attached(&self.client, &self.endpoint)
                .await
                .map_err(failed);
        }
        let mut tab = self.tab.lock().await;
        let ws_url = resolve_cdp_ws_url(&self.client, &self.endpoint)
            .await
            .map_err(failed)?;
        let mut connection = CdpConnection::open(&ws_url).await.map_err(failed)?;
        let target_id = match tab.as_ref() {
            Some(tab) => tab.target_id.clone(),
            None => {
                let created = connection.create_isolated_tab().await.map_err(failed)?;
                tab.insert(created).target_id.clone()
            }
        };
        if let Err(failure) = connection.attach_to_target(&target_id).await {
            // The tab is gone, as after a Chrome restart: the next call
            // starts a new context rather than reach any other page.
            if let Some(gone) = tab.take() {
                let _ = connection.dispose_context(&gone.context_id).await;
            }
            return Err(failed(failure));
        }
        Ok(connection)
    }
}

/// An isolated backend disposes of its context, and so its tab and cookies,
/// when dropped; see [`release_on_drop`].
impl Drop for CdpBrowserBackend {
    fn drop(&mut self) {
        if let Some(tab) = self.tab.get_mut().take() {
            let endpoint = &self.endpoint;
            release_on_drop("local browser context", |client| async move {
                dispose_browser_context(&client, endpoint, &tab.context_id).await
            });
        }
    }
}

/// Dispose of the browser context `context_id` at `endpoint` over a new
/// connection.
async fn dispose_browser_context(
    client: &reqwest::Client,
    endpoint: &str,
    context_id: &str,
) -> Result<bool, ToolError> {
    let failed = |failure: CdpFailure| ToolError::ExecutionFailed(failure.message());
    let ws_url = resolve_cdp_ws_url(client, endpoint)
        .await
        .map_err(failed)?;
    let mut connection = CdpConnection::open(&ws_url).await.map_err(failed)?;
    let disposed = connection.dispose_context(context_id).await;
    connection.close().await;
    disposed.map(|()| true).map_err(failed)
}

impl CdpChannel for CdpBrowserBackend {
    async fn cdp_call(
        &self,
        method: &str,
        params: Value,
        collect: &[&str],
        timeout: Duration,
    ) -> Result<(Value, Vec<Value>), ToolError> {
        self.page_call(method, params, collect, timeout)
            .await
            .map_err(|(err, _)| err)
    }

    async fn cdp_navigate_call(
        &self,
        url: &str,
        timeout: Duration,
    ) -> Result<Value, (ToolError, bool)> {
        self.page_call("Page.navigate", json!({"url": url}), &[], timeout)
            .await
            .map(|(result, _)| result)
    }

    async fn retire_page(&self) {
        self.retired.store(true, Ordering::SeqCst);
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
    async fn blank_page(&self) -> Result<String, ToolError> {
        cdp_blank_page(self).await
    }
    /// A browser on this host reaches networks that a container or remote
    /// terminal cannot, so only a local terminal makes it local.
    /// The terminal is read as the configuration loader resolves it:
    /// `TERMINAL_ENV`, else `TERMINAL_BACKEND`, which the loader does not
    /// copy into `TERMINAL_ENV`.
    fn is_local_backend(&self) -> bool {
        let terminal = ["TERMINAL_ENV", "TERMINAL_BACKEND"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok())
            .map(|value| value.trim().to_ascii_lowercase())
            .find(|value| !value.is_empty());
        self.local && terminal.is_none_or(|terminal| terminal == "local")
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
    fn is_retired(&self) -> bool {
        self.retired.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BrowserBackend for CamoFoxBrowserBackend {
    /// Upstream counts Camofox as local whatever the terminal backend; here
    /// it follows the CDP rule, so a container or remote terminal keeps the
    /// navigation checks on.
    fn is_local_backend(&self) -> bool {
        self.inner.is_local_backend()
    }
    async fn blank_page(&self) -> Result<String, ToolError> {
        self.inner.blank_page().await
    }
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
    fn is_retired(&self) -> bool {
        self.inner.is_retired()
    }
}
