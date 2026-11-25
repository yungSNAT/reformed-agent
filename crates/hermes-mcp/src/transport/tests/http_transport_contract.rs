use super::*;

#[tokio::test]
async fn legacy_http_start_send_receive_and_close_manage_the_response_slot() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", server.local_addr().unwrap());
        let mut transport = HttpTransport::new(&url, Some("fixture-bearer".into()));
        transport.client = reqwest::Client::builder().no_proxy().build().unwrap();

        let (started, head) =
            tokio::join!(transport.start(), answer_once(&server, json_reply("{}")));
        started.unwrap();
        assert!(head.starts_with("get /mcp/health "));
        assert!(matches!(
            transport.start().await,
            Err(McpError::ConnectionError(reason)) if reason.contains("already started")
        ));

        let request = serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "ping"});
        let response = serde_json::json!({"jsonrpc": "2.0", "id": 7, "result": {}});
        transport.set_negotiated_protocol_version("2025-03-26");
        let (sent, (head, body)) = tokio::join!(
            transport.send(request.clone()),
            answer_once_with_body(&server, json_reply(&response.to_string())),
        );
        sent.unwrap();
        assert!(head.starts_with("post /mcp/message "));
        assert!(head.contains("authorization: bearer fixture-bearer"));
        assert!(head.contains("mcp-protocol-version: 2025-03-26"));
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), request);
        assert_eq!(transport.receive().await.unwrap(), response);
        assert!(matches!(
            transport.receive().await,
            Err(McpError::ConnectionError(reason)) if reason.contains("No pending response")
        ));

        let (sent, _) = tokio::join!(
            transport.send(request.clone()),
            answer_once(
                &server,
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 6\r\n\r\ndenied".into(),
            ),
        );
        assert!(matches!(sent, Err(McpError::Auth(reason)) if reason.contains("denied")));
        let (sent, _) = tokio::join!(
            transport.send(request.clone()),
            answer_once(&server, json_reply("invalid json")),
        );
        assert!(matches!(sent, Err(McpError::Protocol { code: -32700, .. })));

        let (sent, _) = tokio::join!(
            transport.send(request),
            answer_once(&server, json_reply(&response.to_string())),
        );
        sent.unwrap();
        transport.close().await.unwrap();
        assert!(transport.receive().await.is_err());
        assert!(transport.protocol_version.is_none());
        let (started, _) = tokio::join!(transport.start(), answer_once(&server, json_reply("{}")));
        started.unwrap();
    })
    .await
    .expect("loopback HTTP exchange timed out");
}

#[tokio::test]
async fn legacy_http_rejects_an_invalid_server_url() {
    let mut transport = HttpTransport::new("http://[", None);
    assert!(matches!(
        transport.start().await,
        Err(McpError::ConnectionError(reason)) if reason.contains("Failed to connect")
    ));
    assert!(matches!(
        transport.send(serde_json::json!({})).await,
        Err(McpError::ConnectionError(reason)) if reason.contains("HTTP send failed")
    ));
}
