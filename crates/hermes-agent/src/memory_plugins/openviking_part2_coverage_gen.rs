#[test]
fn sync_turn_falls_back_when_server_errors() {
    let server = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&server.endpoint, "s-fail"));
    plugin.sync_turn("hello there friend", "sure thing", "");
    assert_eq!(turn_count(&plugin), 1);
    assert!(drain(&plugin, "s-fail"));
    assert!(!server.requests().is_empty());
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());
}

#[test]
fn on_session_end_without_state_or_turns_does_nothing() {
    let none = OpenVikingMemoryPlugin::new();
    none.on_session_end(&[]);
    assert!(none.state.lock().unwrap().is_none());

    let server = start_server(200, "{}");
    let plugin = plugin_with_state(make_state(&server.endpoint, "s0"));
    plugin.on_session_end(&[]);
    assert!(server.requests().is_empty());
    assert_eq!(turn_count(&plugin), 0);
}

#[test]
fn on_session_end_commits_and_resets_turn_count() {
    let server = start_server(200, "{}");
    let mut st = make_state(&server.endpoint, "s-end");
    st.turn_count = 2;
    let plugin = plugin_with_state(st);
    plugin.on_session_end(&[]);
    assert_eq!(turn_count(&plugin), 0);
    assert!(server
        .requests()
        .iter()
        .any(|r| r.starts_with("POST /api/v1/sessions/s-end/commit")));
}

#[test]
fn on_session_end_keeps_turn_count_on_commit_failure() {
    let server = start_server(500, "{}");
    let mut st = make_state(&server.endpoint, "s-end");
    st.turn_count = 2;
    let plugin = plugin_with_state(st);
    plugin.on_session_end(&[]);
    assert_eq!(turn_count(&plugin), 2);
}

#[test]
fn on_session_switch_early_returns() {
    let plugin = OpenVikingMemoryPlugin::new();
    *plugin.prefetch.lock().unwrap() = "cached".to_string();
    plugin.on_session_switch("   ", "", false);
    assert_eq!(*plugin.prefetch.lock().unwrap(), "cached");

    plugin.on_session_switch("new", "", false);
    assert_eq!(*plugin.prefetch.lock().unwrap(), "");
    assert!(plugin.state.lock().unwrap().is_none());

    let mut st = make_state(&unreachable_endpoint(), "s1");
    st.turn_count = 3;
    let plugin = plugin_with_state(st);
    plugin.on_session_switch("s1", "", false);
    assert_eq!(turn_count(&plugin), 3);
    assert_eq!(
        plugin
            .state
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.session_id.clone()),
        Some("s1".to_string())
    );
}

#[test]
fn on_memory_write_ignores_non_add_and_missing_state() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    plugin.on_memory_write("replace", "memory", "likes tea");
    plugin.on_memory_write("add", "memory", "   ");
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());

    let none = OpenVikingMemoryPlugin::new();
    none.on_memory_write("add", "memory", "likes tea");
    assert!(none.inflight_writers.lock().unwrap().is_empty());
}

#[test]
fn on_memory_write_posts_content() {
    let server = start_server(200, "{}");
    let plugin = plugin_with_state(make_state(&server.endpoint, "s-mem"));
    plugin.on_memory_write(" ADD ", "memory", "likes green tea");
    assert!(drain(&plugin, "s-mem"));
    let reqs = server.requests();
    let req = reqs
        .iter()
        .find(|r| r.starts_with("POST /api/v1/content/write"))
        .expect("write request");
    assert!(req.contains("likes green tea"));
    assert!(req.contains("viking://user/alice/memories/patterns/mem_"));
    assert!(req
        .to_ascii_lowercase()
        .contains("x-openviking-user: alice"));
}

#[test]
fn handle_tool_call_requires_initialization() {
    let plugin = OpenVikingMemoryPlugin::new();
    let out = parse(&plugin.handle_tool_call("viking_search", &json!({"query": "x"})));
    assert_eq!(out["error"], "OpenViking not initialized");
}

#[test]
fn handle_tool_call_unknown_tool() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("nope", &json!({})));
    assert_eq!(out["error"], "Unknown tool: nope");
}

#[test]
fn viking_search_paths() {
    let body =
        r#"{"result":{"memories":[{"uri":"viking://user/alice/memories/preferences/a.md"}]}}"#;
    let server = start_server(200, body);
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));

    let out = parse(&plugin.handle_tool_call("viking_search", &json!({"query": ""})));
    assert_eq!(out["error"], "query is required");

    let out = parse(&plugin.handle_tool_call(
        "viking_search",
        &json!({"query": "tea", "mode": "deep", "scope": "viking://user/alice", "limit": 3}),
    ));
    assert_eq!(out, parse(body));
    let reqs = server.requests();
    let req = reqs
        .iter()
        .find(|r| r.starts_with("POST /api/v1/search/find"))
        .expect("search request");
    assert!(req.contains("\"mode\":\"deep\""));
    assert!(req.contains("\"target_uri\":\"viking://user/alice\""));
    assert!(req.contains("\"top_k\":3"));

    let bad = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&bad.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_search", &json!({"query": "tea"})));
    assert!(out["error"].as_str().unwrap().starts_with("HTTP 500"));

    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("viking_search", &json!({"query": "tea"})));
    assert!(!out["error"].as_str().unwrap().is_empty());
}

#[test]
fn viking_read_requires_uri() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("viking_read", &json!({})));
    assert_eq!(out["error"], "uri is required");
    let out = parse(&plugin.handle_tool_call("viking_read", &json!({"uris": [], "uri": "  "})));
    assert_eq!(out["error"], "uri is required");
}

#[test]
fn viking_read_single_uri_error_returns_entry() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let uri = "viking://user/alice/memories/preferences/a.md";
    let out = parse(&plugin.handle_tool_call("viking_read", &json!({"uri": format!("  {uri}  ")})));
    assert_eq!(out["uri"], uri);
    assert!(out.get("error").is_some());
}

#[test]
fn viking_read_single_uri_success_returns_result() {
    let server = start_server(200, r#"{"result":"hello content"}"#);
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));
    let out = plugin.handle_tool_call(
        "viking_read",
        &json!({"uri": "viking://user/alice/memories/preferences/a.md"}),
    );
    assert!(out.contains("hello content"));
    assert!(!server.requests().is_empty());
}

#[test]
fn viking_browse_paths() {
    let body = r#"{"result":[{"uri":"viking://user"}]}"#;
    let server = start_server(200, body);
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));
    for (action, path) in [
        ("tree", "/api/v1/fs/tree"),
        ("stat", "/api/v1/fs/stat"),
        ("list", "/api/v1/fs/ls"),
    ] {
        let out = parse(&plugin.handle_tool_call(
            "viking_browse",
            &json!({"action": action, "path": "viking://user"}),
        ));
        assert_eq!(out, parse(body));
        assert!(server
            .requests()
            .iter()
            .any(|r| r.starts_with(&format!("GET {path}?uri="))));
    }

    let bad = start_server(404, "{}");
    let plugin = plugin_with_state(make_state(&bad.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_browse", &json!({})));
    assert!(out["error"].as_str().unwrap().starts_with("HTTP 404"));

    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("viking_browse", &json!({"action": "tree"})));
    assert!(!out["error"].as_str().unwrap().is_empty());
}

#[test]
fn viking_remember_paths() {
    let server = start_server(200, "{}");
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));

    let out = parse(&plugin.handle_tool_call("viking_remember", &json!({"content": ""})));
    assert_eq!(out["error"], "content is required");

    let out = parse(&plugin.handle_tool_call(
        "viking_remember",
        &json!({"content": "Alice works at Acme", "category": "entity"}),
    ));
    assert_eq!(out["status"], "stored");
    let uri = out["uri"].as_str().unwrap().to_string();
    assert!(uri.starts_with("viking://user/alice/memories/entities/mem_"));
    let reqs = server.requests();
    let req = reqs
        .iter()
        .find(|r| r.starts_with("POST /api/v1/content/write"))
        .expect("write request");
    assert!(req.contains(&uri));
    assert!(req.contains("Alice works at Acme"));

    let bad = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&bad.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_remember", &json!({"content": "x"})));
    assert!(out["error"].as_str().unwrap().starts_with("HTTP 500"));

    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("viking_remember", &json!({"content": "x"})));
    assert!(!out["error"].as_str().unwrap().is_empty());
}

#[test]
fn viking_forget_paths() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call(
        "viking_forget",
        &json!({"uri": "viking://resources/doc.md"}),
    ));
    assert_eq!(
        out["error"],
        "viking_forget only deletes user memory file URIs"
    );

    let uri = "viking://user/alice/memories/preferences/mem_1.md";
    let out = parse(&plugin.handle_tool_call("viking_forget", &json!({"uri": uri})));
    assert!(!out["error"].as_str().unwrap().is_empty());

    let body = r#"{"result":{"estimated_deleted_count":1,"semantic_status":"queued","uri":"viking://user/alice/memories/preferences/mem_other.md"}}"#;
    let server = start_server(200, body);
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_forget", &json!({"uri": uri})));
    assert_eq!(out["status"], "deleted");
    assert_eq!(out["estimated_deleted_count"], 1);
    assert_eq!(out["semantic_status"], "queued");
    assert_eq!(
        out["uri"],
        "viking://user/alice/memories/preferences/mem_other.md"
    );
    assert!(out.get("memory_cleanup").is_none());
    let reqs = server.requests();
    let req = reqs
        .iter()
        .find(|r| r.starts_with("DELETE /api/v1/fs?"))
        .expect("delete request");
    assert!(req.contains("recursive=false"));

    let plain = start_server(200, "{}");
    let plugin = plugin_with_state(make_state(&plain.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_forget", &json!({"uri": uri})));
    assert_eq!(out, json!({"status": "deleted", "uri": uri}));

    let bad = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&bad.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call("viking_forget", &json!({"uri": uri})));
    assert!(out["error"].as_str().unwrap().starts_with("HTTP 500"));
}

#[test]
fn viking_add_resource_paths() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s"));
    let out = parse(&plugin.handle_tool_call("viking_add_resource", &json!({"url": ""})));
    assert_eq!(out["error"], "url is required");

    let out = parse(&plugin.handle_tool_call(
        "viking_add_resource",
        &json!({"url": "/definitely/not/a/real/path/openviking_cov_missing.txt"}),
    ));
    assert!(out.get("error").is_some());

    let out = parse(&plugin.handle_tool_call(
        "viking_add_resource",
        &json!({"url": "https://example.com/doc"}),
    ));
    assert!(!out["error"].as_str().unwrap().is_empty());

    let body = r#"{"status":"ok","root_uri":"viking://resources/doc"}"#;
    let server = start_server(200, body);
    let plugin = plugin_with_state(make_state(&server.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call(
        "viking_add_resource",
        &json!({"url": "https://example.com/doc", "reason": "docs"}),
    ));
    assert_eq!(out, parse(body));
    let reqs = server.requests();
    let req = reqs
        .iter()
        .find(|r| r.starts_with("POST /api/v1/resources"))
        .expect("resources request");
    assert!(req.contains("https://example.com/doc"));

    let bad = start_server(500, "{}");
    let plugin = plugin_with_state(make_state(&bad.endpoint, "s"));
    let out = parse(&plugin.handle_tool_call(
        "viking_add_resource",
        &json!({"url": "https://example.com/doc"}),
    ));
    assert!(out["error"].as_str().unwrap().starts_with("HTTP 500"));
}

#[test]
fn shutdown_drains_writers_and_clears_state() {
    let plugin = plugin_with_state(make_state(&unreachable_endpoint(), "s1"));
    let handle: std::thread::JoinHandle<()> = std::thread::spawn(|| {});
    plugin
        .inflight_writers
        .lock()
        .unwrap()
        .insert("s1".to_string(), vec![handle]);
    plugin.shutdown();
    assert!(plugin.state.lock().unwrap().is_none());
    assert!(plugin.inflight_writers.lock().unwrap().is_empty());
}
