use super::*;

#[tokio::test]
async fn quick_aliases_preserve_arguments_and_report_resolution() {
    let cases = [
        (
            "later",
            " /queue ",
            "review the notes",
            "🧵 Queued follow-up for the active session: review the notes",
        ),
        (
            "guide",
            "/steer",
            "use the draft",
            "🧭 Steering instruction accepted: use the draft",
        ),
        (
            "modelpick",
            "/model",
            "fixture-model",
            "🔀 Model switch alias parsed: fixture-model",
        ),
        (
            "label",
            "/title",
            "Review notes",
            "🏷 Session title set to: Review notes",
        ),
        (
            "showlabel",
            "/title",
            "",
            "Quick command `showlabel` routed to `/title`.",
        ),
        (
            "empty",
            " ",
            "",
            "Quick command `empty` has no target defined.",
        ),
    ];
    let mut config = GatewayConfig::default();
    for (name, target, _, _) in cases {
        config.quick_commands.insert(
            name.to_owned(),
            QuickCommandConfig {
                kind: "alias".to_owned(),
                target: Some(target.to_owned()),
                ..Default::default()
            },
        );
    }
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    let gateway = Gateway::with_defaults(sessions, config);

    for (name, _, arguments, expected) in cases {
        let input = format!("/{name} {arguments}");
        let reply = gateway
            .resolve_quick_command(input.trim_end())
            .await
            .unwrap();
        assert_eq!(reply.as_deref(), Some(expected), "{input}");
    }
}

#[tokio::test]
async fn smart_routing_keeps_an_explicit_session_model_and_uses_defaults_for_new_sessions() {
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    let gateway = Gateway::new(
        sessions.clone(),
        DmManager::with_ignore_behavior(),
        GatewayConfig {
            model: Some("fixture:configured-model".into()),
            ..GatewayConfig::default()
        },
    );
    assert!(Arc::ptr_eq(gateway.session_manager(), &sessions));
    assert_eq!(
        gateway.config().model.as_deref(),
        Some("fixture:configured-model")
    );
    assert_eq!(
        gateway.load_smart_model_routing("quick summary").as_deref(),
        Some("fixture:configured-model")
    );
    assert!(gateway.load_smart_model_routing("hello").is_none());

    let session = sessions.compose_session_key("test", "second-chat", "user1");
    gateway
        .maybe_apply_smart_model_routing(&session, "hello")
        .await;
    assert!(gateway.runtime_state.read().await.get(&session).is_none());
    *gateway.default_model.write().await = None;
    gateway
        .maybe_apply_smart_model_routing(&session, "analyze the fixture")
        .await;
    assert_eq!(
        gateway.runtime_state.read().await[&session]
            .model
            .as_deref(),
        Some("fixture:configured-model")
    );

    gateway
        .merge_request_runtime_overrides(
            "test",
            "second-chat",
            "user1",
            Some("fixture:explicit-model".into()),
            None,
            None,
        )
        .await;
    *gateway.default_model.write().await = Some("fixture:new-default".into());
    gateway
        .maybe_apply_smart_model_routing(&session, "refactor the fixture")
        .await;
    assert_eq!(
        gateway.runtime_state.read().await[&session]
            .model
            .as_deref(),
        Some("fixture:explicit-model")
    );
    let new_session = sessions.compose_session_key("test", "third-chat", "user1");
    gateway
        .maybe_apply_smart_model_routing(&new_session, "refactor the fixture")
        .await;
    assert_eq!(
        gateway.runtime_state.read().await[&new_session]
            .model
            .as_deref(),
        Some("fixture:new-default")
    );
}

#[tokio::test]
async fn gateway_prompt_and_media_hints_avoid_inserting_empty_context() {
    let sessions = Arc::new(SessionManager::new(SessionConfig::default()));
    let gateway = Gateway::new(
        sessions,
        DmManager::with_ignore_behavior(),
        GatewayConfig::default(),
    );
    let root = tempfile::tempdir().unwrap();
    let prompt = root.path().join("ephemeral.txt");
    assert!(gateway.load_ephemeral_system_prompt(&prompt).is_none());
    std::fs::write(&prompt, "  \n").unwrap();
    assert!(gateway.load_ephemeral_system_prompt(&prompt).is_none());
    std::fs::write(&prompt, "  Fixture prompt\n").unwrap();
    assert_eq!(
        gateway.load_ephemeral_system_prompt(&prompt).as_deref(),
        Some("Fixture prompt")
    );
    assert_eq!(
        gateway.enrich_message_with_vision("plain text"),
        "plain text"
    );
    assert_eq!(
        gateway.enrich_message_with_vision("See https://example.invalid/chart.png"),
        "[vision_candidate]\nSee https://example.invalid/chart.png"
    );
    assert_eq!(
        gateway.enrich_message_with_transcription("plain text"),
        "plain text"
    );
    assert_eq!(
        gateway.enrich_message_with_transcription("See recording.m4a"),
        "[transcription_candidate]\nSee recording.m4a"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn process_watcher_delivers_lines_and_surfaces_absent_or_invalid_stdout() {
    use std::process::Stdio;

    let gateway = Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        DmManager::with_ignore_behavior(),
        GatewayConfig::default(),
    );
    let lines = Arc::new(Mutex::new(Vec::new()));
    let captured = lines.clone();
    let on_output: Arc<dyn Fn(String) + Send + Sync> =
        Arc::new(move |line| captured.lock().unwrap().push(line));

    for (script, stdout) in [
        (r"printf 'first\nsecond'", Stdio::piped()),
        ("exit 0", Stdio::null()),
        (r"printf '\377\n'", Stdio::piped()),
    ] {
        let child = tokio::process::Command::new("/bin/sh")
            .env_clear()
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            gateway.run_process_watcher(child, on_output.clone()),
        )
        .await
        .expect("fixture process watcher timed out");
        match script {
            "exit 0" => assert!(matches!(
                result,
                Err(GatewayError::Platform(reason)) if reason == "Process has no stdout"
            )),
            r"printf '\377\n'" => assert!(matches!(
                result,
                Err(GatewayError::Platform(reason)) if reason.starts_with("Watcher read error")
            )),
            _ => result.unwrap(),
        }
    }
    assert_eq!(*lines.lock().unwrap(), ["first", "second"]);
}
