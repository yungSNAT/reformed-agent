#[test]
fn test_parse_openai_response_basic() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "Hello!"
            },
            "finish_reason": "stop"
        }],
        "model": "gpt-4o",
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    });
    let resp = parse_openai_response(&json).unwrap();
    assert_eq!(resp.message.content.as_deref(), Some("Hello!"));
    assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    assert_eq!(resp.usage.as_ref().unwrap().total_tokens, 15);
}

#[test]
fn test_parse_openai_response_null_content_is_safe() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_null_content",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\":\"Cargo.toml\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "reasoning-tool-only"
    });

    let resp = parse_openai_response(&json).expect("null content response should parse");

    assert_eq!(resp.message.content.as_deref(), Some(""));
    let calls = resp.message.tool_calls.as_ref().expect("tool calls");
    assert_eq!(calls[0].id, "call_null_content");
    assert_eq!(calls[0].function.name, "read_file");
}

#[test]
fn test_parse_openai_response_no_choices_includes_provider_context() {
    let json = serde_json::json!({
        "status": 400,
        "message": "This request is not valid. Check the model name and other parameters. Additional info: Provider returned error",
    });
    let err = parse_openai_response(&json).unwrap_err().to_string();
    assert!(err.contains("No choices in response"));
    assert!(err.contains("status=400"));
    assert!(err.contains("Provider returned error"));
}

#[test]
fn test_parse_openai_response_empty_choices_includes_error_context() {
    let json = serde_json::json!({
        "choices": [],
        "error": {"message": "Check that you're sending a valid payload."},
    });
    let err = parse_openai_response(&json).unwrap_err().to_string();
    assert!(err.contains("Empty choices array"));
    assert!(err.contains("valid payload"));
}

#[test]
fn test_parse_openai_response_with_tool_calls() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\": \"test.txt\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "gpt-4o"
    });
    let resp = parse_openai_response(&json).unwrap();
    let tc = resp.message.tool_calls.as_ref().unwrap();
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].function.name, "read_file");
}

#[test]
fn test_parse_openai_response_accepts_object_valued_tool_arguments() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_dict_args",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": {"path": "README.md"}
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "local-openai-compatible"
    });

    let resp = parse_openai_response(&json).expect("object arguments should parse");
    let tc = resp.message.tool_calls.as_ref().unwrap();
    let args: Value = serde_json::from_str(&tc[0].function.arguments).unwrap();
    assert_eq!(args["path"], "README.md");
}

#[test]
fn test_parse_openai_response_with_tool_call_extra_content() {
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\": \"test.txt\"}"
                    },
                    "extra_content": {
                        "google": {
                            "thought_signature": "SIG_ABC123"
                        }
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "gemini-2.5-pro"
    });
    let resp = parse_openai_response(&json).unwrap();
    let tc = resp.message.tool_calls.as_ref().unwrap();
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].function.name, "read_file");
    assert_eq!(
        tc[0].extra_content,
        Some(serde_json::json!({
            "google": {
                "thought_signature": "SIG_ABC123"
            }
        }))
    );
}

#[test]
fn test_parse_sse_chunk_content() {
    let json = serde_json::json!({
        "choices": [{
            "delta": {
                "content": "Hello"
            },
            "finish_reason": null
        }]
    });
    let chunk = parse_sse_chunk(&json).unwrap();
    assert_eq!(
        chunk.delta.as_ref().unwrap().content.as_deref(),
        Some("Hello")
    );
    assert!(chunk.finish_reason.is_none());
}

#[test]
fn test_parse_sse_chunk_tool_call() {
    let json = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_abc",
                    "function": {
                        "name": "search",
                        "arguments": ""
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    let chunk = parse_sse_chunk(&json).unwrap();
    let tc = chunk.delta.as_ref().unwrap().tool_calls.as_ref().unwrap();
    assert_eq!(tc[0].index, 0);
    assert_eq!(tc[0].id.as_deref(), Some("call_abc"));
}

#[test]
fn test_parse_sse_chunk_keeps_the_usage_chunk_without_choices() {
    // OpenAI's last chunk when stream_options.include_usage is set.
    let json = serde_json::json!({
        "choices": [],
        "usage": {"prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15}
    });
    let chunk = parse_sse_chunk(&json).expect("usage chunk");
    assert!(chunk.delta.is_none());
    assert_eq!(chunk.usage.as_ref().unwrap().total_tokens, 15);
    assert!(parse_sse_chunk(&serde_json::json!({"choices": []})).is_none());
}

#[test]
fn test_parse_sse_chunk_finish() {
    let json = serde_json::json!({
        "choices": [{
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150
        }
    });
    let chunk = parse_sse_chunk(&json).unwrap();
    assert_eq!(chunk.finish_reason.as_deref(), Some("stop"));
    assert_eq!(chunk.usage.as_ref().unwrap().total_tokens, 150);
}

#[tokio::test]
async fn loopback_provider_bounds_and_validates_completion_responses() {
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let success = ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"role": "assistant", "content": "Fixture response"}, "finish_reason": "stop"}]
    }));
    let cases = [
        ("normal", success),
        ("large", ResponseTemplate::new(200).set_body_string("x".repeat(1024))),
        ("invalid", ResponseTemplate::new(200).set_body_string("not JSON")),
        ("unavailable", ResponseTemplate::new(503).set_body_string("fixture diagnostic")),
    ];
    for (model, response) in cases {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({"model": model})))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
    }
    let provider = GenericProvider::new(server.uri(), "fixture-only", "normal")
        .with_direct_endpoint()
        .unwrap()
        .with_completion_response_limit(256);
    let messages = [Message::user("Summarise the fixture")];
    let success = provider
        .chat_completion(&messages, &[], None, None, None, None)
        .await
        .unwrap();
    assert_eq!(success.message.content.as_deref(), Some("Fixture response"));
    for (model, expected) in [
        ("large", "Completion response exceeded its size limit."),
        ("invalid", "Completion response was not valid JSON."),
        ("unavailable", "API error 503 Service Unavailable"),
    ] {
        let error = provider
            .chat_completion(&messages, &[], None, None, Some(model), None)
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::LlmApi(message) if message == expected));
    }
}

#[tokio::test]
async fn stale_provider_probes_the_transport_once_before_completing_requests() {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(405))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "Fixture answer"}, "finish_reason": "stop"}]
        })))
        .expect(2)
        .mount(&server)
        .await;

    let provider = GenericProvider::new(server.uri(), "fixture-only", "fixture-model")
        .with_direct_endpoint()
        .unwrap()
        .with_request_timeout_seconds(5.0);
    *provider.client_refreshed_at.lock().unwrap() = Instant::now() - Duration::from_secs(301);

    for _ in 0..2 {
        let response = provider
            .chat_completion(
                &[Message::user("Summarise the fixture")],
                &[],
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.message.content.as_deref(), Some("Fixture answer"));
    }
    let requests = server.received_requests().await.unwrap();
    let methods: Vec<_> = requests
        .iter()
        .map(|request| request.method.as_str())
        .collect();
    assert_eq!(methods, ["GET", "POST", "POST"]);
}

#[tokio::test]
async fn loopback_provider_limits_chunked_completion_without_content_length() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(socket.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some((_, value)) = line
                .split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            {
                content_length = value.trim().parse::<usize>().unwrap();
            }
        }
        let mut request_body = [0; 1024];
        socket.read_exact(&mut request_body[..content_length]).await.unwrap();
        let part = "x".repeat(160);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\na0\r\n{part}\r\na0\r\n{part}\r\n0\r\n\r\n"
        );
        socket.get_mut().write_all(response.as_bytes()).await.unwrap();
    });
    let provider = GenericProvider::new(format!("http://{address}"), "fixture-only", "fixture")
        .with_optional_request_timeout_seconds(Some(10.0))
        .with_direct_endpoint()
        .unwrap()
        .with_completion_response_limit(256);
    let error = provider
        .chat_completion(&[Message::user("Summarise the fixture")], &[], None, None, None, None)
        .await
        .unwrap_err();
    assert!(matches!(error, AgentError::LlmApi(message) if message == "Completion response exceeded its size limit."));
    server.await.unwrap();
}
