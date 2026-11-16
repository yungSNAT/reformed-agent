//! The connection contract Hermes Desktop's remote mode relies on: a public `/api/status`
//! probe, then a WebSocket to `/api/ws?token=...` whose first frame is `gateway.ready`.
//! Every refusal happens before the upgrade, so a refused client never gets a socket.

use std::net::SocketAddr;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message};

const TOKEN: &str = "0123456789abcdef0123456789abcdef-test-token";

async fn start() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(hermes_desktop_gateway::serve(listener, TOKEN.to_string()));
    addr
}

fn ws_request(addr: SocketAddr, query: &str) -> tungstenite::handshake::client::Request {
    format!("ws://{addr}/api/ws{query}")
        .into_client_request()
        .unwrap()
}

/// The HTTP status a refused upgrade answered with.
async fn refused_status(request: tungstenite::handshake::client::Request) -> u16 {
    match tokio_tungstenite::connect_async(request).await {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(other) => panic!("expected an HTTP refusal, got {other}"),
        Ok(_) => panic!("the upgrade was accepted"),
    }
}

async fn next_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    loop {
        match socket.next().await.expect("socket closed").unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

#[tokio::test]
async fn status_is_public_and_names_the_version() {
    let addr = start().await;
    let response = reqwest::get(format!("http://{addr}/api/status"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["auth_required"], true);
}

#[tokio::test]
async fn upgrade_without_a_token_is_refused() {
    let addr = start().await;
    assert_eq!(refused_status(ws_request(addr, "")).await, 401);
}

#[tokio::test]
async fn upgrade_with_a_wrong_token_is_refused() {
    let addr = start().await;
    for query in ["?token=wrong", "?token=", &format!("?token={TOKEN}x")] {
        assert_eq!(
            refused_status(ws_request(addr, query)).await,
            401,
            "{query}"
        );
    }
    let prefix = &TOKEN[..TOKEN.len() - 1];
    assert_eq!(
        refused_status(ws_request(addr, &format!("?token={prefix}"))).await,
        401
    );
}

#[tokio::test]
async fn first_frame_is_gateway_ready() {
    let addr = start().await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(ws_request(addr, &format!("?token={TOKEN}")))
            .await
            .unwrap();
    let ready = next_json(&mut socket).await;
    assert_eq!(ready["jsonrpc"], "2.0");
    assert_eq!(ready["method"], "event");
    assert_eq!(ready["params"]["type"], "gateway.ready");
    let payload = &ready["params"]["payload"];
    assert!(payload["skin"].is_object());
    assert_eq!(payload["change_events"], false);
    assert!(payload["replay_epoch"]
        .as_str()
        .is_some_and(|e| !e.is_empty()));
}

#[tokio::test]
async fn an_unknown_method_gets_method_not_found_with_its_id() {
    let addr = start().await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(ws_request(addr, &format!("?token={TOKEN}")))
            .await
            .unwrap();
    next_json(&mut socket).await; // gateway.ready
    let request = json!({"jsonrpc": "2.0", "id": "r1", "method": "no.such.method", "params": {}});
    socket
        .send(Message::text(request.to_string()))
        .await
        .unwrap();
    let reply = next_json(&mut socket).await;
    assert_eq!(reply["id"], "r1");
    assert_eq!(reply["error"]["code"], -32601);

    socket.send(Message::text("not json")).await.unwrap();
    let reply = next_json(&mut socket).await;
    assert_eq!(reply["id"], Value::Null);
    assert_eq!(reply["error"]["code"], -32700);
}

#[tokio::test]
async fn a_foreign_host_header_is_refused() {
    let addr = start().await;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{addr}/api/status"))
        .header("host", format!("attacker.example:{}", addr.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);

    let wrong_port = client
        .get(format!("http://{addr}/api/status"))
        .header("host", format!("127.0.0.1:{}", addr.port().wrapping_add(1)))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_port.status(), 403);

    let mut request = ws_request(addr, &format!("?token={TOKEN}"));
    request.headers_mut().insert(
        "host",
        format!("attacker.example:{}", addr.port()).parse().unwrap(),
    );
    assert_eq!(refused_status(request).await, 403);
}

#[tokio::test]
async fn loopback_names_for_the_bound_port_are_accepted() {
    let addr = start().await;
    let client = reqwest::Client::new();
    for host in [
        format!("127.0.0.1:{}", addr.port()),
        format!("localhost:{}", addr.port()),
        format!("[::1]:{}", addr.port()),
    ] {
        let response = client
            .get(format!("http://{addr}/api/status"))
            .header("host", &host)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{host}");
    }
}

#[tokio::test]
async fn a_foreign_web_origin_is_refused() {
    let addr = start().await;
    for origin in [
        "https://attacker.example".to_string(),
        format!("http://localhost:{}", addr.port().wrapping_add(1)),
        format!("http://attacker.example:{}", addr.port()),
        "http://[::1".to_string(),
    ] {
        let mut request = ws_request(addr, &format!("?token={TOKEN}"));
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        assert_eq!(refused_status(request).await, 403, "{origin}");
    }
}

/// The packaged desktop's renderer is not a web page: Chromium sends `file://`, `null` or
/// the app's own scheme. The token is the boundary there, as upstream's server treats it.
#[tokio::test]
async fn the_packaged_app_origin_may_connect() {
    let addr = start().await;
    for origin in ["file://", "null", "app://hermes"] {
        let mut request = ws_request(addr, &format!("?token={TOKEN}"));
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .unwrap_or_else(|e| panic!("{origin}: {e}"));
        assert_eq!(
            next_json(&mut socket).await["params"]["type"],
            "gateway.ready"
        );
    }
}

#[tokio::test]
async fn a_same_origin_page_may_connect() {
    let addr = start().await;
    let mut request = ws_request(addr, &format!("?token={TOKEN}"));
    request.headers_mut().insert(
        "origin",
        format!("http://127.0.0.1:{}", addr.port()).parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(
        next_json(&mut socket).await["params"]["type"],
        "gateway.ready"
    );
}
