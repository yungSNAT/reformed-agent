#[test]
fn feedback_tool_paths() {
    let (_tmp, plugin) = init_plugin();
    let id = plugin.add_fact("Feedback target", "general", "").unwrap();

    let out = call(
        &plugin,
        "fact_feedback",
        json!({"action": "unhelpful", "fact_id": id}),
    );
    assert!(approx(out["old_trust"].as_f64().unwrap(), 0.5));
    assert!(approx(out["new_trust"].as_f64().unwrap(), 0.4));
    assert_eq!(out["helpful_count"].as_i64(), Some(0));

    let out = call(
        &plugin,
        "fact_feedback",
        json!({"action": "helpful", "fact_id": id}),
    );
    assert!(approx(out["new_trust"].as_f64().unwrap(), 0.45));
    assert_eq!(out["helpful_count"].as_i64(), Some(1));

    let out = call(
        &plugin,
        "fact_feedback",
        json!({"action": "helpful", "fact_id": 9999}),
    );
    assert_eq!(out["error"].as_str(), Some("fact_id 9999 not found"));

    let out = call(&plugin, "fact_feedback", json!({"fact_id": id}));
    assert_eq!(
        out["error"].as_str(),
        Some("Missing required argument: action")
    );
    let out = call(&plugin, "fact_feedback", json!({"action": "helpful"}));
    assert_eq!(
        out["error"].as_str(),
        Some("Missing required argument: fact_id")
    );
}

#[test]
fn system_prompt_and_prefetch() {
    let (_tmp, plugin) = init_plugin();
    assert!(plugin.system_prompt_block().contains("Empty fact store"));
    assert_eq!(plugin.prefetch("   ", "s"), "");
    assert_eq!(plugin.prefetch("nothingmatches", "s"), "");

    plugin
        .add_fact("User prefers dark mode", "user_pref", "")
        .unwrap();
    assert!(plugin
        .system_prompt_block()
        .contains("Active. 1 facts stored"));
    let out = plugin.prefetch("dark mode", "s");
    assert_eq!(out, "## Holographic Memory\n- [0.5] User prefers dark mode");
}

#[test]
fn sync_turn_is_noop_and_unknown_tool() {
    let (_tmp, plugin) = init_plugin();
    plugin.sync_turn("Remember John Doe", "Sure", "s");
    assert_eq!(plugin.fact_count(), 0);
    let out = call(&plugin, "bogus_tool", json!({}));
    assert_eq!(out["error"].as_str(), Some("Unknown tool: bogus_tool"));
}

#[test]
fn on_memory_write_adds_facts_with_category() {
    let (_tmp, plugin) = init_plugin();
    plugin.on_memory_write("add", "user", "Likes tea");
    plugin.on_memory_write("add", "memory", "Uses vim");
    plugin.on_memory_write("remove", "user", "Ignored");
    plugin.on_memory_write("add", "user", "");
    assert_eq!(plugin.fact_count(), 2);

    let out = call(
        &plugin,
        "fact_store",
        json!({"action": "list", "category": "user_pref"}),
    );
    assert_eq!(out["count"].as_u64(), Some(1));
    assert_eq!(out["facts"][0]["content"].as_str(), Some("Likes tea"));

    let out = call(
        &plugin,
        "fact_store",
        json!({"action": "list", "category": "general"}),
    );
    assert_eq!(out["count"].as_u64(), Some(1));
    assert_eq!(out["facts"][0]["content"].as_str(), Some("Uses vim"));
}

#[test]
fn config_schema_and_save() {
    let plugin = HolographicMemoryPlugin::new();
    let schema = plugin.get_config_schema().unwrap();
    let arr = schema.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["key"].as_str(), Some("db_path"));
    assert_eq!(arr[1]["default"].as_str(), Some("0.5"));
    assert_eq!(arr[2]["key"].as_str(), Some("min_trust_threshold"));
    assert!(plugin.save_config(&json!({"x": 1})).is_ok());
}

#[test]
fn fact_store_missing_and_unknown_arguments() {
    let (_tmp, plugin) = init_plugin();
    let cases = [
        (json!({}), "Missing required argument: action"),
        (
            json!({"action": "add"}),
            "Missing required argument: content",
        ),
        (
            json!({"action": "search"}),
            "Missing required argument: query",
        ),
        (
            json!({"action": "update"}),
            "Missing required argument: fact_id",
        ),
        (
            json!({"action": "remove"}),
            "Missing required argument: fact_id",
        ),
        (json!({"action": "explode"}), "Unknown action: explode"),
    ];
    for (args, expected) in cases {
        let out = call(&plugin, "fact_store", args);
        assert_eq!(out["error"].as_str(), Some(expected));
    }
}

#[test]
fn fact_store_update_paths() {
    let (_tmp, plugin) = init_plugin();
    // Remove the FTS external-content machinery so content updates are plain SQL updates.
    plugin
        .with_conn(|c| {
            c.execute_batch(
                "DROP TRIGGER IF EXISTS facts_au;
                 DROP TRIGGER IF EXISTS facts_ai;
                 DROP TRIGGER IF EXISTS facts_ad;
                 DROP TABLE IF EXISTS facts_fts;",
            )
            .unwrap()
        })
        .unwrap();
    *plugin.fts_enabled.lock().unwrap() = false;

    let added = call(
        &plugin,
        "fact_store",
        json!({"action": "add", "content": "Original content"}),
    );
    assert_eq!(added["status"].as_str(), Some("added"));
    let id = added["fact_id"].as_i64().unwrap();

    let out = call(
        &plugin,
        "fact_store",
        json!({
            "action": "update",
            "fact_id": id,
            "content": "Changed content",
            "tags": "t1",
            "category": "project",
            "trust_delta": 0.3
        }),
    );
    assert_eq!(out["updated"].as_bool(), Some(true), "{out}");

    let list = call(
        &plugin,
        "fact_store",
        json!({"action": "list", "category": "project"}),
    );
    assert_eq!(list["count"].as_u64(), Some(1));
    let fact = &list["facts"][0];
    assert_eq!(fact["content"].as_str(), Some("Changed content"));
    assert_eq!(fact["tags"].as_str(), Some("t1"));
    assert!(approx(fact["trust_score"].as_f64().unwrap(), 0.8));

    let out = call(
        &plugin,
        "fact_store",
        json!({"action": "update", "fact_id": id, "trust_delta": 5.0}),
    );
    assert_eq!(out["updated"].as_bool(), Some(true), "{out}");
    let list = call(&plugin, "fact_store", json!({"action": "list"}));
    assert!(approx(
        list["facts"][0]["trust_score"].as_f64().unwrap(),
        1.0
    ));

    let out = call(
        &plugin,
        "fact_store",
        json!({"action": "update", "fact_id": 9999, "trust_delta": 0.1}),
    );
    assert_eq!(out["updated"].as_bool(), Some(false), "{out}");

    plugin.add_fact("Other content", "general", "").unwrap();
    let out = call(
        &plugin,
        "fact_store",
        json!({"action": "update", "fact_id": id, "content": "Other content"}),
    );
    assert!(out["error"].as_str().unwrap().contains("UNIQUE"), "{out}");
}

#[test]
fn fact_store_remove_and_list() {
    let (_tmp, plugin) = init_plugin();
    let id = plugin
        .add_fact("John Doe removable fact", "general", "")
        .unwrap();
    plugin.add_fact("Kept fact one", "general", "").unwrap();
    plugin.add_fact("Kept fact two", "general", "").unwrap();

    let out = call(
        &plugin,
        "fact_store",
        json!({"action": "remove", "fact_id": id}),
    );
    assert_eq!(out["removed"].as_bool(), Some(true));
    let out = call(
        &plugin,
        "fact_store",
        json!({"action": "remove", "fact_id": id}),
    );
    assert_eq!(out["removed"].as_bool(), Some(false));

    let list = call(&plugin, "fact_store", json!({"action": "list"}));
    assert_eq!(list["count"].as_u64(), Some(2));
    let list = call(
        &plugin,
        "fact_store",
        json!({"action": "list", "limit": 1, "min_trust": 0.1}),
    );
    assert_eq!(list["count"].as_u64(), Some(1));
    let list = call(
        &plugin,
        "fact_store",
        json!({"action": "list", "min_trust": 0.9}),
    );
    assert_eq!(list["count"].as_u64(), Some(0));
}

#[test]
fn fact_store_not_initialized() {
    let plugin = HolographicMemoryPlugin::new();
    for args in [
        json!({"action": "update", "fact_id": 1}),
        json!({"action": "remove", "fact_id": 1}),
        json!({"action": "list"}),
        json!({"action": "add", "content": "x"}),
    ] {
        let out = call(&plugin, "fact_store", args);
        assert_eq!(out["error"].as_str(), Some("Not initialized"));
    }
}

#[test]
fn fact_store_list_reports_prepare_error() {
    let (_tmp, plugin) = init_plugin();
    plugin
        .with_conn(|c| c.execute_batch("DROP TABLE facts").unwrap())
        .unwrap();
    let out = call(&plugin, "fact_store", json!({"action": "list"}));
    assert!(out["error"].as_str().unwrap().contains("no such table"));
}
