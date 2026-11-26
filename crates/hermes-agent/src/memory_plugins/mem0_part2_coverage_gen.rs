#[test]
fn list_memories_success_fallback_and_failure() {
    if run_isolated("list_memories_success_fallback_and_failure") {
        return;
    }

    let (base, rx) = mock_server(vec![(200, r#"{"memories":[{"memory":"x"}]}"#)]);
    let res = Mem0MemoryPlugin::list_memories(&cfg(base), 0, 500).expect("ok");
    assert_eq!(res.len(), 1);
    let req = recv(&rx);
    assert!(req.starts_with("GET /memories?"));
    assert!(req.contains("page=1"));
    assert!(req.contains("page_size=200"));
    assert!(req.contains("user_id=u1"));
    assert!(req.contains("agent_id=a1"));

    let (base, rx) = mock_server(vec![
        (404, ""),
        (404, ""),
        (200, r#"{"results":[{"memory":"y"}]}"#),
    ]);
    let res = Mem0MemoryPlugin::list_memories(&cfg(base), 1, 10).expect("ok");
    assert_eq!(res.len(), 1);
    assert!(recv(&rx).starts_with("GET /memories?"));
    assert!(recv(&rx).starts_with("GET /memory?"));
    assert!(recv(&rx).starts_with("POST /memories/search "));

    let err = Mem0MemoryPlugin::list_memories(&cfg(dead_base_url()), 1, 10).unwrap_err();
    assert!(err.contains("fallback search failed"));
}

#[test]
fn add_messages_success_and_failure() {
    if run_isolated("add_messages_success_and_failure") {
        return;
    }

    let (base, rx) = mock_server(vec![(200, r#"{"ok":true}"#)]);
    Mem0MemoryPlugin::add_messages(
        &cfg(base),
        vec![json!({"role":"user","content":"hi"})],
        Some(false),
    )
    .expect("ok");
    let req = recv(&rx);
    assert!(req.starts_with("POST /memories "));
    assert!(req.contains("\"infer\":false"));

    let err = Mem0MemoryPlugin::add_messages(&cfg(dead_base_url()), vec![], None).unwrap_err();
    assert!(err.contains("Mem0 request POST"));
}

#[test]
fn update_and_delete_errors() {
    if run_isolated("update_and_delete_errors") {
        return;
    }

    let c = cfg(dead_base_url());
    assert_eq!(
        Mem0MemoryPlugin::update_memory(&c, "m1", "  ").unwrap_err(),
        "Missing required parameter: text"
    );
    assert!(Mem0MemoryPlugin::update_memory(&c, "m1", "text")
        .unwrap_err()
        .contains("Mem0 request"));
    assert!(Mem0MemoryPlugin::delete_memory(&c, "m1")
        .unwrap_err()
        .contains("Mem0 request"));
}

#[test]
fn prefetch_returns_and_clears_result() {
    if run_isolated("prefetch_returns_and_clears_result") {
        return;
    }

    let plugin = Mem0MemoryPlugin::new();
    assert_eq!(plugin.prefetch("q", "s"), "");
    *plugin.prefetch_result.lock().unwrap() = "- x".to_string();
    assert_eq!(plugin.prefetch("q", "s"), "## Mem0 Memory\n- x");
    assert_eq!(plugin.prefetch("q", "s"), "");
}

#[test]
fn queue_prefetch_early_returns() {
    if run_isolated("queue_prefetch_early_returns") {
        return;
    }

    let plugin = Mem0MemoryPlugin::new();
    plugin.queue_prefetch("   ", "s");
    plugin.queue_prefetch("query", "s");
    assert!(plugin.prefetch_result.lock().unwrap().is_empty());
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 0);
}

#[test]
fn queue_prefetch_success_populates_result() {
    if run_isolated("queue_prefetch_success_populates_result") {
        return;
    }

    let (base, rx) = mock_server(vec![(
        200,
        r#"{"results":[{"memory":"fact one"},{"other":1}]}"#,
    )]);
    let plugin = plugin_with(cfg(base));
    plugin.queue_prefetch("  topic  ", "s");
    assert!(wait_until(|| !plugin
        .prefetch_result
        .lock()
        .unwrap()
        .is_empty()));
    let req = recv(&rx);
    assert!(req.contains("\"query\":\"topic\""));
    assert_eq!(plugin.prefetch("q", "s"), "## Mem0 Memory\n- fact one");
}

#[test]
fn queue_prefetch_failure_records_failure() {
    if run_isolated("queue_prefetch_failure_records_failure") {
        return;
    }

    let plugin = plugin_with(cfg(dead_base_url()));
    plugin.queue_prefetch("topic", "s");
    assert!(wait_until(|| plugin
        .consecutive_failures
        .load(Ordering::Relaxed)
        == 1));
    assert!(plugin.prefetch_result.lock().unwrap().is_empty());
}

#[test]
fn sync_turn_early_returns() {
    if run_isolated("sync_turn_early_returns") {
        return;
    }

    let plugin = Mem0MemoryPlugin::new();
    plugin.sync_turn("  ", "a", "s");
    plugin.sync_turn("u", " ", "s");
    plugin.sync_turn("u", "a", "s");
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 0);
}

#[test]
fn sync_turn_success_sends_truncated_messages() {
    if run_isolated("sync_turn_success_sends_truncated_messages") {
        return;
    }

    let (base, rx) = mock_server(vec![(200, r#"{"ok":true}"#)]);
    let plugin = plugin_with(cfg(base));
    let long_user = "a".repeat(5000);
    plugin.sync_turn(&long_user, "assistant reply", "s");
    let req = recv(&rx);
    assert!(req.starts_with("POST /memories "));
    assert!(req.contains(&"a".repeat(4000)));
    assert!(!req.contains(&"a".repeat(4001)));
    assert!(req.contains("assistant reply"));
}

#[test]
fn sync_turn_failure_records_failure() {
    if run_isolated("sync_turn_failure_records_failure") {
        return;
    }

    let plugin = plugin_with(cfg(dead_base_url()));
    plugin.sync_turn("user", "assistant", "s");
    assert!(wait_until(|| plugin
        .consecutive_failures
        .load(Ordering::Relaxed)
        == 1));
}

#[test]
fn handle_tool_call_without_config_and_missing_params() {
    if run_isolated("handle_tool_call_without_config_and_missing_params") {
        return;
    }

    let plugin = Mem0MemoryPlugin::new();
    let not_init = "Mem0 plugin is not initialized";
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_list", &json!({})))["error"],
        not_init
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_search", &json!({"query":"q"})))["error"],
        not_init
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_search", &json!({})))["error"],
        "Missing required parameter: query"
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_add", &json!({})))["error"],
        "Missing required parameter: content"
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_add", &json!({"content":"c"})))["error"],
        not_init
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_update", &json!({"memory_id":"m"})))["error"],
        not_init
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("mem0_delete", &json!({"memory_id":"m"})))["error"],
        not_init
    );
    assert_eq!(
        parse(&plugin.handle_tool_call("nope", &json!({})))["error"],
        "Unknown tool: nope"
    );
}

#[test]
fn handle_list_success_variants() {
    if run_isolated("handle_list_success_variants") {
        return;
    }

    let (base, rx) = mock_server(vec![(
        200,
        r#"{"results":[{"id":"m1","memory":"likes tea"},{"id":"m2"}]}"#,
    )]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call("mem0_list", &json!({"page":2,"page_size":5})));
    assert_eq!(out["count"], 1);
    assert_eq!(out["results"][0]["id"], "m1");
    assert_eq!(out["results"][0]["memory"], "likes tea");
    assert_eq!(out["page"], 2);
    assert_eq!(out["page_size"], 5);
    let req = recv(&rx);
    assert!(req.contains("page=2"));

    let (base, _rx) = mock_server(vec![(200, r#"[{"memory":"a"},{"content":"b"}]"#)]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call("mem0_profile", &json!({})));
    assert_eq!(out["result"], "a\nb");
    assert_eq!(out["count"], 2);

    let (base, _rx) = mock_server(vec![(200, "[]")]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call("mem0_list", &json!({})));
    assert_eq!(out["result"], "No memories stored yet.");
}

#[test]
fn handle_list_failure_records_failure() {
    if run_isolated("handle_list_failure_records_failure") {
        return;
    }

    let plugin = plugin_with(cfg(dead_base_url()));
    let out = parse(&plugin.handle_tool_call("mem0_list", &json!({})));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Failed to fetch profile:"));
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 1);
}

#[test]
fn handle_search_success_variants() {
    if run_isolated("handle_search_success_variants") {
        return;
    }

    let (base, rx) = mock_server(vec![(
        200,
        r#"{"results":[{"id":"s1","memory":"x","score":0.9},{"id":"s2","content":"y","similarity":0.5},{"id":"s3"}]}"#,
    )]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call(
        "mem0_search",
        &json!({"query":"q","rerank":false,"top_k":100}),
    ));
    assert_eq!(out["count"], 2);
    assert_eq!(out["results"][0]["id"], "s1");
    assert_eq!(out["results"][0]["score"], 0.9);
    assert_eq!(out["results"][1]["memory"], "y");
    assert_eq!(out["results"][1]["score"], 0.5);
    let req = recv(&rx);
    assert!(req.contains("\"rerank\":false"));
    assert!(req.contains("\"top_k\":50"));

    let (base, rx) = mock_server(vec![(200, r#"{"results":[]}"#)]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call("mem0_search", &json!({"query":"q"})));
    assert_eq!(out["result"], "No relevant memories found.");
    let req = recv(&rx);
    assert!(req.contains("\"rerank\":true"));
    assert!(req.contains("\"top_k\":10"));
}

#[test]
fn handle_search_failure_records_failure() {
    if run_isolated("handle_search_failure_records_failure") {
        return;
    }

    let plugin = plugin_with(cfg(dead_base_url()));
    let out = parse(&plugin.handle_tool_call("mem0_search", &json!({"query":"q"})));
    assert!(out["error"].as_str().unwrap().starts_with("Search failed:"));
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 1);
}

#[test]
fn handle_add_success_and_failure() {
    if run_isolated("handle_add_success_and_failure") {
        return;
    }

    let (base, rx) = mock_server(vec![(200, r#"{"ok":true}"#)]);
    let plugin = plugin_with(cfg(base));
    plugin.consecutive_failures.store(2, Ordering::Relaxed);
    let out = parse(&plugin.handle_tool_call("mem0_conclude", &json!({"conclusion":"fact z"})));
    assert_eq!(out["result"], "Fact stored.");
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 0);
    let req = recv(&rx);
    assert!(req.contains("fact z"));
    assert!(req.contains("\"infer\":false"));

    let plugin = plugin_with(cfg(dead_base_url()));
    let out = parse(&plugin.handle_tool_call("mem0_add", &json!({"content":"c"})));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Failed to store:"));
    assert_eq!(plugin.consecutive_failures.load(Ordering::Relaxed), 1);
}

#[test]
fn handle_update_success_and_failure() {
    if run_isolated("handle_update_success_and_failure") {
        return;
    }

    let (base, rx) = mock_server(vec![(200, r#"{"status":"ok"}"#)]);
    let plugin = plugin_with(cfg(base));
    let out =
        parse(&plugin.handle_tool_call("mem0_update", &json!({"memory_id":"m9","text":"new"})));
    assert_eq!(out["result"], "Memory updated.");
    assert_eq!(out["memory_id"], "m9");
    assert_eq!(out["response"]["status"], "ok");
    assert!(recv(&rx).starts_with("PATCH /memories/m9 "));

    let plugin = plugin_with(cfg(dead_base_url()));
    let out = parse(&plugin.handle_tool_call("mem0_update", &json!({"memory_id":"m9"})));
    assert_eq!(
        out["error"],
        "Update failed: Missing required parameter: text"
    );
    let out = parse(&plugin.handle_tool_call("mem0_update", &json!({"memory_id":"m9","text":"t"})));
    assert!(out["error"].as_str().unwrap().starts_with("Update failed:"));
}

#[test]
fn handle_delete_success_and_failure() {
    if run_isolated("handle_delete_success_and_failure") {
        return;
    }

    let (base, rx) = mock_server(vec![(200, r#"{"status":"gone"}"#)]);
    let plugin = plugin_with(cfg(base));
    let out = parse(&plugin.handle_tool_call("mem0_delete", &json!({"memory_id":"d1"})));
    assert_eq!(out["result"], "Memory deleted.");
    assert_eq!(out["memory_id"], "d1");
    assert_eq!(out["response"]["status"], "gone");
    assert!(recv(&rx).starts_with("DELETE /memories/d1 "));

    let plugin = plugin_with(cfg(dead_base_url()));
    let out = parse(&plugin.handle_tool_call("mem0_delete", &json!({"memory_id":"d1"})));
    assert!(out["error"].as_str().unwrap().starts_with("Delete failed:"));
    let out = parse(&plugin.handle_tool_call("mem0_delete", &json!({})));
    assert_eq!(
        out["error"],
        "Delete failed: Missing required parameter: memory_id"
    );
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path
        .split_once("::")
        .map_or(path.as_str(), |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary")) // nosemgrep: rust.lang.security.current-exe.current-exe
        .args([name, "--exact", "--test-threads=1", "--nocapture"])
        .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
        .output()
        .expect("run the isolated test");
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "isolated test {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    true
}
