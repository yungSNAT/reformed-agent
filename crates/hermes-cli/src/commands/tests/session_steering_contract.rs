use super::*;

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
    let created_on_start = home
        .join("sessions")
        .join(format!("{}.json", app.session_id));
    if created_on_start.exists() {
        std::fs::remove_file(created_on_start).expect("remove the isolated empty startup snapshot");
    }

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
    let sessions = home.join("sessions");
    for (label, reply) in [
        ("branch-left", "left reply"),
        ("branch-right", "right reply"),
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
            .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000))
            .expect("mark original fixture checkpoint as old");
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
            &["merge", "branch-", "branch-right"][..],
            "Snapshot 'branch-' is ambiguous",
        ),
        (
            &["merge", "branch-left", "missing"][..],
            "Snapshot 'missing' not found",
        ),
    ] {
        let output = command_output(&mut app, "/branch", args).await;
        assert!(output.contains(expected), "{args:?}: {output}");
    }

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
    let listed = command_output(&mut app, "/branch", &["list"]).await;
    assert!(
        listed.contains("branch-left") && listed.contains("branch-right"),
        "{listed}"
    );
    let forked = command_output(&mut app, "/timetravel", &["branch", "review fixture"]).await;
    assert!(forked.contains("review-fixture.json"), "{forked}");
}
