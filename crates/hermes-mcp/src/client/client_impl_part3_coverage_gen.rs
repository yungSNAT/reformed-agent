#[tokio::test]
async fn modern_http_keepalive_is_silent_and_resource_reads_add_metadata() {
    let result = json!({"resultType": "complete", "contents": []});
    let (mut client, log) = scripted([reply(1, result.clone())]);
    client.modern_http = true;

    client.keepalive_probe().await.unwrap();
    assert!(log.lock().unwrap().sent.is_empty());
    assert_eq!(client.read_resource("test://notes").await.unwrap(), result);

    let log = log.lock().unwrap();
    assert_eq!(log.sent.len(), 1);
    let message = &log.sent[0];
    assert_eq!(message["method"], "resources/read");
    assert_eq!(message["params"]["uri"], "test://notes");
    assert_eq!(
        message["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        MODERN_PROTOCOL_VERSION
    );
}

#[test]
fn modern_requests_and_results_reject_unsupported_shapes() {
    assert!(matches!(
        McpClient::modern_params(json!(["not an object"])),
        Err(McpError::InvalidParams(message)) if message == "MCP request parameters must be an object"
    ));
    assert_protocol(
        McpClient::parse_modern_result(reply(1, json!({"resultType": "input_required"})))
            .unwrap_err(),
        -32000,
        "MCP server requested input that Hermes did not advertise",
    );
    for result in [json!({}), json!({"resultType": "partial"})] {
        assert_protocol(
            McpClient::parse_modern_result(reply(1, result)).unwrap_err(),
            -32602,
            "MCP response has an invalid or unsupported resultType",
        );
    }
}

#[tokio::test(start_paused = true)]
async fn modern_stdio_aborts_on_broken_or_stalled_write() {
    for (step, expected) in [
        (
            SendStep::Fail(McpError::ConnectionError("broken write".into())),
            "broken write",
        ),
        (SendStep::Stall, "MCP tools/list timed out after 0.1s"),
    ] {
        let (mut client, log) = attached(ScriptedTransport {
            sends: VecDeque::from([step]),
            ..ScriptedTransport::default()
        });
        client.modern_stdio = true;
        seed_caches(&mut client);
        assert!(matches!(
            client
                .send_modern_request("tools/list", json!({}), Duration::from_millis(100))
                .await,
            Err(McpError::ConnectionError(message)) if message == expected
        ));
        assert_detached(&client);
        assert_eq!(log.lock().unwrap().sent.len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn modern_stdio_failed_cancel_aborts_session_and_does_not_replay_request() {
    let (mut client, log) = attached(ScriptedTransport {
        sends: VecDeque::from([
            SendStep::Deliver,
            SendStep::Fail(McpError::ConnectionError("cancel write failed".into())),
        ]),
        wait_for_reply: true,
        ..ScriptedTransport::default()
    });
    client.modern_stdio = true;
    seed_caches(&mut client);

    assert!(matches!(
        client
            .send_modern_request("tools/list", json!({}), Duration::from_millis(100))
            .await,
        Err(McpError::ConnectionError(message)) if message == "MCP tools/list timed out after 0.1s"
    ));
    assert_detached(&client);

    assert!(matches!(
        client.recover_modern_stdio().await,
        Err(McpError::Config(_))
    ));
    assert!(client.modern_stdio);
    assert_detached(&client);

    let log = log.lock().unwrap();
    assert_eq!(log.sent.len(), 2);
    assert_eq!(log.sent[0]["id"], 1);
    assert_eq!(log.sent[0]["method"], "tools/list");
    assert_eq!(
        log.sent[1],
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 1, "reason": "Request timed out"}
        })
    );
}

#[tokio::test(start_paused = true)]
async fn modern_discovery_can_fall_back_without_a_supported_probe() {
    let (mut client, log) = attached(ScriptedTransport {
        wait_for_reply: true,
        ..ScriptedTransport::default()
    });

    assert!(!client.discover_modern_stdio().await.unwrap());
    assert!(!client.discover_modern_http().await.unwrap());
    let log = log.lock().unwrap();
    assert_eq!(log.sent.len(), 2);
    for (message, id) in log.sent.iter().zip([1, 2]) {
        assert_eq!(message["id"], id);
        assert_eq!(message["method"], "server/discover");
        assert_eq!(
            message["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
            MODERN_PROTOCOL_VERSION
        );
    }
}

#[test]
fn discovery_requires_valid_versions_capabilities_and_current_version() {
    for (versions, capabilities) in [(json!([42]), json!({})), (json!([]), json!([]))] {
        assert_protocol(
            McpClient::validate_discovery(reply(
                1,
                json!({
                    "resultType": "complete",
                    "supportedVersions": versions,
                    "capabilities": capabilities
                }),
            ))
            .unwrap_err(),
            -32602,
            "MCP discovery response omitted valid versions or capabilities",
        );
    }
    assert_protocol(
        McpClient::validate_discovery(reply(
            1,
            json!({
                "resultType": "complete",
                "supportedVersions": ["2025-11-25"],
                "capabilities": {}
            }),
        ))
        .unwrap_err(),
        -32022,
        "MCP server did not offer the current protocol version",
    );
}

#[tokio::test]
async fn unsupported_server_request_gets_an_error_without_consuming_client_reply() {
    let (mut client, log) = scripted([
        request(81, "example/unsupported", json!({})),
        reply(1, json!({"contents": []})),
    ]);
    assert_eq!(
        client.read_resource("test://notes").await.unwrap(),
        json!({"contents": []})
    );
    let log = log.lock().unwrap();
    assert_eq!(log.sent.len(), 2);
    assert_eq!(log.sent[1]["jsonrpc"], "2.0");
    assert_eq!(log.sent[1]["id"], 81);
    assert_eq!(log.sent[1]["error"]["code"], -32601);
    assert!(log.sent[1]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Unsupported server-initiated MCP method: example/unsupported"));
}

#[test]
fn reverse_request_error_codes_match_client_error_categories() {
    for (error, expected) in [
        (McpError::MethodNotFound("test".into()), -32601),
        (McpError::InvalidParams("test".into()), -32602),
        (McpError::NotConfigured("test".into()), -32001),
        (McpError::Forbidden("test".into()), -32600),
        (McpError::Serialization("test".into()), -32700),
        (McpError::ConnectionError("test".into()), -32000),
    ] {
        assert_eq!(McpClient::jsonrpc_code_for_error(&error), expected);
    }
}

#[test]
fn rpc_errors_supply_a_blank_message_fallback_and_classify_missing_config() {
    assert_protocol(
        McpClient::parse_jsonrpc_result(error_reply(1, -32123, " \t")).unwrap_err(),
        -32123,
        "ProtocolError(code=-32123)",
    );
    assert!(matches!(
        McpClient::parse_jsonrpc_result(error_reply(2, -32000, " Missing URL for tool ")),
        Err(McpError::NotConfigured(message)) if message == "Missing URL for tool"
    ));
}

#[test]
fn registered_tool_schema_returns_an_independent_copy() {
    let expected = example_schema();
    let handler = RegisteredMcpToolHandler {
        client: Arc::new(tokio::sync::Mutex::new(McpClient::new(
            unconfigured_server(),
        ))),
        original_tool_name: "echo".into(),
        schema: expected.clone(),
        available: Arc::new(AtomicBool::new(true)),
    };
    let mut copy = handler.schema();
    assert_eq!(copy.name, "local_echo");
    assert_eq!(copy.description, "Echo test data");
    assert_eq!(copy.parameters, expected.parameters);

    copy.name.clear();
    assert_ne!(copy.name, handler.schema().name);
    let fresh = handler.schema();
    assert_eq!(fresh.name, expected.name);
    assert_eq!(fresh.description, expected.description);
    assert_eq!(fresh.parameters, expected.parameters);
}
