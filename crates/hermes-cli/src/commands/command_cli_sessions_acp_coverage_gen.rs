use super::*;
use serde_json::{json, Value};

fn assert_config_error(error: hermes_core::AgentError, expected: &str) {
    let hermes_core::AgentError::Config(message) = error else {
        panic!("expected a configuration error, got {error:?}");
    };
    assert!(message.contains(expected), "{message}");
}

fn assert_io_error(error: hermes_core::AgentError, expected: &str) {
    let hermes_core::AgentError::Io(message) = error else {
        panic!("expected an I/O error, got {error:?}");
    };
    assert!(message.contains(expected), "{message}");
}

fn write_snapshot(dir: &Path, id: &str, data: &Value) -> PathBuf {
    let path = dir.join(format!("{id}.json"));
    std::fs::write(&path, data.to_string()).unwrap();
    path
}

fn set_modified(path: &Path, modified: SystemTime) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
}

#[test]
fn snapshot_lookup_distinguishes_absent_ambiguous_and_unremovable_entries() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("sessions");
    assert!(list_session_snapshot_entries(&dir).unwrap().is_empty());

    std::fs::write(&dir, "").unwrap();
    assert_io_error(
        list_session_snapshot_entries(&dir).unwrap_err(),
        "Failed to read sessions directory",
    );
    std::fs::remove_file(&dir).unwrap();
    std::fs::create_dir(&dir).unwrap();

    let first = write_snapshot(&dir, "run-one", &json!({"source": " cli "}));
    let second = write_snapshot(&dir, "run-two", &json!({"metadata": {"source": "web"}}));
    std::fs::write(dir.join("run-three.txt"), "{}").unwrap();

    let mut entries = list_session_snapshot_entries(&dir).unwrap();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path, first);
    assert_eq!(entries[0].source.as_deref(), Some("cli"));
    assert_eq!(entries[1].path, second);
    assert_eq!(entries[1].source.as_deref(), Some("web"));

    for query in [" \t ", "missing"] {
        assert!(resolve_unique_session_snapshot(&dir, query)
            .unwrap()
            .is_none());
    }
    assert_eq!(
        delete_session_snapshot_by_query(&dir, "missing").unwrap(),
        None
    );
    assert_config_error(
        delete_session_snapshot_by_query(&dir, " run- ").unwrap_err(),
        "Session prefix 'run-' is ambiguous (2 matches).",
    );
    assert!(first.is_file() && second.is_file());

    let directory_snapshot = dir.join("directory.json");
    std::fs::create_dir(&directory_snapshot).unwrap();
    assert_io_error(
        delete_session_snapshot_by_query(&dir, "DIRECTORY").unwrap_err(),
        "Failed to delete session snapshot",
    );
    assert!(directory_snapshot.is_dir());
}

#[test]
fn snapshot_export_filters_sorts_and_redacts_without_writing_back() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    let fixture = json!({
        "session_id": "  internal-id  ",
        "session_info": {"source": "CLI"},
        "metadata": {"API_KEY": "fixture-key", "tokens": ["fixture-token", 4, null]},
        "messages": [{"content": "Bearer abcdefghijklmnop; token=local-only"}]
    });
    let b = write_snapshot(dir, "b", &json!({"source": "cli"}));
    let a = write_snapshot(dir, "a", &fixture);
    let recent = write_snapshot(dir, "recent", &json!({"source": "cli"}));
    write_snapshot(dir, "other", &json!({"source": "web"}));
    for path in [&a, &b] {
        set_modified(path, old);
    }
    set_modified(&recent, old + Duration::from_secs(3600));

    let entries = collect_session_export_snapshots(dir, None, Some(" cLi "), None, true).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["recent", "internal-id", "b"]
    );
    let data = &entries[1].data;
    assert_eq!(data["metadata"]["API_KEY"], "[REDACTED]");
    assert_eq!(data["metadata"]["tokens"], json!(["[REDACTED]", 4, null]));
    assert_eq!(
        data["messages"][0]["content"],
        "Bearer [REDACTED]; token=[REDACTED]"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&a).unwrap()).unwrap(),
        fixture
    );
    assert!(
        collect_session_export_snapshots(dir, Some("absent"), None, None, false)
            .unwrap()
            .is_empty()
    );

    let fresh = write_snapshot(dir, "fresh", &json!({"source": "cli"}));
    set_modified(&fresh, SystemTime::now());
    let mut old_cli = prune_session_snapshot_candidates(dir, Some("cli"), Some(1)).unwrap();
    old_cli.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(
        old_cli
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "recent"]
    );
    assert_eq!(render_prune_preview(&[]), "0 session(s) match.");
    assert_eq!(
        render_prune_preview(&old_cli),
        "3 session(s) match (oldest 1600000000, newest 1600003600)."
    );
}

#[test]
fn export_read_reports_io_and_parse_failures_separately() {
    let root = tempfile::tempdir().unwrap();
    let entry = SessionSnapshotEntry {
        id: "broken".into(),
        path: root.path().join("broken.json"),
        modified: SystemTime::UNIX_EPOCH,
        source: None,
    };
    assert_io_error(
        read_session_export_snapshot(&entry, false).unwrap_err(),
        "Failed to read session snapshot",
    );
    std::fs::write(&entry.path, "{not JSON").unwrap();
    assert_io_error(
        read_session_export_snapshot(&entry, false).unwrap_err(),
        "Failed to parse session snapshot",
    );
}

#[test]
fn export_option_spellings_and_positional_paths() {
    assert_config_error(
        normalize_session_export_format(Some(" CSV ")).unwrap_err(),
        "Unsupported sessions export format 'csv'",
    );
    for spelling in [" USER ", "prompts", "USER-PROMPTS", "user_prompts"] {
        assert_eq!(
            normalize_session_export_only(Some(spelling)).unwrap(),
            Some(SessionExportOnly::UserPrompts)
        );
    }
    assert_config_error(
        normalize_session_export_only(Some(" Assistant ")).unwrap_err(),
        "Unsupported sessions export filter 'assistant'",
    );
    for path in [
        " - ",
        "dir/out",
        "out.json",
        "out.jsonl",
        "out.md",
        "out.markdown",
        "out.html",
        "out.htm",
    ] {
        assert!(session_export_positional_looks_like_output(path), "{path}");
    }
    for subject in ["", "session-17", "session.json-backup"] {
        assert!(
            !session_export_positional_looks_like_output(subject),
            "{subject}"
        );
    }
}

#[test]
fn session_export_renderers_preserve_format_specific_content() {
    let sessions = [
        SessionExportSnapshot {
            id: "run&one".into(),
            data: json!({
                "title": "  First\n \n Second <&> ",
                "created_at": " 2026-01-01T00:00:00Z ",
                "messages": [
                    {"role": "system", "content": "hidden system text"},
                    {
                        "role": " USER ", "id": 17, "platform_message_id": "platform-17",
                        "event_id": "not-preferred", "timestamp": " noon ",
                        "content": [null, {"text": "hello"}, 42]
                    },
                    {"role": "assistant", "content": "<answer>", "tool_calls": [{"name": "lookup&"}]},
                    {"role": "tool", "name": "lookup\nresult", "content": "literal ```` run"},
                    {"role": "tool", "content": null},
                    {"role": "notice", "timestamp": false, "content": {"n": 7}},
                    {"role": "session_meta", "content": "hidden HTML metadata"}
                ]
            }),
        },
        SessionExportSnapshot {
            id: "empty".into(),
            data: json!({"messages": [{"role": "system", "content": "only system"}]}),
        },
    ];
    let single = &sessions[..1];
    let only = Some(SessionExportOnly::UserPrompts);

    let rendered = render_session_exports(single, SessionExportFormat::Json, None).unwrap();
    assert!(rendered.ends_with('\n'));
    assert_eq!(
        serde_json::from_str::<Value>(&rendered).unwrap(),
        sessions[0].data
    );
    let rendered = render_session_exports(&sessions, SessionExportFormat::Json, None).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&rendered).unwrap(),
        json!([sessions[0].data, sessions[1].data])
    );
    assert_eq!(
        render_session_exports(&[], SessionExportFormat::Json, None).unwrap(),
        "[]\n"
    );
    for format in [SessionExportFormat::Json, SessionExportFormat::Html] {
        assert_config_error(
            render_session_exports(&sessions, format, only).unwrap_err(),
            "--only user-prompts supports --format jsonl or md.",
        );
    }

    let rendered = render_session_exports(&sessions, SessionExportFormat::Jsonl, None).unwrap();
    let rows: Vec<Value> = rendered
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows, [sessions[0].data.clone(), sessions[1].data.clone()]);
    assert_eq!(
        render_session_exports(&[], SessionExportFormat::Jsonl, None).unwrap(),
        ""
    );
    let rendered = render_session_exports(&sessions, SessionExportFormat::Jsonl, only).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&rendered).unwrap(),
        json!({
            "session_id": "run&one", "index": 1, "role": "user", "text": "hello\n42",
            "created_at": "noon", "message_id": 17, "event_id": "platform-17"
        })
    );

    let md = render_session_exports(single, SessionExportFormat::Markdown, None).unwrap();
    for snippet in [
        "# Session: First Second <&>\n",
        "- Started: 2026-01-01T00:00:00Z",
        "## User - noon\n\nhello\n42",
        "## Assistant\n\n<answer>",
        "## Tool: lookup result\n\n`````text\nliteral ```` run\n`````",
        "## Tool: tool\n\n```text\n\n```",
        "## Notice\n\n{\"n\":7}",
    ] {
        assert!(md.contains(snippet), "missing {snippet:?} in {md}");
    }
    assert!(!md.contains("hidden system text"));
    let md = render_session_exports(&sessions, SessionExportFormat::Markdown, None).unwrap();
    assert!(md.starts_with("# Hermes sessions export\n"));
    assert!(md.contains("### Assistant\n"));
    assert!(md.contains("## Session: empty\n"));
    assert!(md.ends_with("_No messages found._\n"));

    let prompts = render_session_exports(&sessions, SessionExportFormat::Markdown, only).unwrap();
    assert!(prompts.starts_with("# User prompts export\n"));
    assert!(prompts.contains("### 1. noon\nMessage ID: `17`\n\nhello\n42"));
    assert!(prompts.contains("## Session empty\n"));
    assert!(prompts.ends_with("_No user prompts found._\n"));
    assert!(!prompts.contains("<answer>"));

    let html = render_session_exports(&sessions, SessionExportFormat::Html, None).unwrap();
    assert!(html.contains("href=\"#session-run&amp;one\""));
    assert!(html.contains("<pre>&lt;answer&gt;</pre>"));
    assert!(html
        .contains("<summary>Tool call</summary><pre>{&quot;name&quot;:&quot;lookup&amp;&quot;}"));
    assert!(!html.contains("hidden HTML metadata"));
    assert_eq!(format_session_timestamp(&json!(" \t ")), None);
    assert_eq!(title_case_ascii(""), "");
    assert_eq!(message_text(None), "");
}

#[test]
fn shell_quoting_and_recursive_file_counts() {
    assert_eq!(
        quote_shell_arg(Path::new("plain/path-npx")),
        "plain/path-npx"
    );
    assert_eq!(quote_shell_arg(Path::new("a b'c")), "'a b'\\''c'");
    assert_eq!(quote_shell_arg(Path::new("")), "''");

    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("child");
    assert_eq!(count_files_recursive(&child), 0);
    std::fs::create_dir_all(child.join("empty")).unwrap();
    std::fs::write(root.path().join("top"), "").unwrap();
    std::fs::write(child.join("nested"), "").unwrap();
    assert_eq!(count_files_recursive(root.path()), 2);
    assert_eq!(count_files_recursive(&child), 1);
}

// Run handlers that consult process-wide paths or stdio in a fresh test process.
#[cfg(unix)]
mod isolated_cli {
    use super::*;
    use std::io::{Seek, Write};
    use std::process::Command;

    const SCENARIO_ENV: &str = "HERMES_SESSIONS_ACP_COVERAGE_SCENARIO";

    fn run_scenario(scenario: &str, input: &str) -> String {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let hermes = home.join(".hermes");
        std::fs::create_dir_all(&hermes).unwrap();

        let mut stdin = tempfile::tempfile().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
        stdin.rewind().unwrap();

        let (_, module) = module_path!().split_once("::").unwrap();
        let test_name = format!("{module}::isolated_worker");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", &test_name, "--ignored", "--nocapture"])
            .env_clear()
            .env("HOME", &home)
            .env("HERMES_HOME", &hermes)
            .env("TMPDIR", root.path())
            .env(SCENARIO_ENV, scenario)
            .current_dir(root.path())
            .stdin(stdin);
        for name in [
            "LLVM_PROFILE_FILE",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
            "DYLD_FALLBACK_LIBRARY_PATH",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.status.success() && stdout.contains("test result: ok. 1 passed"),
            "{scenario}: {}\n{stdout}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
    }

    fn options(action: &str) -> CliSessionsOptions {
        CliSessionsOptions {
            action: Some(action.into()),
            session: None,
            id: None,
            session_id: None,
            name: None,
            format: None,
            only: None,
            output: None,
            redact: false,
            yes: false,
            source: None,
            older_than: None,
        }
    }

    fn assert_output(output: &str, expected: &[&str]) {
        for snippet in expected {
            assert!(
                output.contains(snippet),
                "missing {snippet:?} in:\n{output}"
            );
        }
    }

    #[test]
    fn reports_handle_empty_directories_and_summarize_matching_sessions() {
        let output = run_scenario("reports", "");
        assert_output(
            &output,
            &[
                "No sessions directory found.",
                "No sessions directory.",
                "No saved sessions.",
                "No sessions found.",
                "Saved sessions (5 total;",
                "0 msgs, invalid)",
                "Total sessions: 5",
                "Session Browser",
                "2h ago",
                "5d ago",
                "Filter: source=cli",
                "Sessions:       2",
                "Messages:       3",
                "Input tokens:   121",
                "Output tokens:  79",
                "Total tokens:   200",
                "Estimated cost: $0.1250",
                "Models Used:",
                "Averages per session:",
                "  Messages: 1.5",
                "  Tokens:   100",
                "Sessions action 'bogus' is not recognized.",
                CLI_SESSIONS_ACTIONS,
            ],
        );
        assert!(output.lines().any(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields == ["fixture-model", "2", "session(s)"]
        }));
    }

    async fn reports(home: &Path) {
        for action in ["list", "stats", "prune", "browse"] {
            handle_cli_sessions(options(action)).await.unwrap();
        }
        handle_cli_insights(1, None).await.unwrap();

        let dir = home.join("sessions");
        std::fs::create_dir(&dir).unwrap();
        for action in ["list", "browse"] {
            handle_cli_sessions(options(action)).await.unwrap();
        }

        write_snapshot(
            &dir,
            "recent",
            &json!({
                "source": "cli", "model": "fixture-model",
                "messages": [{"role":"user"}, {"role":"assistant"}],
                "usage": {"input_tokens":121, "output_tokens":79, "cost":12.5}
            }),
        );
        let earlier = write_snapshot(
            &dir,
            "earlier",
            &json!({"source":"cli", "model":"fixture-model", "messages":[{"role":"user"}]}),
        );
        let old = write_snapshot(&dir, "old", &json!({"source":"cli", "model":"old-model"}));
        write_snapshot(&dir, "other", &json!({"source":"web", "model":"web-model"}));
        let now = SystemTime::now();
        set_modified(&earlier, now - Duration::from_secs(2 * 3600 + 60));
        set_modified(&old, now - Duration::from_secs(5 * 86_400 + 60));
        std::fs::write(dir.join("invalid.json"), "not JSON").unwrap();
        std::fs::write(dir.join("ignored.txt"), "{}").unwrap();

        for action in ["list", "stats", "browse", "bogus"] {
            handle_cli_sessions(options(action)).await.unwrap();
        }
        handle_cli_insights(1, Some("cli".into())).await.unwrap();
    }

    #[test]
    fn cli_export_disambiguates_subjects_and_output_paths() {
        let output = run_scenario("exports", "");
        assert_output(
            &output,
            &[
                "Exported 2 sessions to",
                "Exported 1 prompt to",
                "Exported 2 sessions to positional.html",
                "Session 'missing' not found.",
                "No sessions found.",
                "\"id\": \"first-id\"",
            ],
        );
    }

    async fn exports(home: &Path) {
        let dir = home.join("sessions");
        std::fs::create_dir(&dir).unwrap();
        write_snapshot(
            &dir,
            "first",
            &json!({
                "id": "first-id",
                "messages": [{"role":"user", "content":"export me"}, {"role":"assistant", "content":"reply"}]
            }),
        );
        write_snapshot(&dir, "second", &json!({"id":"second-id"}));

        let mut args = options("export");
        args.format = Some("html".into());
        args.output = Some(" - ".into());
        assert_config_error(
            handle_cli_sessions(args).await.unwrap_err(),
            "HTML export requires an output file path.",
        );

        let mut args = options("export");
        args.session = Some(" \t ".into());
        args.output = Some("all.json".into());
        handle_cli_sessions(args).await.unwrap();
        let all: Value =
            serde_json::from_str(&std::fs::read_to_string("all.json").unwrap()).unwrap();
        let mut ids: Vec<_> = all
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ["first-id", "second-id"]);

        let mut args = options("export");
        args.session = Some(" first ".into());
        args.format = Some("jsonl".into());
        args.only = Some("user-prompts".into());
        args.output = Some("prompts.jsonl".into());
        handle_cli_sessions(args).await.unwrap();
        let prompt: Value =
            serde_json::from_str(&std::fs::read_to_string("prompts.jsonl").unwrap()).unwrap();
        assert_eq!(
            prompt,
            json!({"index":1, "role":"user", "session_id":"first-id", "text":"export me"})
        );

        let mut args = options("export");
        args.format = Some("html".into());
        args.session = Some("positional.html".into());
        handle_cli_sessions(args).await.unwrap();
        let html = std::fs::read_to_string("positional.html").unwrap();
        assert_output(
            &html,
            &[
                "<title>Hermes sessions export (2 sessions)</title>",
                "<pre>export me</pre>",
            ],
        );

        let mut args = options("export");
        args.session = Some("missing".into());
        args.output = Some("missing.json".into());
        handle_cli_sessions(args).await.unwrap();
        assert!(!Path::new("missing.json").exists());

        let mut args = options("export");
        args.source = Some("no-such-source".into());
        args.output = Some("empty.json".into());
        handle_cli_sessions(args).await.unwrap();
        assert!(!Path::new("empty.json").exists());

        let mut args = options("export");
        args.id = Some("first".into());
        args.output = Some("-".into());
        handle_cli_sessions(args).await.unwrap();
    }

    #[test]
    fn delete_rename_and_prune_preserve_unselected_snapshots() {
        let output = run_scenario("mutations", "n\nno\n");
        assert_output(
            &output,
            &[
                "Session 'missing' not found.",
                "Delete session 'before'?",
                "Session renamed: before -> after",
                "0 session(s) match.",
                "Prune matching sessions?",
                "Pruned: after",
                "Pruned 1 session(s).",
            ],
        );
        assert_eq!(output.matches("Cancelled.").count(), 2);
    }

    async fn mutations(home: &Path) {
        let dir = home.join("sessions");
        std::fs::create_dir(&dir).unwrap();
        let before = write_snapshot(&dir, "before", &json!({"source":"CLI"}));
        let keep = write_snapshot(&dir, "keep", &json!({"source":"web"}));
        let original = std::fs::read(&before).unwrap();

        assert_config_error(
            handle_cli_sessions(options("delete")).await.unwrap_err(),
            "Missing session ID",
        );
        let mut args = options("delete");
        args.id = Some("missing".into());
        handle_cli_sessions(args).await.unwrap();
        let mut args = options("delete");
        args.id = Some("befo".into());
        handle_cli_sessions(args).await.unwrap();
        assert_eq!(std::fs::read(&before).unwrap(), original);

        assert_config_error(
            handle_cli_sessions(options("rename")).await.unwrap_err(),
            "Missing session ID",
        );
        let mut args = options("rename");
        args.id = Some("before".into());
        assert_config_error(
            handle_cli_sessions(args).await.unwrap_err(),
            "Missing new name",
        );

        let mut args = options("rename");
        args.id = Some("missing".into());
        args.name = Some("after".into());
        handle_cli_sessions(args).await.unwrap();
        let after = dir.join("after.json");
        assert!(!after.exists());

        let mut args = options("rename");
        args.id = Some("before".into());
        args.name = Some("nonexistent-parent/after".into());
        assert!(matches!(
            handle_cli_sessions(args).await,
            Err(hermes_core::AgentError::Io(_))
        ));
        assert!(before.is_file());

        let mut args = options("rename");
        args.id = Some("before".into());
        args.name = Some("after".into());
        handle_cli_sessions(args).await.unwrap();
        assert!(!before.exists());
        assert_eq!(std::fs::read(&after).unwrap(), original);

        let mut args = options("prune");
        args.source = Some("absent-source".into());
        handle_cli_sessions(args).await.unwrap();
        let mut args = options("prune");
        args.source = Some(" cli ".into());
        handle_cli_sessions(args).await.unwrap();
        assert_eq!(std::fs::read(&after).unwrap(), original);

        let mut args = options("prune");
        args.source = Some("cli".into());
        args.yes = true;
        handle_cli_sessions(args).await.unwrap();
        assert!(!after.exists());
        assert_eq!(
            std::fs::read_to_string(keep).unwrap(),
            r#"{"source":"web"}"#
        );
    }

    #[test]
    fn repair_reports_missing_and_unsupported_database_states() {
        let output = run_scenario("repair", "");
        assert_output(
            &output,
            &[
                "nothing to repair",
                "not the targeted malformed-schema repair class: no such table: sessions",
                "not the targeted malformed-schema repair class: file is not a database",
            ],
        );
        assert!(!output.contains("Repairing with a raw backup first"));
    }

    async fn repair(home: &Path) {
        let store = SessionPersistence::new(home);
        let db = store.db_path();
        assert!(db.starts_with(home) && !db.exists());
        handle_cli_sessions(options("repair")).await.unwrap();
        assert!(!db.exists());

        // A valid SQLite file without Hermes's sessions table is not a healthy store.
        rusqlite::Connection::open(db)
            .unwrap()
            .execute_batch("CREATE TABLE fixture (id INTEGER PRIMARY KEY)")
            .unwrap();
        let incomplete = std::fs::read(db).unwrap();
        handle_cli_sessions(options("repair")).await.unwrap();
        assert_eq!(std::fs::read(db).unwrap(), incomplete);

        let corrupt = [b'!'; 4096];
        std::fs::write(db, corrupt.as_slice()).unwrap();
        handle_cli_sessions(options("repair")).await.unwrap();
        assert_eq!(std::fs::read(db).unwrap(), corrupt);
    }

    #[test]
    fn whatsapp_offline_paths_and_qr_block_art() {
        let output = run_scenario("whatsapp-status", "");
        assert_eq!(output.matches("WhatsApp: not configured").count(), 2);
        assert_output(
            &output,
            &[
                "Enabled:        true",
                "Phone Number ID: fixture-phone",
                "Access Token:   missing",
                "WhatsApp action 'bogus' is not recognized.",
                "(empty QR data)",
            ],
        );
        assert!(output.lines().any(|line| line == "  █▀▄ "));
        assert!(output.lines().any(|line| line == "  ▀  "));
        assert!(!output.contains("API Connectivity:"));

        for (input, message) in [
            (" \n", "phone number ID is required"),
            ("fixture-phone\n\n", "business account ID is required"),
            (
                "fixture-phone\nfixture-business\n \n",
                "access token is required",
            ),
        ] {
            assert_output(&run_scenario("whatsapp-setup", input), &[message]);
        }
    }

    async fn whatsapp_status_and_art(home: &Path) {
        let config = home.join("config.yaml");
        handle_cli_whatsapp(None).await.unwrap();
        std::fs::create_dir(&config).unwrap();
        assert!(matches!(
            handle_cli_whatsapp(None).await,
            Err(hermes_core::AgentError::Io(_))
        ));
        std::fs::remove_dir(&config).unwrap();
        std::fs::write(&config, "platforms: [").unwrap();
        handle_cli_whatsapp(None).await.unwrap();

        std::fs::write(
            &config,
            "platforms:\n  whatsapp:\n    enabled: true\n    phone_number_id: fixture-phone\n    access_token: ''\n",
        )
        .unwrap();
        handle_cli_whatsapp(Some("status".into())).await.unwrap();
        handle_cli_whatsapp(Some("bogus".into())).await.unwrap();
        render_qr_to_terminal("");
        render_qr_to_terminal("aabbababxx");
        render_qr_to_terminal("bbbbbba");
    }

    #[test]
    fn pairing_reports_empty_store_and_requires_device_ids() {
        assert_output(
            &run_scenario("pairing", ""),
            &[
                "No paired devices.",
                "No pending pairing requests to clear.",
                "Pairing action 'bogus' is not recognized.",
            ],
        );
    }

    async fn pairing(home: &Path) {
        use crate::pairing_store::PairingStore;

        let store_path = PairingStore::default_path();
        assert!(store_path.starts_with(home.parent().unwrap()));
        handle_cli_pairing(None, None).await.unwrap();
        for action in ["approve", "revoke"] {
            assert_config_error(
                handle_cli_pairing(Some(action.into()), None)
                    .await
                    .unwrap_err(),
                &format!("Missing --device-id. Usage: hermes pairing {action}"),
            );
        }
        for action in ["clear-pending", "bogus"] {
            handle_cli_pairing(Some(action.into()), None).await.unwrap();
        }
        assert!(PairingStore::open_default().list().unwrap().is_empty());
    }

    #[test]
    fn claw_reports_legacy_files_and_only_cleans_after_confirmation() {
        assert_output(
            &run_scenario("claw", "no\n YeS \n"),
            &[
                "No OpenClaw directory found.",
                "Nothing to migrate.",
                "Nothing to clean up.",
                "config.yaml:       not found",
                "config.yaml:       present",
                ".env:              present",
                "skills/:           present",
                "sessions/:         1 file(s)",
                "Shell config references found:",
                "Cleanup cancelled.",
                "✓ Cleaned ~/.bashrc",
                "Cleanup complete.",
                "Claw action 'bogus' is not recognized.",
            ],
        );
    }

    async fn claw(hermes: &Path) {
        use crate::claw_migrate::find_openclaw_dir;

        let home = hermes.parent().unwrap();
        assert_eq!(dirs::home_dir().as_deref(), Some(home));
        assert!(find_openclaw_dir(None).unwrap().is_none());
        handle_cli_claw(None).await.unwrap();
        handle_cli_claw(Some("migrate".into())).await.unwrap();
        handle_cli_claw(Some("cleanup".into())).await.unwrap();

        let legacy = home.join(".openclaw");
        std::fs::create_dir(&legacy).unwrap();
        assert_eq!(
            find_openclaw_dir(None).unwrap().as_deref(),
            Some(legacy.as_path())
        );
        handle_cli_claw(None).await.unwrap();
        std::fs::create_dir(legacy.join("skills")).unwrap();
        std::fs::create_dir(legacy.join("sessions")).unwrap();
        std::fs::write(legacy.join("sessions").join("example.json"), "{}").unwrap();
        std::fs::write(legacy.join("config.yaml"), "{}").unwrap();
        std::fs::write(legacy.join(".env"), "FIXTURE=1\n").unwrap();
        let shell = home.join(".bashrc");
        let original = "# openclaw path\n# CLAWDBOT marker\nexport KEEP=1\n";
        std::fs::write(&shell, original).unwrap();

        handle_cli_claw(None).await.unwrap();
        handle_cli_claw(Some("cleanup".into())).await.unwrap();
        assert!(legacy.is_dir());
        assert_eq!(std::fs::read_to_string(&shell).unwrap(), original);
        handle_cli_claw(Some("cleanup".into())).await.unwrap();
        assert!(!legacy.exists());
        assert_eq!(std::fs::read_to_string(&shell).unwrap(), "export KEEP=1\n");
        handle_cli_claw(Some("bogus".into())).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "invoked by the isolated CLI tests"]
    async fn isolated_worker() {
        let Ok(scenario) = std::env::var(SCENARIO_ENV) else {
            return;
        };
        let home = PathBuf::from(std::env::var_os("HERMES_HOME").unwrap());
        assert_eq!(hermes_config::hermes_home(), home);
        match scenario.as_str() {
            "reports" => reports(&home).await,
            "exports" => exports(&home).await,
            "mutations" => mutations(&home).await,
            "repair" => repair(&home).await,
            "whatsapp-status" => whatsapp_status_and_art(&home).await,
            "whatsapp-setup" => {
                handle_cli_whatsapp(Some("setup".into())).await.unwrap();
                assert!(!home.join("config.yaml").exists());
            }
            "pairing" => pairing(&home).await,
            "claw" => claw(&home).await,
            other => panic!("unknown coverage scenario: {other}"),
        }
    }
}
