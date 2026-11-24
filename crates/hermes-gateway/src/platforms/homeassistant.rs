//! Home Assistant REST API + WebSocket adapter.
//!
//! Inbound: subscribes to `state_changed` events on the WebSocket API, keeps
//! the entities `watch_domains`, `watch_entities` or `watch_all` select (minus
//! `ignore_entities`), applies a per-entity `cooldown_seconds`, and forwards a
//! readable description to the agent on the `ha_events` channel. Outbound:
//! replies to `ha_events` become persistent notifications; any other chat id
//! names a `notify` service.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use crate::gateway::IncomingMessage;

/// Chat id of the inbound event channel; replies to it become persistent notifications.
pub const EVENTS_CHAT_ID: &str = "ha_events";
pub const MAX_MESSAGE_LENGTH: usize = 4096;
pub const DEFAULT_URL: &str = "http://homeassistant.local:8123";
const DEFAULT_COOLDOWN_SECONDS: u64 = 30;
const BACKOFF_SECS: [u64; 4] = [5, 10, 30, 60];
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const HEARTBEAT: Duration = Duration::from_secs(30);

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeAssistantConfig {
    pub base_url: String,
    pub long_lived_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_id: Option<String>,
    #[serde(default)]
    pub watch_domains: Vec<String>,
    #[serde(default)]
    pub watch_entities: Vec<String>,
    #[serde(default)]
    pub ignore_entities: Vec<String>,
    #[serde(default)]
    pub watch_all: bool,
    #[serde(default = "default_cooldown_seconds")]
    pub cooldown_seconds: u64,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

fn default_cooldown_seconds() -> u64 {
    DEFAULT_COOLDOWN_SECONDS
}

impl HomeAssistantConfig {
    /// Read `platforms.homeassistant`: `base_url` or `url` (else `HASS_URL`, else
    /// [`DEFAULT_URL`]), the token (else `HASS_TOKEN`) and the event filters.
    pub fn from_platform_config(platform: &hermes_config::PlatformConfig) -> Self {
        Self::from_platform_config_with_env(platform, |key| std::env::var(key).ok())
    }

    fn from_platform_config_with_env<F>(platform: &hermes_config::PlatformConfig, env: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let extra = &platform.extra;
        let nonempty =
            |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let base_url = nonempty(extra_str(extra, "base_url"))
            .or_else(|| nonempty(extra_str(extra, "url")))
            .or_else(|| nonempty(env("HASS_URL")))
            .unwrap_or_else(|| DEFAULT_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        let long_lived_token = nonempty(platform.token.clone())
            .or_else(|| nonempty(extra_str(extra, "token")))
            .or_else(|| nonempty(extra_str(extra, "long_lived_token")))
            .or_else(|| nonempty(env("HASS_TOKEN")))
            .unwrap_or_default();
        Self {
            base_url,
            long_lived_token,
            webhook_id: nonempty(extra_str(extra, "webhook_id")),
            watch_domains: extra_list(extra, "watch_domains"),
            watch_entities: extra_list(extra, "watch_entities"),
            ignore_entities: extra_list(extra, "ignore_entities"),
            watch_all: extra.get("watch_all").is_some_and(truthy),
            cooldown_seconds: extra
                .get("cooldown_seconds")
                .and_then(seconds)
                .unwrap_or(DEFAULT_COOLDOWN_SECONDS),
            proxy: AdapterProxyConfig::default(),
        }
    }
}

fn extra_str(extra: &HashMap<String, Value>, key: &str) -> Option<String> {
    extra.get(key).and_then(Value::as_str).map(String::from)
}

/// A YAML list, or one comma-separated string.
fn extra_list(extra: &HashMap<String, Value>, key: &str) -> Vec<String> {
    let items: Vec<String> = match extra.get(key) {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        Some(Value::String(s)) => s.split(',').map(String::from).collect(),
        _ => Vec::new(),
    };
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => false,
    }
}

fn seconds(v: &Value) -> Option<u64> {
    let f = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    Some(if f.is_finite() && f > 0.0 {
        f as u64
    } else {
        0
    })
}

/// Python-style rendering of a JSON scalar inside a message.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        Value::Number(n) if n.is_f64() => n.as_f64().map_or_else(|| n.to_string(), py_float),
        other => other.to_string(),
    }
}

/// Python's `repr(float)`: shortest digits, exponent form below 1e-4 and from 1e16.
fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.into();
    }
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if f != 0.0 && !(-4..16).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exp.abs());
    }
    let plain = format!("{f}");
    if plain.contains('.') {
        plain
    } else {
        format!("{plain}.0")
    }
}

fn domain_of(entity_id: &str) -> &str {
    match entity_id.split_once('.') {
        Some((domain, _)) => domain,
        None => "",
    }
}

/// A state object, if present and non-empty.
fn state_object(v: Option<&Value>) -> Option<&serde_json::Map<String, Value>> {
    v.and_then(Value::as_object).filter(|m| !m.is_empty())
}

/// Describe a `state_changed` event; `None` when there is no new state or the state is unchanged.
pub fn format_state_change(
    entity_id: &str,
    old_state: Option<&Value>,
    new_state: Option<&Value>,
) -> Option<String> {
    let new_state = state_object(new_state)?;
    let unknown = Value::String("unknown".into());
    let old_val = state_object(old_state)
        .and_then(|m| m.get("state"))
        .unwrap_or(&unknown);
    let new_val = new_state.get("state").unwrap_or(&unknown);
    if old_val == new_val {
        return None;
    }
    let empty = serde_json::Map::new();
    let attrs = new_state
        .get("attributes")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let attr = |key: &str, default: &str| attrs.get(key).map_or(default.to_string(), py_str);
    let name = attr("friendly_name", entity_id);
    let (old, new) = (py_str(old_val), py_str(new_val));
    let on_off = |v: &str, on: &'static str, off: &'static str| if v == "on" { on } else { off };
    Some(match domain_of(entity_id) {
        "climate" => format!(
            "[Home Assistant] {name}: HVAC mode changed from '{old}' to '{new}' (current: {}, target: {})",
            attr("current_temperature", "?"),
            attr("temperature", "?")
        ),
        "sensor" => {
            let unit = attr("unit_of_measurement", "");
            format!("[Home Assistant] {name}: changed from {old}{unit} to {new}{unit}")
        }
        "binary_sensor" => format!(
            "[Home Assistant] {name}: {} (was {})",
            on_off(&new, "triggered", "cleared"),
            on_off(&old, "triggered", "cleared")
        ),
        "light" | "switch" | "fan" => {
            format!("[Home Assistant] {name}: turned {}", on_off(&new, "on", "off"))
        }
        "alarm_control_panel" => {
            format!("[Home Assistant] {name}: alarm state changed from '{old}' to '{new}'")
        }
        _ => format!("[Home Assistant] {name} ({entity_id}): changed from '{old}' to '{new}'"),
    })
}

fn websocket_url(base_url: &str) -> String {
    let base = if let Some(rest) = base_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base_url.to_string()
    };
    format!("{base}/api/websocket")
}

struct HaInner {
    base: BasePlatformAdapter,
    config: HomeAssistantConfig,
    client: Client,
    watch_domains: HashSet<String>,
    watch_entities: HashSet<String>,
    ignore_entities: HashSet<String>,
    last_event: Mutex<HashMap<String, Instant>>,
    inbound_tx: RwLock<Option<mpsc::Sender<IncomingMessage>>>,
    msg_id: AtomicU64,
    backoff: Vec<Duration>,
    heartbeat: Duration,
}

pub struct HomeAssistantAdapter {
    inner: Arc<HaInner>,
    run_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl HomeAssistantAdapter {
    pub fn new(config: HomeAssistantConfig) -> Result<Self, GatewayError> {
        let base =
            BasePlatformAdapter::new(&config.long_lived_token).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        let set = |items: &[String]| items.iter().cloned().collect::<HashSet<_>>();
        let inner = HaInner {
            watch_domains: set(&config.watch_domains),
            watch_entities: set(&config.watch_entities),
            ignore_entities: set(&config.ignore_entities),
            base,
            config,
            client,
            last_event: Mutex::new(HashMap::new()),
            inbound_tx: RwLock::new(None),
            msg_id: AtomicU64::new(0),
            backoff: BACKOFF_SECS
                .iter()
                .map(|s| Duration::from_secs(*s))
                .collect(),
            heartbeat: HEARTBEAT,
        };
        Ok(Self {
            inner: Arc::new(inner),
            run_task: Mutex::new(None),
        })
    }

    pub fn config(&self) -> &HomeAssistantConfig {
        &self.inner.config
    }

    pub async fn set_inbound_sender(&self, tx: mpsc::Sender<IncomingMessage>) {
        *self.inner.inbound_tx.write().await = Some(tx);
    }

    /// Filter, cool down, format and forward one `state_changed` event object
    /// (the `event` field of a WebSocket `event` message). Returns whether it was forwarded.
    pub async fn handle_event(&self, event: &Value) -> Result<bool, GatewayError> {
        self.inner.handle_event_at(event, Instant::now()).await
    }

    /// Send a notification via Home Assistant REST API.
    pub async fn send_notification(
        &self,
        service: &str,
        message: &str,
    ) -> Result<(), GatewayError> {
        let path = format!("notify/{service}");
        let body = serde_json::json!({ "message": message });
        self.inner.post_service(&path, &body, "HA notify").await
    }

    /// Create a persistent notification titled "Hermes Agent" (the reply path for `ha_events`).
    pub async fn send_persistent_notification(&self, message: &str) -> Result<(), GatewayError> {
        let message: String = message.chars().take(MAX_MESSAGE_LENGTH).collect();
        let body = serde_json::json!({ "title": "Hermes Agent", "message": message });
        self.inner
            .post_service(
                "persistent_notification/create",
                &body,
                "HA persistent notification",
            )
            .await
    }

    /// Call a Home Assistant service.
    pub async fn call_service(
        &self,
        domain: &str,
        service: &str,
        data: &serde_json::Value,
    ) -> Result<(), GatewayError> {
        let path = format!("{domain}/{service}");
        self.inner
            .post_service(&path, data, "HA service call")
            .await
    }
}

impl HaInner {
    async fn post_service(&self, path: &str, body: &Value, what: &str) -> Result<(), GatewayError> {
        let url = format!("{}/api/services/{path}", self.config.base_url);
        let resp = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.long_lived_token),
            )
            .json(body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("{what} failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("{what} error: {text}")));
        }
        Ok(())
    }

    fn has_filters(&self) -> bool {
        self.config.watch_all || !self.watch_domains.is_empty() || !self.watch_entities.is_empty()
    }

    /// Apply the ignore and watch filters, then the per-entity cooldown.
    fn admit(&self, entity_id: &str, now: Instant) -> bool {
        if self.ignore_entities.contains(entity_id) {
            return false;
        }
        if !self.watch_domains.is_empty() || !self.watch_entities.is_empty() {
            if !self.watch_domains.contains(domain_of(entity_id))
                && !self.watch_entities.contains(entity_id)
            {
                return false;
            }
        } else if !self.config.watch_all {
            return false;
        }
        let cooldown = Duration::from_secs(self.config.cooldown_seconds);
        let mut last = self.last_event.lock().unwrap_or_else(|e| e.into_inner());
        if last
            .get(entity_id)
            .is_some_and(|at| now.saturating_duration_since(*at) < cooldown)
        {
            return false;
        }
        last.insert(entity_id.to_string(), now);
        true
    }

    async fn handle_event_at(&self, event: &Value, now: Instant) -> Result<bool, GatewayError> {
        let data = event.get("data");
        let entity_id = data
            .and_then(|d| d.get("entity_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if entity_id.is_empty() || !self.admit(entity_id, now) {
            return Ok(false);
        }
        let Some(text) = format_state_change(
            entity_id,
            data.and_then(|d| d.get("old_state")),
            data.and_then(|d| d.get("new_state")),
        ) else {
            return Ok(false);
        };
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let incoming = IncomingMessage {
            platform: "homeassistant".into(),
            chat_id: EVENTS_CHAT_ID.into(),
            user_id: "homeassistant".into(),
            text,
            message_id: Some(format!("ha_{entity_id}_{unix}")),
            thread_id: None,
            is_dm: false,
        };
        let Some(tx) = self.inbound_tx.read().await.clone() else {
            debug!("HomeAssistant inbound sender unset; dropping event");
            return Ok(false);
        };
        tx.send(incoming).await.map_err(|e| {
            GatewayError::Platform(format!("HomeAssistant inbound channel closed: {e}"))
        })?;
        Ok(true)
    }

    /// Connect, authenticate and subscribe to `state_changed`.
    async fn connect(&self) -> Result<WsStream, GatewayError> {
        let handshake = async {
            let url = websocket_url(&self.config.base_url);
            let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
                .await
                .map_err(|e| GatewayError::ConnectionFailed(format!("HA WebSocket: {e}")))?;
            let msg = recv_json(&mut ws).await?;
            if msg["type"] != "auth_required" {
                return Err(GatewayError::ConnectionFailed(format!(
                    "HA WebSocket: expected auth_required, got {}",
                    msg["type"]
                )));
            }
            let auth =
                serde_json::json!({ "type": "auth", "access_token": self.config.long_lived_token });
            send_json(&mut ws, &auth).await?;
            let msg = recv_json(&mut ws).await?;
            if msg["type"] != "auth_ok" {
                return Err(GatewayError::Auth(format!(
                    "HA WebSocket auth failed: {}",
                    msg["type"]
                )));
            }
            let id = self.msg_id.fetch_add(1, Ordering::Relaxed) + 1;
            let subscribe = serde_json::json!({
                "id": id,
                "type": "subscribe_events",
                "event_type": "state_changed",
            });
            send_json(&mut ws, &subscribe).await?;
            let msg = recv_json(&mut ws).await?;
            if msg["success"] != Value::Bool(true) {
                return Err(GatewayError::Platform(format!(
                    "HA WebSocket subscribe failed: {msg}"
                )));
            }
            Ok(ws)
        };
        tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
            .await
            .map_err(|_| {
                GatewayError::ConnectionFailed("HA WebSocket handshake timed out".into())
            })?
    }

    /// Read events until the socket closes, fails or misses a heartbeat reply.
    async fn listen(&self, ws: &mut WsStream) -> Result<(), GatewayError> {
        // As aiohttp's heartbeat: ping after `heartbeat` without a frame, then
        // give up if nothing arrives within half of it.
        let mut last_seen = tokio::time::Instant::now();
        let mut reply_due: Option<tokio::time::Instant> = None;
        loop {
            let wake = reply_due.unwrap_or(last_seen + self.heartbeat);
            tokio::select! {
                _ = tokio::time::sleep_until(wake) => {
                    if reply_due.is_some() {
                        return Err(GatewayError::ConnectionFailed("HA WebSocket heartbeat lost".into()));
                    }
                    ws.send(Message::Ping(Vec::new().into())).await.map_err(ws_err)?;
                    reply_due = Some(tokio::time::Instant::now() + self.heartbeat / 2);
                }
                frame = ws.next() => {
                    match frame {
                        Some(Ok(Message::Text(text))) => match serde_json::from_str::<Value>(text.as_str()) {
                            Ok(msg) if msg["type"] == "event" => {
                                if let Err(e) = self.handle_event_at(&msg["event"], Instant::now()).await {
                                    warn!("HomeAssistant event not forwarded: {e}");
                                }
                            }
                            Ok(_) => {}
                            Err(_) => debug!("Invalid JSON from HA WebSocket"),
                        },
                        Some(Ok(Message::Close(_))) | None => return Ok(()),
                        Some(Ok(_)) => {}
                        Some(Err(e)) => return Err(ws_err(e)),
                    }
                    // A slow inbound queue is not a lost connection.
                    last_seen = tokio::time::Instant::now();
                    reply_due = None;
                }
            }
        }
    }

    /// Connect and listen, reconnecting with backoff (reset after each successful connect).
    async fn run(self: Arc<Self>) {
        let mut backoff_idx = 0usize;
        let mut first = true;
        while self.base.is_running() {
            if !first {
                let delay = self.backoff[backoff_idx.min(self.backoff.len() - 1)];
                info!("HomeAssistant reconnecting in {}s", delay.as_secs());
                tokio::time::sleep(delay).await;
                backoff_idx += 1;
            }
            first = false;
            match self.connect().await {
                Ok(mut ws) => {
                    backoff_idx = 0;
                    info!("HomeAssistant connected to {}", self.config.base_url);
                    if let Err(e) = self.listen(&mut ws).await {
                        warn!("HomeAssistant WebSocket error: {e}");
                    }
                }
                Err(e) => warn!("HomeAssistant connection failed: {e}"),
            }
        }
    }
}

async fn recv_json(ws: &mut WsStream) -> Result<Value, GatewayError> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(text.as_str()).map_err(|e| {
                    GatewayError::ConnectionFailed(format!("HA WebSocket sent invalid JSON: {e}"))
                })
            }
            Some(Ok(Message::Close(_))) | None => {
                return Err(GatewayError::ConnectionFailed(
                    "HA WebSocket closed during handshake".into(),
                ))
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(ws_err(e)),
        }
    }
}

async fn send_json(ws: &mut WsStream, value: &Value) -> Result<(), GatewayError> {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .map_err(ws_err)
}

fn ws_err(e: tokio_tungstenite::tungstenite::Error) -> GatewayError {
    GatewayError::ConnectionFailed(format!("HA WebSocket: {e}"))
}

#[async_trait]
impl PlatformAdapter for HomeAssistantAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "HomeAssistant adapter starting (url: {})",
            self.inner.config.base_url
        );
        if !self.inner.has_filters() {
            warn!(
                "HomeAssistant: no watch_domains, watch_entities or watch_all configured; \
                 every state_changed event will be dropped"
            );
        }
        // The lock covers the flag and the handle together, so a concurrent
        // stop either precedes the spawn or finds the handle to abort.
        let mut run_task = self.run_task.lock().unwrap_or_else(|e| e.into_inner());
        self.inner.base.mark_running();
        if let Some(old) = run_task.replace(tokio::spawn(self.inner.clone().run())) {
            old.abort();
        }
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("HomeAssistant adapter stopping");
        let mut run_task = self.run_task.lock().unwrap_or_else(|e| e.into_inner());
        self.inner.base.mark_stopped();
        if let Some(task) = run_task.take() {
            task.abort();
        }
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        if chat_id == EVENTS_CHAT_ID {
            return self.send_persistent_notification(text).await;
        }
        // Any other chat_id is used as the notification service name
        self.send_notification(chat_id, text).await
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        debug!("HomeAssistant does not support message editing");
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        // Home Assistant notify service supports `data` field with file attachments.
        // Different notification integrations handle this differently; the most common
        // pattern is to pass the file path in `data.attachment.url` or `data.file`.
        let message = caption.unwrap_or("File attachment");
        let body = serde_json::json!({
            "message": message,
            "data": {
                "attachment": {
                    "url": file_path
                },
                "file": file_path
            }
        });
        let path = format!("notify/{chat_id}");
        self.inner
            .post_service(&path, &body, "HA file notify")
            .await
    }

    fn is_running(&self) -> bool {
        self.inner.base.is_running()
    }
    fn platform_name(&self) -> &str {
        "homeassistant"
    }
}

#[cfg(test)]
#[path = "homeassistant_tests.rs"]
mod tests;
