//! Voice and updates, as Hermes Desktop v0.23.2 asks for them at start-up: `wake.status` on the
//! socket, `GET /api/audio/voice-live/status` and `GET /api/hermes/update/check`. This gateway
//! serves no wake word, voice chat or self-update, so each answers with upstream's own
//! 'unavailable' shape, which the desktop reads as the feature being off.

use std::net::SocketAddr;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";

async fn start() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let chat = hermes_desktop_gateway::Chat::new(hermes_config::GatewayConfig::default());
    tokio::spawn(hermes_desktop_gateway::serve(
        listener,
        TOKEN.to_string(),
        chat,
    ));
    addr
}

async fn get(addr: SocketAddr, path: &str, token: Option<&str>) -> (u16, Value) {
    let mut request = reqwest::Client::new().get(format!("http://{addr}{path}"));
    if let Some(token) = token {
        request = request.header("X-Hermes-Session-Token", token);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn voice_live_status_reports_gpt_live_unavailable_and_needs_the_token() {
    let addr = start().await;
    let path = "/api/audio/voice-live/status?profile=default";
    assert_eq!(get(addr, path, None).await.0, 401);
    let (status, body) = get(addr, path, Some(TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(body["ok"], true);
    assert_eq!(body["mode"], "chained");
    assert_eq!(body["available"], false);
    assert!(
        body["reason"].as_str().is_some_and(|r| !r.is_empty()),
        "{body}"
    );
    assert_eq!(body["model"], "gpt-live-1");
    assert_eq!(body["voice"], "marin");
}

#[tokio::test]
async fn update_check_reports_no_update_it_can_apply_and_needs_the_token() {
    let addr = start().await;
    let path = "/api/hermes/update/check?profile=default";
    assert_eq!(get(addr, path, None).await.0, 401);
    let (status, body) = get(addr, path, Some(TOKEN)).await;
    assert_eq!(status, 200);
    assert_eq!(body["current_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["update_available"], false);
    assert_eq!(body["can_apply"], false);
    assert_eq!(body["behind"], Value::Null);
    assert_eq!(body["update_command"], "");
    assert!(
        body["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{body}"
    );
    assert!(body["install_method"].is_string());
}

#[tokio::test]
async fn wake_status_reports_the_wake_word_unavailable() {
    let addr = start().await;
    let request = format!("ws://{addr}/api/ws?token={TOKEN}")
        .into_client_request()
        .unwrap();
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": "wake.status",
                       "params": {"client_capture": true, "surface": "gui"}});
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
    let reply = loop {
        let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
            continue;
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        if value["id"] == 1 {
            break value;
        }
    };
    let result = &reply["result"];
    assert_eq!(result["available"], false, "{reply}");
    assert_eq!(result["enabled"], false);
    assert_eq!(result["listening"], false);
    assert_eq!(result["owned_by_caller"], false);
    assert_eq!(result["local_input_available"], false);
    assert!(result["hint"].as_str().is_some_and(|h| !h.is_empty()));
    // Upstream's `WakeStatusResult` fields, with the values its own gateway reports for an
    // unavailable detector, and no others.
    let mut fields = result.as_object().unwrap().clone();
    fields.remove("hint");
    assert_eq!(
        Value::Object(fields),
        json!({
            "listening": false, "owned_by_caller": false, "owner_surface": null,
            "phrase": "hey hermes", "provider": "openwakeword", "configured_surface": "auto",
            "input_device": {"selector": null, "error": null}, "available": false,
            "enabled": false, "audio_silent": false, "capture": "client",
            "local_input_available": false, "sample_rate": 16000, "frame_length": 1280,
        })
    );
}
