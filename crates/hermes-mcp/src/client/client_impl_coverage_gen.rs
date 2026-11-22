use super::*;
use serde_json::json;
use std::future::pending;
use std::sync::Mutex as StdMutex;

#[derive(Default)]
struct TransportLog {
    sent: Vec<Value>,
    closes: usize,
}

type Transcript = Arc<StdMutex<TransportLog>>;

enum SendStep {
    Deliver,
    Fail(McpError),
    Stall,
}

#[derive(Default)]
struct ScriptedTransport {
    log: Transcript,
    replies: VecDeque<Value>,
    sends: VecDeque<SendStep>,
    wait_for_reply: bool,
    close_error: Option<McpError>,
}

#[async_trait::async_trait]
impl McpTransport for ScriptedTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        self.log.lock().unwrap().sent.push(message);
        match self.sends.pop_front() {
            None | Some(SendStep::Deliver) => Ok(()),
            Some(SendStep::Fail(error)) => Err(error),
            Some(SendStep::Stall) => pending().await,
        }
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        if let Some(reply) = self.replies.pop_front() {
            return Ok(reply);
        }
        if self.wait_for_reply {
            return pending().await;
        }
        Err(McpError::ConnectionError(
            "Script has no more replies".into(),
        ))
    }

    async fn close(&mut self) -> Result<(), McpError> {
        self.log.lock().unwrap().closes += 1;
        self.close_error.take().map_or(Ok(()), Err)
    }

    async fn probe_modern_http(&mut self, message: Value) -> Result<Option<Value>, McpError> {
        self.log.lock().unwrap().sent.push(message);
        Ok(None)
    }
}

fn unconfigured_server() -> McpServerConfig {
    let mut config = McpServerConfig::stdio("unused-coverage-fixture", Vec::new());
    config.command = None;
    config.url = None;
    config.sampling = None;
    config
}

fn attached(transport: ScriptedTransport) -> (McpClient, Transcript) {
    let log = Arc::clone(&transport.log);
    let mut client = McpClient::new(unconfigured_server());
    client.transport = Some(Box::new(transport));
    client.connected = true;
    client.connected_at = Some(Instant::now());
    (client, log)
}

fn scripted<const N: usize>(replies: [Value; N]) -> (McpClient, Transcript) {
    attached(ScriptedTransport {
        replies: VecDeque::from(replies),
        ..ScriptedTransport::default()
    })
}

fn reply(id: u64, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_reply(id: u64, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn example_schema() -> ToolSchema {
    ToolSchema {
        name: "local_echo".into(),
        description: "Echo test data".into(),
        parameters: mcp_input_schema_to_json_schema(
            serde_json::from_value(json!({"type": "object"})).unwrap(),
        ),
    }
}

fn example_resource() -> Value {
    json!({
        "uri": "test://example/notes",
        "name": "notes",
        "description": "Test notes",
        "mimeType": "text/plain"
    })
}

fn seed_caches(client: &mut McpClient) {
    client.tools.push(example_schema());
    client
        .resources
        .push(serde_json::from_value(example_resource()).unwrap());
}

fn assert_detached(client: &McpClient) {
    assert!(!client.is_connected());
    assert!(client.uptime().is_none());
    assert!(client.transport.is_none());
    assert!(client.cached_tools().is_empty());
    assert!(client.cached_resources().is_empty());
}

fn sampling_settings() -> SamplingConfig {
    let mut config = SamplingConfig {
        enabled: true,
        model: None,
        max_rpm: 12,
        max_tokens_cap: 8,
        timeout_secs: 1,
        max_tool_rounds: 2,
        ..SamplingConfig::default()
    };
    config.allowed_models.clear();
    config
}

fn assert_protocol(error: McpError, expected_code: i64, expected_message: &str) {
    match error {
        McpError::Protocol { code, message } => {
            assert_eq!(code, expected_code);
            assert_eq!(message, expected_message);
        }
        other => panic!("expected protocol error, got {other}"),
    }
}

#[tokio::test]
async fn duplicate_connect_preserves_session_until_disconnect() {
    let (mut client, log) = scripted([]);
    seed_caches(&mut client);
    client.modern_http = true;
    let connected_at = client.connected_at.unwrap();
    let lower_bound = connected_at.elapsed();
    let uptime = client.uptime().unwrap();
    assert!((lower_bound..=connected_at.elapsed()).contains(&uptime));

    assert!(matches!(
        client.connect().await,
        Err(McpError::ConnectionError(message)) if message == "Already connected"
    ));
    assert!(client.is_connected());
    assert_eq!(client.connected_at, Some(connected_at));
    assert_eq!(client.cached_tools().len(), 1);
    assert_eq!(client.cached_resources().len(), 1);

    client.disconnect().await.unwrap();
    assert_detached(&client);
    assert!(!client.uses_modern_protocol());
    let log = log.lock().unwrap();
    assert_eq!(log.closes, 1);
    assert!(log.sent.is_empty());
}

#[tokio::test]
async fn failed_handshake_preserves_original_error_when_close_fails() {
    let transport = ScriptedTransport {
        replies: VecDeque::from([error_reply(1, -32050, "handshake rejected")]),
        close_error: Some(McpError::ConnectionError("close failed".into())),
        ..ScriptedTransport::default()
    };
    let log = Arc::clone(&transport.log);
    let mut client = McpClient::new(unconfigured_server());
    seed_caches(&mut client);

    let error = client
        .finish_connect_with_transport(Box::new(transport))
        .await
        .unwrap_err();
    assert_protocol(error, -32050, "handshake rejected");
    assert_detached(&client);
    let log = log.lock().unwrap();
    assert_eq!(log.closes, 1);
    assert_eq!(log.sent.len(), 1);
    assert_eq!(log.sent[0]["method"], "initialize");
}

#[tokio::test]
async fn resources_ignore_stale_reply_and_preserve_last_good_cache() {
    let uri = "test://example/notes";
    let contents = json!({"contents": [{"uri": uri, "text": "test contents"}]});
    let (mut client, log) = scripted([
        reply(99, json!({"resources": []})),
        reply(1, json!({"resources": [example_resource()]})),
        reply(2, json!({"resources": false})),
        reply(3, contents.clone()),
    ]);

    let listed = client.list_resources().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].uri, uri);
    assert!(matches!(
        client.list_resources().await,
        Err(McpError::Serialization(_))
    ));
    assert_eq!(client.cached_resources().len(), 1);
    assert_eq!(client.cached_resources()[0].uri, uri);
    assert_eq!(client.read_resource(uri).await.unwrap(), contents);
    assert_eq!(
        log.lock().unwrap().sent,
        [
            request(1, "resources/list", json!({})),
            request(2, "resources/list", json!({})),
            request(3, "resources/read", json!({"uri": uri})),
        ]
    );
}

#[tokio::test]
async fn prompts_decode_results_and_reject_invalid_payloads() {
    let prompt = json!({
        "description": "A greeting",
        "messages": [{"role": "user", "content": "Hello, Ada"}]
    });
    let (mut client, log) = scripted([
        reply(
            1,
            json!({"prompts": [{"name": "greet", "description": "A greeting", "arguments": []}]}),
        ),
        reply(2, prompt),
        reply(3, json!({"prompts": false})),
        reply(4, json!({"messages": false})),
    ]);
    let args = HashMap::from([("name".to_string(), "Ada".to_string())]);

    let prompts = client.list_prompts().await.unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].name, "greet");
    assert_eq!(
        client
            .get_prompt("greet", args)
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    assert!(matches!(
        client.list_prompts().await,
        Err(McpError::Serialization(_))
    ));
    assert!(matches!(
        client.get_prompt("greet", HashMap::new()).await,
        Err(McpError::Serialization(_))
    ));
    assert_eq!(
        log.lock().unwrap().sent,
        [
            request(1, "prompts/list", json!({})),
            request(
                2,
                "prompts/get",
                json!({"name": "greet", "arguments": {"name": "Ada"}}),
            ),
            request(3, "prompts/list", json!({})),
            request(4, "prompts/get", json!({"name": "greet", "arguments": {}})),
        ]
    );
}

#[tokio::test]
async fn tool_calls_join_text_preserve_raw_content_and_classify_tool_errors() {
    let raw = json!({
        "content": [
            {"type": "resource", "resource": {"uri": "test://example/empty"}},
            {"type": "text", "text": " \t"},
            {"type": "text", "text": 42}
        ]
    });
    let (mut client, log) = scripted([
        reply(
            1,
            json!({"content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "   "},
                {"type": "text", "text": " second "}
            ]}),
        ),
        reply(2, raw.clone()),
        reply(3, json!({"structuredContent": {"count": 3}})),
        reply(
            4,
            json!({"isError": true, "content": [
                {"type": "text", "text": "Permission denied for test tool"}
            ]}),
        ),
    ]);

    assert_eq!(
        client.call_tool("echo", json!({"value": 1})).await.unwrap(),
        json!("first\n second ")
    );
    assert_eq!(client.call_tool("echo", json!({})).await.unwrap(), raw);
    assert_eq!(
        client.call_tool("echo", json!({})).await.unwrap(),
        json!({"structuredContent": {"count": 3}})
    );
    assert!(matches!(
        client.call_tool("echo", json!({})).await,
        Err(McpError::Forbidden(message)) if message.contains("Permission denied for test tool")
    ));
    assert_eq!(
        log.lock().unwrap().sent[0],
        request(
            1,
            "tools/call",
            json!({"name": "echo", "arguments": {"value": 1}})
        )
    );
}

#[tokio::test(start_paused = true)]
async fn legacy_tool_call_timeout_reports_timeout_without_retrying() {
    let (mut client, log) = attached(ScriptedTransport {
        wait_for_reply: true,
        ..ScriptedTransport::default()
    });
    assert!(matches!(
        client.call_tool("echo", json!({})).await,
        Err(McpError::ConnectionError(message))
            if message.starts_with("MCP call timed out after ")
                && message.contains("(configured timeout: ")
    ));
    assert_eq!(
        log.lock().unwrap().sent,
        [request(
            1,
            "tools/call",
            json!({"name": "echo", "arguments": {}})
        )]
    );
}

#[tokio::test]
async fn keepalive_returns_ping_errors_when_no_fallback_is_applicable() {
    let (mut client, log) = scripted([
        error_reply(1, -32601, "ping unsupported"),
        error_reply(2, -32602, "bad ping arguments"),
    ]);

    assert!(matches!(
        client.keepalive_probe().await,
        Err(McpError::MethodNotFound(message)) if message == "ping unsupported"
    ));
    assert!(!client.ping_unsupported);

    seed_caches(&mut client);
    assert!(matches!(
        client.keepalive_probe().await,
        Err(McpError::InvalidParams(message)) if message == "bad ping arguments"
    ));
    assert!(!client.ping_unsupported);
    assert_eq!(
        log.lock().unwrap().sent,
        [request(1, "ping", json!({})), request(2, "ping", json!({}))]
    );
}

#[tokio::test(start_paused = true)]
async fn keepalive_distinguishes_ping_timeout_from_tools_list_timeout() {
    let (mut client, log) = attached(ScriptedTransport {
        wait_for_reply: true,
        ..ScriptedTransport::default()
    });
    assert!(matches!(
        client.keepalive_probe().await,
        Err(McpError::ConnectionError(message))
            if message == format!("MCP keepalive ping timed out after {MCP_KEEPALIVE_PROBE_TIMEOUT_SECS}s")
    ));
    assert_eq!(log.lock().unwrap().sent, [request(1, "ping", json!({}))]);

    let (mut client, log) = attached(ScriptedTransport {
        replies: VecDeque::from([error_reply(1, -32601, "ping unsupported")]),
        wait_for_reply: true,
        ..ScriptedTransport::default()
    });
    seed_caches(&mut client);
    assert!(matches!(
        client.keepalive_probe().await,
        Err(McpError::ConnectionError(message))
            if message == format!("MCP keepalive tools/list timed out after {MCP_KEEPALIVE_PROBE_TIMEOUT_SECS}s")
    ));
    assert!(client.ping_unsupported);
    assert_eq!(client.cached_tools().len(), 1);
    assert_eq!(
        log.lock().unwrap().sent,
        [
            request(1, "ping", json!({})),
            request(2, "tools/list", json!({}))
        ]
    );
}

#[tokio::test]
async fn failed_keepalive_reconnect_closes_old_transport_and_clears_caches() {
    let (mut client, log) = attached(ScriptedTransport {
        close_error: Some(McpError::ConnectionError("stale transport".into())),
        ..ScriptedTransport::default()
    });
    seed_caches(&mut client);
    client.set_sampling_config(sampling_settings());
    let callback: LlmCallback = Arc::new(|_| panic!("reconnect must not invoke sampling"));
    client.set_sampling_callback(Arc::clone(&callback));

    assert!(matches!(
        client.reconnect_after_keepalive_failure().await,
        Err(McpError::Config(message))
            if message == "server config must specify either command (stdio) or url (http)"
    ));
    assert_detached(&client);
    assert!(client.sampling_config.as_ref().unwrap().enabled);
    assert!(Arc::ptr_eq(
        client.sampling_callback.as_ref().unwrap(),
        &callback
    ));
    assert_eq!(log.lock().unwrap().closes, 1);
}

#[tokio::test]
async fn sampling_rejections_do_not_invoke_callback() {
    let mut client = McpClient::new(unconfigured_server());
    let callback: LlmCallback = Arc::new(|_| panic!("invalid sampling request reached callback"));
    assert!(matches!(
        client.handle_sampling_request(json!({}), &callback).await,
        Err(McpError::Config(message)) if message == "Sampling not configured on this client"
    ));

    let mut config = sampling_settings();
    config.enabled = false;
    client.set_sampling_config(config.clone());
    assert!(matches!(
        client.handle_sampling_request(json!({}), &callback).await,
        Err(McpError::Forbidden(message)) if message == "Sampling is disabled on this client"
    ));

    config.enabled = true;
    config.allowed_models = vec!["allowed".into()];
    client.set_sampling_config(config);
    assert!(matches!(
        client.handle_sampling_request(json!({"model": " other "}), &callback).await,
        Err(McpError::InvalidParams(message)) if message == "Model 'other' is not in the allowed list"
    ));
    assert_eq!(client.sampling_metrics().requests, 3);
    assert_eq!(client.sampling_metrics().errors, 2);

    client.set_sampling_callback(callback);
    client.clear_sampling_callback();
    let response = client
        .handle_server_request_message(request(11, "sampling/createMessage", json!({})))
        .await
        .unwrap();
    assert_eq!(response["id"], 11);
    assert_eq!(response["error"]["code"], -32001);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Sampling callback is not configured"));
    assert_eq!(client.sampling_metrics().requests, 3);
}

#[tokio::test]
async fn sampling_forwards_optional_fields_and_caps_token_budget() {
    let mut client = McpClient::new(unconfigured_server());
    client.set_sampling_config(sampling_settings());
    let callback: LlmCallback = Arc::new(|input| {
        Box::pin(async move {
            assert_eq!(
                input,
                json!({
                    "model": "test-model",
                    "messages": [{"role": "user", "content": "hello"}],
                    "max_tokens": 8,
                    "system_prompt": "Answer briefly",
                    "temperature": 0.25,
                    "stop": ["END"]
                })
            );
            Ok(json!({
                "choices": [{"message": {"content": "answer"}, "finish_reason": "length"}],
                "usage": {"totalTokens": 7}
            }))
        })
    });

    let response = client
        .handle_sampling_request(
            json!({
                "model": " test-model ",
                "modelPreferences": {"hints": [{"name": "ignored-hint"}]},
                "messages": [{"content": "hello"}],
                "maxTokens": 99,
                "systemPrompt": "Answer briefly",
                "temperature": 0.25,
                "stopSequences": ["END"]
            }),
            &callback,
        )
        .await
        .unwrap();
    assert_eq!(
        response,
        json!({
            "role": "assistant",
            "model": "test-model",
            "content": {"type": "text", "text": "answer"},
            "stopReason": "maxTokens"
        })
    );
    assert_eq!(client.sampling_metrics().tokens_used, 7);
    assert_eq!(client.sampling_metrics().errors, 0);
}

#[tokio::test(start_paused = true)]
async fn sampling_callback_failures_timeouts_and_invalid_responses_count_as_errors() {
    let mut client = McpClient::new(unconfigured_server());
    client.set_sampling_config(sampling_settings());

    let failure: LlmCallback =
        Arc::new(|_| Box::pin(async { Err(McpError::Forbidden("callback refused".into())) }));
    assert!(matches!(
        client.handle_sampling_request(json!({}), &failure).await,
        Err(McpError::Forbidden(message)) if message == "callback refused"
    ));

    let stalled: LlmCallback = Arc::new(|_| Box::pin(pending()));
    assert!(matches!(
        client.handle_sampling_request(json!({}), &stalled).await,
        Err(McpError::ConnectionError(message)) if message == "Sampling LLM callback timed out"
    ));

    let no_choices: LlmCallback =
        Arc::new(|_| Box::pin(async { Ok(json!({"choices": [], "usage": {"total_tokens": 3}})) }));
    assert!(matches!(
        client.handle_sampling_request(json!({}), &no_choices).await,
        Err(McpError::Serialization(message)) if message == "Sampling response missing choices[0]"
    ));
    assert_eq!(client.sampling_metrics().requests, 3);
    assert_eq!(client.sampling_metrics().errors, 3);
    assert_eq!(client.sampling_metrics().tokens_used, 3);
}

#[test]
fn sampling_rate_limit_rejects_zero_and_expires_old_timestamps() {
    let mut client = McpClient::new(unconfigured_server());
    assert!(!client.check_sampling_rate_limit(0));
    assert!(client.sampling_rate_timestamps.is_empty());

    let expired = Instant::now() - Duration::from_secs(61);
    client.sampling_rate_timestamps.push_back(expired);
    assert!(client.check_sampling_rate_limit(1));
    assert_eq!(client.sampling_rate_timestamps.len(), 1);
    assert_ne!(client.sampling_rate_timestamps[0], expired);
    assert!(!client.check_sampling_rate_limit(1));
}

#[test]
fn sampling_tool_results_normalize_inputs_and_reset_rounds_when_disabled() {
    let mut client = McpClient::new(unconfigured_server());
    let mut config = sampling_settings();
    let calls = [
        json!({"id": "echo-1", "function": {"name": "echo", "arguments": {"text": "hi"}}}),
        json!({"name": "echo", "input": false}),
    ];

    let response = client
        .build_sampling_tool_use_response(&calls, "test-model", &config)
        .unwrap();
    assert_eq!(
        response,
        json!({
            "role": "assistant",
            "model": "test-model",
            "stopReason": "toolUse",
            "content": [
                {"type": "tool_use", "id": "echo-1", "name": "echo", "input": {"text": "hi"}},
                {"type": "tool_use", "id": "call_1", "name": "echo", "input": {"_raw": "false"}}
            ]
        })
    );
    assert_eq!(client.sampling_tool_rounds, 1);

    config.max_tool_rounds = 0;
    assert!(matches!(
        client.build_sampling_tool_use_response(&calls, "test-model", &config),
        Err(McpError::Forbidden(message))
            if message == "Tool loops disabled for sampling (max_tool_rounds=0)"
    ));
    assert_eq!(client.sampling_tool_rounds, 0);
    assert_eq!(client.sampling_metrics().tool_use_count, 4);
}

#[test]
fn sampling_conversion_handles_plain_missing_and_invalid_message_content() {
    assert_eq!(
        McpClient::convert_mcp_messages_to_openai(&json!({"not": "a message array"})),
        json!([])
    );
    assert_eq!(
        McpClient::convert_mcp_messages_to_openai(&json!([
            {},
            {"role": "assistant", "content": "plain"},
            {"role": "user", "content": {"text": "single block"}},
            {"content": false},
            {"content": [
                {"text": "first"},
                {"content": "second"},
                {"content": {"text": "third"}},
                {"content": [{"text": "fourth"}, {}, {"text": "fifth"}]},
                {"content": 9}
            ]}
        ])),
        json!([
            {"role": "user", "content": ""},
            {"role": "assistant", "content": "plain"},
            {"role": "user", "content": "single block"},
            {"role": "user", "content": ""},
            {"role": "user", "content": "first\nsecond\nthird\nfourth\nfifth"}
        ])
    );
}

#[test]
fn sampling_conversion_preserves_tool_call_ids_and_extracts_result_text() {
    let converted = McpClient::convert_mcp_messages_to_openai(&json!([
        {"role": "assistant", "content": [
            {"type": "text", "text": "Checking"},
            {"type": "tool_use", "id": "lookup-1", "name": "lookup", "input": {"key": "test"}},
            {"type": "tool_use"}
        ]},
        {"role": "user", "content": [
            {"toolUseId": "lookup-1", "content": [{"text": "found"}, {"text": "value"}]},
            {"type": "tool_result", "tool_use_id": "tool_call", "content": "no input"},
            {"type": "text", "text": "continue"}
        ]}
    ]));
    assert_eq!(
        converted,
        json!([
            {"role": "assistant", "content": "Checking", "tool_calls": [
                {"id": "lookup-1", "type": "function",
                 "function": {"name": "lookup", "arguments": "{\"key\":\"test\"}"}},
                {"id": "tool_call", "type": "function",
                 "function": {"name": "unknown_tool", "arguments": "{}"}}
            ]},
            {"role": "tool", "tool_call_id": "lookup-1", "content": "found\nvalue"},
            {"role": "tool", "tool_call_id": "tool_call", "content": "no input"},
            {"role": "user", "content": "continue"}
        ])
    );
}

#[test]
fn sampling_conversion_emits_data_urls_and_combines_image_captions() {
    let converted = McpClient::convert_mcp_messages_to_openai(&json!([
        {"content": [
            {"type": "image", "mime_type": "image/jpeg", "data": "AQI="},
            {"type": "text", "text": "caption"},
            {"type": "image", "data": "AwQ="},
            {"type": "text", "text": "details"},
            {"type": "image"}
        ]}
    ]));
    assert_eq!(
        converted,
        json!([{"role": "user", "content": [
            {"type": "text", "text": "caption\ndetails"},
            {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,AQI="}},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AwQ="}}
        ]}])
    );
}

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
