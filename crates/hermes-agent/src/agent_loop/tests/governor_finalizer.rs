#[test]
fn google_workspace_finaliser_checks_probe_evidence_before_accepting_the_answer() {
    let request = Message::user("Summarise recent Gmail messages.");
    let terminal_call = |id: &str, command: &str| {
        Message::assistant_with_tool_calls(
            None,
            vec![ToolCall {
                id: id.into(),
                function: hermes_core::FunctionCall {
                    name: "terminal".into(),
                    arguments: json!({"command": command}).to_string(),
                },
                extra_content: None,
            }],
        )
    };

    let setup = "google-workspace/setup.py --check";
    let blocked = format!(
        "GOOGLE_WORKSPACE_USED: no\nGmail is blocked.\ncommand={setup}\nNot authenticated."
    );
    let mut setup_history = vec![request.clone()];
    assert!(finalizer_google_workspace_requires_retry(&setup_history, &blocked, 0));
    setup_history.push(terminal_call("fixture-setup", setup));
    assert!(finalizer_google_workspace_requires_retry(&setup_history, &blocked, 0));
    setup_history.push(Message::tool_result_with_name(
        "fixture-setup",
        "terminal",
        "Not authenticated.",
    ));
    assert!(!finalizer_google_workspace_requires_retry(&setup_history, &blocked, 0));

    let success = "GOOGLE_WORKSPACE_USED: yes\nGmail search and reading were successful.";
    let mut read_history = vec![request];
    assert!(finalizer_google_workspace_requires_retry(&read_history, success, 0));
    read_history.push(terminal_call("fixture-read", "google_api.py gmail list"));
    read_history.push(Message::tool_result_with_name(
        "fixture-read",
        "terminal",
        r#"{"messages":[{"id":"fixture-message","subject":"Weekly summary"}]}"#,
    ));
    assert!(!finalizer_google_workspace_requires_retry(&read_history, success, 0));
}

#[test]
fn memory_sync_keeps_the_words_of_an_acp_image_prompt() {
    let parts = json!([
        {"type": "text", "text": "What does this chart show?"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,aGVsbG8="}},
        {"type": "text", "text": "Answer briefly."}
    ]);
    // As the ACP adapter sends a prompt with an image: its parts as JSON.
    let messages = vec![
        Message::user(format!("__hermes_acp_parts_json__:{parts}")),
        Message::assistant("A rising line."),
    ];
    assert_eq!(
        extract_last_user_assistant(&messages),
        (
            "What does this chart show?\nAnswer briefly.".to_string(),
            "A rising line.".to_string()
        )
    );
    let plain = vec![Message::user("Plain words."), Message::assistant("Reply.")];
    assert_eq!(
        extract_last_user_assistant(&plain),
        ("Plain words.".to_string(), "Reply.".to_string())
    );
}

#[test]
fn test_governor_reduces_budget_under_high_pressure() {
    let mut ctx = ContextManager::default_budget();
    let payload = "x".repeat(((ctx.max_context_chars() as f64) * 0.9) as usize);
    ctx.add_message(Message::user(payload));
    let config = AgentConfig {
        max_tokens: Some(1200),
        ..AgentConfig::default()
    };
    let gov = governor_for_turn(&config, &ctx, 12, None);
    assert!(gov.pressure >= 0.9);
    assert!(gov.max_tokens.unwrap_or(1200) < 1200);
    assert!(gov.tool_concurrency <= 4);
}

#[test]
fn test_governor_reduces_budget_under_latency_degradation() {
    let ctx = ContextManager::default_budget();
    let config = AgentConfig {
        max_tokens: Some(1200),
        ..AgentConfig::default()
    };
    let runtime = GovernorRuntimeState {
        avg_llm_latency_ms: Some(7000.0),
        avg_tool_error_rate: 0.0,
        consecutive_error_turns: 0,
    };
    let gov = governor_for_turn(&config, &ctx, 6, Some(&runtime));
    assert!(gov.latency_degraded);
    assert!(gov.max_tokens.unwrap_or(1200) < 1200);
    assert!(gov.tool_concurrency <= 2);
}

#[test]
fn test_governor_reduces_budget_under_error_degradation() {
    let ctx = ContextManager::default_budget();
    let config = AgentConfig {
        max_tokens: Some(1200),
        ..AgentConfig::default()
    };
    let runtime = GovernorRuntimeState {
        avg_llm_latency_ms: Some(1000.0),
        avg_tool_error_rate: 0.55,
        consecutive_error_turns: 3,
    };
    let gov = governor_for_turn(&config, &ctx, 10, Some(&runtime));
    assert!(gov.error_degraded);
    assert!(gov.max_tokens.unwrap_or(1200) < 1200);
    assert!(gov.tool_concurrency <= 2);
}

#[test]
fn test_tool_loop_guard_trips_on_consecutive_full_failure_turns() {
    assert!(!should_trip_tool_loop_guard_with_config(
        2, 2, 2, true, 3, 1
    ));
    assert!(should_trip_tool_loop_guard_with_config(3, 2, 2, true, 3, 1));
    assert!(!should_trip_tool_loop_guard_with_config(
        3, 2, 2, false, 3, 1
    ));
}

#[test]
fn test_tool_loop_guard_ignores_partial_success_turns() {
    assert!(!should_trip_tool_loop_guard_with_config(
        4, 3, 2, true, 2, 1
    ));
}

#[test]
fn test_looks_like_tool_error_output_detects_json_error_envelope() {
    assert!(looks_like_tool_error_output(
        r#"{"error":"Invalid tool parameters: Missing 'platform' parameter"}"#
    ));
    assert!(looks_like_tool_error_output(
        r#"{"success":false,"message":"failed"}"#
    ));
    assert!(!looks_like_tool_error_output(
        r#"{"success":true,"result":"ok"}"#
    ));
}

#[test]
fn test_looks_like_tool_error_output_detects_text_error_signatures() {
    assert!(looks_like_tool_error_output("error: invalid request"));
    assert!(looks_like_tool_error_output(
        "Invalid tool parameters: Missing 'platform' parameter"
    ));
    assert!(!looks_like_tool_error_output("all good"));
}

#[test]
fn test_redact_json_value_masks_sensitive_fields() {
    let mut payload = serde_json::json!({
        "api_key": "abc",
        "nested": { "token": "def", "safe": "ok" },
        "list": [{"password":"x"}, {"value":"y"}],
        "text": "Authorization: Bearer sk-secretvalue12345"
    });
    redact_json_value(&mut payload);
    assert_eq!(payload["api_key"], "[redacted]");
    assert_eq!(payload["nested"]["token"], "[redacted]");
    assert_eq!(payload["nested"]["safe"], "ok");
    assert_eq!(payload["list"][0]["password"], "[redacted]");
    assert_eq!(payload["text"], "Authorization: Bearer [redacted]");
}

#[test]
fn test_replay_recorder_adds_hash_chain_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let replay_path = tmp.path().join("trace.jsonl");
    let recorder = ReplayRecorder {
        path: Some(replay_path.clone()),
        state: Some(Arc::new(Mutex::new(ReplayState {
            seq: 0,
            prev_hash: short_sha256_hex("seed"),
            trace_root: short_sha256_hex("trace-seed"),
        }))),
    };

    recorder.record("turn_start", serde_json::json!({"token":"abc"}));
    recorder.record("tool_call", serde_json::json!({"cmd":"echo ok"}));

    let body = std::fs::read_to_string(&replay_path).expect("replay file");
    let mut lines = body.lines();
    let first: serde_json::Value =
        serde_json::from_str(lines.next().expect("line1")).expect("json line1");
    let second: serde_json::Value =
        serde_json::from_str(lines.next().expect("line2")).expect("json line2");

    assert_eq!(first["seq"], 1);
    assert_eq!(second["seq"], 2);
    assert!(first.get("trace_id").is_some());
    assert!(second.get("trace_id").is_some());
    assert_eq!(first["payload"]["token"], "[redacted]");
    assert_eq!(second["prev_hash"], first["event_hash"]);
    assert_ne!(first["event_hash"], second["event_hash"]);
}

#[tokio::test]
async fn replay_recorder_respects_explicit_home_and_unavailable_log_directory() {
    const CHILD: &str = "REFORMED_TEST_REPLAY_HOME";
    if std::env::var_os(CHILD).is_none() {
        let root = tempfile::tempdir().unwrap();
        let executable = std::env::current_dir()
            .unwrap()
            .join(std::env::args_os().next().unwrap());
        let mut command = tokio::process::Command::new(executable);
        let (_, test_module) = module_path!().split_once("::").unwrap();
        let test_name = format!(
            "{test_module}::replay_recorder_respects_explicit_home_and_unavailable_log_directory"
        );
        command
            .env_clear()
            .env(CHILD, "1")
            .env("HERMES_REPLAY_ENABLED", " Yes ")
            .env("HERMES_HOME", root.path().join("environment"))
            .current_dir(root.path())
            .kill_on_drop(true)
            .args(["--exact", &test_name]);
        for key in ["LLVM_PROFILE_FILE", "SystemRoot"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(15), command.output())
                .await
                .expect("isolated replay test timed out")
                .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed;"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let root = std::env::current_dir().unwrap();
    let config_home = root.join("profile");
    let config = AgentConfig {
        hermes_home: Some(config_home.to_string_lossy().into_owned()),
        ..AgentConfig::default()
    };
    let recorder = ReplayRecorder::for_session(&config, "Draft review #1");
    let path = config_home.join("logs/replay/Draft_review__1.jsonl");
    assert_eq!(recorder.path.as_deref(), Some(path.as_path()));
    recorder.record("turn_start", json!({"purpose": "draft review"}));
    let entry: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(entry["seq"], 1);
    assert_eq!(entry["prev_hash"], short_sha256_hex("session:Draft_review__1:v1"));
    assert_eq!(entry["payload"]["purpose"], "draft review");
    assert!(!root.join("environment").exists());

    let fallback = ReplayRecorder::for_session(&AgentConfig::default(), " ");
    let fallback_path = fallback.path.unwrap();
    assert_eq!(fallback_path.parent(), Some(root.join("environment/logs/replay").as_path()));
    let fallback_name = fallback_path.file_name().unwrap().to_str().unwrap();
    assert!(fallback_name.starts_with("session-") && fallback_name.ends_with(".jsonl"));

    let blocked = root.join("blocked");
    std::fs::write(&blocked, "ordinary file").unwrap();
    let config = AgentConfig {
        hermes_home: Some(blocked.to_string_lossy().into_owned()),
        ..AgentConfig::default()
    };
    let recorder = ReplayRecorder::for_session(&config, "draft");
    assert!(recorder.path.is_none() && recorder.state.is_none());
    recorder.record("turn_start", json!({"purpose": "draft review"}));
    assert_eq!(std::fs::read_to_string(blocked).unwrap(), "ordinary file");
}

#[test]
fn test_detect_contextlattice_connect_intent() {
    let msgs = vec![Message::user(
        "please confirm and connect to contextlattice, then harden it",
    )];
    assert!(detect_contextlattice_connect_intent(&msgs));

    let msgs = vec![Message::user("explain contextlattice architecture only")];
    assert!(!detect_contextlattice_connect_intent(&msgs));
}

#[test]
fn test_contextlattice_connect_system_hint_emitted() {
    let msgs = vec![Message::user("connect to contextlattice and verify health")];
    let hint = contextlattice_connect_system_hint(&msgs).expect("expected hint");
    assert!(hint.contains("contextlattice_search"));
    assert!(hint.contains("HERMES_CONTEXTLATTICE_INSTRUCTIONS_PATH"));
    assert!(hint.contains("Never use terminal command `contextlattice`"));
}

#[test]
fn test_contextlattice_intelligence_system_hint_requires_tools_and_intent() {
    let msgs = vec![Message::user(
        "perform deep repo audit and objective verification on /tmp/repo",
    )];
    let tools = vec![
        ToolSchema::new("contextlattice_search", "search", JsonSchema::new("object")),
        ToolSchema::new(
            "contextlattice_context_pack",
            "pack",
            JsonSchema::new("object"),
        ),
    ];
    let hint = contextlattice_intelligence_system_hint(&msgs, &tools).expect("expected hint");
    assert!(hint.contains("ContextLattice-first intelligence policy active"));
    assert!(hint.contains("scoped retrieval"));
    assert!(hint.contains("Copy numeric facts verbatim"));
}

#[test]
fn test_contextlattice_intelligence_system_hint_skips_without_tools() {
    let msgs = vec![Message::user(
        "perform deep repo audit and objective verification on /tmp/repo",
    )];
    let tools = vec![ToolSchema::new(
        "terminal",
        "terminal",
        JsonSchema::new("object"),
    )];
    assert!(contextlattice_intelligence_system_hint(&msgs, &tools).is_none());
}

#[test]
fn test_contextlattice_shell_invocation_detector() {
    assert!(is_contextlattice_shell_invocation(
        r#"{"command":"contextlattice"}"#
    ));
    assert!(is_contextlattice_shell_invocation(
        r#"{"command":"contextlattice status"}"#
    ));
    assert!(!is_contextlattice_shell_invocation(
        r#"{"command":"which contextlattice"}"#
    ));
    assert!(!is_contextlattice_shell_invocation(r#"{"command":"ls"}"#));
}

#[test]
fn test_repo_review_tool_profile_keeps_todo_filters_messaging() {
    let _guard = env_test_lock();
    let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "balanced");
    let msgs = vec![Message::user("review repo at /tmp/app and diagnose issue")];
    let mut calls = vec![
        ToolCall {
            id: "a".to_string(),
            function: hermes_core::FunctionCall {
                name: "todo".to_string(),
                arguments: "{}".to_string(),
            },
            extra_content: None,
        },
        ToolCall {
            id: "b".to_string(),
            function: hermes_core::FunctionCall {
                name: "telegram_send".to_string(),
                arguments: r#"{"text":"status"}"#.to_string(),
            },
            extra_content: None,
        },
    ];
    let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
    assert!(note.is_some());
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "todo");
}

#[test]
fn test_repo_review_tool_profile_escape_hatch_disables_filtering() {
    let _guard = env_test_lock();
    let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "focus");
    let msgs = vec![Message::user(
        "review repo at /tmp/app and diagnose issue; allow all tools",
    )];
    let mut calls = vec![ToolCall {
        id: "b".to_string(),
        function: hermes_core::FunctionCall {
            name: "telegram_send".to_string(),
            arguments: r#"{"text":"status"}"#.to_string(),
        },
        extra_content: None,
    }];
    let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
    assert!(note.is_some());
    assert_eq!(calls.len(), 1, "escape hatch should bypass filtering");
}

#[test]
fn test_repo_review_tool_profile_off_mode_disables_filtering() {
    let _guard = env_test_lock();
    let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "off");
    let msgs = vec![Message::user("review repo at /tmp/app and diagnose issue")];
    let mut calls = vec![ToolCall {
        id: "b".to_string(),
        function: hermes_core::FunctionCall {
            name: "telegram_send".to_string(),
            arguments: r#"{"text":"status"}"#.to_string(),
        },
        extra_content: None,
    }];
    let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
    assert!(note.is_none());
    assert_eq!(calls.len(), 1, "off mode should keep all calls");
}

#[test]
fn test_repo_review_discovery_policy_trims_repeated_loops() {
    let _guard = env_test_lock();
    let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE", "enforce");
    let msgs = vec![Message::user(
        "inspect repo /tmp/app and review codebase deeply",
    )];
    let mut state = RepoReviewBudgetState::default();
    let make_calls = || {
        vec![
            ToolCall {
                id: "1".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "2".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "3".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
        ]
    };
    let mut first = make_calls();
    assert!(apply_repo_review_discovery_budget_policy(&mut first, &msgs, &mut state).is_none());
    let mut second = make_calls();
    assert!(
        apply_repo_review_discovery_budget_policy(&mut second, &msgs, &mut state).is_none()
    );
    let mut third = make_calls();
    let note = apply_repo_review_discovery_budget_policy(&mut third, &msgs, &mut state);
    assert!(note.is_some());
    assert!(third.len() < 3);
}

#[test]
fn test_repo_review_discovery_policy_advisory_keeps_calls() {
    let _guard = env_test_lock();
    let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE", "advisory");
    let msgs = vec![Message::user(
        "inspect repo /tmp/app and review codebase deeply",
    )];
    let mut state = RepoReviewBudgetState::default();
    let mut first = vec![
        ToolCall {
            id: "1".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
            },
            extra_content: None,
        },
        ToolCall {
            id: "2".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
            },
            extra_content: None,
        },
        ToolCall {
            id: "3".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
            },
            extra_content: None,
        },
    ];
    assert!(apply_repo_review_discovery_budget_policy(&mut first, &msgs, &mut state).is_none());
    let mut second = first.clone();
    let _ = apply_repo_review_discovery_budget_policy(&mut second, &msgs, &mut state);
    let mut third = first.clone();
    let note = apply_repo_review_discovery_budget_policy(&mut third, &msgs, &mut state);
    assert!(note.is_some());
    assert_eq!(third.len(), 3, "advisory mode must not trim tool calls");
}

#[test]
fn test_repo_review_intent_includes_path_scoped_read_only_research() {
    let msgs = vec![Message::user(
        "Conduct READ-ONLY local research in /tmp/algotraderV2_rust and report back on how to improve profitability.",
    )];
    assert!(detect_repo_review_intent(&msgs));
    assert!(detect_research_evidence_intent(&msgs));
    let hint = exploratory_problem_solving_system_hint(&msgs).expect("research hint");
    assert!(hint.contains("Exploratory problem-solving protocol active"));
}

#[test]
fn test_finalizer_claim_retry_for_research_without_explicit_evidence() {
    let msgs = vec![Message::user(
        "Conduct read-only research in /tmp/algotraderV2_rust and report back with evidence-rich recommendations.",
    )];
    let answer = "Profitability can be improved by 60.2% based on local research.";
    assert!(finalizer_claim_requires_evidence_retry(&msgs, answer, 0));

    let grounded =
        "confidence=medium\nfile=Cargo.toml\ncmd=rg -n profit src\nObserved facts only.";
    assert!(!finalizer_claim_requires_evidence_retry(&msgs, grounded, 0));
}

#[test]
fn test_finalizer_claim_retry_for_missing_evidence_path() {
    let _lock = env_test_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("src")).expect("create src");
    std::fs::write(tmp.path().join("src/lib.rs"), "fn main() {}\n").expect("write file");

    assert!(!assistant_references_missing_evidence_paths_from_base(
        "confidence=high\nfile=src/lib.rs:1\ncmd=rg -n main src/lib.rs",
        tmp.path()
    ));
    assert!(assistant_references_missing_evidence_paths_from_base(
        "confidence=high\nfile=src/missing.rs;cmd=rg -n main src/missing.rs",
        tmp.path()
    ));
}

#[test]
fn test_repo_research_plan_finalizer_requires_workstream_evidence() {
    let msgs = vec![Message::user(
        "Conduct read-only repo research in crates/hermes-agent/src/agent_loop.rs and report how to improve planning.",
    )];
    assert!(detect_exploratory_repo_research_intent(&msgs));

    let shallow = "confidence=medium\nfile=Cargo.toml\ncmd=rg -n planning crates";
    assert!(finalizer_repo_research_plan_requires_retry(
        &msgs, shallow, 0
    ));

    let grounded = "REPO_RESEARCH_PLAN: complete\n\
confidence=medium\n\
- workstream=web status=complete file=Cargo.toml cmd=rg -n web Cargo.toml\n\
- workstream=agent-loop status=complete file=src/agent_loop.rs cmd=rg -n finalizer src/agent_loop.rs\n\
- workstream=tests status=unproven file=Cargo.toml cmd=cargo test -p hermes-agent finalizer";
    assert!(!finalizer_repo_research_plan_requires_retry(
        &msgs, grounded, 0
    ));
}

#[test]
fn test_repo_research_plan_finalizer_accepts_explicit_blocker() {
    let msgs = vec![Message::user(
        "Research the missing repo path /tmp/does-not-exist and report blockers.",
    )];
    let blocked =
        "REPO_RESEARCH_PLAN: blocked\nblocker=path missing\ncmd=rg --files /tmp/does-not-exist";
    assert!(!finalizer_repo_research_plan_requires_retry(
        &msgs, blocked, 0
    ));
}

#[test]
fn test_tool_result_signal_score_rewards_workstream_evidence() {
    let score = tool_result_signal_score(
        "workstream=agent status=complete file=Cargo.toml path=crates/hermes-agent/src/agent_loop.rs cmd=rg -n finalizer crates command=cargo test",
        false,
    );
    assert!(score > 0.75, "score={score}");
}

#[test]
fn test_web_research_finalizer_requires_web_tool_and_urls() {
    let msgs = vec![Message::user(
        "Search the web for Solana trading strategies and cite concrete URLs.",
    )];
    assert!(detect_web_research_intent(&msgs));
    assert!(finalizer_web_research_requires_retry(
        &msgs,
        "WEB_SEARCH_USED: yes\nNo URLs found.",
        0
    ));

    let mut grounded = msgs.clone();
    grounded.push(Message::tool_result_with_name(
        "web1",
        "web_search",
        r#"{"results":[{"url":"https://docs.jito.wtf/lowlatencytxnsend/"}]}"#,
    ));
    let search_only_answer = "WEB_SEARCH_USED: yes\nSOURCE_QUALITY: primary=1 community=1 secondary=0\nObserved:\n- https://docs.jito.wtf/lowlatencytxnsend/\n- https://www.helius.dev/blog/solana-local-fee-markets";
    assert!(finalizer_web_research_requires_retry(
        &grounded,
        search_only_answer,
        0
    ));
    grounded.push(Message::tool_result_with_name(
        "web2",
        "web_extract",
        "Extracted Jito low-latency transaction send docs.",
    ));
    let answer = "WEB_SEARCH_USED: yes\nSOURCE_QUALITY: primary=1 community=1 secondary=0\nObserved:\n- https://docs.jito.wtf/lowlatencytxnsend/\n- https://www.helius.dev/blog/solana-local-fee-markets";
    assert!(!finalizer_web_research_requires_retry(&grounded, answer, 0));
}

#[test]
fn test_web_research_finalizer_requires_source_quality_counts() {
    let mut msgs = vec![Message::user(
        "Search the web for Solana trading strategies and cite concrete URLs.",
    )];
    msgs.push(Message::tool_result_with_name(
        "web1",
        "web_search",
        r#"{"results":[{"url":"https://docs.jito.wtf/lowlatencytxnsend/"}]}"#,
    ));
    msgs.push(Message::tool_result_with_name(
        "web2",
        "web_extract",
        "Extracted source",
    ));
    let missing_quality = "WEB_SEARCH_USED: yes\n- https://docs.jito.wtf/lowlatencytxnsend/\n- https://www.helius.dev/blog/solana-local-fee-markets";
    assert!(finalizer_web_research_requires_retry(
        &msgs,
        missing_quality,
        0
    ));
}

#[test]
fn test_web_research_system_hint_reports_tool_availability() {
    let msgs = vec![Message::user(
        "Do online research across the web and cite URLs.",
    )];
    let tools = vec![ToolSchema::new(
        "web_search",
        "Search the web",
        JsonSchema::new("object"),
    )];
    let hint = web_research_system_hint(&msgs, &tools).expect("web hint");
    assert!(hint.contains("Web research contract active"));
    assert!(hint.contains("web_search"));
    assert!(hint.contains("SOURCE_QUALITY"));
}

#[test]
fn test_task_focus_finalizer_retries_when_explicit_anchors_disappear() {
    let msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    assert!(finalizer_task_focus_requires_retry(
        &msgs,
        "Here is a generic repository analysis with no email evidence.",
        0
    ));
    assert!(!finalizer_task_focus_requires_retry(
        &msgs,
        "Gmail is blocked: not authenticated for sheawinkler@gmail.com.",
        0
    ));
}

#[test]
fn test_finalizer_guards_read_the_words_of_an_acp_image_prompt() {
    // As the ACP adapter sends a prompt with an image: its parts as JSON.
    let parts = serde_json::json!([
        {"type": "text", "text": "describe this picture"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}},
    ]);
    let msgs = vec![Message::user(format!("__hermes_acp_parts_json__:{parts}"))];
    assert_eq!(
        latest_user_content(&msgs).as_deref(),
        Some("describe this picture")
    );
    // The marker and the JSON are not words the reply must repeat, so a
    // plain answer is not asked again.
    assert!(!finalizer_task_focus_requires_retry(
        &msgs,
        "Hello from the stub model.",
        0
    ));
    // An anchor in the text still counts.
    let parts = serde_json::json!([
        {"type": "text", "text": "check crates/hermes-acp in this screenshot"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}},
    ]);
    let msgs = vec![Message::user(format!("__hermes_acp_parts_json__:{parts}"))];
    assert!(finalizer_task_focus_requires_retry(
        &msgs,
        "Hello from the stub model.",
        0
    ));
}

#[test]
fn test_google_workspace_finalizer_retries_absent_skill_claim() {
    let mut msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    msgs.push(Message::tool_result_with_name(
        "skill1",
        "skill_view",
        "# Google Workspace\nGmail, Calendar, Drive.",
    ));

    assert!(finalizer_google_workspace_requires_retry(
        &msgs,
        "No Google Workspace tools exist, so this is blocked.",
        0
    ));
}

#[test]
fn test_google_workspace_finalizer_requires_status_marker() {
    let msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    assert!(finalizer_google_workspace_requires_retry(
        &msgs,
        "Here is an unrelated repo analysis.",
        0
    ));
}

#[test]
fn test_google_workspace_finalizer_accepts_setup_probe_blocker() {
    let mut msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    msgs.push(Message::assistant_with_tool_calls(
        None,
        vec![hermes_core::ToolCall {
            id: "call_setup".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"python3.12 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --check"}"#.to_string(),
            },
            extra_content: None,
        }],
    ));
    msgs.push(Message::tool_result_with_name(
        "call_setup",
        "terminal",
        r#"{"result":"NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json"}"#,
    ));

    assert!(!finalizer_google_workspace_requires_retry(
        &msgs,
        "GOOGLE_WORKSPACE_USED: no\ncmd=python3.12 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --check\nerror=NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json",
        0
    ));
}

#[test]
fn test_google_workspace_finalizer_retries_success_after_auth_blocker() {
    let mut msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    msgs.push(Message::assistant_with_tool_calls(
        None,
        vec![hermes_core::ToolCall {
            id: "call_setup".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"env HERMES_HOME=/Users/me/.hermes-agent-ultra python3.12 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --check"}"#.to_string(),
            },
            extra_content: None,
        }],
    ));
    msgs.push(Message::tool_result_with_name(
        "call_setup",
        "terminal",
        r#"{"result":"NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json"}"#,
    ));

    assert!(finalizer_google_workspace_requires_retry(
        &msgs,
        "GOOGLE_WORKSPACE_USED: yes\n20 important emails were found. Gmail search and reading were successful.",
        0
    ));
}

#[test]
fn test_google_workspace_auth_blocker_guard_blocks_setup_mutation() {
    let mut msgs = vec![Message::user(
        "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
    )];
    msgs.push(Message::tool_result_with_name(
        "call_setup",
        "terminal",
        r#"{"result":"NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json"}"#,
    ));
    let calls = vec![hermes_core::ToolCall {
        id: "write_fake".to_string(),
        function: hermes_core::FunctionCall {
            name: "write_file".to_string(),
            arguments: r#"{"path":"/tmp/simulated_clients.json","content":"{}"}"#.to_string(),
        },
        extra_content: None,
    }];

    assert!(google_workspace_auth_blocker_mutation_guard(&msgs, &calls).is_some());
    let auth_url_calls = vec![hermes_core::ToolCall {
        id: "auth_url".to_string(),
        function: hermes_core::FunctionCall {
            name: "terminal".to_string(),
            arguments: r#"{"command":"env HERMES_HOME=/Users/me/.hermes-agent-ultra python3 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --auth-url --services email"}"#.to_string(),
        },
        extra_content: None,
    }];
    assert!(google_workspace_auth_blocker_mutation_guard(&msgs, &auth_url_calls).is_some());
}

#[test]
fn test_terminal_command_system_hint_warns_against_shell_wrappers() {
    let tools = vec![ToolSchema::new(
        "terminal",
        "Execute command",
        JsonSchema::new("object"),
    )];
    let hint = terminal_command_system_hint(&tools).expect("terminal hint");
    assert!(hint.contains("bash -lc"));
    assert!(hint.contains("direct commands"));
}

#[test]
fn test_finalizer_output_quality_retry_detects_placeholders() {
    let templated =
        "**Title:** Example\n**Authors:** pack of authors\n(Full text available at [URL](URL))";
    assert!(finalizer_output_quality_requires_retry(templated, 0));
}

#[test]
fn test_finalizer_output_quality_retry_detects_fake_attachments() {
    let answer = "The full evidence is attached separately; proposed calibration: redacted.";
    assert!(finalizer_output_quality_requires_retry(answer, 0));
    assert!(finalizer_output_quality_requires_retry(
        r#"{"name":"terminal","arguments":{}}</tool_call>"#,
        0
    ));
}

#[test]
fn test_finalizer_output_quality_retry_detects_duplicate_lines() {
    let duplicated =
        "- **Title:** Bayesian Learning for Dive State Prediction and Management\n\
        - **Title:** Bayesian Learning for Dive State Prediction and Management\n\
        - **Title:** Bayesian Learning for Dive State Prediction and Management\n\
        - **Title:** Bayesian Learning for Dive State Prediction and Management";
    assert!(finalizer_output_quality_requires_retry(duplicated, 0));
    assert!(!finalizer_output_quality_requires_retry(duplicated, 2));
}

#[test]
fn test_finalizer_action_execution_retry_detects_intent_narration() {
    let msgs = vec![Message::user(
        "proceed with deep repo review for /tmp/app and implement patches",
    )];
    assert!(finalizer_action_execution_requires_retry(
        &msgs,
        "I will proceed now and report back shortly.",
        0
    ));
    assert!(!finalizer_action_execution_requires_retry(
        &msgs,
        "I will proceed now and report back shortly.",
        2
    ));
}

#[test]
fn test_finalizer_action_execution_retry_skips_when_evidence_present() {
    let msgs = vec![Message::user(
        "proceed with deep repo review for /tmp/app and implement patches",
    )];
    assert!(!finalizer_action_execution_requires_retry(
        &msgs,
        "cmd=rg -n TODO src\nfile=/tmp/app/src/main.rs\nobjective_state=advancing",
        0
    ));
}

#[test]
fn test_objective_guard_requires_sections_for_trading_objective() {
    let msgs = vec![
        Message::system("[SESSION_OBJECTIVE] Exponentiate Solana wallet via trading."),
        Message::user("review repo /tmp/algotraderv2_rust and produce patch plan"),
    ];
    let (active, needs_analytics, deep_audit_required) = objective_guard_policy(&msgs);
    assert!(active);
    assert!(needs_analytics);
    assert!(!deep_audit_required);
    assert!(!objective_guard_satisfied("plain response", true, false));
    assert!(objective_guard_satisfied(
        "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.42 SOL",
        true,
        false
    ));
}

#[test]
fn test_deep_objective_guard_requires_deep_audit_section() {
    let msgs = vec![
        Message::system("[SESSION_OBJECTIVE] Exponentiate Solana wallet via trading."),
        Message::user(
            "deep end-to-end review repo /tmp/algotraderv2_rust and produce complete patch plan",
        ),
    ];
    let (active, needs_analytics, deep_audit_required) = objective_guard_policy(&msgs);
    assert!(active);
    assert!(needs_analytics);
    assert!(deep_audit_required);

    let shallow = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.42 SOL";
    assert!(!objective_guard_satisfied(shallow, true, true));

    let numeric_only = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- verified_files=8\n- commands_run=5\n- unknowns=1\n- blockers=none";
    assert!(!objective_guard_satisfied(numeric_only, true, true));

    let deep = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- workstream=ingestion status=complete evidence(file=/tmp/a.rs cmd=rg -n ingest src)\n- workstream=strategy status=complete evidence(file=/tmp/b.rs cmd=sed -n 1,220p src/strategy.rs)\n- workstream=execution status=complete evidence(file=/tmp/c.rs cmd=cargo test -p hermes-agent objective_guard)\n- file=/tmp/a.rs\n- file=/tmp/b.rs\n- file=/tmp/c.rs\n- file=/tmp/d.rs\n- file=/tmp/e.rs\n- cmd=rg -n objective src\n- cmd=sed -n 1,220p src/main.rs\n- cmd=cargo test -p hermes-agent objective_guard\n- unknowns=1\n- blockers=none";
    assert!(objective_guard_satisfied(deep, true, true));
}

#[test]
fn test_deep_objective_retry_prompt_contains_audit_requirements() {
    let prompt = objective_guard_retry_prompt(true, true);
    assert!(prompt.contains(OBJECTIVE_DEEP_AUDIT_TAG));
    assert!(prompt.contains("file=<verified_path_1>"));
    assert!(prompt.contains("cmd=<command_1>"));
    assert!(prompt.contains("workstream=<name> status=<complete|blocked|unproven>"));
}

#[test]
fn test_deep_objective_scope_complete_rejects_non_complete_streams() {
    let text = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- workstream=ingestion status=complete evidence(file=/tmp/a.rs cmd=rg -n ingest src)\n- workstream=strategy status=blocked evidence(file=/tmp/b.rs cmd=rg -n strategy src)\n- workstream=execution status=complete evidence(file=/tmp/c.rs cmd=cargo test)\n- file=/tmp/a.rs\n- file=/tmp/b.rs\n- file=/tmp/c.rs\n- file=/tmp/d.rs\n- file=/tmp/e.rs\n- cmd=rg -n objective src\n- cmd=sed -n 1,220p src/main.rs\n- cmd=cargo test -p hermes-agent objective_guard\n- unknowns=1\n- blockers=rpc unavailable";
    assert!(!objective_guard_satisfied(text, true, true));
}

#[test]
fn test_coerce_textual_tool_calls_extracts_and_cleans_message() {
    let msg = Message::assistant(
        "Proceeding with discovery now.\n<tool_call name=\"skill_view\">\n<argument name=\"skill\">contextlattice-master-router</argument>\n</tool_call>",
    );
    let (coerced, calls, parsed_textual) = AgentLoop::coerce_textual_tool_calls(msg);
    assert!(parsed_textual);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "skill_view");
    assert_eq!(
        coerced.content.as_deref(),
        Some("Proceeding with discovery now.")
    );
}

#[test]
fn test_coerce_textual_tool_calls_keeps_declared_calls() {
    let msg = Message::assistant_with_tool_calls(
        Some("Running tool.".to_string()),
        vec![ToolCall {
            id: "id1".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"pwd"}"#.to_string(),
            },
            extra_content: None,
        }],
    );
    let (coerced, calls, parsed_textual) = AgentLoop::coerce_textual_tool_calls(msg);
    assert!(!parsed_textual);
    assert_eq!(calls.len(), 1);
    assert_eq!(coerced.tool_calls.as_ref().map(|v| v.len()), Some(1));
    assert_eq!(coerced.content.as_deref(), Some("Running tool."));
}

#[test]
fn test_extract_objective_state_marker_prefers_explicit_marker() {
    let text = "ANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.12 SOL";
    assert_eq!(extract_objective_state_marker(text), "advancing");
    let colon_text = "ANALYTICS_VERIFIED:\n- objective_state: regressing metric=-0.30 SOL";
    assert_eq!(extract_objective_state_marker(colon_text), "regressing");
}

#[test]
fn test_extract_marker_values_collects_unique_paths_and_cmds() {
    let text = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nDEEP_AUDIT_VERIFIED:\n- cmd=rg -n objective src\n- cmd=cargo test -p hermes-agent objective_guard";
    let files = extract_marker_values(text, "path=", 8);
    let cmds = extract_marker_values(text, "cmd=", 8);
    assert_eq!(
        files,
        vec!["/tmp/a.rs".to_string(), "/tmp/b.rs".to_string()]
    );
    assert_eq!(cmds, vec!["rg".to_string(), "cargo".to_string()]);
}

struct FinalizerReplyProvider {
    calls: Arc<std::sync::atomic::AtomicU32>,
    first_reply: Option<&'static str>,
    reply: String,
}

impl FinalizerReplyProvider {
    fn next_reply(&self) -> &str {
        let attempt = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let (0, Some(reply)) = (attempt, self.first_reply) {
            reply
        } else {
            &self.reply
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for FinalizerReplyProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        Ok(hermes_core::LlmResponse {
            message: Message::assistant(self.next_reply()),
            usage: None,
            model: "dummy".into(),
            finish_reason: Some("stop".into()),
        })
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        let chunk = StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: Some(self.next_reply().to_owned()),
                tool_calls: None,
                extra: None,
            }),
            finish_reason: Some("stop".into()),
            usage: None,
        };
        futures::stream::iter(vec![Ok(chunk)]).boxed()
    }
}

/// A reply that drops the user's explicit anchor (`U1-abc`) is re-asked only when the turn
/// offers tools to gather evidence; without tools the first answer stands, on both loops. The
/// output-quality rewrite (a placeholder reply) needs no tools and is re-asked either way.
#[test]
fn test_finalizer_retries_need_tools() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let calls_for = |reply: &'static str, tools: Vec<ToolSchema>, stream: bool| {
        let calls = Arc::new(AtomicU32::new(0));
        let provider = Arc::new(FinalizerReplyProvider {
            calls: calls.clone(),
            first_reply: None,
            reply: reply.to_owned(),
        });
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            provider,
        );
        let messages = vec![Message::user("U1-abc hi remote")];
        let result = if stream {
            rt.block_on(agent.run_stream(messages, Some(tools), Some(Box::new(|_| {}))))
        } else {
            rt.block_on(agent.run(messages, Some(tools)))
        };
        result.expect("agent run should succeed");
        calls.load(Ordering::SeqCst)
    };
    let echo = || vec![ToolSchema::new("echo", "Echo", JsonSchema::new("object"))];

    for stream in [false, true] {
        assert_eq!(calls_for("remote hello", Vec::new(), stream), 1);
        assert!(calls_for("remote hello", echo(), stream) > 1);
        assert!(calls_for("U1-abc: see [URL](URL)", Vec::new(), stream) > 1);
    }
}

#[tokio::test]
async fn both_run_modes_reissue_final_answers_with_the_required_contract() {
    let cases = [
        (
            "Verify the fixture session is ready.",
            "I will check the fixture session shortly.",
            "Execution contract",
            "BLOCKED: read_fixture is unavailable. Next probe: check the fixture registry.",
        ),
        (
            "Search the web for the estuary water level and cite sources.",
            "The estuary is calm.",
            "Web research contract failed",
            "WEB_SEARCH_USED: no\nBLOCKED: web search is unavailable.",
        ),
        (
            "Research the missing repo path /fixtures/missing-notes and report blockers.",
            "The repo path /fixtures/missing-notes needs further review.",
            "Repo research planning contract failed",
            "REPO_RESEARCH_PLAN: blocked\nblocker=path missing\ncmd=list /fixtures/missing-notes",
        ),
    ];
    for stream in [false, true] {
        for (prompt, first_reply, contract, reply) in cases {
            let mut config = AgentConfig {
                skip_context_files: true,
                skip_memory: true,
                ..AgentConfig::default()
            };
            let _home = isolate_route_learning_home(&mut config);
            let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
            let agent = AgentLoop::new(
                config,
                Arc::new(ToolRegistry::new()),
                Arc::new(FinalizerReplyProvider {
                    calls: calls.clone(),
                    first_reply: Some(first_reply),
                    reply: reply.to_owned(),
                }),
            );
            let schema = ToolSchema::new("read_fixture", "Read fixture notes", JsonSchema::new("object"));
            let messages = vec![Message::user(prompt)];
            let tools = Some(vec![schema]);
            let result = if stream {
                agent.run_stream(messages, tools, Some(Box::new(|_| {}))).await
            } else {
                agent.run(messages, tools).await
            }
            .unwrap_or_else(|err| panic!("finalizer failed for {prompt:?} (stream={stream}): {err}"));
            assert!(result.finished_naturally, "{prompt:?} (stream={stream})");
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2, "{prompt:?} (stream={stream})");
            assert!(result.messages.iter().any(|message| {
                message.role == MessageRole::System
                    && message.content.as_deref().is_some_and(|text| text.contains(contract))
            }), "missing retry contract for {prompt:?} (stream={stream})");
            assert_eq!(
                result.messages.last().and_then(|message| message.content.as_deref()),
                Some(reply),
                "{prompt:?} (stream={stream})"
            );
        }
    }
}

#[tokio::test]
async fn workspace_finalizer_retries_unsupported_mail_claims_in_both_loops() {
    use std::sync::atomic::{AtomicU32, Ordering};

    for stream in [false, true] {
        let mut config = AgentConfig {
            skip_context_files: true,
            skip_memory: true,
            ..AgentConfig::default()
        };
        let home = isolate_route_learning_home(&mut config);
        let scripts = home
            .path()
            .join("skills/productivity/google-workspace/scripts");
        let setup = scripts.join("setup.py");
        let api = scripts.join("google_api.py");
        let setup_command = format!(
            "env HERMES_HOME={} python3.12 {} --check",
            home.path().display(),
            setup.display()
        );
        let error = "NOT_AUTHENTICATED: workspace fixture has no token";
        let reply = format!("GOOGLE_WORKSPACE_USED: no\nGmail is blocked.\ncmd={setup_command}\nerror={error}");
        let calls = Arc::new(AtomicU32::new(0));
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(FinalizerReplyProvider {
                calls: calls.clone(),
                first_reply: Some(
                    "GOOGLE_WORKSPACE_USED: yes\nI found an email titled Weekly Notes.",
                ),
                reply: reply.clone(),
            }),
        );
        let messages = vec![
            Message::user("Summarise recent Gmail messages."),
            Message::assistant_with_tool_calls(
                None,
                vec![ToolCall {
                    id: "workspace-setup".into(),
                    function: hermes_core::FunctionCall {
                        name: "terminal".into(),
                        arguments: json!({"command": setup_command}).to_string(),
                    },
                    extra_content: None,
                }],
            ),
            Message::tool_result_with_name("workspace-setup", "terminal", error),
        ];
        let tools = vec![
            ToolSchema::new("skill_view", "Read a skill", JsonSchema::new("object")),
            ToolSchema::new("terminal", "Run commands", JsonSchema::new("object")),
        ];
        let delivered = Arc::new(AtomicU32::new(0));
        let result = if stream {
            let chunks = delivered.clone();
            agent
                .run_stream(
                    messages,
                    Some(tools),
                    Some(Box::new(move |chunk| {
                        if chunk.delta.is_some_and(|delta| delta.content.is_some()) {
                            chunks.fetch_add(1, Ordering::SeqCst);
                        }
                    })),
                )
                .await
        } else {
            agent.run(messages, Some(tools)).await
        }
        .expect("workspace finalizer completes the fixture turn");
        assert!(result.finished_naturally, "stream={stream}");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "stream={stream}");
        assert_eq!(delivered.load(Ordering::SeqCst) > 0, stream);

        let system_text = |marker: &str| {
            result
                .messages
                .iter()
                .filter(|message| message.role == MessageRole::System)
                .filter_map(|message| message.content.as_deref())
                .find(|text| text.contains(marker))
        };
        let initial = system_text("Google Workspace/Gmail execution contract active")
            .expect("initial Workspace execution contract");
        assert!(initial.contains("skills_tools=true, terminal=true"));
        let retry = system_text("Google Workspace evidence contract failed")
            .expect("Workspace evidence retry contract");
        for text in [initial, retry] {
            assert!(text.contains(&setup_command), "{text}");
            assert!(
                text.contains(&format!("{} gmail search", api.display())),
                "{text}"
            );
            assert!(text.contains("GOOGLE_WORKSPACE_USED: no"), "{text}");
        }
        assert_eq!(
            result
                .messages
                .last()
                .and_then(|message| message.content.as_deref()),
            Some(reply.as_str()),
            "stream={stream}"
        );
    }
}

#[tokio::test]
async fn objective_turns_record_decisions_and_bound_the_evaluation_history() {
    use serde_json::json;

    let home = tempfile::tempdir().unwrap();
    let alpha = home.path().join("alpha");
    std::fs::create_dir(&alpha).unwrap();
    let trend = alpha.join("objective_eval_trend.json");
    let ledger = alpha.join("objective_runtime_ledger.jsonl");
    std::fs::write(&trend, r#"{"samples":false}"#).unwrap();
    let objective_id = "obj-c39e3c1ce183";
    let config = AgentConfig {
        hermes_home: Some(home.path().display().to_string()),
        stored_system_prompt: Some(format!("{SESSION_OBJECTIVE_PREFIX}Review fixture progress")),
        skip_context_files: true,
        skip_memory: true,
        ..AgentConfig::default()
    };
    let cases = [
        (false, "advancing", "promote", 1.0, 2),
        (true, "regressing", "investigate", 0.0, 2),
        (false, "unproven", "collect-more-evidence", 0.25, 1),
        (true, "flat", "monitor", 0.5, 1),
    ];
    let mut expected_entries = Vec::new();
    let mut expected_samples = Vec::new();
    for (stream, state, decision, score, turns) in cases {
        let reply = format!(
            "PATCH_VERIFIED:\npath=fixture.md exists_now=true\nANALYTICS_VERIFIED:\nobjective_state={state}\ncmd=fixture-check"
        );
        let first_reply = (turns == 2).then_some("The fixture shows progress.");
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent = AgentLoop::new(
            config.clone(),
            Arc::new(ToolRegistry::new()),
            Arc::new(FinalizerReplyProvider {
                calls: calls.clone(),
                first_reply,
                reply,
            }),
        );
        let messages = vec![Message::user("What does the fixture show?")];
        let result = if stream {
            agent
                .run_stream(messages, Some(Vec::new()), Some(Box::new(|_| {})))
                .await
        } else {
            agent.run(messages, Some(Vec::new())).await
        }
        .unwrap();
        assert!(result.finished_naturally);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), turns);
        assert_eq!(result.total_turns, turns);
        if turns == 2 {
            assert!(result.messages.iter().any(|message| {
                message.role == MessageRole::System
                    && message
                        .content
                        .as_deref()
                        .is_some_and(|text| text.contains("Objective guard check failed"))
            }));
        }

        expected_entries.push((state, decision, turns));
        let text = std::fs::read_to_string(&ledger).unwrap();
        let records: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), expected_entries.len());
        for (record, (state, decision, turns)) in records.iter().zip(&expected_entries) {
            assert_eq!(record["objective_id"], objective_id);
            assert_eq!(record["objective_state"], *state);
            assert_eq!(record["decision"], *decision);
            assert_eq!(record["turns"], *turns);
            assert_eq!(record["evidence_files"], json!(["fixture.md"]));
            assert_eq!(record["evidence_commands"], json!(["fixture-check"]));
        }

        let mut data: Value = serde_json::from_slice(&std::fs::read(&trend).unwrap()).unwrap();
        let samples = data["samples"].as_array().unwrap();
        let sample = samples.last().unwrap();
        assert_eq!(sample["objective_id"], objective_id);
        assert_eq!(sample["objective_state"], state);
        assert_eq!(sample["score"], score);
        assert_eq!(sample["note"], format!("decision={decision} turns={turns}"));
        expected_samples.push(sample.clone());
        if expected_samples.len() > 512 {
            expected_samples.remove(0);
        }
        assert_eq!(*samples, expected_samples);
        if state == "advancing" {
            expected_samples.splice(0..0, (0..511).map(|index| json!({"objective_id": index})));
            data["samples"] = json!(expected_samples);
            std::fs::write(&trend, data.to_string()).unwrap();
        }
    }
}
