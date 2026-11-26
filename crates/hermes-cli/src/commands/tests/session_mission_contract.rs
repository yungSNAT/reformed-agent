use super::*;

#[tokio::test]
async fn mission_trading_views_render_the_saved_report() {
    use crate::alpha_runtime::{
        CapitalAllocationRow, EnvProvenanceGate, ExperimentSpec, PortfolioRiskGovernor,
        RemediationRunbookAction, ReplayCanaryResult, RepoDriftSentinel, ResearchSourceIngestion,
        RunContextAudit, StrategyHypothesis, TradingAlphaReport,
    };

    if !crate::isolated_test_profile::enter(
        "commands::tests::session_mission_contract::mission_trading_views_render_the_saved_report",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let report_dir = crate::alpha_runtime::alpha_state_dir().join("trading");
    std::fs::create_dir_all(&report_dir).unwrap();
    let report_path = report_dir.join("last_report.json");
    let project = "fixture-project";
    let report = TradingAlphaReport {
        generated_at: "2024-01-01T00:00:00Z".into(),
        postmortem: "Fixture incident summary".into(),
        hypotheses: vec![StrategyHypothesis {
            id: "hypothesis-a".into(),
            statement: "Fixture hypothesis".into(),
            ..Default::default()
        }],
        experiments: vec![ExperimentSpec {
            id: "experiment-a".into(),
            control: "off".into(),
            treatment: "on".into(),
            pass_criterion: "reproducible".into(),
            ..Default::default()
        }],
        backtest_matrix: vec!["fixture backtest".into()],
        walkforward_checks: vec!["fixture walkforward".into()],
        meta_ranking: vec!["fixture ranking".into()],
        capital_allocator: vec![CapitalAllocationRow {
            project_id: project.into(),
            target_weight: 0.25,
            ..Default::default()
        }],
        risk_governor: PortfolioRiskGovernor {
            mode: "hold".into(),
            halt_new_entries: true,
            reason: "fixture drawdown rule".into(),
            ..Default::default()
        },
        repo_drift: vec![RepoDriftSentinel {
            project_id: project.into(),
            git_head: "current".into(),
            baseline_head: "baseline".into(),
            dirty_files: 2,
            changed_since_baseline: true,
            drift_state: "changed".into(),
        }],
        run_context_audits: vec![
            RunContextAudit {
                project_id: project.into(),
                files_scanned: 2,
                missing_metrics: vec!["latency".into(), "slippage".into()],
                ..Default::default()
            },
            RunContextAudit {
                project_id: "complete-fixture".into(),
                passed: true,
                files_scanned: 1,
                ..Default::default()
            },
        ],
        env_provenance: vec![EnvProvenanceGate {
            project_id: project.into(),
            inspected_files: vec!["fixture-config".into()],
            conflicting_keys: vec!["FIXTURE_SETTING".into()],
            decision: "review".into(),
            ..Default::default()
        }],
        replay_canary: vec![ReplayCanaryResult {
            project_id: project.into(),
            sample_size: 4,
            pass_rate: 0.75,
            decision: "hold".into(),
        }],
        remediation_runbook: vec![RemediationRunbookAction {
            project_id: project.into(),
            priority: "p1".into(),
            title: "Review snapshot".into(),
            command: "fixture-review".into(),
            rationale: "fixture mismatch".into(),
        }],
        research_sources: vec![ResearchSourceIngestion {
            project_id: project.into(),
            source: "notes".into(),
            path: "fixture-notes".into(),
            found: true,
            items: 2,
        }],
        ..Default::default()
    };
    let saved = serde_json::to_vec(&report).unwrap();
    std::fs::write(&report_path, &saved).unwrap();

    for (args, expected) in [
        (&["trading"][..], &["generated_at: 2024-01-01T00:00:00Z"][..]),
        (&["trading", "postmortem"], &["Fixture incident summary"]),
        (
            &["trading", "autoresearch"],
            &[
                "hypothesis-a",
                "experiment-a off -> on pass=reproducible",
                "fixture backtest",
                "fixture walkforward",
                "fixture ranking",
            ],
        ),
        (&["trading", "allocator"], &["fixture-project weight=0.2500"]),
        (
            &["trading", "governor"],
            &["mode=hold", "halt_new_entries=true", "fixture drawdown rule"],
        ),
        (
            &["trading", "drift"],
            &["fixture-project state=changed head=current baseline=baseline dirty_files=2 changed_since_baseline=true"],
        ),
        (
            &["trading", "audit"],
            &["files_scanned=2 missing=latency,slippage", "complete-fixture passed=true files_scanned=1 missing=none"],
        ),
        (
            &["trading", "provenance"],
            &["inspected_files=1 conflicts=FIXTURE_SETTING decision=review"],
        ),
        (&["trading", "replay"], &["sample_size=4 pass_rate=0.750 decision=hold"]),
        (
            &["trading", "runbook"],
            &["[p1] fixture-project :: Review snapshot", "cmd: fixture-review", "why: fixture mismatch"],
        ),
        (&["trading", "sources"], &["fixture-project:notes found=true items=2 path=fixture-notes"]),
        (&["trading", "unknown"], &["Usage: /mission trading"]),
    ] {
        let output = command_output(&mut app, "/mission", args).await;
        for &fragment in expected {
            assert!(output.contains(fragment), "{args:?} missing {fragment:?}: {output}");
        }
    }
    assert_eq!(std::fs::read(report_path).unwrap(), saved);
}

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
async fn bare_resume_can_recover_legacy_and_empty_canonical_snapshots() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::session_mission_contract::bare_resume_can_recover_legacy_and_empty_canonical_snapshots",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let sessions = home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let save = |name: &str, id: &str, messages: serde_json::Value| {
        let snapshot = serde_json::json!({
            "session_info": {"session_id": id},
            "messages": messages,
        });
        std::fs::write(sessions.join(format!("{name}.json")), snapshot.to_string()).unwrap();
    };
    let empty = command_output(&mut app, "/resume", &[]).await;
    assert!(empty.contains("No saved sessions found"), "{empty}");

    let oneshot = "oneshot-00000000-0000-4000-8000-000000000001";
    save(
        oneshot,
        oneshot,
        serde_json::json!([{"role": "User", "content": "One-shot fixture"}]),
    );
    std::fs::write(sessions.join("broken.json"), "{").unwrap();
    let active = app.session_id.clone();
    app.messages = vec![hermes_core::Message::user("Active local message")];
    let failure = command_output(&mut app, "/resume", &[]).await;
    assert!(
        failure.contains("No valid saved sessions found"),
        "{failure}"
    );
    assert_eq!(app.session_id, active);
    assert_eq!(
        app.messages,
        [hermes_core::Message::user("Active local message")]
    );

    let legacy = "legacy-session-id";
    save(
        "legacy-name",
        legacy,
        serde_json::json!([{"role": "User", "content": "Legacy local message"}]),
    );
    let restored = command_output(&mut app, "/resume", &[]).await;
    assert!(
        restored.contains("Resumed session 'legacy-name'"),
        "{restored}"
    );
    assert_eq!(app.session_id, legacy);
    assert_eq!(app.agent.config.session_id.as_deref(), Some(legacy));
    assert_eq!(
        app.messages,
        [hermes_core::Message::user("Legacy local message")]
    );

    let canonical = "empty-canonical-id";
    save(canonical, canonical, serde_json::json!([]));
    std::fs::OpenOptions::new()
        .write(true)
        .open(sessions.join(format!("{canonical}.json")))
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000))
        .unwrap();
    let restored = command_output(&mut app, "/resume", &[]).await;
    assert!(
        restored.contains("Resumed session 'empty-canonical-id' (0 messages"),
        "{restored}"
    );
    assert_eq!(app.session_id, canonical);
    assert_eq!(app.agent.config.session_id.as_deref(), Some(canonical));
    assert!(app.messages.is_empty());
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
