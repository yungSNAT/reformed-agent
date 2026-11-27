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
async fn legacy_sse_uses_the_default_post_endpoint_and_recovers_after_a_bad_event() {
    use serde_json::json;

    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", server.local_addr().unwrap());
    let mut transport = HttpSseTransport::new(&url, None);
    transport.client = reqwest::Client::builder().no_proxy().build().unwrap();
    let request = json!({"jsonrpc": "2.0", "id": 7, "method": "ping"});
    let ready = json!({"jsonrpc": "2.0", "method": "notifications/ready"});
    let response = json!({"jsonrpc": "2.0", "id": 7, "result": {}});
    let later = json!({"jsonrpc": "2.0", "method": "notifications/resources/list_changed"});
    let batch = json!([ready, response]);
    let events = format!(
        "event: message\ndata: {batch}\n\nevent: message\ndata: not-json\n\nevent: message\ndata: {later}\n\n"
    );

    let serve = async {
        let head = answer_once(&server, json_reply("{}")).await;
        assert!(head.starts_with("get /mcp "), "{head}");
        let (head, body) = answer_once_with_body(
            &server,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into(),
        )
        .await;
        assert!(head.starts_with("post /mcp "), "{head}");
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), request);
        let head = answer_once(
            &server,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{events}",
                events.len()
            ),
        )
        .await;
        assert!(head.starts_with("get /mcp/sse "), "{head}");
        let (head, body) = answer_once_with_body(
            &server,
            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n".into(),
        )
        .await;
        assert!(head.starts_with("post /mcp/message "), "{head}");
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), request);
    };
    let exchange = async {
        transport.start().await.unwrap();
        transport.send(request.clone()).await.unwrap();
        assert_eq!(transport.receive().await.unwrap(), ready);
        assert_eq!(transport.receive().await.unwrap(), response);
        assert!(matches!(
            transport.receive().await,
            Err(McpError::Protocol { code: -32700, message })
                if message.contains("Invalid JSON in event stream")
        ));
        assert_eq!(transport.receive().await.unwrap(), later);
        assert!(matches!(
            transport.receive().await,
            Err(McpError::ConnectionError(reason)) if reason.contains("No pending response")
        ));
        transport.close().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(serve, exchange)
    })
    .await
    .expect("loopback legacy SSE exchange timed out");
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
    let mut stream = HttpSseTransport::new("http://[", None);
    assert!(matches!(
        stream.start().await,
        Err(McpError::ConnectionError(reason)) if reason.contains("Failed to connect")
    ));
}

#[tokio::test]
async fn modern_http_checks_probe_fallback_and_reports_transport_errors() {
    use serde_json::json;

    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", server.local_addr().unwrap());
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}
    });
    let legacy_reply = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";

    tokio::time::timeout(Duration::from_secs(5), async {
        let new_transport = || {
            HttpSseTransport::new(&url, None)
                .with_modern_http(true)
                .unwrap()
        };
        let mut legacy = new_transport();
        legacy.start().await.unwrap();
        let (probe, _) = tokio::join!(
            legacy.probe_modern_http(request.clone()),
            answer_once(&server, legacy_reply.to_owned()),
        );
        assert!(probe.unwrap().is_none());
        assert!(legacy.modern_http.is_none());

        let mut modern = new_transport();
        modern.start().await.unwrap();
        for (response, expected) in [
            (legacy_reply, "Unexpected MCP HTTP response (404)"),
            (
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
                "Unexpected MCP HTTP response (503)",
            ),
        ] {
            let (sent, _) = tokio::join!(
                modern.send(request.clone()),
                answer_once(&server, response.to_owned()),
            );
            assert!(
                matches!(sent, Err(McpError::ConnectionError(ref detail)) if detail == expected)
            );
            assert!(modern.modern_http.is_some());
        }

        let (sent, _) = tokio::join!(
            modern.send(request.clone()),
            answer_once(
                &server,
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".into(),
            ),
        );
        assert!(
            matches!(sent, Err(McpError::Forbidden(ref detail)) if detail == "MCP HTTP status 403")
        );

        let (sent, _) = tokio::join!(
            modern.send(request),
            answer_once(
                &server,
                json_reply(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#)
            ),
        );
        assert!(matches!(sent, Err(McpError::Protocol { code: -32700, .. })));
    })
    .await
    .expect("modern HTTP fixture timed out");
}

#[tokio::test]
async fn modern_http_applies_configured_headers_and_keeps_unique_named_tools() {
    use serde_json::json;

    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", server.local_addr().unwrap());
    let mut transport = HttpSseTransport::new(&url, Some("fixture-bearer".into()))
        .with_headers([("x-fixture-context".into(), "local-fixture".into())])
        .with_modern_http(true)
        .unwrap();
    let list = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}
    });
    let valid = json!({"name": "valid", "inputSchema": {"type": "object"}});
    let duplicate = json!({"name": "duplicate", "inputSchema": {"type": "object"}});
    let unnamed = json!({"inputSchema": {"type": "object"}});
    let listed = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {"resultType": "complete", "tools": [&valid, &duplicate, &duplicate, &unnamed]}
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        transport.start().await.unwrap();
        let (sent, (headers, body)) = tokio::join!(
            transport.send(list.clone()),
            answer_once_with_body(&server, json_reply(&listed.to_string())),
        );
        sent.unwrap();
        assert!(headers.starts_with("post /mcp "));
        assert!(headers.contains("\r\nauthorization: bearer fixture-bearer\r\n"));
        assert!(headers.contains("\r\nx-fixture-context: local-fixture\r\n"));
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap(), list);
        assert_eq!(
            transport.receive().await.unwrap()["result"]["tools"],
            json!([valid])
        );

        let call = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "duplicate", "arguments": {}}
        });
        assert!(matches!(
            transport.send(call).await,
            Err(McpError::MethodNotFound(ref detail)) if detail == "Unknown modern MCP tool"
        ));
        transport.close().await.unwrap();
    })
    .await
    .expect("modern HTTP fixture timed out");
}
