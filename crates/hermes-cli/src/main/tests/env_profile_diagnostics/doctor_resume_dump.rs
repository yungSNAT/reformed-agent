#[test]
fn doctor_self_heal_creates_missing_state_dirs() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "doctor",
    ]);
    let state_root = hermes_state_root(&cli);
    assert!(!state_root.join("profiles").exists());

    let actions = run_doctor_self_heal(&cli);
    assert!(state_root.join("profiles").exists());
    assert!(state_root.join("sessions").exists());
    assert!(state_root.join("logs").exists());
    assert!(actions
        .iter()
        .any(|entry| entry.get("status").and_then(|v| v.as_str()) == Some("created")));
}

#[test]
fn doctor_self_heal_removes_stale_gateway_pid_file() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "doctor",
    ]);
    let pid_path = gateway_pid_path_for_cli(&cli);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir pid dir");
    }
    std::fs::write(&pid_path, "999999").expect("write stale pid");
    assert!(pid_path.exists());

    let actions = run_doctor_self_heal(&cli);
    assert!(!pid_path.exists(), "stale pid file should be removed");
    assert!(actions.iter().any(|entry| {
        entry
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("removed stale gateway pid file")
    }));
}

#[test]
fn doctor_elite_diagnostics_payload_has_required_sections() {
    use clap::Parser;

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "doctor",
    ]);
    let payload = build_elite_doctor_diagnostics(&cli);
    assert!(payload.get("provenance").is_some());
    assert!(payload.get("route_learning").is_some());
    assert!(payload.get("route_health").is_some());
    assert!(payload.get("tool_policy").is_some());
    assert!(payload.get("elite_gate").is_some());
}

#[test]
fn doctor_elite_diagnostics_defaults_to_native_gate() {
    use clap::Parser;

    let previous = std::env::var("HERMES_ELITE_GATE_CMD").ok();
    std::env::remove_var("HERMES_ELITE_GATE_CMD");

    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = tmp.path().join("cfg");
    let cli = Cli::parse_from([
        "hermes-ultra",
        "--config-dir",
        cfg.to_str().expect("utf8 path"),
        "doctor",
    ]);
    let payload = build_elite_doctor_diagnostics(&cli);
    assert_eq!(payload["elite_gate"]["runner"].as_str(), Some("native"));
    assert_eq!(
        payload["elite_gate"]["command"].as_str(),
        Some("native rust elite sync gate")
    );
    assert_eq!(
        payload["elite_gate"]["script_available"].as_bool(),
        Some(true)
    );

    match previous {
        Some(value) => std::env::set_var("HERMES_ELITE_GATE_CMD", value),
        None => std::env::remove_var("HERMES_ELITE_GATE_CMD"),
    }
}

#[test]
fn resolve_resume_session_file_prefers_latest_modified_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let old = sessions_dir.join("old-session.json");
    let new = sessions_dir.join("new-session.json");
    std::fs::write(&old, r#"{"messages":[{"role":"user","content":"old"}]}"#)
        .expect("write old session");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&new, r#"{"messages":[{"role":"user","content":"new"}]}"#)
        .expect("write new session");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
    assert_eq!(resolved, "new-session");
    assert_eq!(path, new);
}

#[test]
fn resolve_resume_session_file_latest_skips_oneshot_runs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let snapshot = |id: &str, age_minutes: u64| {
        let path = sessions_dir.join(format!("{id}.json"));
        std::fs::write(
            &path,
            format!(
                r#"{{"session_info":{{"session_id":"{id}"}},"messages":[{{"role":"User","content":"hi"}}]}}"#
            ),
        )
        .expect("write snapshot");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| {
                file.set_modified(
                    std::time::SystemTime::now()
                        - std::time::Duration::from_secs(age_minutes * 60),
                )
            })
            .expect("date snapshot");
        path
    };

    let interactive = snapshot("c0ffee00-0000-4000-8000-000000000002", 20);
    snapshot("oneshot-c0ffee00-0000-4000-8000-000000000003", 10);

    let latest = resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
    assert_eq!(latest.1, interactive);
    let nonempty =
        resolve_latest_nonempty_session_file(&sessions_dir).expect("resolve latest non-empty");
    assert_eq!(nonempty.1, interactive);

    // Other names with the prefix stay ordinary sessions.
    let named = snapshot("oneshot-notes", 5);
    let latest = resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
    assert_eq!(latest.1, named);
}

#[test]
fn resolve_resume_session_file_latest_prefers_canonical_session_stem() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let canonical = sessions_dir.join("c0ffee00-0000-4000-8000-000000000001.json");
    std::fs::write(
        &canonical,
        r#"{
  "session_info": {"session_id":"c0ffee00-0000-4000-8000-000000000001","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
    )
    .expect("write canonical");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let named = sessions_dir.join("newest.json");
    std::fs::write(
        &named,
        r#"{
  "session_info": {"session_id":"snap-prune","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"snapshot payload"}]
}"#,
    )
    .expect("write named artifact");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
    assert_eq!(resolved, "c0ffee00-0000-4000-8000-000000000001");
    assert_eq!(path, canonical);
}

#[test]
fn resolve_resume_session_file_searches_session_id_when_exact_file_is_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let snapshot = sessions_dir.join("saved-snapshot-name.json");
    std::fs::write(
        &snapshot,
        r#"{
  "session_info": {"session_id":"20260603_090200_abcd12","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"hello"}]
}"#,
    )
    .expect("write snapshot");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, Some("20260603")).expect("resolve prefix");
    assert_eq!(resolved, "saved-snapshot-name");
    assert_eq!(path, snapshot);

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, Some("ABCD12")).expect("resolve substring");
    assert_eq!(resolved, "saved-snapshot-name");
    assert_eq!(path, snapshot);
}

#[test]
fn resolve_resume_session_file_search_ranks_exact_before_prefix() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let exact = sessions_dir.join("snap-exact.json");
    std::fs::write(
        &exact,
        r#"{
  "session_info": {"session_id":"20260603_090200_abcd12","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"exact"}]
}"#,
    )
    .expect("write exact");
    let prefix = sessions_dir.join("20260603_090200_abcd12_child.json");
    std::fs::write(
        &prefix,
        r#"{
  "session_info": {"session_id":"20260603_090200_abcd12_child","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"prefix"}]
}"#,
    )
    .expect("write prefix");

    let (resolved, path) =
        resolve_resume_session_file(&sessions_dir, Some("20260603_090200_abcd12"))
            .expect("resolve exact session_info id");
    assert_eq!(resolved, "snap-exact");
    assert_eq!(path, exact);
}

#[test]
fn should_resume_fallback_to_fresh_only_for_latest_missing_state() {
    let latest_missing = AgentError::Config("No saved sessions found in /tmp".to_string());
    assert!(should_resume_fallback_to_fresh(None, &latest_missing));
    assert!(should_resume_fallback_to_fresh(
        Some("latest"),
        &latest_missing
    ));
    assert!(!should_resume_fallback_to_fresh(
        Some("abc123"),
        &latest_missing
    ));

    let other_error = AgentError::Config("Session 'abc123' not found".to_string());
    assert!(!should_resume_fallback_to_fresh(None, &other_error));
}

#[test]
fn load_resume_payload_restores_metadata_and_messages() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let session_path = sessions_dir.join("abc123.json");
    std::fs::write(
        &session_path,
        r#"{
  "session_info": {
"session_id": "session-xyz",
"model": "nous:openai/gpt-5.5-pro",
"personality": "technical"
  },
  "messages": [
{"role":"System","content":"[SESSION_OBJECTIVE] Keep context fresh"},
{"role":"User","content":"hello"},
{"role":"Assistant","content":"world"}
  ]
}"#,
    )
    .expect("write session");

    let payload = load_resume_payload(&cli, Some("abc123")).expect("load payload");
    assert_eq!(payload.resolved_id, "abc123");
    assert_eq!(payload.session_id, "session-xyz");
    assert_eq!(payload.model.as_deref(), Some("nous:openai/gpt-5.5-pro"));
    assert_eq!(payload.personality.as_deref(), Some("technical"));
    assert_eq!(payload.messages.len(), 3);
    assert!(matches!(
        payload.messages[0].role,
        hermes_core::MessageRole::System
    ));
    assert!(matches!(
        payload.messages[1].role,
        hermes_core::MessageRole::User
    ));
    assert!(matches!(
        payload.messages[2].role,
        hermes_core::MessageRole::Assistant
    ));
}

#[test]
fn load_resume_payload_follows_compression_tip_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let state_root = hermes_state_root(&cli);
    let sessions_dir = state_root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(
        sessions_dir.join("root.json"),
        r#"{
  "session_info": {"session_id":"root","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"pre-compression turn"}]
}"#,
    )
    .expect("write root session");
    std::fs::write(
        sessions_dir.join("cont.json"),
        r#"{
  "session_info": {"session_id":"cont","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"Assistant","content":"post-compression reply"}]
}"#,
    )
    .expect("write continuation session");

    let persistence = SessionPersistence::new(&state_root);
    persistence
        .persist_session(
            "root",
            &[hermes_core::Message::user("pre-compression turn")],
            Some("nous:openai/gpt-5.5"),
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    persistence
        .persist_session(
            "cont",
            &[hermes_core::Message::assistant("post-compression reply")],
            Some("nous:openai/gpt-5.5"),
            Some("cli"),
            None,
            None,
        )
        .unwrap();
    let base = chrono::Utc::now() - chrono::Duration::hours(1);
    let root_created = base.to_rfc3339();
    let root_ended = (base + chrono::Duration::seconds(10)).to_rfc3339();
    let cont_created = (base + chrono::Duration::seconds(20)).to_rfc3339();
    assert!(persistence
        .update_session_lineage(
            "root",
            None,
            Some("compression"),
            Some(&root_created),
            Some(&root_ended),
        )
        .expect("mark root compressed"));
    assert!(persistence
        .update_session_lineage("cont", Some("root"), None, Some(&cont_created), None,)
        .expect("link continuation"));

    let payload = load_resume_payload(&cli, Some("root")).expect("load payload");

    assert_eq!(payload.resolved_id, "cont");
    assert_eq!(payload.session_id, "cont");
    assert_eq!(payload.source_path, sessions_dir.join("cont.json"));
    assert_eq!(payload.messages.len(), 1);
    assert_eq!(
        payload.messages[0].content.as_deref(),
        Some("post-compression reply")
    );
}

#[test]
fn load_resume_payload_continues_the_session_from_a_compressed_row() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let state_root = hermes_state_root(&cli);
    let sessions_dir = state_root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(
        sessions_dir.join("live.json"),
        r#"{
  "session_info": {"session_id":"live","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"Assistant","content":"post-compression reply"}]
}"#,
    )
    .expect("write session");
    let persistence = SessionPersistence::new(&state_root);
    persistence
        .persist_session_after_compression(
            "live",
            &[vec![hermes_core::Message::user("pre-compression turn")]],
            &[hermes_core::Message::assistant("post-compression reply")],
            Some("nous:openai/gpt-5.5"),
            None,
            None,
        )
        .unwrap();
    // The row holding the compressed turns has no snapshot of its own.
    let older: String = rusqlite::Connection::open(persistence.db_path())
        .unwrap()
        .query_row(
            "SELECT parent_session_id FROM sessions WHERE id = 'live'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let payload = load_resume_payload(&cli, Some(&older)).expect("load payload");

    assert_eq!(payload.session_id, "live");
    assert_eq!(payload.source_path, sessions_dir.join("live.json"));
}

#[test]
fn load_resume_payload_falls_back_to_legacy_sessions_dir() {
    let _guard = env_lock();
    let prev_home = std::env::var("HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path().join("fake-home");
    let legacy_sessions = fake_home.join(".hermes").join("sessions");
    std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");
    let legacy_path = legacy_sessions.join("legacy-abc.json");
    std::fs::write(
        &legacy_path,
        r#"{
  "session_info": {
"session_id": "legacy-session",
"model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": [
{"role":"User","content":"from-legacy"}
  ]
}"#,
    )
    .expect("write legacy session");

    std::env::set_var("HOME", &fake_home);
    let state_root = tmp.path().join("ultra-state");
    let cli = cli_for_temp_state_root(&state_root);
    let payload = load_resume_payload(&cli, Some("legacy-abc")).expect("load payload");
    assert_eq!(payload.resolved_id, "legacy-abc");
    assert_eq!(payload.session_id, "legacy-session");
    assert_eq!(payload.messages.len(), 1);
    assert!(payload.source_path.starts_with(&legacy_sessions));

    match prev_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn load_resume_payload_accepts_empty_messages_for_startup_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    let session_path = sessions_dir.join("empty-messages.json");
    std::fs::write(
        &session_path,
        r#"{
  "session_info": {
"session_id": "empty-messages",
"model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": []
}"#,
    )
    .expect("write empty session");

    let payload = load_resume_payload(&cli, Some("empty-messages")).expect("load payload");
    assert_eq!(payload.resolved_id, "empty-messages");
    assert_eq!(payload.session_id, "empty-messages");
    assert_eq!(
        payload.model.as_deref(),
        Some("nous:nousresearch/hermes-4-70b")
    );
    assert_eq!(payload.messages.len(), 0);
}

#[test]
fn load_resume_payload_latest_prefers_nonempty_snapshot_over_newer_empty_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

    let non_empty = sessions_dir.join("history-real.json");
    std::fs::write(
        &non_empty,
        r#"{
  "session_info": {"session_id":"history-real","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"hello"},{"role":"Assistant","content":"world"}]
}"#,
    )
    .expect("write non-empty session");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let empty_snapshot = sessions_dir.join("startup-empty.json");
    std::fs::write(
        &empty_snapshot,
        r#"{
  "session_info": {"session_id":"startup-empty","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
    )
    .expect("write empty session");

    let payload = load_resume_payload(&cli, None).expect("load payload");
    assert_eq!(payload.resolved_id, "history-real");
    assert_eq!(payload.messages.len(), 2);
    assert_eq!(payload.source_path, non_empty);
}

#[test]
fn load_resume_payload_latest_falls_back_to_legacy_nonempty_when_primary_empty_only() {
    let _guard = env_lock();
    let prev_home = std::env::var("HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_home = tmp.path().join("fake-home");
    let legacy_sessions = fake_home.join(".hermes").join("sessions");
    std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");

    let legacy_non_empty = legacy_sessions.join("legacy-rich.json");
    std::fs::write(
        &legacy_non_empty,
        r#"{
  "session_info": {"session_id":"legacy-rich","model":"nous:nousresearch/hermes-4-70b"},
  "messages":[{"role":"User","content":"from legacy"}]
}"#,
    )
    .expect("write legacy non-empty session");

    std::env::set_var("HOME", &fake_home);
    let state_root = tmp.path().join("ultra-state");
    let cli = cli_for_temp_state_root(&state_root);
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(
        sessions_dir.join("empty-only.json"),
        r#"{
  "session_info": {"session_id":"empty-only","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
    )
    .expect("write primary empty session");

    let payload = load_resume_payload(&cli, None).expect("load payload");
    assert_eq!(payload.resolved_id, "legacy-rich");
    assert_eq!(payload.messages.len(), 1);
    assert!(payload.source_path.starts_with(&legacy_sessions));

    match prev_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}

#[tokio::test]
async fn run_dump_writes_real_saved_session_export_with_system_prompt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = cli_for_temp_state_root(tmp.path());
    let sessions_dir = hermes_state_root(&cli).join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(
        sessions_dir.join("abc123.json"),
        r#"{
  "session_info": {
"session_id": "session-xyz",
"model": "nous:openai/gpt-5.5",
"personality": "technical",
"created_at": "2026-06-05T09:00:00Z"
  },
  "system_prompt": "persisted system prompt",
  "messages": [
{"role":"User","content":"hello"},
{"role":"Assistant","content":"world"}
  ]
}"#,
    )
    .expect("write session");

    run_dump(cli, Some("abc123".to_string()), None).expect("dump session");

    let saved_dir = tmp.path().join("sessions").join("saved");
    let entries = std::fs::read_dir(&saved_dir)
        .expect("saved dir")
        .collect::<Result<Vec<_>, _>>()
        .expect("saved entries");
    assert_eq!(entries.len(), 1);
    let path = entries[0].path();
    assert!(path
        .file_name()
        .and_then(|v| v.to_str())
        .is_some_and(|name| name.starts_with("hermes_conversation_")));

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read dump"))
            .expect("parse dump");
    assert_eq!(doc["session_id"], "session-xyz");
    assert_eq!(doc["resolved_id"], "abc123");
    assert_eq!(doc["model"], "nous:openai/gpt-5.5");
    assert_eq!(doc["personality"], "technical");
    assert_eq!(doc["system_prompt"], "persisted system prompt");
    assert_eq!(doc["session_start"], "2026-06-05T09:00:00Z");
    assert_eq!(doc["messages"].as_array().map(Vec::len), Some(2));
    assert!(doc["source_path"]
        .as_str()
        .is_some_and(|p| p.ends_with("abc123.json")));
}

const HERMES_EXPORT_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../hermes-parity-tests/fixtures/session_import/hermes_export.jsonl"
));

#[test]
fn imported_hermes_history_resumes_from_native_snapshots() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("hermes-export.jsonl");
    let profile = workspace.path().join("reformed");
    let fixture = HERMES_EXPORT_FIXTURE.replace("The notes are ready.", "");
    std::fs::write(&source, &fixture).unwrap();
    let summary = hermes_cli::commands::import_hermes_sessions(&source, &profile).unwrap();
    assert_eq!(
        (summary.sessions, summary.messages, summary.excluded_rows),
        (2, 5, 2)
    );

    let sessions = profile.join("sessions");
    let (id, path) =
        resolve_resume_session_file(&sessions, Some("20261109_103000_c0ffeed0")).unwrap();
    let parent = parse_resume_payload_file(id, path).unwrap();
    assert_eq!(
        parent.system_prompt.as_deref(),
        Some("Synthetic test session.")
    );
    assert_eq!(parent.model.as_deref(), Some("openai:gpt-4o-mini"));
    assert_eq!(parent.messages.len(), 3);
    let assistant = &parent.messages[1];
    assert_eq!(
        assistant.reasoning_content.as_deref(),
        Some("Read the fixture.")
    );
    let call = &assistant.tool_calls.as_ref().unwrap()[0];
    assert_eq!(call.function.name, "read_file");
    assert_eq!(call.function.arguments, r#"{"path":"fixture.txt"}"#);
    assert_eq!(
        parent.messages[2].tool_call_id.as_deref(),
        Some(call.id.as_str())
    );
    assert_eq!(parent.messages[2].name.as_deref(), Some("read_file"));
    assert_eq!(parent.messages[2].content.as_deref(), Some(""));

    let (id, path) = resolve_resume_session_file(&sessions, Some("latest")).unwrap();
    let child = parse_resume_payload_file(id, path.clone()).unwrap();
    assert_eq!(child.session_id, "20261109_103200_c0ffeed1");
    assert_eq!(child.messages.len(), 2);
    let snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        snapshot["session_info"]["parent_session_id"],
        parent.session_id
    );
    assert!(hermes_cli::commands::import_hermes_sessions(&source, &profile).is_err());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), fixture);
}

#[tokio::test]
async fn imported_hermes_history_is_searchable_as_recorded() {
    use hermes_tools::SessionSearchBackend;

    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("hermes-export.jsonl");
    let profile = workspace.path().join("reformed");
    // A repaired tool row without its own time takes the session's last activity,
    // here its rewound turn, as in Hermes.
    let fixture = HERMES_EXPORT_FIXTURE.replace(r#""timestamp":1794220203,"#, "");
    std::fs::write(&source, fixture).unwrap();
    hermes_cli::commands::import_hermes_sessions(&source, &profile).unwrap();
    let db = profile.join("sessions.db");
    let backend = hermes_tools::SqliteSessionSearchBackend::new(db.to_str().unwrap()).unwrap();

    // Both sessions mention the notes; the compression chain is one result at its tip.
    let found: serde_json::Value =
        serde_json::from_str(&backend.search(Some("notes"), None, 5, None).await.unwrap())
            .unwrap();
    assert_eq!(found["count"], 1);
    assert_eq!(found["sessions_searched"], 1);
    assert_eq!(found["results"][0]["session_id"], "20261109_103200_c0ffeed1");

    // As in Hermes, recent sessions list the chain once, at its tip, with the
    // root's start.
    let recent: serde_json::Value =
        serde_json::from_str(&backend.search(None, None, 5, None).await.unwrap()).unwrap();
    assert_eq!(recent["count"], 1);
    let chain = &recent["results"][0];
    assert_eq!(chain["session_id"], "20261109_103200_c0ffeed1");
    assert_eq!(chain["started_at"], "2026-11-09T10:30:00.125+00:00");
    assert_eq!(chain["last_active"], "2026-11-09T10:32:50.500+00:00");

    // The child's last activity is its latest message, not its lagging heartbeat.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let sessions: Vec<[Option<String>; 5]> = conn
        .prepare(
            "SELECT id, updated_at, ended_at, end_reason, model_config FROM sessions ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?])
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let expected = [
        [
            Some("20261109_103000_c0ffeed0"),
            Some("2026-11-09T10:30:04+00:00"),
            Some("2026-11-09T10:31:00.500+00:00"),
            Some("compression"),
            Some(r#"{"reasoning_effort":"medium"}"#),
        ],
        [
            Some("20261109_103200_c0ffeed1"),
            Some("2026-11-09T10:32:50.500+00:00"),
            None,
            None,
            None,
        ],
    ];
    assert_eq!(
        sessions,
        expected.map(|row| row.map(|cell| cell.map(String::from)))
    );
    let message_times: Vec<String> = conn
        .prepare("SELECT created_at FROM messages ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        message_times,
        [
            "2026-11-09T10:30:01+00:00",
            "2026-11-09T10:30:02+00:00",
            "2026-11-09T10:30:04+00:00",
            "2026-11-09T10:32:02+00:00",
            "2026-11-09T10:32:50.500+00:00",
        ]
    );
}

#[test]
fn rejected_hermes_history_never_publishes_partial_snapshots() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("hermes-export.jsonl");
    let profile = workspace.path().join("reformed");
    for malformed in [
        r#"{"id":"../outside","started_at":1700000000,"messages":[]}"#,
        r#"{"id":"rich","started_at":1700000000,"messages":[{"role":"user","content":[{"type":"image"}]}]}"#,
        r#"{"id":"20261109_103000_C0FFEED0","started_at":1700000000,"messages":[]}"#,
        r#"{"id":"orphan","started_at":1700000000,"messages":[{"role":"assistant","active":0,"tool_calls":[{"id":"call_1","function":{"name":"read_file","arguments":"{}"}}]},{"role":"tool","tool_call_id":"call_1","content":""}]}"#,
        r#"{"id":"pending","started_at":1700000000,"messages":[{"role":"assistant","tool_calls":[{"id":"call_1","function":{"name":"read_file","arguments":"{}"}}]}]}"#,
        r#"{"id":"duplicate","started_at":1700000000,"messages":[{"role":"assistant","tool_calls":[{"id":"call_1","function":{"name":"read_file","arguments":"{}"}}]},{"role":"tool","tool_call_id":"call_1","content":""},{"role":"tool","tool_call_id":"call_1","content":""}]}"#,
        r#"{"id":"ended","started_at":1700000000,"last_activity_at":1700000100,"ended_at":-1,"messages":[]}"#,
        r#"{"id":"heartbeat","started_at":1700000000,"last_activity_at":-1,"messages":[{"role":"user","content":"Hello.","timestamp":1700000001}]}"#,
        r#"{"id":"stamped","started_at":1700000000,"messages":[{"role":"user","content":"When?","timestamp":"noon"}]}"#,
        r#"{"id":"rewound","started_at":1700000000,"messages":[{"role":"user","content":"Undone.","timestamp":-1,"active":0}]}"#,
        r#"{"id":"far-future","started_at":1700000000,"messages":[{"role":"user","content":"Later.","timestamp":4200000001}]}"#,
    ] {
        std::fs::write(&source, format!("{HERMES_EXPORT_FIXTURE}\n{malformed}")).unwrap();
        let error = hermes_cli::commands::import_hermes_sessions(&source, &profile).unwrap_err();
        assert!(error.to_string().contains("Hermes export line 4"));
        assert!(!profile.exists());
        assert!(!workspace.path().join("outside.json").exists());
    }
}

#[cfg(windows)]
#[test]
fn hermes_history_rejects_windows_profile_leaf_with_path_syntax() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("hermes-export.jsonl");
    std::fs::write(&source, HERMES_EXPORT_FIXTURE).unwrap();
    for destination in [
        format!(r"{}\D:reformed", workspace.path().display()),
        format!(r"\\?\{}\../outside", workspace.path().display()),
        format!(r"{}\CON.txt", workspace.path().display()),
        format!(r"{}\LPT¹", workspace.path().display()),
    ] {
        let error = hermes_cli::commands::import_hermes_sessions(
            &source,
            std::path::Path::new(&destination),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("Invalid Windows profile directory name"));
    }
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 1);
}
