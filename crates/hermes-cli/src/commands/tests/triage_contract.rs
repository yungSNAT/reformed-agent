use super::*;

#[tokio::test]
async fn triage_learns_from_feedback_and_escalates_to_the_approval_queue() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::triage_contract::triage_learns_from_feedback_and_escalates_to_the_approval_queue",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    let status = command_output(&mut app, "/triage", &[]).await;
    assert!(status.contains("source_bias: none"), "{status}");

    for (payload, decision) in [("ok", "dropped"), ("fixture info", "notify-only")] {
        let output = command_output(&mut app, "/triage", &["queue", "fixture", payload]).await;
        assert!(
            output.contains(&format!("queue_action: {decision}")),
            "{output}"
        );
    }
    assert!(load_subconscious_state().tasks.is_empty());

    let output = command_output(&mut app, "/triage", &["eval", "fixture", "timeout error"]).await;
    assert!(output.contains("severity: 3"), "{output}");
    assert!(output.contains("decision: notify"), "{output}");
    let output = command_output(
        &mut app,
        "/triage",
        &["feedback", "Fixture", "confirmed", "timeout error"],
    )
    .await;
    assert!(output.contains("source_bias_now=+2"), "{output}");
    let output = command_output(&mut app, "/triage", &["eval", "fixture", "timeout error"]).await;
    assert!(output.contains("severity: 5"), "{output}");
    assert!(output.contains("decision: agent-run"), "{output}");
    let output = command_output(
        &mut app,
        "/triage",
        &["eval", "another-source", "timeout error"],
    )
    .await;
    assert!(
        output.contains("matched prior note 'timeout error'"),
        "{output}"
    );
    let status = command_output(&mut app, "/triage", &["status"]).await;
    assert!(status.contains("fixture => +2"), "{status}");

    let output = command_output(
        &mut app,
        "/triage",
        &["queue", "fixture-webhook", "fixture panic and outage"],
    )
    .await;
    let state = load_subconscious_state();
    let [task] = state.tasks.as_slice() else {
        panic!("expected one escalated task");
    };
    assert!(output.contains(&task.id), "{output}");
    assert_eq!(task.source, "fixture-webhook");
    assert_eq!(task.prompt, "fixture panic and outage");
    assert_eq!(task.status, "pending-approval");
    assert!(task.requires_approval);
    assert!(task.job_id.is_none());
}

#[tokio::test]
async fn subconscious_approval_and_dry_run_obey_the_selected_profile() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::triage_contract::subconscious_approval_and_dry_run_obey_the_selected_profile",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    for (prompt, status) in [
        ("verify local formatting", "pending"),
        ("review prod notes", "pending-approval"),
    ] {
        let output = command_output(&mut app, "/subconscious", &["add", prompt]).await;
        assert!(output.contains(&format!("status={status} ")), "{output}");
    }
    let state = load_subconscious_state();
    let [low, high] = state.tasks.as_slice() else {
        panic!("expected two review tasks");
    };
    let output = command_output(&mut app, "/subconscious", &["approve", &high.id]).await;
    assert!(output.contains(&format!("{} approve", high.id)), "{output}");

    for (profile, blocked) in [("strict", 1), ("balanced", 1), ("dev", 0)] {
        let output = command_output(
            &mut app,
            "/subconscious",
            &["run", "8", "--dry-run", &format!("profile={profile}")],
        )
        .await;
        assert!(
            output.contains(&format!("reviewed=2 dispatched=0 blocked={blocked}")),
            "{output}"
        );
        assert!(
            output.contains(&format!("{} would dispatch", low.id)),
            "{output}"
        );
        let action = if blocked == 0 {
            "would dispatch"
        } else {
            "blocked"
        };
        assert!(
            output.contains(&format!("{} {action}", high.id)),
            "{output}"
        );
    }
    let saved = load_subconscious_state();
    assert!(saved
        .tasks
        .iter()
        .all(|task| task.status == "pending" && task.job_id.is_none()));

    command_output(&mut app, "/subconscious", &["reject", &high.id]).await;
    let output = command_output(&mut app, "/subconscious", &["list"]).await;
    assert!(
        output.contains(&format!("{} [rejected]", high.id)),
        "{output}"
    );
    assert!(
        output.contains(&format!("{} [pending]", low.id)),
        "{output}"
    );
    command_output(&mut app, "/subconscious", &["clear"]).await;
    let output = command_output(&mut app, "/subconscious", &[]).await;
    assert!(output.contains("No queued subconscious tasks."), "{output}");
}
