#[test]
fn terminal_delegate_passes_depth_and_remaining_budget_to_the_handler() {
    let mut registry = ToolRegistry::new();
    registry.register(
        "delegate_task",
        ToolSchema::new(
            "delegate_task",
            "Record delegation arguments",
            JsonSchema::new("object"),
        ),
        Arc::new(|args| Ok(args.to_string())),
    );
    let arguments = serde_json::json!({"task": "Summarise the railway timetable"});
    let delegated = AgentLoop::execute_tool_call_terminal(
        &registry,
        "delegate",
        "delegate_task",
        arguments.clone(),
        2,
        1,
        Some(0.25),
    );
    assert_eq!(delegated.tool_call_id, "delegate");
    assert!(!delegated.is_error);
    assert_eq!(
        serde_json::from_str::<Value>(&delegated.content).unwrap(),
        serde_json::json!({
            "task": "Summarise the railway timetable",
            "child_depth": 2,
            "max_depth": 2,
            "parent_budget_remaining_usd": 0.25,
        })
    );

    let refused = AgentLoop::execute_tool_call_terminal(
        &registry,
        "at-limit",
        "delegate_task",
        arguments,
        2,
        2,
        None,
    );
    assert_eq!(refused.tool_call_id, "at-limit");
    assert!(refused.is_error);
    assert_eq!(refused.content, "Delegation depth limit reached (2/2).");
}

#[test]
fn tool_dispatch_accounts_for_errors_and_keeps_valid_results() {
    let _env = env_test_lock();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut registry = ToolRegistry::new();
    registry.register(
        "fixture_echo",
        ToolSchema::new(
            "fixture_echo",
            "Return the given text",
            JsonSchema::new("object"),
        ),
        Arc::new(|args| Ok(args["text"].as_str().unwrap().to_string())),
    );
    registry.register(
        "terminal",
        ToolSchema::new(
            "terminal",
            "Runs a local command",
            JsonSchema::new("object"),
        ),
        Arc::new(|_| panic!("the ContextLattice command must be redirected before execution")),
    );
    let mut config = AgentConfig {
        isolated: true,
        skip_memory: true,
        skip_context_files: true,
        ..Default::default()
    };
    let home = isolate_route_learning_home(&mut config);
    let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
    let orchestrator = Arc::new(
        crate::sub_agent_orchestrator::SubAgentOrchestrator::from_parent(
            &agent,
            home.path().to_path_buf(),
        ),
    );
    let agent = agent.with_sub_agent_orchestrator(orchestrator);
    let call = |id: &str, name: &str, arguments: &str| ToolCall {
        id: id.into(),
        function: hermes_core::FunctionCall {
            name: name.into(),
            arguments: arguments.into(),
        },
        extra_content: None,
    };
    let rejected = [
        (
            call("malformed", "fixture_echo", "{"),
            "Invalid JSON params for tool 'fixture_echo'",
        ),
        (
            call("missing", "missing_fixture", "{}"),
            "Unknown tool 'missing_fixture'",
        ),
        (
            call("bad-delegate", "delegate_task", "{"),
            "Invalid JSON params for tool 'delegate_task'",
        ),
        (
            call("blank-delegate", "delegate_task", r#"{"task":"  "}"#),
            "delegate_task requires non-empty 'task' string.",
        ),
        (
            call(
                "context",
                "terminal",
                r#"{"command":"contextlattice status"}"#,
            ),
            "ContextLattice integration requests must use",
        ),
    ];
    let mut calls: Vec<_> = rejected.iter().map(|(call, _)| call.clone()).collect();
    calls.push(call(
        "valid",
        "fixture_echo",
        r#"{"text":"Northbound timetable"}"#,
    ));
    for concurrency in [1, calls.len()] {
        let mut errors = Vec::new();
        let results = runtime.block_on(agent.execute_tool_calls(
            &calls,
            3,
            concurrency,
            &[],
            true,
            None,
            &mut errors,
        ));

        assert_eq!(results.len(), calls.len());
        assert_eq!(errors.len(), rejected.len());
        for (call, expected) in &rejected {
            let result = results
                .iter()
                .find(|result| result.tool_call_id == call.id)
                .unwrap();
            assert!(
                result.is_error && result.content.contains(*expected),
                "{result:?}"
            );
            assert!(errors.iter().any(|error| error.turn == 3
                && error.tool_name == call.function.name
                && error.error == result.content));
        }
        let valid = results
            .iter()
            .find(|result| result.tool_call_id == "valid")
            .unwrap();
        assert!(!valid.is_error);
        assert_eq!(valid.content, "Northbound timetable");
    }

    let limit = agent.resolve_max_delegate_depth();
    let agent = agent.with_delegate_depth(limit);
    let mut errors = Vec::new();
    let results = runtime.block_on(agent.execute_tool_calls(
        &[call(
            "depth",
            "delegate_task",
            r#"{"task":"Summarise the timetable"}"#,
        )],
        4,
        1,
        &[],
        false,
        None,
        &mut errors,
    ));
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].tool_call_id, "depth");
    assert!(results[0].is_error);
    assert_eq!(
        results[0].content,
        format!("Delegation depth limit reached ({limit}/{limit}).")
    );
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].tool_name, "delegate_task");
    assert_eq!(errors[0].turn, 4);
    assert_eq!(errors[0].error, results[0].content);
}
