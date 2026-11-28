use super::*;

#[tokio::test]
async fn non_tui_ask_displays_options_and_picks_the_recommended_default() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::session_steering_contract::non_tui_ask_displays_options_and_picks_the_recommended_default",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    let usage = command_output(&mut app, "/ask", &[]).await;
    assert!(usage.contains("Usage: `/ask <question>"));
    let incomplete = command_output(&mut app, "/ask", &["Which note? | "]).await;
    assert!(incomplete.contains("For non-TUI usage provide options"));

    let request = ["Which note?", "|", "Summary", "|", "Details (recommended)"];
    let output = command_output(&mut app, "/ask", &request).await;
    assert!(output.contains("Q: Which note?"));
    assert!(output.contains("1. Summary"));
    assert!(output.contains("2. Details (recommended)"));
    assert!(output.contains("Selected: Details (recommended)"));

    let fallback = command_output(&mut app, "/ask", &["Which note? | Summary | Details"]).await;
    assert!(fallback.contains("Selected: Summary"));
}

#[tokio::test]
async fn session_handoff_queues_only_enabled_destinations_and_resolves_the_home_channel() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::session_steering_contract::session_handoff_queues_only_enabled_destinations_and_resolves_the_home_channel",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let platforms = &mut Arc::make_mut(&mut app.config).platforms;
    platforms.clear();
    platforms.insert("discord".into(), hermes_config::PlatformConfig::default());
    platforms.insert(
        "telegram".into(),
        hermes_config::PlatformConfig {
            enabled: true,
            ..Default::default()
        },
    );
    let queue = home.join("handoff_requests");
    for (args, expected) in [
        (&[][..], "Configured platforms: discord, telegram"),
        (&["discord"], "configured but disabled"),
        (&["telegram"], "No home channel marker"),
    ] {
        let output = command_output(&mut app, "/handoff", args).await;
        assert!(output.contains(expected), "{output}");
        assert!(!queue.exists());
    }

    let marker = home.join("home-session.json");
    let status = command_output(&mut app, "/sethome", &[]).await;
    assert!(status.contains("No home marker set."), "{status}");
    assert!(!marker.exists());
    let cleared = command_output(&mut app, "/sethome", &["clear"]).await;
    assert_eq!(cleared, "Home marker already clear.");

    command_output(&mut app, "/sethome", &[" fixture-room "]).await;
    let status = command_output(&mut app, "/sethome", &["status"]).await;
    let (heading, payload) = status.split_once('\n').expect("home marker status");
    assert_eq!(heading, format!("Home marker file: {}", marker.display()));
    let saved: serde_json::Value = serde_json::from_str(payload).expect("home marker JSON");
    assert_eq!(saved["session_id"], app.session_id);
    assert_eq!(saved["home"], "fixture-room");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&marker).unwrap()).unwrap(),
        saved
    );

    let path = queue.join(format!("{}-telegram.json", app.session_id));
    let queued = command_output(&mut app, "/handoff", &[" TELEGRAM "]).await;
    assert!(queued.contains("Queued handoff request."));
    assert!(queued.contains("home_channel: fixture-room"));
    let request: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(request["session_id"], app.session_id);
    assert_eq!(request["platform"], "telegram");
    assert_eq!(request["home_channel"], "fixture-room");
    assert_eq!(request["requested_by"], "cli");
    assert_eq!(request["state"], "pending");

    Arc::make_mut(&mut app.config)
        .platforms
        .get_mut("telegram")
        .unwrap()
        .home_channel = Some("configured-room".into());
    let queued = command_output(&mut app, "/handoff", &["telegram"]).await;
    assert!(queued.contains("home_channel: configured-room"));
    let request: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(request["home_channel"], "configured-room");
    assert_eq!(std::fs::read_dir(queue).unwrap().count(), 1);
    let cleared = command_output(&mut app, "/sethome", &["clear"]).await;
    assert_eq!(cleared, "Cleared home marker.");
    assert!(!marker.exists());
}

#[tokio::test]
async fn subgoal_commands_persist_state_and_leave_it_unchanged_after_invalid_edits() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::session_steering_contract::subgoal_commands_persist_state_and_leave_it_unchanged_after_invalid_edits",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    app.session_objective = Some("Check the local fixture".into());
    let path = home
        .join("subgoals")
        .join(format!("{}.json", app.session_id));
    for (args, expected) in [
        (&["list"][..], "items: (none)"),
        (&["inspect", "fixture"], "[ ] 1. inspect fixture (pending)"),
        (&["verify", "fixture"], "[ ] 2. verify fixture (pending)"),
        (&["done", "1"], "[x] 1. inspect fixture (completed)"),
        (&["impossible", "2"], "[!] 2. verify fixture (impossible)"),
        (&["undo", "1"], "[ ] 1. inspect fixture (pending)"),
    ] {
        let display = command_output(&mut app, "/subgoal", args).await;
        assert!(display.contains("objective: Check the local fixture"));
        assert!(display.contains(expected), "{display}");
    }

    let written = std::fs::read(&path).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&written).unwrap();
    assert_eq!(saved["session_id"], app.session_id);
    assert_eq!(saved["items"][0]["source"], "user");
    assert_eq!(saved["items"][0]["status"], "pending");
    assert_eq!(saved["items"][1]["status"], "impossible");
    for (args, expected) in [
        (&["remove"][..], "Usage: /subgoal remove <n>"),
        (&["done", "first"], "must be an integer"),
        (&["undo", "0"], "out of range"),
        (&["impossible", "3"], "out of range"),
    ] {
        let display = command_output(&mut app, "/subgoal", args).await;
        assert!(display.contains(expected), "{display}");
        assert_eq!(std::fs::read(&path).unwrap(), written);
    }

    let removed = command_output(&mut app, "/subgoal", &["remove", "1"]).await;
    assert!(removed.contains("Removed subgoal 1: inspect fixture"));
    assert!(removed.contains("[!] 1. verify fixture (impossible)"));
    command_output(&mut app, "/subgoal", &["clear"]).await;
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(saved["items"], serde_json::json!([]));
}

#[tokio::test]
async fn session_checkpoints_compare_merge_and_rewind_local_messages() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::session_steering_contract::session_checkpoints_compare_merge_and_rewind_local_messages",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let cli = crate::cli::Cli::try_parse_from(["hermes", "--ignore-user-config", "--ignore-rules"])
        .expect("parse isolated arguments");
    let mut app = App::new(cli).await.expect("build isolated app");
    let (tx, _rx) = mpsc::unbounded_channel::<crate::tui::Event>();
    app.set_stream_handle(Some(tx.into()));
    let sessions = home.join("sessions");
    let created_on_start = sessions.join(format!("{}.json", app.session_id));
    if created_on_start.exists() {
        std::fs::remove_file(created_on_start).expect("remove the isolated empty startup snapshot");
    }
    if sessions.exists() {
        std::fs::remove_dir(&sessions).expect("remove the empty isolated snapshots directory");
    }
    let missing = command_output(&mut app, "/snapshot", &[]).await;
    assert!(missing.contains("No snapshots found"), "{missing}");
    std::fs::create_dir(&sessions).expect("create empty snapshots directory");
    assert_eq!(
        command_output(&mut app, "/snapshot", &["list"]).await,
        missing
    );

    for (command, args, expected) in [
        ("/branch", &["list"][..], "No snapshots found"),
        ("/branch", &["help"][..], "Usage: /branch"),
        ("/branch", &["diff"][..], "Usage: /branch diff"),
        ("/branch", &["merge"][..], "Usage: /branch merge"),
        (
            "/rollback",
            &["latest"][..],
            "No snapshots available to rollback",
        ),
        ("/rollback", &["load"][..], "Usage: /rollback load"),
        ("/timetravel", &["help"][..], "jump to named snapshot"),
        ("/timetravel", &["goto"][..], "Usage: /timetravel goto"),
        (
            "/timetravel",
            &["unknown"][..],
            "Unknown /timetravel action",
        ),
    ] {
        let output = command_output(&mut app, command, args).await;
        assert!(output.contains(expected), "{command} {args:?}: {output}");
    }
    for (label, reply, modified_seconds) in [
        ("branch-left", "left reply", 1_000),
        ("branch-right", "right reply", 2_000),
    ] {
        app.messages = vec![
            hermes_core::Message::user("shared question"),
            hermes_core::Message::assistant(reply),
        ];
        let saved = command_output(&mut app, "/snapshot", &["save", label]).await;
        assert!(saved.contains(&format!("{label}.json")), "{saved}");
        let snapshot = sessions.join(format!("{label}.json"));
        std::fs::File::options()
            .write(true)
            .open(snapshot)
            .unwrap()
            .set_modified(
                std::time::SystemTime::UNIX_EPOCH
                    + std::time::Duration::from_secs(modified_seconds),
            )
            .expect("mark original fixture checkpoint as old");
    }
    let listed = command_output(&mut app, "/snapshot", &["list"]).await;
    assert!(listed.starts_with("Session snapshots:"), "{listed}");
    let newest = listed.lines().nth(1).expect("newest snapshot row");
    assert!(newest.starts_with("  - branch-right (latest)"), "{listed}");
    assert_eq!(listed.matches("(latest)").count(), 1, "{listed}");
    let rollback = command_output(&mut app, "/rollback", &["list"]).await;
    assert!(rollback.contains("recent snapshots:"), "{rollback}");
    for label in ["branch-left", "branch-right"] {
        assert!(listed.contains(label), "{listed}");
        assert!(rollback.contains(label), "{rollback}");
    }

    let diff = command_output(
        &mut app,
        "/branch",
        &["diff", "branch-left", "branch-right"],
    )
    .await;
    for expected in [
        "messages: 2 vs 2",
        "unique_to_branch-left: 1",
        "unique_to_branch-right: 1",
        "last_assistant_branch-left: left reply",
        "last_assistant_branch-right: right reply",
    ] {
        assert!(diff.contains(expected), "{diff}");
    }
    let snapshots = std::fs::read_dir(&sessions).unwrap().count();
    let messages: Vec<_> = app.messages.iter().map(message_signature).collect();
    for (args, expected) in [
        (&["diff", "missing"][..], "Snapshot 'missing' not found"),
        (
            &["diff", "branch-", "branch-right"][..],
            "Snapshot 'branch-' is ambiguous",
        ),
        (
            &["diff", "branch-left", "missing"][..],
            "Snapshot 'missing' not found",
        ),
        (
            &["diff", "branch-left", "branch-"][..],
            "Snapshot 'branch-' is ambiguous",
        ),
        (
            &["merge", "missing", "branch-right"][..],
            "Snapshot 'missing' not found",
        ),
        (
            &["merge", "branch-", "branch-right"][..],
            "Snapshot 'branch-' is ambiguous",
        ),
        (
            &["merge", "branch-left", "missing"][..],
            "Snapshot 'missing' not found",
        ),
        (
            &["merge", "branch-left", "branch-"][..],
            "Snapshot 'branch-' is ambiguous",
        ),
    ] {
        let output = command_output(&mut app, "/branch", args).await;
        assert!(output.contains(expected), "{args:?}: {output}");
    }
    assert_eq!(
        app.messages
            .iter()
            .map(message_signature)
            .collect::<Vec<_>>(),
        messages
    );
    assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), snapshots);

    let merged = command_output(
        &mut app,
        "/branch",
        &["merge", "branch-left", "branch-right"],
    )
    .await;
    assert!(merged.contains("appended_unique_messages: 1"), "{merged}");
    assert!(merged.contains("merged_total_messages: 3"), "{merged}");
    let expected = ["shared question", "right reply", "left reply"];
    assert_eq!(
        app.messages
            .iter()
            .map(|message| message.content.as_deref().unwrap())
            .collect::<Vec<_>>(),
        expected
    );

    let jumped = command_output(&mut app, "/timetravel", &["goto", "branch-left"]).await;
    assert!(jumped.contains("Loaded session 'branch-left'"), "{jumped}");
    assert_eq!(
        app.messages.last().unwrap().content.as_deref(),
        Some("left reply")
    );
    let latest = command_output(&mut app, "/timetravel", &["latest"]).await;
    assert!(
        latest.contains("merge-branch-left-into-branch-right"),
        "{latest}"
    );
    assert_eq!(
        app.messages
            .iter()
            .map(|message| message.content.as_deref().unwrap())
            .collect::<Vec<_>>(),
        expected
    );

    let undone = command_output(&mut app, "/timetravel", &["undo", "0"]).await;
    assert!(undone.contains("Rolled back 1 exchange(s)"), "{undone}");
    assert!(app.messages.is_empty());
    assert_eq!(
        app.take_pending_input_prefill().as_deref(),
        Some("shared question")
    );
    let corrupt = sessions.join("branch-corrupt.json");
    std::fs::write(&corrupt, "malformed checkpoint fixture").unwrap();
    let listed = command_output(&mut app, "/branch", &["list"]).await;
    for expected in [
        "✓ `branch-left` (messages=2)",
        "✓ `branch-right` (messages=2)",
        "⚠ `branch-corrupt` (json_invalid:",
    ] {
        assert!(listed.contains(expected), "{listed}");
    }
    assert_eq!(
        std::fs::read_to_string(&corrupt).unwrap(),
        "malformed checkpoint fixture"
    );
    let forked = command_output(&mut app, "/timetravel", &["branch", "review fixture"]).await;
    assert!(forked.contains("review-fixture.json"), "{forked}");
}
