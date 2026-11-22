use super::*;
use axum::body::to_bytes;
use axum::http::HeaderValue;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};

const FAILURE: &str = "coverage fixture rejected delivery";

async fn offline_state(home: &FsPath) -> HttpServerState {
    let mut config = GatewayConfig {
        home_dir: Some(home.to_string_lossy().into_owned()),
        model: Some("openai:coverage-default".into()),
        ..GatewayConfig::default()
    };
    config.sessions.auto_prune = false;
    config.streaming.enabled = false;

    let state = HttpServerState::build(config).await.expect("HTTP state");
    state
        .gateway
        .set_message_handler_with_context(Arc::new(|messages, _| {
            Box::pin(async move {
                let text = messages
                    .iter()
                    .rev()
                    .find(|message| message.role == MessageRole::User)
                    .and_then(|message| message.content.as_deref())
                    .expect("user input");
                Ok(format!("echo: {text}"))
            })
        }))
        .await;
    state
}

fn message_request(text: &str) -> SendMessageRequest {
    SendMessageRequest {
        text: text.into(),
        model: None,
        provider: None,
        personality: None,
        user_id: Some("coverage-user".into()),
    }
}

fn command_request(command: &str) -> CommandRequest {
    CommandRequest {
        command: command.into(),
        session_id: Some("coverage-commands".into()),
        user_id: Some("coverage-user".into()),
    }
}

#[tokio::test]
async fn adapter_acknowledges_unsupported_operations_without_buffering_them() {
    let adapter = HttpPlatformAdapter {
        buf: ChatOutboundBuffer::default(),
    };
    adapter.start().await.expect("start");
    assert!(adapter.is_running());
    assert_eq!(adapter.platform_name(), "http");

    adapter
        .send_message("chat", "original", None)
        .await
        .expect("send");
    adapter
        .edit_message("chat", "message-id", "replacement")
        .await
        .expect("edit acknowledgement");
    adapter
        .send_file("chat", "unused-file", Some("caption"))
        .await
        .expect("file acknowledgement");
    adapter.stop().await.expect("stop");

    assert_eq!(adapter.buf.drain_chat("chat"), ["original"]);
    assert!(adapter.buf.drain_chat("chat").is_empty());
}

#[tokio::test]
async fn message_uses_header_session_and_joins_only_current_output() {
    let home = TempDir::new().unwrap();
    let state = offline_state(home.path()).await;
    let adapter = HttpPlatformAdapter {
        buf: state.outbound.clone(),
    };
    adapter
        .send_message("effective-chat", "stale", None)
        .await
        .unwrap();
    adapter
        .send_message("path-chat", "unrelated", None)
        .await
        .unwrap();

    state
        .gateway
        .set_message_handler_with_context(Arc::new(move |_, ctx| {
            let outbound = adapter.buf.clone();
            Box::pin(async move {
                let adapter = HttpPlatformAdapter { buf: outbound };
                let model = resolve_model_for_gateway("openai:coverage-default", &ctx);
                adapter
                    .send_message("effective-chat", &format!("model={model}"), None)
                    .await?;
                Ok(format!(
                    "personality={}",
                    ctx.personality.as_deref().unwrap_or("<unset>")
                ))
            })
        }))
        .await;

    let mut headers = HeaderMap::new();
    headers.insert(
        SESSION_KEY_HEADER,
        HeaderValue::from_static("  effective-chat  "),
    );
    let mut request = message_request("first turn");
    request.provider = Some("anthropic".into());
    request.model = Some("coverage-model".into());
    request.personality = Some("concise".into());

    let Json(first) = send_message(
        Path("path-chat".into()),
        State(state.clone()),
        headers.clone(),
        Json(request),
    )
    .await
    .unwrap();
    assert_eq!(first.session_id, "effective-chat");
    assert_eq!(
        first.reply,
        "model=anthropic:coverage-model\npersonality=concise"
    );
    assert!(first.message_count > 0);

    let mut request = message_request("second turn");
    request.personality = Some("friendly".into());
    let Json(second) = send_message(
        Path("path-chat".into()),
        State(state.clone()),
        headers,
        Json(request),
    )
    .await
    .unwrap();
    assert_eq!(second.session_id, "effective-chat");
    assert_eq!(
        second.reply,
        "model=anthropic:coverage-model\npersonality=friendly"
    );
    assert!(second.message_count > first.message_count);
    assert_eq!(state.outbound.drain_chat("path-chat"), ["unrelated"]);
    assert!(state.outbound.drain_chat("effective-chat").is_empty());
}

#[tokio::test]
async fn commands_reject_blank_input_and_normalize_bare_commands() {
    let home = TempDir::new().unwrap();
    let state = offline_state(home.path()).await;

    let Json(blank) = exec_command(
        State(state.clone()),
        HeaderMap::new(),
        Json(command_request(" \t\n ")),
    )
    .await
    .unwrap();
    assert!(!blank.accepted);
    assert_eq!(blank.output, "empty command");

    let Json(bare) = exec_command(
        State(state.clone()),
        HeaderMap::new(),
        Json(command_request("  help  ")),
    )
    .await
    .unwrap();
    let Json(prefixed) = exec_command(
        State(state),
        HeaderMap::new(),
        Json(command_request("/help")),
    )
    .await
    .unwrap();
    assert!(bare.accepted && prefixed.accepted);
    assert_eq!(bare.output, prefixed.output);
}

#[tokio::test]
async fn handlers_report_when_there_is_no_output_in_the_http_buffer() {
    let home = TempDir::new().unwrap();
    let state = offline_state(home.path()).await;
    let redirected = ChatOutboundBuffer::default();
    state
        .gateway
        .register_adapter(
            HTTP_PLATFORM,
            Arc::new(HttpPlatformAdapter {
                buf: redirected.clone(),
            }),
        )
        .await;

    let Json(message) = send_message(
        Path("silent-chat".into()),
        State(state.clone()),
        HeaderMap::new(),
        Json(message_request("redirect me")),
    )
    .await
    .unwrap();
    assert_eq!(message.session_id, "silent-chat");
    assert_eq!(message.reply, "(no gateway output)");
    assert_eq!(redirected.drain_chat("silent-chat"), ["echo: redirect me"]);

    let Json(command) = exec_command(
        State(state),
        HeaderMap::new(),
        Json(command_request("/help")),
    )
    .await
    .unwrap();
    assert!(command.accepted);
    assert_eq!(command.output, "(no gateway output)");
}

struct RejectDelivery;

#[async_trait]
impl PlatformAdapter for RejectDelivery {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        _chat_id: &str,
        _text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        Err(GatewayError::Platform(FAILURE.into()))
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Err(GatewayError::Platform(FAILURE.into()))
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Err(GatewayError::Platform(FAILURE.into()))
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        HTTP_PLATFORM
    }
}

async fn reject_gateway_output(state: &HttpServerState) {
    state
        .gateway
        .register_adapter(HTTP_PLATFORM, Arc::new(RejectDelivery))
        .await;
    state
        .gateway
        .set_message_handler_with_context(Arc::new(|_, _| {
            Box::pin(async { Err(GatewayError::Platform(FAILURE.into())) })
        }))
        .await;
}

#[tokio::test]
async fn gateway_errors_become_bad_gateway_responses() {
    let home = TempDir::new().unwrap();
    let state = offline_state(home.path()).await;
    reject_gateway_output(&state).await;

    let error = send_message(
        Path("failed-chat".into()),
        State(state.clone()),
        HeaderMap::new(),
        Json(message_request("cannot deliver")),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert!(error.message.contains(FAILURE));

    let error = exec_command(
        State(state),
        HeaderMap::new(),
        Json(command_request("/help")),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_GATEWAY);
    assert!(error.message.contains(FAILURE));
}

#[tokio::test]
async fn agent_error_preserves_its_message_in_http_display_and_json() {
    let source = AgentError::Io("coverage I/O error".into());
    let message = source.to_string();
    let error = HttpError::from(source);
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.message, message);
    assert_eq!(error.to_string(), format!("400 Bad Request: {message}"));

    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({ "error": message })
    );
}

#[test]
fn rpc_params_distinguish_omission_invalid_values_and_explicit_values() {
    let request: RpcRequest = serde_json::from_value(json!({ "method": "test" })).unwrap();
    assert!(request.id.is_none());
    assert_eq!(request.params, json!({}));

    for (params, expected) in [
        (json!({}), Some(0.25)),
        (json!({ "temperature": null }), Some(0.25)),
        (json!({ "temperature": true }), Some(0.25)),
        (json!({ "temperature": 0 }), Some(0.0)),
        (json!({ "temperature": 0.75 }), Some(0.75)),
        (json!({ "temperature": " 1.25 " }), Some(1.25)),
        (json!({ "temperature": "invalid" }), None),
    ] {
        assert_eq!(rpc_param_f64(&params, "temperature", Some(0.25)), expected);
    }
    assert_eq!(rpc_param_f64(&json!({}), "temperature", None), None);

    let mut variables = rpc_param_variables(&json!({
        "variables": { "topic": "coverage", "items": [1, null] }
    }));
    assert_eq!(variables.remove("topic"), Some(json!("coverage")));
    assert_eq!(variables.remove("items"), Some(json!([1, null])));
    assert!(variables.is_empty());
    assert!(rpc_param_variables(&json!({})).is_empty());
    assert!(rpc_param_variables(&json!({ "variables": ["not an object"] })).is_empty());
}

#[test]
fn auxiliary_main_runtime_requires_a_provider_and_model() {
    assert!(main_runtime_from_model(None).is_none());
    for invalid in ["plain-model", " :model", "provider: ", ":"] {
        assert!(main_runtime_from_model(Some(invalid.into())).is_none());
    }
    assert!(main_runtime_from_model(Some(" openai : model:version ".into())).is_some());
}

async fn call_rpc(state: &HttpServerState, request: Value) -> Value {
    let request = serde_json::from_value(request).expect("RPC request");
    let Json(response) = exec_rpc(State(state.clone()), Json(request)).await;
    serde_json::to_value(response).expect("RPC response")
}

#[tokio::test]
async fn rpc_reports_local_validation_errors_and_autonomy_snapshot() {
    // Isolate the home used by auxiliary-client discovery from this test process.
    const CHILD_HOME: &str = "HERMES_HTTP_LIB_COVERAGE_RPC_HOME";
    let Some(home) = std::env::var_os(CHILD_HOME) else {
        let home = TempDir::new().unwrap();
        let qualified = concat!(
            module_path!(),
            "::rpc_reports_local_validation_errors_and_autonomy_snapshot"
        );
        let (_, test_name) = qualified.split_once("::").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD_HOME, home.path())
            .env("HERMES_HOME", home.path())
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .env("XDG_DATA_HOME", home.path())
            .env("XDG_CACHE_HOME", home.path())
            .output()
            .expect("run RPC test with an isolated home");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("running 1 test"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let home = PathBuf::from(home);
    let state = offline_state(&home).await;

    let unknown = call_rpc(&state, json!({ "id": 5, "method": "  missing.method " })).await;
    assert_eq!(
        unknown,
        json!({
            "id": 5,
            "error": { "code": -32601, "message": "unknown method: missing.method" }
        })
    );
    let empty = call_rpc(&state, json!({ "id": "empty", "method": "llm.oneshot" })).await;
    assert_eq!(
        empty,
        json!({
            "id": "empty",
            "error": {
                "code": 4030,
                "message": "llm.oneshot requires a template or instructions/input"
            }
        })
    );

    // Unknown templates are rejected before generation, with or without session routing.
    let template = "__coverage_nonexistent_template__";
    for params in [
        json!({ "template": template, "temperature": "0.5" }),
        json!({
            "template": template,
            "session_key": "coverage-rpc",
            "user_id": "coverage-user",
            "variables": { "topic": "coverage" },
            "timeout": 1
        }),
    ] {
        let response = call_rpc(
            &state,
            json!({ "id": 6, "method": "llm.oneshot", "params": params }),
        )
        .await;
        assert_eq!(
            response,
            json!({
                "id": 6,
                "error": {
                    "code": 4031,
                    "message": format!("unknown one-shot template: {template}")
                }
            })
        );
    }

    let autonomy = call_rpc(
        &state,
        json!({
            "id": "local",
            "method": "harness.autonomy",
            "params": { "cwd": home.to_string_lossy() }
        }),
    )
    .await;
    assert_eq!(autonomy["id"], "local");
    assert!(autonomy.get("error").is_none());
    let payload = &autonomy["result"]["autonomy"];
    assert!(payload.is_object());
    let rendered = payload.to_string();
    for field in ["ultra_autonomy", "contextlattice", "resource_plan"] {
        assert!(rendered.contains(field), "missing {field}: {rendered}");
    }
}

#[test]
fn provider_api_modes_accept_normalized_names_and_reject_unknown_modes() {
    assert!(matches!(
        parse_provider_api_mode(" Chat-Completions "),
        Some(ApiMode::ChatCompletions)
    ));
    assert!(matches!(
        parse_provider_api_mode("ANTHROPIC_MESSAGES"),
        Some(ApiMode::AnthropicMessages)
    ));
    assert!(matches!(
        parse_provider_api_mode("codex-responses"),
        Some(ApiMode::CodexResponses)
    ));
    assert!(matches!(
        parse_provider_api_mode("bedrock_converse"),
        Some(ApiMode::BedrockConverse)
    ));
    assert!(parse_provider_api_mode("unknown").is_none());
    assert!(parse_provider_api_mode(" ").is_none());
}

#[test]
fn agent_config_copies_the_cheap_model_route() {
    let home = TempDir::new().unwrap();
    let mut config = GatewayConfig {
        home_dir: Some(home.path().to_string_lossy().into_owned()),
        ..GatewayConfig::default()
    };
    config.smart_model_routing.enabled = true;
    config.smart_model_routing.cheap_model = Some(
        serde_json::from_value(json!({
            "provider": "openai",
            "model": "coverage-cheap",
            "base_url": "https://coverage.invalid/v1",
            "api_key_env": "COVERAGE_CHEAP_MODEL_KEY"
        }))
        .unwrap(),
    );
    let expected = config.smart_model_routing.cheap_model.as_ref().unwrap();
    let agent = build_agent_config(&config, "openai:coverage-primary");
    assert!(agent.smart_model_routing.enabled);
    let route = agent.smart_model_routing.cheap_model.unwrap();
    assert_eq!(route.provider, expected.provider);
    assert_eq!(route.model, expected.model);
    assert_eq!(route.base_url, expected.base_url);
    assert_eq!(route.api_key_env, expected.api_key_env);
}

#[test]
fn request_model_resolution_preserves_explicit_qualification() {
    for (default, provider, model, expected) in [
        ("old:default", Some("new"), Some("picked"), "new:picked"),
        (
            "old:default",
            Some("new"),
            Some("other:picked"),
            "other:picked",
        ),
        ("old:default", Some("new"), None, "new:default"),
        ("default", Some("new"), None, "new:default"),
        ("old:default", None, Some("picked"), "picked"),
        ("old:default", None, None, "old:default"),
    ] {
        assert_eq!(resolve_model(default, provider, model), expected);
    }
}

#[tokio::test]
async fn gateway_model_resolution_handles_each_form_of_override() {
    let home = TempDir::new().unwrap();
    let state = offline_state(home.path()).await;
    state
        .gateway
        .set_message_handler_with_context(Arc::new(|_, mut ctx| {
            Box::pin(async move {
                for (default, provider, model, expected) in [
                    ("old:default", Some("new"), Some("picked"), "new:picked"),
                    (
                        "old:default",
                        Some("new"),
                        Some("other:picked"),
                        "other:picked",
                    ),
                    ("old:default", None, Some("picked"), "picked"),
                    ("old:default", Some("new"), None, "new:default"),
                    ("default", Some("new"), None, "new:default"),
                    ("old:default", None, None, "old:default"),
                ] {
                    ctx.provider = provider.map(str::to_owned);
                    ctx.model = model.map(str::to_owned);
                    assert_eq!(resolve_model_for_gateway(default, &ctx), expected);
                }
                Ok("checked model overrides".into())
            })
        }))
        .await;

    let Json(response) = send_message(
        Path("model-resolution".into()),
        State(state),
        HeaderMap::new(),
        Json(message_request("test context")),
    )
    .await
    .unwrap();
    assert_eq!(response.reply, "checked model overrides");
}

#[test]
fn assistant_reply_skips_other_roles_and_assistant_messages_without_content() {
    let messages: Vec<Message> = serde_json::from_value(json!([
        { "role": "assistant", "content": "older reply", "tool_calls": [] },
        { "role": "user", "content": "question", "tool_calls": [] },
        { "role": "assistant", "content": "latest reply", "tool_calls": [] },
        { "role": "assistant", "content": null, "tool_calls": [] },
        { "role": "user", "content": "not a reply", "tool_calls": [] }
    ]))
    .unwrap();
    assert_eq!(extract_last_assistant_reply(&messages), "latest reply");
    assert_eq!(
        extract_last_assistant_reply(&messages[3..]),
        "(no assistant reply)"
    );
    assert_eq!(extract_last_assistant_reply(&[]), "(no assistant reply)");
}

// Axum performs a real HTTP upgrade over an in-memory stream, without a TCP socket.
struct InMemoryListener(Option<DuplexStream>);

impl axum::serve::Listener for InMemoryListener {
    type Io = DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.take() {
            Some(stream) => (stream, ()),
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(())
    }
}

async fn write_client_frame(client: &mut DuplexStream, opcode: u8, payload: &[u8]) {
    assert!(payload.len() < 126);
    let mask = [0x71, 0x36, 0x82, 0x4f];
    let mut frame = vec![0x80 | opcode, 0x80 | u8::try_from(payload.len()).unwrap()];
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(i, byte)| *byte ^ mask[i % mask.len()]),
    );
    client.write_all(&frame).await.unwrap();
}

async fn read_server_text(client: &mut BufReader<DuplexStream>) -> String {
    let mut header = [0; 2];
    client.read_exact(&mut header).await.unwrap();
    assert_eq!(header[0], 0x81, "expected a final text frame");
    assert!(header[1] < 126, "expected an unmasked short frame");
    let mut payload = vec![0; usize::from(header[1])];
    client.read_exact(&mut payload).await.unwrap();
    String::from_utf8(payload).unwrap()
}

#[tokio::test]
async fn websocket_upgrade_routes_raw_and_json_text_and_reports_gateway_errors() {
    let home = TempDir::new().unwrap();
    let state = offline_state(home.path()).await;
    let app: Router = Router::new()
        .route("/v1/ws/{session_id}", get(ws_upgrade))
        .with_state(state.clone());
    let (client, server) = tokio::io::duplex(4096);
    let task = tokio::spawn(async move {
        axum::serve(InMemoryListener(Some(server)), app)
            .await
            .unwrap();
    });

    let exchange = tokio::time::timeout(Duration::from_secs(3), async {
        let mut client = BufReader::new(client);
        client
            .get_mut()
            .write_all(
                concat!(
                    "GET /v1/ws/path-chat HTTP/1.1\r\n\
                  Host: coverage.invalid\r\n\
                  Connection: Upgrade\r\n\
                  Upgrade: websocket\r\n\
                  Sec-WebSocket-Version: 13\r\n\
                  Sec-WebSocket-Key: dGhl",
                    "IHNhbXBsZSBub25jZQ==\r\n\
                  x-hermes-session-key: websocket-chat\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut line = String::new();
        assert_ne!(client.read_line(&mut line).await.unwrap(), 0);
        assert!(line.starts_with("HTTP/1.1 101 "), "{line}");
        loop {
            line.clear();
            assert_ne!(client.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
        }
        assert_eq!(
            read_server_text(&mut client).await,
            "connected session=websocket-chat"
        );

        write_client_frame(client.get_mut(), 2, b"ignored binary").await;
        write_client_frame(client.get_mut(), 1, b"plain text").await;
        assert_eq!(read_server_text(&mut client).await, "echo: plain text");

        write_client_frame(
            client.get_mut(),
            1,
            br#"{"text":"from JSON","user_id":"coverage-user"}"#,
        )
        .await;
        assert_eq!(read_server_text(&mut client).await, "echo: from JSON");
        assert!(
            state
                .gateway
                .session_transcript_len(HTTP_PLATFORM, "websocket-chat", "coverage-user")
                .await
                > 0
        );

        reject_gateway_output(&state).await;
        write_client_frame(client.get_mut(), 1, b"cannot deliver").await;
        let error = read_server_text(&mut client).await;
        assert!(error.starts_with("502 Bad Gateway: "), "{error}");
        assert!(error.contains(FAILURE), "{error}");

        write_client_frame(client.get_mut(), 8, &[]).await;
        let mut tail = Vec::new();
        client.read_to_end(&mut tail).await.unwrap();
        assert!(tail.is_empty() || tail[0] == 0x88, "{tail:?}");
    })
    .await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    exchange.expect("in-memory WebSocket exchange timed out");
}
