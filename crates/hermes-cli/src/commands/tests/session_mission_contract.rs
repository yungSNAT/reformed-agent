use super::*;

#[tokio::test]
async fn session_commands_restore_saved_messages_and_diagnose_invalid_snapshots() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::session_mission_contract::session_commands_restore_saved_messages_and_diagnose_invalid_snapshots",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let cli = crate::cli::Cli::try_parse_from(["hermes", "--ignore-user-config", "--ignore-rules"])
        .expect("parse isolated arguments");
    let mut app = App::new(cli).await.expect("build isolated app");
    let (tx, _rx) = mpsc::unbounded_channel::<crate::tui::Event>();
    app.set_stream_handle(Some(tx.into()));
    assert_eq!(app.state_root, home);
    let created_on_start = home
        .join("sessions")
        .join(format!("{}.json", app.session_id));
    if created_on_start.exists() {
        std::fs::remove_file(created_on_start).expect("remove the isolated empty startup snapshot");
    }
    for command in ["/load", "/resume", "/sessions"] {
        let output = command_output(&mut app, command, &[]).await;
        assert!(output.contains("No saved sessions found"), "{output}");
    }

    let original_id = app.session_id.clone();
    app.current_personality = Some("Fixture persona".into());
    app.messages = vec![hermes_core::Message::user("saved question")];
    let saved = command_output(&mut app, "/save", &["fixture-backup"]).await;
    let sessions = home.join("sessions");
    let named = sessions.join("fixture-backup.json");
    assert!(saved.contains(&named.display().to_string()), "{saved}");
    let canonical_name = format!("{original_id}.json");
    std::fs::copy(&named, sessions.join(&canonical_name)).expect("canonical snapshot");
    std::fs::write(sessions.join("fixture-invalid.json"), "{").expect("malformed snapshot");
    std::fs::write(sessions.join("fixture-no-messages.json"), "{}").expect("incomplete snapshot");

    for (command, args, expected) in [
        ("/load", &[][..], "Saved sessions:"),
        ("/load", &["missing"][..], "Session 'missing' not found"),
        (
            "/load",
            &["fixture-no-messages"][..],
            "Session file has no messages array",
        ),
        ("/resume", &["missing"][..], "Session 'missing' not found"),
        (
            "/resume",
            &["fixture-"][..],
            "Session name 'fixture-' is ambiguous.",
        ),
        (
            "/resume",
            &["fixture-invalid"][..],
            "is present but invalid: json_invalid",
        ),
    ] {
        let output = command_output(&mut app, command, args).await;
        assert!(output.contains(expected), "{command} {args:?}: {output}");
    }

    let doctor = command_output(&mut app, "/sessions", &["doctor"]).await;
    for expected in [
        "invalid_snapshots=2",
        "split_session_ids=1",
        "fixture-invalid: json_invalid",
        "fixture-no-messages: missing_messages_array",
        "fixture-backup",
        original_id.as_str(),
    ] {
        assert!(doctor.contains(expected), "{doctor}");
    }

    app.session_id = "unrelated-active-session".into();
    app.current_personality = None;
    app.messages.clear();
    let loaded = command_output(&mut app, "/load", &["fixture-backup"]).await;
    assert!(
        loaded.contains("Loaded session 'fixture-backup'"),
        "{loaded}"
    );
    assert_eq!(app.session_id, "unrelated-active-session");
    assert_eq!(app.current_personality.as_deref(), Some("Fixture persona"));
    assert_eq!(app.messages[0].content.as_deref(), Some("saved question"));

    app.current_personality = None;
    app.messages.clear();

    let resumed = command_output(&mut app, "/resume", &[]).await;
    assert!(
        resumed.contains(&format!("Resumed session '{original_id}'")),
        "{resumed}"
    );
    assert_eq!(app.session_id, original_id);
    assert_eq!(app.current_personality.as_deref(), Some("Fixture persona"));
    assert_eq!(
        app.messages,
        vec![hermes_core::Message::user("saved question")]
    );
    assert_eq!(
        app.agent.config.session_id.as_deref(),
        Some(original_id.as_str())
    );
}

#[tokio::test]
async fn mission_commands_recover_and_deduplicate_the_isolated_queue() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::session_mission_contract::mission_commands_recover_and_deduplicate_the_isolated_queue",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let initialized = command_output(&mut app, "/mission", &["init"]).await;
    assert!(
        initialized.contains("Mission runtime initialized."),
        "{initialized}"
    );
    let usage = command_output(&mut app, "/mission", &["enqueue", "fixture-loop"]).await;
    assert!(usage.contains("Usage: /mission enqueue"), "{usage}");

    for _ in 0..2 {
        let queued = command_output(
            &mut app,
            "/mission",
            &["enqueue", "fixture-loop", "fixture-event", "local", "notes"],
        )
        .await;
        assert!(
            queued.contains("loop=fixture-loop type=fixture-event status=queued"),
            "{queued}"
        );
    }

    let queue = crate::alpha_runtime::alpha_state_dir().join("loop_queue.jsonl");
    let read_events = || {
        std::fs::read_to_string(&queue)
            .expect("read isolated event queue")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("queue event"))
            .collect::<Vec<_>>()
    };
    let mut events = read_events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["payload"], "local notes");
    assert_eq!(events[0]["fingerprint"], events[1]["fingerprint"]);
    events[0]["created_at"] = serde_json::json!("2000-01-01T00:00:00Z");
    events[0]["status"] = serde_json::json!("running");
    std::fs::write(&queue, format!("{}\n{}\n", events[0], events[1]))
        .expect("queue after interruption");

    let recovered = command_output(&mut app, "/mission", &["recover"]).await;
    assert!(
        recovered.contains("Marked 1 orphaned running event(s)."),
        "{recovered}"
    );
    assert_eq!(read_events()[0]["status"], "orphaned");
    let replayed = command_output(&mut app, "/mission", &["replay", "10"]).await;
    assert!(
        replayed.contains("Replayed 1 event(s) (limit=10)"),
        "{replayed}"
    );
    let events = read_events();
    assert_eq!(events[0]["status"], "replayed");
    assert_eq!(events[1]["status"], "deduped");
    let repeated = command_output(&mut app, "/mission", &["replay", "10"]).await;
    assert!(repeated.contains("Replayed 0 event(s)"), "{repeated}");
}
