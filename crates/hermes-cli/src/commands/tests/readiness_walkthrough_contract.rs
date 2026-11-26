use super::*;

#[tokio::test]
async fn readiness_full_probes_local_memory_while_quick_skips_it() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    if !crate::isolated_test_profile::enter(
        "commands::tests::readiness_walkthrough_contract::readiness_full_probes_local_memory_while_quick_skips_it",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    assert!(command_output(&mut app, "/boot", &["profile", "list"])
        .await
        .contains("prod: warnings and fails"));
    assert!(command_output(&mut app, "/boot", &["profile", "fixture"])
        .await
        .contains("Usage: /boot profile"));
    assert!(command_output(&mut app, "/boot", &["profile", "prod"])
        .await
        .contains("Boot profile set to prod"));

    for (status, label) in [(200, "PASS"), (503, "WARN")] {
        let health = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&health)
            .await;
        std::env::set_var("CONTEXTLATTICE_ORCHESTRATOR_URL", health.uri());
        let quick = command_output(&mut app, "/boot", &["quick"]).await;
        assert!(quick.contains("Boot readiness gate (quick)"), "{quick}");
        assert!(quick.contains("profile: prod"), "{quick}");
        assert!(!quick.contains("ContextLattice probe"), "{quick}");
        assert!(health.received_requests().await.unwrap().is_empty());

        let full = command_output(&mut app, "/boot", &[]).await;
        assert!(full.contains("Boot readiness gate (full)"), "{full}");
        assert!(full.contains("Tool registry"), "{full}");
        assert!(
            full.contains(&format!("[{label}] ContextLattice probe")),
            "{full}"
        );
        if status == 503 {
            assert!(full.contains("status=503"), "{full}");
            assert!(full.contains("prod_policy:"), "{full}");
        }
        health.verify().await;
    }
    assert!(command_output(&mut app, "/boot", &["profile", "clear"])
        .await
        .contains("Cleared boot profile override"));
    assert!(command_output(&mut app, "/boot", &["profile", "status"])
        .await
        .contains("Boot profile: standard"));
}

#[tokio::test]
async fn walkthrough_tracks_full_progress_and_preserves_events_after_reset() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::readiness_walkthrough_contract::walkthrough_tracks_full_progress_and_preserves_events_after_reset",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let original = command_output(&mut app, "/walkthrough", &["insights"]).await;
    assert!(original.contains("starts: none"), "{original}");
    assert!(
        original.contains("dropoff: no completed steps yet"),
        "{original}"
    );
    let started = command_output(&mut app, "/onboard", &["start", "full"]).await;
    assert!(
        started.contains("Started full walkthrough (6 steps)"),
        "{started}"
    );
    let next = command_output(&mut app, "/walkthrough", &["next"]).await;
    assert!(next.contains("Next walkthrough step: boot-full"), "{next}");

    for (args, expected) in [
        (&["done"][..], "Usage: /walkthrough done"),
        (&["done", "not-a-step"][..], "Unknown step 'not-a-step'"),
        (&["not-an-action"][..], "Usage: /walkthrough [status"),
    ] {
        let output = command_output(&mut app, "/walkthrough", args).await;
        assert!(output.contains(expected), "{output}");
    }
    assert!(load_walkthrough_state().completed_steps.is_empty());
    let steps = [
        "boot-full",
        "commands-catalog",
        "auth-refresh",
        "objective-pin",
        "policy-check",
        "integration-check",
    ];
    for (index, step) in steps.iter().enumerate() {
        let output = command_output(&mut app, "/walkthrough", &["done", step]).await;
        assert!(output.contains(&format!("✓ {step}")), "{output}");
        let state = load_walkthrough_state();
        assert_eq!(state.completed_steps, steps[..=index]);
        assert_eq!(state.current_step, index + 1);
    }
    let next = command_output(&mut app, "/walkthrough", &["next"]).await;
    assert!(next.contains("Walkthrough complete"), "{next}");
    let insights = command_output(&mut app, "/walkthrough", &["insights"]).await;
    assert!(insights.contains("full => 1"), "{insights}");
    assert!(insights.contains("integration-check => 1"), "{insights}");
    assert!(
        insights.contains("resume_hint: Walkthrough complete"),
        "{insights}"
    );

    let reset = command_output(&mut app, "/walkthrough", &["reset"]).await;
    assert!(reset.contains("Walkthrough state reset"), "{reset}");
    assert!(!walkthrough_state_path().exists());
    let events = load_walkthrough_events(30);
    let completed: Vec<_> = events
        .iter()
        .filter(|row| row.action == "done")
        .map(|row| row.step_id.as_deref().unwrap())
        .collect();
    assert_eq!(completed, steps);
    assert_eq!(events.last().unwrap().action, "reset");
    assert!(events.iter().all(|row| row.session_id == app.session_id));
    let status = command_output(&mut app, "/walkthrough", &["status"]).await;
    assert!(status.contains("Walkthrough (quick)"), "{status}");
    assert!(status.contains("→ boot-gate"), "{status}");
}
