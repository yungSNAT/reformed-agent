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

/// The next text frame as JSON; a missing reply fails the test instead of hanging it.
async fn next_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
            .await
            .expect("no frame within 10 seconds")
            .expect("socket closed");
        match frame.unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

/// `auth_required: false` tells the desktop this is a token gateway; `true` means an OAuth
/// gate (upstream `apps/desktop/electron/connection-config.ts`, `authModeFromStatus`).
#[tokio::test]
async fn status_and_health_are_public_and_report_token_auth() {
    let addr = start().await;
    for path in ["/api/status", "/api/health"] {
        let response = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
        assert_eq!(response.status(), 200, "{path}");
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"), "{path}");
        assert_eq!(body["auth_required"], false, "{path}");
    }
    let health: Value = reqwest::get(format!("http://{addr}/api/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["ok"], true);
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

    // A batch gets one reply per request, in order; its notification gets none.
    let batch = json!([
        {"jsonrpc": "2.0", "id": 1, "method": "a"},
        {"jsonrpc": "2.0", "method": "notified"},
        {"jsonrpc": "2.0", "id": "two", "method": "b"},
        7,
    ]);
    socket.send(Message::text(batch.to_string())).await.unwrap();
    let reply = next_json(&mut socket).await;
    let replies = reply.as_array().expect("a batch reply is an array");
    assert_eq!(replies.len(), 3, "{reply}");
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["error"]["code"], -32601);
    assert_eq!(replies[1]["id"], "two");
    assert_eq!(replies[1]["error"]["code"], -32601);
    assert_eq!(replies[2]["id"], Value::Null);
    assert_eq!(replies[2]["error"]["code"], -32600);

    socket.send(Message::text("[]")).await.unwrap();
    let reply = next_json(&mut socket).await;
    assert_eq!(reply["error"]["code"], -32600);

    // Malformed requests get -32600, alone or in a batch; a response to us still gets none.
    let malformed = [
        json!({"jsonrpc": "2.0", "id": 5, "method": 7}),
        json!({"jsonrpc": "2.0", "id": 6}),
        json!({"jsonrpc": "2.0"}),
    ];
    for frame in &malformed {
        socket.send(Message::text(frame.to_string())).await.unwrap();
        let reply = next_json(&mut socket).await;
        assert_eq!(reply["error"]["code"], -32600, "{frame}");
    }
    let batch = json!([
        malformed[0],
        {"jsonrpc": "2.0", "id": 8, "result": {}},
        malformed[1],
    ]);
    socket.send(Message::text(batch.to_string())).await.unwrap();
    let reply = next_json(&mut socket).await;
    let replies = reply.as_array().expect("a batch reply is an array");
    assert_eq!(replies.len(), 2, "{reply}");
    assert_eq!(replies[0]["id"], 5);
    assert_eq!(replies[0]["error"]["code"], -32600);
    assert_eq!(replies[1]["id"], 6);
    assert_eq!(replies[1]["error"]["code"], -32600);

    // A request needs "jsonrpc": "2.0" and a non-empty method, with or without an id.
    let unversioned = [
        json!({"jsonrpc": "1.0", "method": "x"}),
        json!({"jsonrpc": "2.0", "method": ""}),
        json!({"id": 9, "method": "x"}),
    ];
    for frame in &unversioned {
        socket.send(Message::text(frame.to_string())).await.unwrap();
        let reply = next_json(&mut socket).await;
        assert_eq!(reply["error"]["code"], -32600, "{frame}");
        assert_eq!(reply["id"], frame.get("id").cloned().unwrap_or(Value::Null));
    }
    let batch = json!([
        unversioned[0],
        {"jsonrpc": "2.0", "method": "notified"},
        unversioned[1],
    ]);
    socket.send(Message::text(batch.to_string())).await.unwrap();
    let reply = next_json(&mut socket).await;
    let replies = reply.as_array().expect("a batch reply is an array");
    assert_eq!(replies.len(), 2, "{reply}");
    assert!(
        replies.iter().all(|r| r["error"]["code"] == -32600),
        "{reply}"
    );
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A socket past `gateway.ready`.
async fn connected() -> Socket {
    let addr = start().await;
    let (mut socket, _) =
        tokio_tungstenite::connect_async(ws_request(addr, &format!("?token={TOKEN}")))
            .await
            .unwrap();
    next_json(&mut socket).await; // gateway.ready
    socket
}

/// An id must be a string, a number or null, and params an array or an object; otherwise
/// the frame is an invalid request, answered with a null id.
#[tokio::test]
async fn an_id_or_params_of_the_wrong_type_makes_an_invalid_request() {
    let mut socket = connected().await;
    for frame in [
        json!({"jsonrpc": "2.0", "id": {"a": 1}, "method": "x"}),
        json!({"jsonrpc": "2.0", "id": [1], "method": "x"}),
        json!({"jsonrpc": "2.0", "id": true, "method": "x"}),
        json!({"jsonrpc": "2.0", "id": [1], "method": 7}),
        json!({"jsonrpc": "2.0", "id": 1, "method": "x", "params": 5}),
        json!({"jsonrpc": "2.0", "id": 1, "method": "x", "params": "bar"}),
    ] {
        socket.send(Message::text(frame.to_string())).await.unwrap();
        let reply = next_json(&mut socket).await;
        assert_eq!(reply["error"]["code"], -32600, "{frame}");
        let expected_id = match &frame["id"] {
            id @ Value::Number(_) => id.clone(),
            _ => Value::Null,
        };
        assert_eq!(reply["id"], expected_id, "{frame}");
    }
    // Valid ids and params still reach method lookup.
    for frame in [
        json!({"jsonrpc": "2.0", "id": null, "method": "x"}),
        json!({"jsonrpc": "2.0", "id": 1.5, "method": "x", "params": [1]}),
        json!({"jsonrpc": "2.0", "id": "s", "method": "x", "params": {}}),
    ] {
        socket.send(Message::text(frame.to_string())).await.unwrap();
        let reply = next_json(&mut socket).await;
        assert_eq!(reply["error"]["code"], -32601, "{frame}");
        assert_eq!(reply["id"], frame["id"], "{frame}");
    }
}

#[tokio::test]
async fn a_batch_over_the_length_limit_gets_one_error() {
    use hermes_desktop_gateway::MAX_BATCH;
    let mut socket = connected().await;
    let request = |n: usize| json!({"jsonrpc": "2.0", "id": n, "method": "x"});

    let at_limit: Vec<Value> = (0..MAX_BATCH).map(request).collect();
    socket
        .send(Message::text(Value::Array(at_limit).to_string()))
        .await
        .unwrap();
    let reply = next_json(&mut socket).await;
    assert_eq!(reply.as_array().map(Vec::len), Some(MAX_BATCH));

    let over: Vec<Value> = (0..=MAX_BATCH).map(request).collect();
    socket
        .send(Message::text(Value::Array(over).to_string()))
        .await
        .unwrap();
    let reply = next_json(&mut socket).await;
    assert!(reply.is_object(), "one error object, not a batch reply");
    assert_eq!(reply["id"], Value::Null);
    assert_eq!(reply["error"]["code"], -32600);
}

/// A request of exactly `len` bytes.
fn request_of_len(len: usize) -> String {
    let base = json!({"jsonrpc": "2.0", "id": 1, "method": "x", "params": {"p": ""}}).to_string();
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "x",
        "params": {"p": "a".repeat(len - base.len())}})
    .to_string();
    assert_eq!(request.len(), len);
    request
}

/// Sends `text` as one message split across 2 frames, each under the frame limit, so only
/// the message limit applies to it.
async fn send_in_2_frames(socket: &mut Socket, text: String) -> Result<(), tungstenite::Error> {
    use tungstenite::protocol::frame::coding::{Data, OpCode};
    use tungstenite::protocol::frame::Frame;
    let bytes = text.into_bytes();
    let (head, tail) = bytes.split_at(bytes.len() / 2);
    let first = Frame::message(head.to_vec(), OpCode::Data(Data::Text), false);
    let last = Frame::message(tail.to_vec(), OpCode::Data(Data::Continue), true);
    socket.send(Message::Frame(first)).await?;
    socket.send(Message::Frame(last)).await
}

#[tokio::test]
async fn a_message_over_the_size_limit_closes_the_socket() {
    use hermes_desktop_gateway::MAX_MESSAGE_BYTES;
    let mut socket = connected().await;
    // At the limit, in one frame and in 2.
    socket
        .send(Message::text(request_of_len(MAX_MESSAGE_BYTES)))
        .await
        .unwrap();
    assert_eq!(next_json(&mut socket).await["error"]["code"], -32601);
    send_in_2_frames(&mut socket, request_of_len(MAX_MESSAGE_BYTES))
        .await
        .unwrap();
    assert_eq!(next_json(&mut socket).await["error"]["code"], -32601);

    // The send itself may fail once the server drops the connection.
    let _ = send_in_2_frames(&mut socket, request_of_len(MAX_MESSAGE_BYTES + 1)).await;
    let next = tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
        .await
        .expect("the socket neither replied nor closed within 10 seconds");
    let closed = matches!(next, None | Some(Err(_)) | Some(Ok(Message::Close(_))));
    assert!(closed, "expected the socket to close, got a {} reply", {
        next.and_then(Result::ok).map_or(0, |m| m.len())
    });
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
        format!("http://localhost:{}@attacker.example", addr.port()),
        format!("http://attacker.example@localhost:{}x", addr.port()),
        // Not the packaged app: sandboxed frames and data: pages send null.
        "null".to_string(),
        "app://hermes".to_string(),
        "file://attacker.example".to_string(),
    ] {
        let mut request = ws_request(addr, &format!("?token={TOKEN}"));
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        assert_eq!(refused_status(request).await, 403, "{origin}");
    }
}

/// The packaged desktop loads its renderer from a file, so its socket sends exactly
/// `Origin: file://` (upstream's tests/hermes_cli/test_dashboard_auth_ws_auth.py); a client
/// without a browser, such as the desktop's main-process probe, sends none.
#[tokio::test]
async fn the_packaged_app_and_a_non_browser_client_may_connect() {
    let addr = start().await;
    for origin in ["file://", ""] {
        let mut request = ws_request(addr, &format!("?token={TOKEN}"));
        if !origin.is_empty() {
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
        }
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
