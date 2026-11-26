use super::*;
use crate::alpha_runtime::{
    append_objective_eval_sample, load_contextlattice_policy, load_objective_contract,
    load_objective_dag, load_objective_ensemble_policy, load_objective_learning_ledger,
    load_objective_profile, load_objective_simulation_policy,
};

async fn assert_objective_output(app: &mut App, args: &[&str], expected: &str) {
    // Lifecycle changes can prune UI rows anchored to the old prompt.
    app.ui_messages.clear();
    assert_eq!(
        handle_slash_command(app, "/objective", args)
            .await
            .unwrap_or_else(|error| panic!("{args:?}: {error}")),
        CommandResult::Handled
    );
    let output = app
        .ui_messages
        .iter()
        .rev()
        .find(|row| row.message.role == hermes_core::MessageRole::Assistant)
        .and_then(|row| row.message.content.as_deref())
        .unwrap_or_else(|| panic!("{args:?}: no new assistant output"));
    assert!(
        output.contains(expected),
        "{args:?}: expected {expected:?} in {output:?}"
    );
}

#[tokio::test]
async fn objective_lifecycle_persists_state_and_controls_prompt_injection() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::objective_contract::objective_lifecycle_persists_state_and_controls_prompt_injection",
    ) {
        return;
    }
    let _guard = test_env_lock::lock_async().await;
    let home = tempdir().expect("create temporary home");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;
    let objective = "Document the public CLI";

    assert_objective_output(&mut app, &[], "No objective set.").await;
    assert_objective_output(&mut app, &["lifecycle"], "No objective contract.").await;
    assert_objective_output(&mut app, &["behavior"], "No objective contract.").await;
    assert_objective_output(&mut app, &[objective], "Session objective set:").await;
    let created = load_objective_contract().unwrap().unwrap();
    assert_eq!(created.objective_text, objective);
    assert!(!created.trading_sensitive);
    assert_eq!(app.session_objective.as_deref(), Some(objective));
    assert_objective_output(&mut app, &[], "Current objective:").await;

    assert_objective_output(&mut app, &["lifecycle", "list"], "- paused").await;
    assert_objective_output(&mut app, &["lifecycle", "unknown"], "Usage:").await;
    assert_objective_output(
        &mut app,
        &["pause", "waiting", "for", "review"],
        "objective_injected=no",
    )
    .await;
    let paused = load_objective_contract().unwrap().unwrap();
    assert_eq!(paused.id, created.id);
    assert_eq!(paused.lifecycle_status, "paused");
    assert_eq!(paused.status_reason, "waiting for review");
    assert!(app.session_objective.is_none());
    assert_objective_output(&mut app, &["lifecycle"], "status=paused").await;
    assert_objective_output(&mut app, &["behavior", "list"], "- strict").await;
    assert_objective_output(&mut app, &["behavior", "strict"], "mode=strict").await;
    assert_objective_output(&mut app, &["behavior"], "success_criteria:").await;
    assert_objective_output(&mut app, &["state", "resume"], "status=active").await;
    assert_eq!(app.session_objective.as_deref(), Some(objective));

    assert_objective_output(&mut app, &["status"], "lifecycle_status: active").await;
    assert_objective_output(&mut app, &["off"], "Session objective cleared.").await;
    assert!(load_objective_contract().unwrap().is_none());
    assert!(app.session_objective.is_none());
}

#[tokio::test]
async fn objective_profile_changes_persist_and_reset_to_general_defaults() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::objective_contract::objective_profile_changes_persist_and_reset_to_general_defaults",
    ) {
        return;
    }
    let _guard = test_env_lock::lock_async().await;
    let home = tempdir().expect("create temporary home");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;

    assert_objective_output(&mut app, &["profile"], "profile_id: repo-general").await;
    assert_objective_output(&mut app, &["profile", "list"], "- operator-custom:").await;
    assert_objective_output(&mut app, &["profile", "set"], "Usage: /objective profile set").await;
    assert_eq!(load_objective_profile().unwrap().profile_id, "repo-general");

    assert_objective_output(
        &mut app,
        &["profile", "set", "Fixture Operator"],
        "profile_id=operator-custom",
    )
    .await;
    assert_eq!(
        load_objective_profile().unwrap().operator_hint,
        "Fixture Operator"
    );
    assert_objective_output(
        &mut app,
        &["profile", "unknown"],
        "Usage: /objective profile",
    )
    .await;
    assert_objective_output(&mut app, &["profile", "show"], "operator_hint: Fixture Operator").await;

    assert_objective_output(&mut app, &["profile", "me"], "shell=zsh").await;
    assert_eq!(
        load_objective_profile().unwrap().memory_backend,
        "contextlattice-primary"
    );
    assert_objective_output(&mut app, &["profile", "show"], "preferred_languages:\n- rust").await;
    assert_objective_output(&mut app, &["profile", "reset"], "profile_id=repo-general").await;
    assert_eq!(load_objective_profile().unwrap().profile_id, "repo-general");
}

#[tokio::test]
async fn objective_wait_replaces_the_previous_target_and_records_each_transition() {
    if !crate::isolated_test_profile::enter("commands::tests::objective_contract::objective_wait_replaces_the_previous_target_and_records_each_transition") {
        return;
    }
    let _guard = test_env_lock::lock_async().await;
    let home = tempdir().expect("create temporary home");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;
    assert_objective_output(&mut app, &["Write release notes"], "Session objective set:").await;
    assert_objective_output(&mut app, &["wait"], "Usage: /objective wait").await;

    let pid = std::process::id();
    assert_objective_output(&mut app, &["wait", &pid.to_string()], &format!("pid={pid}")).await;
    let process = load_objective_contract().unwrap().unwrap();
    assert_eq!(process.waiting_on_pid, Some(pid));

    assert_objective_output(
        &mut app,
        &["wait", "--session", "fixture-session", "rehearsal"],
        "fixture-session",
    )
    .await;
    let session = load_objective_contract().unwrap().unwrap();
    assert_eq!(session.waiting_on_pid, None);
    assert_eq!(
        session.waiting_on_session.as_deref(),
        Some("fixture-session")
    );
    assert_eq!(session.waiting_reason, "rehearsal");

    assert_objective_output(
        &mut app,
        &["wait", "--seconds", "60", "allow", "completion"],
        "continuation_enforcer=parked_until_released",
    )
    .await;
    let timer = load_objective_contract().unwrap().unwrap();
    assert_eq!(timer.waiting_on_session, None);
    assert!(timer.waiting_until_unix_ms > 0);
    assert_eq!(timer.waiting_reason, "allow completion");

    assert_objective_output(&mut app, &["unwait"], "Objective wait barrier cleared.").await;
    let cleared = load_objective_contract().unwrap().unwrap();
    assert_eq!(cleared.waiting_until_unix_ms, 0);
    assert!(cleared.waiting_reason.is_empty());
    let ledger = load_objective_learning_ledger().unwrap();
    let decisions: Vec<_> = ledger
        .entries
        .iter()
        .map(|entry| entry.decision.as_str())
        .collect();
    assert_eq!(
        decisions,
        [
            "objective_set",
            "objective_wait_pid",
            "objective_wait_session",
            "objective_wait_seconds",
            "objective_unwait",
        ]
    );
    assert_objective_output(&mut app, &["ledger", "status"], "entries=5").await;
    assert_objective_output(
        &mut app,
        &["ledger", "tail", "1"],
        "decision=objective_unwait",
    )
    .await;
    assert_objective_output(
        &mut app,
        &["ledger", "clear"],
        "Objective learning ledger cleared.",
    )
    .await;
    assert_objective_output(&mut app, &["ledger", "tail"], "(empty)").await;
}

#[tokio::test]
async fn objective_commands_persist_policy_and_workspace_records() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::objective_contract::objective_commands_persist_policy_and_workspace_records",
    ) {
        return;
    }
    let _guard = test_env_lock::lock_async().await;
    let home = tempdir().expect("create temporary home");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;
    assert_objective_output(
        &mut app,
        &["Document the market data format"],
        "Session objective set:",
    )
    .await;
    let contract = load_objective_contract().unwrap().unwrap();
    assert!(contract.trading_sensitive);
    assert_objective_output(&mut app, &["dag", "rebuild"], "Objective DAG rebuilt.").await;
    let dag = load_objective_dag().unwrap();
    assert_eq!(dag.objective_id, contract.id);
    assert!(!dag.nodes.is_empty());
    assert_objective_output(&mut app, &["dag", "status"], "depends_on=[").await;
    assert_objective_output(&mut app, &["dag", "clear"], "Objective DAG cleared.").await;
    assert_objective_output(&mut app, &["dag", "status"], "nodes: (empty)").await;

    assert_objective_output(&mut app, &["context", "list"], "policy presets:").await;
    assert_objective_output(&mut app, &["context", "fast"], "mode=fast").await;
    let context = load_contextlattice_policy().unwrap();
    assert_objective_output(
        &mut app,
        &["context", "status"],
        &format!("mode_hint: {}", context.preferred_retrieval_mode),
    )
    .await;
    assert_objective_output(&mut app, &["simulator", "invalid"], "Usage:").await;
    assert_objective_output(&mut app, &["simulator", "strict"], "mode=strict").await;
    assert_eq!(load_objective_simulation_policy().unwrap().mode, "strict");
    assert_objective_output(&mut app, &["simulator", "status"], "mode=strict").await;
    assert_objective_output(&mut app, &["ensemble", "invalid"], "Usage:").await;
    assert_objective_output(&mut app, &["ensemble", "debate"], "mode=debate").await;
    assert_eq!(load_objective_ensemble_policy().unwrap().mode, "debate");
    assert_objective_output(&mut app, &["ensemble", "status"], "mode=debate").await;

    assert_objective_output(&mut app, &["counterfactual"], "Usage:").await;
    assert_objective_output(
        &mut app,
        &["counterfactual", "Reuse the glossary | less editing"],
        "journal entries=1",
    )
    .await;
    let updated = load_objective_contract().unwrap().unwrap();
    assert_eq!(
        updated.counterfactual_journal[0].scenario,
        "Reuse the glossary"
    );
    assert_eq!(
        updated.counterfactual_journal[0].expected_delta,
        "less editing"
    );
}

#[tokio::test]
async fn objective_verifier_checks_trend_and_deduplicates_evidence_paths() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::objective_contract::objective_verifier_checks_trend_and_deduplicates_evidence_paths",
    ) {
        return;
    }
    let _guard = test_env_lock::lock_async().await;
    let home = tempdir().expect("create temporary home");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;

    assert_objective_output(&mut app, &["verify"], "No objective contract.").await;
    assert_objective_output(&mut app, &["plan"], "No objective contract.").await;
    assert_objective_output(&mut app, &["Update the CLI docs"], "Session objective set:").await;
    let contract = load_objective_contract().unwrap().unwrap();
    assert_objective_output(&mut app, &["plan"], "Utility weights:").await;
    assert_objective_output(&mut app, &["constraints"], "Objective hard constraints").await;
    assert_objective_output(&mut app, &["eval", "tail"], "(empty)").await;
    assert_objective_output(&mut app, &["verify"], "verified_files_total=0").await;

    std::fs::write("verified-evidence.txt", "fixture evidence\n").unwrap();
    app.messages.push(hermes_core::Message::assistant(
        "PATCH_VERIFIED path=verified-evidence.txt path=verified-evidence.txt path=missing-objective-evidence.txt",
    ));
    for (sample, expected) in [
        ("active", "unproven"),
        ("active", "flat"),
        ("complete", "advancing"),
        ("unmet", "regressing"),
    ] {
        append_objective_eval_sample(&contract.id, sample, "fixture evaluation").unwrap();
        assert_objective_output(
            &mut app,
            &["verify"],
            &format!("objective_state={expected}"),
        )
        .await;
        let output = latest_ui_assistant_text(&app);
        assert!(output.contains("verified_files_present=1"));
        assert!(output.contains("verified_files_total=2"));
    }
    assert_objective_output(&mut app, &["eval", "tail", "1"], "state=unmet").await;
    assert_objective_output(&mut app, &["eval", "status"], "samples=4").await;
}
