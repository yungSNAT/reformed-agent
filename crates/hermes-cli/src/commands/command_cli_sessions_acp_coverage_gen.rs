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
        run_scenario_in(root.path(), scenario, input)
    }

    fn run_scenario_in(root: &Path, scenario: &str, input: &str) -> String {
        let home = root.join("home");
        let hermes = home.join(".hermes");
        std::fs::create_dir_all(&hermes).unwrap();

        let mut stdin = tempfile::tempfile().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
        stdin.rewind().unwrap();

        let (_, module) = module_path!().split_once("::").unwrap();
        let test_name = format!("{module}::isolated_worker");
        let mut command = Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
        command
            .args(["--exact", &test_name, "--ignored", "--nocapture"])
            .env_clear()
            .env("HOME", &home)
            .env("HERMES_HOME", &hermes)
            .env("TMPDIR", root)
            .env(SCENARIO_ENV, scenario)
            .current_dir(root)
            .stdin(stdin);
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", std::env::current_dir().unwrap().join(profile));
        }
        for name in [
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

    include!("command_cli_sessions_acp_part2_coverage_gen.rs");
}
