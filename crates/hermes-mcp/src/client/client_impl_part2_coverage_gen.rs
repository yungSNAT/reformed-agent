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
