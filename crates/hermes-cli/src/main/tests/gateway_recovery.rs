#[tokio::test]
async fn a_gateway_turn_receives_a_child_result_an_earlier_process_lost() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "c1", "object": "chat.completion", "model": "test-model",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "thanks"}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
        })))
        .mount(&server)
        .await;

    let home = tempfile::tempdir().expect("home");
    let session = "telegram:chat-77";
    // A finished child whose answer no parent turn received: the process
    // stopped first.
    let id = "subagent-lost-77";
    let subagents = home.path().join("subagents");
    let digest = sha2::Sha256::digest(session.as_bytes());
    let bucket: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(subagents.join("pending").join(&bucket)).expect("pending dir");
    std::fs::write(
        subagents.join(format!("{id}.json")),
        serde_json::to_vec(&serde_json::json!({
            "sub_agent_id": id, "parent_session_id": session, "task": "count the files",
            "depth": 1, "max_depth": 2, "status": "completed",
            "started_at": "2026-11-24T00:00:00Z", "ended_at": "2026-11-24T00:01:00Z",
            "result": "child answer 77", "delivered": false
        }))
        .expect("record json"),
    )
    .expect("record");
    std::fs::write(subagents.join("pending").join(&bucket).join(id), b"").expect("marker");

    let mut config = GatewayConfig {
        model: Some("openai:test-model".to_string()),
        ..GatewayConfig::default()
    };
    config.llm_providers.insert(
        "openai".to_string(),
        hermes_config::LlmProviderConfig {
            api_key: Some("sk-test".to_string()),
            base_url: Some(server.uri()),
            ..Default::default()
        },
    );
    let ctx = GatewayRuntimeContext {
        session_key: session.to_string(),
        platform: "telegram".into(),
        chat_id: "chat-77".into(),
        user_id: "77".into(),
        home: Some(home.path().to_string_lossy().into_owned()),
        ..GatewayRuntimeContext::default()
    };
    let agent = build_agent_for_gateway_context(
        &config,
        &ctx,
        Arc::new(hermes_agent::agent_loop::ToolRegistry::new()),
    );
    let delegate = hermes_core::ToolSchema::new(
        "delegate_task",
        "Delegate a task",
        hermes_core::JsonSchema::new("object"),
    );
    agent
        .run(
            vec![hermes_core::Message::user("what did it find?")],
            Some(vec![delegate]),
        )
        .await
        .expect("gateway turn");

    let requests = server.received_requests().await.expect("requests");
    assert!(
        requests
            .iter()
            .any(|r| String::from_utf8_lossy(&r.body).contains("child answer 77")),
        "the turn carries the lost result"
    );
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(subagents.join(format!("{id}.json"))).expect("record after"),
    )
    .expect("record json after");
    assert_eq!(record["delivered"], serde_json::json!(true), "delivered once");
}
