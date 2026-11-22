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
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
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

#[tokio::test]
async fn logs_are_limited_and_feedback_appends_only_nonempty_notes() {
    if run_isolated("logs_are_limited_and_feedback_appends_only_nonempty_notes") {
        return;
    }

    let Some(home) = isolated_home(
        "logs_are_limited_and_feedback_appends_only_nonempty_notes",
        &[],
        |home, output| {
            let logs = home.join("logs");
            assert_eq!(output.len(), 6);
            assert_eq!(
                output[0],
                format!("No log files found in {}", logs.display())
            );

            let mut lines = output[1].lines();
            assert_eq!(
                lines.next(),
                Some(format!("Recent log files in {}:", logs.display()).as_str())
            );
            for index in (2..14).rev() {
                assert_eq!(lines.next(), Some(format!("  - {index:02}.log").as_str()));
            }
            assert_eq!(
                lines.next(),
                Some("Use `hermes logs` for full tail output.")
            );
            assert_eq!(lines.next(), None);

            assert!(output[2].starts_with("Usage: /feedback <note>\n"));
            assert_eq!(output[3], "Usage: /feedback <note>");
            let confirmation = format!(
                "Feedback captured in {}",
                logs.join("feedback.ndjson").display()
            );
            assert_eq!(
                &output[4..],
                &[confirmation.as_str(), confirmation.as_str()]
            );
        },
    ) else {
        return;
    };
    let mut app = test_app().await;
    let logs = home.join("logs");
    handled(handle_log_command(&mut app));

    fs::create_dir_all(logs.join("zzz-directory")).expect("create logs fixture");
    for index in 0..14 {
        fs::write(logs.join(format!("{index:02}.log")), b"fixture").expect("write log fixture");
    }
    handled(handle_log_command(&mut app));

    let feedback = logs.join("feedback.ndjson");
    handled(handle_feedback_command(&mut app, &[]));
    handled(handle_feedback_command(&mut app, &[" \t", " "]));
    assert!(!feedback.exists());

    handled(handle_feedback_command(&mut app, &["  first", "note  "]));
    handled(handle_feedback_command(&mut app, &["second note"]));
    let text = fs::read_to_string(&feedback).expect("read captured feedback");
    let records: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid feedback JSON line"))
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["note"], "first note");
    assert_eq!(records[1]["note"], "second note");
    for record in &records {
        assert_eq!(record["session_id"].as_str(), Some(app.session_id.as_str()));
        assert_eq!(record["model"].as_str(), Some(app.current_model.as_str()));
        assert!(record["at"].is_string());
    }
}

#[tokio::test]
async fn experiment_steering_changes_only_when_requested() {
    if run_isolated("experiment_steering_changes_only_when_requested") {
        return;
    }

    let Some(_home) = isolated_home(
        "experiment_steering_changes_only_when_requested",
        &[],
        |_, output| {
            let usage = "Usage: /experiment <label or instruction> | /experiment clear";
            assert_eq!(output.len(), 6);
            assert_eq!(output[0], format!("Experiment steering: (none)\n{usage}"));
            assert_eq!(output[1], usage);
            assert_eq!(
                output[2],
                "No experiment steering context active. Use `/experiment <instruction>`."
            );
            assert!(
                output[3].starts_with("Experiment steering applied.\nExperiment: compare models\n")
            );
            assert_eq!(
                output[4],
                format!("Experiment steering: compare models\n{usage}")
            );
            assert_eq!(output[5], "Cleared experiment steering context.");
        },
    ) else {
        return;
    };
    let mut app = test_app().await;

    handled(handle_experiment_command(&mut app, &[]));
    handled(handle_experiment_command(&mut app, &[" \t", " "]));
    assert!(current_session_steer(&app).is_none());

    set_session_steer(&mut app, Some("ordinary steering".to_string()));
    handled(handle_experiment_command(&mut app, &["CLEAR"]));
    assert_eq!(
        current_session_steer(&app).as_deref(),
        Some("ordinary steering")
    );

    handled(handle_experiment_command(
        &mut app,
        &["  compare", "models  "],
    ));
    assert_eq!(
        current_session_steer(&app).as_deref(),
        Some("Experiment: compare models")
    );
    handled(handle_experiment_command(&mut app, &[]));
    handled(handle_experiment_command(&mut app, &["clear"]));
    assert!(current_session_steer(&app).is_none());
}

#[tokio::test]
async fn restart_discards_the_previous_turn_and_keeps_the_requested_model() {
    if run_isolated("restart_discards_the_previous_turn_and_keeps_the_requested_model") {
        return;
    }

    let Some(_home) = isolated_home(
        "restart_discards_the_previous_turn_and_keeps_the_requested_model",
        &[],
        |_, output| {
            assert_eq!(output.len(), 1);
            let report = output[0]
                .strip_prefix("Session restarted.\n  new_session_id: ")
                .expect("restart should report the new session ID");
            let (session, model) = report
                .split_once("\n  model: ")
                .expect("restart should report the model");
            assert!(!session.is_empty());
            assert!(!model.is_empty());
        },
    ) else {
        return;
    };
    let mut app = test_app().await;
    let old_id = app.session_id.clone();
    let old_model = app.current_model.clone();
    add_message(&mut app, hermes_core::MessageRole::User, "previous turn");

    handled(handle_restart_command(&mut app, &["--KEEP-MODEL"]));
    assert_ne!(app.session_id, old_id);
    assert_eq!(app.current_model, old_model);
    assert!(app
        .messages
        .iter()
        .all(|message| message.content.as_deref() != Some("previous turn")));
}

#[tokio::test]
async fn paste_show_uses_the_full_mock_clipboard_and_copy_skips_blank_answers() {
    if run_isolated("paste_show_uses_the_full_mock_clipboard_and_copy_skips_blank_answers") {
        return;
    }

    let text = format!("{} full-preview-tail", "λ".repeat(300));
    let mock = format!("  {text}\n");
    let Some(_home) = isolated_home(
        "paste_show_uses_the_full_mock_clipboard_and_copy_skips_blank_answers",
        &[("HERMES_TEST_CLIPBOARD_TEXT", &mock)],
        |home, output| {
            assert_eq!(output.len(), 2);
            assert_eq!(
                output[0],
                "Copy skipped: no assistant message content available yet."
            );
            let mut entries = fs::read_dir(home.join("pastes")).expect("read paste directory");
            let file = entries
                .next()
                .expect("one paste")
                .expect("paste entry")
                .path();
            assert!(entries.next().is_none());
            assert_eq!(fs::read_to_string(&file).expect("read paste file"), text);
            assert!(output[1].contains(&format!("  - saved: {}\n", file.display())));
            assert!(output[1].contains(&format!("  - chars: {}\n", text.chars().count())));
            assert!(output[1].contains(&format!("  - preview: {text}\n")));
        },
    ) else {
        return;
    };
    let mut app = test_app().await;
    app.messages.clear();
    add_message(&mut app, hermes_core::MessageRole::Assistant, " \t\n ");
    add_message(&mut app, hermes_core::MessageRole::User, "user-only text");

    // Keep this fixture on the path that skips accessing the real clipboard.
    assert!(app.transcript_messages().iter().all(|message| {
        message.role != hermes_core::MessageRole::Assistant
            || message
                .content
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
    }));
    handled(handle_copy_command(&mut app));
    handled(handle_paste_command(&mut app, &["SHOW"]));
}

#[tokio::test]
async fn quota_diagnostics_report_credential_names_without_values() {
    if run_isolated("quota_diagnostics_report_credential_names_without_values") {
        return;
    }

    let Some(_home) = isolated_home(
        "quota_diagnostics_report_credential_names_without_values",
        &[("GOOGLE_API_KEY", "mock-key-not-for-display")],
        |_, output| {
            assert_eq!(output.len(), 1);
            let report = output[0];
            assert!(report.contains("  - active provider: google-gemini-cli\n"));
            assert!(report.contains("  - gemini creds in env: yes (GOOGLE_API_KEY)\n"));
            assert!(report.contains("  - oauth state file: missing\n"));
            assert!(!report.contains("mock-key-not-for-display"));
            assert!(!report.contains("token expiry:"));
        },
    ) else {
        return;
    };
    let mut app = test_app().await;
    app.current_model = " Google-Gemini-CLI :gemini-test".to_string();
    handled(handle_gquota_command(&mut app, &[]).await);
}

#[tokio::test]
async fn pairing_commands_report_an_empty_store_and_unknown_devices() {
    if run_isolated("pairing_commands_report_an_empty_store_and_unknown_devices") {
        return;
    }

    let Some(_home) = isolated_home(
        "pairing_commands_report_an_empty_store_and_unknown_devices",
        &[],
        |_, output| {
            assert_eq!(output.len(), 6);
            assert_eq!(
                output[0],
                "No pending devices to approve. Use `hermes pairing list` for full inventory."
            );
            assert_eq!(output[1], "Approved 0 pending device(s).");
            assert!(output[2].starts_with("Approve failed: "));
            assert_eq!(
                output[3],
                concat!(
                    "Pairing devices (deny/revoke candidates):\n",
                    "  - none\n",
                    "Revoke one with `/deny <device-id>` or purge pending with `/deny pending`."
                )
            );
            assert_eq!(output[4], "Removed 0 pending device(s).");
            assert!(output[5].starts_with("Deny failed: "));
        },
    ) else {
        return;
    };
    let mut app = test_app().await;

    handled(handle_approve_command(&mut app, &[]));
    handled(handle_approve_command(&mut app, &["ALL"]));
    handled(handle_approve_command(&mut app, &["absent-device"]));
    handled(handle_deny_command(&mut app, &["LIST"]));
    handled(handle_deny_command(&mut app, &["clear-pending"]));
    handled(handle_deny_command(&mut app, &["absent-device"]));
    assert!(PairingStore::open_default()
        .list()
        .expect("list devices")
        .is_empty());
}

#[tokio::test]
async fn mouse_status_validation_and_both_setting_forms_update_consistently() {
    if run_isolated("mouse_status_validation_and_both_setting_forms_update_consistently") {
        return;
    }

    let Some(_home) = isolated_home(
        "mouse_status_validation_and_both_setting_forms_update_consistently",
        &[("HERMES_TUI_MOUSE", "0")],
        |_, output| {
            assert_eq!(
                output,
                &[
                    "Mouse interactions: OFF (use `/mouse on` or `/mouse off`)",
                    "Usage: /mouse [on|off|toggle]",
                    "Usage: /mouse [on|off|toggle]",
                    "Mouse interactions: ON",
                    "Mouse interactions: ON (use `/mouse on` or `/mouse off`)",
                    "Mouse interactions: OFF",
                ]
            );
        },
    ) else {
        return;
    };
    struct RestoreMouseEnv(Option<std::ffi::OsString>);
    impl Drop for RestoreMouseEnv {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("HERMES_TUI_MOUSE", value),
                None => std::env::remove_var("HERMES_TUI_MOUSE"),
            }
        }
    }

    let _restore_mouse = RestoreMouseEnv(std::env::var_os("HERMES_TUI_MOUSE"));
    let mut app = test_app().await;
    app.set_mouse_enabled(false);

    handled(handle_mouse_command(&mut app, &[]));
    for invalid in [&["SET", "invalid"][..], &["invalid"][..]] {
        handled(handle_mouse_command(&mut app, invalid));
        assert!(!app.mouse_enabled());
        assert_eq!(std::env::var("HERMES_TUI_MOUSE").as_deref(), Ok("0"));
    }

    handled(handle_mouse_command(&mut app, &["SeT", " YeS "]));
    assert!(app.mouse_enabled());
    assert_eq!(std::env::var("HERMES_TUI_MOUSE").as_deref(), Ok("1"));
    handled(handle_mouse_command(&mut app, &["STATUS"]));

    handled(handle_mouse_command(&mut app, &["toggle"]));
    assert!(!app.mouse_enabled());
    assert_eq!(std::env::var("HERMES_TUI_MOUSE").as_deref(), Ok("0"));
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path.split_once("::").map_or(path.as_str(), |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([name, "--exact", "--test-threads=1", "--nocapture"])
        .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
        .output()
        .expect("run the isolated test");
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "isolated test {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    true
}
