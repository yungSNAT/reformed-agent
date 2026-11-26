use super::*;

#[test]
fn deleting_empty_sessions_keeps_unrecognised_snapshots_and_nonempty_transcripts() {
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join("sessions");
    let store = SessionPersistence::new(home.path());
    assert!(!store.delete_session_if_empty("unknown").unwrap());
    assert!(!store.db_path().exists());
    std::fs::create_dir(&sessions).unwrap();

    for (id, data) in [
        ("old-format", "legacy snapshot without JSON"),
        ("missing-id", r#"{"session_info":{},"messages":[]}"#),
        (
            "different-id",
            r#"{"session_info":{"session_id":"other"},"messages":[]}"#,
        ),
    ] {
        store
            .persist_session(id, &[], None, None, None, None)
            .unwrap();
        let snapshot = sessions.join(format!("{id}.json"));
        let transcript = sessions.join(format!("{id}.jsonl"));
        let contents = if id == "missing-id" {
            b"".as_slice()
        } else {
            b"old transcript"
        };
        std::fs::write(&snapshot, data).unwrap();
        std::fs::write(&transcript, contents).unwrap();

        assert!(store.delete_session_if_empty(&format!(" {id} ")).unwrap());
        assert_eq!(std::fs::read_to_string(&snapshot).unwrap(), data);
        if contents.is_empty() {
            assert!(!transcript.exists());
        } else {
            assert_eq!(std::fs::read(&transcript).unwrap(), contents);
        }
    }
    assert!(!store.delete_session_if_empty(" ").unwrap());
}

#[test]
fn maintenance_surfaces_an_unreadable_marker_without_pruning() {
    let home = tempfile::tempdir().unwrap();
    let store = SessionPersistence::new(home.path());
    assert!(store.db_health_error().is_none());
    store
        .persist_session(
            "old-record",
            &[Message::user("old survey")],
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let db = rusqlite::Connection::open(store.db_path()).unwrap();
    db.execute(
        "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params!["2000-01-01T00:00:00Z", "old-record"],
    )
    .unwrap();
    let legacy_value: &[u8] = b"legacy binary marker";
    db.execute(
        "INSERT INTO state_meta (key, value) VALUES (?1, ?2)",
        rusqlite::params!["last_auto_prune", legacy_value],
    )
    .unwrap();

    let outcome = store.maybe_auto_prune_and_vacuum(30, 24, true);
    assert_eq!(outcome.pruned, 0);
    assert!(!outcome.vacuumed);
    assert!(outcome.error.unwrap().contains("Failed to read state_meta"));

    let rows = db
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE id = ?1",
            ["old-record"],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
    let saved_value = db
        .query_row(
            "SELECT value FROM state_meta WHERE key = ?1",
            ["last_auto_prune"],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .unwrap();
    assert_eq!(saved_value, legacy_value);
}
