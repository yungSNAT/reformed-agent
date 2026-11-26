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

#[tokio::test]
async fn invalid_triage_and_queue_commands_leave_state_untouched() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::triage_contract::invalid_triage_and_queue_commands_leave_state_untouched",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    command_output(
        &mut app,
        "/triage",
        &["feedback", "fixture", "confirmed", "timeout error"],
    )
    .await;
    command_output(
        &mut app,
        "/subconscious",
        &["add", "verify fixture formatting"],
    )
    .await;
    let learning = trigger_triage_learning_state_path();
    let queue = subconscious_state_path();
    let saved_learning = std::fs::read(&learning).unwrap();
    let saved_queue = std::fs::read(&queue).unwrap();

    for (command, args, expected) in [
        ("/triage", &["rules"][..], "Trigger triage heuristics"),
        ("/triage", &["feedback"], "Usage: /triage feedback"),
        (
            "/triage",
            &["feedback", "fixture"],
            "Usage: /triage feedback",
        ),
        (
            "/triage",
            &["feedback", "fixture", "confirmed", " "],
            "Usage: /triage feedback",
        ),
        ("/triage", &["queue"], "Usage: /triage queue"),
        (
            "/triage",
            &["eval", "fixture", " "],
            "Payload cannot be empty",
        ),
        ("/triage", &["unexpected"], "Usage: /triage [status"),
        ("/subconscious", &["add", " "], "Usage: /subconscious add"),
        (
            "/subconscious",
            &["approve"],
            "Usage: /subconscious approve",
        ),
        (
            "/subconscious",
            &["reject", "sc-missing"],
            "Task not found: sc-missing",
        ),
        (
            "/subconscious",
            &["unexpected"],
            "Usage: /subconscious [status",
        ),
    ] {
        let output = command_output(&mut app, command, args).await;
        assert!(output.contains(expected), "{command} {args:?}: {output}");
        assert_eq!(
            std::fs::read(&learning).unwrap(),
            saved_learning,
            "{command} {args:?}"
        );
        assert_eq!(
            std::fs::read(&queue).unwrap(),
            saved_queue,
            "{command} {args:?}"
        );
    }
}

#[tokio::test]
async fn subconscious_profile_changes_and_clears_within_the_isolated_session() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::triage_contract::subconscious_profile_changes_and_clears_within_the_isolated_session",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    command_output(&mut app, "/subconscious", &["add", "review prod notes"]).await;
    let saved = load_subconscious_state();
    let [task] = saved.tasks.as_slice() else {
        panic!("expected one task in the isolated queue");
    };
    assert!(task.requires_approval);
    command_output(&mut app, "/subconscious", &["approve", &task.id]).await;
    let queue = subconscious_state_path();
    let saved_queue = std::fs::read(&queue).unwrap();

    for (args, expected) in [
        (&["profile", "status"][..], "Subconscious profile: balanced"),
        (
            &["profile", "list"],
            "strict: only low-risk non-approval tasks auto-dispatch",
        ),
        (&["profile", "strict"], "Subconscious profile set to strict"),
        (&["profile", "unknown"], "Usage: /subconscious profile"),
        (&["profile", "show"], "Subconscious profile: strict"),
    ] {
        let output = command_output(&mut app, "/subconscious", args).await;
        assert!(output.contains(expected), "{args:?}: {output}");
    }
    let preview = command_output(&mut app, "/subconscious", &["run", "preview"]).await;
    assert!(
        preview.contains("Dry-run subconscious run profile=strict"),
        "{preview}"
    );
    assert!(
        preview.contains("reviewed=1 dispatched=0 blocked=1"),
        "{preview}"
    );
    assert!(
        preview.contains(&format!("{} blocked", task.id)),
        "{preview}"
    );
    assert_eq!(std::fs::read(&queue).unwrap(), saved_queue);

    let cleared = command_output(&mut app, "/subconscious", &["profile", "clear"]).await;
    assert!(
        cleared.contains("Cleared subconscious profile override"),
        "{cleared}"
    );
    let status = command_output(&mut app, "/subconscious", &["profile"]).await;
    assert!(
        status.contains("Subconscious profile: balanced"),
        "{status}"
    );
}
