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

    /// Two requests of one Anthropic tool loop, as sent on the wire.
    async fn anthropic_tool_loop_requests() -> Vec<serde_json::Value> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-test",
                "content": [{"type": "tool_use", "id": "tu_1", "name": "alpha", "input": {}}],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            })))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "msg_2", "type": "message", "role": "assistant", "model": "claude-test",
                "content": [{"type": "text", "text": "done"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 12, "output_tokens": 2}
            })))
            .with_priority(2)
            .mount(&server)
            .await;

        let mut registry = ToolRegistry::new();
        for name in ["alpha", "zeta"] {
            registry.register(
                name,
                ToolSchema::new(name, name, JsonSchema::new("object")),
                Arc::new(|_args| Ok("tool output".to_string())),
            );
        }
        let mut config = AgentConfig {
            model: "anthropic:claude-test".to_string(),
            skip_memory: true,
            skip_context_files: true,
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let provider = Arc::new(
            crate::provider::AnthropicProvider::new("sk-ant-test")
                .with_model("claude-test")
                .with_base_url(server.uri()),
        );
        let agent = AgentLoop::new(config, Arc::new(registry), provider);
        // Hash order is the order a caller's tool list arrives in.
        let tools = vec![
            ToolSchema::new("zeta", "zeta", JsonSchema::new("object")),
            ToolSchema::new("alpha", "alpha", JsonSchema::new("object")),
        ];
        agent
            .run(vec![Message::user("use alpha")], Some(tools))
            .await
            .expect("tool loop finishes");

        server
            .received_requests()
            .await
            .expect("recorded requests")
            .iter()
            .map(|r| serde_json::from_slice(&r.body).expect("request json"))
            .collect()
    }

    #[tokio::test]
    async fn anthropic_tool_loop_keeps_one_cached_prefix_and_moves_its_breakpoints() {
        let requests = anthropic_tool_loop_requests().await;
        assert_eq!(requests.len(), 2, "{requests:#?}");
        let (first, second) = (&requests[0], &requests[1]);

        for body in [first, second] {
            let names: Vec<&str> = body["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .filter_map(|t| t["name"].as_str())
                .collect();
            assert_eq!(names, ["alpha", "zeta"]);
            let breakpoints = body.to_string().matches("\"cache_control\"").count();
            assert!(breakpoints <= 4, "{breakpoints} breakpoints: {body}");
            let system = body["system"].as_array().expect("system blocks");
            assert_eq!(
                system.last().expect("system block")["cache_control"],
                serde_json::json!({"type": "ephemeral"})
            );
        }
        assert_eq!(first["tools"], second["tools"]);
        assert_eq!(first["system"], second["system"]);

        // The loop's newest message, the tool result, carries a breakpoint.
        let last = second["messages"]
            .as_array()
            .and_then(|m| m.last())
            .expect("last message");
        assert_eq!(last["content"][0]["type"], "tool_result");
        assert_eq!(
            last["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );

        // Apart from where the breakpoints sit, the first request's messages
        // are the start of the second's.
        let mut first_messages = first["messages"].clone();
        let mut second_messages = second["messages"].clone();
        strip_cache_control(&mut first_messages);
        strip_cache_control(&mut second_messages);
        let first_messages = first_messages.as_array().expect("messages").clone();
        let second_messages = second_messages.as_array().expect("messages");
        assert_eq!(&second_messages[..first_messages.len()], &first_messages[..]);
    }
