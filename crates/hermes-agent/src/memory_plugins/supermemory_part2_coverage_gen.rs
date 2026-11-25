#[test]
fn forget_memory_and_ingest() {
    let server = MockServer::start(|_m, p| {
        if p.starts_with("/v3/memories/") {
            (404, "x".to_string())
        } else {
            (200, "{}".to_string())
        }
    });
    let c = cfg(&server.base_url);
    SupermemoryMemoryPlugin::forget_memory(&c, "id1").unwrap();
    let reqs = server.reqs();
    assert_eq!(reqs[0].method, "DELETE");
    assert_eq!(reqs[1].path, "/v1/memories/id1");

    let fail = MockServer::start(|_m, _p| (500, "x".to_string()));
    assert!(SupermemoryMemoryPlugin::forget_memory(&cfg(&fail.base_url), "z").is_err());

    let ok = MockServer::start(|_m, _p| (200, "{}".to_string()));
    SupermemoryMemoryPlugin::ingest_conversation(
        &cfg(&ok.base_url),
        "sid",
        &[json!({"role": "user", "content": "hi"})],
    )
    .unwrap();
    let r = ok.reqs();
    assert_eq!(r[0].path, "/v4/conversations");
    let body = parse(&r[0].body);
    assert_eq!(body["conversationId"], "sid");
    assert_eq!(body["containerTags"][0], "hermes");
}

#[test]
fn extract_profile_lists_shapes() {
    let nested = json!({
        "profile": {"static": ["a", 1, "b"], "dynamic": ["c"]},
        "search_results": {"results": [{"memory": "m"}]}
    });
    let (s, d, r) = SupermemoryMemoryPlugin::extract_profile_lists(&nested);
    assert_eq!(s, vec!["a", "b"]);
    assert_eq!(d, vec!["c"]);
    assert_eq!(r.len(), 1);

    let flat = json!({"static": ["x"], "dynamic": [], "searchResults": [{"a": 1}, {"b": 2}]});
    let (s, d, r) = SupermemoryMemoryPlugin::extract_profile_lists(&flat);
    assert_eq!(s, vec!["x"]);
    assert!(d.is_empty());
    assert_eq!(r.len(), 2);

    let (s, d, r) = SupermemoryMemoryPlugin::extract_profile_lists(&json!({}));
    assert!(s.is_empty() && d.is_empty() && r.is_empty());
}

#[test]
fn handle_tool_call_save() {
    let server = MockServer::start(|_m, _p| (200, "{\"id\":\"n1\"}".to_string()));
    let p = active_plugin(cfg(&server.base_url));
    let v = parse(&p.handle_tool_call("supermemory-save", &json!({"content": "  "})));
    assert_eq!(v["error"], "content is required");

    let v = parse(&p.handle_tool_call(
        "supermemory_store",
        &json!({"content": " remember me ", "metadata": "str", "container_tag": "ct"}),
    ));
    assert_eq!(v["saved"], true);
    assert_eq!(v["preview"], "remember me");
    assert_eq!(v["raw"]["id"], "n1");
    let body = parse(&server.reqs()[0].body);
    assert_eq!(body["metadata"], json!({"sm_source": "hermes"}));
    assert_eq!(body["container_tag"], "ct");

    p.handle_tool_call(
        "supermemory-save",
        &json!({"content": "x", "metadata": {"k": "v"}}),
    );
    let body = parse(&server.reqs()[1].body);
    assert_eq!(body["metadata"]["k"], "v");

    let dead = active_plugin(cfg(&dead_url()));
    let v = parse(&dead.handle_tool_call("supermemory-save", &json!({"content": "x"})));
    assert!(v["error"].as_str().unwrap().contains("failed"));
}

#[test]
fn handle_tool_call_search_profile_unknown() {
    let server = MockServer::start(|_m, p| {
        if p.starts_with("/v3/profile") {
            (
                200,
                "{\"static\":[\"a\",\"b\"],\"dynamic\":[\"c\"],\"search_results\":[{\"memory\":\"m\"}]}"
                    .to_string(),
            )
        } else {
            (
                200,
                "{\"results\":[{\"id\":\"1\"},{\"id\":\"2\"}]}".to_string(),
            )
        }
    });
    let p = active_plugin(cfg(&server.base_url));
    let v = parse(&p.handle_tool_call("supermemory-search", &json!({})));
    assert_eq!(v["error"], "query is required");
    let v = parse(&p.handle_tool_call("supermemory_search", &json!({"query": "q", "limit": 3})));
    assert_eq!(v["count"], 2);
    assert_eq!(parse(&server.reqs()[0].body)["limit"], 3);
    p.handle_tool_call("supermemory-search", &json!({"query": "q"}));
    assert_eq!(parse(&server.reqs()[1].body)["limit"], 10);

    let v = parse(&p.handle_tool_call("supermemory-profile", &json!({"query": "who"})));
    assert_eq!(v["static_count"], 2);
    assert_eq!(v["dynamic_count"], 1);
    assert_eq!(v["search_count"], 1);

    let v = parse(&p.handle_tool_call("nope", &json!({})));
    assert_eq!(v["error"], "Unknown tool: nope");

    let dead = active_plugin(cfg(&dead_url()));
    let v = parse(&dead.handle_tool_call("supermemory-search", &json!({"query": "q"})));
    assert!(v.get("error").is_some());
    let v = parse(&dead.handle_tool_call("supermemory_profile", &json!({})));
    assert!(v.get("error").is_some());

    let noconf = SupermemoryMemoryPlugin::new();
    *noconf.active.lock().unwrap() = true;
    let v = parse(&noconf.handle_tool_call("supermemory-search", &json!({"query": "q"})));
    assert_eq!(v["error"], "Supermemory is not configured");
}

#[test]
fn handle_tool_call_forget() {
    let server = MockServer::start(|m, p| {
        if m == "DELETE" {
            (200, "{}".to_string())
        } else if p == "/v3/search/memories" {
            (
                200,
                "{\"results\":[{\"memory\":\"x\"},{\"id\":\"mem-9\"}]}".to_string(),
            )
        } else {
            (500, "x".to_string())
        }
    });
    let p = active_plugin(cfg(&server.base_url));
    let v = parse(&p.handle_tool_call("supermemory-forget", &json!({})));
    assert_eq!(v["error"], "Provide either id or query");
    let v = parse(&p.handle_tool_call("supermemory-forget", &json!({"id": "abc"})));
    assert_eq!(v, json!({"forgotten": true, "id": "abc"}));
    let v = parse(&p.handle_tool_call("supermemory_forget", &json!({"query": "find"})));
    assert_eq!(v, json!({"forgotten": true, "id": "mem-9"}));
    assert!(server
        .reqs()
        .iter()
        .any(|r| r.method == "DELETE" && r.path == "/v3/memories/mem-9"));

    let noid = MockServer::start(|_m, _p| (200, "{\"results\":[{\"memory\":\"x\"}]}".to_string()));
    let p2 = active_plugin(cfg(&noid.base_url));
    let v = parse(&p2.handle_tool_call("supermemory-forget", &json!({"query": "q"})));
    assert_eq!(v["error"], "No matching memory id found");

    let delfail = MockServer::start(|m, _p| {
        if m == "DELETE" {
            (500, "x".to_string())
        } else {
            (200, "[{\"id\":\"q1\"}]".to_string())
        }
    });
    let p3 = active_plugin(cfg(&delfail.base_url));
    let v = parse(&p3.handle_tool_call("supermemory-forget", &json!({"query": "q"})));
    assert!(v["error"].as_str().unwrap().contains("500"));

    let dead = active_plugin(cfg(&dead_url()));
    let v = parse(&dead.handle_tool_call("supermemory-forget", &json!({"id": "z"})));
    assert!(v.get("error").is_some());
    let v = parse(&dead.handle_tool_call("supermemory-forget", &json!({"query": "z"})));
    assert!(v.get("error").is_some());
}

#[test]
fn prefetch_paths() {
    let server = MockServer::start(|_m, _p| {
        (
            200,
            "{\"profile\":{\"static\":[\"likes tea\"],\"dynamic\":[\"debugging\"]},\"searchResults\":{\"results\":[{\"content\":\"remember x\"}]}}"
                .to_string(),
        )
    });
    let p = active_plugin(cfg(&server.base_url));
    *p.prefetch_result.lock().unwrap() = "queued ctx".to_string();
    assert_eq!(p.prefetch("ignored", "s"), "queued ctx");
    assert!(p.prefetch_result.lock().unwrap().is_empty());

    assert_eq!(p.prefetch("   ", "s"), "");
    assert!(server.reqs().is_empty());

    let long = "a".repeat(300);
    let out = p.prefetch(&long, "s");
    assert!(out.contains("likes tea"));
    assert!(out.contains("## Recent Context\n- debugging"));
    assert!(out.contains("remember x"));
    let body = parse(&server.reqs()[0].body);
    assert_eq!(body["q"].as_str().unwrap().len(), 200);

    let mut c = cfg(&server.base_url);
    c.auto_recall = false;
    let off = active_plugin(c);
    assert_eq!(off.prefetch("q", "s"), "");

    let noconf = SupermemoryMemoryPlugin::new();
    *noconf.active.lock().unwrap() = true;
    assert_eq!(noconf.prefetch("q", "s"), "");

    let dead = active_plugin(cfg(&dead_url()));
    assert_eq!(dead.prefetch("q", "s"), "");
}

#[test]
fn sync_turn_posts_in_background() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let p = active_plugin(cfg(&server.base_url));
    p.sync_turn(" hi there ", " hello back ", "s");
    let reqs = server.wait_for(1);
    assert_eq!(reqs.len(), 1);
    let body = parse(&reqs[0].body);
    let content = body["content"].as_str().unwrap();
    assert!(content.contains("[role: user]\nhi there\n[user:end]"));
    assert!(content.contains("[role: assistant]\nhello back\n[assistant:end]"));
    assert_eq!(body["metadata"]["type"], "conversation_turn");
}

#[test]
fn sync_turn_early_returns() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let inactive = SupermemoryMemoryPlugin::new();
    *inactive.config.lock().unwrap() = Some(cfg(&server.base_url));
    inactive.sync_turn("u", "a", "s");
    let noconf = SupermemoryMemoryPlugin::new();
    *noconf.active.lock().unwrap() = true;
    noconf.sync_turn("u", "a", "s");
    let mut c = cfg(&server.base_url);
    c.auto_capture = false;
    active_plugin(c).sync_turn("u", "a", "s");
    let p = active_plugin(cfg(&server.base_url));
    p.sync_turn("  ", "a", "s");
    p.sync_turn("u", "", "s");
    std::thread::sleep(Duration::from_millis(100));
    assert!(server.reqs().is_empty());
}

#[test]
fn on_session_end_ingests_cleaned_messages() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let p = active_plugin(cfg(&server.base_url));
    p.on_session_end(&[
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": " hi "}),
        json!({"role": "assistant", "content": "hello"}),
        json!({"role": "user", "content": "  "}),
        json!({"content": "no role"}),
    ]);
    let reqs = server.wait_for(1);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/v4/conversations");
    let body = parse(&reqs[0].body);
    assert_eq!(body["conversationId"], "sess-1");
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": "hi"}, {"role": "assistant", "content": "hello"}])
    );
}

#[test]
fn on_session_end_early_returns() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let msgs = [json!({"role": "user", "content": "hi"})];
    let inactive = SupermemoryMemoryPlugin::new();
    *inactive.config.lock().unwrap() = Some(cfg(&server.base_url));
    inactive.on_session_end(&msgs);
    let p = active_plugin(cfg(&server.base_url));
    p.on_session_end(&[]);
    p.on_session_end(&[json!({"role": "system", "content": "x"})]);
    let noconf = SupermemoryMemoryPlugin::new();
    *noconf.active.lock().unwrap() = true;
    noconf.on_session_end(&msgs);
    std::thread::sleep(Duration::from_millis(100));
    assert!(server.reqs().is_empty());
}

#[test]
fn on_memory_write_mirrors_adds() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let p = active_plugin(cfg(&server.base_url));
    p.on_memory_write("add", "memory", "  a fact  ");
    let reqs = server.wait_for(1);
    assert_eq!(reqs.len(), 1);
    let body = parse(&reqs[0].body);
    assert_eq!(body["content"], "a fact");
    assert_eq!(body["metadata"]["source"], "hermes_memory");
    assert_eq!(body["metadata"]["type"], "explicit_memory");
}

#[test]
fn on_memory_write_early_returns() {
    let server = MockServer::start(|_m, _p| (200, "{}".to_string()));
    let p = active_plugin(cfg(&server.base_url));
    p.on_memory_write("remove", "memory", "x");
    p.on_memory_write("add", "memory", "   ");
    let inactive = SupermemoryMemoryPlugin::new();
    *inactive.config.lock().unwrap() = Some(cfg(&server.base_url));
    inactive.on_memory_write("add", "memory", "x");
    let noconf = SupermemoryMemoryPlugin::new();
    *noconf.active.lock().unwrap() = true;
    noconf.on_memory_write("add", "memory", "x");
    std::thread::sleep(Duration::from_millis(100));
    assert!(server.reqs().is_empty());
}
