use super::*;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn isolated_home(test_name: &str) -> Option<PathBuf> {
    const CHILD_HOME: &str = "HERMES_CLI_RUNTIME_STATUS_TEST_CHILD_HOME";

    // Set home overrides only in a child running this one test, so parallel
    // tests cannot observe them.
    if let Some(expected_home) = std::env::var_os(CHILD_HOME) {
        let home = hermes_config::hermes_home();
        assert_eq!(home, PathBuf::from(expected_home));
        return Some(home);
    }

    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    fs::create_dir(&home).unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let full_name = format!("{module}::{test_name}");
    let output = Command::new(std::env::current_exe().unwrap()) // nosemgrep: rust.lang.security.current-exe.current-exe
        .arg("--exact")
        .arg(&full_name)
        .arg("--nocapture")
        .current_dir(sandbox.path())
        .env(CHILD_HOME, &home)
        .env("HERMES_HOME", &home)
        .env("HOME", sandbox.path())
        .env("USERPROFILE", sandbox.path())
        .env("APPDATA", sandbox.path())
        .env("LOCALAPPDATA", sandbox.path())
        .env("XDG_CONFIG_HOME", sandbox.path())
        .env("XDG_DATA_HOME", sandbox.path())
        .env("XDG_CACHE_HOME", sandbox.path())
        .env_remove("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .env("MEMMCP_ORCHESTRATOR_URL", "not-an-absolute-url///")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "{full_name} did not pass in its sandbox ({})\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

#[test]
fn balanced_policy_is_persisted_and_unknown_mode_is_rejected() {
    let Some(_) = isolated_home("balanced_policy_is_persisted_and_unknown_mode_is_rejected") else {
        return;
    };

    let fast = set_contextlattice_policy_mode("speed").unwrap();
    assert!(!fast.include_retrieval_debug_for_execution);

    let balanced = set_contextlattice_policy_mode(" BaLaNcEd ").unwrap();
    let invalid = set_contextlattice_policy_mode("unsupported-mode");
    assert!(matches!(
        invalid,
        Err(AgentError::Config(message))
            if message == "unknown contextlattice policy mode; expected one of: max|strict|balanced|fast|speed"
    ));

    let saved = load_contextlattice_policy().unwrap();
    for policy in [balanced, saved] {
        assert!(policy.preflight_required);
        assert!(policy.auto_context_pack_on_mission_start);
        assert!(policy.degradation_aware_planning);
        assert!(policy.readback_verification_required);
        assert!(policy.include_grounding_required);
        assert!(policy.include_retrieval_debug_for_execution);
        assert!(policy.broaden_scope_on_zero_hits);
        assert!(policy.scoped_recency_pass_before_finalize);
        assert!(policy.contradiction_check_across_layers);
        assert!(policy.numeric_fact_verbatim_copy);
        assert!(policy.objective_analytics_writeback_required);
        assert!(policy.required_project_scoping);
        assert!(policy.checkpoint_payload_requires_project_file_topic);
        assert_eq!(policy.preferred_retrieval_mode, "balanced");
        assert_eq!(policy.deep_retry_budget_secs, [90, 120, 180]);
        assert_eq!(policy.regular_retry_budget_secs, [90, 120]);
    }
}

const OLD_TIMESTAMP: &str = "2000-01-01T00:00:00Z";
const FUTURE_TIMESTAMP: &str = "2099-01-01T00:00:00Z";

fn fixture_event(id: &str, status: &str, fingerprint: &str, created_at: &str) -> LoopQueueEvent {
    LoopQueueEvent {
        id: id.to_string(),
        created_at: created_at.to_string(),
        loop_id: "fixture-loop".to_string(),
        event_type: "fixture-event".to_string(),
        status: status.to_string(),
        payload: "{}".to_string(),
        fingerprint: fingerprint.to_string(),
    }
}

fn assert_queue_statuses(expected: &[&str]) {
    let events = load_queue_events().unwrap();
    let actual: Vec<_> = events.iter().map(|event| event.status.as_str()).collect();
    assert_eq!(actual.as_slice(), expected);
}

#[test]
fn recovery_and_replay_persist_only_eligible_transitions() {
    let Some(home) = isolated_home("recovery_and_replay_persist_only_eligible_transitions") else {
        return;
    };

    let path = loop_queue_path();
    assert!(path.starts_with(&home));
    ensure_alpha_runtime_bootstrap(false).unwrap();

    write_queue_events(&[]).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "");
    assert!(load_queue_events().unwrap().is_empty());

    let events = [
        fixture_event("done", "replayed", "shared", OLD_TIMESTAMP),
        fixture_event("stale", "running", "shared", OLD_TIMESTAMP),
        fixture_event("duplicate", "queued", "shared", OLD_TIMESTAMP),
        fixture_event("recent", "running", "recent", FUTURE_TIMESTAMP),
        fixture_event("bad-date", "running", "bad-date", "not a timestamp"),
        fixture_event("next", "queued", "next", OLD_TIMESTAMP),
        fixture_event("over-limit", "queued", "over-limit", OLD_TIMESTAMP),
    ];
    write_queue_events(&events).unwrap();
    assert_eq!(recover_orphan_loop_events(60).unwrap(), 1);
    let recovered = [
        "replayed", "orphaned", "queued", "running", "running", "queued", "queued",
    ];
    assert_queue_statuses(&recovered);

    let clean = fs::read_to_string(&path).unwrap();
    let dirty = format!("\n \t\nnot JSON\n{clean}");
    fs::write(&path, &dirty).unwrap();
    assert_queue_statuses(&recovered);
    assert_eq!(recover_orphan_loop_events(60).unwrap(), 0);
    assert_eq!(fs::read_to_string(&path).unwrap(), dirty);

    assert_eq!(replay_loop_queue(0).unwrap(), 0);
    assert_queue_statuses(&recovered);
    assert_eq!(replay_loop_queue(2).unwrap(), 2);
    assert_queue_statuses(&[
        "replayed", "replayed", "deduped", "running", "running", "replayed", "queued",
    ]);
}

#[test]
fn runtime_state_persists_queue_totals_and_loop_health() {
    let Some(home) = isolated_home("runtime_state_persists_queue_totals_and_loop_health") else {
        return;
    };

    let path = loop_runtime_path();
    assert!(path.starts_with(&home));
    assert!(loop_queue_path().starts_with(&home));
    ensure_alpha_runtime_bootstrap(false).unwrap();
    let loops = load_alpha_loops().unwrap();
    write_queue_events(&[
        fixture_event("pending-a", "queued", "a", OLD_TIMESTAMP),
        fixture_event("pending-b", "queued", "b", OLD_TIMESTAMP),
        fixture_event("stale", "orphaned", "c", OLD_TIMESTAMP),
        fixture_event("done", "replayed", "d", OLD_TIMESTAMP),
        fixture_event("active", "running", "e", OLD_TIMESTAMP),
    ])
    .unwrap();

    for (counts, expected_status, expected_health) in [
        ((2, 1, 1, 3), "degraded", 0.25),
        ((2, 1, 3, 1), "running", 0.75),
        ((2, 0, 2, 2), "queued", 0.5),
        ((0, 0, 0, 0), "healthy", 0.0),
    ] {
        let state = refresh_loop_runtime_state(&loops, counts).unwrap();
        assert_eq!(
            (
                state.queue_pending,
                state.queue_replayable,
                state.orphaned_events
            ),
            (2, 3, 1)
        );
        assert!(hermes_core::time::parse_rfc3339(&state.updated_at).is_some());
        assert_eq!(state.loops.len(), loops.len());
        for (row, definition) in state.loops.iter().zip(&loops) {
            assert_eq!(row.id, definition.id);
            assert_eq!(row.last_status, expected_status);
            assert_eq!(row.success_count, counts.2 as u64);
            assert_eq!(row.failure_count, counts.3 as u64);
            assert!((row.health_score - expected_health).abs() < f64::EPSILON);
            assert!(row.last_started_at.is_none());
            assert!(row.last_finished_at.is_none());
        }

        let persisted: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted, serde_json::to_value(&state).unwrap());
    }
}

#[tokio::test]
async fn contextlattice_reports_invalid_fallback_url_without_connecting() {
    let Some(_) = isolated_home("contextlattice_reports_invalid_fallback_url_without_connecting")
    else {
        return;
    };

    // A relative URL fails during request construction, before any I/O.
    let status = contextlattice_status().await;
    assert!(status
        .health_line
        .starts_with("contextlattice: unreachable ("));
    assert_eq!(
        status.preflight_line,
        "contextlattice preflight: Rust-native memory write endpoint not-an-absolute-url/memory/write"
    );
}

#[test]
fn reasoning_keywords_are_case_insensitive_and_higher_tiers_win() {
    for (text, expected) in [
        ("INVESTIGATE a quick workaround", "high"),
        ("DeBuG the display", "high"),
        ("Implement a formatter", "high"),
        ("Describe the architecture", "high"),
        ("SUMMARIZE these notes", "low"),
        ("Give a short answer", "low"),
        ("List the headings", "low"),
        ("QUICK answer, please", "low"),
        ("Please explain this", "medium"),
        ("Quickly debug the SECURITY issue", "xhigh"),
    ] {
        assert_eq!(
            recommend_reasoning_level_from_text(text),
            expected,
            "{text}"
        );
    }
}

fn assert_oauth_rows(expected: &[(&str, &str, &str)]) {
    let rows = oauth_session_sentinel();
    let actual: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row.provider.as_str(),
                row.expires_at.as_str(),
                row.status.as_str(),
            )
        })
        .collect();
    assert_eq!(actual.as_slice(), expected);
}

#[test]
fn oauth_sentinel_handles_missing_invalid_and_both_metadata_layouts() {
    let Some(home) =
        isolated_home("oauth_sentinel_handles_missing_invalid_and_both_metadata_layouts")
    else {
        return;
    };

    let auth = home.join("auth");
    fs::create_dir_all(&auth).unwrap();
    let tokens = auth.join("tokens.json");
    assert_oauth_rows(&[]);
    fs::write(&tokens, "{ malformed").unwrap();
    assert_oauth_rows(&[]);
    fs::write(&tokens, "[]").unwrap();
    assert_oauth_rows(&[]);

    let now = jiff::Timestamp::now();
    let expires_in = |seconds| {
        now.checked_add(jiff::SignedDuration::from_secs(seconds))
            .unwrap()
            .to_string()
    };
    let soon = expires_in(1_800);
    let today = expires_in(21_600);
    let credentials = serde_json::json!({
        "credentials": [
            {"provider": "zeta", "expires_at": FUTURE_TIMESTAMP},
            {"provider": "urgent", "expires_at": &soon},
            {"expires_at": "not a timestamp"},
            {"provider": "skipped", "expires_at": 123},
            {"provider": "today", "expires_at": &today},
            {"provider": "expired", "expires_at": OLD_TIMESTAMP}
        ]
    });
    fs::write(&tokens, credentials.to_string()).unwrap();
    assert_oauth_rows(&[
        ("expired", OLD_TIMESTAMP, "expired"),
        ("today", today.as_str(), "expires<24h"),
        ("unknown", "not a timestamp", "unknown"),
        ("urgent", soon.as_str(), "expires<1h"),
        ("zeta", FUTURE_TIMESTAMP, "ok"),
    ]);

    let legacy = serde_json::json!({
        "legacy-z": {"expires_at": FUTURE_TIMESTAMP},
        "legacy-a": {"expires_at": OLD_TIMESTAMP},
        "empty": {"expires_at": ""},
        "missing": {}
    });
    fs::write(&tokens, legacy.to_string()).unwrap();
    assert_oauth_rows(&[
        ("legacy-a", OLD_TIMESTAMP, "expired"),
        ("legacy-z", FUTURE_TIMESTAMP, "ok"),
    ]);
}

#[tokio::test]
async fn mission_board_propagates_runtime_write_failure_before_status_requests() {
    let Some(home) =
        isolated_home("mission_board_propagates_runtime_write_failure_before_status_requests")
    else {
        return;
    };

    let path = loop_runtime_path();
    assert!(path.starts_with(&home));
    ensure_alpha_runtime_bootstrap(false).unwrap();
    if path.is_file() {
        fs::remove_file(&path).unwrap();
    }
    fs::create_dir_all(&path).unwrap();

    // A directory in place of the runtime JSON file prevents the board from
    // reaching either of its remote status lookups.
    assert!(refresh_loop_runtime_state(&[], (0, 0, 0, 0)).is_err());
    assert!(render_mission_board("fixture-model", None, (0, 0, 0, 0))
        .await
        .is_err());
}

#[test]
fn utility_terms_are_empty_without_a_persisted_contract() {
    let Some(_) = isolated_home("utility_terms_are_empty_without_a_persisted_contract") else {
        return;
    };

    assert!(utility_terms_from_contract().unwrap().is_empty());
}
