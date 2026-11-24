use super::*;

async fn raw_output(app: &mut App, args: &[&str]) -> String {
    app.ui_messages.clear();
    assert_eq!(
        handle_slash_command(app, "/raw", args)
            .await
            .unwrap_or_else(|error| panic!("{args:?}: {error}")),
        CommandResult::Handled
    );
    let output = latest_ui_assistant_text(app);
    assert!(!output.is_empty(), "no output for {args:?}");
    output
}

#[tokio::test]
async fn raw_replay_command_reports_and_exports_fixture_events() {
    if !isolated_profile::enter(
        "replay_command_contract::raw_replay_command_reports_and_exports_fixture_events",
    ) {
        return;
    }
    verify_replay_command(&hermes_config::hermes_home()).await;
}

async fn verify_replay_command(home: &Path) {
    let mut app = build_test_app_with_stream(home).await;
    let path = home
        .join("logs")
        .join("replay")
        .join(format!("{}.jsonl", app.session_id));
    assert!(raw_output(&mut app, &["trace", "status"])
        .await
        .contains("Replay trace: OFF (no log yet)"));
    assert!(raw_output(&mut app, &["trace", "path"])
        .await
        .contains(&path.display().to_string()));
    assert!(raw_output(&mut app, &["trace", "focus"])
        .await
        .contains("trace focus <trace-id>"));
    for words in [
        &["trace", "tail"][..],
        &["trace", "focus", "chain-id"],
        &["trace", "graph"],
        &["trace", "verify"],
        &["trace", "export"],
    ] {
        let missing = raw_output(&mut app, words).await;
        assert!(
            missing.contains("Replay log not found"),
            "{words:?}: {missing}"
        );
    }
    for (args, expected) in [
        (&["trace", "on"][..], "Replay trace mode: ON"),
        (&["trace", "toggle"], "Replay trace mode: OFF"),
        (&["trace", "toggle"], "Replay trace mode: ON"),
        (&["trace", "off"], "Replay trace mode: OFF"),
        (&["trace", "help"], "Replay trace controls:"),
        (&["trace", "unknown"], "Usage: /raw trace"),
        (&["help"], "RTK raw controls:"),
        (&["once"], "armed for next tool call only"),
    ] {
        let output = raw_output(&mut app, args).await;
        assert!(output.contains(expected), "{args:?}: {output}");
    }
    assert!(app.tool_registry.raw_mode_state().once_pending);
    assert!(raw_output(&mut app, &[]).await.contains("one-shot pending"));
    assert!(raw_output(&mut app, &["yes"])
        .await
        .contains("RTK raw mode: ON"));
    assert!(!app.tool_registry.raw_mode_state().once_pending);
    assert!(raw_output(&mut app, &["toggle"])
        .await
        .contains("RTK raw mode: OFF"));
    assert!(raw_output(&mut app, &["no"])
        .await
        .contains("RTK raw mode: OFF"));
    assert!(raw_output(&mut app, &["unknown"])
        .await
        .contains("Usage: /raw"));

    std::fs::create_dir_all(path.parent().unwrap()).expect("create replay log directory");
    std::fs::write(
        &path,
        concat!(
            "{\"seq\":1,\"event\":\"entry.start\",\"trace_id\":\"chain-id\",\"prev_hash\":\"root\",\"event_hash\":\"h1\"}\n",
            "{\"seq\":2,\"event\":\"entry.end\",\"trace_id\":\"chain-id\",\"prev_hash\":\"h1\",\"event_hash\":\"h2\"}\n",
        ),
    )
    .expect("write fixture replay log");
    let tail = raw_output(&mut app, &["trace", "tail", "1"]).await;
    assert!(tail.contains("entry.end"));
    assert!(!tail.contains("entry.start"));
    let focus = raw_output(&mut app, &["trace", "focus", "chain-id", "invalid"]).await;
    assert!(focus.contains("events: 2"));
    assert!(focus.contains("entry.start") && focus.contains("entry.end"));
    let graph = raw_output(&mut app, &["trace", "graph", "2"]).await;
    assert!(graph.contains("root -> h1") && graph.contains("h1 -> h2"));
    let integrity = raw_output(&mut app, &["trace", "verify"]).await;
    assert!(integrity.contains("Replay integrity: PASS"));
    assert!(integrity.contains("entries: 2"));

    let output = raw_output(&mut app, &["trace", "export", "1"]).await;
    assert!(output.contains("rows: 1"));
    let export = path
        .parent()
        .unwrap()
        .join("exports")
        .join(format!("{}-tail.json", app.session_id));
    let bundle: serde_json::Value =
        serde_json::from_slice(&std::fs::read(export).unwrap()).unwrap();
    assert_eq!(bundle["rows"][0]["event"], "entry.end");
    assert_eq!(bundle["rows"].as_array().unwrap().len(), 1);

    let explicit = home.join("custom-export.json");
    let exported = raw_output(
        &mut app,
        &["trace", "export", "2", explicit.to_str().unwrap()],
    )
    .await;
    assert!(exported.contains("rows: 2"));
    assert!(explicit.is_file());

    std::fs::write(&path, "malformed fixture row\n").unwrap();
    let invalid = raw_output(&mut app, &["trace", "verify"]).await;
    assert!(invalid.contains("Replay integrity: FAIL"));
    assert!(invalid.contains("parse_errors: 1"));
}
