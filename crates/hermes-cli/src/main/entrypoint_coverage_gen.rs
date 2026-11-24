use super::*;

const PROBE_ENV: &str = "HERMES_ENTRYPOINT_COVERAGE_PROBE";

fn sandboxed_command(program: &std::path::Path, root: &std::path::Path) -> std::process::Command {
    let user_home = root.join("user");
    let hermes_home = root.join("hermes");
    std::fs::create_dir_all(&user_home).expect("create temporary user home");
    std::fs::create_dir_all(&hermes_home).expect("create temporary Hermes home");

    let mut command = std::process::Command::new(program);
    command
        .env_clear()
        .current_dir(root)
        .env("HOME", &user_home)
        .env("USERPROFILE", &user_home)
        .env("APPDATA", user_home.join("appdata"))
        .env("LOCALAPPDATA", user_home.join("local-appdata"))
        .env("XDG_CONFIG_HOME", user_home.join("config"))
        .env("XDG_DATA_HOME", user_home.join("data"))
        .env("XDG_CACHE_HOME", user_home.join("cache"))
        .env("XDG_STATE_HOME", user_home.join("state"))
        .env("HERMES_HOME", hermes_home)
        .stdin(std::process::Stdio::null());

    // Resolve relative profile paths before changing the child's working directory.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        let profile = std::env::current_dir()
            .expect("locate parent working directory")
            .join(profile);
        command.env("LLVM_PROFILE_FILE", profile);
    }

    command
}

fn run_child(command: std::process::Command) -> std::process::Output {
    let description = format!("{command:?}");
    let output = assert_cmd::Command::from_std(command)
        .timeout(std::time::Duration::from_secs(20))
        .output()
        .expect("run isolated child");
    assert!(
        output.status.success(),
        "{description} returned {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn test_probe(full_name: &str, mode: &str, root: &std::path::Path) -> std::process::Command {
    let (_, test_name) = full_name
        .split_once("::")
        .expect("test name starts with the crate name");
    let executable = std::env::current_exe().expect("locate unit-test executable"); // nosemgrep: rust.lang.security.current-exe.current-exe
    let mut command = sandboxed_command(&executable, root);
    command
        .args(["--exact", test_name, "--nocapture"])
        .env(PROBE_ENV, mode);
    command
}

fn standalone_cli() -> Option<std::path::PathBuf> {
    if let Some(path) = option_env!("CARGO_BIN_EXE_hermes-agent-ultra") {
        return Some(path.into());
    }

    // A unit-only build may not have the standalone binary.
    let executable = std::env::current_exe().expect("locate unit-test executable");
    let deps = executable.parent()?;
    if deps.file_name()? != "deps" {
        return None;
    }
    let binary = deps.parent()?.join(format!(
        "hermes-agent-ultra{}",
        std::env::consts::EXE_SUFFIX
    ));
    binary.is_file().then_some(binary)
}

fn parse_command(args: &[&str]) -> CliCommand {
    <Cli as clap::Parser>::try_parse_from(
        std::iter::once("hermes-agent-ultra").chain(args.iter().copied()),
    )
    .expect("valid CLI arguments")
    .effective_command()
}

#[test]
fn verbose_tracing_uses_different_defaults_for_tui_and_cli() {
    match std::env::var(PROBE_ENV).as_deref() {
        Ok("tui-tracing") => {
            init_tracing(true, true);
            assert_eq!(std::env::var("RUST_LOG").as_deref(), Ok("info"));
            assert!(tracing::enabled!(tracing::Level::INFO));
            assert!(!tracing::enabled!(tracing::Level::DEBUG));
            println!("tui tracing verified");
        }
        Ok("cli-tracing") => {
            init_tracing(true, false);
            assert!(tracing::enabled!(tracing::Level::DEBUG));
            assert!(!tracing::enabled!(tracing::Level::TRACE));
            println!("cli tracing verified");
        }
        _ => {
            // A global subscriber cannot be reset, so use a fresh process for each default.
            for (mode, marker) in [
                ("tui-tracing", "tui tracing verified"),
                ("cli-tracing", "cli tracing verified"),
            ] {
                let root = tempfile::tempdir().expect("create tracing sandbox");
                let mut command = test_probe(
                    concat!(
                        module_path!(),
                        "::verbose_tracing_uses_different_defaults_for_tui_and_cli"
                    ),
                    mode,
                    root.path(),
                );
                if mode == "tui-tracing" {
                    command.env("RUST_LOG", "trace");
                }
                let output = run_child(command);
                assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
            }
        }
    }
}

#[test]
fn completion_writes_a_bash_registration() {
    let args = [
        "--ignore-user-config",
        "--ignore-rules",
        "completion",
        "bash",
    ];
    let CliCommand::Completion(CompletionArgs { shell }) = parse_command(&args) else {
        panic!("expected the completion command");
    };

    if std::env::var(PROBE_ENV).as_deref() == Ok("bash-completion") {
        run_completion(shell).expect("generate Bash completions");
        return;
    }

    let root = tempfile::tempdir().expect("create completion sandbox");
    let command = if let Some(binary) = standalone_cli() {
        let mut command = sandboxed_command(&binary, root.path());
        command.args(args);
        command
    } else {
        test_probe(
            concat!(module_path!(), "::completion_writes_a_bash_registration"),
            "bash-completion",
            root.path(),
        )
    };
    let output = run_child(command);
    let script = String::from_utf8_lossy(&output.stdout);
    assert!(script.contains("COMPREPLY"), "{script}");
    assert!(
        script
            .lines()
            .any(|line| line.trim_start().starts_with("complete ")),
        "{script}"
    );
    assert!(script.contains("chat"), "{script}");
}

#[test]
fn acp_version_flag_selects_version_action() {
    let args = ["acp", "--version"];
    let CliCommand::Acp(AcpArgs {
        action,
        check,
        setup,
        setup_browser,
        version,
        ..
    }) = parse_command(&args)
    else {
        panic!("expected the ACP command");
    };
    assert_eq!(
        acp_action_from_flags(action, check, setup, setup_browser, version).as_deref(),
        Some("version")
    );

    if let Some(binary) = standalone_cli() {
        let root = tempfile::tempdir().expect("create ACP sandbox");
        let mut command = sandboxed_command(&binary, root.path());
        command.args(args);
        let output = run_child(command);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.lines().any(|line| {
                line.trim()
                    .strip_prefix("Reformed Agent v")
                    .is_some_and(|version| version.starts_with(|ch: char| ch.is_ascii_digit()))
            }),
            "expected the Reformed Agent version banner, got {stdout:?}"
        );
    }
}

#[test]
fn local_list_commands_select_their_own_handlers() {
    let binary = standalone_cli();
    for (name, expected_words) in [
        ("cron", ["cron", "job"]),
        ("webhook", ["webhook", "route"]),
        ("mcp", ["mcp", "server"]),
    ] {
        let args = [name, "list"];
        let action = match (name, parse_command(&args)) {
            ("cron", CliCommand::Cron(CronArgs { action, .. }))
            | ("webhook", CliCommand::Webhook(WebhookArgs { action, .. }))
            | ("mcp", CliCommand::Mcp(McpArgs { action, .. })) => action,
            _ => panic!("{name} list selected the wrong command"),
        };
        assert_eq!(action.as_deref(), Some("list"), "{name}");

        if let Some(binary) = &binary {
            let root = tempfile::tempdir().expect("create empty list-command sandbox");
            let mut command = sandboxed_command(binary, root.path());
            command.args(args);
            let output = run_child(command);
            let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
            assert!(
                text.trim() == "[]" || expected_words.iter().any(|word| text.contains(*word)),
                "{name} list did not describe its inventory: {text:?}"
            );
        }
    }
}
