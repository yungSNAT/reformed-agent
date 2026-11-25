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

include!("holographic_part2_coverage_gen.rs");
