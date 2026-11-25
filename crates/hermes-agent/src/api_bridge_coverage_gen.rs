use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BAD_SCHEME_URI: &str = "unsupported-scheme://localhost";

fn sample_tool() -> ToolSchema {
    serde_json::from_value(serde_json::json!({
        "name": "read_file",
        "description": "Read a file",
        "parameters": {
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            }
        }
    }))
    .expect("tool schema")
}

async fn mount_sse(server: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(body.to_string(), "text/event-stream"),
        )
        .mount(server)
        .await;
}

#[test]
fn chatgpt_base_url_adds_cloudflare_and_beta_headers() {
    let provider = CodexProvider::new("tok").with_base_url("https://chatgpt.com/backend-api/codex");
    assert!(provider.headers.is_empty());
    let headers = provider.request_headers("tok");
    assert!(headers
        .iter()
        .any(|(n, v)| n.eq_ignore_ascii_case("originator") && v == "codex_cli_rs"));
    assert!(headers
        .iter()
        .any(|(n, v)| n.eq_ignore_ascii_case("OpenAI-Beta") && v == CODEX_RESPONSES_BETA_HEADER));
}

#[test]
fn non_chatgpt_request_headers_are_passthrough() {
    let provider =
        CodexProvider::new("tok").with_headers(vec![("X-Custom".to_string(), "1".to_string())]);
    let headers = provider.request_headers("tok");
    assert_eq!(headers, vec![("X-Custom".to_string(), "1".to_string())]);
}

#[test]
fn non_stream_body_requests_json_accept() {
    let provider = CodexProvider::new("k");
    let body = serde_json::json!({"model": "m"});
    let request = provider
        .request_builder("http://localhost/responses", "k", &body)
        .build()
        .expect("request");
    assert_eq!(
        request.headers().get(ACCEPT).and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
        Some("Bearer k")
    );
}

#[test]
fn convert_tools_maps_to_function_format() {
    let tools = vec![sample_tool()];
    let converted = CodexProvider::convert_tools(&tools);
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0]["type"], "function");
    assert_eq!(converted[0]["name"], "read_file");
    assert_eq!(converted[0]["description"], "Read a file");
    assert_eq!(converted[0]["parameters"]["type"], "object");
}

#[test]
fn build_body_sets_optional_fields() {
    let provider = CodexProvider::new("k");
    let tools = vec![sample_tool()];
    let body = provider.build_body(
        &[Message::user("hi")],
        &tools,
        Some(128),
        Some(0.5),
        "codex-mini-latest",
        None,
        true,
    );
    assert_eq!(body["model"], "codex-mini-latest");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_output_tokens"], 128);
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(body["tools"][0]["name"], "read_file");
    assert_eq!(body["instructions"], CodexProvider::default_instructions());
    assert!(body.get("store").is_none());
}

#[test]
fn parse_response_handles_incomplete_and_odd_items() {
    let json = serde_json::json!({
        "output": [
            {"type": "message"},
            {"type": "reasoning", "summary": []},
            {"type": "message", "content": [{"text": "A"}, {"no": "text"}, {"text": "B"}]}
        ],
        "status": "incomplete"
    });
    let resp = CodexProvider::parse_response(&json).unwrap();
    assert_eq!(resp.message.content.as_deref(), Some("AB"));
    assert!(resp.message.tool_calls.is_none());
    assert_eq!(resp.finish_reason.as_deref(), Some("length"));
    assert_eq!(resp.model, "unknown");
    assert!(resp.usage.is_none());
}

#[test]
fn parse_response_passes_through_other_status() {
    let json = serde_json::json!({
        "output": [],
        "status": "cancelled",
        "usage": {"input_tokens": 3, "output_tokens": 4}
    });
    let resp = CodexProvider::parse_response(&json).unwrap();
    assert_eq!(resp.finish_reason.as_deref(), Some("cancelled"));
    assert!(resp.message.content.is_none());
    let usage = resp.usage.expect("usage");
    assert_eq!(usage.prompt_tokens, 3);
    assert_eq!(usage.completion_tokens, 4);
    assert_eq!(usage.total_tokens, 7);
}

#[test]
fn parse_sse_event_block_variants() {
    assert!(CodexProvider::parse_sse_event_block("event: ping\n").is_none());
    assert!(CodexProvider::parse_sse_event_block("event: x\ndata: not json\n").is_none());
    let (et, json) = CodexProvider::parse_sse_event_block(
        "event: response.output_text.delta\ndata: {\"delta\":\"hi\"}\n",
    )
    .expect("event");
    assert_eq!(et, "response.output_text.delta");
    assert_eq!(json["delta"], "hi");
}

#[test]
fn active_credential_reflects_replacement() {
    let provider = CodexProvider::new("old-token");
    assert!(provider.replace_credential("new-token"));
    let active = provider.active_credential();
    assert!(active.is_none() || active.as_deref() == Some("new-token"));
    assert_eq!(provider.effective_api_key(), "new-token");
}

#[tokio::test]
async fn chatgpt_backend_collects_streamed_text_when_completed_is_empty() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: ping\n\n",
        "event: response.created\ndata: {\"id\":\"r1\"}\n\n",
        "event: response.output_text.delta\ndata: {\"delta\":\"Hel\"}\n\n",
        "event: response.output_text.delta\ndata: {\"delta\":\"lo\"}\n\n",
        "event: response.completed\ndata: {\"response\":{\"output\":[]}}\n\n"
    );
    mount_sse(&server, body).await;
    let provider = CodexProvider::openai_pro("tok", "gpt-5.5").with_base_url(server.uri());
    let result = provider
        .chat_completion(&[Message::user("hi")], &[], None, None, None, None)
        .await;
    match result {
        Ok(resp) => {
            assert_eq!(resp.message.content.as_deref(), Some("Hello"));
            assert_eq!(resp.model, "gpt-5.5");
            assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
            assert!(resp.usage.is_none());
            assert!(resp.message.tool_calls.is_none());
        }
        Err(_) => panic!("expected success"),
    }
}

#[tokio::test]
async fn chatgpt_backend_uses_completed_response_with_tool_calls() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: response.completed\ndata: {\"response\":{\"model\":\"gpt-5.5\",\"status\":\"completed\",",
        "\"output\":[{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read_file\",\"arguments\":\"{}\"}]}}\n\n"
    );
    mount_sse(&server, body).await;
    let provider = CodexProvider::openai_pro("tok", "gpt-5.5").with_base_url(server.uri());
    let result = provider
        .chat_completion(&[Message::user("hi")], &[], None, None, None, None)
        .await;
    match result {
        Ok(resp) => {
            let calls = resp.message.tool_calls.expect("tool calls");
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "c1");
            assert_eq!(calls[0].function.name, "read_file");
        }
        Err(_) => panic!("expected success"),
    }
}

#[tokio::test]
async fn chatgpt_backend_reports_failed_response() {
    let server = MockServer::start().await;
    let body = "event: response.failed\ndata: {\"error\":\"boom\"}\n\n";
    mount_sse(&server, body).await;
    let provider = CodexProvider::openai_pro("tok", "gpt-5.5").with_base_url(server.uri());
    let result = provider
        .chat_completion(&[Message::user("hi")], &[], None, None, None, None)
        .await;
    match result {
        Err(AgentError::LlmApi(msg)) => {
            assert!(msg.contains("response failed"), "{msg}");
            assert!(msg.contains("boom"), "{msg}");
        }
        Err(_) => panic!("unexpected error variant"),
        Ok(_) => panic!("expected error"),
    }
}

#[tokio::test]
async fn chat_completion_reports_invalid_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("not json", "application/json"))
        .mount(&server)
        .await;
    let provider = CodexProvider::new("k").with_base_url(server.uri());
    let result = provider
        .chat_completion(&[Message::user("hi")], &[], None, None, None, None)
        .await;
    match result {
        Err(AgentError::LlmApi(msg)) => assert!(msg.contains("Failed to parse response"), "{msg}"),
        Err(_) => panic!("unexpected error variant"),
        Ok(_) => panic!("expected error"),
    }
}

#[tokio::test]
async fn chat_completion_reports_request_failure() {
    let provider = CodexProvider::new("k").with_base_url(BAD_SCHEME_URI);
    let result = provider
        .chat_completion(&[Message::user("hi")], &[], None, None, None, None)
        .await;
    match result {
        Err(AgentError::LlmApi(msg)) => assert!(msg.contains("HTTP request failed"), "{msg}"),
        Err(_) => panic!("unexpected error variant"),
        Ok(_) => panic!("expected error"),
    }
}

#[tokio::test]
async fn stream_yields_text_tool_and_completion_chunks() {
    let server = MockServer::start().await;
    let body = concat!(
        "event: ping\n\n",
        "event: response.created\ndata: {\"id\":\"r1\"}\n\n",
        "event: response.output_text.delta\ndata: {\"delta\":\"Hi\"}\n\n",
        "event: response.function_call_arguments.delta\ndata: {\"call_id\":\"c1\",\"name\":\"read_file\",\"delta\":\"{}\"}\n\n",
        "event: response.completed\ndata: {\"response\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n",
        "event: response.output_text.delta\ndata: {\"delta\":\"ignored\"}\n\n"
    );
    mount_sse(&server, body).await;
    let provider = CodexProvider::new("k").with_base_url(server.uri());
    let tools = vec![sample_tool()];
    let extra = serde_json::json!({"service_tier": "fast"});
    let chunks: Vec<_> = provider
        .chat_completion_stream(
            &[Message::user("hi")],
            &tools,
            Some(64),
            Some(0.2),
            Some("codex-mini-latest"),
            Some(&extra),
        )
        .collect()
        .await;
    assert_eq!(chunks.len(), 3);

    match &chunks[0] {
        Ok(chunk) => {
            let delta = chunk.delta.as_ref().expect("delta");
            assert_eq!(delta.content.as_deref(), Some("Hi"));
            assert!(chunk.finish_reason.is_none());
        }
        Err(_) => panic!("expected text chunk"),
    }
    match &chunks[1] {
        Ok(chunk) => {
            let delta = chunk.delta.as_ref().expect("delta");
            assert!(delta.content.is_none());
            let calls = delta.tool_calls.as_ref().expect("tool calls");
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].index, 0);
            assert_eq!(calls[0].id.as_deref(), Some("c1"));
            let func = calls[0].function.as_ref().expect("function");
            assert_eq!(func.name.as_deref(), Some("read_file"));
            assert_eq!(func.arguments.as_deref(), Some("{}"));
        }
        Err(_) => panic!("expected tool chunk"),
    }
    match &chunks[2] {
        Ok(chunk) => {
            assert!(chunk.delta.is_none());
            assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
            let usage = chunk.usage.as_ref().expect("usage");
            assert_eq!(usage.prompt_tokens, 2);
            assert_eq!(usage.completion_tokens, 3);
            assert_eq!(usage.total_tokens, 5);
        }
        Err(_) => panic!("expected completion chunk"),
    }
}

#[tokio::test]
async fn stream_reports_request_failure() {
    let provider = CodexProvider::new("k").with_base_url(BAD_SCHEME_URI);
    let chunks: Vec<_> = provider
        .chat_completion_stream(&[Message::user("hi")], &[], None, None, None, None)
        .collect()
        .await;
    assert_eq!(chunks.len(), 1);
    match &chunks[0] {
        Err(AgentError::LlmApi(msg)) => assert!(msg.contains("HTTP request failed"), "{msg}"),
        Err(_) => panic!("unexpected error variant"),
        Ok(_) => panic!("expected error"),
    }
}
