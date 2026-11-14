impl BrowserbaseBrowserBackend {
    pub fn new(config: BrowserbaseConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            session: Mutex::new(None),
            cdp: CloudCdpConnection::default(),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        Ok(Self::new(BrowserbaseConfig::from_env()?))
    }

    pub fn config(&self) -> &BrowserbaseConfig {
        &self.config
    }

    async fn ensure_session(&self) -> Result<BrowserbaseSession, ToolError> {
        let mut guard = self.session.lock().await;
        if let Some(session) = guard.as_ref() {
            return Ok(session.clone());
        }
        let session = self.create_session().await?;
        *guard = Some(session.clone());
        Ok(session)
    }

    async fn create_session(&self) -> Result<BrowserbaseSession, ToolError> {
        let mut omit_keep_alive = false;
        let mut omit_proxies = false;
        let mut keepalive_fallback = false;
        let mut proxies_fallback = false;

        let mut response = self.post_session(omit_keep_alive, omit_proxies).await?;
        if response.status() == reqwest::StatusCode::PAYMENT_REQUIRED && self.config.keep_alive {
            keepalive_fallback = true;
            omit_keep_alive = true;
            response = self.post_session(omit_keep_alive, omit_proxies).await?;
        }
        if response.status() == reqwest::StatusCode::PAYMENT_REQUIRED && self.config.proxies {
            proxies_fallback = true;
            omit_proxies = true;
            response = self.post_session(omit_keep_alive, omit_proxies).await?;
        }

        let status = response.status();
        let text = response.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Browserbase response: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed to create Browserbase session: {status} {text}"
            )));
        }
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Browserbase response: {e}"))
        })?;
        let bb_session_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("Browserbase response missing session id".into())
            })?
            .to_string();
        let cdp_url = data
            .get("connectUrl")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("Browserbase response missing connectUrl".into())
            })?
            .to_string();
        let suffix = Uuid::new_v4().simple().to_string();
        let session_name = format!("hermes_{}_{}", self.config.task_id, &suffix[..8]);
        tracing::info!(
            session_id = %bb_session_id,
            session_name = %session_name,
            keep_alive = self.config.keep_alive && !keepalive_fallback,
            proxies = self.config.proxies && !proxies_fallback,
            advanced_stealth = self.config.advanced_stealth,
            "created Browserbase session"
        );
        Ok(BrowserbaseSession {
            bb_session_id,
            cdp_url,
        })
    }

    async fn post_session(
        &self,
        omit_keep_alive: bool,
        omit_proxies: bool,
    ) -> Result<reqwest::Response, ToolError> {
        self.client
            .post(format!("{}/v1/sessions", self.config.base_url))
            .header("Content-Type", "application/json")
            .header("X-BB-API-Key", &self.config.api_key)
            .json(&browserbase_session_payload(
                &self.config,
                omit_keep_alive,
                omit_proxies,
            ))
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Browserbase API connection failed: {e}"))
            })
    }

    pub async fn close_active_session(&self) -> Result<bool, ToolError> {
        let mut guard = self.session.lock().await;
        self.cdp.disconnect().await;
        let Some(session) = guard.take() else {
            return Ok(false);
        };
        Self::request_close(&self.client, &self.config, &session.bb_session_id).await
    }

    async fn request_close(
        client: &reqwest::Client,
        config: &BrowserbaseConfig,
        session_id: &str,
    ) -> Result<bool, ToolError> {
        let resp = client
            .post(format!("{}/v1/sessions/{session_id}", config.base_url))
            .header("Content-Type", "application/json")
            .header("X-BB-API-Key", &config.api_key)
            .json(&json!({
                "projectId": config.project_id,
                "status": "REQUEST_RELEASE",
            }))
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Browserbase close failed: {e}")))?;
        Ok(release_accepted(resp.status()))
    }
}

impl CloudBrowserSession for BrowserbaseBrowserBackend {
    const PROVIDER: &'static str = "Browserbase";

    fn cdp(&self) -> &CloudCdpConnection {
        &self.cdp
    }

    async fn session_cdp_url(&self) -> Result<String, ToolError> {
        Ok(self.ensure_session().await?.cdp_url)
    }

    async fn release_session(&self) -> Result<bool, ToolError> {
        self.close_active_session().await
    }
}

impl BrowserUseBrowserBackend {
    pub fn new(config: BrowserUseConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            session: Mutex::new(None),
            pending_create_key: Mutex::new(None),
            cdp: CloudCdpConnection::default(),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        Ok(Self::new(BrowserUseConfig::from_env()?))
    }

    pub fn config(&self) -> &BrowserUseConfig {
        &self.config
    }

    async fn ensure_session(&self) -> Result<BrowserUseSession, ToolError> {
        let mut guard = self.session.lock().await;
        if let Some(session) = guard.as_ref() {
            return Ok(session.clone());
        }
        let session = self.create_session().await?;
        *guard = Some(session.clone());
        Ok(session)
    }

    async fn create_session(&self) -> Result<BrowserUseSession, ToolError> {
        let idempotency_key = if self.config.managed_mode {
            Some(self.get_or_create_pending_create_key().await)
        } else {
            None
        };

        let response = self.post_session(idempotency_key.as_deref()).await?;
        let status = response.status();
        let external_call_id = if self.config.managed_mode {
            response
                .headers()
                .get("x-external-call-id")
                .and_then(|value| value.to_str().ok())
                .map(|value| value.to_string())
        } else {
            None
        };
        let text = response.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Browser Use response: {e}"))
        })?;

        if !status.is_success() {
            if self.config.managed_mode
                && !browser_use_should_preserve_pending_create_key(status, &text)
            {
                self.clear_pending_create_key().await;
            }
            return Err(ToolError::ExecutionFailed(format!(
                "Failed to create Browser Use session: {status} {text}"
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Browser Use response: {e}"))
        })?;
        if self.config.managed_mode {
            self.clear_pending_create_key().await;
        }
        let bb_session_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("Browser Use response missing session id".into())
            })?
            .to_string();
        let cdp_url = data
            .get("cdpUrl")
            .or_else(|| data.get("connectUrl"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("Browser Use response missing cdpUrl".into())
            })?
            .to_string();
        let suffix = Uuid::new_v4().simple().to_string();
        let session_name = format!("hermes_{}_{}", self.config.task_id, &suffix[..8]);
        tracing::info!(
            session_id = %bb_session_id,
            session_name = %session_name,
            managed = self.config.managed_mode,
            external_call_id = external_call_id.as_deref(),
            "created Browser Use session"
        );
        Ok(BrowserUseSession {
            bb_session_id,
            cdp_url,
        })
    }

    async fn post_session(
        &self,
        idempotency_key: Option<&str>,
    ) -> Result<reqwest::Response, ToolError> {
        let mut request = self
            .client
            .post(format!("{}/browsers", self.config.base_url))
            .json(&browser_use_session_payload(self.config.managed_mode));
        for (name, value) in browser_use_headers(&self.config, idempotency_key) {
            request = request.header(name, value);
        }
        request.send().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Browser Use API connection failed: {e}"))
        })
    }

    async fn get_or_create_pending_create_key(&self) -> String {
        let mut guard = self.pending_create_key.lock().await;
        if let Some(existing) = guard.as_ref() {
            return existing.clone();
        }
        let created = format!("browser-use-session-create:{}", Uuid::new_v4().simple());
        *guard = Some(created.clone());
        created
    }

    async fn clear_pending_create_key(&self) {
        let mut guard = self.pending_create_key.lock().await;
        *guard = None;
    }

    pub async fn close_active_session(&self) -> Result<bool, ToolError> {
        let mut guard = self.session.lock().await;
        self.cdp.disconnect().await;
        let Some(session) = guard.take() else {
            return Ok(false);
        };
        Self::request_close(&self.client, &self.config, &session.bb_session_id).await
    }

    async fn request_close(
        client: &reqwest::Client,
        config: &BrowserUseConfig,
        session_id: &str,
    ) -> Result<bool, ToolError> {
        let mut request = client
            .patch(format!("{}/browsers/{session_id}", config.base_url))
            .json(&json!({"action": "stop"}));
        for (name, value) in browser_use_headers(config, None) {
            request = request.header(name, value);
        }
        let resp = request
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Browser Use close failed: {e}")))?;
        Ok(release_accepted(resp.status()))
    }
}

impl CloudBrowserSession for BrowserUseBrowserBackend {
    const PROVIDER: &'static str = "Browser Use";

    fn cdp(&self) -> &CloudCdpConnection {
        &self.cdp
    }

    async fn session_cdp_url(&self) -> Result<String, ToolError> {
        Ok(self.ensure_session().await?.cdp_url)
    }

    async fn release_session(&self) -> Result<bool, ToolError> {
        self.close_active_session().await
    }
}

impl FirecrawlBrowserBackend {
    pub fn new(config: FirecrawlBrowserConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            session: Mutex::new(None),
            cdp: CloudCdpConnection::default(),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        Ok(Self::new(FirecrawlBrowserConfig::from_env()?))
    }

    pub fn config(&self) -> &FirecrawlBrowserConfig {
        &self.config
    }

    async fn ensure_session(&self) -> Result<FirecrawlBrowserSession, ToolError> {
        let mut guard = self.session.lock().await;
        if let Some(session) = guard.as_ref() {
            return Ok(session.clone());
        }
        let session = self.create_session().await?;
        *guard = Some(session.clone());
        Ok(session)
    }

    async fn create_session(&self) -> Result<FirecrawlBrowserSession, ToolError> {
        let response = self.post_session().await?;
        let status = response.status();
        let text = response.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Firecrawl browser response: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed to create Firecrawl browser session: {status} {text}"
            )));
        }
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Firecrawl browser response: {e}"))
        })?;
        let bb_session_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("Firecrawl browser response missing id".into())
            })?
            .to_string();
        let cdp_url = data
            .get("cdpUrl")
            .or_else(|| data.get("connectUrl"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("Firecrawl browser response missing cdpUrl".into())
            })?
            .to_string();
        let suffix = Uuid::new_v4().simple().to_string();
        let session_name = format!("hermes_{}_{}", self.config.task_id, &suffix[..8]);
        tracing::info!(
            session_id = %bb_session_id,
            session_name = %session_name,
            ttl_secs = self.config.ttl_secs,
            "created Firecrawl browser session"
        );
        Ok(FirecrawlBrowserSession {
            bb_session_id,
            cdp_url,
        })
    }

    async fn post_session(&self) -> Result<reqwest::Response, ToolError> {
        self.client
            .post(format!("{}/v2/browser", self.config.base_url))
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .json(&firecrawl_browser_session_payload(&self.config))
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Firecrawl browser API connection failed: {e}"))
            })
    }

    pub async fn close_active_session(&self) -> Result<bool, ToolError> {
        let mut guard = self.session.lock().await;
        self.cdp.disconnect().await;
        let Some(session) = guard.take() else {
            return Ok(false);
        };
        Self::request_close(&self.client, &self.config, &session.bb_session_id).await
    }

    async fn request_close(
        client: &reqwest::Client,
        config: &FirecrawlBrowserConfig,
        session_id: &str,
    ) -> Result<bool, ToolError> {
        let resp = client
            .delete(format!("{}/v2/browser/{session_id}", config.base_url))
            .header("Authorization", format!("Bearer {}", config.api_key))
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Firecrawl browser close failed: {e}"))
            })?;
        Ok(release_accepted(resp.status()))
    }
}

impl CloudBrowserSession for FirecrawlBrowserBackend {
    const PROVIDER: &'static str = "Firecrawl";

    fn cdp(&self) -> &CloudCdpConnection {
        &self.cdp
    }

    async fn session_cdp_url(&self) -> Result<String, ToolError> {
        Ok(self.ensure_session().await?.cdp_url)
    }

    async fn release_session(&self) -> Result<bool, ToolError> {
        self.close_active_session().await
    }
}

/// Whether a vendor accepted a request to end a session.
fn release_accepted(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::OK | reqwest::StatusCode::CREATED | reqwest::StatusCode::NO_CONTENT
    )
}

/// Each cloud backend releases the session it holds when dropped; see
/// [`release_on_drop`].
macro_rules! release_session_on_drop {
    ($($backend:ty),+) => {$(
        impl Drop for $backend {
            fn drop(&mut self) {
                if let Some(session) = self.session.get_mut().take() {
                    let config = &self.config;
                    release_on_drop(Self::PROVIDER, |client| async move {
                        Self::request_close(&client, config, &session.bb_session_id).await
                    });
                }
            }
        }
    )+};
}

release_session_on_drop!(
    BrowserbaseBrowserBackend,
    BrowserUseBrowserBackend,
    FirecrawlBrowserBackend
);

fn browser_use_headers(
    config: &BrowserUseConfig,
    idempotency_key: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        ("Content-Type", "application/json".to_string()),
        ("X-Browser-Use-API-Key", config.api_key.clone()),
    ];
    if let Some(key) = idempotency_key {
        headers.push(("X-Idempotency-Key", key.to_string()));
    }
    headers
}

fn browser_use_session_payload(managed_mode: bool) -> Value {
    if managed_mode {
        json!({
            "timeout": BROWSER_USE_MANAGED_TIMEOUT_MINUTES,
            "proxyCountryCode": BROWSER_USE_MANAGED_PROXY_COUNTRY_CODE,
        })
    } else {
        json!({})
    }
}

fn firecrawl_browser_session_payload(config: &FirecrawlBrowserConfig) -> Value {
    json!({"ttl": config.ttl_secs})
}

fn browser_use_should_preserve_pending_create_key(status: reqwest::StatusCode, body: &str) -> bool {
    if status.as_u16() >= 500 {
        return true;
    }
    if status != reqwest::StatusCode::CONFLICT {
        return false;
    }
    let Ok(payload) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    payload
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|message| message.as_str())
        .map(|message| message.to_ascii_lowercase().contains("already in progress"))
        .unwrap_or(false)
}

fn browserbase_session_payload(
    config: &BrowserbaseConfig,
    omit_keep_alive: bool,
    omit_proxies: bool,
) -> Value {
    let mut payload = json!({"projectId": &config.project_id});
    if config.keep_alive && !omit_keep_alive {
        payload["keepAlive"] = json!(true);
    }
    if let Some(timeout) = config.session_timeout_secs {
        payload["timeout"] = json!(timeout);
    }
    if config.proxies && !omit_proxies {
        payload["proxies"] = json!(true);
    }
    if config.advanced_stealth {
        payload["browserSettings"] = json!({"advancedStealth": true});
    }
    payload
}
