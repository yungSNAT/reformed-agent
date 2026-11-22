use super::*;
use serde_json::json;

#[test]
fn flatten_parts_keeps_nonempty_text_and_image_urls_in_order() {
    let parts = [
        json!(42),
        json!({"type": "text", "text": "first"}),
        json!({"type": "text", "text": ""}),
        json!({"type": "text", "text": 42}),
        json!({"type": "custom", "text": "second"}),
        json!({"type": "custom", "text": ""}),
        json!({"type": "custom"}),
        json!({"type": "image_url", "image_url": {"url": "  data:image/png;base64,AA==  "}}),
        json!({"type": "input_image", "url": "  "}),
        json!({"text": "last"}),
    ];

    assert_eq!(
        flatten_openai_parts_to_text(&parts),
        "first\nsecond\n[Attached image]\nURL: data:image/png;base64,AA==\nlast"
    );
}

fn tool_call_json(id: &str, name: &str, arguments: &str) -> serde_json::Value {
    // The core ToolCall representation flattens the function fields.
    json!({
        "id": id,
        "type": "function",
        "name": name,
        "arguments": arguments,
    })
}

#[test]
fn history_preserves_tool_turns_and_appends_the_missing_user_turn() {
    let call = tool_call_json("lookup-1", "lookup", r#"{"query":"books"}"#);
    let history = [
        json!({"role": "system", "text": "Instructions"}),
        json!({"role": "user", "content": {"text": "Find books"}}),
        json!({"role": "assistant", "tool_calls": [call.clone()]}),
        json!({"role": "tool", "tool_call_id": "lookup-1", "content": "Three books"}),
        json!({"role": "assistant", "content": "Looking again", "tool_calls": [call]}),
        json!({"role": "tool", "content": {"text": "No more results"}}),
        json!({"role": "assistant", "content": "Done", "tool_calls": {"invalid": true}}),
        json!({"role": "unknown", "content": "ignore this"}),
        json!({"role": "user", "content": {"text": 12}}),
        json!({"role": "user", "content": ["not an OpenAI part"]}),
    ];

    let messages = acp_history_to_messages(&history, "  What did you find?  ");
    let contents: Vec<_> = messages.iter().map(|m| m.content.as_deref()).collect();
    assert_eq!(
        contents,
        [
            Some("Instructions"),
            Some("Find books"),
            None,
            Some("Three books"),
            Some("Looking again"),
            Some("No more results"),
            Some("Done"),
            Some("  What did you find?  "),
        ]
    );
    assert!(matches!(messages[0].role, hermes_core::MessageRole::System));
    assert!(matches!(messages[1].role, hermes_core::MessageRole::User));
    for index in [2, 4] {
        assert!(matches!(
            messages[index].role,
            hermes_core::MessageRole::Assistant
        ));
        let calls = messages[index].tool_calls.as_deref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "lookup-1");
        assert_eq!(calls[0].function.name, "lookup");
        assert_eq!(calls[0].function.arguments, r#"{"query":"books"}"#);
    }
    assert!(matches!(messages[3].role, hermes_core::MessageRole::Tool));
    assert_eq!(messages[3].tool_call_id.as_deref(), Some("lookup-1"));
    assert!(matches!(messages[5].role, hermes_core::MessageRole::Tool));
    assert_eq!(messages[5].tool_call_id.as_deref(), Some("tool_call"));
    assert!(matches!(
        messages[6].role,
        hermes_core::MessageRole::Assistant
    ));
    assert!(messages[6].tool_calls.is_none());
    assert!(matches!(messages[7].role, hermes_core::MessageRole::User));
}

#[test]
fn history_does_not_append_a_duplicate_or_blank_user_turn() {
    let history = [json!({
        "role": "user",
        "content": [{"type": "text", "text": "one"}, {"type": "text", "text": "two"}]
    })];
    let messages = acp_history_to_messages(&history, "duplicate");
    assert_eq!(messages.len(), 1);
    assert!(matches!(messages[0].role, hermes_core::MessageRole::User));
    assert_eq!(messages[0].content.as_deref(), Some("one\ntwo"));
    assert!(acp_history_to_messages(&[], " \t\n ").is_empty());
}

#[test]
fn tool_arguments_distinguish_absent_json_and_raw_text() {
    assert_eq!(acp_tool_arguments(" \t\n "), None);
    assert_eq!(
        acp_tool_arguments(" {\"limit\":2} \n"),
        Some(json!({"limit": 2}))
    );
    assert_eq!(acp_tool_arguments("null"), Some(serde_json::Value::Null));
    assert_eq!(
        acp_tool_arguments("  raw arguments  "),
        Some(json!("  raw arguments  "))
    );
}

#[test]
fn tool_events_generate_sequential_ids_and_ignore_unassociated_results() {
    let calls = serde_json::from_value::<Vec<hermes_core::ToolCall>>(json!([
        tool_call_json(" ", "lookup", " "),
        tool_call_json("explicit-id", "read", "{}"),
        tool_call_json("", "list", "{}"),
    ]))
    .unwrap();
    let mut orphan_result = hermes_core::Message::tool_result("orphan", "ignored result");
    orphan_result.tool_call_id = None;
    let messages = [
        hermes_core::Message::user("Find books"),
        hermes_core::Message::assistant_with_tool_calls(None, calls),
        orphan_result,
        hermes_core::Message::tool_result("tc-00000001", "result from lookup"),
    ];

    let events = acp_events_from_agent_messages("test-session", &messages);
    assert_eq!(events.len(), 4);
    for (event, (id, name)) in events.iter().zip([
        ("tc-00000001", "lookup"),
        ("explicit-id", "read"),
        ("tc-00000002", "list"),
        ("tc-00000001", "lookup"),
    ]) {
        let description = format!("{event:?}");
        for expected in ["test-session", id, name] {
            assert!(description.contains(expected), "{description}");
        }
    }
}

#[test]
fn secret_file_detection_checks_exact_names_and_sensitive_extensions() {
    for name in [
        ".env",
        "auth.json",
        "state.db",
        "local.env",
        "app.conf",
        "data.json",
    ] {
        assert!(backup_secret_like_file(Path::new(name)), "{name}");
    }
    for name in [".", "", "README", "notes.txt", "other.db", "auth.json.bak"] {
        assert!(!backup_secret_like_file(Path::new(name)), "{name}");
    }
}

#[test]
fn archive_paths_are_normalized_and_routed_to_the_correct_root() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("home");
    let hermes = home.join(".hermes");

    assert_eq!(
        safe_relative_archive_path(Path::new("./hermes/settings.toml")),
        Some(PathBuf::from("hermes/settings.toml"))
    );
    for rejected in [
        "",
        ".",
        "hermes/../settings.toml",
        "../settings.toml",
        "/settings.toml",
    ] {
        assert_eq!(safe_relative_archive_path(Path::new(rejected)), None);
        assert_eq!(
            restore_archive_entry_target(Path::new(rejected), &hermes, &home),
            None
        );
    }
    for prefix_only in ["hermes", "external", "legacy"] {
        assert_eq!(
            restore_archive_entry_target(Path::new(prefix_only), &hermes, &home),
            None
        );
    }
    for (member, target) in [
        ("./hermes/config.toml", hermes.join("config.toml")),
        (
            "external/provider/data.json",
            home.join("provider/data.json"),
        ),
        ("legacy/notes.txt", hermes.join("legacy/notes.txt")),
    ] {
        assert_eq!(
            restore_archive_entry_target(Path::new(member), &hermes, &home),
            Some(target)
        );
    }
}

#[test]
fn regular_file_collection_handles_missing_files_and_nested_directories() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path();
    let direct = root.join("top.txt");
    let nested = root.join("one/two/nested.txt");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    std::fs::write(&direct, b"top").unwrap();
    std::fs::write(&nested, b"nested").unwrap();

    assert!(backup_collect_regular_files(&root.join("missing")).is_empty());
    assert_eq!(backup_collect_regular_files(&direct), vec![direct.clone()]);
    let mut collected = backup_collect_regular_files(root);
    collected.sort();
    assert_eq!(collected, vec![nested, direct]);
}

#[cfg(unix)]
#[test]
fn regular_file_collection_does_not_follow_symlinks() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("root");
    let outside = sandbox.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let kept = root.join("kept.txt");
    let outside_file = outside.join("outside.txt");
    std::fs::write(&kept, b"keep").unwrap();
    std::fs::write(&outside_file, b"do not collect").unwrap();

    let directory_link = root.join("linked-directory");
    let file_link = root.join("linked-file");
    std::os::unix::fs::symlink(&outside, &directory_link).unwrap();
    std::os::unix::fs::symlink(&outside_file, &file_link).unwrap();

    assert!(backup_collect_regular_files(&directory_link).is_empty());
    assert!(backup_collect_regular_files(&file_link).is_empty());
    assert_eq!(backup_collect_regular_files(&root), vec![kept]);
}

#[cfg(target_os = "linux")]
#[test]
fn regular_file_collection_excludes_pipes_without_opening_them() {
    use std::os::unix::ffi::OsStrExt;

    let sandbox = tempfile::tempdir().unwrap();
    let pipe = sandbox.path().join("pipe");
    let path = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is a live, NUL-terminated path in the temporary directory.
    let status = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(
        status,
        0,
        "could not create test FIFO: {}",
        std::io::Error::last_os_error()
    );
    assert!(backup_collect_regular_files(&pipe).is_empty());
    assert!(backup_collect_regular_files(sandbox.path()).is_empty());
}

#[test]
fn missing_home_prevents_external_backup_discovery() {
    let sandbox = tempfile::tempdir().unwrap();
    assert!(collect_memory_provider_external_backup_files(
        &sandbox.path().join("hermes"),
        &sandbox.path().join("missing-home"),
    )
    .is_empty());
}

fn fixture_archive(
    path: &Path,
    members: &[(&str, tar::EntryType, &[u8])],
) -> tar::Archive<flate2::read::GzDecoder<std::fs::File>> {
    let file = std::fs::File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for &(name, entry_type, body) in members {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(entry_type);
        header.set_mode(0o644);
        header.set_size(u64::try_from(body.len()).unwrap());
        if entry_type == tar::EntryType::Symlink {
            header.set_link_name("config.toml").unwrap();
        }
        builder.append_data(&mut header, name, body).unwrap();
    }
    drop(builder.into_inner().unwrap().finish().unwrap());
    tar::Archive::new(flate2::read::GzDecoder::new(
        std::fs::File::open(path).unwrap(),
    ))
}

#[test]
fn restore_creates_directories_skips_links_and_counts_external_files() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("home");
    let hermes = home.join(".hermes");
    let mut archive = fixture_archive(
        &sandbox.path().join("backup.tar.gz"),
        &[
            ("hermes", tar::EntryType::Directory, b""),
            ("hermes/empty", tar::EntryType::Directory, b""),
            (
                "hermes/conf/config.toml",
                tar::EntryType::Regular,
                b"settings",
            ),
            ("hermes/conf/alias", tar::EntryType::Symlink, b""),
            ("legacy/notes.txt", tar::EntryType::Regular, b"legacy"),
            (
                "external/provider/auth.json",
                tar::EntryType::Regular,
                b"{}",
            ),
            (
                "external/provider/readme.txt",
                tar::EntryType::Regular,
                b"notes",
            ),
        ],
    );

    assert_eq!(
        restore_backup_archive(&mut archive, &hermes, &home).unwrap(),
        (4, 2)
    );
    assert!(hermes.join("empty").is_dir());
    assert!(std::fs::symlink_metadata(hermes.join("conf/alias")).is_err());
    let provider_auth = home.join("provider/auth.json");
    for (path, body) in [
        (hermes.join("conf/config.toml"), b"settings".as_slice()),
        (hermes.join("legacy/notes.txt"), b"legacy".as_slice()),
        (provider_auth.clone(), b"{}".as_slice()),
        (home.join("provider/readme.txt"), b"notes".as_slice()),
    ] {
        assert_eq!(std::fs::read(&path).unwrap().as_slice(), body, "{path:?}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let permissions = std::fs::metadata(provider_auth).unwrap().permissions();
        assert_eq!(permissions.mode() & 0o777, 0o600);
    }
}

#[test]
fn restore_reports_directory_creation_errors_without_overwriting_a_file() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("home");
    let hermes = home.join(".hermes");
    std::fs::create_dir_all(&hermes).unwrap();
    let obstruction = hermes.join("occupied");
    std::fs::write(&obstruction, b"keep").unwrap();

    for (name, entry_type) in [
        ("hermes/occupied", tar::EntryType::Directory),
        ("hermes/occupied/child", tar::EntryType::Regular),
    ] {
        let mut archive = fixture_archive(
            &sandbox.path().join("backup.tar.gz"),
            &[(name, entry_type, b"")],
        );
        let error = restore_backup_archive(&mut archive, &hermes, &home).unwrap_err();
        assert!(
            matches!(error, hermes_core::AgentError::Io(ref message) if message.starts_with("Create dir error: ")),
            "{error}"
        );
        assert_eq!(std::fs::read(&obstruction).unwrap().as_slice(), b"keep");
    }
}

#[tokio::test]
async fn import_reports_the_path_of_a_missing_archive() {
    let sandbox = tempfile::tempdir().unwrap();
    let missing = sandbox.path().join("missing.tar.gz");
    let path = missing.to_str().unwrap();
    let error = handle_cli_import(path.to_string()).await.unwrap_err();
    assert!(
        matches!(error, hermes_core::AgentError::Io(ref message) if message == &format!("Backup archive not found: {path}")),
        "{error}"
    );
}

#[cfg(unix)]
fn run_isolated_test(test: &str, sandbox: &Path) -> (String, String) {
    let (_, module) = module_path!().split_once("::").unwrap();
    let test = format!("{module}::{test}");
    let home = sandbox.join("home");
    let bin = sandbox.join("bin");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&bin).unwrap();

    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &test, "--ignored", "--nocapture"])
        .current_dir(sandbox)
        .env_clear()
        .env("HERMES_ACP_COVERAGE_SANDBOX", sandbox)
        .env("HOME", &home)
        .env("HERMES_HOME", home.join(".hermes"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("PATH", bin)
        .stdin(std::process::Stdio::null());
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env(
            "LLVM_PROFILE_FILE",
            std::env::current_dir().unwrap().join(profile),
        );
    }
    let output = command.output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{test}\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("running 1 test"),
        "{test}\n{stdout}\n{stderr}"
    );
    (stdout.into_owned(), stderr.into_owned())
}

#[cfg(unix)]
#[test]
fn acp_informational_actions_print_their_own_status_and_guidance() {
    let sandbox = tempfile::tempdir().unwrap();
    let (stdout, _) = run_isolated_test("isolated_acp_actions", sandbox.path());
    for expected in [
        "Hermes ACP check OK",
        "ACP setup is handled by the Rust model/provider setup flow.",
        "ACP server: not running",
        "ACP stop is not a separate command in stdio mode.",
        "ACP restart in stdio mode is equivalent to stop + start.",
        "Unknown ACP action 'coverage-unknown'.",
        "Available actions: start, status, stop, restart, check, setup, setup-browser, version",
        "ACP browser dependency node (`node`): OK\n",
        "ACP browser dependency browser (`agent-browser`): OK\n",
        "ACP browser dependency node (`node`): OK (non-interactive)",
        "ACP browser dependency browser (`agent-browser`): OK (non-interactive)",
        "Hermes ACP browser setup OK",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?}:\n{stdout}");
    }
    assert_eq!(stdout.matches("Hermes ACP browser setup OK").count(), 2);
    let version = hermes_core::version::version_label();
    assert!(stdout.lines().any(|line| line == version), "{stdout}");
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "run in a subprocess with an isolated PATH and home"]
async fn isolated_acp_actions() {
    use std::os::unix::fs::PermissionsExt;

    let Some(sandbox) = std::env::var_os("HERMES_ACP_COVERAGE_SANDBOX").map(PathBuf::from) else {
        return;
    };
    for action in [
        "check",
        "version",
        "--version",
        "setup",
        "status",
        "stop",
        "restart",
        "coverage-unknown",
    ] {
        handle_cli_acp(Some(action.to_string()), false)
            .await
            .unwrap();
    }

    let error = handle_cli_acp(Some("setup-browser".to_string()), true)
        .await
        .unwrap_err();
    assert!(
        matches!(error, hermes_core::AgentError::Config(ref message)
            if message.contains("node dependency command `node`")
                && message.contains("hermes acp setup-browser --yes")),
        "{error}"
    );

    for command in ["node", "agent-browser"] {
        let executable = sandbox.join("bin").join(command);
        std::fs::write(&executable, b"#!/bin/sh\ntrue\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    handle_cli_acp(Some("setup-browser".to_string()), false)
        .await
        .unwrap();
    handle_cli_acp(Some("setup_browser".to_string()), true)
        .await
        .unwrap();
}

#[cfg(unix)]
#[test]
fn acp_default_action_starts_on_stdio_with_no_terminal_attached() {
    let sandbox = tempfile::tempdir().unwrap();
    let (_, stderr) = run_isolated_test("isolated_acp_start", sandbox.path());
    assert!(stderr.contains("Starting ACP server (model="), "{stderr}");
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "run in a subprocess with closed stdin and an isolated home"]
async fn isolated_acp_start() {
    let Some(sandbox) = std::env::var_os("HERMES_ACP_COVERAGE_SANDBOX").map(PathBuf::from) else {
        return;
    };
    let cron_dir = hermes_config::cron_dir();
    assert!(cron_dir.starts_with(sandbox.join("home")));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        handle_cli_acp(None, false),
    )
    .await
    .expect("stdio server should return at EOF")
    .unwrap();
    assert!(cron_dir.is_dir());
}

#[cfg(unix)]
#[test]
fn backup_handles_an_absent_home_and_round_trips_a_default_named_archive() {
    let sandbox = tempfile::tempdir().unwrap();
    let (stdout, _) = run_isolated_test("isolated_backup_and_import", sandbox.path());
    assert!(
        stdout.contains("Hermes home directory not found at "),
        "{stdout}"
    );
    assert!(
        stdout.contains("Backup complete: hermes-backup-"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Import complete. 1 files restored to "),
        "{stdout}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "run in a subprocess with an isolated home and working directory"]
async fn isolated_backup_and_import() {
    let Some(sandbox) = std::env::var_os("HERMES_ACP_COVERAGE_SANDBOX").map(PathBuf::from) else {
        return;
    };
    let hermes = sandbox.join("home/.hermes");
    assert_eq!(hermes_config::hermes_home(), hermes);
    assert!(!hermes.exists());

    let unused_output = sandbox.join("should-not-exist.tar.gz");
    handle_cli_backup(Some(unused_output.to_str().unwrap().to_string()))
        .await
        .unwrap();
    assert!(!unused_output.exists());
    assert!(!hermes.exists());

    std::fs::create_dir(&hermes).unwrap();
    let settings = hermes.join("settings.txt");
    std::fs::write(&settings, b"workspace settings").unwrap();
    handle_cli_backup(None).await.unwrap();

    let archives: Vec<_> = std::fs::read_dir(&sandbox)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("hermes-backup-") && name.ends_with(".tar.gz"))
        })
        .collect();
    assert_eq!(archives.len(), 1);
    std::fs::remove_dir_all(&hermes).unwrap();
    handle_cli_import(archives[0].to_str().unwrap().to_string())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(settings).unwrap().as_slice(),
        b"workspace settings"
    );

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;

        // A socket inode exists but cannot be opened as a file. No socket is bound.
        let socket = sandbox.join("unopenable.tar.gz");
        let raw_path = std::ffi::CString::new(socket.as_os_str().as_bytes()).unwrap();
        // SAFETY: `raw_path` is a live, NUL-terminated path in the temporary directory.
        let status = unsafe { libc::mknod(raw_path.as_ptr(), libc::S_IFSOCK | 0o600, 0) };
        assert_eq!(
            status,
            0,
            "could not create test socket inode: {}",
            std::io::Error::last_os_error()
        );
        let path = socket.to_str().unwrap();
        let error = handle_cli_import(path.to_string()).await.unwrap_err();
        assert!(
            matches!(error, hermes_core::AgentError::Io(ref message)
                if message.starts_with(&format!("Cannot open {path}: "))),
            "{error}"
        );
    }
}
