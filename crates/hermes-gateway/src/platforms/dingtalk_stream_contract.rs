use super::*;
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use wiremock::matchers::{body_json, body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn adapter_for_fixture(server: &MockServer) -> DingTalkAdapter {
    let mut config = sample_cfg();
    config.openapi_endpoint = server.uri();
    let mut adapter = DingTalkAdapter::new(config).unwrap();
    Arc::get_mut(&mut adapter.inner).unwrap().client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    adapter
}

#[tokio::test]
async fn websocket_start_delivers_callback_and_acks_duplicate() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "ws://{}/fixture?region=local",
        listener.local_addr().unwrap()
    );
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1.0/gateway/connections/open"))
        .and(body_partial_json(json!({
            "clientId": "test_client",
            "clientSecret": "test_secret",
            "subscriptions": [{"type": "CALLBACK", "topic": CHATBOT_TOPIC}],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "endpoint": endpoint,
            "ticket": "fixture ticket/&",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut peer = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws =
            tokio_tungstenite::accept_hdr_async(socket, |request: &Request, response: Response| {
                assert_eq!(
                    request.uri().path_and_query().unwrap().as_str(),
                    "/fixture?region=local&ticket=fixture+ticket%2F%26",
                );
                Ok(response)
            })
            .await
            .unwrap();
        ws.send(WsMessage::Ping(b"fixture-ping".to_vec().into()))
            .await
            .unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            WsMessage::Pong(b"fixture-ping".to_vec().into()),
        );
        for message_id in ["first-frame", "second-frame"] {
            let frame = callback_frame_json(message_id, "same-message", "café fixture");
            ws.send(WsMessage::Text(frame.into())).await.unwrap();
            let ack = ws.next().await.unwrap().unwrap().into_text().unwrap();
            let ack: Value = serde_json::from_str(&ack).unwrap();
            assert_eq!(ack["code"], 200);
            assert_eq!(ack["headers"]["messageId"], message_id);
        }
    });

    let adapter = adapter_for_fixture(&server);
    let (sender, mut inbound) = mpsc::channel(4);
    adapter.set_inbound_sender(sender).await;
    assert!(!adapter.is_running());
    adapter.start().await.unwrap();
    assert!(adapter.is_running());
    let handshake = tokio::time::timeout(Duration::from_secs(10), &mut peer).await;
    if handshake.is_err() {
        peer.abort();
    }
    adapter.stop().await.unwrap();
    handshake
        .expect("complete loopback DingTalk callback and ACK")
        .expect("WebSocket fixture completed");
    assert!(!adapter.is_running());
    let incoming = tokio::time::timeout(Duration::from_secs(2), inbound.recv())
        .await
        .expect("receive forwarded DingTalk message")
        .expect("inbound channel remains open");
    assert_eq!(incoming.platform, "dingtalk");
    assert_eq!(incoming.chat_id, "conv-1");
    assert_eq!(incoming.user_id, "user-1");
    assert_eq!(incoming.text, "café fixture");
    assert_eq!(incoming.message_id.as_deref(), Some("same-message"));
    assert!(incoming.is_dm);
    assert_eq!(
        adapter.inner.session_webhooks.read().await["conv-1"],
        "https://api.dingtalk.com/v1.0/robot/oapi/inbound/xxx",
    );
    drop(adapter);
    assert!(tokio::time::timeout(Duration::from_secs(2), inbound.recv())
        .await
        .expect("DingTalk closes its inbound channel after stopping")
        .is_none());
}

#[tokio::test]
async fn markdown_replies_use_the_cached_session_and_report_http_failures() {
    let server = MockServer::start().await;
    let adapter = adapter_for_fixture(&server);
    assert!(matches!(
        adapter.send_message("fixture", "reply", None).await,
        Err(GatewayError::SendFailed(message)) if message.contains("inbound message required")
    ));

    let reply = |title: &str, text: &str| json!({"msgtype": "markdown", "markdown": {"title": title, "text": text}});
    for (title, text, status) in [
        ("Hermes", "reply", 200),
        ("Attachment", "caption\n\n[local file: sample.txt]", 200),
        (
            "Attachment",
            "\n\n[file](https://example.invalid/sample.txt)",
            200,
        ),
        ("Hermes", "cannot send", 503),
        ("Attachment", "\n\n[local file: unavailable.txt]", 503),
    ] {
        Mock::given(method("POST"))
            .and(path("/session"))
            .and(body_json(reply(title, text)))
            .respond_with(ResponseTemplate::new(status).set_body_string("fixture response"))
            .expect(1)
            .mount(&server)
            .await;
    }
    adapter
        .inner
        .session_webhooks
        .write()
        .await
        .insert("fixture".into(), format!("{}/session", server.uri()));
    adapter
        .send_message("fixture", "reply", Some(ParseMode::Markdown))
        .await
        .unwrap();
    adapter
        .send_file("fixture", "sample.txt", Some("caption"))
        .await
        .unwrap();
    adapter
        .send_file("fixture", "https://example.invalid/sample.txt", None)
        .await
        .unwrap();
    assert!(matches!(
        adapter.send_message("fixture", "cannot send", None).await,
        Err(GatewayError::SendFailed(message))
            if message.contains("503") && message.contains("fixture response")
    ));
    assert!(matches!(
        adapter.send_file("fixture", "unavailable.txt", None).await,
        Err(GatewayError::SendFailed(message))
            if message.contains("503") && message.contains("fixture response")
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
}
