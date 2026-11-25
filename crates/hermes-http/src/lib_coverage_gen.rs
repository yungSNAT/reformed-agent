use super::*;
use axum::body::to_bytes;
use axum::http::HeaderValue;
use hermes_agent::session_persistence::ImportedSession;
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
async fn server_startup_prunes_only_expired_sessions_when_enabled() {
    let home = TempDir::new().unwrap();
    let store = SessionPersistence::new(home.path());
    let old_time = (jiff::Timestamp::now() - jiff::SignedDuration::from_hours(14 * 24)).to_string();
    let old_messages = [(Message::user("older fixture"), old_time.clone())];
    store
        .import_sessions(|import| {
            import.add_session(&ImportedSession {
                id: "older-fixture",
                model: None,
                platform: Some("http"),
                title: None,
                system_prompt: None,
                model_config: None,
                parent_session_id: None,
                created_at: &old_time,
                updated_at: &old_time,
                ended_at: None,
                end_reason: None,
                messages: &old_messages,
            })
        })
        .unwrap();
    store
        .persist_session(
            "fresh-fixture",
            &[Message::user("keep this fixture")],
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let mut config = GatewayConfig {
        home_dir: Some(format!("  {}  ", home.path().display())),
        ..GatewayConfig::default()
    };
    config.sessions.auto_prune = false;
    config.sessions.retention_days = 7;
    HttpServerState::build(config.clone()).await.unwrap();
    assert_eq!(store.load_session("older-fixture").unwrap().len(), 1);
    assert_eq!(store.get_meta("last_auto_prune").unwrap(), None);

    config.sessions.auto_prune = true;
    config.sessions.retention_days = 30;
    config.sessions.min_interval_hours = 0;
    HttpServerState::build(config.clone()).await.unwrap();
    assert_eq!(store.load_session("older-fixture").unwrap().len(), 1);
    assert!(store.get_meta("last_auto_prune").unwrap().is_some());

    config.sessions.retention_days = 7;
    config.sessions.vacuum_after_prune = true;
    HttpServerState::build(config).await.unwrap();
    assert!(store.load_session("older-fixture").unwrap().is_empty());
    assert_eq!(
        store.load_session("fresh-fixture").unwrap()[0]
            .content
            .as_deref(),
        Some("keep this fixture")
    );
    assert!(store.get_meta("last_auto_prune").unwrap().is_some());
}

#[tokio::test]
async fn server_startup_stays_available_when_session_maintenance_cannot_open_its_home() {
    let home = TempDir::new().unwrap();
    let home_file = home.path().join("unavailable-home");
    std::fs::write(&home_file, b"fixture remains intact").unwrap();
    let mut config = GatewayConfig {
        home_dir: Some(home_file.to_str().unwrap().into()),
        ..GatewayConfig::default()
    };
    config.sessions.auto_prune = true;
    let state = HttpServerState::build(config).await.unwrap();
    assert!(state.config.sessions.auto_prune);
    assert_eq!(std::fs::read(home_file).unwrap(), b"fixture remains intact");
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

include!("lib_part2_coverage_gen.rs");
