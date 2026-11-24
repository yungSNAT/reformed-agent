use super::*;

use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Cases recorded from the Python adapter's `_handle_ha_event` with a scripted clock.
const CASES: &str = include_str!("../../tests/fixtures/homeassistant_state_changed.json");

fn platform(extra: Value) -> hermes_config::PlatformConfig {
    let extra = extra
        .as_object()
        .map(|m| m.clone().into_iter().collect())
        .unwrap_or_default();
    hermes_config::PlatformConfig {
        enabled: true,
        extra,
        ..Default::default()
    }
}

// Distinctive so a leak check cannot match ordinary words such as "token".
const SENTINEL: &str = "zq-sentinel-41";

fn config(extra: Value) -> HomeAssistantConfig {
    HomeAssistantConfig::from_platform_config_with_env(&platform(extra), |key| {
        (key == "HASS_TOKEN").then(|| SENTINEL.to_string())
    })
}

async fn adapter_with_inbox(
    config: HomeAssistantConfig,
) -> (HomeAssistantAdapter, mpsc::Receiver<IncomingMessage>) {
    let mut adapter = HomeAssistantAdapter::new(config).expect("adapter");
    Arc::get_mut(&mut adapter.inner)
        .expect("sole owner")
        .backoff = vec![Duration::from_millis(20)];
    let (tx, rx) = mpsc::channel(16);
    adapter.set_inbound_sender(tx).await;
    (adapter, rx)
}

#[tokio::test]
async fn state_changed_events_match_the_recorded_python_cases() {
    let cases: Value = serde_json::from_str(CASES).expect("fixture");
    for case in cases["cases"].as_array().expect("cases") {
        let name = case["name"].as_str().unwrap_or_default();
        let (adapter, mut rx) = adapter_with_inbox(config(case["extra"].clone())).await;
        let start = Instant::now();
        let events = case["events"].as_array().expect("events");
        let expected = case["expected"].as_array().expect("expected");
        assert_eq!(
            events.len(),
            expected.len(),
            "{name}: fixture lengths differ"
        );
        for (i, (pair, want)) in events.iter().zip(expected).enumerate() {
            let at = start + Duration::from_secs_f64(pair[0].as_f64().expect("time"));
            let forwarded = adapter
                .inner
                .handle_event_at(&pair[1], at)
                .await
                .expect("handled");
            if want.is_null() {
                assert!(!forwarded, "{name} event {i} should be dropped");
                assert!(rx.try_recv().is_err(), "{name} event {i} sent a message");
                continue;
            }
            assert!(forwarded, "{name} event {i} should be forwarded");
            let msg = rx.try_recv().expect("message");
            assert_eq!(msg.text, want["text"].as_str().unwrap(), "{name} event {i}");
            assert_eq!(msg.chat_id, want["chat_id"].as_str().unwrap());
            assert_eq!(msg.user_id, want["user_id"].as_str().unwrap());
            assert_eq!(want["chat_type"], "channel");
            assert!(!msg.is_dm);
            assert_eq!(msg.platform, "homeassistant");
            let entity = pair[1]["data"]["entity_id"].as_str().unwrap();
            let id = msg.message_id.expect("message id");
            assert!(id.starts_with(&format!("ha_{entity}_")), "{id}");
        }
    }
}

#[test]
fn config_reads_filters_cooldown_and_fallbacks() {
    let cfg = config(serde_json::json!({
        "url": "http://ha.lan:8123/",
        "watch_domains": ["climate", " light "],
        "watch_entities": "sensor.a, sensor.b",
        "ignore_entities": ["sensor.uptime"],
        "watch_all": "yes",
        "cooldown_seconds": "12",
    }));
    assert_eq!(cfg.base_url, "http://ha.lan:8123");
    assert_eq!(cfg.long_lived_token, SENTINEL);
    assert_eq!(cfg.watch_domains, ["climate", "light"]);
    assert_eq!(cfg.watch_entities, ["sensor.a", "sensor.b"]);
    assert_eq!(cfg.ignore_entities, ["sensor.uptime"]);
    assert!(cfg.watch_all);
    assert_eq!(cfg.cooldown_seconds, 12);

    let defaults = HomeAssistantConfig::from_platform_config_with_env(
        &hermes_config::PlatformConfig::default(),
        |_| None,
    );
    assert_eq!(defaults.base_url, DEFAULT_URL);
    assert_eq!(defaults.long_lived_token, "");
    assert_eq!(defaults.cooldown_seconds, 30);
    assert!(!defaults.watch_all);

    let from_env = HomeAssistantConfig::from_platform_config_with_env(
        &hermes_config::PlatformConfig {
            extra: [("base_url".to_string(), Value::from("https://ha.example"))]
                .into_iter()
                .collect(),
            ..Default::default()
        },
        |key| match key {
            "HASS_URL" => Some("http://ignored:8123".into()),
            "HASS_TOKEN" => Some(" env-token ".into()),
            _ => None,
        },
    );
    assert_eq!(from_env.base_url, "https://ha.example");
    assert_eq!(from_env.long_lived_token, "env-token");
    assert_eq!(
        config(serde_json::json!({"cooldown_seconds": -4})).cooldown_seconds,
        0
    );
}

#[tokio::test]
async fn a_silent_peer_is_dropped_half_a_heartbeat_after_the_ping() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let mut cfg = config(serde_json::json!({"watch_all": true}));
    cfg.base_url = format!("http://{}", listener.local_addr().expect("addr"));
    let (mut adapter, _rx) = adapter_with_inbox(cfg).await;
    Arc::get_mut(&mut adapter.inner)
        .expect("sole owner")
        .heartbeat = Duration::from_millis(200);
    // The server never reads after the handshake, so it never answers a ping.
    let server = tokio::spawn(async move {
        let ws = accept_handshake(&listener, true).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        drop(ws);
    });
    let mut ws = adapter.inner.connect().await.expect("connect");
    let started = std::time::Instant::now();
    let Err(err) = adapter.inner.listen(&mut ws).await else {
        panic!("a silent peer was kept");
    };
    let waited = started.elapsed();
    assert!(err.to_string().contains("heartbeat lost"), "{err}");
    assert!(
        waited >= Duration::from_millis(280) && waited < Duration::from_secs(2),
        "{waited:?}"
    );
    server.abort();
}

#[test]
fn floats_render_as_python_repr() {
    // Expected strings from CPython's repr().
    for (value, want) in [
        (1e-05, "1e-05"),
        (21.0, "21.0"),
        (0.0001, "0.0001"),
        (1e16, "1e+16"),
        (123.456, "123.456"),
        (-2.5e-7, "-2.5e-07"),
        (1.5e20, "1.5e+20"),
        (0.0, "0.0"),
        (9999999999999998.0, "9999999999999998.0"),
    ] {
        assert_eq!(py_str(&serde_json::json!(value)), want);
    }
    assert_eq!(py_str(&serde_json::json!(21)), "21");
}

#[test]
fn websocket_url_swaps_the_scheme() {
    assert_eq!(
        websocket_url("https://ha.example"),
        "wss://ha.example/api/websocket"
    );
    assert_eq!(
        websocket_url("http://10.0.0.2:8123"),
        "ws://10.0.0.2:8123/api/websocket"
    );
}

async fn recv_text(ws: &mut WebSocketStream<TcpStream>) -> Value {
    loop {
        match ws.next().await.expect("frame").expect("ws") {
            Message::Text(t) => return serde_json::from_str(t.as_str()).expect("json"),
            Message::Close(_) => panic!("client closed"),
            _ => {}
        }
    }
}

async fn send_text(ws: &mut WebSocketStream<TcpStream>, value: Value) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .expect("send");
}

/// Accept one client and run Home Assistant's handshake; `auth_ok` false answers `auth_invalid`.
async fn accept_handshake(
    listener: &TcpListener,
    auth_ok: bool,
) -> Option<WebSocketStream<TcpStream>> {
    let (stream, _) = listener.accept().await.expect("accept");
    let mut ws = tokio_tungstenite::accept_async(stream)
        .await
        .expect("upgrade");
    send_text(&mut ws, serde_json::json!({"type": "auth_required"})).await;
    let auth = recv_text(&mut ws).await;
    assert_eq!(auth["type"], "auth");
    assert_eq!(auth["access_token"], SENTINEL);
    if !auth_ok {
        send_text(&mut ws, serde_json::json!({"type": "auth_invalid"})).await;
        return None;
    }
    send_text(&mut ws, serde_json::json!({"type": "auth_ok"})).await;
    let sub = recv_text(&mut ws).await;
    assert_eq!(sub["type"], "subscribe_events");
    assert_eq!(sub["event_type"], "state_changed");
    send_text(
        &mut ws,
        serde_json::json!({"id": sub["id"], "type": "result", "success": true}),
    )
    .await;
    Some(ws)
}

fn light_event(entity_id: &str, old: &str, new: &str) -> Value {
    serde_json::json!({
        "type": "event",
        "event": {
            "event_type": "state_changed",
            "data": {
                "entity_id": entity_id,
                "old_state": {"state": old},
                "new_state": {"state": new, "attributes": {"friendly_name": "Hall"}},
            },
        },
    })
}

async fn listener_on_localhost() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    (listener, url)
}

#[tokio::test]
async fn listener_forwards_watched_events_and_reconnects() {
    let (listener, url) = listener_on_localhost().await;
    let mut cfg = config(serde_json::json!({"watch_domains": ["light"], "cooldown_seconds": 0}));
    cfg.base_url = url;
    let (adapter, mut rx) = adapter_with_inbox(cfg).await;
    adapter.start().await.expect("start");

    let mut ws = accept_handshake(&listener, true).await.expect("handshake");
    send_text(&mut ws, light_event("sensor.cpu", "1", "2")).await;
    send_text(&mut ws, light_event("light.hall", "off", "on")).await;
    let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("forwarded in time")
        .expect("message");
    assert_eq!(msg.text, "[Home Assistant] Hall: turned on");
    assert_eq!(msg.chat_id, EVENTS_CHAT_ID);
    assert!(rx.try_recv().is_err(), "unwatched sensor was forwarded");

    // Home Assistant restarts: the adapter reconnects, authenticates and resubscribes.
    drop(ws);
    let mut ws = accept_handshake(&listener, true).await.expect("reconnect");
    send_text(&mut ws, light_event("light.hall", "on", "off")).await;
    let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("forwarded after reconnect")
        .expect("message");
    assert_eq!(msg.text, "[Home Assistant] Hall: turned off");

    adapter.stop().await.expect("stop");
    assert!(!adapter.is_running());
}

#[tokio::test]
async fn rejected_token_is_an_auth_error() {
    let (listener, url) = listener_on_localhost().await;
    let mut cfg = config(serde_json::json!({"watch_all": true}));
    cfg.base_url = url;
    let (adapter, _rx) = adapter_with_inbox(cfg).await;
    let server = tokio::spawn(async move { accept_handshake(&listener, false).await });
    let Err(err) = adapter.inner.connect().await else {
        panic!("auth rejected token was accepted");
    };
    assert!(matches!(err, GatewayError::Auth(_)), "{err:?}");
    assert!(!err.to_string().contains(SENTINEL), "token leaked: {err}");
    server.await.expect("server");
}

#[tokio::test]
async fn replies_to_the_event_channel_become_persistent_notifications() {
    let server = MockServer::start().await;
    let long = "x".repeat(MAX_MESSAGE_LENGTH + 10);
    Mock::given(method("POST"))
        .and(path("/api/services/persistent_notification/create"))
        .and(header(
            "Authorization",
            format!("Bearer {SENTINEL}").as_str(),
        ))
        .and(body_json(serde_json::json!({
            "title": "Hermes Agent",
            "message": "x".repeat(MAX_MESSAGE_LENGTH),
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/services/notify/mobile_app_phone"))
        .and(body_json(serde_json::json!({"message": "hi"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = config(serde_json::json!({}));
    cfg.base_url = server.uri();
    let adapter = HomeAssistantAdapter::new(cfg).expect("adapter");
    adapter
        .send_message(EVENTS_CHAT_ID, &long, None)
        .await
        .expect("persistent notification");
    adapter
        .send_message("mobile_app_phone", "hi", None)
        .await
        .expect("notify service");
}
