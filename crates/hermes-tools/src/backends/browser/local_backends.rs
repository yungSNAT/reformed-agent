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

    /// Send one CDP command to the first page target and return its `result`.
    ///
    /// Each command opens its own WebSocket. None of the commands this
    /// backend sends needs an enabled domain or events, so no session state
    /// has to outlive a command.
    async fn cdp_command_ws(&self, method: &str, params: Value) -> Result<Value, ToolError> {
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

        let (mut socket, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to open CDP WebSocket {ws_url}: {e}"))
            })?;
        let result = cdp_exchange(&mut socket, method, params).await;
        if let Err(e) = socket.close(None).await {
            tracing::debug!("closing CDP WebSocket {ws_url} failed: {e}");
        }
        result
    }

    /// Run `Runtime.evaluate` and fail when the page script throws.
    async fn evaluate(&self, params: Value) -> Result<Value, ToolError> {
        let result = self.cdp_command("Runtime.evaluate", params).await?;
        if let Some(details) = result.get("exceptionDetails") {
            let message = details
                .pointer("/exception/description")
                .or_else(|| details.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("unknown exception");
            return Err(ToolError::ExecutionFailed(format!(
                "Page script threw: {message}"
            )));
        }
        Ok(result)
    }

    async fn cdp_command_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout_duration: Duration,
    ) -> Result<Value, ToolError> {
        match tokio::time::timeout(timeout_duration, self.cdp_command_ws(method, params)).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(format_cdp_command_error(method, &self.endpoint, err)),
            Err(_) => Err(ToolError::ExecutionFailed(format_cdp_timeout_error(
                method,
                &self.endpoint,
                timeout_duration,
            ))),
        }
    }

    async fn cdp_command(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        self.cdp_command_with_timeout(method, params, cdp_command_timeout())
            .await
    }
}

/// Fail when a selector script reported that no element matched.
fn ensure_element_found(evaluation: &Value, selector: &str) -> Result<(), ToolError> {
    if evaluation.pointer("/result/value").and_then(Value::as_str) == Some("not found") {
        return Err(ToolError::NotFound(format!(
            "No element matches selector {selector}"
        )));
    }
    Ok(())
}

/// Id of the single command sent on each CDP connection.
const CDP_COMMAND_ID: u64 = 1;

/// Send one command on an open CDP socket and wait for its response,
/// skipping events and replies to other ids.
async fn cdp_exchange(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    method: &str,
    params: Value,
) -> Result<Value, ToolError> {
    let command = json!({"id": CDP_COMMAND_ID, "method": method, "params": params});
    socket
        .send(Message::text(command.to_string()))
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to send CDP command: {e}")))?;

    while let Some(frame) = socket.next().await {
        let frame = frame
            .map_err(|e| ToolError::ExecutionFailed(format!("CDP WebSocket read failed: {e}")))?;
        let Message::Text(text) = frame else {
            continue;
        };
        let reply: Value = serde_json::from_str(text.as_str())
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to parse CDP message: {e}")))?;
        if reply.get("id").and_then(Value::as_u64) != Some(CDP_COMMAND_ID) {
            continue;
        }
        if let Some(error) = reply.get("error") {
            let message = error["message"].as_str().unwrap_or("unknown error");
            let mut detail = format!("CDP error {}: {message}", error["code"]);
            if let Some(data) = error["data"].as_str() {
                detail.push_str(&format!(" ({data})"));
            }
            return Err(ToolError::ExecutionFailed(detail));
        }
        return reply.get("result").cloned().ok_or_else(|| {
            ToolError::ExecutionFailed("CDP response has neither result nor error".into())
        });
    }
    Err(ToolError::ExecutionFailed(
        "CDP WebSocket closed before the command's response arrived".into(),
    ))
}

#[async_trait]
impl BrowserBackend for CdpBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        let first_open = self.first_navigation.swap(false, Ordering::SeqCst);
        let result = self
            .cdp_command_with_timeout(
                "Page.navigate",
                json!({"url": url}),
                cdp_open_timeout(first_open),
            )
            .await
            .map_err(|err| {
                ToolError::ExecutionFailed(format!(
                    "Failed to open {url}: {}",
                    tool_error_message(err)
                ))
            })?;
        // Chrome answers a failed navigation with `errorText` rather than a
        // protocol error.
        if let Some(error_text) = result.get("errorText").and_then(Value::as_str) {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed to open {url}: {error_text}"
            )));
        }
        Ok(json!({"status": "navigated", "url": url, "cdp": result}).to_string())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        let result = self
            .cdp_command("Accessibility.getFullAXTree", json!({}))
            .await?;
        Ok(redact_browser_observation(
            &json!({"status": "snapshot", "cdp": result}).to_string(),
        ))
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        // Use Runtime.evaluate to find and click the element. The block keeps
        // `el` out of the page's global scope, where a second `let el` throws.
        let js = format!(
            "{{ let el = document.querySelector('{}'); if(el) {{ el.click(); 'clicked' }} else {{ 'not found' }} }}",
            selector.replace('\'', "\\'")
        );
        let result = self.evaluate(json!({"expression": js})).await?;
        ensure_element_found(&result, selector)?;
        Ok(json!({"status": "clicked", "selector": selector, "cdp": result}).to_string())
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        let js = format!(
            "{{ let el = document.querySelector('{}'); if(el) {{ el.value = '{}'; el.dispatchEvent(new Event('input')); 'typed' }} else {{ 'not found' }} }}",
            selector.replace('\'', "\\'"),
            text.replace('\'', "\\'")
        );
        let result = self.evaluate(json!({"expression": js})).await?;
        ensure_element_found(&result, selector)?;
        Ok(
            json!({"status": "typed", "selector": selector, "text": text, "cdp": result})
                .to_string(),
        )
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        let px = amount.unwrap_or(500) as i32;
        let (x, y) = match direction {
            "up" => (0, -px),
            "down" => (0, px),
            "left" => (-px, 0),
            "right" => (px, 0),
            _ => (0, px),
        };
        let js = format!("window.scrollBy({}, {}); 'scrolled'", x, y);
        let result = self.evaluate(json!({"expression": js})).await?;
        Ok(
            json!({"status": "scrolled", "direction": direction, "amount": px, "cdp": result})
                .to_string(),
        )
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        let result = self
            .evaluate(json!({"expression": "history.back(); 'back'"}))
            .await?;
        Ok(json!({"status": "navigated_back", "cdp": result}).to_string())
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        let result = self
            .cdp_command(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyDown",
                    "key": key,
                }),
            )
            .await?;
        Ok(json!({"status": "key_pressed", "key": key, "cdp": result}).to_string())
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        let sel = selector.unwrap_or("img");
        let js = format!(
            "JSON.stringify(Array.from(document.querySelectorAll('{}')).map(img => ({{src: img.src, alt: img.alt, width: img.width, height: img.height}})))",
            sel.replace('\'', "\\'")
        );
        let result = self
            .evaluate(json!({"expression": js, "returnByValue": true}))
            .await?;
        Ok(json!({"status": "images_found", "selector": sel, "cdp": result}).to_string())
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        // Take a screenshot and analyze with vision model
        let result = self
            .cdp_command("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        Ok(browser_vision_payload(instruction, result))
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match action {
            "read" => {
                let result = self.evaluate(json!({
                    "expression": "'Console messages require Runtime.consoleAPICalled event listener'"
                })).await?;
                Ok(json!({"status": "console_read", "cdp": result}).to_string())
            }
            "clear" => {
                let result = self
                    .evaluate(json!({"expression": "console.clear(); 'cleared'"}))
                    .await?;
                Ok(json!({"status": "console_cleared", "cdp": result}).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown console action: {}",
                other
            ))),
        }
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
    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        self.inner.vision(instruction).await
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        self.inner.console(action).await
    }
}
