#[test]
fn generated_runtime_peer_id_variants() {
    let config = test_config("");
    assert_eq!(generated_runtime_peer_id(&config, "p-", "abc"), "p-abc");

    let hex = format!("{:x}", Sha256::digest(b"p-abc"));

    let mut config = test_config("");
    config.peer_name = Some("p-abc".to_string());
    config
        .user_peer_aliases
        .insert("a".to_string(), format!("p-abc-{}", &hex[..8]));
    assert_eq!(
        generated_runtime_peer_id(&config, "p-", "abc"),
        format!("p-abc-{}", &hex[..12])
    );

    let mut config = test_config("");
    config.peer_name = Some("p-abc".to_string());
    for len in PEER_ID_HASH_ESCALATION_LENGTHS {
        config
            .user_peer_aliases
            .insert(format!("k{len}"), format!("p-abc-{}", &hex[..*len]));
    }
    assert_eq!(
        generated_runtime_peer_id(&config, "p-", "abc"),
        format!("p-abc-{hex}")
    );
}

#[test]
fn extract_text_result_variants() {
    assert_eq!(
        HonchoMemoryPlugin::extract_text_result(&json!({"result": "r"})),
        Some("r".to_string())
    );
    assert_eq!(
        HonchoMemoryPlugin::extract_text_result(&json!({"context": "c"})),
        Some("c".to_string())
    );
    assert_eq!(
        HonchoMemoryPlugin::extract_text_result(&json!({"answer": "a"})),
        Some("a".to_string())
    );
    assert_eq!(
        HonchoMemoryPlugin::extract_text_result(&json!({"results": [
            {"memory": " m1 "}, {"content": "c2"}, {"text": "t3"}, {"text": "  "}, {"other": 1}
        ]})),
        Some("m1\nc2\nt3".to_string())
    );
    assert_eq!(
        HonchoMemoryPlugin::extract_text_result(&json!({"results": [{"text": " "}]})),
        None
    );
    assert_eq!(HonchoMemoryPlugin::extract_text_result(&json!({})), None);
}

// ---------------------------------------------------------------------------
// Plugin lifecycle
// ---------------------------------------------------------------------------

#[test]
fn default_plugin_and_simple_accessors() {
    let plugin = HonchoMemoryPlugin::default();
    assert_eq!(plugin.name(), "honcho");
    assert!(plugin.config.lock().unwrap().is_none());
    assert_eq!(plugin.recall_mode.lock().unwrap().as_str(), "hybrid");

    let expected = dirs::home_dir()
        .map(|home| vec![home.join(".honcho")])
        .unwrap_or_default();
    assert_eq!(plugin.backup_paths(), expected);

    plugin.on_turn_start(7, "hi");
    assert_eq!(*plugin.turn_count.lock().unwrap(), 7);

    let schema = plugin.get_config_schema().expect("schema");
    let arr = schema.as_array().expect("array");
    assert_eq!(arr.len(), 8);
    assert_eq!(arr[0]["key"], json!("api_key"));
    assert_eq!(arr[3]["default"], json!(DEFAULT_TIMEOUT_SECS));
}

#[test]
fn shutdown_keeps_state() {
    let plugin = plugin_with(test_config("https://x.test"));
    plugin.shutdown();
    assert!(plugin.config.lock().unwrap().is_some());
    assert_eq!(plugin.session_key.lock().unwrap().as_str(), "sess-1");
}

#[test]
fn initialize_disabled_leaves_plugin_inactive() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("honcho.json"), r#"{"enabled": false}"#).unwrap();
    let plugin = HonchoMemoryPlugin::new();
    plugin.initialize("s1", dir.path().to_str().unwrap());
    assert!(plugin.config.lock().unwrap().is_none());
    assert_eq!(plugin.session_key.lock().unwrap().as_str(), "");
    let out = parse(plugin.handle_tool_call("honcho_profile", &json!({})));
    assert_eq!(
        out["error"],
        json!("Honcho is not active for this session.")
    );
}

#[test]
fn prefetch_modes() {
    let plugin = HonchoMemoryPlugin::new();
    *plugin.recall_mode.lock().unwrap() = "tools".to_string();
    *plugin.prefetch_result.lock().unwrap() = "stored".to_string();
    assert_eq!(plugin.prefetch("q", "s"), "");
    assert_eq!(plugin.prefetch_result.lock().unwrap().as_str(), "stored");

    *plugin.recall_mode.lock().unwrap() = "hybrid".to_string();
    assert_eq!(plugin.prefetch("q", "s"), "## Honcho Context\nstored");
    assert_eq!(plugin.prefetch("q", "s"), "");
}

#[test]
fn on_session_switch_updates_state() {
    let plugin = plugin_with(test_config("https://x.test"));
    *plugin.turn_count.lock().unwrap() = 5;
    *plugin.prefetch_result.lock().unwrap() = "old".to_string();
    plugin.on_session_switch("   ", "parent", false);
    assert_eq!(plugin.session_key.lock().unwrap().as_str(), "sess-1");
    assert_eq!(*plugin.turn_count.lock().unwrap(), 5);

    plugin.on_session_switch("  new-sess ", "parent", true);
    assert_eq!(plugin.session_key.lock().unwrap().as_str(), "new-sess");
    assert_eq!(*plugin.turn_count.lock().unwrap(), 0);
    assert!(plugin.prefetch_result.lock().unwrap().is_empty());
}

#[test]
fn queue_prefetch_skips_and_fetches() {
    let plugin = HonchoMemoryPlugin::new();
    plugin.queue_prefetch("question", "s");
    assert!(plugin.prefetch_result.lock().unwrap().is_empty());

    let (url, rx) = spawn_server(vec![(200, r#"{"result":"user likes tea"}"#)]);
    let plugin = plugin_with(test_config(&url));
    *plugin.recall_mode.lock().unwrap() = "tools".to_string();
    plugin.queue_prefetch("question", "sess-1");
    *plugin.recall_mode.lock().unwrap() = "hybrid".to_string();
    plugin.queue_prefetch("    ", "sess-1");
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());

    plugin.queue_prefetch("  what do they like  ", "sess-1");
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/context/query"));
    assert!(req.body.contains("\"query\":\"what do they like\""));
    assert!(wait_for(|| !plugin
        .prefetch_result
        .lock()
        .unwrap()
        .is_empty()));
    assert_eq!(plugin.prefetch("", ""), "## Honcho Context\nuser likes tea");
}

#[test]
fn sync_turn_skips_and_posts_messages() {
    let plugin = HonchoMemoryPlugin::new();
    plugin.sync_turn("hi", "there", "s");
    assert!(plugin.config.lock().unwrap().is_none());

    let (url, rx) = spawn_server(vec![(200, "{}")]);
    let mut config = test_config(&url);
    config
        .endpoints
        .insert("messages".to_string(), "/m/{session}".to_string());
    let plugin = plugin_with(config);
    plugin.sync_turn("hello", "   ", "sess-1");
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());

    plugin.sync_turn("  hello  ", " world ", "sess-1");
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /m/sess-1"));
    assert!(req.body.contains("\"content\":\"hello\""));
    assert!(req.body.contains("\"content\":\"world\""));
    assert!(req.body.contains("\"workspace_id\":\"ws\""));
}

#[test]
fn on_memory_write_mirrors_user_additions() {
    let plugin = HonchoMemoryPlugin::new();
    plugin.on_memory_write("add", "user", "fact");
    assert!(plugin.config.lock().unwrap().is_none());

    let (url, rx) = spawn_server(vec![(200, "{}")]);
    let plugin = plugin_with(test_config(&url));
    plugin.on_memory_write("remove", "user", "fact");
    plugin.on_memory_write("add", "memory", "fact");
    plugin.on_memory_write("add", "user", "   ");
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());

    plugin.on_memory_write("add", "user", "  likes tea  ");
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/conclusions"));
    assert!(req.body.contains("\"conclusion\":\"likes tea\""));
    assert!(req.body.contains("memory_write_hook"));
}

#[test]
fn on_session_end_flushes_session() {
    let plugin = HonchoMemoryPlugin::new();
    plugin.on_session_end(&[]);
    assert!(plugin.session_flush().is_none());

    let (url, rx) = spawn_server(vec![(500, "boom")]);
    let mut config = test_config(&url);
    config.api_key = "secret".to_string();
    let plugin = plugin_with(config);
    plugin.on_session_end(&[]);
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req
        .request_line
        .starts_with("POST /v1/sessions/sess-1/flush"));
    assert!(req.body.contains("\"session_id\":\"sess-1\""));
}

#[test]
fn prepare_session_end_runs_flush_future() {
    let plugin = HonchoMemoryPlugin::new();
    assert!(plugin.prepare_session_end(&[]).is_none());

    let (url, rx) = spawn_server(vec![(500, "nope")]);
    let plugin = plugin_with(test_config(&url));
    let fut = plugin.prepare_session_end(&[]).expect("future");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(fut);
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req
        .request_line
        .starts_with("POST /v1/sessions/sess-1/flush"));
}

// ---------------------------------------------------------------------------
// Tool calls
// ---------------------------------------------------------------------------

#[test]
fn handle_tool_call_unknown_tool() {
    let plugin = plugin_with(test_config("https://x.test"));
    let out = parse(plugin.handle_tool_call("nope", &json!({})));
    assert_eq!(out["error"], json!("Unknown tool: nope"));
}

#[test]
fn handle_tool_call_profile_variants() {
    let (url, rx) = spawn_server(vec![
        (200, r#"{"card":["likes tea","lives in Oslo"]}"#),
        (200, r#"{"card":[]}"#),
        (200, r#"{"ok":true}"#),
        (500, "boom"),
    ]);
    let plugin = plugin_with(test_config(&url));

    let out = parse(plugin.handle_tool_call("honcho_profile", &json!({"peer": "alice"})));
    assert_eq!(out["count"], json!(2));
    assert_eq!(out["result"], json!(["likes tea", "lives in Oslo"]));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("GET /v1/peers/alice/card?"));
    assert!(req.request_line.contains("workspace_id=ws"));
    assert!(req.request_line.contains("peer=alice"));

    let out = parse(plugin.handle_tool_call("honcho_profile", &json!({"peer": "alice"})));
    assert_eq!(out["count"], json!(0));
    assert_eq!(out["result"], json!([]));
    assert!(out.get("hint").is_some());
    rx.recv_timeout(WAIT).expect("request");

    let out = parse(plugin.handle_tool_call(
        "honcho_profile",
        &json!({"peer": "alice", "card": ["new fact"]}),
    ));
    assert_eq!(out["result"], json!({"ok": true}));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/peers/alice/card"));
    assert!(req.body.contains("\"card\":[\"new fact\"]"));

    let out = parse(plugin.handle_tool_call("honcho_profile", &json!({"peer": "alice"})));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Honcho profile failed"));
}

#[test]
fn handle_tool_call_search_variants() {
    let (url, rx) = spawn_server(vec![
        (200, r#"{"results":[{"memory":"m1"},{"content":"c2"}]}"#),
        (200, ""),
        (500, "x"),
    ]);
    let plugin = plugin_with(test_config(&url));

    let out = parse(plugin.handle_tool_call("honcho_search", &json!({"peer": "alice"})));
    assert_eq!(out["error"], json!("Missing required parameter: query"));

    let out = parse(plugin.handle_tool_call(
        "honcho_search",
        &json!({"peer": "alice", "query": "tea", "max_tokens": 5}),
    ));
    assert_eq!(out["result"], json!("m1\nc2"));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/context/search"));
    assert!(req.body.contains("\"max_tokens\":64"));

    let out =
        parse(plugin.handle_tool_call("honcho_search", &json!({"peer": "alice", "query": "tea"})));
    assert_eq!(out["result"], json!("No relevant context found."));
    assert_eq!(out["raw"], json!({}));

    let out =
        parse(plugin.handle_tool_call("honcho_search", &json!({"peer": "alice", "query": "tea"})));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Honcho search failed"));
}

#[test]
fn handle_tool_call_context_variants() {
    let (url, rx) = spawn_server(vec![
        (200, r#"{"answer":"they like tea"}"#),
        (200, r#"{"results":[]}"#),
        (500, "x"),
    ]);
    let plugin = plugin_with(test_config(&url));

    let out = parse(plugin.handle_tool_call("honcho_context", &json!({"peer": "ai"})));
    assert_eq!(out["error"], json!("Missing required parameter: query"));

    let out =
        parse(plugin.handle_tool_call("honcho_context", &json!({"peer": "ai", "query": "what?"})));
    assert_eq!(out["result"], json!("they like tea"));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/context/query"));
    assert!(req.body.contains("\"peer\":\"hermes\""));
    assert!(req.body.contains("\"max_tokens\":800"));

    let out =
        parse(plugin.handle_tool_call("honcho_context", &json!({"peer": "ai", "query": "what?"})));
    assert_eq!(out["result"], json!("No result from Honcho."));

    let out =
        parse(plugin.handle_tool_call("honcho_context", &json!({"peer": "ai", "query": "what?"})));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Honcho context failed"));
}

#[test]
fn handle_tool_call_conclude_variants() {
    let (url, rx) = spawn_server(vec![
        (200, "{}"),
        (500, "x"),
        (200, r#"{"id":"n1"}"#),
        (500, "y"),
    ]);
    let plugin = plugin_with(test_config(&url));

    let out = parse(plugin.handle_tool_call("honcho_conclude", &json!({"peer": "alice"})));
    assert_eq!(out["error"], json!("Provide conclusion or delete_id"));

    let out = parse(plugin.handle_tool_call(
        "honcho_conclude",
        &json!({"peer": "alice", "conclusion": "c", "delete_id": "d"}),
    ));
    assert_eq!(
        out["error"],
        json!("Provide either conclusion or delete_id, not both")
    );

    let out = parse(plugin.handle_tool_call(
        "honcho_conclude",
        &json!({"peer": "alice", "delete_id": " c-1 "}),
    ));
    assert_eq!(out["result"], json!("Conclusion deleted"));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("DELETE /v1/conclusions/c-1?"));
    assert!(req.request_line.contains("peer=alice"));

    let out = parse(plugin.handle_tool_call(
        "honcho_conclude",
        &json!({"peer": "alice", "delete_id": "c-2"}),
    ));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Honcho delete failed"));
    rx.recv_timeout(WAIT).expect("request");

    let out = parse(plugin.handle_tool_call(
        "honcho_conclude",
        &json!({"peer": "alice", "conclusion": " likes tea "}),
    ));
    assert_eq!(out["result"], json!("Conclusion saved."));
    assert_eq!(out["raw"], json!({"id": "n1"}));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.request_line.starts_with("POST /v1/conclusions"));
    assert!(req.body.contains("\"conclusion\":\"likes tea\""));

    let out = parse(plugin.handle_tool_call(
        "honcho_conclude",
        &json!({"peer": "alice", "conclusion": "x"}),
    ));
    assert!(out["error"]
        .as_str()
        .unwrap()
        .starts_with("Honcho conclude failed"));
}

#[test]
fn context_query_returns_parsed_value() {
    let (url, rx) = spawn_server(vec![(200, r#"{"context":"ctx"}"#)]);
    let config = test_config(&url);
    let value = HonchoMemoryPlugin::context_query(&config, "sess-9", "q", 128, "bob")
        .expect("context query");
    assert_eq!(value, json!({"context": "ctx"}));
    let req = rx.recv_timeout(WAIT).expect("request");
    assert!(req.body.contains("\"session_id\":\"sess-9\""));
    assert!(req.body.contains("\"peer\":\"bob\""));
    assert!(req.body.contains("\"max_tokens\":128"));
}
