use super::*;

#[tokio::test]
async fn pet_command_validates_changes_and_reloads_saved_preferences() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::policy_context_contract::pet_command_validates_changes_and_reloads_saved_preferences",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    for (args, text) in [
        (&[][..], "- enabled: OFF"),
        (&["list"], "Available pets:"),
        (&["wake"], "Pet enabled."),
        (&["tuck"], "Pet hidden."),
        (&["toggle"], "Pet enabled."),
        (&["set"], "Usage: /pet set"),
        (&["set", "penguin"], "Unknown species"),
        (&["species", " OtTer "], "species: otter"),
        (&["mood"], "Usage: /pet mood"),
        (&["mood", "violet"], "Unknown mood"),
        (&["mood", "sleepy"], "mood: sleepy"),
        (&["dock"], "Usage: /pet dock"),
        (&["dock", "centre"], "Usage: /pet dock"),
        (&["dock", "right"], "dock: right"),
        (&["dock", "left"], "dock: left"),
        (&["speed"], "Usage: /pet speed"),
        (&["speed", "fast"], "Usage: /pet speed"),
        (&["speed", "660"], "speed_ms: 660"),
        (&["unknown"], "Usage: /pet"),
    ] {
        let output = command_output(&mut app, "/pet", args).await;
        assert!(output.contains(text), "{args:?}: {output}");
    }
    let expected = app.pet_settings().clone();
    assert!(expected.enabled);
    assert_eq!(expected.species, "otter");
    assert_eq!(expected.mood, "sleepy");
    assert_eq!(expected.dock, PetDock::Left);
    assert_eq!(expected.tick_ms, 660);
    drop(app);
    let restored = build_test_app_with_stream(&home).await;
    assert_eq!(restored.pet_settings(), &expected);
}

#[tokio::test]
async fn recap_and_context_report_the_merged_transcript_without_rewriting_it() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::policy_context_contract::recap_and_context_report_the_merged_transcript_without_rewriting_it",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    app.messages.clear();
    assert!(command_output(&mut app, "/context", &["breakdown"])
        .await
        .contains("No transcript"));
    assert!(command_output(&mut app, "/recap", &[])
        .await
        .contains("No activity"));
    app.messages.extend([
        hermes_core::Message::system("fixture system notes"),
        hermes_core::Message::user("fixture question"),
        hermes_core::Message::assistant("fixture answer\nsecond line"),
        hermes_core::Message::tool_result("fixture-call", "tool result"),
    ]);
    let durable = app.messages.clone();
    async fn report_with_ui_message(app: &mut App, command: &str, args: &[&str]) -> String {
        app.ui_messages.clear();
        app.push_ui_user("fixture UI-only follow-up");
        assert_eq!(
            handle_slash_command(app, command, args).await.unwrap(),
            CommandResult::Handled,
        );
        latest_ui_assistant_text(app)
    }

    let recap = report_with_ui_message(&mut app, "/recap", &["5"]).await;
    assert!(recap.contains("roles: user=2 assistant=1 tool=1 system=1"));
    assert!(recap.contains("latest_user: fixture UI-only follow-up"));
    assert!(recap.contains("latest_hermes: fixture answer"));
    let recent = command_output(&mut app, "/recap", &["2"]).await;
    assert!(recent.contains("roles: user=0 assistant=1 tool=1 system=0"));
    assert!(recent.contains("latest_user: (none)"));

    let summary = report_with_ui_message(&mut app, "/context", &[]).await;
    assert!(summary.contains("transcript_messages: 5"));
    let footprint = report_with_ui_message(&mut app, "/context", &["breakdown"]).await;
    for (role, preview) in [
        ("SYSTEM", "fixture system notes"),
        ("USER", "fixture question"),
        ("HERMES", "fixture answer"),
        ("TOOL", "tool result"),
        ("USER", "fixture UI-only follow-up"),
    ] {
        assert!(footprint
            .lines()
            .any(|line| line.contains(role) && line.ends_with(preview)));
    }
    assert!(!footprint.contains("second line"));
    assert!(command_output(&mut app, "/context", &["unknown"])
        .await
        .contains("Usage: /context"));
    assert_eq!(app.messages, durable);
}

#[tokio::test]
async fn policy_reports_and_applies_the_selected_profile() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::policy_context_contract::policy_reports_and_applies_the_selected_profile",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    let standard = command_output(&mut app, "/policy", &["standard"]).await;
    assert!(standard.contains("Policy profile switched to `standard`"));
    let status = command_output(&mut app, "/policy", &[]).await;
    for expected in [
        "Policy profile: standard",
        "Preset: balanced",
        "Mode: enforce",
        "Sandbox: balanced",
        "Skills tier: balanced",
        "Counters: allow=0 deny=0 audit_only=0 simulate=0 would_block=0",
    ] {
        assert!(status.contains(expected), "{status}");
    }

    let strict = command_output(&mut app, "/policy", &["strict"]).await;
    assert!(strict.contains("Policy profile switched to `strict`"));
    let status = command_output(&mut app, "/policy", &["status"]).await;
    assert!(status.contains("Policy profile: strict"));
    assert!(status.contains("Skills tier: trusted"));
    let profiles = command_output(&mut app, "/policy", &["list"]).await;
    assert!(profiles.lines().any(|line| line.starts_with("* strict")));
    assert!(profiles.contains("standard"));
    assert!(profiles.contains("dev"));

    let invalid = command_output(&mut app, "/policy", &["unknown"]).await;
    assert!(invalid.contains("Usage: /policy"));
    let status = command_output(&mut app, "/policy", &["status"]).await;
    assert!(status.contains("Policy profile: strict"));
    let standard = command_output(&mut app, "/policy", &["prod"]).await;
    assert!(standard.contains("Policy profile switched to `standard`"));
    let status = command_output(&mut app, "/policy", &["status"]).await;
    assert!(status.contains("Sandbox: balanced"));
}

#[tokio::test]
async fn history_prints_only_recent_messages_and_clips_unicode_previews() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::policy_context_contract::history_prints_only_recent_messages_and_clips_unicode_previews",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    app.messages.clear();
    let empty = command_output(&mut app, "/history", &[]).await;
    assert!(empty.contains("No conversation history"));
    app.messages
        .push(hermes_core::Message::user("outside the recent window"));
    for entry in 2..=10 {
        app.messages.push(hermes_core::Message::user(format!(
            "fixture message {entry}"
        )));
    }
    app.messages.extend([
        hermes_core::Message::system("fixture instructions"),
        hermes_core::Message::assistant(format!("{}\nhidden second line", "é".repeat(105))),
        hermes_core::Message::tool_result("fixture-call", "fixture response"),
    ]);

    let history = command_output(&mut app, "/history", &[]).await;
    let entries = history.lines().skip(1).collect::<Vec<_>>();
    assert_eq!(entries.len(), 12);
    assert!(entries[0].trim_start().starts_with("2. USER"));
    assert!(entries[9].contains("SYSTEM"));
    assert!(entries[10].contains("HERMES"));
    assert!(entries[10].ends_with(&format!("{}…", "é".repeat(95))));
    assert!(entries[11].contains("TOOL"));
    assert!(!history.contains("outside the recent window"));
    assert!(!history.contains("hidden second line"));
}

#[tokio::test]
async fn operator_controls_update_overrides_in_the_isolated_runtime() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::policy_context_contract::operator_controls_update_overrides_in_the_isolated_runtime",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    for (args, expected) in [
        (&["help"][..], "Operator control plane commands"),
        (&["unknown"], "Unknown /ops target"),
        (&["budget", "list"], "Repo-review budget profiles"),
        (
            &["budget", "clear"],
            "Cleared repo-review budget runtime overrides",
        ),
        (&["budget"], "repo_review_budget profile=balanced"),
        (
            &["budget", "aggressive"],
            "repo_review_budget set to 'aggressive'",
        ),
        (
            &["budget", "status"],
            "repo_review_budget profile=aggressive",
        ),
        (&["budget", "unknown"], "Usage: /ops budget"),
        (
            &["budget", "clear"],
            "Cleared repo-review budget runtime overrides",
        ),
        (&["budget", "status"], "repo_review_budget profile=balanced"),
        (&["tool-profile", "list"], "Repo-review tool profile modes"),
        (
            &["tool-profile", "focus"],
            "repo_review_tool_profile mode set to `focus`",
        ),
        (
            &["toolprofile", "show"],
            "repo_review_tool_profile mode=focus",
        ),
        (&["tool_profile", "unknown"], "Usage: /ops tool-profile"),
        (
            &["tool-profile", "clear"],
            "Cleared repo-review tool profile override",
        ),
        (
            &["tool-profile", "status"],
            "repo_review_tool_profile mode=off",
        ),
        (&["skills-tier", "unknown"], "Usage: /ops skills-tier"),
        (&["skills-tier", "trusted"], "skills_tier set to 'trusted'"),
        (
            &["skills-tier", "status"],
            "skills_tier=trusted (bypass=OFF)",
        ),
    ] {
        let output = command_output(&mut app, "/ops", args).await;
        assert!(output.contains(expected), "{args:?}: {output}");
    }
}

#[tokio::test]
async fn reporting_commands_show_runtime_state_and_validate_arguments() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::policy_context_contract::reporting_commands_show_runtime_state_and_validate_arguments",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    app.current_model = "fixture:runtime-contract".into();
    for (command, args, expected) in [
        ("/whoami", &[][..], "Active profile: default"),
        ("/telemetry", &["status"], "model: fixture:runtime-contract"),
        ("/telemetry", &["lane"], "lane hints:"),
        ("/telemetry", &["unknown"], "Usage: /telemetry"),
        ("/runbook", &["list"], "replay-trace"),
        ("/runbook", &["show"], "Usage: /runbook show"),
        ("/runbook", &["show", "unknown"], "Unknown runbook"),
        (
            "/runbook",
            &["show", "replay-trace"],
            "Inspect, verify, export, and diff replay traces",
        ),
        ("/runbook", &["unknown"], "Usage: /runbook"),
    ] {
        let output = command_output(&mut app, command, args).await;
        assert!(output.contains(expected), "{command} {args:?}: {output}");
    }

    let config = std::sync::Arc::make_mut(&mut app.config);
    config.platform_toolsets.clear();
    let output = command_output(&mut app, "/toolsets", &[]).await;
    assert!(output.contains("No explicit platform toolsets configured."));
    std::sync::Arc::make_mut(&mut app.config)
        .platform_toolsets
        .extend([
            ("zeta".to_string(), vec!["search".into()]),
            ("alpha".to_string(), vec!["files".into(), "editor".into()]),
        ]);
    let output = command_output(&mut app, "/toolsets", &[]).await;
    let rows: Vec<_> = output.lines().skip(1).collect();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].contains("alpha") && rows[0].contains("files, editor"));
    assert!(rows[1].contains("zeta") && rows[1].contains("search"));
}

#[tokio::test]
async fn qos_status_reports_and_ranks_local_route_health() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::policy_context_contract::qos_status_reports_and_ranks_local_route_health",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let status = command_output(&mut app, "/qos", &[]).await;
    assert!(status.contains("route_learning_entries=0"), "{status}");
    assert!(status.contains("route_health=unknown"), "{status}");
    assert!(status.contains("route_autotune_state=missing"), "{status}");

    std::fs::write(home.join("route-learning.json"), r#"{"entries":[{},{}]}"#)
        .expect("route learning fixture");
    std::fs::write(home.join("route-autotune.json"), "{}").expect("autotune fixture");
    let health_path = home.join("route-health.json");
    for (entries, trace) in [
        (serde_json::json!([]), "route_health_trace=no_entries"),
        (
            serde_json::json!([{}]),
            "route_health_trace=no_parseable_entries",
        ),
    ] {
        std::fs::write(
            &health_path,
            serde_json::json!({"entries": entries}).to_string(),
        )
        .expect("route health fixture");
        let status = command_output(&mut app, "/qos", &["status"]).await;
        assert!(status.contains(trace), "{status}");
    }

    std::fs::write(
        &health_path,
        serde_json::json!({
            "overall": "degraded",
            "generated_at": "2026-01-01T00:00:00Z",
            "summary": {"health_score": 0.67},
            "entries": [
                {"key": "rank-three", "tier": "normal", "health_score": 0.7},
                {"key": "out-of-range", "tier": "normal", "health_score": 0.95},
                {"key": "rank-one", "tier": "slow", "health_score": 0.1, "reasons": ["stale", 8, "retries"]},
                {"key": "rank-two", "health_score": 0.5}
            ]
        }).to_string(),
    ).expect("ranked route health fixture");
    let status = command_output(&mut app, "/qos", &["show"]).await;
    for expected in [
        "route_learning_entries=2",
        "route_health=degraded score=0.67",
        "route_autotune_state=present",
        "rank-one tier=slow score=0.10 reasons=stale|retries",
        "rank-two tier=unknown score=0.50 reasons=no_reasons",
        "rank-three tier=normal score=0.70 reasons=no_reasons",
    ] {
        assert!(status.contains(expected), "{status}");
    }
    assert!(status.find("rank-one").unwrap() < status.find("rank-two").unwrap());
    assert!(status.find("rank-two").unwrap() < status.find("rank-three").unwrap());
    assert!(!status.contains("out-of-range"), "{status}");
    let invalid = command_output(&mut app, "/qos", &["autotune", "invalid"]).await;
    assert!(
        invalid.contains("Usage: /qos autotune [plan|apply]"),
        "{invalid}"
    );
}

#[tokio::test]
async fn operator_reports_and_autopilot_controls_use_the_isolated_checkout() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::policy_context_contract::operator_reports_and_autopilot_controls_use_the_isolated_checkout",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let checkout = tempfile::tempdir_in(&home).expect("isolated checkout");
    let reports = checkout.path().join(".sync-reports");
    std::fs::create_dir(&reports).expect("report directory");
    std::env::set_var("HERMES_REPO_ROOT", checkout.path());
    let mut app = build_test_app_with_stream(&home).await;

    std::fs::write(reports.join("eval-trend-gate-01.json"), r#"{"ok":false}"#)
        .expect("older eval report");
    std::fs::write(reports.join("eval-trend-gate-02.json"), r#"{"ok":true}"#)
        .expect("latest eval report");
    std::fs::write(reports.join("slo-auto-rollback-01.json"), r#"{"ok":false}"#)
        .expect("failed SLO report");
    let gate = command_output(&mut app, "/ops", &["gate", "status"]).await;
    assert!(
        gate.contains("eval_trend=pass @ unknown (eval-trend-gate-02.json)"),
        "{gate}"
    );
    assert!(gate.contains("slo_rollback=fail"), "{gate}");
    assert!(gate.contains("elite_sync_gate=unknown"), "{gate}");
    let gate = command_output(&mut app, "/ops", &["gate", "slo"]).await;
    assert!(
        gate.contains("Set HERMES_SLO_CHECK_CMD and HERMES_SLO_ROLLBACK_CMD"),
        "{gate}"
    );

    let report = reports.join("performance-autopilot-01.json");
    let output = command_output(&mut app, "/ops", &["autopilot", "recommend"]).await;
    assert!(
        output.contains("No performance autopilot reports found"),
        "{output}"
    );
    std::fs::write(&report, r#"{"ok": true, "recommendations":[]}"#).expect("empty report");
    let output = command_output(&mut app, "/ops", &["autopilot", "recommend"]).await;
    assert!(
        output.contains("No recommendations found in performance-autopilot-01.json"),
        "{output}"
    );
    std::fs::write(
        report,
        r#"{"ok":true,"recommendations":[{"id":"FIXTURE_DELAY","severity":"P1","title":"Delay","recommendation":"Check local queue timing"}]}"#,
    ).expect("autopilot report");
    for (args, expected) in [
        (&["status"][..], "autopilot=pass"),
        (&["recs"][..], "Check local queue timing"),
        (&["profile", "list"][..], "Autopilot profiles:"),
        (
            &["profile", "quality"][..],
            "autopilot profile set to 'quality'",
        ),
        (&["profile", "status"][..], "autopilot profile=quality"),
        (&["profile", "unknown"][..], "Unknown profile 'unknown'"),
        (&["mode", "list"][..], "Autopilot modes:"),
        (&["mode", "off"][..], "autopilot mode set to 'off'"),
        (&["mode", "status"][..], "autopilot mode=off"),
        (&["mode", "unknown"][..], "Unknown mode 'unknown'"),
    ] {
        let args = ["autopilot"]
            .into_iter()
            .chain(args.iter().copied())
            .collect::<Vec<_>>();
        let output = command_output(&mut app, "/ops", &args).await;
        assert!(output.contains(expected), "{args:?}: {output}");
    }
}
