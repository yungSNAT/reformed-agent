use super::SessionPersistence;
use hermes_core::AgentError;
use rusqlite::Connection;

#[test]
fn absent_session_database_is_not_created_by_metadata_queries_or_updates() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = SessionPersistence::new(directory.path());

    assert_eq!(persistence.get_session_model("session").unwrap(), None);
    assert!(!persistence
        .update_session_model("session", "model")
        .unwrap());
    assert!(!persistence
        .update_session_lineage("session", None, None, None, None)
        .unwrap());
    assert_eq!(
        persistence.get_compression_tip("  session  ").unwrap(),
        "session"
    );
    assert!(!persistence.db_path().exists());
}

#[test]
fn an_empty_database_has_no_session_metadata_or_continuation() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = SessionPersistence::new(directory.path());
    let database = Connection::open(persistence.db_path()).unwrap();

    assert_eq!(persistence.get_session_model("session").unwrap(), None);
    assert!(!persistence
        .update_session_model("session", "model")
        .unwrap());
    assert!(matches!(
        persistence.update_session_lineage("session", None, None, None, None),
        Err(AgentError::Io(message)) if message.starts_with("Failed to update session lineage:")
    ));
    assert_eq!(
        persistence.get_compression_tip("session").unwrap(),
        "session"
    );

    let tables = database
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'table'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
}

#[test]
fn legacy_session_schema_preserves_rows_when_metadata_columns_are_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = SessionPersistence::new(directory.path());
    let database = Connection::open(persistence.db_path()).unwrap();
    database
        .execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, note TEXT NOT NULL);
             CREATE TABLE messages (session_id TEXT NOT NULL, created_at TEXT NOT NULL);
             INSERT INTO sessions VALUES ('session', 'keep this record');",
        )
        .unwrap();

    assert!(matches!(
        persistence.get_session_model("session"),
        Err(AgentError::Io(message)) if message.starts_with("Failed to prepare session model query:")
    ));
    assert!(matches!(
        persistence.update_session_model("session", "model"),
        Err(AgentError::Io(message)) if message.starts_with("Failed to update session model:")
    ));
    assert!(matches!(
        persistence.update_session_lineage("session", None, None, None, None),
        Err(AgentError::Io(message)) if message.starts_with("Failed to update session lineage:")
    ));
    assert_eq!(
        persistence.get_compression_tip("session").unwrap(),
        "session"
    );

    let note: String = database
        .query_row(
            "SELECT note FROM sessions WHERE id = 'session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(note, "keep this record");
}

#[test]
fn titles_are_neither_read_nor_written_without_a_session_id_or_database() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = SessionPersistence::new(directory.path());

    assert!(!persistence.update_session_title("  ", "title").unwrap());
    assert!(!persistence
        .update_session_title("session", "title")
        .unwrap());
    assert_eq!(persistence.get_session_title(" ").unwrap(), None);
    assert_eq!(persistence.get_session_title("session").unwrap(), None);
    assert!(!persistence.db_path().exists());
}

#[test]
fn a_database_without_a_sessions_table_has_no_titles_to_set() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = SessionPersistence::new(directory.path());
    let database = Connection::open(persistence.db_path()).unwrap();
    assert!(persistence.db_path().exists());

    assert!(!persistence
        .update_session_title("session", "title")
        .unwrap());
    assert_eq!(persistence.get_session_title("session").unwrap(), None);

    let tables = database
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'table'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
}

#[test]
fn a_sessions_table_without_a_title_column_reports_the_failure() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = SessionPersistence::new(directory.path());
    Connection::open(persistence.db_path())
        .unwrap()
        .execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY);
             INSERT INTO sessions VALUES ('session');",
        )
        .unwrap();

    assert!(matches!(
        persistence.update_session_title("session", "title"),
        Err(AgentError::Io(message)) if message.starts_with("Failed to update session title:")
    ));
    assert!(matches!(
        persistence.get_session_title("session"),
        Err(AgentError::Io(message)) if message.starts_with("Failed to read session title:")
    ));
}

#[test]
fn a_set_title_reads_back_and_a_blank_or_absent_one_reads_as_none() {
    let directory = tempfile::tempdir().unwrap();
    let persistence = SessionPersistence::new(directory.path());
    let database = Connection::open(persistence.db_path()).unwrap();
    database
        .execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, updated_at TEXT);
             INSERT INTO sessions VALUES ('session', NULL, NULL);",
        )
        .unwrap();

    assert_eq!(persistence.get_session_title("session").unwrap(), None);
    assert!(persistence.update_session_title("session", "Plan").unwrap());
    assert_eq!(
        persistence.get_session_title("session").unwrap().as_deref(),
        Some("Plan")
    );
    let updated_at: Option<String> = database
        .query_row(
            "SELECT updated_at FROM sessions WHERE id = 'session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(updated_at.is_some());

    assert!(persistence.update_session_title("session", " \t").unwrap());
    assert_eq!(persistence.get_session_title("session").unwrap(), None);

    assert!(!persistence.update_session_title("other", "Plan").unwrap());
    assert_eq!(persistence.get_session_title("other").unwrap(), None);
}
