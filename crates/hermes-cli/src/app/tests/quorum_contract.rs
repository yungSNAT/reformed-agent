fn configure_quorum_fixture_environment(home: &Path) -> EnvSnapshot {
    let env = EnvSnapshot::capture(&[
        "HOME",
        "USERPROFILE",
        "HERMES_HOME",
        "HERMES_MANAGED_DIR",
        "HERMES_AUTH_FILE",
        "NO_PROXY",
        "HERMES_MODEL",
        "HERMES_INFERENCE_MODEL",
        "HERMES_INFERENCE_PROVIDER",
        "HERMES_TUI_PROVIDER",
        "HERMES_QUORUM_AGENT_CONTRACT_PATH",
        "HERMES_QUORUM_VOTER_PASSES",
        "HERMES_QUORUM_VOTER_TOOLS",
        "HERMES_QUORUM_SYNTHESIS_TOOLS",
    ]);
    for name in ["HOME", "USERPROFILE", "HERMES_HOME", "HERMES_MANAGED_DIR"] {
        std::env::set_var(name, home);
    }
    let contract = home.join("fixture-contract.md");
    std::fs::write(&contract, "Report the fixture survey evidence.").unwrap();
    std::env::set_var("HERMES_AUTH_FILE", home.join("absent-auth.json"));
    std::env::set_var("NO_PROXY", "127.0.0.1");
    std::env::set_var("HERMES_QUORUM_AGENT_CONTRACT_PATH", &contract);
    std::env::set_var("HERMES_QUORUM_VOTER_PASSES", "2");
    std::env::set_var("HERMES_QUORUM_VOTER_TOOLS", "false");
    std::env::set_var("HERMES_QUORUM_SYNTHESIS_TOOLS", "false");
    env
}

async fn mock_quorum_surveys(server: &MockServer, fixture_key: &str, synthesis: &str) {
    use wiremock::matchers::{body_partial_json, header};

    let authorization = format!("Bearer {fixture_key}");
    for (model, content) in [
        ("survey-a", "The heron returned at dawn."),
        ("survey-b", "The heron returned after the rain."),
        ("survey-c", "I cannot access the survey tools."),
        ("survey-summary", synthesis),
    ] {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("Authorization", authorization.as_str()))
            .and(body_partial_json(serde_json::json!({"model": model})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": format!("completion-{model}"),
                "object": "chat.completion",
                "created": 0,
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
            })))
            .expect(1..)
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn quorum_synthesises_a_majority_and_preserves_each_voter_outcome() {
    let _lock = test_env_lock::lock_async().await;
    let temp = tempfile::tempdir().unwrap();
    let _env = configure_quorum_fixture_environment(temp.path());
    let server = MockServer::start().await;
    assert!(server.address().ip().is_loopback());
    let fixture_key = format!("fixture-{}", uuid::Uuid::new_v4());
    let synthesis = "The 2 surveys agree that the heron returned.";
    mock_quorum_surveys(&server, &fixture_key, synthesis).await;

    let mut app = build_minimal_test_app_with_state_root(temp.path().to_path_buf());
    let config = Arc::make_mut(&mut app.config);
    config.streaming.enabled = false;
    config.llm_providers.insert(
        "custom".into(),
        LlmProviderConfig {
            base_url: Some(server.uri()),
            api_key: Some(fixture_key),
            request_timeout_seconds: Some(3.0),
            ..Default::default()
        },
    );
    app.try_switch_model("custom:survey-summary").unwrap();
    app.messages = vec![hermes_core::Message::user("Summarise the fixture surveys.")];
    let policy = QuorumPolicy {
        enabled: true,
        voters: 3,
        models: ["custom:survey-a", "custom:survey-b", "custom:survey-c"]
            .map(str::to_owned)
            .to_vec(),
        mode: "balanced".into(),
        updated_at: hermes_core::time::now_rfc3339(),
    };
    let completed = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        app.run_quorum_fanout_turn(Instant::now(), policy),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(completed);
    assert_eq!(app.current_model, "custom:survey-summary");
    assert_eq!(App::extract_last_assistant_output(&app.messages), synthesis);
    assert_quorum_artifact(temp.path(), synthesis);
    assert_quorum_requests(&server).await;
}

fn assert_quorum_artifact(home: &Path, synthesis: &str) {
    let path = std::fs::read_dir(home.join("quorum"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .unwrap();
    let report: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(report["synthesis"], synthesis);
    let voters = report["voters"].as_array().unwrap();
    assert_eq!(voters.len(), 3);
    assert_eq!(voters[0]["status"], "ok");
    assert_eq!(voters[1]["status"], "ok");
    assert_eq!(voters[2]["status"], "degraded");
    assert_eq!(voters[2]["error"], "voter returned degraded non-answer");
}

async fn assert_quorum_requests(server: &MockServer) {
    let requests = server.received_requests().await.unwrap();
    assert!(requests.iter().any(|request| {
        let body = String::from_utf8_lossy(&request.body);
        body.contains("[PRIOR_VOTER_DRAFT]") && body.contains("The heron returned at dawn.")
    }));
    assert!(requests.iter().any(|request| {
        let body = String::from_utf8_lossy(&request.body);
        body.contains("[QUORUM_SYNTHESIS]")
            && body.contains("The heron returned after the rain.")
            && body.contains("Report the fixture survey evidence.")
    }));
}
