use super::*;

use std::panic::{catch_unwind, AssertUnwindSafe};

fn init_plugin() -> (tempfile::TempDir, HolographicMemoryPlugin) {
    let tmp = tempfile::tempdir().unwrap();
    let plugin = HolographicMemoryPlugin::new();
    plugin.initialize("cov-session", tmp.path().to_str().unwrap());
    (tmp, plugin)
}

fn call(plugin: &HolographicMemoryPlugin, tool: &str, args: Value) -> Value {
    serde_json::from_str(&plugin.handle_tool_call(tool, &args)).unwrap()
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn fts5_unavailable_detection() {
    let conn = Connection::open_in_memory().unwrap();
    let err = conn
        .execute_batch("CREATE VIRTUAL TABLE t USING nosuchmodulexyz(x)")
        .unwrap_err();
    assert!(is_fts5_unavailable(&err));
    let err2 = conn.execute_batch("SELEC 1").unwrap_err();
    assert!(!is_fts5_unavailable(&err2));
}

#[test]
fn jaccard_handles_empty_and_overlap() {
    let empty: Vec<String> = Vec::new();
    let one = vec!["a".to_string()];
    let two = vec!["a".to_string(), "b".to_string()];
    assert!(approx(jaccard_similarity(&empty, &one), 0.0));
    assert!(approx(jaccard_similarity(&one, &empty), 0.0));
    assert!(approx(jaccard_similarity(&two, &one), 0.5));
}

#[test]
fn extract_entities_double_quoted_terms_deduplicated() {
    let entities = extract_entities(r#"he said "hello world" and "hello world" again"#);
    assert_eq!(entities, vec!["hello world".to_string()]);
}

#[test]
fn default_matches_new() {
    let plugin = HolographicMemoryPlugin::default();
    assert_eq!(plugin.name(), "holographic");
    assert_eq!(plugin.fact_count(), 0);
    assert!(approx(plugin.default_trust, 0.5));
    assert!(approx(plugin.min_trust, 0.3));
    assert!(plugin.with_conn(|c| c.is_autocommit()).is_none());
}

#[test]
fn add_fact_rejects_empty_content() {
    let (_tmp, plugin) = init_plugin();
    assert_eq!(
        plugin.add_fact("   ", "general", ""),
        Err("content must not be empty".to_string())
    );
    let out = call(
        &plugin,
        "fact_store",
        json!({"action": "add", "content": ""}),
    );
    assert_eq!(out["error"].as_str(), Some("content must not be empty"));
    assert_eq!(plugin.fact_count(), 0);
}

#[test]
fn add_fact_resolves_entities() {
    let (_tmp, plugin) = init_plugin();
    plugin
        .add_fact("John Doe joined Acme Corp", "general", "")
        .unwrap();
    plugin
        .add_fact(r#"John Doe likes "Rust""#, "general", "")
        .unwrap();
    let entities = plugin
        .with_conn(|c| {
            c.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get::<_, i64>(0))
                .unwrap()
        })
        .unwrap();
    assert_eq!(entities, 3);
    let links = plugin
        .with_conn(|c| {
            c.query_row("SELECT COUNT(*) FROM fact_entities", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        })
        .unwrap();
    assert_eq!(links, 4);
}

#[test]
fn add_fact_reports_other_sql_errors() {
    let (_tmp, plugin) = init_plugin();
    plugin
        .with_conn(|c| c.execute_batch("DROP TABLE facts").unwrap())
        .unwrap();
    let err = plugin.add_fact("something", "general", "").unwrap_err();
    assert!(err.contains("no such table"), "{err}");
}

#[test]
fn ensure_fts_schema_variants() {
    let ok_conn = Connection::open_in_memory().unwrap();
    ok_conn.execute_batch(SCHEMA).unwrap();
    assert_eq!(
        HolographicMemoryPlugin::ensure_fts_schema(&ok_conn),
        Ok(true)
    );

    let rebuild_conn = Connection::open_in_memory().unwrap();
    rebuild_conn.execute_batch(SCHEMA).unwrap();
    rebuild_conn
        .execute_batch("CREATE TABLE facts_fts(content, tags)")
        .unwrap();
    assert_eq!(
        HolographicMemoryPlugin::ensure_fts_schema(&rebuild_conn),
        Ok(true)
    );

    let bad_conn = Connection::open_in_memory().unwrap();
    bad_conn.execute_batch(SCHEMA).unwrap();
    bad_conn
        .execute_batch("CREATE INDEX facts_fts ON facts(content)")
        .unwrap();
    let err = HolographicMemoryPlugin::ensure_fts_schema(&bad_conn).unwrap_err();
    assert!(!err.is_empty());
}

#[test]
fn initialize_with_fts_failure_falls_back_to_like() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let conn = Connection::open(tmp.path().join("memory_store.db")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch("CREATE INDEX facts_fts ON facts(content)")
            .unwrap();
    }
    let plugin = HolographicMemoryPlugin::new();
    plugin.initialize("s", tmp.path().to_str().unwrap());
    assert!(plugin.with_conn(|c| c.is_autocommit()).is_some());
    assert!(!*plugin.fts_enabled.lock().unwrap());

    plugin
        .add_fact("Rust gateway deploy note", "project", "")
        .unwrap();
    let second = plugin
        .add_fact("Rust gateway second note", "project", "")
        .unwrap();
    plugin
        .add_fact("Rust gateway pref", "user_pref", "")
        .unwrap();
    plugin.record_feedback(second, true).unwrap();

    let results = plugin.search_facts("rust gateway", Some("project"), 0.0, 10);
    assert_eq!(results.len(), 2);
    assert!(results
        .iter()
        .all(|r| r["category"].as_str() == Some("project")));
    assert!(results[0]["content"].as_str().unwrap().contains("second"));
    assert!(approx(results[0]["fts_rank"].as_f64().unwrap(), 0.25));
    assert!(results[0]["score"].as_f64().unwrap() >= results[1]["score"].as_f64().unwrap());
}

#[test]
fn initialize_with_corrupt_db_leaves_plugin_uninitialized() {
    let tmp = tempfile::tempdir().unwrap();
    let junk = b"this is definitely not a sqlite database file. ".repeat(100);
    std::fs::write(tmp.path().join("memory_store.db"), junk).unwrap();
    let plugin = HolographicMemoryPlugin::new();
    plugin.initialize("s", tmp.path().to_str().unwrap());
    assert!(plugin.with_conn(|c| c.is_autocommit()).is_none());
    assert!(plugin.db_path.lock().unwrap().is_none());
    assert_eq!(plugin.fact_count(), 0);
}

#[test]
fn initialize_with_unopenable_path_leaves_plugin_uninitialized() {
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, b"file").unwrap();
    let home = blocker.join("sub");
    let plugin = HolographicMemoryPlugin::new();
    plugin.initialize("s", home.to_str().unwrap());
    assert!(plugin.with_conn(|c| c.is_autocommit()).is_none());
    assert!(plugin.session_id.lock().unwrap().is_empty());
}

#[test]
fn search_edge_cases_return_empty() {
    let uninit = HolographicMemoryPlugin::new();
    assert!(uninit.search_facts("anything", None, 0.0, 5).is_empty());

    let (_tmp, plugin) = init_plugin();
    plugin.add_fact("some fact here", "general", "").unwrap();
    assert!(plugin.search_facts("   ", None, 0.0, 5).is_empty());
    assert!(plugin.search_facts("fact", None, 0.0, 0).is_empty());
}

#[test]
fn search_with_fts_disabled_uses_like() {
    let (_tmp, plugin) = init_plugin();
    plugin.add_fact("alpha beta gamma", "general", "").unwrap();
    *plugin.fts_enabled.lock().unwrap() = false;
    let results = plugin.search_facts("beta", None, 0.0, 5);
    assert_eq!(results.len(), 1);
    assert!(approx(results[0]["fts_rank"].as_f64().unwrap(), 0.25));
    let count = plugin
        .with_conn(|c| {
            c.query_row("SELECT retrieval_count FROM facts", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn search_punctuation_and_stopword_queries() {
    let (_tmp, plugin) = init_plugin();
    plugin.add_fact("wow!!! amazing", "general", "").unwrap();
    let results = plugin.search_facts("!!!", None, 0.0, 5);
    assert_eq!(results.len(), 1);
    assert!(results[0]["content"].as_str().unwrap().contains("wow"));

    plugin.add_fact("over there", "general", "").unwrap();
    let results = plugin.search_facts("the", None, 0.0, 5);
    assert_eq!(results.len(), 1);
    assert!(results[0]["content"].as_str().unwrap().contains("there"));
}

#[test]
fn search_survives_missing_tables() {
    let (_tmp, plugin) = init_plugin();
    plugin.add_fact("alpha unique fact", "general", "").unwrap();
    plugin
        .with_conn(|c| c.execute_batch("DROP TABLE facts_fts").unwrap())
        .unwrap();
    let results = plugin.search_facts("alpha", None, 0.0, 5);
    assert_eq!(results.len(), 1);

    plugin
        .with_conn(|c| c.execute_batch("DROP TABLE facts").unwrap())
        .unwrap();
    assert!(plugin.search_facts("alpha", None, 0.0, 5).is_empty());
}

#[test]
fn search_sorts_multiple_results_by_score() {
    let (_tmp, plugin) = init_plugin();
    plugin.add_fact("rust alpha", "general", "").unwrap();
    let id = plugin.add_fact("rust beta delta", "general", "").unwrap();
    plugin.add_fact("rust gamma", "general", "").unwrap();
    plugin.record_feedback(id, true).unwrap();
    let results = plugin.search_facts("rust", None, 0.0, 10);
    assert_eq!(results.len(), 3);
    for pair in results.windows(2) {
        assert!(pair[0]["score"].as_f64().unwrap() >= pair[1]["score"].as_f64().unwrap());
    }
}

#[test]
fn poisoned_mutex_paths_return_errors() {
    let (_tmp, plugin) = init_plugin();
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = plugin.conn.lock().unwrap();
        panic!("poison the connection mutex");
    }));
    assert!(plugin.conn.is_poisoned());
    assert!(plugin.search_facts("x", None, 0.0, 5).is_empty());
    assert!(plugin.add_fact("x", "general", "").is_err());
    let upd = call(
        &plugin,
        "fact_store",
        json!({"action": "update", "fact_id": 1}),
    );
    assert!(upd["error"].is_string());
    let rem = call(
        &plugin,
        "fact_store",
        json!({"action": "remove", "fact_id": 1}),
    );
    assert!(rem["error"].is_string());
    let list = call(&plugin, "fact_store", json!({"action": "list"}));
    assert!(list["error"].is_string());
}

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
