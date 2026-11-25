#[tokio::test]
async fn quorum_and_claim_commands_persist_policy_without_dispatching_a_turn() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::quorum_and_claim_commands_persist_policy_without_dispatching_a_turn",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    app.messages
        .push(hermes_core::Message::system("Existing system instruction"));
    app.messages.push(hermes_core::Message::user(
        "[QUORUM_MODE] Existing user message",
    ));

    handle_quorum_command(&mut app, &[]).await.unwrap();
    assert!(latest_ui_assistant_text(&app).contains("enabled=false"));
    handle_quorum_command(&mut app, &["run"]).await.unwrap();
    assert!(!app.quorum_armed_once);
    assert!(latest_ui_assistant_text(&app).contains("Quorum mode is OFF"));
    for (args, expected) in [
        (&["voters"][..], "Usage: /quorum voters"),
        (&["models"][..], "Usage: /quorum models"),
        (&["unknown"][..], "Usage: /quorum [status"),
    ] {
        handle_quorum_command(&mut app, args).await.unwrap();
        assert!(latest_ui_assistant_text(&app).contains(expected));
    }

    handle_quorum_command(&mut app, &["on"]).await.unwrap();
    assert!(load_quorum_policy().unwrap().enabled);
    assert_eq!(std::env::var("HERMES_QUORUM_ENABLED").as_deref(), Ok("1"));
    for (value, count) in [("99", 8), ("1", 2), ("invalid", 3)] {
        handle_quorum_command(&mut app, &["voters", value])
            .await
            .unwrap();
        assert_eq!(load_quorum_policy().unwrap().voters, count);
        let hints: Vec<_> = app
            .messages
            .iter()
            .filter_map(|message| {
                let text = message.content.as_deref()?;
                (message.role == hermes_core::MessageRole::System
                    && text.starts_with("[QUORUM_MODE] "))
                .then_some(text)
            })
            .collect();
        assert_eq!(hints.len(), 1);
        assert!(hints[0].contains(&format!("at least {count} independent hypotheses")));
    }
    handle_quorum_command(&mut app, &["run"]).await.unwrap();
    assert!(app.quorum_armed_once);
    assert!(latest_ui_assistant_text(&app).contains("armed for subsequent turns"));
    handle_quorum_command(&mut app, &["off"]).await.unwrap();
    assert!(!app.quorum_armed_once);
    assert!(!load_quorum_policy().unwrap().enabled);
    assert_eq!(std::env::var("HERMES_QUORUM_ENABLED").as_deref(), Ok("0"));
    assert_eq!(app.messages.len(), 2);
    assert_eq!(
        app.messages[0].content.as_deref(),
        Some("Existing system instruction")
    );
    assert_eq!(
        app.messages[1].content.as_deref(),
        Some("[QUORUM_MODE] Existing user message")
    );

    handle_claims_command(&mut app, &["on"]).unwrap();
    assert!(load_claim_verifier_policy().unwrap().enabled);
    handle_claims_command(&mut app, &[]).unwrap();
    assert!(latest_ui_assistant_text(&app).contains("enabled=true"));
    handle_claims_command(&mut app, &["off"]).unwrap();
    assert!(!load_claim_verifier_policy().unwrap().enabled);
    assert_eq!(
        std::env::var("HERMES_CLAIM_VERIFIER_ENABLED").as_deref(),
        Ok("0")
    );
    handle_claims_command(&mut app, &["unknown"]).unwrap();
    assert_eq!(
        latest_ui_assistant_text(&app),
        "Usage: /claims [status|on|off]"
    );
}

#[tokio::test]
async fn quorum_artifact_status_prefers_the_latest_from_the_current_session() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::quorum_artifact_status_prefers_the_latest_from_the_current_session",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    handle_swarm_command(&mut app, &[]).await.unwrap();
    assert!(latest_ui_assistant_text(&app).contains("latest_artifact=(none yet)"));
    handle_swarm_command(&mut app, &["artifact"]).await.unwrap();
    assert!(latest_ui_assistant_text(&app).contains("No swarm/quorum artifact exists"));

    let output = app.state_root.join("quorum");
    std::fs::create_dir_all(&output).unwrap();
    let write_artifact = |name: &str, seconds: u64| {
        let path = output.join(name);
        std::fs::write(&path, b"{}").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds)),
            )
            .unwrap();
        path
    };

    let other = write_artifact("other-session.json", 1_700_000_003);
    handle_swarm_command(&mut app, &["status"]).await.unwrap();
    assert!(
        latest_ui_assistant_text(&app).contains(&format!("latest_artifact={}", other.display()))
    );

    write_artifact(&format!("{}-older.json", app.session_id), 1_700_000_000);
    let latest = write_artifact(&format!("{}-latest.json", app.session_id), 1_700_000_001);
    write_artifact(&format!("{}-notes.txt", app.session_id), 1_700_000_005);
    handle_swarm_command(&mut app, &["status"]).await.unwrap();
    assert!(
        latest_ui_assistant_text(&app).contains(&format!("latest_artifact={}", latest.display()))
    );

    std::fs::write(
        &latest,
        serde_json::json!({
            "session_id": app.session_id,
            "saved_at": "2026-01-02T03:04:05Z",
            "voters": [{}, {}]
        })
        .to_string(),
    )
    .unwrap();
    handle_swarm_command(&mut app, &["artifact"]).await.unwrap();
    let summary = latest_ui_assistant_text(&app);
    assert!(summary.contains(&format!("path={}", latest.display())));
    assert!(summary.contains(&format!("session_id={}", app.session_id)));
    assert!(summary.contains("saved_at=2026-01-02T03:04:05Z\nvoters=2"));

    std::fs::write(&latest, b"incomplete artifact").unwrap();
    handle_swarm_command(&mut app, &["artifact"]).await.unwrap();
    let summary = latest_ui_assistant_text(&app);
    assert!(summary.contains(&format!("path={}", latest.display())));
    assert!(summary.contains("(unable to parse artifact summary)"));
}
