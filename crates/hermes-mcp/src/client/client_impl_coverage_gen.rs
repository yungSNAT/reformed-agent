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
        max_tokens_cap: 8, // nosemgrep: codacy.yaml.security.hard-coded-tokens
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

include!("client_impl_part2_coverage_gen.rs");
include!("client_impl_part3_coverage_gen.rs");
