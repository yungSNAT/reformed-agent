use super::*;
use hermes_core::time::rfc3339;
use hermes_core::Message;
use jiff::{SignedDuration, Timestamp};
use rusqlite::params;

fn mark_session_old(sp: &SessionPersistence, session_id: &str, days_old: i64) {
    let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
    let ts = rfc3339(Timestamp::now() - SignedDuration::from_hours(24 * days_old));
    conn.execute(
        "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
        params![ts, session_id],
    )
    .expect("update updated_at");
}

fn set_session_lineage(
    sp: &SessionPersistence,
    session_id: &str,
    parent_session_id: Option<&str>,
    end_reason: Option<&str>,
    created_at: &str,
    ended_at: Option<&str>,
) {
    let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
    conn.execute(
        "UPDATE sessions
         SET parent_session_id = ?1,
             end_reason = ?2,
             created_at = ?3,
             updated_at = ?3,
             ended_at = ?4
         WHERE id = ?5",
        params![
            parent_session_id,
            end_reason,
            created_at,
            ended_at,
            session_id
        ],
    )
    .expect("update session lineage");
}

fn set_session_model_config(sp: &SessionPersistence, session_id: &str, model_config: &str) {
    let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
    conn.execute(
        "UPDATE sessions SET model_config = ?1 WHERE id = ?2",
        params![model_config, session_id],
    )
    .expect("update model config");
}

fn set_session_platform(sp: &SessionPersistence, session_id: &str, platform: &str) {
    let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
    conn.execute(
        "UPDATE sessions SET platform = ?1 WHERE id = ?2",
        params![platform, session_id],
    )
    .expect("update platform");
}

fn set_message_created_at(sp: &SessionPersistence, session_id: &str, created_at: &str) {
    let conn = rusqlite::Connection::open(&sp.db_path).expect("open db");
    conn.execute(
        "UPDATE messages SET created_at = ?1 WHERE session_id = ?2",
        params![created_at, session_id],
    )
    .expect("update message timestamp");
}

fn grow_sessions_wal(sp: &SessionPersistence) -> (rusqlite::Connection, std::path::PathBuf, u64) {
    sp.ensure_db().unwrap();
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    conn.execute_batch("PRAGMA wal_autocheckpoint=0;").unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS wal_growth (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            payload TEXT NOT NULL
        )",
        [],
    )
    .unwrap();

    let payload = "x".repeat(64 * 1024);
    let tx = conn.unchecked_transaction().unwrap();
    for _ in 0..96 {
        tx.execute(
            "INSERT INTO wal_growth (payload) VALUES (?1)",
            params![&payload],
        )
        .unwrap();
    }
    tx.commit().unwrap();

    let wal_path = std::path::PathBuf::from(format!("{}-wal", sp.db_path.display()));
    let wal_len = std::fs::metadata(&wal_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    assert!(
        wal_len > 0,
        "test setup should create a non-empty sessions.db-wal sidecar"
    );
    (conn, wal_path, wal_len)
}

fn wal_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn fts_match_rows(sp: &SessionPersistence, query: &str) -> Vec<(i64, String)> {
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    if !SessionPersistence::fts_table_exists(&conn).unwrap() {
        return Vec::new();
    }
    let mut stmt = conn
        .prepare(
            "SELECT rowid, snippet(messages_fts, 0, '[', ']', '...', 8)
             FROM messages_fts
             WHERE messages_fts MATCH ?1
             ORDER BY rowid",
        )
        .unwrap();
    stmt.query_map(params![query], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn persist_searchable_session(sp: &SessionPersistence) {
    let messages = vec![
        Message::user("hello world"),
        Message::assistant("reply about pizza"),
        Message::user("second turn"),
        Message::assistant("more pizza details"),
    ];
    sp.persist_session("schema-repair", &messages, Some("gpt-4o"), None, None, None)
        .unwrap();
    assert_eq!(fts_match_rows(sp, "pizza").len(), 2);
}

fn corrupt_duplicate_fts_schema(sp: &SessionPersistence) {
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
    conn.execute(
        "INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql)
         SELECT type, name, tbl_name, rootpage, sql
         FROM sqlite_master
         WHERE name = 'messages_fts'",
        [],
    )
    .unwrap();
    let _ = conn.execute_batch("PRAGMA writable_schema=OFF");
    drop(conn);
}

#[test]
fn malformed_schema_error_classifier_matches_expected_sqlite_messages() {
    assert!(SessionPersistence::is_malformed_db_error_message(
        "malformed database schema (messages_fts) - table messages_fts already exists"
    ));
    assert!(SessionPersistence::is_malformed_db_error_message(
        "database disk image is malformed"
    ));
    assert!(!SessionPersistence::is_malformed_db_error_message(
        "database is locked"
    ));
}

#[test]
fn duplicate_fts_schema_makes_first_statement_fail() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    persist_searchable_session(&sp);
    corrupt_duplicate_fts_schema(&sp);

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let err = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
        .unwrap_err();
    assert!(SessionPersistence::is_malformed_sqlite_error(&err));
}

#[test]
fn repair_malformed_schema_preserves_sessions_messages_and_search() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    persist_searchable_session(&sp);
    corrupt_duplicate_fts_schema(&sp);

    let report = sp.repair_malformed_schema(true);
    assert!(report.repaired, "{report:?}");
    assert_eq!(report.strategy.as_deref(), Some("dedup_schema"));
    assert!(report.backup_path.as_ref().is_some_and(|p| p.exists()));
    assert!(sp.db_health_error().is_none());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let session_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    let message_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(session_count, 1);
    assert_eq!(message_count, 4);
    drop(conn);
    assert_eq!(fts_match_rows(&sp, "pizza").len(), 2);
}

#[test]
fn ensure_db_auto_heals_duplicate_fts_schema_once() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    persist_searchable_session(&sp);
    corrupt_duplicate_fts_schema(&sp);

    sp.ensure_db().unwrap();
    assert!(sp.db_health_error().is_none());
    assert_eq!(fts_match_rows(&sp, "pizza").len(), 2);
}

#[test]
fn drop_fts_repair_rebuilds_search_from_canonical_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    persist_searchable_session(&sp);
    corrupt_duplicate_fts_schema(&sp);

    sp.repair_schema_drop_fts_pass().unwrap();
    assert!(sp.db_health_error().is_none());
    assert_eq!(fts_match_rows(&sp, "pizza").len(), 2);
}

#[test]
fn unrepairable_sessions_db_fails_safely_with_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    std::fs::create_dir_all(tmp.path()).unwrap();
    std::fs::write(
        &sp.db_path,
        b"SQLite format 3\0not actually a valid database",
    )
    .unwrap();

    let report = sp.repair_malformed_schema(true);
    assert!(!report.repaired);
    assert!(report.error.as_deref().is_some_and(|e| !e.is_empty()));
    assert!(report.backup_path.as_ref().is_some_and(|p| p.exists()));
}

#[test]
fn sessions_backup_keeps_the_database_and_readable_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    assert!(SessionPersistence::backup_db_file(sp.db_path()).is_none());

    let sidecar = |db: &std::path::Path, suffix: &str| {
        let mut name = db.file_name().unwrap().to_os_string();
        name.push(suffix);
        db.with_file_name(name)
    };
    let db_contents = b"fixture database bytes";
    let wal_contents = b"fixture journal bytes";
    let wal = sidecar(sp.db_path(), "-wal");
    let shm = sidecar(sp.db_path(), "-shm");
    std::fs::write(sp.db_path(), db_contents).unwrap();
    std::fs::write(&wal, wal_contents).unwrap();
    std::fs::create_dir(&shm).unwrap();

    let backup = SessionPersistence::backup_db_file(sp.db_path()).unwrap();
    assert_eq!(backup.parent(), Some(tmp.path()));
    assert_ne!(backup.as_path(), sp.db_path());
    assert_eq!(std::fs::read(&backup).unwrap(), db_contents);
    assert_eq!(
        std::fs::read(sidecar(&backup, "-wal")).unwrap(),
        wal_contents
    );
    assert_eq!(std::fs::read(sp.db_path()).unwrap(), db_contents);
    assert_eq!(std::fs::read(wal).unwrap(), wal_contents);
    assert!(shm.is_dir());
}

#[test]
fn test_persist_and_load_session() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    let mut assistant = Message::assistant("Hi there!");
    assistant.reasoning_content = Some("provider scratchpad".to_string());
    let messages = vec![
        Message::system("You are helpful"),
        Message::user("Hello"),
        assistant,
    ];

    sp.persist_session(
        "test-session-1",
        &messages,
        Some("gpt-4o"),
        None,
        Some("Test"),
        Some("cached system blob"),
    )
    .unwrap();

    assert_eq!(
        sp.get_system_prompt("test-session-1").unwrap().as_deref(),
        Some("cached system blob")
    );

    let loaded = sp.load_session("test-session-1").unwrap();
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0].role, MessageRole::System);
    assert_eq!(loaded[1].content.as_deref(), Some("Hello"));
    assert_eq!(loaded[2].content.as_deref(), Some("Hi there!"));
    assert_eq!(
        loaded[2].reasoning_content.as_deref(),
        Some("provider scratchpad")
    );
}

#[test]
fn test_persist_and_load_tool_result_name() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::tool_result_with_name(
        "call_terminal",
        "terminal",
        "stdout",
    )];

    sp.persist_session("tool-name-session", &messages, None, None, None, None)
        .unwrap();

    let loaded = sp.load_session("tool-name-session").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].role, MessageRole::Tool);
    assert_eq!(loaded[0].tool_call_id.as_deref(), Some("call_terminal"));
    assert_eq!(loaded[0].name.as_deref(), Some("terminal"));
    assert_eq!(loaded[0].content.as_deref(), Some("stdout"));
}

#[test]
fn test_migrates_reasoning_content_column_for_legacy_db() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("sessions.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            model TEXT,
            platform TEXT DEFAULT 'cli',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            title TEXT,
            message_count INTEGER DEFAULT 0
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT,
            tool_call_id TEXT,
            tool_calls TEXT,
            created_at TEXT NOT NULL
        );",
    )
    .unwrap();
    drop(conn);

    let sp = SessionPersistence::new(tmp.path());
    let mut assistant = Message::assistant("legacy");
    assistant.reasoning_content = Some("legacy-think".to_string());
    sp.persist_session("legacy-migrate", &[assistant], None, None, None, None)
        .unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let mut sessions_stmt = conn
        .prepare("PRAGMA table_info(sessions)")
        .expect("sessions pragma prepare");
    let session_cols = sessions_stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for column in [
        "system_prompt",
        "parent_session_id",
        "model_config",
        "end_reason",
        "ended_at",
    ] {
        assert!(
            session_cols.iter().any(|name| name == column),
            "missing migrated sessions.{column}"
        );
    }
    let mut stmt = conn
        .prepare("PRAGMA table_info(messages)")
        .expect("pragma prepare");
    let message_cols = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(message_cols.iter().any(|name| name == "reasoning_content"));
    assert!(message_cols.iter().any(|name| name == "name"));

    let loaded = sp.load_session("legacy-migrate").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].reasoning_content.as_deref(), Some("legacy-think"));
}

#[test]
fn test_save_session_log() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    let messages = vec![Message::user("What is 2+2?"), Message::assistant("4")];

    let path = sp
        .save_session_log("log-test", &messages, Some("gpt-4o"))
        .unwrap();

    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("What is 2+2?"));
    assert!(content.contains("👤 User"));
    assert!(content.contains("🤖 Assistant"));
}

#[test]
fn test_save_trajectory() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    let messages = vec![
        Message::user("Build a website"),
        Message::assistant("Sure, I'll help with that."),
    ];

    let path = sp
        .save_trajectory("traj-test", &messages, "Build a website", true)
        .unwrap();

    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["completed"], true);
    assert_eq!(parsed["user_query"], "Build a website");
}

#[test]
fn test_load_nonexistent_session() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.ensure_db().unwrap();

    let loaded = sp.load_session("nonexistent").unwrap();
    assert!(loaded.is_empty());
}

#[test]
fn test_persist_replaces_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    let messages1 = vec![Message::user("First")];
    sp.persist_session("replace-test", &messages1, None, None, None, None)
        .unwrap();

    let messages2 = vec![
        Message::user("First"),
        Message::assistant("Response"),
        Message::user("Second"),
    ];
    sp.persist_session("replace-test", &messages2, None, None, None, None)
        .unwrap();

    let loaded = sp.load_session("replace-test").unwrap();
    assert_eq!(loaded.len(), 3);
}

#[test]
fn failed_persist_keeps_the_previous_row_transcript_and_search() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let first = vec![Message::user("heron"), Message::assistant("willow")];
    sp.persist_session("rollback-test", &first, None, None, None, None)
        .unwrap();

    // Fail the second message insert of the next write.
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_insert BEFORE INSERT ON messages WHEN NEW.content = 'boom'
         BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
    )
    .unwrap();
    let second = vec![
        Message::user("kingfisher"),
        Message::assistant("boom"),
        Message::user("mill"),
    ];
    sp.persist_session("rollback-test", &second, None, None, None, None)
        .expect_err("forced insert failure");
    conn.execute_batch("DROP TRIGGER fail_insert").unwrap();

    let loaded = sp.load_session("rollback-test").unwrap();
    let contents: Vec<_> = loaded.iter().map(|m| m.content.as_deref()).collect();
    assert_eq!(contents, [Some("heron"), Some("willow")]);
    let count: i64 = conn
        .query_row(
            "SELECT message_count FROM sessions WHERE id = 'rollback-test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(fts_match_rows(&sp, "willow").len(), 1);
    assert!(fts_match_rows(&sp, "kingfisher").is_empty());
}

fn lineage_row(
    sp: &SessionPersistence,
    session_id: &str,
) -> (Option<String>, Option<String>, String, Option<String>) {
    rusqlite::Connection::open(&sp.db_path)
        .unwrap()
        .query_row(
            "SELECT parent_session_id, end_reason, created_at, model_config
             FROM sessions WHERE id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

fn fts_hit_sessions(sp: &SessionPersistence, query: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let mut stmt = conn
        .prepare("SELECT session_id FROM messages_fts WHERE messages_fts MATCH ?1 ORDER BY rowid")
        .unwrap();
    stmt.query_map([query], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn compressed_turns_stay_searchable_and_resume_follows_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let before = vec![
        Message::user("where did the heron nest"),
        Message::assistant("under the willow bridge"),
        Message::user("and the kingfisher"),
    ];
    sp.persist_session("live", &before[..2], Some("m"), None, None, None)
        .unwrap();
    // A branch of "origin": the marker belongs at the root of its chain.
    let started = "2026-11-01T00:00:00+00:00";
    set_session_lineage(&sp, "live", Some("origin"), None, started, None);
    let marker = r#"{"_branched_from":"origin"}"#;
    set_session_model_config(&sp, "live", marker);

    let after = vec![
        Message::system("summary"),
        Message::user("and the kingfisher"),
        Message::assistant("beside the old mill"),
    ];
    sp.persist_session_after_compression(
        "live",
        std::slice::from_ref(&before),
        &after,
        Some("m"),
        None,
        None,
    )
    .unwrap();

    let contents = |id: &str| -> Vec<Option<String>> {
        sp.load_session(id)
            .unwrap()
            .into_iter()
            .map(|m| m.content)
            .collect()
    };
    let expected = |messages: &[Message]| -> Vec<Option<String>> {
        messages.iter().map(|m| m.content.clone()).collect()
    };
    assert_eq!(contents("live"), expected(&after));
    let hits = fts_hit_sessions(&sp, "willow");
    assert_eq!(hits.len(), 1, "the replaced turn stays indexed once");
    let parent = hits[0].clone();
    assert_ne!(parent, "live");
    assert_eq!(contents(&parent), expected(&before));
    assert_eq!(
        lineage_row(&sp, &parent),
        (
            Some("origin".to_string()),
            Some("compression".to_string()),
            started.to_string(),
            Some(marker.to_string()),
        )
    );
    assert_eq!(
        lineage_row(&sp, "live"),
        (Some(parent.clone()), None, started.to_string(), None)
    );
    assert_eq!(sp.resolve_resume_session_id(&parent).unwrap(), "live");
}

#[test]
fn gateway_runs_keep_compressed_turns_searchable_and_the_system_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let result = AgentResult {
        messages: vec![
            Message::system("be brief"),
            Message::user("[Conversation summary] a heron"),
            Message::user("and the kingfisher"),
        ],
        compressed_transcripts: vec![vec![
            Message::system("be brief"),
            Message::user("where did the heron nest"),
            Message::assistant("under the willow bridge"),
        ]],
        ..AgentResult::default()
    };

    sp.persist_agent_result("live", &result, Some("m"), Some("telegram"))
        .unwrap();

    let hits = fts_hit_sessions(&sp, "willow");
    assert_eq!(hits.len(), 1, "the replaced turn stays indexed");
    assert_ne!(hits[0], "live");
    assert_eq!(sp.resolve_resume_session_id(&hits[0]).unwrap(), "live");
    assert_eq!(sp.load_session("live").unwrap().len(), 3);
    assert_eq!(
        sp.get_system_prompt("live").unwrap().as_deref(),
        Some("be brief")
    );
}

#[test]
fn gateway_results_save_only_under_a_session_key() {
    let tmp = tempfile::tempdir().unwrap();
    let home = format!(" {} ", tmp.path().display());
    let sp = SessionPersistence::new(tmp.path());
    let result = AgentResult {
        messages: vec![Message::user("heron")],
        ..AgentResult::default()
    };

    persist_gateway_result(Some(&home), " ", &result, "m", "telegram");
    assert!(!sp.db_path.exists(), "a blank session key saves nothing");

    persist_gateway_result(Some(&home), "live", &result, "m", "telegram");
    assert_eq!(sp.load_session("live").unwrap(), result.messages);
}

#[test]
fn gateway_results_that_fail_to_save_are_logged() {
    #[derive(Clone, Default)]
    struct Log(std::sync::Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Log {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    // A file where the home folder should be, so the save cannot open its database.
    let home = tmp.path().join("home");
    std::fs::write(&home, "not a folder").unwrap();
    let result = AgentResult {
        messages: vec![Message::user("heron")],
        ..AgentResult::default()
    };
    let log = Log::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();

    tracing::subscriber::with_default(subscriber, || {
        persist_gateway_result(home.to_str(), "chat-42", &result, "m", "telegram");
    });

    let logged = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains("WARN") && logged.contains("failed to save a telegram session"),
        "the failed save is logged: {logged:?}"
    );
    assert!(
        !logged.contains("chat-42"),
        "the session key stays out of the log"
    );
}

#[test]
fn pruning_keeps_compressed_turns_whilst_their_session_is_active() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let first = vec![Message::user("heron"), Message::assistant("willow")];
    let second = vec![Message::system("summary one"), Message::user("kingfisher")];
    let after = vec![Message::system("summary two"), Message::user("otter")];
    sp.persist_session_after_compression("live", &[first, second], &after, None, None, None)
        .unwrap();
    let older = fts_hit_sessions(&sp, "willow").remove(0);
    let newer = fts_hit_sessions(&sp, "kingfisher").remove(0);
    let rows = || -> i64 {
        rusqlite::Connection::open(&sp.db_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap()
    };

    mark_session_old(&sp, &older, 40);
    mark_session_old(&sp, &newer, 40);
    assert_eq!(sp.prune_sessions(30).unwrap(), 0);
    assert_eq!(rows(), 3);
    assert_eq!(fts_hit_sessions(&sp, "willow"), [older]);

    mark_session_old(&sp, "live", 40);
    assert_eq!(sp.prune_sessions(30).unwrap(), 3);
    assert_eq!(rows(), 0);
}

#[test]
fn compressions_in_one_save_chain_oldest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let first = vec![Message::user("heron"), Message::assistant("willow")];
    let second = vec![
        Message::system("summary one"),
        Message::user("kingfisher"),
        Message::assistant("mill"),
    ];
    let after = vec![Message::system("summary two"), Message::user("otter")];
    sp.persist_session_after_compression("live", &[first, second], &after, None, None, None)
        .unwrap();

    let older = fts_hit_sessions(&sp, "willow").remove(0);
    let newer = fts_hit_sessions(&sp, "kingfisher").remove(0);
    assert_eq!(lineage_row(&sp, &older).0, None);
    assert_eq!(lineage_row(&sp, &newer).0.as_deref(), Some(older.as_str()));
    assert_eq!(lineage_row(&sp, "live").0.as_deref(), Some(newer.as_str()));
    assert_eq!(sp.resolve_resume_session_id(&older).unwrap(), "live");
}

#[test]
fn failed_compressed_save_keeps_the_previous_session() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let first = vec![Message::user("heron"), Message::assistant("willow")];
    sp.persist_session("live", &first, None, None, None, None)
        .unwrap();

    // Fail while storing the replaced turns, after the session's own upsert.
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_insert BEFORE INSERT ON messages WHEN NEW.content = 'boom'
         BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
    )
    .unwrap();
    let replaced = vec![Message::user("heron"), Message::assistant("boom")];
    let after = vec![Message::system("summary"), Message::user("otter")];
    sp.persist_session_after_compression("live", &[replaced], &after, None, None, None)
        .expect_err("forced insert failure");
    conn.execute_batch("DROP TRIGGER fail_insert").unwrap();

    let loaded = sp.load_session("live").unwrap();
    let contents: Vec<_> = loaded.iter().map(|m| m.content.as_deref()).collect();
    assert_eq!(contents, [Some("heron"), Some("willow")]);
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    assert_eq!(lineage_row(&sp, "live").0, None);
    assert!(fts_hit_sessions(&sp, "otter").is_empty());
}

#[test]
fn test_persist_session_updates_existing_model() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("hello")];

    sp.persist_session(
        "model-update",
        &messages,
        Some("openai:gpt-4o"),
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "model-update",
        &messages,
        Some("nous:hermes-4"),
        Some("cli"),
        None,
        None,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let model: String = conn
        .query_row(
            "SELECT model FROM sessions WHERE id='model-update'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(model, "nous:hermes-4");
}

#[test]
fn resolve_resume_session_id_follows_compression_tip_with_nonempty_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "root",
        &[Message::user("pre-compression turn")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "cont",
        &[Message::assistant("post-compression reply")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    let base = Timestamp::now() - SignedDuration::from_hours(1);
    let root_created = rfc3339(base);
    let root_ended = rfc3339(base + SignedDuration::from_secs(10));
    let cont_created = rfc3339(base + SignedDuration::from_secs(20));
    set_session_lineage(
        &sp,
        "root",
        None,
        Some("compression"),
        &root_created,
        Some(&root_ended),
    );
    set_session_lineage(&sp, "cont", Some("root"), None, &cont_created, None);

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "cont");
}

#[test]
fn resolve_resume_session_id_ignores_non_compression_branch_child() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "root",
        &[Message::user("parent turn")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "branch",
        &[Message::assistant("delegated work")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    let base = Timestamp::now() - SignedDuration::from_hours(1);
    let root_created = rfc3339(base);
    let branch_created = rfc3339(base + SignedDuration::from_secs(20));
    set_session_lineage(&sp, "root", None, None, &root_created, None);
    set_session_lineage(&sp, "branch", Some("root"), None, &branch_created, None);

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "root");
}

#[test]
fn resolve_resume_session_id_follows_child_created_before_parent_end() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "root",
        &[Message::user("parent turn")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "early",
        &[Message::assistant("older branch")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    let base = Timestamp::now() - SignedDuration::from_hours(1);
    let root_created = rfc3339(base);
    let root_ended = rfc3339(base + SignedDuration::from_secs(30));
    let early_created = rfc3339(base + SignedDuration::from_secs(10));
    set_session_lineage(
        &sp,
        "root",
        None,
        Some("compression"),
        &root_created,
        Some(&root_ended),
    );
    set_session_lineage(&sp, "early", Some("root"), None, &early_created, None);

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "early");
}

#[test]
fn resolve_resume_session_id_prefers_live_race_continuation_over_closed_stale_sibling() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    for (id, content) in [
        ("root", "parent turn"),
        ("real", "real continuation"),
        ("stale", "stale websocket sibling"),
    ] {
        sp.persist_session(
            id,
            &[Message::assistant(content)],
            None,
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    }
    let base = Timestamp::now() - SignedDuration::from_hours(1);
    let root_created = rfc3339(base);
    let real_created = rfc3339(base + SignedDuration::from_secs(10));
    let root_ended = rfc3339(base + SignedDuration::from_secs(30));
    let stale_created = rfc3339(base + SignedDuration::from_secs(40));
    let stale_ended = rfc3339(base + SignedDuration::from_secs(50));
    let real_message_at = rfc3339(base + SignedDuration::from_secs(60));
    let stale_message_at = rfc3339(base + SignedDuration::from_secs(70));
    set_session_lineage(
        &sp,
        "root",
        None,
        Some("compression"),
        &root_created,
        Some(&root_ended),
    );
    set_session_lineage(&sp, "real", Some("root"), None, &real_created, None);
    set_session_lineage(
        &sp,
        "stale",
        Some("root"),
        None,
        &stale_created,
        Some(&stale_ended),
    );
    set_message_created_at(&sp, "real", &real_message_at);
    set_message_created_at(&sp, "stale", &stale_message_at);

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "real");
}

#[test]
fn resolve_resume_session_id_excludes_branch_delegate_and_tool_children() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    for id in ["root", "branch", "delegate", "tool", "cont"] {
        sp.persist_session(
            id,
            &[Message::assistant(format!("{id} message"))],
            None,
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    }
    let base = Timestamp::now() - SignedDuration::from_hours(1);
    let root_created = rfc3339(base);
    let root_ended = rfc3339(base + SignedDuration::from_secs(10));
    set_session_lineage(
        &sp,
        "root",
        None,
        Some("compression"),
        &root_created,
        Some(&root_ended),
    );
    for (offset, id) in [(20, "cont"), (30, "branch"), (40, "delegate"), (50, "tool")] {
        let created = rfc3339(base + SignedDuration::from_secs(offset));
        set_session_lineage(&sp, id, Some("root"), None, &created, None);
        let msg_created = rfc3339(base + SignedDuration::from_secs(offset + 100));
        set_message_created_at(&sp, id, &msg_created);
    }
    set_session_model_config(&sp, "branch", r#"{"_branched_from":"root"}"#);
    set_session_model_config(&sp, "delegate", r#"{"_delegate_from":"root"}"#);
    set_session_platform(&sp, "tool", "tool");

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "cont");
}

#[test]
fn resolve_resume_session_id_walks_compression_chain_to_latest_tip() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    for id in ["root", "mid", "tip"] {
        sp.persist_session(
            id,
            &[Message::assistant(format!("{id} message"))],
            None,
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    }
    let base = Timestamp::now() - SignedDuration::from_hours(1);
    let root_created = rfc3339(base);
    let root_ended = rfc3339(base + SignedDuration::from_secs(10));
    let mid_created = rfc3339(base + SignedDuration::from_secs(20));
    let mid_ended = rfc3339(base + SignedDuration::from_secs(30));
    let tip_created = rfc3339(base + SignedDuration::from_secs(40));
    set_session_lineage(
        &sp,
        "root",
        None,
        Some("compression"),
        &root_created,
        Some(&root_ended),
    );
    set_session_lineage(
        &sp,
        "mid",
        Some("root"),
        Some("compression"),
        &mid_created,
        Some(&mid_ended),
    );
    set_session_lineage(&sp, "tip", Some("mid"), None, &tip_created, None);

    assert_eq!(sp.resolve_resume_session_id("root").unwrap(), "tip");
}

#[test]
fn delete_session_removes_rows_search_entries_and_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = [Message::user("remember the falcon")];
    for id in ["gone", "kept", "branch"] {
        sp.persist_session(id, &messages, Some("gpt-4o"), Some("cli"), None, None)
            .unwrap();
    }
    set_session_lineage(
        &sp,
        "branch",
        Some("gone"),
        None,
        &hermes_core::time::now_rfc3339(),
        None,
    );
    std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
    let transcript = tmp.path().join("sessions").join("gone.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();

    assert!(sp.delete_session("gone").unwrap());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).unwrap() };
    assert_eq!(count("SELECT COUNT(*) FROM sessions WHERE id = 'gone'"), 0);
    assert_eq!(
        count("SELECT COUNT(*) FROM messages WHERE session_id = 'gone'"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM messages_fts WHERE session_id = 'gone'"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM messages WHERE session_id = 'kept'"),
        1
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM messages_fts WHERE session_id = 'kept'"),
        1
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM sessions WHERE id = 'branch' AND parent_session_id IS NULL"),
        1
    );
    assert!(!transcript.exists());
    assert!(!sp.delete_session("gone").unwrap());
    assert!(sp.delete_session("../gone").is_err());
}

/// Archives one tool result for each session; returns the archive folder.
fn archive_a_result_for(home: &Path, sessions: &[&str]) -> PathBuf {
    use crate::context::tool_result_prune::{prune_tool_results, PRUNED_SUBDIR};
    let archives = home.join(PRUNED_SUBDIR);
    for session in sessions {
        let mut history = vec![
            Message::user("show"),
            Message::tool_result("c1", "x".repeat(9_000)),
        ];
        history.extend((0..20).map(|i| Message::assistant(format!("a{i}"))));
        let len = history.len();
        assert_eq!(
            prune_tool_results(&mut history, len, &archives, session, 0).pruned,
            1
        );
    }
    archives
}

fn archive_names(archives: &Path) -> Vec<String> {
    std::fs::read_dir(archives)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn delete_session_removes_the_sessions_tool_result_archives() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let archives = archive_a_result_for(tmp.path(), &["gone", "kept"]);

    sp.delete_session("gone").unwrap();

    let left = archive_names(&archives);
    assert_eq!(left.len(), 1);
    assert!(left[0].starts_with("kept."), "{left:?}");
}

#[test]
fn pruning_old_sessions_removes_their_tool_result_archives() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    for session in ["old", "recent"] {
        sp.persist_session(session, &[Message::user("hi")], None, None, None, None)
            .unwrap();
    }
    let archives = archive_a_result_for(tmp.path(), &["old", "recent"]);
    mark_session_old(&sp, "old", 40);

    assert_eq!(sp.prune_sessions(30).unwrap(), 1);

    let left = archive_names(&archives);
    assert_eq!(left.len(), 1);
    assert!(left[0].starts_with("recent."), "{left:?}");
}

#[test]
fn delete_session_changes_nothing_when_the_store_lacks_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "child",
        &[Message::user("hi")],
        Some("gpt-4o"),
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    set_session_lineage(
        &sp,
        "child",
        Some("ghost"),
        None,
        &hermes_core::time::now_rfc3339(),
        None,
    );

    assert!(!sp.delete_session("ghost").unwrap());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let linked: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = 'child' AND parent_session_id = 'ghost'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(linked, 1);
}

#[test]
fn delete_session_if_empty_deletes_empty_untitled_row_and_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session("empty", &[], Some("gpt-4o"), Some("cli"), None, None)
        .unwrap();
    std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
    std::fs::write(
        tmp.path().join("sessions").join("empty.json"),
        r#"{"session_info":{"session_id":"empty"},"messages":[]}"#,
    )
    .unwrap();

    assert!(sp.delete_session_if_empty("empty").unwrap());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id='empty'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert!(!tmp.path().join("sessions").join("empty.json").exists());
}

#[test]
fn delete_session_if_empty_preserves_sessions_with_messages_title_or_children() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "with-message",
        &[Message::user("hello")],
        None,
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    sp.persist_session("titled", &[], None, Some("cli"), Some("Plans"), None)
        .unwrap();
    sp.persist_session("parent", &[], None, Some("cli"), None, None)
        .unwrap();
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let now = hermes_core::time::now_rfc3339();
    conn.execute(
        "INSERT INTO sessions (id, model, platform, created_at, updated_at, parent_session_id)
         VALUES ('child', 'gpt-4o', 'cli', ?1, ?1, 'parent')",
        rusqlite::params![now],
    )
    .unwrap();
    drop(conn);

    assert!(!sp.delete_session_if_empty("with-message").unwrap());
    assert!(!sp.delete_session_if_empty("titled").unwrap());
    assert!(!sp.delete_session_if_empty("parent").unwrap());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    for kept in ["with-message", "titled", "parent", "child"] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id = ?1",
                rusqlite::params![kept],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "{kept} should remain");
    }
}

#[test]
fn delete_session_if_empty_keeps_nonempty_snapshot_file() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session("empty-db", &[], Some("gpt-4o"), Some("cli"), None, None)
        .unwrap();
    let sessions_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let snapshot_path = sessions_dir.join("empty-db.json");
    std::fs::write(
        &snapshot_path,
        r#"{"session_info":{"session_id":"empty-db"},"messages":[{"role":"User","content":"keep"}]}"#,
    )
    .unwrap();

    assert!(sp.delete_session_if_empty("empty-db").unwrap());
    assert!(snapshot_path.exists());
}

#[test]
fn test_update_session_model_only_updates_existing_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("hello")];

    assert!(!sp
        .update_session_model("missing-session", "openai:gpt-4o")
        .unwrap());

    sp.persist_session(
        "existing-session",
        &messages,
        Some("openai:gpt-4o"),
        Some("cli"),
        None,
        None,
    )
    .unwrap();
    assert!(sp
        .update_session_model("existing-session", "anthropic:claude-sonnet")
        .unwrap());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let model: String = conn
        .query_row(
            "SELECT model FROM sessions WHERE id='existing-session'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(model, "anthropic:claude-sonnet");
}

#[test]
fn test_rewind_soft_deletes_n_user_turns_and_loads_only_active_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![
        Message::system("sys"),
        Message::user("question 1"),
        Message::assistant("answer 1"),
        Message::user("question 2"),
        Message::assistant("answer 2"),
        Message::user("question 3"),
        Message::assistant("answer 3"),
    ];
    sp.persist_session("rewind-n", &messages, None, None, None, None)
        .unwrap();

    let recent = sp.list_recent_user_messages("rewind-n", 2).unwrap();
    assert_eq!(
        recent
            .iter()
            .filter_map(|row| row.content.as_deref())
            .collect::<Vec<_>>(),
        vec!["question 3", "question 2"]
    );

    let outcome = sp.rewind_active_user_turns("rewind-n", 2).unwrap().unwrap();
    assert_eq!(outcome.target_content.as_deref(), Some("question 2"));
    assert_eq!(outcome.inactive_count, 4);
    assert_eq!(outcome.active_message_count, 3);
    assert_eq!(outcome.rewind_count, 1);

    let loaded = sp.load_session("rewind-n").unwrap();
    assert_eq!(
        loaded
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>(),
        vec!["sys", "question 1", "answer 1"]
    );

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let inactive: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id='rewind-n' AND active=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(inactive, 4);
    let rewind_count: i64 = conn
        .query_row(
            "SELECT rewind_count FROM sessions WHERE id='rewind-n'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rewind_count, 1);

    assert_eq!(
        sp.restore_rewound_since("rewind-n", outcome.target_message_id)
            .unwrap(),
        4
    );
    assert_eq!(sp.load_session("rewind-n").unwrap().len(), messages.len());
}

#[test]
fn test_persist_session_preserves_inactive_rewind_audit_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![
        Message::user("keep"),
        Message::assistant("kept"),
        Message::user("rewind me"),
        Message::assistant("rewound"),
    ];
    sp.persist_session("audit-preserve", &messages, None, None, None, None)
        .unwrap();
    sp.rewind_active_user_turns("audit-preserve", 1)
        .unwrap()
        .unwrap();

    sp.persist_session(
        "audit-preserve",
        &[Message::user("keep"), Message::assistant("replacement")],
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let inactive: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id='audit-preserve' AND active=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(inactive, 2);
    assert_eq!(sp.load_session("audit-preserve").unwrap().len(), 2);
}

#[test]
fn test_replacing_messages_clears_old_fts_rows_when_available() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    sp.persist_session(
        "fts-replace",
        &[Message::user("needlebefore")],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "fts-replace",
        &[Message::user("needleafter")],
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    if SessionPersistence::fts_table_exists(&conn).unwrap() {
        let old_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'needlebefore'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'needleafter'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_hits, 0);
        assert_eq!(new_hits, 1);
    }
}

#[test]
fn test_fts_unavailable_error_classifier_matches_missing_modules() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let err = conn
        .execute_batch("CREATE VIRTUAL TABLE broken_fts USING definitely_missing_module(content);")
        .expect_err("missing virtual table module should fail");
    assert!(SessionPersistence::is_fts5_unavailable_error(&err));
}

#[test]
fn test_optimize_fts_returns_existing_index_count() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "opt-count",
        &[Message::user("hello optimized world")],
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let expected = u32::from(SessionPersistence::fts_table_exists(&conn).unwrap());
    assert_eq!(sp.optimize_fts().unwrap(), expected);
}

#[test]
fn test_optimize_fts_preserves_search_rows_and_snippets() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = (0..50)
        .map(|idx| Message::user(format!("needle alpha bravo charlie message {idx}")))
        .collect::<Vec<_>>();
    sp.persist_session("opt-search", &messages, None, None, None, None)
        .unwrap();

    let before = fts_match_rows(&sp, "needle");
    if before.is_empty() {
        assert_eq!(sp.optimize_fts().unwrap(), 0);
        return;
    }

    assert_eq!(sp.optimize_fts().unwrap(), 1);
    let after = fts_match_rows(&sp, "needle");
    assert_eq!(after, before);
    assert!(after
        .iter()
        .all(|(_, snippet)| snippet.contains("[needle]")));
}

#[test]
fn test_optimize_fts_skips_missing_optional_trigram_table() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "opt-missing-trigram",
        &[Message::user("trigram optional")],
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    assert!(!SessionPersistence::named_fts_table_exists(&conn, "messages_fts_trigram").unwrap());
    let expected = u32::from(SessionPersistence::fts_table_exists(&conn).unwrap());
    assert_eq!(sp.optimize_fts().unwrap(), expected);
}

#[test]
fn test_optimize_fts_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "opt-idempotent",
        &[Message::user("repeatable optimize marker")],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let expected = u32::from(SessionPersistence::fts_table_exists(&conn).unwrap());

    assert_eq!(sp.optimize_fts().unwrap(), expected);
    assert_eq!(sp.optimize_fts().unwrap(), expected);
    assert_eq!(sp.load_session("opt-idempotent").unwrap().len(), 1);
    if expected > 0 {
        assert_eq!(fts_match_rows(&sp, "repeatable").len(), 1);
    }
}

#[test]
fn test_vacuum_runs_after_fts_optimize_without_changing_search() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    sp.persist_session(
        "vacuum-opt",
        &[Message::user("vacuum needle still searchable")],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let before = fts_match_rows(&sp, "needle");

    sp.vacuum().unwrap();

    assert_eq!(fts_match_rows(&sp, "needle"), before);
}

#[test]
fn test_state_meta_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    assert_eq!(sp.get_meta("nope").unwrap(), None);
    sp.set_meta("k1", "v1").unwrap();
    assert_eq!(sp.get_meta("k1").unwrap().as_deref(), Some("v1"));
    sp.set_meta("k1", "v2").unwrap();
    assert_eq!(sp.get_meta("k1").unwrap().as_deref(), Some("v2"));
}

#[test]
fn test_truncate_wal_checkpoint_shrinks_sessions_wal() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let (_conn, wal_path, before) = grow_sessions_wal(&sp);

    sp.truncate_wal_checkpoint().unwrap();

    assert_eq!(
        wal_len(&wal_path),
        0,
        "TRUNCATE checkpoint should shrink WAL from {before} bytes"
    );
}

#[test]
fn test_auto_maintenance_truncates_wal_after_meta_write() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let (_conn, wal_path, before) = grow_sessions_wal(&sp);

    let result = sp.maybe_auto_prune_and_vacuum(90, 0, false);

    assert!(!result.skipped);
    assert_eq!(result.pruned, 0);
    assert!(result.error.is_none());
    assert_eq!(
        wal_len(&wal_path),
        0,
        "auto-maintenance should truncate WAL from {before} bytes"
    );
}

#[test]
fn test_auto_maintenance_prunes_and_vacuums() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let old_messages = vec![Message::user("old"), Message::assistant("done")];
    let new_messages = vec![Message::user("fresh"), Message::assistant("ok")];

    sp.persist_session("old-1", &old_messages, None, None, None, None)
        .unwrap();
    sp.persist_session("old-2", &old_messages, None, None, None, None)
        .unwrap();
    sp.persist_session("fresh-1", &new_messages, None, None, None, None)
        .unwrap();
    mark_session_old(&sp, "old-1", 100);
    mark_session_old(&sp, "old-2", 100);

    let result = sp.maybe_auto_prune_and_vacuum(90, 24, true);
    assert!(!result.skipped);
    assert_eq!(result.pruned, 2);
    assert!(result.vacuumed);
    assert!(result.error.is_none());

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 1);
}

#[test]
fn test_auto_maintenance_respects_interval_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("old"), Message::assistant("done")];

    sp.persist_session("old-1", &messages, None, None, None, None)
        .unwrap();
    mark_session_old(&sp, "old-1", 100);
    let first = sp.maybe_auto_prune_and_vacuum(90, 24, false);
    assert!(!first.skipped);
    assert_eq!(first.pruned, 1);

    sp.persist_session("old-2", &messages, None, None, None, None)
        .unwrap();
    mark_session_old(&sp, "old-2", 100);
    let second = sp.maybe_auto_prune_and_vacuum(90, 24, false);
    assert!(second.skipped);
    assert_eq!(second.pruned, 0);

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let still_there: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions WHERE id='old-2'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(still_there, 1);
}

#[test]
fn test_auto_maintenance_no_prunable_rows_skips_vacuum() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("fresh"), Message::assistant("ok")];
    sp.persist_session("fresh-1", &messages, None, None, None, None)
        .unwrap();

    let result = sp.maybe_auto_prune_and_vacuum(90, 24, true);
    assert!(!result.skipped);
    assert_eq!(result.pruned, 0);
    assert!(!result.vacuumed);
    assert!(sp.get_meta("last_auto_prune").unwrap().is_some());
}

#[test]
fn test_auto_maintenance_corrupt_marker_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = vec![Message::user("old"), Message::assistant("done")];
    sp.persist_session("old-1", &messages, None, None, None, None)
        .unwrap();
    mark_session_old(&sp, "old-1", 100);
    sp.set_meta("last_auto_prune", "not-a-timestamp").unwrap();

    let result = sp.maybe_auto_prune_and_vacuum(90, 24, false);
    assert!(!result.skipped);
    assert_eq!(result.pruned, 1);
}

fn imported_child(messages: &[(Message, String)]) -> ImportedSession<'_> {
    ImportedSession {
        id: "child",
        model: Some("glm-5.3"),
        platform: Some("cli"),
        title: None,
        system_prompt: None,
        model_config: Some(r#"{"reasoning_effort":"high"}"#),
        parent_session_id: Some("parent"),
        created_at: "2026-10-01T09:00:00+00:00",
        updated_at: "2026-10-01T09:30:00+00:00",
        ended_at: Some("2026-10-01T09:31:00+00:00"),
        end_reason: Some("compression"),
        messages,
    }
}

#[test]
fn import_sessions_keeps_recorded_times_lineage_and_end_state() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let transcript = [
        Message::user("imported question"),
        Message::assistant("imported answer"),
    ];
    let message_times = ["2026-10-01T09:00:05+00:00", "2026-10-01T09:29:00.500+00:00"];
    let messages: Vec<(Message, String)> = transcript
        .iter()
        .cloned()
        .zip(message_times.map(String::from))
        .collect();
    sp.import_sessions(|import| import.add_session(&imported_child(&messages)))
        .unwrap();

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let row: (String, String, String, String, String, String, i64) = conn
        .query_row(
            "SELECT created_at, updated_at, parent_session_id, model_config, end_reason,
                    ended_at, message_count
             FROM sessions WHERE id = 'child'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "2026-10-01T09:00:00+00:00".to_string(),
            "2026-10-01T09:30:00+00:00".to_string(),
            "parent".to_string(),
            r#"{"reasoning_effort":"high"}"#.to_string(),
            "compression".to_string(),
            "2026-10-01T09:31:00+00:00".to_string(),
            2
        )
    );
    let stored_times: Vec<String> = conn
        .prepare("SELECT created_at FROM messages WHERE session_id = 'child' ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(stored_times, message_times);
    assert_eq!(sp.load_session("child").unwrap(), transcript);
    assert_eq!(fts_match_rows(&sp, "imported").len(), 2);
}

#[test]
fn import_sessions_writes_nothing_when_the_import_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let messages = [(
        Message::user("imported question"),
        "2026-10-01T09:00:05+00:00".to_string(),
    )];
    let error = sp
        .import_sessions(|import| {
            import.add_session(&imported_child(&messages))?;
            Err::<(), _>(AgentError::Config("later record is invalid".into()))
        })
        .unwrap_err();
    assert!(error.to_string().contains("later record is invalid"));

    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    let rows: i64 = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM sessions) + (SELECT COUNT(*) FROM messages)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}

/// The delete and update triggers this crate created before they used FTS5's 'delete' command.
const OLD_FTS_TRIGGERS: &str = "DROP TRIGGER messages_ad;
    DROP TRIGGER messages_au;
    CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
        DELETE FROM messages_fts WHERE rowid = old.id;
    END;
    CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
        DELETE FROM messages_fts WHERE rowid = old.id;
        INSERT INTO messages_fts(rowid, content, session_id, role)
        VALUES (new.id, new.content, new.session_id, new.role);
    END;";

fn open_db(sp: &SessionPersistence) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(&sp.db_path).unwrap();
    assert!(SessionPersistence::fts_table_exists(&conn).unwrap());
    conn
}

/// FTS5's own check of the index against `messages`.
fn fts_index_check(conn: &rusqlite::Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO messages_fts(messages_fts, rank) VALUES('integrity-check', 1)",
        [],
    )
}

fn fts_hits(conn: &rusqlite::Connection, word: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1",
        params![word],
        |row| row.get(0),
    )
    .unwrap()
}

fn fts_delete_triggers(conn: &rusqlite::Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT sql FROM sqlite_master
             WHERE type = 'trigger' AND name IN ('messages_ad', 'messages_au') ORDER BY name",
        )
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn fts_index_blocks(conn: &rusqlite::Connection) -> Vec<(i64, Vec<u8>)> {
    let mut stmt = conn
        .prepare("SELECT id, block FROM messages_fts_data ORDER BY id")
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A store indexed under the old triggers, whose deleted and edited messages left it damaged.
fn store_damaged_by_old_fts_triggers(sp: &SessionPersistence) {
    sp.persist_session(
        "kept",
        &[
            Message::user("golf lemon"),
            Message::assistant("hotel lemon"),
        ],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "dropped",
        &[Message::user("india lime")],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let conn = open_db(sp);
    conn.execute_batch(OLD_FTS_TRIGGERS).unwrap();
    conn.execute("DELETE FROM messages WHERE session_id = 'dropped'", [])
        .unwrap();
    conn.execute(
        "UPDATE messages SET content = 'juliet melon' WHERE content = 'golf lemon'",
        [],
    )
    .unwrap();
    let damage = fts_index_check(&conn).expect_err("the old triggers damage the index");
    assert!(
        SessionPersistence::is_fts_index_mismatch(&damage),
        "{damage}"
    );
}

#[test]
fn fts_index_matches_messages_after_rewind_restore_rewrite_prune_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    let turns = [
        Message::user("alpha kiwi"),
        Message::assistant("bravo kiwi"),
        Message::user("charlie mango"),
        Message::assistant("delta mango"),
    ];
    sp.persist_session("kept", &turns, None, None, None, None)
        .unwrap();
    let rewound = sp
        .rewind_active_user_turns("kept", 1)
        .unwrap()
        .expect("a turn to rewind");
    sp.restore_rewound_since("kept", rewound.target_message_id)
        .unwrap();
    sp.rewind_active_user_turns("kept", 1).unwrap();
    // Rewrites the live transcript and keeps the rewound rows.
    sp.persist_session(
        "kept",
        &[
            Message::user("alpha kiwi"),
            Message::assistant("echo papaya"),
        ],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    sp.persist_session(
        "deleted",
        &[Message::user("foxtrot quince")],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert!(sp.delete_session("deleted").unwrap());
    sp.persist_session(
        "pruned",
        &[Message::user("golf raisin")],
        None,
        None,
        None,
        None,
    )
    .unwrap();
    mark_session_old(&sp, "pruned", 40);
    assert_eq!(sp.prune_sessions(30).unwrap(), 1);

    let conn = open_db(&sp);
    fts_index_check(&conn).expect("the index matches messages");
    assert_eq!(fts_hits(&conn, "quince"), 0);
    assert_eq!(fts_hits(&conn, "raisin"), 0);
    assert_eq!(fts_hits(&conn, "papaya"), 1);
    assert_eq!(fts_hits(&conn, "mango"), 2, "rewound rows stay indexed");
}

#[test]
fn opening_a_store_replaces_the_old_fts_triggers_and_rebuilds_the_index_they_damaged() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    store_damaged_by_old_fts_triggers(&sp);

    sp.ensure_db().unwrap();

    let conn = open_db(&sp);
    fts_index_check(&conn).expect("the index is rebuilt");
    assert!(fts_delete_triggers(&conn)
        .iter()
        .all(|sql| sql.contains("'delete'")));
    assert_eq!(fts_hits(&conn, "lime"), 0);
    assert_eq!(fts_hits(&conn, "golf"), 0);
    assert_eq!(fts_hits(&conn, "melon"), 1);
    assert_eq!(fts_hits(&conn, "lemon"), 1);
    // The new triggers keep it sound.
    conn.execute("DELETE FROM messages WHERE content = 'juliet melon'", [])
        .unwrap();
    fts_index_check(&conn).expect("the index matches messages after a delete");
}

#[test]
fn a_sound_fts_index_keeps_its_data_when_its_old_triggers_are_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    // Separate writes give the index several segments, which a rebuild would merge.
    for (id, text) in [("one", "kilo"), ("two", "lima"), ("three", "mike")] {
        sp.persist_session(id, &[Message::user(text)], None, None, None, None)
            .unwrap();
    }
    let conn = open_db(&sp);
    conn.execute_batch(OLD_FTS_TRIGGERS).unwrap();
    fts_index_check(&conn).expect("inserts alone keep the index sound");
    let blocks = fts_index_blocks(&conn);

    sp.ensure_db().unwrap();

    assert_eq!(fts_index_blocks(&conn), blocks, "the index was rewritten");
    let triggers = fts_delete_triggers(&conn);
    assert!(triggers.iter().all(|sql| sql.contains("'delete'")));
    // A current store is left as it is.
    sp.ensure_db().unwrap();
    assert_eq!(fts_index_blocks(&conn), blocks);
    assert_eq!(fts_delete_triggers(&conn), triggers);
}

#[test]
fn the_fts_trigger_repair_commits_once_and_a_refused_commit_changes_nothing() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Commits made by one `ensure_fts_schema`; a hook returning true turns each into a rollback.
    fn repair_commits(sp: &SessionPersistence, refuse: bool) -> usize {
        let commits = Arc::new(AtomicUsize::new(0));
        let mut conn = open_db(sp);
        let seen = Arc::clone(&commits);
        conn.commit_hook(Some(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            refuse
        }))
        .unwrap();
        SessionPersistence::ensure_fts_schema(&conn, &sp.db_path).unwrap();
        commits.load(Ordering::SeqCst)
    }

    let refused_dir = tempfile::tempdir().unwrap();
    let refused = SessionPersistence::new(refused_dir.path());
    store_damaged_by_old_fts_triggers(&refused);
    let old_triggers = fts_delete_triggers(&open_db(&refused));
    assert_eq!(repair_commits(&refused, true), 1, "one transaction");
    // A failed repair is not retried in this process, so later opens skip the full check.
    assert_eq!(repair_commits(&refused, false), 0);
    let conn = open_db(&refused);
    assert!(fts_index_check(&conn).is_err(), "the index was changed");
    assert_eq!(fts_delete_triggers(&conn), old_triggers);

    let repaired_dir = tempfile::tempdir().unwrap();
    let repaired = SessionPersistence::new(repaired_dir.path());
    store_damaged_by_old_fts_triggers(&repaired);
    assert_eq!(repair_commits(&repaired, false), 1, "one transaction");
    fts_index_check(&open_db(&repaired)).expect("the index is rebuilt");
}

#[test]
fn only_an_index_that_no_longer_matches_its_messages_is_rebuilt() {
    let failure = |extended_code| {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(extended_code), None)
    };
    assert!(SessionPersistence::is_fts_index_mismatch(&failure(
        rusqlite::ffi::SQLITE_CORRUPT_VTAB
    )));
    // Damage to the database file itself, which a rebuild would write into.
    assert!(!SessionPersistence::is_fts_index_mismatch(&failure(
        rusqlite::ffi::SQLITE_CORRUPT
    )));
    assert!(!SessionPersistence::is_fts_index_mismatch(&failure(
        rusqlite::ffi::SQLITE_BUSY
    )));
}

#[test]
fn a_repair_blocked_by_another_writer_runs_once_the_lock_is_released() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    store_damaged_by_old_fts_triggers(&sp);
    let old_triggers = fts_delete_triggers(&open_db(&sp));
    let writer = open_db(&sp);
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();

    let conn = open_db(&sp);
    SessionPersistence::ensure_fts_schema(&conn, &sp.db_path).unwrap();
    assert_eq!(
        fts_delete_triggers(&conn),
        old_triggers,
        "repaired under a lock"
    );

    writer.execute_batch("COMMIT").unwrap();
    SessionPersistence::ensure_fts_schema(&conn, &sp.db_path).unwrap();
    fts_index_check(&conn).expect("the index is rebuilt once the lock is free");
    assert!(fts_delete_triggers(&conn)
        .iter()
        .all(|sql| sql.contains("'delete'")));
}

#[test]
fn a_repair_whose_commit_waits_on_a_reader_runs_once_the_reader_finishes() {
    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    store_damaged_by_old_fts_triggers(&sp);
    let old_triggers = fts_delete_triggers(&open_db(&sp));
    // A read transaction's SHARED lock, which the repair's COMMIT has to wait out.
    let reader = open_db(&sp);
    reader.execute_batch("BEGIN").unwrap();
    reader
        .query_row("SELECT COUNT(*) FROM messages", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();

    let conn = open_db(&sp);
    conn.busy_timeout(std::time::Duration::from_millis(50))
        .unwrap();
    SessionPersistence::ensure_fts_schema(&conn, &sp.db_path).unwrap();
    assert_eq!(
        fts_delete_triggers(&conn),
        old_triggers,
        "committed under a reader"
    );

    reader.execute_batch("COMMIT").unwrap();
    SessionPersistence::ensure_fts_schema(&conn, &sp.db_path).unwrap();
    fts_index_check(&conn).expect("the index is rebuilt once the reader has finished");
    assert!(fts_delete_triggers(&conn)
        .iter()
        .all(|sql| sql.contains("'delete'")));
}

#[test]
fn a_failed_repair_of_a_store_missing_a_trigger_changes_none_of_its_triggers() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());
    store_damaged_by_old_fts_triggers(&sp);
    let conn = open_db(&sp);
    conn.execute_batch("DROP TRIGGER messages_au").unwrap();
    let before = fts_delete_triggers(&conn);
    assert_eq!(before.len(), 1);

    // Refusing writes to the index makes the repair's integrity check fail.
    let mut failing = open_db(&sp);
    failing
        .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Insert {
                table_name: "messages_fts",
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    let failed = SessionPersistence::ensure_fts_schema(&failing, &sp.db_path);
    drop(failing);

    assert_eq!(fts_delete_triggers(&conn), before, "a trigger changed");
    // A store missing a trigger is not usable as it is, so the failure is reported.
    assert!(failed.is_err());
    // Without both triggers the store is not left alone: the next open repairs it.
    SessionPersistence::ensure_fts_schema(&conn, &sp.db_path).unwrap();
    fts_index_check(&conn).expect("the index is rebuilt");
    assert_eq!(fts_delete_triggers(&conn).len(), 2);
}
