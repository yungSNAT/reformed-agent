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
