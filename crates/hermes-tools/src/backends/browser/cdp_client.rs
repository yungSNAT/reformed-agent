/// Largest CDP message accepted, and so the largest frame. Chrome sends each
/// reply as a single WebSocket frame, and the base64 PNG from
/// `Page.captureScreenshot` on a large or high-density viewport can pass
/// tungstenite's 16 MiB frame default. Puppeteer allows 256 MiB for the same
/// reason.
const CDP_MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;

/// Events a console read collects while `Runtime.enable` replays what the
/// page has already logged.
const CDP_CONSOLE_EVENTS: &[&str] = &["Runtime.consoleAPICalled", "Runtime.exceptionThrown"];

/// Protocol error code for a command whose session id the browser does not
/// know.
const CDP_SESSION_NOT_FOUND: i64 = -32_001;

type CdpSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Why a CDP call failed.
enum CdpFailure {
    /// The browser answered with a protocol error; the connection is still usable.
    Rejected(String),
    /// The connection failed, closed or timed out and cannot be reused.
    Lost(String),
}

impl CdpFailure {
    fn message(self) -> String {
        match self {
            CdpFailure::Rejected(message) | CdpFailure::Lost(message) => message,
        }
    }
}

/// One open CDP WebSocket. Commands carry increasing ids. After
/// [`CdpConnection::attach_to_first_page`] they also carry the page's
/// flattened session id, as a browser-level endpoint requires.
struct CdpConnection {
    socket: CdpSocket,
    next_id: u64,
    session_id: Option<String>,
}

impl CdpConnection {
    async fn open(ws_url: &str) -> Result<Self, CdpFailure> {
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(CDP_MAX_MESSAGE_BYTES))
            .max_frame_size(Some(CDP_MAX_MESSAGE_BYTES));
        let (socket, _) = tokio_tungstenite::connect_async_with_config(ws_url, Some(config), false)
            .await
            .map_err(|e| CdpFailure::Lost(format!("Failed to open the CDP WebSocket: {e}")))?;
        Ok(Self {
            socket,
            next_id: 0,
            session_id: None,
        })
    }

    /// Open a browser-level endpoint and attach to its first page. A
    /// connection that cannot attach is of no use, so any attach failure
    /// counts as [`CdpFailure::Lost`].
    async fn open_attached(client: &reqwest::Client, cdp_url: &str) -> Result<Self, CdpFailure> {
        let ws_url = resolve_cdp_ws_url(client, cdp_url).await?;
        let mut connection = Self::open(&ws_url).await?;
        connection
            .attach_to_first_page()
            .await
            .map_err(|failure| CdpFailure::Lost(failure.message()))?;
        Ok(connection)
    }

    /// Attach to the browser's first page so that later commands reach it.
    async fn attach_to_first_page(&mut self) -> Result<(), CdpFailure> {
        let (targets, _) = self.call("Target.getTargets", json!({}), &[]).await?;
        let target_id = targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|target| target["type"] == "page")
            .and_then(|target| target["targetId"].as_str())
            .ok_or_else(|| CdpFailure::Lost("The browser has no page target".into()))?
            .to_string();
        let (attached, _) = self
            .call(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
                &[],
            )
            .await?;
        let session_id = attached["sessionId"].as_str().ok_or_else(|| {
            CdpFailure::Lost("Target.attachToTarget returned no sessionId".into())
        })?;
        self.session_id = Some(session_id.to_string());
        Ok(())
    }

    /// Send one command and wait for its response, skipping replies to other
    /// ids. Events named in `collect` that belong to this connection's page
    /// are returned in arrival order.
    async fn call(
        &mut self,
        method: &str,
        params: Value,
        collect: &[&str],
    ) -> Result<(Value, Vec<Value>), CdpFailure> {
        self.next_id += 1;
        let id = self.next_id;
        let mut command = json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = &self.session_id {
            command["sessionId"] = json!(session_id);
        }
        self.socket
            .send(Message::text(command.to_string()))
            .await
            .map_err(|e| CdpFailure::Lost(format!("Failed to send CDP command: {e}")))?;

        let mut events = Vec::new();
        while let Some(frame) = self.socket.next().await {
            let frame =
                frame.map_err(|e| CdpFailure::Lost(format!("CDP WebSocket read failed: {e}")))?;
            let Message::Text(text) = frame else {
                continue;
            };
            let message: Value = serde_json::from_str(text.as_str())
                .map_err(|e| CdpFailure::Lost(format!("Failed to parse CDP message: {e}")))?;
            if let Some(event) = message["method"].as_str() {
                if collect.contains(&event)
                    && message["sessionId"].as_str() == self.session_id.as_deref()
                {
                    events.push(message);
                }
                continue;
            }
            if message["id"].as_u64() != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let mut detail = format!(
                    "CDP error {}: {}",
                    error["code"],
                    error["message"].as_str().unwrap_or("unknown error")
                );
                if let Some(data) = error["data"].as_str() {
                    detail.push_str(&format!(" ({data})"));
                }
                // Chrome answers a command for a session it no longer has,
                // because the page closed or crashed, with this code
                // (`SessionNotFound` in Chromium's DevToolsSession).
                if error["code"] == CDP_SESSION_NOT_FOUND {
                    return Err(CdpFailure::Lost(detail));
                }
                return Err(CdpFailure::Rejected(detail));
            }
            let result = message.get("result").cloned().ok_or_else(|| {
                CdpFailure::Lost("CDP response has neither result nor error".into())
            })?;
            return Ok((result, events));
        }
        Err(CdpFailure::Lost(
            "CDP WebSocket closed before the command's response arrived".into(),
        ))
    }

    async fn close(mut self) {
        if let Err(e) = self.socket.close(None).await {
            tracing::debug!("closing CDP WebSocket failed: {e}");
        }
    }
}

/// A cloud session's CDP connection. It opens on first use and stays open
/// for the session's life: Browserbase ends a session without keepAlive as
/// soon as its connection closes, so a connection per command would end it.
#[derive(Default)]
struct CloudCdpConnection {
    connection: Mutex<Option<CdpConnection>>,
    /// Client for CDP discovery requests.
    client: reqwest::Client,
}

impl CloudCdpConnection {
    /// Open and attach the connection within `timeout` unless it is open.
    async fn connect(&self, cdp_url: &str, timeout: Duration) -> Result<(), CdpFailure> {
        let mut guard = self.connection.lock().await;
        if guard.is_none() {
            *guard =
                Some(within(timeout, CdpConnection::open_attached(&self.client, cdp_url)).await?);
        }
        Ok(())
    }

    /// Run one command on the browser at `cdp_url` within `timeout`. A
    /// [`CdpFailure::Lost`] result means the connection was dropped.
    async fn call(
        &self,
        cdp_url: &str,
        method: &str,
        params: Value,
        collect: &[&str],
        timeout: Duration,
    ) -> Result<(Value, Vec<Value>), CdpFailure> {
        let mut guard = self.connection.lock().await;
        let attempt = async {
            // Taken out so that a failure or timeout drops it.
            let mut connection = match guard.take() {
                Some(connection) => connection,
                None => CdpConnection::open_attached(&self.client, cdp_url).await?,
            };
            let outcome = connection.call(method, params, collect).await;
            if !matches!(outcome, Err(CdpFailure::Lost(_))) {
                *guard = Some(connection);
            }
            outcome
        };
        within(timeout, attempt).await
    }

    async fn disconnect(&self) {
        if let Some(connection) = self.connection.lock().await.take() {
            connection.close().await;
        }
    }
}

/// The browser WebSocket URL for a CDP endpoint, as upstream's
/// `_resolve_cdp_override` finds it. A `/devtools/browser/` URL, or any other
/// WebSocket URL with a path, is used as given. An HTTP discovery endpoint, or
/// a bare `ws(s)://host:port`, is resolved through `/json/version`. Upstream
/// keeps the raw URL when discovery fails; that URL cannot be opened here, so
/// the failure is returned instead.
async fn resolve_cdp_ws_url(client: &reqwest::Client, cdp_url: &str) -> Result<String, CdpFailure> {
    let raw = cdp_url.trim();
    // Messages include only the origin: the path, query and user info may
    // contain credentials, and these messages reach the model and the logs.
    let mut discovery =
        Url::parse(raw).map_err(|e| CdpFailure::Lost(format!("Invalid CDP URL: {e}")))?;
    match discovery.scheme() {
        "ws" | "wss" => {
            if discovery.port().is_none() || discovery.path() != "/" || discovery.query().is_some()
            {
                return Ok(raw.to_string());
            }
            let scheme = if discovery.scheme() == "ws" {
                "http"
            } else {
                "https"
            };
            discovery
                .set_scheme(scheme)
                .map_err(|()| CdpFailure::Lost("Invalid CDP URL".into()))?;
        }
        "http" | "https" => {}
        _ => return Err(CdpFailure::Lost("Unsupported CDP URL scheme".into())),
    }
    if !discovery.path().ends_with("/json/version") {
        let path = format!("{}/json/version", discovery.path().trim_end_matches('/'));
        discovery.set_path(&path);
    }
    let origin = discovery.origin().ascii_serialization();
    let failed = |detail: String| {
        CdpFailure::Lost(format!("CDP discovery (/json/version) at {origin} failed: {detail}"))
    };
    let response = client
        .get(discovery)
        .send()
        .await
        .map_err(|e| failed(e.without_url().to_string()))?;
    if !response.status().is_success() {
        return Err(failed(format!("HTTP {}", response.status())));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|e| failed(e.without_url().to_string()))?;
    match payload["webSocketDebuggerUrl"].as_str().map(str::trim) {
        Some(ws_url) if !ws_url.is_empty() => {
            tracing::info!("resolved CDP endpoint at {origin} to the browser WebSocket");
            Ok(ws_url.to_string())
        }
        _ => Err(failed("no webSocketDebuggerUrl in the response".into())),
    }
}

/// Run `attempt` within `timeout`; running out of time loses the connection.
async fn within<T>(
    timeout: Duration,
    attempt: impl std::future::Future<Output = Result<T, CdpFailure>>,
) -> Result<T, CdpFailure> {
    tokio::time::timeout(timeout, attempt)
        .await
        .unwrap_or_else(|_| {
            Err(CdpFailure::Lost(format!(
                "timed out after {}",
                duration_label(timeout)
            )))
        })
}

/// A cloud browser vendor whose sessions carry a CDP URL.
trait CloudBrowserSession: Send + Sync {
    /// Vendor name for logs and errors.
    const PROVIDER: &'static str;

    fn cdp(&self) -> &CloudCdpConnection;

    /// The current session's CDP URL, creating a session when there is none.
    fn session_cdp_url(
        &self,
    ) -> impl std::future::Future<Output = Result<String, ToolError>> + Send;

    /// End the current session with the vendor.
    fn release_session(&self) -> impl std::future::Future<Output = Result<bool, ToolError>> + Send;
}

impl<T: CloudBrowserSession> CdpChannel for T {
    async fn cdp_call(
        &self,
        method: &str,
        params: Value,
        collect: &[&str],
        timeout: Duration,
    ) -> Result<(Value, Vec<Value>), ToolError> {
        let cdp_url = self.session_cdp_url().await?;
        let outcome = self
            .cdp()
            .call(&cdp_url, method, params, collect, timeout)
            .await;
        if matches!(outcome, Err(CdpFailure::Lost(_))) {
            release_lost_session(self).await;
        }
        outcome.map_err(|failure| cloud_cdp_error(T::PROVIDER, method, failure))
    }

    /// The next command starts a new session, with its own page and history.
    async fn retire_page(&self) {
        release_lost_session(self).await;
    }
}

/// Release a session whose connection was lost: the vendor may already have
/// ended it, and the next command should start a new one.
async fn release_lost_session<T: CloudBrowserSession>(cloud: &T) {
    if let Err(err) = cloud.release_session().await {
        tracing::warn!("releasing {} session failed: {err}", T::PROVIDER);
    }
}

/// Make sure the cloud browser can take commands: a session exists and its
/// CDP connection is open and attached to a page. An error here means the
/// provider is unavailable, unlike an error from a command the page ran.
async fn cloud_connect<T: CloudBrowserSession>(cloud: &T) -> Result<(), ToolError> {
    let cdp_url = cloud.session_cdp_url().await?;
    if let Err(failure) = cloud.cdp().connect(&cdp_url, cdp_command_timeout()).await {
        release_lost_session(cloud).await;
        return Err(ToolError::ExecutionFailed(format!(
            "{} browser connection failed: {}",
            T::PROVIDER,
            failure.message()
        )));
    }
    Ok(())
}

/// Error for a failed cloud CDP command.
fn cloud_cdp_error(provider: &str, method: &str, failure: CdpFailure) -> ToolError {
    ToolError::ExecutionFailed(format!(
        "{provider} browser CDP command '{method}' failed: {}",
        failure.message()
    ))
}

/// Where a browser backend's CDP commands go: the local DevTools endpoint or
/// a cloud browser session.
trait CdpChannel: Send + Sync {
    /// Send one command within `timeout`. Returns its result and the events
    /// named in `collect` that arrived before it.
    async fn cdp_call(
        &self,
        method: &str,
        params: Value,
        collect: &[&str],
        timeout: Duration,
    ) -> Result<(Value, Vec<Value>), ToolError>;

    /// Timeout for the next `Page.navigate`.
    fn next_navigate_timeout(&self) -> Duration {
        cdp_open_timeout(false)
    }

    /// Make sure no later command reaches the current page, once clearing
    /// it has failed.
    async fn retire_page(&self);

    async fn cdp_command(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        let (result, _) = self
            .cdp_call(method, params, &[], cdp_command_timeout())
            .await?;
        Ok(result)
    }
}

/// A JavaScript string literal for `value`. JSON string syntax is valid
/// JavaScript, so quotes, backslashes and line breaks stay inside the string.
fn js_string(value: &str) -> String {
    Value::String(value.to_owned()).to_string()
}

/// Run `Runtime.evaluate` and fail when the page script throws.
async fn cdp_evaluate<C: CdpChannel>(channel: &C, expression: String) -> Result<Value, ToolError> {
    let result = channel
        .cdp_command(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true}),
        )
        .await?;
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

/// Fail when a selector script reported that no element matched.
fn ensure_element_found(evaluation: &Value, selector: &str) -> Result<(), ToolError> {
    if evaluation.pointer("/result/value").and_then(Value::as_str) == Some("not found") {
        return Err(ToolError::NotFound(format!(
            "No element matches selector {selector}"
        )));
    }
    Ok(())
}

/// `Input.dispatchKeyEvent` fields for a key press, or `None` for a key this
/// backend cannot describe. `text` is present only for keys that type text.
fn cdp_key_fields(key: &str) -> Option<Value> {
    let named = match key {
        "Enter" => Some((13, Some("\r"))),
        "Tab" => Some((9, None)),
        "Escape" => Some((27, None)),
        "Backspace" => Some((8, None)),
        "Delete" => Some((46, None)),
        "ArrowLeft" => Some((37, None)),
        "ArrowUp" => Some((38, None)),
        "ArrowRight" => Some((39, None)),
        "ArrowDown" => Some((40, None)),
        "Home" => Some((36, None)),
        "End" => Some((35, None)),
        "PageUp" => Some((33, None)),
        "PageDown" => Some((34, None)),
        _ => None,
    };
    if let Some((virtual_key_code, text)) = named {
        let mut fields =
            json!({"key": key, "code": key, "windowsVirtualKeyCode": virtual_key_code});
        if let Some(text) = text {
            fields["text"] = json!(text);
        }
        return Some(fields);
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(json!({"key": key, "text": key})),
        _ => None,
    }
}

/// Render one replayed console event as `{type, text}`.
fn cdp_console_entry(event: &Value) -> Value {
    let params = &event["params"];
    if event["method"] == "Runtime.exceptionThrown" {
        let details = &params["exceptionDetails"];
        let text = details
            .pointer("/exception/description")
            .or_else(|| details.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("unknown exception");
        return json!({"type": "exception", "text": text});
    }
    let text = params["args"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|arg| match arg.get("value") {
            Some(Value::String(s)) => s.clone(),
            Some(value) => value.to_string(),
            None => arg
                .get("unserializableValue")
                .or_else(|| arg.get("description"))
                .and_then(Value::as_str)
                .unwrap_or("undefined")
                .to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    json!({"type": params["type"], "text": text})
}

async fn cdp_navigate<C: CdpChannel>(channel: &C, url: &str) -> Result<String, ToolError> {
    validate_url_does_not_exfiltrate_secret(url)?;
    let failed =
        |detail: String| ToolError::ExecutionFailed(format!("Failed to open {url}: {detail}"));
    let (result, _) = channel
        .cdp_call(
            "Page.navigate",
            json!({"url": url}),
            &[],
            channel.next_navigate_timeout(),
        )
        .await
        .map_err(|err| failed(tool_error_message(err)))?;
    // Chrome answers a failed navigation with `errorText` rather than a
    // protocol error.
    if let Some(error_text) = result.get("errorText").and_then(Value::as_str) {
        return Err(failed(error_text.to_string()));
    }
    // `Page.navigate` answers once the page commits, after any HTTP
    // redirects, so the current history entry is the page the browser landed
    // on. Without it a redirect could not be checked, so the page is blanked
    // and navigation fails.
    match cdp_current_url(channel).await {
        Ok(landed) => Ok(json!({"status": "navigated", "url": landed, "cdp": result}).to_string()),
        Err(detail) => {
            let cleared = match cdp_blank_page(channel).await {
                Ok(_) => String::new(),
                Err(blank) => format!("; {}", tool_error_message(blank)),
            };
            Err(ToolError::ExecutionFailed(format!(
                "Opened {url} but could not read the page URL: {detail}{cleared}"
            )))
        }
    }
}

/// The URL of the page's current history entry.
async fn cdp_current_url<C: CdpChannel>(channel: &C) -> Result<String, String> {
    let history = channel
        .cdp_command("Page.getNavigationHistory", json!({}))
        .await
        .map_err(tool_error_message)?;
    history["currentIndex"]
        .as_u64()
        .and_then(|index| history["entries"].get(usize::try_from(index).ok()?))
        .and_then(|entry| entry["url"].as_str())
        .map(str::to_string)
        .ok_or_else(|| "no current history entry".to_string())
}

/// Open `about:blank` and clear the history, so going back cannot reopen the
/// page that was shown. If that fails, the page is retired and the error
/// says so.
async fn cdp_blank_page<C: CdpChannel>(channel: &C) -> Result<String, ToolError> {
    match cdp_clear_page(channel).await {
        Ok(()) => Ok(json!({"status": "blank"}).to_string()),
        Err(err) => {
            channel.retire_page().await;
            Err(ToolError::ExecutionFailed(format!(
                "the page could not be cleared ({}), so the browser session was retired",
                tool_error_message(err)
            )))
        }
    }
}

/// Fails unless `about:blank` is the only page left in the history.
async fn cdp_clear_page<C: CdpChannel>(channel: &C) -> Result<(), ToolError> {
    let opened = channel
        .cdp_command("Page.navigate", json!({"url": "about:blank"}))
        .await?;
    if let Some(error_text) = opened.get("errorText").and_then(Value::as_str) {
        return Err(ToolError::ExecutionFailed(format!(
            "Failed to open about:blank: {error_text}"
        )));
    }
    channel
        .cdp_command("Page.resetNavigationHistory", json!({}))
        .await?;
    let history = channel
        .cdp_command("Page.getNavigationHistory", json!({}))
        .await?;
    match history["entries"].as_array().map(Vec::as_slice) {
        Some([entry]) if entry["url"] == "about:blank" => Ok(()),
        _ => Err(ToolError::ExecutionFailed(
            "The browser history still holds the previous page".into(),
        )),
    }
}

async fn cdp_snapshot<C: CdpChannel>(channel: &C) -> Result<String, ToolError> {
    let result = channel
        .cdp_command("Accessibility.getFullAXTree", json!({}))
        .await?;
    Ok(redact_browser_observation(
        &json!({"status": "snapshot", "cdp": result}).to_string(),
    ))
}

async fn cdp_click<C: CdpChannel>(channel: &C, selector: &str) -> Result<String, ToolError> {
    // The block keeps `el` out of the page's global scope, where a second
    // `let el` would throw.
    let js = format!(
        "{{ let el = document.querySelector({}); if (el) {{ el.click(); 'clicked' }} else {{ 'not found' }} }}",
        js_string(selector)
    );
    let result = cdp_evaluate(channel, js).await?;
    ensure_element_found(&result, selector)?;
    Ok(json!({"status": "clicked", "selector": selector, "cdp": result}).to_string())
}

async fn cdp_type<C: CdpChannel>(
    channel: &C,
    selector: &str,
    text: &str,
) -> Result<String, ToolError> {
    let js = format!(
        "{{ let el = document.querySelector({}); if (el) {{ el.value = {}; el.dispatchEvent(new Event('input')); 'typed' }} else {{ 'not found' }} }}",
        js_string(selector),
        js_string(text)
    );
    let result = cdp_evaluate(channel, js).await?;
    ensure_element_found(&result, selector)?;
    Ok(json!({"status": "typed", "selector": selector, "text": text, "cdp": result}).to_string())
}

async fn cdp_scroll<C: CdpChannel>(
    channel: &C,
    direction: &str,
    amount: Option<u32>,
) -> Result<String, ToolError> {
    let px = amount.unwrap_or(500) as i32;
    let (x, y) = match direction {
        "up" => (0, -px),
        "down" => (0, px),
        "left" => (-px, 0),
        "right" => (px, 0),
        _ => (0, px),
    };
    let js = format!("window.scrollBy({}, {}); 'scrolled'", x, y);
    let result = cdp_evaluate(channel, js).await?;
    Ok(
        json!({"status": "scrolled", "direction": direction, "amount": px, "cdp": result})
            .to_string(),
    )
}

async fn cdp_go_back<C: CdpChannel>(channel: &C) -> Result<String, ToolError> {
    let result = cdp_evaluate(channel, "history.back(); 'back'".into()).await?;
    Ok(json!({"status": "navigated_back", "cdp": result}).to_string())
}

/// Press and release `key`, as Chrome needs for the key's default action.
async fn cdp_press<C: CdpChannel>(channel: &C, key: &str) -> Result<String, ToolError> {
    let fields = cdp_key_fields(key).ok_or_else(|| {
        ToolError::InvalidParams(format!(
            "Unsupported key '{key}': use Enter, Tab, Escape, Backspace, Delete, ArrowLeft, ArrowUp, ArrowRight, ArrowDown, Home, End, PageUp, PageDown or a single character"
        ))
    })?;
    let mut key_down = fields.clone();
    // Chrome types and runs default actions for `keyDown` only when text is
    // present; other keys go down as `rawKeyDown`, as in Puppeteer.
    key_down["type"] = json!(if fields.get("text").is_some() {
        "keyDown"
    } else {
        "rawKeyDown"
    });
    let mut key_up = fields;
    if let Some(fields) = key_up.as_object_mut() {
        fields.remove("text");
    }
    key_up["type"] = json!("keyUp");
    channel
        .cdp_command("Input.dispatchKeyEvent", key_down)
        .await?;
    channel
        .cdp_command("Input.dispatchKeyEvent", key_up)
        .await?;
    Ok(json!({"status": "key_pressed", "key": key}).to_string())
}

async fn cdp_get_images<C: CdpChannel>(
    channel: &C,
    selector: Option<&str>,
) -> Result<String, ToolError> {
    let sel = selector.unwrap_or("img");
    let js = format!(
        "JSON.stringify(Array.from(document.querySelectorAll({})).map(img => ({{src: img.src, alt: img.alt, width: img.width, height: img.height}})))",
        js_string(sel)
    );
    let result = cdp_evaluate(channel, js).await?;
    Ok(json!({"status": "images_found", "selector": sel, "cdp": result}).to_string())
}

async fn cdp_screenshot<C: CdpChannel>(channel: &C) -> Result<String, ToolError> {
    let result = channel
        .cdp_command("Page.captureScreenshot", json!({"format": "png"}))
        .await?;
    let data = result["data"].as_str().ok_or_else(|| {
        ToolError::ExecutionFailed("Page.captureScreenshot returned no data".into())
    })?;
    let png =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "Page.captureScreenshot returned invalid base64: {e}"
            ))
        })?;
    let path = save_browser_screenshot(&png).await?;
    Ok(json!({"status": "screenshot_captured", "screenshot_path": path}).to_string())
}

async fn cdp_console<C: CdpChannel>(channel: &C, action: &str) -> Result<String, ToolError> {
    match action {
        "read" => {
            // Enabling the Runtime domain replays the console calls and
            // exceptions the page has stored before the command's response.
            let (_, events) = channel
                .cdp_call(
                    "Runtime.enable",
                    json!({}),
                    CDP_CONSOLE_EVENTS,
                    cdp_command_timeout(),
                )
                .await?;
            // Stop the stream on a kept connection; a later read replays again.
            channel.cdp_command("Runtime.disable", json!({})).await?;
            let messages: Vec<Value> = events.iter().map(cdp_console_entry).collect();
            Ok(redact_browser_observation(
                &json!({"status": "console_read", "messages": messages}).to_string(),
            ))
        }
        "clear" => {
            let result = cdp_evaluate(channel, "console.clear(); 'cleared'".into()).await?;
            Ok(json!({"status": "console_cleared", "cdp": result}).to_string())
        }
        other => Err(ToolError::InvalidParams(format!(
            "Unknown console action: {}",
            other
        ))),
    }
}
