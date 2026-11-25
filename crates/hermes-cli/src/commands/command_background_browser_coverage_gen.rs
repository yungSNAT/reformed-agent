use super::*;

fn write_file(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write temp file");
    path
}

#[test]
fn tail_file_lines_reads_last_lines_and_reports_missing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "job.log", "a\nb\nc\nd\n");
    let tail = tail_file_lines(&path, 2).expect("tail ok");
    assert_eq!(tail, "c\nd");

    let missing = dir.path().join("missing.log");
    let Err(err) = tail_file_lines(&missing, 5) else {
        panic!("expected error for missing log");
    };
    assert!(matches!(err, AgentError::Io(ref m) if m.contains("Failed to read background log")));
}

#[test]
fn inspect_snapshot_integrity_covers_all_outcomes() {
    let dir = tempfile::tempdir().expect("tempdir");

    let missing = inspect_snapshot_integrity(&dir.path().join("nope.json"));
    assert!(!missing.valid);
    assert!(missing
        .reason
        .as_deref()
        .unwrap_or_default()
        .starts_with("read_failed"));
    assert_eq!(missing.message_count, 0);
    assert!(missing.session_id.is_none());

    let bad = write_file(dir.path(), "bad.json", "{not json");
    let bad_i = inspect_snapshot_integrity(&bad);
    assert!(!bad_i.valid);
    assert!(bad_i
        .reason
        .as_deref()
        .unwrap_or_default()
        .starts_with("json_invalid"));

    let no_msgs = write_file(
        dir.path(),
        "nomsgs.json",
        r#"{"session_info":{"session_id":"sid-1"}}"#,
    );
    let no_msgs_i = inspect_snapshot_integrity(&no_msgs);
    assert!(!no_msgs_i.valid);
    assert_eq!(no_msgs_i.reason.as_deref(), Some("missing_messages_array"));
    assert_eq!(no_msgs_i.session_id.as_deref(), Some("sid-1"));

    let good = write_file(
        dir.path(),
        "good.json",
        r#"{"session_info":{"session_id":"sid-2"},"messages":[{"role":"User","content":"hi"},{"role":"Assistant","content":"yo"}]}"#,
    );
    let good_i = inspect_snapshot_integrity(&good);
    assert!(good_i.valid);
    assert!(good_i.reason.is_none());
    assert_eq!(good_i.session_id.as_deref(), Some("sid-2"));
    assert_eq!(good_i.message_count, 2);
}

#[test]
fn canonical_snapshot_name_requires_session_id() {
    let without = SnapshotIntegrity {
        valid: true,
        reason: None,
        session_id: None,
        message_count: 1,
    };
    assert!(!is_canonical_snapshot_name("abc", &without));

    let blank = SnapshotIntegrity {
        valid: true,
        reason: None,
        session_id: Some("   ".to_string()),
        message_count: 1,
    };
    assert!(!is_canonical_snapshot_name("abc", &blank));

    let with = SnapshotIntegrity {
        valid: true,
        reason: None,
        session_id: Some("ABC".to_string()),
        message_count: 1,
    };
    assert!(is_canonical_snapshot_name(" abc ", &with));
    assert!(!is_canonical_snapshot_name("other", &with));
}

#[test]
fn resolve_saved_session_entry_exact_prefix_missing_ambiguous() {
    let now = std::time::SystemTime::UNIX_EPOCH;
    let entries = vec![
        (
            "alpha".to_string(),
            std::path::PathBuf::from("alpha.json"),
            now,
        ),
        (
            "alpine".to_string(),
            std::path::PathBuf::from("alpine.json"),
            now,
        ),
        (
            "beta".to_string(),
            std::path::PathBuf::from("beta.json"),
            now,
        ),
    ];

    let exact = resolve_saved_session_entry(&entries, "BETA").expect("exact match");
    assert_eq!(exact.0, "beta");

    let prefix = resolve_saved_session_entry(&entries, "be").expect("prefix match");
    assert_eq!(prefix.0, "beta");

    let missing = resolve_saved_session_entry(&entries, "zzz").unwrap_err();
    assert_eq!(missing, "not_found: zzz");

    let ambiguous = resolve_saved_session_entry(&entries, "alp").unwrap_err();
    assert!(ambiguous.starts_with("ambiguous: "));
    assert!(ambiguous.contains("`alpha`"));
    assert!(ambiguous.contains("`alpine`"));
}

#[test]
fn load_messages_from_snapshot_parses_roles_and_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let good = write_file(
        dir.path(),
        "s.json",
        r#"{"messages":[
            {"role":"User","content":"u"},
            {"role":"Assistant","content":"a"},
            {"role":"System","content":"s"},
            {"role":"Tool","content":"t"},
            {"content":"default"}
        ]}"#,
    );
    let msgs = load_messages_from_snapshot(&good).expect("load ok");
    assert_eq!(msgs.len(), 5);
    assert!(msgs[0].role == hermes_core::MessageRole::User);
    assert!(msgs[1].role == hermes_core::MessageRole::Assistant);
    assert!(msgs[2].role == hermes_core::MessageRole::System);
    assert!(msgs[3].role == hermes_core::MessageRole::Assistant);
    assert!(msgs[4].role == hermes_core::MessageRole::User);
    assert_eq!(msgs[3].content.as_deref(), Some("t"));
    assert_eq!(msgs[4].content.as_deref(), Some("default"));

    let Err(err) = load_messages_from_snapshot(&dir.path().join("missing.json")) else {
        panic!("expected read error");
    };
    assert!(matches!(err, AgentError::Io(ref m) if m.contains("Failed to read session")));

    let bad = write_file(dir.path(), "bad.json", "nope");
    let Err(err) = load_messages_from_snapshot(&bad) else {
        panic!("expected parse error");
    };
    assert!(matches!(err, AgentError::Config(ref m) if m.contains("Failed to parse session")));

    let no_msgs = write_file(dir.path(), "nomsgs.json", r#"{"x":1}"#);
    let Err(err) = load_messages_from_snapshot(&no_msgs) else {
        panic!("expected missing messages error");
    };
    assert!(matches!(err, AgentError::Config(ref m) if m.contains("no messages array")));
}

#[test]
fn message_signature_covers_every_role() {
    assert_eq!(
        message_signature(&hermes_core::Message::system("s")),
        "system|s"
    );
    assert_eq!(
        message_signature(&hermes_core::Message::user("u")),
        "user|u"
    );
    assert_eq!(
        message_signature(&hermes_core::Message::assistant("a")),
        "assistant|a"
    );
    let mut tool = hermes_core::Message::user("x");
    tool.role = hermes_core::MessageRole::Tool;
    tool.content = None;
    assert_eq!(message_signature(&tool), "tool|");
}

#[test]
fn summarize_branch_diff_reports_counts_and_last_assistant() {
    let left = vec![
        hermes_core::Message::user("hi"),
        hermes_core::Message::assistant("A1"),
    ];
    let right = vec![
        hermes_core::Message::user("hi"),
        hermes_core::Message::assistant("B1"),
        hermes_core::Message::user("extra"),
    ];
    let out = summarize_branch_diff(" left ", &left, "right", &right);
    assert!(out.contains("Branch diff: `left` vs `right`"));
    assert!(out.contains("messages: 2 vs 3"));
    assert!(out.contains("unique_to_left: 1"));
    assert!(out.contains("unique_to_right: 2"));
    assert!(out.contains("last_assistant_left: A1"));
    assert!(out.contains("last_assistant_right: B1"));
    assert!(!out.ends_with('\n'));

    let empty = summarize_branch_diff("a", &[], "b", &[]);
    assert!(empty.contains("messages: 0 vs 0"));
    assert!(empty.contains("last_assistant_a: "));
}

#[test]
fn enumerate_saved_sessions_skips_non_json_and_blank_stems() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_file(dir.path(), "one.json", "{}");
    write_file(dir.path(), "notes.txt", "x");
    write_file(dir.path(), " .json", "{}");
    let entries = enumerate_saved_sessions(dir.path());
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "one");
}

#[test]
fn chrome_candidates_darwin_and_windows() {
    let darwin = chrome_debug_candidates_with("Darwin", |_| None, |_| None, |_| true);
    let expected: Vec<String> = DARWIN_BROWSER_APPS.iter().map(|s| s.to_string()).collect();
    assert_eq!(darwin, expected);

    let darwin_none = chrome_debug_candidates_with("Darwin", |_| None, |_| None, |_| false);
    assert!(darwin_none.is_empty());

    let windows = chrome_debug_candidates_with(
        "Windows",
        |key| {
            if key == "ProgramFiles" {
                Some("C:\\Program Files\\".to_string())
            } else {
                None
            }
        },
        |name| {
            if name == "chrome.exe" {
                Some("C:\\bin\\chrome.exe".to_string())
            } else {
                None
            }
        },
        |_| true,
    );
    assert_eq!(windows[0], "C:\\bin\\chrome.exe");
    assert!(
        windows.contains(&"C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".to_string())
    );
    assert!(windows
        .contains(&"C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe".to_string()));
}

#[test]
fn which_on_path_finds_absolute_file_and_misses_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(dir.path(), "fakebrowser", "#!/bin/sh\n");
    let abs = path.display().to_string();
    assert_eq!(which_on_path(&abs), Some(abs.clone()));
    assert_eq!(
        which_on_path("definitely-not-a-real-binary-hermes-cov-xyz-123"),
        None
    );
}

#[test]
fn get_chrome_debug_candidates_only_returns_existing_darwin_apps() {
    let found = get_chrome_debug_candidates("Darwin");
    for candidate in &found {
        assert!(DARWIN_BROWSER_APPS.contains(&candidate.as_str()));
        assert!(std::path::Path::new(candidate).is_file());
    }
}

#[test]
fn manual_chrome_command_fallbacks() {
    let darwin =
        manual_chrome_debug_command_with_candidates(9333, "Darwin", &[]).expect("darwin fallback");
    assert!(darwin.starts_with("open -a \"Google Chrome\""));
    assert!(darwin.contains("--remote-debugging-port=9333"));

    assert!(manual_chrome_debug_command_with_candidates(9333, "Linux", &[]).is_none());

    let via_lookup = manual_chrome_debug_command(9444, "Darwin").expect("darwin always some");
    assert!(via_lookup.contains("--remote-debugging-port=9444"));
}

#[test]
fn browser_http_probe_base_passthrough_for_http() {
    assert_eq!(
        browser_http_probe_base("  http://127.0.0.1:9222  "),
        "http://127.0.0.1:9222"
    );
    assert_eq!(browser_http_probe_base("ws://h:1"), "http://h:1");
    assert_eq!(browser_http_probe_base("wss://h:1"), "https://h:1");
}

#[tokio::test]
async fn browser_probe_success_and_failures() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let ok_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/json/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Browser": "Chrome/123",
            "webSocketDebuggerUrl": "ws://127.0.0.1/devtools/browser/abc"
        })))
        .mount(&ok_server)
        .await;
    let endpoint = format!("{}/", ok_server.uri());
    let Ok(summary) = browser_probe(&endpoint).await else {
        panic!("probe should succeed");
    };
    assert!(summary.contains("Browser: Chrome/123"));
    assert!(summary.contains("WebSocket target: ws://127.0.0.1/devtools/browser/abc"));
    assert!(summary.contains(&format!("Connected CDP endpoint: {}", endpoint)));

    let bad_status = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/json/version"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&bad_status)
        .await;
    let Err(err) = browser_probe(&bad_status.uri()).await else {
        panic!("probe should fail on 500");
    };
    assert!(matches!(err, AgentError::Io(ref m) if m.contains("with status")));

    let bad_json = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/json/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&bad_json)
        .await;
    let Err(err) = browser_probe(&bad_json.uri()).await else {
        panic!("probe should fail on invalid json");
    };
    assert!(matches!(err, AgentError::Config(ref m) if m.contains("Browser probe parse failed")));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let Err(err) = browser_probe(&format!("http://127.0.0.1:{port}")).await else {
        panic!("probe should fail on closed port");
    };
    assert!(matches!(err, AgentError::Io(ref m) if m.contains("Browser probe failed at")));
}
