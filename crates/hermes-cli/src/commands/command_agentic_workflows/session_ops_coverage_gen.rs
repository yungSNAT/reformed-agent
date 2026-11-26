use super::*;
use clap::Parser;
use std::{fs, path::Path, sync::Arc};

const OUTPUT_START: &str = "<<< session ops output start >>>";
const OUTPUT_END: &str = "<<< session ops command end >>>";

async fn test_app() -> App {
    let cli = crate::cli::Cli::try_parse_from(["hermes"]).expect("parse test launch arguments");
    let app = App::new(cli).await.expect("initialize the isolated app");
    println!("{OUTPUT_START}");
    app
}

// The CLI emits these command results to stdout, not to the transcript.
// Capture stdout in a child test process that also owns its environment.
fn isolated_home(
    test_name: &str,
    extra_env: &[(&str, &str)],
    check_output: impl FnOnce(&Path, &[&str]),
) -> Option<PathBuf> {
    const CHILD_HOME: &str = "HERMES_SESSION_OPS_COVERAGE_CHILD_HOME";

    if let Some(home) = std::env::var_os(CHILD_HOME) {
        let home = PathBuf::from(home);
        assert_eq!(hermes_config::hermes_home(), home);
        return Some(home);
    }

    let dir = tempfile::tempdir().expect("create isolated Hermes home");
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary")); // nosemgrep: rust.lang.security.current-exe.current-exe
    command
        .args(["--exact", &format!("{module}::{test_name}"), "--nocapture"])
        .current_dir(dir.path())
        .env(CHILD_HOME, dir.path())
        .env("HERMES_HOME", dir.path())
        .env("HOME", dir.path())
        .env("USERPROFILE", dir.path())
        .env("APPDATA", dir.path())
        .env("LOCALAPPDATA", dir.path())
        .env("XDG_CONFIG_HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("XDG_STATE_HOME", dir.path())
        .env("XDG_CACHE_HOME", dir.path())
        .env_remove("HERMES_GEMINI_OAUTH_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY");

    for &(key, value) in extra_env {
        command.env(key, value);
    }

    let output = command.output().expect("run isolated session-ops test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "isolated test {test_name} did not pass:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let (_, after_start) = stdout
        .split_once(OUTPUT_START)
        .expect("child should mark the start of command output");
    let (command_output, _) = after_start
        .rsplit_once(OUTPUT_END)
        .expect("child should complete at least one command");
    let outputs: Vec<_> = command_output.split(OUTPUT_END).map(str::trim).collect();
    check_output(dir.path(), &outputs);
    None
}

fn handled(result: Result<CommandResult, AgentError>) {
    assert!(matches!(
        result.expect("command should succeed"),
        CommandResult::Handled
    ));
    println!("{OUTPUT_END}");
}

fn add_message(app: &mut App, role: hermes_core::MessageRole, content: &str) {
    app.messages.push(
        serde_json::from_value(serde_json::json!({
            "role": role,
            "content": content,
            "tool_call_id": null,
            "tool_calls": [],
            "reasoning_content": null,
        }))
        .expect("construct a transcript message"),
    );
}

#[tokio::test]
async fn compat_commands_and_session_insights_report_to_stdout() {
    if run_isolated("compat_commands_and_session_insights_report_to_stdout") {
        return;
    }

    let Some(home) = isolated_home(
        "compat_commands_and_session_insights_report_to_stdout",
        &[],
        |home, output| {
            assert_eq!(output.len(), 8);
            let session = output[0]
                .strip_prefix(
                    "Session insights:\n  - Total messages: 4\n  - User messages: 2\n  - Hermes messages: 1\n  - Session: ",
                )
                .expect("insights should report the correct role counts");
            assert!(!session.is_empty());
            assert_eq!(
                &output[1..5],
                &[
                    "Usage: /title <name>",
                    "Session title marker set to: coverage session",
                    "Use `/branch` (native) for list/diff/merge/save controls.",
                    "Compatibility command acknowledged.",
                ]
            );

            assert_eq!(
                output[5],
                format!(
                    concat!(
                        "Session snapshot format\n",
                        "  root keys: session_info, messages\n",
                        "  session_info keys: session_id, model, personality, message_count, created_at\n",
                        "  message keys: role, content, tool_call_id, tool_calls, reasoning_content\n",
                        "  save path: {}/sessions/<session-id>.json\n",
                        "Use `/save [name]` to persist a snapshot now."
                    ),
                    home.display()
                )
            );
            assert!(output[6].contains("Repaint pulse sent.\n"));
            assert_eq!(
                output[7],
                "Status bar is always enabled in the current TUI renderer."
            );
        },
    ) else {
        return;
    };
    let mut app = test_app().await;
    app.state_root = home;
    app.messages.clear();
    add_message(&mut app, hermes_core::MessageRole::System, "system context");
    add_message(&mut app, hermes_core::MessageRole::User, "first question");
    add_message(&mut app, hermes_core::MessageRole::Assistant, "an answer");
    add_message(&mut app, hermes_core::MessageRole::User, "follow-up");

    handled(handle_insights_command(&mut app));
    for (command, args) in [
        ("/title", &["  "][..]),
        ("/title", &["  coverage", "session  "][..]),
        ("/branch", &[][..]),
        ("/other", &[][..]),
    ] {
        handled(handle_session_compat_command(&mut app, command, args));
    }
    handled(handle_dump_format_command(&mut app));
    handled(handle_redraw_command(&mut app));
    handled(handle_statusbar_command(&mut app));
}

#[tokio::test]
async fn platform_lists_are_sorted_and_do_not_show_credential_values() {
    if run_isolated("platform_lists_are_sorted_and_do_not_show_credential_values") {
        return;
    }

    let Some(_home) = isolated_home(
        "platform_lists_are_sorted_and_do_not_show_credential_values",
        &[],
        |_, output| {
            assert_eq!(output.len(), 5);
            assert_eq!(
                &output[..4],
                &[
                    "No explicit gateway platform adapters configured (running in local CLI mode).",
                    "Configured gateway platforms:\n  - alpha\n  - zeta",
                    "Usage: /platform pause <name>",
                    "Unknown platform: beta",
                ]
            );
            assert!(output[4].starts_with("Usage: /platform <list|pause|resume> [name]\n"));
        },
    ) else {
        return;
    };
    let mut app = test_app().await;
    Arc::make_mut(&mut app.config).platforms.clear();

    handled(handle_platforms_command(&mut app));
    assert_eq!(
        render_platform_command_list(&app),
        "Gateway platforms\nConnected: local CLI\nConfigured adapters: (none)\nFailed/paused: unavailable in local CLI mode"
    );

    let platforms = &mut Arc::make_mut(&mut app.config).platforms;
    let zeta = platforms.entry("zeta".to_string()).or_default();
    zeta.enabled = false;
    zeta.token = Some("zeta-secret".to_string());
    zeta.webhook_url = None;
    let alpha = platforms.entry("alpha".to_string()).or_default();
    alpha.enabled = true;
    alpha.token = Some(" \t ".to_string());
    alpha.webhook_url = Some("https://example.invalid/private-hook".to_string());

    handled(handle_platforms_command(&mut app));
    assert_eq!(
        render_platform_command_list(&app),
        concat!(
            "Gateway platforms\nConnected: local CLI\nConfigured adapters:\n",
            "  - alpha: enabled=true token=missing webhook=configured\n",
            "  - zeta: enabled=false token=configured webhook=missing\n",
            "Failed/paused: unavailable in local CLI mode; run /platform from the gateway chat to control retry queues."
        )
    );

    handled(handle_platform_command(&mut app, &["PAUSE", " \t"]));
    handled(handle_platform_command(&mut app, &["resume", " beta "]));
    handled(handle_platform_command(&mut app, &["invalid"]));
    assert!(app.config.platforms["alpha"].enabled);
    assert!(!app.config.platforms["zeta"].enabled);
}

#[test]
fn repair_plan_distinguishes_passing_probes_from_actionable_failures() {
    if run_isolated("repair_plan_distinguishes_passing_probes_from_actionable_failures") {
        return;
    }

    let output = render_integrations_repair_steps(
        "test-provider",
        true,
        Some((false, "runtime is stale".to_string())),
        "PASS (local health check)",
    );
    assert!(output.contains("provider: test-provider\n- auth: PASS\n"));
    assert!(output.contains(
        "- oauth runtime gate: FAIL (runtime is stale) -> rebuild/install latest CLI binary.\n"
    ));
    assert!(output.contains("- contextlattice probe: PASS (local health check)\n"));
    assert!(!output.contains("verify local orchestrator"));

    let output = render_integrations_repair_steps("test-provider", false, None, "wArN (offline)");
    assert!(output.contains("- auth: FAIL -> run `/auth status` then `/auth verify`"));
    assert!(!output.contains("oauth runtime gate"));
    assert!(output.contains("- contextlattice probe: wArN (offline) -> verify local orchestrator"));
}

#[tokio::test]
async fn integrations_usage_and_snapshot_error_need_no_network() {
    if run_isolated("integrations_usage_and_snapshot_error_need_no_network") {
        return;
    }

    // Reqwest rejects the malformed URL before opening a connection.
    let Some(home) = isolated_home(
        "integrations_usage_and_snapshot_error_need_no_network",
        &[("CONTEXTLATTICE_ORCHESTRATOR_URL", "not-a-url")],
        |_, output| {
            assert_eq!(output.len(), 2);
            assert!(output[0].contains("contextlattice_url: not-a-url\n"));
            assert!(output[0].contains("probe: WARN (not-a-url/health error="));
            assert_eq!(
                output[1],
                "Usage: /integrations [status|all|auth|providers|gateway|memory|repair|snapshot]"
            );
        },
    ) else {
        return;
    };
    let mut app = test_app().await;

    handled(handle_integrations_command(&mut app, &["memory"]).await);
    handled(handle_integrations_command(&mut app, &["invalid"]).await);

    let logs = home.join("logs");
    if logs.is_dir() {
        fs::remove_dir_all(&logs).expect("remove isolated logs directory");
    }
    fs::write(&logs, "not a directory").expect("block snapshot parent creation");
    let result = handle_integrations_command(&mut app, &["snapshot"]).await;
    assert!(matches!(
        result,
        Err(AgentError::Io(message))
            if message.starts_with(&format!("Failed to create {}: ", logs.display()))
    ));
    assert_eq!(
        fs::read_to_string(logs).expect("read snapshot blocker"),
        "not a directory"
    );
}

#[tokio::test]
async fn clear_queue_only_removes_recognized_json_status_files() {
    if run_isolated("clear_queue_only_removes_recognized_json_status_files") {
        return;
    }

    let Some(home) = isolated_home(
        "clear_queue_only_removes_recognized_json_status_files",
        &[],
        |_, output| {
            assert_eq!(
                output,
                &[
                    "Cleared 0 queued/background status file(s).",
                    "Cleared 5 queued/background status file(s).",
                ]
            );
        },
    ) else {
        return;
    };
    let mut app = test_app().await;
    handled(handle_clear_queue_command(&mut app));

    let jobs = home.join("background_jobs");
    fs::create_dir_all(&jobs).expect("create jobs directory");
    let removable = [
        ("queued.json", r#"{"status":"queued"}"#),
        ("running.json", r#"{"status":"running"}"#),
        ("failed.json", r#"{"status":"failed"}"#),
        ("completed.json", r#"{"status":"completed"}"#),
        (
            "out-of-range.json",
            r#"{"status":"running","pid":18446744073709551615}"#,
        ),
    ];
    let retained = [
        ("notes.txt", r#"{"status":"queued"}"#),
        ("unknown.json", r#"{"status":"paused"}"#),
        ("malformed.json", "{"),
        ("not-an-object.json", r#"["queued"]"#),
    ];
    for (name, content) in removable.into_iter().chain(retained) {
        fs::write(jobs.join(name), content).expect("write job fixture");
    }

    handled(handle_clear_queue_command(&mut app));
    for (name, _) in removable {
        assert!(!jobs.join(name).exists(), "{name} was not cleared");
    }
    for (name, content) in retained {
        assert_eq!(
            fs::read_to_string(jobs.join(name)).expect("retained job fixture"),
            content
        );
    }
}

include!("session_ops_part2_coverage_gen.rs");
