use super::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn new_backend() -> (tempfile::TempDir, SqliteSessionSearchBackend) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("sessions.db");
    let backend =
        SqliteSessionSearchBackend::new(db.to_str().expect("utf8 path")).expect("backend");
    (dir, backend)
}

fn insert_session(
    conn: &Connection,
    id: &str,
    platform: &str,
    parent: Option<&str>,
    end_reason: Option<&str>,
    created: &str,
) {
    conn.execute(
        "INSERT INTO sessions (id, model, platform, created_at, updated_at, parent_session_id, end_reason)
         VALUES (?1, 'gpt', ?2, ?3, ?3, ?4, ?5)",
        rusqlite::params![id, platform, created, parent, end_reason],
    )
    .expect("insert session");
}

#[test]
fn fts5_unavailable_detection() {
    assert!(SqliteSessionSearchBackend::is_fts5_unavailable_message(
        "no such module: fts5"
    ));
    assert!(SqliteSessionSearchBackend::is_fts5_unavailable_message(
        "Unknown Tokenizer: foo"
    ));
    assert!(SqliteSessionSearchBackend::is_fts5_unavailable_message(
        "FTS5 broke"
    ));
    assert!(!SqliteSessionSearchBackend::is_fts5_unavailable_message(
        "disk I/O error"
    ));
    assert!(SqliteSessionSearchBackend::is_fts5_unavailable_error(
        &rusqlite::Error::InvalidColumnName("fts5_col".to_string())
    ));
    assert!(!SqliteSessionSearchBackend::is_fts5_unavailable_error(
        &rusqlite::Error::QueryReturnedNoRows
    ));
}

#[test]
fn fts_table_exists_reports_presence() {
    let conn = Connection::open_in_memory().unwrap();
    assert!(!SqliteSessionSearchBackend::fts_table_exists(&conn).unwrap());
    conn.execute_batch("CREATE TABLE messages_fts (content TEXT);")
        .unwrap();
    assert!(SqliteSessionSearchBackend::fts_table_exists(&conn).unwrap());
}

#[test]
fn ensure_fts_schema_without_messages_table_fails() {
    let conn = Connection::open_in_memory().unwrap();
    match SqliteSessionSearchBackend::ensure_fts_schema(&conn, "mem") {
        Ok(enabled) => assert!(!enabled),
        Err(e) => assert!(e
            .to_string()
            .contains("Failed to ensure session FTS schema")),
    }
}

#[test]
fn ensure_text_column_adds_and_reports_errors() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t (id INTEGER);").unwrap();
    SqliteSessionSearchBackend::ensure_text_column(&conn, "t", "c").unwrap();
    SqliteSessionSearchBackend::ensure_text_column(&conn, "t", "c").unwrap();
    assert!(conn.prepare("SELECT c FROM t").is_ok());
    let err = SqliteSessionSearchBackend::ensure_text_column(&conn, "missing", "c").unwrap_err();
    assert!(err.to_string().contains("Failed to ensure missing.c"));
}

#[test]
fn ensure_integer_column_adds_with_default_and_reports_errors() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t (id INTEGER);").unwrap();
    SqliteSessionSearchBackend::ensure_integer_column(&conn, "t", "n", 7, true).unwrap();
    SqliteSessionSearchBackend::ensure_integer_column(&conn, "t", "n", 7, true).unwrap();
    conn.execute("INSERT INTO t (id) VALUES (1)", []).unwrap();
    let n: i64 = conn
        .query_row("SELECT n FROM t WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 7);
    let err = SqliteSessionSearchBackend::ensure_integer_column(&conn, "missing", "n", 0, false)
        .unwrap_err();
    assert!(err.to_string().contains("Failed to ensure missing.n"));
}

#[test]
fn ensure_session_compat_columns_index_failure() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT); CREATE TABLE messages (id INTEGER PRIMARY KEY);",
    )
    .unwrap();
    let err = SqliteSessionSearchBackend::ensure_session_compat_columns(&conn).unwrap_err();
    assert!(err
        .to_string()
        .contains("Failed to ensure active message index"));
}

#[test]
fn branch_marker_handles_blank_and_value() {
    assert_eq!(
        SqliteSessionSearchBackend::branch_marker_from_model_config(Some("   ")),
        None
    );
    assert_eq!(
        SqliteSessionSearchBackend::branch_marker_from_model_config(Some(
            r#"{"_branched_from":" abc "}"#
        )),
        Some("abc".to_string())
    );
}

#[test]
fn parse_timestamp_utc_variants() {
    let ts = SqliteSessionSearchBackend::parse_timestamp_utc(Some("1700000000.5")).unwrap();
    assert_eq!(ts.as_second(), 1_700_000_000);
    assert_eq!(ts.subsec_nanosecond(), 500_000_000);

    let naive =
        SqliteSessionSearchBackend::parse_timestamp_utc(Some("2024-01-02 03:04:05")).unwrap();
    let expected: Timestamp = "2024-01-02T03:04:05Z".parse().unwrap();
    assert_eq!(naive, expected);

    assert!(SqliteSessionSearchBackend::parse_timestamp_utc(Some("garbage")).is_none());
}

#[test]
fn legacy_branch_child_with_unparseable_times() {
    assert!(SqliteSessionSearchBackend::is_legacy_branch_child(
        Some("bad"),
        Some("branched"),
        Some("   ")
    ));
    assert!(SqliteSessionSearchBackend::is_legacy_branch_child(
        Some("bad"),
        Some("branched"),
        None
    ));
    assert!(!SqliteSessionSearchBackend::is_legacy_branch_child(
        Some("bad"),
        Some("branched"),
        Some("also bad")
    ));
}

#[test]
fn compression_root_and_tip_edge_cases() {
    let (_dir, backend) = new_backend();
    let conn = backend.conn.lock().unwrap();
    // Cycle of compression children.
    insert_session(
        &conn,
        "a",
        "cli",
        Some("b"),
        Some("compression"),
        "2024-01-01 00:00:00",
    );
    insert_session(
        &conn,
        "b",
        "cli",
        Some("a"),
        Some("compression"),
        "2024-01-01 00:00:01",
    );
    // Parent with blank id.
    insert_session(
        &conn,
        " ",
        "cli",
        None,
        Some("compression"),
        "2024-01-01 00:00:02",
    );
    insert_session(
        &conn,
        "orphan",
        "cli",
        Some(" "),
        None,
        "2024-01-01 00:00:03",
    );

    assert_eq!(
        SqliteSessionSearchBackend::compression_root(&conn, "missing"),
        "missing"
    );
    assert_eq!(
        SqliteSessionSearchBackend::compression_root(&conn, "a"),
        "a"
    );
    assert_eq!(SqliteSessionSearchBackend::compression_tip(&conn, "a"), "a");
    assert_eq!(
        SqliteSessionSearchBackend::compression_root(&conn, "orphan"),
        "orphan"
    );
}

#[test]
fn compression_tip_without_schema_returns_input() {
    let conn = Connection::open_in_memory().unwrap();
    assert_eq!(SqliteSessionSearchBackend::compression_tip(&conn, "x"), "x");
}

#[test]
fn logical_key_and_resolve_to_parent() {
    let (_dir, backend) = new_backend();
    let conn = backend.conn.lock().unwrap();
    insert_session(&conn, "p", "cli", None, None, "2024-01-01 00:00:00");
    insert_session(&conn, "c", "cli", Some("p"), None, "2024-01-01 00:00:01");
    insert_session(&conn, "x", "cli", Some("y"), None, "2024-01-01 00:00:02");
    insert_session(&conn, "y", "cli", Some("x"), None, "2024-01-01 00:00:03");

    assert_eq!(
        SqliteSessionSearchBackend::logical_session_key_for_session_id(&conn, "nope"),
        "nope"
    );
    assert_eq!(
        SqliteSessionSearchBackend::logical_session_key_for_session_id(&conn, "c"),
        "p"
    );
    assert_eq!(
        SqliteSessionSearchBackend::resolve_to_parent(&conn, "x"),
        "x"
    );
}

#[test]
fn search_sessions_by_id_edge_cases() {
    let conn = Connection::open_in_memory().unwrap();
    assert!(
        SqliteSessionSearchBackend::search_sessions_by_id(&conn, "   ", 3)
            .unwrap()
            .is_empty()
    );
    assert!(
        SqliteSessionSearchBackend::search_sessions_by_id(&conn, "abc", 0)
            .unwrap()
            .is_empty()
    );
    let err = match SqliteSessionSearchBackend::search_sessions_by_id(&conn, "abc", 3) {
        Ok(_) => panic!("expected prepare failure"),
        Err(e) => e,
    };
    assert!(err
        .to_string()
        .contains("Failed to prepare session id search query"));
}

#[test]
fn load_summary_task_prepare_failure() {
    let conn = Connection::open_in_memory().unwrap();
    let err = match SqliteSessionSearchBackend::load_summary_task(&conn, "s", "q", true) {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("Failed to prepare messages query"));
}

#[test]
fn load_summary_task_empty_session() {
    let (_dir, backend) = new_backend();
    let conn = backend.conn.lock().unwrap();
    insert_session(&conn, "s1", "discord", None, None, "2024-01-01 00:00:00");
    assert!(
        SqliteSessionSearchBackend::load_summary_task(&conn, "s1", "q", false)
            .unwrap()
            .is_none()
    );
    let task = SqliteSessionSearchBackend::load_summary_task(&conn, "s1", "q", true)
        .unwrap()
        .expect("task");
    assert_eq!(task.session_id, "s1");
    assert_eq!(task.source, "discord");
    assert_eq!(task.model.as_deref(), Some("gpt"));
    assert_eq!(task.conversation_text, "Session ID: s1");
}

#[test]
fn format_conversation_tool_truncation_and_tool_calls() {
    let long = format!("{}{}", "a".repeat(300), "b".repeat(300));
    let calls = r#"[{"name":"read"},{"function":{"name":"write"}},{"name":"  "}]"#;
    let messages = vec![
        ("tool".to_string(), long, None),
        (
            "assistant".to_string(),
            "done".to_string(),
            Some(calls.to_string()),
        ),
        (
            "assistant".to_string(),
            "  ".to_string(),
            Some("not json".to_string()),
        ),
        ("user".to_string(), "hi".to_string(), None),
    ];
    let out = SqliteSessionSearchBackend::format_conversation(&messages);
    let expected = format!(
        "[TOOL]: {}\n...[truncated]...\n{}\n\n[ASSISTANT]: [Called: read, write]\n\n[ASSISTANT]: done\n\n[USER]: hi",
        "a".repeat(250),
        "b".repeat(250)
    );
    assert_eq!(out, expected);
}

#[test]
fn truncate_around_match_near_end() {
    let text = format!("{}ab", "x".repeat(20));
    let out = SqliteSessionSearchBackend::truncate_around_matches(&text, "b", 10);
    assert_eq!(
        out,
        format!(
            "...[earlier conversation truncated]...\n\n{}ab",
            "x".repeat(8)
        )
    );
}

#[test]
fn format_timestamp_variants() {
    let numeric = SqliteSessionSearchBackend::format_timestamp(Some("1700000000"));
    assert!(numeric.contains("2023"));
    assert!(numeric.contains(" at "));

    let rfc = SqliteSessionSearchBackend::format_timestamp(Some("2024-06-15T12:00:00Z"));
    assert!(rfc.contains("June"));
    assert!(rfc.contains("2024"));

    let naive = SqliteSessionSearchBackend::format_timestamp(Some("2024-06-15 12:00:00"));
    assert!(naive.contains("June"));
    assert!(naive.contains("2024"));
    assert_ne!(naive, "2024-06-15 12:00:00");

    assert_eq!(
        SqliteSessionSearchBackend::format_timestamp(Some("not a date")),
        "not a date"
    );
}

#[tokio::test]
async fn summarize_one_returns_summary_with_openrouter_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/openrouter.ai/chat/completions"))
        .and(header("X-OpenRouter-Title", "Hermes Agent"))
        .and(header("authorization", "Bearer secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "  summary text  "}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = SessionSummaryClient {
        client: reqwest::Client::new(),
        base_url: format!("{}/openrouter.ai", server.uri()),
        api_key: " secret ".to_string(),
        model: "m".to_string(),
    };
    let task = SummaryTask {
        session_id: "s".to_string(),
        source: "cli".to_string(),
        when: None,
        model: None,
        conversation_text: "[USER]: hi".to_string(),
    };
    let out = SqliteSessionSearchBackend::summarize_one(&client, "topic", &task).await;
    assert_eq!(out.as_deref(), Some("summary text"));
}

#[tokio::test(start_paused = true)]
async fn summarize_one_gives_up_after_retries() {
    let client = SessionSummaryClient {
        client: reqwest::Client::new(),
        base_url: "notaurl".to_string(),
        api_key: "secret".to_string(),
        model: "m".to_string(),
    };
    let task = SummaryTask {
        session_id: "s".to_string(),
        source: "cli".to_string(),
        when: Some("today".to_string()),
        model: None,
        conversation_text: "text".to_string(),
    };
    let out = SqliteSessionSearchBackend::summarize_one(&client, "topic", &task).await;
    assert!(out.is_none());
}

#[test]
fn new_fails_when_parent_is_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file.txt");
    std::fs::write(&file, b"x").unwrap();
    let db = file.join("sub").join("sessions.db");
    let err = match SqliteSessionSearchBackend::new(db.to_str().unwrap()) {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    assert!(err
        .to_string()
        .contains("Failed to create session db directory"));
}

#[test]
fn new_fails_when_path_is_directory() {
    let dir = tempfile::tempdir().unwrap();
    let err = match SqliteSessionSearchBackend::new(dir.path().to_str().unwrap()) {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("Failed to open sessions DB")
            || msg.contains("Failed to ensure session schema")
    );
}

#[test]
fn new_fails_on_incompatible_schema() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("sessions.db");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE messages (id INTEGER PRIMARY KEY);")
            .unwrap();
    }
    let err = match SqliteSessionSearchBackend::new(db.to_str().unwrap()) {
        Ok(_) => panic!("expected failure"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("Failed to ensure session schema"));
}

#[test]
fn index_message_inserts_row() {
    let (_dir, backend) = new_backend();
    {
        let conn = backend.conn.lock().unwrap();
        insert_session(&conn, "s1", "cli", None, None, "2024-01-01 00:00:00");
    }
    backend
        .index_message("s1", "user", "hello world", "2024-01-01 00:00:00")
        .unwrap();
    let conn = backend.conn.lock().unwrap();
    let (role, content): (String, String) = conn
        .query_row(
            "SELECT role, content FROM messages WHERE session_id = 's1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(role, "user");
    assert_eq!(content, "hello world");
}

#[tokio::test]
async fn search_fails_on_poisoned_lock() {
    let (_dir, backend) = new_backend();
    let joined = std::thread::scope(|s| {
        s.spawn(|| {
            let _guard = backend.conn.lock().unwrap();
            panic!("poison the lock");
        })
        .join()
    });
    assert!(joined.is_err());
    assert!(backend.search(None, None, 5, None).await.is_err());
}

#[tokio::test]
async fn recent_mode_prepare_failure() {
    let backend = SqliteSessionSearchBackend {
        conn: Mutex::new(Connection::open_in_memory().unwrap()),
        fts_enabled: false,
    };
    let err = backend.search(None, None, 5, None).await.unwrap_err();
    assert!(err
        .to_string()
        .contains("Failed to prepare recent sessions query"));
}

#[tokio::test]
async fn recent_mode_excludes_current_session() {
    let (_dir, backend) = new_backend();
    {
        let conn = backend.conn.lock().unwrap();
        insert_session(&conn, "a", "cli", None, None, "2024-01-01 00:00:00");
        insert_session(&conn, "b", "cli", None, None, "2024-01-02 00:00:00");
    }
    let out = backend.search(None, None, 5, Some(" a ")).await.unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["mode"], "recent");
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["session_id"], "b");
}

#[tokio::test]
async fn query_mode_excludes_current_session() {
    let (_dir, backend) = new_backend();
    {
        let conn = backend.conn.lock().unwrap();
        insert_session(&conn, "alpha-1", "cli", None, None, "2024-01-01 00:00:00");
        insert_session(&conn, "alpha-2", "cli", None, None, "2024-01-02 00:00:00");
    }
    let out = backend
        .search(Some("alpha"), None, 5, Some("alpha-1"))
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["session_id"], "alpha-2");
    assert_eq!(v["sessions_searched"], 1);
}

#[tokio::test]
async fn query_mode_role_filter() {
    let (_dir, backend) = new_backend();
    {
        let conn = backend.conn.lock().unwrap();
        insert_session(&conn, "s1", "cli", None, None, "2024-01-01 00:00:00");
        insert_session(&conn, "s2", "cli", None, None, "2024-01-02 00:00:00");
    }
    backend
        .index_message("s1", "user", "kumquat recipe", "2024-01-01 00:00:00")
        .unwrap();
    backend
        .index_message("s2", "assistant", "kumquat answer", "2024-01-02 00:00:00")
        .unwrap();
    let out = backend
        .search(Some("kumquat"), Some("assistant, ,"), 5, None)
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let results = v["results"].as_array().unwrap();
    if backend.fts_enabled {
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["session_id"], "s2");
    } else {
        assert_eq!(v["search_degraded"], "fts5_unavailable");
        assert!(results.is_empty());
    }
}
