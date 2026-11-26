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

    let mut command = std::process::Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
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
        let status = unsafe { libc::mknod(raw_path.as_ptr(), libc::S_IFSOCK | 0o600, 0) }; // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
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
