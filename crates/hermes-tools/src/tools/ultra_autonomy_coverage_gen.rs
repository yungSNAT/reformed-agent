use std::path::Path;

use super::*;

fn ev(tool: &str, hash: &str, failed: bool, no_action: bool, text: Option<&str>) -> ToolLoopEvent {
    ToolLoopEvent {
        tool: tool.to_string(),
        params_hash: hash.to_string(),
        failed,
        no_action,
        step_text: text.map(str::to_string),
        timestamp: None,
    }
}

fn card(id: &str, status: CardStatus, priority: CardPriority, deps: &[&str]) -> AutonomyCard {
    AutonomyCard {
        id: id.to_string(),
        title: id.to_string(),
        description: String::new(),
        status,
        priority,
        labels: Vec::new(),
        dependencies: deps.iter().map(|d| d.to_string()).collect(),
        token_budget: None,
        token_used: 0,
        comments: Vec::new(),
        attachments: Vec::new(),
    }
}

fn board_with(cards: Vec<AutonomyCard>) -> AutonomyBoard {
    let now = Timestamp::now();
    AutonomyBoard {
        id: "cov".to_string(),
        title: "coverage".to_string(),
        context: String::new(),
        variables: BTreeMap::new(),
        instructions: Vec::new(),
        cards,
        created_at: now,
        updated_at: now,
    }
}

fn mc(content: &str, confidence: f64, last_seen_days: u32) -> MemoryCandidate {
    MemoryCandidate {
        content: content.to_string(),
        confidence,
        importance: 0.5,
        durability_days: 30,
        last_seen_days,
    }
}

fn provider(
    name: &str,
    available: bool,
    score: f64,
    confidence: f64,
    last_seen_days: u32,
    evidence_count: u32,
) -> MemoryProviderSignal {
    MemoryProviderSignal {
        provider: name.to_string(),
        available,
        score,
        confidence,
        last_seen_days,
        evidence_count,
    }
}

fn invalid_msg(err: ToolError) -> String {
    match err {
        ToolError::InvalidParams(msg) => msg,
        other => panic!("expected InvalidParams, got {other:?}"),
    }
}

fn not_found_msg(err: ToolError) -> String {
    match err {
        ToolError::NotFound(msg) => msg,
        other => panic!("expected NotFound, got {other:?}"),
    }
}

fn exec_failed_msg(err: ToolError) -> String {
    match err {
        ToolError::ExecutionFailed(msg) => msg,
        other => panic!("expected ExecutionFailed, got {other:?}"),
    }
}

async fn run(handler: &UltraAutonomyHandler, params: Value) -> Value {
    let raw = handler.execute(params).await.expect("execute");
    serde_json::from_str(&raw).expect("payload json")
}

async fn run_err(handler: &UltraAutonomyHandler, params: Value) -> ToolError {
    handler.execute(params).await.expect_err("expected error")
}

#[test]
fn loop_absolute_ceiling_hard_aborts() {
    let events = (0..LOOP_ABSOLUTE_MAX)
        .map(|i| ev("t", &i.to_string(), false, false, None))
        .collect::<Vec<_>>();
    let eval = evaluate_loop_events(&events);
    assert_eq!(eval.verdict, LoopVerdict::HardAbort);
    assert_eq!(eval.reason, "absolute tool-call ceiling reached");
    assert_eq!(eval.total_calls, LOOP_ABSOLUTE_MAX);
    assert!(eval.recommendation.contains("abort the turn"));
}

#[test]
fn loop_failed_ceiling_hard_aborts() {
    let events = (0..LOOP_FAILED_ABSOLUTE_MAX)
        .map(|i| ev(&format!("t{i}"), &i.to_string(), true, false, None))
        .collect::<Vec<_>>();
    let eval = evaluate_loop_events(&events);
    assert_eq!(eval.verdict, LoopVerdict::HardAbort);
    assert_eq!(eval.reason, "failed tool-call ceiling reached");
    assert_eq!(eval.failed_calls, LOOP_FAILED_ABSOLUTE_MAX);
    assert_eq!(eval.similar_failure_count, 1);
}

#[test]
fn loop_similar_failures_are_stuck() {
    let events = (0..LOOP_SIMILAR_FAILURE_THRESHOLD)
        .map(|i| ev("terminal", &i.to_string(), true, false, None))
        .collect::<Vec<_>>();
    let eval = evaluate_loop_events(&events);
    assert_eq!(eval.verdict, LoopVerdict::Stuck);
    assert_eq!(eval.reason, "same tool family is failing repeatedly");
    assert_eq!(eval.similar_failure_count, LOOP_SIMILAR_FAILURE_THRESHOLD);
    assert_eq!(eval.identical_repeat_count, 1);
    assert_eq!(
        eval.recommendation,
        "switch tool, inspect root cause, or surface blocker"
    );
}

#[test]
fn loop_repeated_text_is_suspicious() {
    let events = vec![
        ev("t", "1", false, false, Some("Hello  World")),
        ev("t", "2", false, false, Some("hello world")),
        ev("t", "3", false, false, Some(" HELLO\tworld ")),
        ev("t", "4", false, false, Some("Hello World")),
        ev("t", "5", false, false, Some("   ")),
    ];
    let eval = evaluate_loop_events(&events);
    assert_eq!(eval.verdict, LoopVerdict::Suspicious);
    assert_eq!(eval.reason, "model output text is repeating");
    assert_eq!(eval.repeated_text_count, 4);
    assert_eq!(
        eval.recommendation,
        "compress state and force a concrete next action"
    );
}

#[test]
fn loop_no_action_streak_is_suspicious() {
    let mut events = vec![ev("t", "first", false, false, None)];
    events
        .extend((0..LOOP_NO_ACTION_THRESHOLD).map(|i| ev("t", &i.to_string(), false, true, None)));
    let eval = evaluate_loop_events(&events);
    assert_eq!(eval.verdict, LoopVerdict::Suspicious);
    assert_eq!(eval.reason, "too many no-action steps");
    assert_eq!(eval.consecutive_no_action_steps, LOOP_NO_ACTION_THRESHOLD);
    assert_eq!(
        eval.recommendation,
        "force tool-backed progress or ask a blocking question"
    );
}

#[test]
fn loop_productive_when_no_threshold() {
    let eval = evaluate_loop_events(&[ev("t", "x", false, false, Some("ok"))]);
    assert_eq!(eval.verdict, LoopVerdict::Productive);
    assert_eq!(eval.reason, "no loop threshold crossed");
    assert_eq!(eval.recommendation, "continue within turn budget");
    assert_eq!(eval.repeated_text_count, 1);
}

#[test]
fn normalize_loop_text_collapses_and_lowercases() {
    assert_eq!(normalize_loop_text("  Foo\t BAR\nbaz "), "foo bar baz");
    assert_eq!(normalize_loop_text("   "), "");
}

#[test]
fn board_plan_blocks_and_sorts_ready_cards() {
    let mut budget = card("budget", CardStatus::Todo, CardPriority::Critical, &[]);
    budget.token_budget = Some(5);
    budget.token_used = 5;
    let board = board_with(vec![
        card("done1", CardStatus::Done, CardPriority::Normal, &[]),
        card("blk", CardStatus::Blocked, CardPriority::High, &[]),
        card("unk", CardStatus::Todo, CardPriority::High, &["ghost"]),
        card("wait", CardStatus::Todo, CardPriority::High, &["blk"]),
        budget,
        card("r-low", CardStatus::Todo, CardPriority::Low, &[]),
        card("r-b", CardStatus::Todo, CardPriority::Normal, &[]),
        card("r-a", CardStatus::Todo, CardPriority::Normal, &[]),
        card("r-high", CardStatus::Doing, CardPriority::High, &["done1"]),
        card("r-crit", CardStatus::Todo, CardPriority::Critical, &[]),
    ]);
    let plan = plan_board_execution(&board);
    assert_eq!(plan.ready, ["r-crit", "r-high", "r-a", "r-b", "r-low"]);
    assert_eq!(plan.done, ["done1"]);
    let blocked = plan
        .blocked
        .iter()
        .map(|b| (b.card_id.as_str(), b.reason.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        blocked,
        [
            ("blk", "card status is blocked"),
            ("unk", "unknown dependencies: ghost"),
            ("wait", "waiting on dependencies: blk"),
            ("budget", "card token budget exhausted"),
        ]
    );
    let mut order = plan.execution_order.clone();
    order.sort();
    assert_eq!(order, ["r-a", "r-b", "r-crit", "r-high", "r-low"]);
    assert_eq!(plan.token_budget_remaining, Some(0));
    assert_eq!(plan.events[0].payload["blocked_count"], 4);
    assert_eq!(plan.events[0].payload["ready_count"], 5);
}

#[test]
fn topological_order_follows_ready_edges() {
    let board = board_with(vec![
        card("b", CardStatus::Todo, CardPriority::Normal, &["a"]),
        card("a", CardStatus::Todo, CardPriority::Normal, &[]),
        card("c", CardStatus::Todo, CardPriority::Normal, &["b"]),
    ]);
    let ready = ["a".to_string(), "b".to_string(), "c".to_string()];
    let order = topological_ready_order(&board, &ready).expect("acyclic");
    assert_eq!(order, ["a", "b", "c"]);
}

#[test]
fn topological_order_detects_cycle() {
    let board = board_with(vec![
        card("x", CardStatus::Todo, CardPriority::Normal, &["y"]),
        card("y", CardStatus::Todo, CardPriority::Normal, &["x"]),
    ]);
    let ready = ["x".to_string(), "y".to_string()];
    assert!(topological_ready_order(&board, &ready).is_none());
}

#[test]
fn resource_plan_user_override_limits() {
    let plan = resource_admission_plan(ResourceGovernorInput {
        cpu_cores: 8,
        free_ram_mb: None,
        total_ram_mb: Some(16_000),
        ram_per_agent_mb: 2_048,
        min_free_ram_mb: 1_024,
        token_budget_remaining: None,
        per_agent_token_reserve: 32_000,
        user_override: Some(2),
    });
    assert_eq!(plan.max_concurrent, 2);
    assert_eq!(plan.limiting_factor, "user_override");
    assert_eq!(plan.cpu_limit, 7);
    assert_eq!(plan.ram_limit, None);
    assert_eq!(
        plan.reasons,
        [
            "cpu_limit=7",
            "ram_limit=unproven_no_ram_sample",
            "user_override=2"
        ]
    );

    let zero = resource_admission_plan(ResourceGovernorInput {
        cpu_cores: 4,
        free_ram_mb: None,
        total_ram_mb: None,
        ram_per_agent_mb: 2_048,
        min_free_ram_mb: 1_024,
        token_budget_remaining: None,
        per_agent_token_reserve: 32_000,
        user_override: Some(0),
    });
    assert_eq!(zero.max_concurrent, 1);
    assert!(zero.reasons.contains(&"user_override=1".to_string()));
}

#[test]
fn memory_lifecycle_tiers_providers() {
    let snapshot = memory_lifecycle_snapshot(&[
        provider("Offline", false, 2.0, 0.9, 0, 5),
        provider("Scored", true, 1.3, 0.1, 100, 0),
        provider("confident", true, 0.1, 0.9, 100, 0),
        provider("recent", true, 0.1, 0.1, 3, 0),
        provider("monthly", true, 0.1, 0.1, 20, 0),
        provider("evidenced", true, 0.1, 0.1, 60, 3),
        provider("stale", true, 0.1, 0.1, 60, 0),
        provider(" Stale ", true, 0.1, 0.1, 90, 1),
    ]);
    assert_eq!(snapshot.hot, ["confident", "recent", "scored"]);
    assert_eq!(snapshot.warm, ["evidenced", "monthly"]);
    assert_eq!(snapshot.archive, ["offline", "stale"]);
    assert!(snapshot.provenance_required);
    assert_eq!(snapshot.recall_budget_chars, 1200);
}

#[test]
fn memory_resolution_paths() {
    let created = resolve_memory_candidate(None, mc("new", 0.7, 0), 0.0, false);
    assert_eq!(created.action, "create");
    assert_eq!(created.retained_content, "new");
    assert_eq!(created.provenance_note, "new typed memory candidate");

    let reinforced =
        resolve_memory_candidate(Some(mc("old", 0.7, 3)), mc("new", 0.7, 0), 0.8, false);
    assert_eq!(reinforced.action, "reinforce");
    assert_eq!(reinforced.retained_content, "old");
    assert_eq!(reinforced.reason, "overlap 0.80 exceeded merge threshold");

    let newer = resolve_memory_candidate(Some(mc("old", 0.7, 5)), mc("new", 0.7, 1), 0.8, true);
    assert_eq!(newer.action, "supersede");
    assert_eq!(newer.retained_content, "new");
    assert_eq!(
        newer.provenance_note,
        "superseded lower-confidence memory: old"
    );

    let weaker = resolve_memory_candidate(Some(mc("old", 0.7, 5)), mc("new", 0.5, 0), 0.1, true);
    assert_eq!(weaker.action, "retain_existing");
    assert_eq!(weaker.retained_content, "old");
    assert_eq!(
        weaker.provenance_note,
        "rejected conflicting candidate: new"
    );

    let older = resolve_memory_candidate(Some(mc("old", 0.7, 1)), mc("new", 0.7, 9), 0.1, true);
    assert_eq!(older.action, "retain_existing");

    let related = resolve_memory_candidate(Some(mc("old", 0.7, 1)), mc("new", 0.7, 0), 0.3, false);
    assert_eq!(related.action, "create_related");
    assert_eq!(related.retained_content, "new");
    assert_eq!(
        related.provenance_note,
        "related memory should remain separately searchable"
    );
}

#[test]
fn parse_card_status_and_priority_aliases() {
    for (raw, expected) in [
        ("todo", CardStatus::Todo),
        (" Doing ", CardStatus::Doing),
        ("in_progress", CardStatus::Doing),
        ("done", CardStatus::Done),
        ("complete", CardStatus::Done),
        ("COMPLETED", CardStatus::Done),
        ("blocked", CardStatus::Blocked),
        ("question", CardStatus::Question),
        ("waiting", CardStatus::Question),
        ("needs_feedback", CardStatus::Question),
    ] {
        assert_eq!(parse_card_status(raw).ok(), Some(expected), "status {raw}");
    }
    let err = parse_card_status("Nope").expect_err("bad status");
    assert_eq!(invalid_msg(err), "unknown card status: nope");

    for (raw, expected) in [
        ("low", CardPriority::Low),
        ("normal", CardPriority::Normal),
        ("Medium", CardPriority::Normal),
        ("high", CardPriority::High),
        ("critical", CardPriority::Critical),
        ("P0", CardPriority::Critical),
    ] {
        assert_eq!(
            parse_card_priority(raw).ok(),
            Some(expected),
            "priority {raw}"
        );
    }
    let err = parse_card_priority("urgent").expect_err("bad priority");
    assert_eq!(invalid_msg(err), "unknown card priority: urgent");
}

#[test]
fn small_helpers_behave() {
    assert_eq!(
        stable_value_fingerprint(&json!({"b": 1, "a": [true, null, "x"]})),
        "a:[true,null,x]|b:1"
    );
    assert_eq!(stable_value_fingerprint(&json!(null)), "null");
    assert_eq!(stable_value_fingerprint(&json!(2.5)), "2.5");

    assert!(bool_param(&json!({"f": true}), "f", false));
    assert!(bool_param(&json!({"f": "yes"}), "f", true));
    assert!(!bool_param(&json!({}), "f", false));

    assert_eq!(string_array(Some(&json!([" a ", "", 3, "b"]))), ["a", "b"]);
    assert!(string_array(Some(&json!("nope"))).is_empty());
    assert!(string_array(None).is_empty());

    assert_eq!(safe_id("a b/c"), "a-b-c");
    assert_eq!(safe_id("../.."), "default");
    assert_eq!(safe_id("ok_id-1"), "ok_id-1");

    assert_eq!(
        exec_failed_msg(io_err(std::io::Error::other("boom"))),
        "boom"
    );
    let json_error = serde_json::from_str::<Value>("{").expect_err("bad json");
    let expected = json_error.to_string();
    assert_eq!(exec_failed_msg(json_err(json_error)), expected);

    assert!(default_cpu_cores() >= 1);
    let providers = default_memory_providers();
    assert_eq!(providers.len(), 3);
    assert_eq!(providers[1].provider, "contextlattice");
}

#[test]
fn resource_plan_action_reads_params_and_defaults() {
    let payload = resource_plan_action(&json!({
        "cpu_cores": 4,
        "free_ram_mb": 10_000,
        "total_ram_mb": 16_000,
        "token_budget_remaining": 64_000,
        "user_override": 3
    }));
    assert_eq!(payload["input"]["cpu_cores"], 4);
    assert_eq!(
        payload["input"]["ram_per_agent_mb"],
        DEFAULT_RAM_PER_AGENT_MB
    );
    assert_eq!(payload["input"]["min_free_ram_mb"], DEFAULT_MIN_FREE_RAM_MB);
    assert_eq!(
        payload["input"]["per_agent_token_reserve"],
        DEFAULT_PER_AGENT_TOKEN_RESERVE
    );
    assert_eq!(payload["input"]["user_override"], 3);
    assert_eq!(payload["plan"]["cpu_limit"], 3);
    assert_eq!(payload["plan"]["ram_limit"], 3);
    assert_eq!(payload["plan"]["token_limit"], 2);
    assert_eq!(payload["plan"]["max_concurrent"], 2);
    assert_eq!(payload["plan"]["limiting_factor"], "token");

    let defaults = resource_plan_action(&json!({}));
    let cores = default_cpu_cores();
    assert_eq!(defaults["input"]["cpu_cores"], cores);
    assert_eq!(
        defaults["plan"]["cpu_limit"],
        cores.saturating_sub(1).max(1)
    );
    assert!(defaults["plan"]["ram_limit"].is_null());
    assert_eq!(
        defaults["plan"]["reasons"][1],
        "ram_limit=unproven_no_ram_sample"
    );
}

#[test]
fn memory_lifecycle_action_defaults_and_custom() {
    let defaults = memory_lifecycle_action(&json!({}));
    assert_eq!(defaults["providers"].as_array().map(Vec::len), Some(3));
    assert_eq!(
        defaults["lifecycle"]["hot"],
        json!(["builtin", "contextlattice"])
    );
    assert_eq!(defaults["lifecycle"]["warm"], json!(["supermemory"]));

    let custom = memory_lifecycle_action(&json!({
        "providers": [{"provider": "gone", "available": false}]
    }));
    assert_eq!(custom["lifecycle"]["archive"], json!(["gone"]));
    assert_eq!(custom["providers"].as_array().map(Vec::len), Some(1));
}

#[test]
fn memory_resolve_action_variants() {
    let created = memory_resolve_action(&json!({"content": "remember this"})).expect("create");
    assert_eq!(created["resolution"]["action"], "create");
    assert_eq!(created["resolution"]["retained_content"], "remember this");

    let reinforced = memory_resolve_action(&json!({
        "candidate": {"content": "new", "confidence": 0.8, "importance": 0.5, "durability_days": 30, "last_seen_days": 0},
        "existing": {"content": "old", "confidence": 0.8, "importance": 0.5, "durability_days": 30, "last_seen_days": 2},
        "overlap": 0.9
    }))
    .expect("reinforce");
    assert_eq!(reinforced["resolution"]["action"], "reinforce");
    assert_eq!(reinforced["resolution"]["retained_content"], "old");

    let conflict = memory_resolve_action(&json!({
        "candidate": {"content": "new", "confidence": 0.9, "importance": 0.5, "durability_days": 30, "last_seen_days": 0},
        "existing": {"content": "old", "confidence": 0.5, "importance": 0.5, "durability_days": 30, "last_seen_days": 2},
        "conflict": true
    }))
    .expect("supersede");
    assert_eq!(conflict["resolution"]["action"], "supersede");

    let bad_candidate =
        memory_resolve_action(&json!({"candidate": {"content": 1}})).expect_err("bad candidate");
    assert!(invalid_msg(bad_candidate).starts_with("candidate:"));

    let bad_existing =
        memory_resolve_action(&json!({"content": "x", "existing": {"content": "y"}}))
            .expect_err("bad existing");
    assert!(invalid_msg(bad_existing).starts_with("existing:"));
}

#[test]
fn eval_actions_reject_non_object_params() {
    let err = outcome_rehearsal_action(&json!("not an object")).expect_err("outcome err");
    assert!(invalid_msg(err).starts_with("outcome_rehearsal:"));
    let err = recall_quality_action(&json!(42)).expect_err("recall err");
    assert!(invalid_msg(err).starts_with("recall_quality:"));
}

#[test]
fn static_surfaces_have_expected_content() {
    let service = service_plan_action();
    assert_eq!(service["one_command"], "hermes-ultra up");
    assert_eq!(
        service["equivalent_steps"].as_array().map(Vec::len),
        Some(3)
    );

    let channels = channel_surface_action();
    assert_eq!(channels["channels"].as_array().map(Vec::len), Some(4));
    assert!(channels["permission_modes"]
        .as_array()
        .expect("modes")
        .contains(&json!("yolo_recoverable_only")));

    let catalog = event_catalog();
    assert_eq!(catalog.len(), 9);
    assert_eq!(
        event_catalog_action()["events"][0]["event"],
        "board.created"
    );

    let help = help_action();
    assert_eq!(help["tool"], TOOL_NAME);
    assert_eq!(help["actions"].as_array().map(Vec::len), Some(17));
}

#[test]
fn record_loop_event_drains_to_absolute_max() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = UltraAutonomyState::new(tmp.path().join("ua"));
    let old = (0..LOOP_ABSOLUTE_MAX)
        .map(|i| ev("old", &format!("h{i}"), false, false, None))
        .collect::<Vec<_>>();
    let path = state.loop_path("drain");
    fs::write(&path, serde_json::to_string(&old).expect("encode")).expect("write");

    let out = record_loop_event(
        &state,
        &json!({"session": "drain", "tool": "new", "params_hash": "h"}),
    )
    .expect("record");
    assert_eq!(out["events"], LOOP_ABSOLUTE_MAX);
    assert_eq!(out["evaluation"]["verdict"], "hard_abort");

    let stored: Vec<ToolLoopEvent> =
        serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("decode");
    assert_eq!(stored.len(), LOOP_ABSOLUTE_MAX);
    assert_eq!(stored[0].params_hash, "h1");
    assert_eq!(stored.last().map(|e| e.tool.as_str()), Some("new"));
}

#[test]
fn loop_files_corrupt_handling() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = UltraAutonomyState::new(tmp.path().join("ua"));
    fs::write(state.loop_path("bad"), "not json").expect("write");

    let err = evaluate_loop_action(&state, &json!({"session": "bad"})).expect_err("corrupt");
    assert!(exec_failed_msg(err).contains("read loop events"));

    let out = record_loop_event(&state, &json!({"session": "bad", "tool": "t"})).expect("record");
    assert_eq!(out["events"], 1);
    assert_eq!(out["session"], "bad");

    fs::write(state.board_path("broken"), "{").expect("write board");
    let err = board_plan_action(&state, &json!({"board_id": "broken"})).expect_err("parse");
    assert!(exec_failed_msg(err).contains("parse board"));
}

#[tokio::test]
async fn handler_status_and_static_actions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let handler = UltraAutonomyHandler::new(tmp.path().to_path_buf());
    let root = tmp.path().join("ultra_autonomy");

    let status = run(&handler, json!({})).await;
    assert_eq!(status["status"], "ok");
    assert_eq!(status["boards"], 0);
    assert_eq!(status["rust_runtime_surface"], true);
    assert_eq!(
        status["implemented_items"].as_array().map(Vec::len),
        Some(12)
    );
    assert_eq!(status["event_topics"].as_array().map(Vec::len), Some(9));
    assert_eq!(status["data_root"].as_str(), root.to_str());

    let help = run(&handler, json!({"action": "  HELP "})).await;
    assert_eq!(help["tool"], "ultra_autonomy");

    let service = run(&handler, json!({"action": "service_plan"})).await;
    assert_eq!(service["logs"], "hermes-ultra logs --follow");

    let channels = run(&handler, json!({"action": "channel_surface"})).await;
    assert_eq!(channels["channels"][0]["name"], "cli");

    let events = run(&handler, json!({"action": "events"})).await;
    assert_eq!(events["events"].as_array().map(Vec::len), Some(9));

    let resource = run(&handler, json!({"action": "resource_plan", "cpu_cores": 2})).await;
    assert_eq!(resource["plan"]["cpu_limit"], 1);

    let lifecycle = run(&handler, json!({"action": "memory_lifecycle"})).await;
    assert_eq!(lifecycle["lifecycle"]["warm"], json!(["supermemory"]));

    let resolve = run(
        &handler,
        json!({"action": "memory_resolve", "content": "fact"}),
    )
    .await;
    assert_eq!(resolve["resolution"]["action"], "create");

    let err = run_err(&handler, json!({"action": "nope"})).await;
    assert_eq!(invalid_msg(err), "unknown ultra_autonomy action: nope");
}

#[tokio::test]
async fn handler_loop_record_and_evaluate() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let handler = UltraAutonomyHandler::new(tmp.path().to_path_buf());

    let first = run(
        &handler,
        json!({
            "action": "loop_record",
            "session": "s1",
            "tool": "terminal",
            "tool_params": {"b": 1, "a": [true, null, "x"]},
            "step_text": "hi",
            "failed": true
        }),
    )
    .await;
    assert_eq!(first["session"], "s1");
    assert_eq!(first["events"], 1);
    assert_eq!(first["evaluation"]["failed_calls"], 1);

    let loop_file: &Path = &tmp
        .path()
        .join("ultra_autonomy")
        .join("loops")
        .join("s1.json");
    let stored: Vec<ToolLoopEvent> =
        serde_json::from_str(&fs::read_to_string(loop_file).expect("read")).expect("decode");
    assert_eq!(stored[0].params_hash, "a:[true,null,x]|b:1");
    assert_eq!(stored[0].step_text.as_deref(), Some("hi"));
    assert!(stored[0].failed);
    assert!(stored[0].timestamp.is_some());

    let second = run(
        &handler,
        json!({"action": "loop_record", "session": "s1", "tool": "terminal", "params_hash": "h", "no_action": true}),
    )
    .await;
    assert_eq!(second["events"], 2);
    assert_eq!(second["evaluation"]["consecutive_no_action_steps"], 1);

    let from_file = run(
        &handler,
        json!({"action": "loop_evaluate", "session": "s1"}),
    )
    .await;
    assert_eq!(from_file["events"], 2);
    assert_eq!(from_file["evaluation"]["verdict"], "productive");

    let empty = run(
        &handler,
        json!({"action": "loop_evaluate", "session": "missing"}),
    )
    .await;
    assert_eq!(empty["events"], 0);

    let inline = run(
        &handler,
        json!({"action": "loop_evaluate", "events": [
            {"tool": "x", "params_hash": "p"},
            {"tool": "x", "params_hash": "p"},
            {"tool": "x", "params_hash": "p"},
            {"tool": "x", "params_hash": "p"}
        ]}),
    )
    .await;
    assert_eq!(inline["events"], 4);
    assert_eq!(inline["evaluation"]["verdict"], "stuck");

    let err = run_err(
        &handler,
        json!({"action": "loop_evaluate", "events": "bad"}),
    )
    .await;
    assert!(invalid_msg(err).starts_with("events:"));

    let err = run_err(&handler, json!({"action": "loop_record", "session": "s1"})).await;
    assert_eq!(invalid_msg(err), "missing required string param: tool");
}

#[tokio::test]
async fn handler_board_lifecycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let handler = UltraAutonomyHandler::new(tmp.path().to_path_buf());

    let created = run(
        &handler,
        json!({"action": "board_create", "board_id": "demo board", "title": "Demo", "context": "ctx"}),
    )
    .await;
    assert_eq!(created["board"]["id"], "demo board");
    assert_eq!(created["board"]["title"], "Demo");
    assert_eq!(created["board"]["context"], "ctx");
    assert_eq!(
        created["board"]["instructions"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(created["events"][0]["event"], "board.created");
    assert!(tmp
        .path()
        .join("ultra_autonomy")
        .join("boards")
        .join("demo-board.json")
        .is_file());

    let status = run(&handler, json!({"action": "status"})).await;
    assert_eq!(status["boards"], 1);

    let added = run(
        &handler,
        json!({
            "action": "board_add_card",
            "board_id": "demo board",
            "card_id": "c1",
            "title": "First",
            "status": "doing",
            "priority": "p0",
            "labels": [" a ", ""],
            "token_budget": 100,
            "token_used": 5,
            "attachments": ["x.txt"]
        }),
    )
    .await;
    assert_eq!(added["card_id"], "c1");
    assert_eq!(added["plan"]["ready"], json!(["c1"]));
    assert_eq!(added["plan"]["token_budget_remaining"], 95);
    assert_eq!(added["events"][0]["event"], "card.created");

    let second = run(
        &handler,
        json!({"action": "board_add_card", "board_id": "demo board", "title": "Second", "dependencies": ["c1"]}),
    )
    .await;
    assert_eq!(second["card_id"], "card-2");
    assert_eq!(second["plan"]["blocked"][0]["card_id"], "card-2");
    assert_eq!(
        second["plan"]["blocked"][0]["reason"],
        "waiting on dependencies: c1"
    );

    let err = run_err(
        &handler,
        json!({"action": "board_add_card", "board_id": "demo board", "card_id": "c1", "title": "Dup"}),
    )
    .await;
    assert_eq!(invalid_msg(err), "card already exists: c1");

    let err = run_err(
        &handler,
        json!({"action": "board_add_card", "board_id": "demo board", "card_id": "c9"}),
    )
    .await;
    assert_eq!(invalid_msg(err), "missing required string param: title");

    let err = run_err(
        &handler,
        json!({"action": "board_add_card", "board_id": "demo board", "card_id": "c9", "title": "T", "status": "weird"}),
    )
    .await;
    assert_eq!(invalid_msg(err), "unknown card status: weird");

    let err = run_err(
        &handler,
        json!({"action": "board_add_card", "board_id": "missing", "title": "T"}),
    )
    .await;
    assert!(not_found_msg(err).contains("read board"));

    let err = run_err(&handler, json!({"action": "board_add_card", "title": "T"})).await;
    assert_eq!(invalid_msg(err), "missing required string param: board_id");

    let updated = run(
        &handler,
        json!({
            "action": "board_update",
            "board_id": "demo board",
            "card_id": "c1",
            "status": "completed",
            "priority": "low",
            "token_used": 9,
            "comment": "verified"
        }),
    )
    .await;
    assert_eq!(updated["card_id"], "c1");
    assert_eq!(updated["plan"]["done"], json!(["c1"]));
    assert_eq!(updated["plan"]["ready"], json!(["card-2"]));
    assert_eq!(updated["plan"]["token_budget_remaining"], 91);
    assert_eq!(updated["events"][0]["event"], "card.updated");

    let err = run_err(
        &handler,
        json!({"action": "board_update", "board_id": "demo board", "card_id": "nope"}),
    )
    .await;
    assert_eq!(not_found_msg(err), "card not found: nope");

    let err = run_err(
        &handler,
        json!({"action": "board_update", "board_id": "demo board", "card_id": "c1", "priority": "urgent"}),
    )
    .await;
    assert_eq!(invalid_msg(err), "unknown card priority: urgent");

    let plan = run(
        &handler,
        json!({"action": "board_plan", "board_id": "demo board"}),
    )
    .await;
    let c1 = &plan["board"]["cards"][0];
    assert_eq!(c1["status"], "done");
    assert_eq!(c1["priority"], "low");
    assert_eq!(c1["token_used"], 9);
    assert_eq!(c1["labels"], json!(["a"]));
    assert_eq!(c1["attachments"], json!(["x.txt"]));
    assert_eq!(c1["comments"][0]["author"], "hermes");
    assert_eq!(c1["comments"][0]["body"], "verified");

    let unnamed = run(&handler, json!({"action": "board_create"})).await;
    assert!(unnamed["board"]["id"]
        .as_str()
        .expect("id")
        .starts_with("board-"));
    assert_eq!(unnamed["board"]["title"], "Hermes autonomy board");
    assert_eq!(unnamed["board"]["context"], "");

    let status = run(&handler, json!({"action": "status"})).await;
    assert_eq!(status["boards"], 2);
}
