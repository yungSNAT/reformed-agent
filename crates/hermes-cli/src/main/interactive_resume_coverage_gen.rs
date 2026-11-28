use super::*;

fn write_json(dir: &std::path::Path, name: &str, value: serde_json::Value) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, value.to_string()).expect("write json file");
    path
}

fn error_text(err: AgentError) -> String {
    match err {
        AgentError::Config(msg) => format!("config:{msg}"),
        AgentError::Io(msg) => format!("io:{msg}"),
        _ => "other".to_string(),
    }
}

#[test]
fn read_lock_pid_handles_empty_numeric_and_json() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty.lock");
    std::fs::write(&empty, "   \n").unwrap();
    assert_eq!(read_interactive_lock_pid(&empty), None);

    let numeric = dir.path().join("num.lock");
    std::fs::write(&numeric, "1234\n").unwrap();
    assert_eq!(read_interactive_lock_pid(&numeric), Some(1234));

    let json = dir.path().join("json.lock");
    std::fs::write(&json, "{\"pid\": 42}").unwrap();
    assert_eq!(read_interactive_lock_pid(&json), Some(42));
}

#[cfg(unix)]
#[test]
fn own_process_is_alive() {
    assert!(process_pid_is_alive(std::process::id()));
}

#[cfg(unix)]
#[test]
fn parse_pid_snapshot_line_rejects_empty_and_commandless() {
    assert!(parse_pid_snapshot_line("   ").is_none());
    assert!(parse_pid_snapshot_line("1 ??").is_none());
    let snap = parse_pid_snapshot_line(" 1 ?? hermes-ultra chat ").expect("snapshot");
    assert_eq!(snap.ppid, 1);
    assert_eq!(snap.tty, "??");
    assert_eq!(snap.command, "hermes-ultra chat");
}

#[cfg(unix)]
#[test]
fn snapshot_of_nonexistent_pid_is_none_and_not_reapable() {
    let pid = 2_147_483_000u32;
    assert!(interactive_pid_snapshot(pid).is_none());
    assert!(!interactive_lock_holder_is_reapable_orphan(pid));
}

#[test]
fn follow_tip_returns_payload_when_no_parent() {
    let payload = ResumeSessionPayload {
        resolved_id: "r".into(),
        source_path: std::path::PathBuf::new(),
        session_id: "sid".into(),
        model: None,
        personality: None,
        system_prompt: None,
        session_start: None,
        messages: Vec::new(),
    };
    let out = follow_resume_compression_tip(payload).unwrap();
    assert_eq!(out.session_id, "sid");
    assert_eq!(out.source_path, std::path::PathBuf::new());

    let payload = ResumeSessionPayload {
        resolved_id: "r2".into(),
        source_path: std::path::PathBuf::from("x.json"),
        session_id: "sid2".into(),
        model: Some("m".into()),
        personality: None,
        system_prompt: None,
        session_start: None,
        messages: Vec::new(),
    };
    let out = follow_resume_compression_tip(payload).unwrap();
    assert_eq!(out.session_id, "sid2");
    assert_eq!(out.resolved_id, "r2");
    assert_eq!(out.model.as_deref(), Some("m"));
}

#[test]
fn follow_tip_with_unknown_session_keeps_payload() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let payload = ResumeSessionPayload {
        resolved_id: "abc".into(),
        source_path: sessions.join("abc.json"),
        session_id: "abc".into(),
        model: None,
        personality: None,
        system_prompt: None,
        session_start: None,
        messages: vec![hermes_core::Message::user("hello".to_string())],
    };
    let out = follow_resume_compression_tip(payload).unwrap();
    assert_eq!(out.session_id, "abc");
    assert_eq!(out.resolved_id, "abc");
    assert_eq!(out.messages.len(), 1);
}

#[test]
fn parse_payload_file_errors() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.json");
    let err = parse_resume_payload_file("missing".into(), missing).unwrap_err();
    let text = error_text(err);
    assert!(text.starts_with("io:Failed to read session file"), "{text}");

    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{not json").unwrap();
    let err = parse_resume_payload_file("bad".into(), bad).unwrap_err();
    let text = error_text(err);
    assert!(
        text.starts_with("config:Failed to parse session file"),
        "{text}"
    );

    for snapshot in [
        serde_json::json!({"model": "x"}),
        serde_json::json!({"messages": {}}),
    ] {
        let path = write_json(dir.path(), "nomsg.json", snapshot);
        let err = parse_resume_payload_file("nomsg".into(), path).unwrap_err();
        let text = error_text(err);
        assert!(
            text.starts_with("config:") && text.contains("does not contain a valid `messages` array"),
            "{text}"
        );
    }
}

#[test]
fn parse_payload_file_uses_legacy_metadata_and_resolved_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_json(
        dir.path(),
        "fallback.json",
        serde_json::json!({
            "session_info": {
                "session_id": "  ", "model": "  ", "personality": "  ",
                "created_at": "  2026-06-05T09:00:00Z  "
            },
            "model": "  gpt-x  ",
            "personality": "  fixture-personality  ",
            "system_prompt": "  ",
            "messages": [{"role": "user", "content": "hi"}]
        }),
    );
    let payload = parse_resume_payload_file("fallback".into(), path.clone()).unwrap();
    assert_eq!(payload.session_id, "fallback");
    assert_eq!(payload.model.as_deref(), Some("gpt-x"));
    assert_eq!(payload.personality.as_deref(), Some("fixture-personality"));
    assert_eq!(payload.session_start.as_deref(), Some("2026-06-05T09:00:00Z"));
    assert_eq!(payload.system_prompt, None);
    assert_eq!(payload.source_path, path);
    assert_eq!(payload.messages, vec![hermes_core::Message::user("hi")]);
}

#[test]
fn latest_nonempty_with_fallback_finds_primary() {
    let dir = tempfile::tempdir().unwrap();
    write_json(
        dir.path(),
        "good.json",
        serde_json::json!({
            "session_info": {"session_id": "good"},
            "messages": [{"role": "user", "content": "hi"}]
        }),
    );
    let (id, path) = resolve_latest_nonempty_session_file_with_legacy_fallback(dir.path()).unwrap();
    assert_eq!(id, "good");
    assert_eq!(path, dir.path().join("good.json"));
}

#[test]
fn latest_nonempty_errors() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    let text = error_text(resolve_latest_nonempty_session_file(&missing).unwrap_err());
    assert!(
        text.starts_with("io:Failed to read sessions directory"),
        "{text}"
    );

    write_json(
        dir.path(),
        "e.json",
        serde_json::json!({"session_info": {"session_id": "e"}, "messages": []}),
    );
    write_json(
        dir.path(),
        "n.json",
        serde_json::json!({
            "session_info": {"session_id": "other"},
            "messages": [{"role": "user", "content": "x"}]
        }),
    );
    let text = error_text(resolve_latest_nonempty_session_file(dir.path()).unwrap_err());
    assert!(text.contains("No non-empty saved sessions found"), "{text}");
}

#[test]
fn session_summary_none_for_missing_or_invalid() {
    let dir = tempfile::tempdir().unwrap();
    assert!(session_file_summary(&dir.path().join("missing.json")).is_none());
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "nope").unwrap();
    assert!(session_file_summary(&bad).is_none());
}

#[test]
fn match_score_empty_query_is_none() {
    assert_eq!(resume_session_id_match_score("   ", "abc", None), None);
    assert_eq!(resume_session_id_match_score("abc", "abc", None), Some(0));
}

#[test]
fn id_query_skips_non_json_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha-notes.txt"), "x").unwrap();
    write_json(
        dir.path(),
        "alpha-123.json",
        serde_json::json!({"session_info": {"session_id": "alpha-123"}, "messages": []}),
    );
    let found = resolve_resume_session_file_by_id_query(dir.path(), "alpha")
        .unwrap()
        .expect("candidate");
    assert_eq!(found.0, "alpha-123");
    assert_eq!(found.1, dir.path().join("alpha-123.json"));
}

#[test]
fn resolve_latest_missing_and_empty_dir_errors() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    let text = error_text(resolve_resume_session_file(&missing, None).unwrap_err());
    assert!(
        text.starts_with("io:Failed to read sessions directory"),
        "{text}"
    );

    let text = error_text(resolve_resume_session_file(dir.path(), Some("latest")).unwrap_err());
    assert!(text.contains("No saved sessions found"), "{text}");
    assert!(should_resume_fallback_to_fresh(
        None,
        &AgentError::Config(text.trim_start_matches("config:").to_string())
    ));
}

#[test]
fn resolve_latest_prefers_canonical_empty_over_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write_json(
        dir.path(),
        "sess1.json",
        serde_json::json!({"session_info": {"session_id": "sess1"}, "messages": []}),
    );
    let (id, path) = resolve_resume_session_file(dir.path(), None).unwrap();
    assert_eq!(id, "sess1");
    assert_eq!(path, dir.path().join("sess1.json"));
}

#[test]
fn resolve_latest_falls_back_to_noncanonical() {
    let dir = tempfile::tempdir().unwrap();
    write_json(
        dir.path(),
        "foo.json",
        serde_json::json!({
            "session_info": {"session_id": "other"},
            "messages": [{"role": "user", "content": "x"}]
        }),
    );
    let (id, path) = resolve_resume_session_file(dir.path(), Some("")).unwrap();
    assert_eq!(id, "foo");
    assert_eq!(path, dir.path().join("foo.json"));
}

#[test]
fn resolve_rejects_path_like_ids() {
    let dir = tempfile::tempdir().unwrap();
    let text = error_text(resolve_resume_session_file(dir.path(), Some("a/b")).unwrap_err());
    assert_eq!(text, "config:Session ID should be a file stem, not a path.");
    let text = error_text(resolve_resume_session_file(dir.path(), Some("a\\b")).unwrap_err());
    assert_eq!(text, "config:Session ID should be a file stem, not a path.");
}

#[test]
fn extract_objective_from_system_messages() {
    let messages = vec![
        hermes_core::Message::user("[SESSION_OBJECTIVE] not me".to_string()),
        hermes_core::Message::system("plain system".to_string()),
        hermes_core::Message::system("[SESSION_OBJECTIVE]    ".to_string()),
        hermes_core::Message::system("  [SESSION_OBJECTIVE]   build it  ".to_string()),
    ];
    assert_eq!(
        extract_session_objective(&messages),
        Some("build it".to_string())
    );

    let none = vec![
        hermes_core::Message::user("hello".to_string()),
        hermes_core::Message::system("no objective".to_string()),
    ];
    assert_eq!(extract_session_objective(&none), None);
    assert_eq!(extract_session_objective(&[]), None);
}
