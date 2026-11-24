fn strip_cache_control(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("cache_control");
            map.values_mut().for_each(strip_cache_control);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_cache_control),
        _ => {}
    }
}

fn anthropic_tool_loop_reply(index: usize, streaming: bool) -> wiremock::ResponseTemplate {
    let tool_id = format!("tu_{index}");
    let finish = if index < 2 { "tool_use" } else { "end_turn" };
    let tool = serde_json::json!({"type": "tool_use", "id": tool_id, "name": "alpha", "input": {}});
    if !streaming {
        let content = if index < 2 {
            tool
        } else {
            serde_json::json!({"type": "text", "text": "done"})
        };
        return wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": format!("msg_{index}"), "type": "message", "role": "assistant", "model": "claude-test",
                "content": [content], "stop_reason": finish,
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }));
    }
    let event = |kind: &str, value: serde_json::Value| format!("event: {kind}\ndata: {value}\n\n");
    let content = if index < 2 {
        event(
            "content_block_start",
            serde_json::json!({"index": 0, "content_block": tool}),
        ) + &event(
            "content_block_delta",
            serde_json::json!({"index": 0, "delta": {
                "type": "input_json_delta", "partial_json": "{}"
            }}),
        )
    } else {
        event(
            "content_block_delta",
            serde_json::json!({"index": 0, "delta": {
                "type": "text_delta", "text": "done"
            }}),
        )
    };
    let finish = event(
        "message_delta",
        serde_json::json!({
            "delta": {"stop_reason": finish}, "usage": {"output_tokens": 5}
        }),
    ) + &event("message_stop", serde_json::json!({}));
    wiremock::ResponseTemplate::new(200).set_body_raw(content + &finish, "text/event-stream")
}

async fn anthropic_tool_loop_requests(streaming: bool) -> Vec<serde_json::Value> {
    anthropic_tool_loop_requests_with(streaming, hermes_config::PromptCacheTtl::FiveMinutes).await
}

async fn anthropic_tool_loop_requests_with(
    streaming: bool,
    cache_ttl: hermes_config::PromptCacheTtl,
) -> Vec<serde_json::Value> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request};

    let server = MockServer::start().await;
    let counter = std::sync::atomic::AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(move |_: &Request| {
            let index = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            anthropic_tool_loop_reply(index, streaming)
        })
        .expect(3)
        .mount(&server)
        .await;

    let schema = |name| ToolSchema::new(name, name, JsonSchema::new("object"));
    let mut registry = ToolRegistry::new();
    for name in ["zeta", "alpha"] {
        registry.register(
            name,
            schema(name),
            Arc::new(|_args| Ok("tool output".to_string())),
        );
    }
    let mut config = AgentConfig {
        model: "anthropic:claude-test".to_string(),
        api_mode: ApiMode::AnthropicMessages,
        skip_memory: true,
        skip_context_files: true,
        ephemeral_system_prompt: Some("later ephemeral instruction".to_string()),
        prompt_caching: hermes_config::PromptCachingConfig {
            cache_ttl,
            ..Default::default()
        },
        ..AgentConfig::default()
    };
    let _home = isolate_route_learning_home(&mut config);
    let provider = Arc::new(
        crate::provider::AnthropicProvider::new("sk-ant-test")
            .with_model("claude-test")
            .with_base_url(server.uri()),
    );
    let agent = AgentLoop::new(config, Arc::new(registry), provider);
    let mut user = Message::user("use alpha twice");
    user.cache_control = Some(hermes_core::types::CacheControl {
        cache_type: hermes_core::types::CacheType::Ephemeral,
    });
    let messages = vec![
        Message::system("Stable instructions."),
        user,
        Message::system("<memory-context>synthetic recall</memory-context>"),
    ];
    let result = if streaming {
        agent
            .run_stream(messages, None, Some(Box::new(|_| {})))
            .await
    } else {
        agent
            .run(messages, Some(vec![schema("zeta"), schema("alpha")]))
            .await
    }
    .expect("tool loop finishes");
    assert!(result.finished_naturally);
    server
        .received_requests()
        .await
        .expect("recorded requests")
        .iter()
        .map(|request| serde_json::from_slice(&request.body).expect("request json"))
        .collect()
}

#[tokio::test]
async fn anthropic_tool_loop_keeps_one_cached_prefix_and_moves_its_breakpoints() {
    for streaming in [false, true] {
        let requests = anthropic_tool_loop_requests(streaming).await;
        assert_eq!(requests.len(), 3, "{requests:#?}");
        let first = &requests[0];
        for body in &requests {
            let names: Vec<&str> = body["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect();
            assert_eq!(
                names,
                if streaming {
                    ["alpha", "zeta"]
                } else {
                    ["zeta", "alpha"]
                }
            );
            let breakpoints = body.to_string().matches("\"cache_control\"").count();
            assert!(breakpoints <= 4, "{breakpoints} breakpoints: {body}");
            let system = body["system"].as_array().expect("system blocks");
            let stable = system
                .iter()
                .find(|block| block["text"] == "Stable instructions.")
                .expect("leading system instructions");
            assert_eq!(
                stable["cache_control"],
                serde_json::json!({"type": "ephemeral"})
            );
            let recall = system
                .iter()
                .find(|block| block["text"] == "<memory-context>synthetic recall</memory-context>")
                .expect("later recall keeps system authority");
            assert!(recall.get("cache_control").is_none());
            assert!(
                system
                    .iter()
                    .any(|block| block["text"] == "later ephemeral instruction")
            );
            assert_eq!(body["tools"], first["tools"]);
            assert_eq!(body["system"], first["system"]);
            assert_eq!(
                body["messages"][0]["content"][0]["cache_control"],
                serde_json::json!({"type": "ephemeral"})
            );
        }
        for body in &requests[1..] {
            let last = body["messages"]
                .as_array()
                .and_then(|items| items.last())
                .expect("last message");
            assert_eq!(last["content"][0]["type"], "tool_result");
            assert_eq!(
                last["content"][0]["cache_control"],
                serde_json::json!({"type": "ephemeral"})
            );
        }
        for pair in requests.windows(2) {
            let mut older = pair[0]["messages"].clone();
            let mut newer = pair[1]["messages"].clone();
            strip_cache_control(&mut older);
            strip_cache_control(&mut newer);
            let older = older.as_array().expect("messages");
            let newer = newer.as_array().expect("messages");
            assert_eq!(&newer[..older.len()], older);
        }
    }
}

/// Every `cache_control` object in `value`.
fn cache_markers(value: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(marker) = map.get("cache_control") {
                out.push(marker.clone());
            }
            map.values().for_each(|v| cache_markers(v, out));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| cache_markers(v, out)),
        _ => {}
    }
}

#[tokio::test]
async fn one_hour_tier_puts_its_ttl_on_every_breakpoint_and_never_sends_the_setting() {
    for streaming in [false, true] {
        let hour = anthropic_tool_loop_requests_with(
            streaming,
            hermes_config::PromptCacheTtl::OneHour,
        )
        .await;
        let five = anthropic_tool_loop_requests(streaming).await;
        assert_eq!(hour.len(), 3);
        for (body, default_body) in hour.iter().zip(&five) {
            assert!(body.get("prompt_cache_ttl").is_none(), "{body}");
            let mut markers = Vec::new();
            cache_markers(body, &mut markers);
            let mut default_markers = Vec::new();
            cache_markers(default_body, &mut default_markers);
            assert_eq!(markers.len(), default_markers.len(), "same breakpoints");
            assert!(!markers.is_empty());
            for marker in &markers {
                assert_eq!(marker, &serde_json::json!({"type": "ephemeral", "ttl": "1h"}));
            }
            for marker in &default_markers {
                assert_eq!(marker, &serde_json::json!({"type": "ephemeral"}));
            }
            let (mut hour_bare, mut five_bare) = (body.clone(), default_body.clone());
            strip_cache_control(&mut hour_bare);
            strip_cache_control(&mut five_bare);
            assert_eq!(hour_bare, five_bare, "only the markers differ");
        }
    }
}
