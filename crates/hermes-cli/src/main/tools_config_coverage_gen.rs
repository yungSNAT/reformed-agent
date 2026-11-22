use super::{
    computer_use_install_hint, render_computer_use_health_report, run_computer_use, run_config,
    run_tools, AgentError, Cli,
};
use serde_json::{json, Value};
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

const REQUEST: &str = "HERMES_TOOLS_CONFIG_COVERAGE_REQUEST";
const OK: &str = "coverage command result: ok";

fn invoke(root: &Path, request: &[&str]) -> String {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();

    // Use a separate test process so the handlers see an isolated home, PATH,
    // and editor without changing the environment of other tests.
    let (_, test_name) = concat!(module_path!(), "::coverage_command_child")
        .split_once("::")
        .unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test_name, "--ignored", "--nocapture"])
        .current_dir(root)
        .stdin(Stdio::null())
        .env_clear()
        .env(REQUEST, serde_json::to_string(request).unwrap())
        .env("HERMES_HOME", root)
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("APPDATA", root)
        .env("LOCALAPPDATA", root)
        .env("XDG_CONFIG_HOME", root)
        .env("XDG_CACHE_HOME", root)
        .env("XDG_DATA_HOME", root)
        .env("XDG_STATE_HOME", root)
        .env("PATH", bin)
        .env("EDITOR", root.join("fixture-editor"));
    for key in [
        "LLVM_PROFILE_FILE",
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "SystemRoot",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }

    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "request {request:?} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("coverage command result: "),
        "request {request:?} did not run:\n{stdout}"
    );
    stdout
}

fn invoke_ok(root: &Path, request: &[&str]) -> String {
    let stdout = invoke(root, request);
    assert_line(&stdout, OK);
    stdout
}

fn assert_line(stdout: &str, expected: &str) {
    assert!(
        stdout.lines().any(|line| line == expected),
        "missing line {expected:?} in:\n{stdout}"
    );
}

fn printed_json(stdout: &str) -> Value {
    let start = stdout
        .find('{')
        .expect("command did not print a JSON object");
    serde_json::Deserializer::from_str(&stdout[start..])
        .into_iter::<Value>()
        .next()
        .unwrap()
        .unwrap()
}

#[tokio::test]
#[ignore = "subprocess entry point for tools/config coverage tests"]
async fn coverage_command_child() {
    let Ok(request) = std::env::var(REQUEST) else {
        return;
    };
    let request: Vec<String> = serde_json::from_str(&request).unwrap();
    let (command, args) = request.split_first().unwrap();
    let arg = |index: usize| args.get(index).filter(|text| !text.is_empty()).cloned();
    let flag = |index: usize| args.get(index).is_some_and(|text| text == "true");
    let cli = || {
        let root = std::env::var_os("HERMES_HOME").unwrap();
        <Cli as clap::Parser>::try_parse_from([
            OsStr::new("hermes"),
            OsStr::new("--config-dir"),
            root.as_os_str(),
        ])
        .unwrap()
    };

    println!("coverage command output:");
    let result = match command.as_str() {
        "config" => run_config(cli(), arg(0), arg(1), arg(2)),
        "tools" => run_tools(cli(), arg(0), arg(1), arg(2), flag(3)).await,
        "computer-use" => {
            run_computer_use(
                arg(0),
                flag(1),
                arg(2).into_iter().collect(),
                arg(3).into_iter().collect(),
            )
            .await
        }
        _ => panic!("unknown coverage request: {request:?}"),
    };
    match result {
        Ok(()) => println!("{OK}"),
        Err(AgentError::Config(message)) => println!("coverage command result: config: {message}"),
        Err(AgentError::ToolExecution(message)) => {
            println!("coverage command result: tool execution: {message}");
        }
        Err(error) => println!("coverage command result: unexpected error: {error}"),
    }
}

#[test]
fn tools_summary_and_toggles_reflect_persisted_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    let initial = invoke_ok(root, &["tools", "", "", "", "true"]);
    assert_line(&initial, "Tool summary (platform=cli):");
    assert_line(&initial, "  enabled: (none)");
    assert_line(&initial, "  disabled: (none)");

    let enabled = invoke_ok(root, &["tools", "enable", "terminal", "fixture"]);
    assert_line(&enabled, "Enabled tool 'terminal' for platform 'fixture'.");
    invoke_ok(root, &["tools", "enable", "terminal"]);

    let summary = invoke_ok(root, &["tools", "list", "", "fixture", "true"]);
    assert_line(&summary, "Tool summary (platform=fixture):");
    assert_line(&summary, "  enabled: terminal");
    assert_line(&summary, "  disabled: (none)");

    let disabled = invoke_ok(root, &["tools", "disable", "terminal"]);
    assert_line(&disabled, "Disabled tool 'terminal' for platform 'cli'.");
    invoke_ok(root, &["tools", "disable", "terminal", "fixture"]);

    let summary = invoke_ok(root, &["tools", "list", "", "", "true"]);
    assert_line(&summary, "  enabled: (none)");
    assert_line(&summary, "  disabled: terminal");

    let listing = invoke_ok(root, &["tools", "list", "", "fixture"]);
    assert!(listing.contains("• terminal [disabled]"), "{listing}");
    assert_line(&listing, "Scope: fixture");

    invoke_ok(root, &["tools", "enable", "terminal"]);
    let summary = invoke_ok(root, &["tools", "list", "", "", "true"]);
    assert_line(&summary, "  enabled: terminal");
    assert_line(&summary, "  disabled: (none)");
}

#[test]
fn tools_invalid_actions_do_not_modify_config() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    invoke_ok(root, &["tools", "enable", "terminal"]);
    let path = root.join("config.yaml");
    let original = fs::read(&path).unwrap();

    for action in ["enable", "disable"] {
        let stdout = invoke(root, &["tools", action]);
        assert_line(
            &stdout,
            &format!(
                "coverage command result: config: tools {action}: usage `hermes tools {action} <name>`"
            ),
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    let stdout = invoke_ok(root, &["tools", "not-an-action"]);
    assert_line(
        &stdout,
        "Unknown tools action: not-an-action. Use 'list', 'enable', or 'disable'.",
    );
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn config_set_get_show_and_path_report_the_same_file() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let path = root.join("config.yaml");
    let model = "openai:gpt-5.5";

    let saved = invoke_ok(root, &["config", "set", "model", model]);
    assert_line(
        &saved,
        &format!("Saved model = {model} -> {}", path.display()),
    );
    assert!(path.is_file());

    let field = invoke_ok(root, &["config", "get", "model"]);
    assert!(field.contains(model), "{field}");

    let shown = printed_json(&invoke_ok(root, &["config", "show"]));
    assert_eq!(shown["model"], json!(model));
    assert_eq!(printed_json(&invoke_ok(root, &["config"])), shown);

    let reported_path = invoke_ok(root, &["config", "path"]);
    assert_line(&reported_path, &path.display().to_string());
}

#[test]
fn config_reports_usage_errors_and_unknown_names() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    for (request, diagnostic) in [
        (
            &["config", "get"][..],
            "Missing key. Usage: hermes config get <key>",
        ),
        (
            &["config", "set"][..],
            "Missing key. Usage: hermes config set <key> <value>",
        ),
        (
            &["config", "set", "model"][..],
            "Missing value. Usage: hermes config set <key> <value>",
        ),
    ] {
        assert_line(
            &invoke(root, request),
            &format!("coverage command result: config: {diagnostic}"),
        );
    }

    let stdout = invoke_ok(root, &["config", "get", "coverage_unknown_key"]);
    assert_line(&stdout, "Unknown config key: coverage_unknown_key");
    let stdout = invoke_ok(root, &["config", "coverage-unknown-action"]);
    assert_line(&stdout, "Unknown config action: 'coverage-unknown-action'.");
    assert_line(
        &stdout,
        "Available: show, get, set, path, env-path, check, edit, migrate",
    );
}

#[test]
fn config_env_path_reports_presence_of_the_dotenv_file() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let path = root.join(".env");
    let missing = invoke_ok(root, &["config", "env-path"]);
    assert_line(&missing, &path.display().to_string());
    assert_line(
        &missing,
        "(not found — create it to set environment overrides)",
    );

    let contents = "# test fixture\n";
    fs::write(&path, contents).unwrap();
    let present = invoke_ok(root, &["config", "env-path"]);
    assert_line(&present, &path.display().to_string());
    assert_line(&present, "(exists)");
    assert_eq!(fs::read_to_string(path).unwrap(), contents);
}

#[test]
fn config_migration_preserves_json_and_does_not_overwrite_yaml() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let old = root.join("config.json");
    let new = root.join("config.yaml");

    let absent = invoke_ok(root, &["config", "migrate"]);
    assert_line(&absent, "No legacy config.json found. Nothing to migrate.");
    assert!(!new.exists());

    let legacy = json!({"model": "openai:gpt-5.5"});
    let contents = legacy.to_string();
    fs::write(&old, &contents).unwrap();
    let migrated = invoke_ok(root, &["config", "migrate"]);
    assert_line(&migrated, "Migrated config.json -> config.yaml");
    assert_line(&migrated, "The old config.json was preserved.");
    assert_eq!(fs::read_to_string(&old).unwrap(), contents);
    let yaml = fs::read_to_string(&new).unwrap();
    assert_eq!(serde_yaml::from_str::<Value>(&yaml).unwrap(), legacy);

    fs::write(&old, r#"{"model":"anthropic:claude-sonnet-4-6"}"#).unwrap();
    let repeated = invoke_ok(root, &["config", "migrate"]);
    assert_line(
        &repeated,
        "config.yaml already exists. No migration needed.",
    );
    assert_eq!(fs::read_to_string(new).unwrap(), yaml);
}

#[test]
fn config_migration_reports_bad_json_and_unreadable_input() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let old = root.join("config.json");
    let new = root.join("config.yaml");

    fs::write(&old, "{ invalid json").unwrap();
    let invalid = invoke_ok(root, &["config", "migrate"]);
    assert_line(&invalid, "Could not parse config.json as JSON.");
    assert_eq!(fs::read_to_string(&old).unwrap(), "{ invalid json");
    assert!(!new.exists());

    fs::remove_file(&old).unwrap();
    fs::create_dir(&old).unwrap();
    let unreadable = invoke_ok(root, &["config", "migrate"]);
    assert!(
        unreadable.contains("Could not read config.json:"),
        "{unreadable}"
    );
    assert!(old.is_dir());
    assert!(!new.exists());
}

#[test]
fn config_edit_reports_a_missing_editor_without_creating_config() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let stdout = invoke_ok(root, &["config", "edit"]);
    let editor = root.join("fixture-editor");
    assert!(
        stdout.contains(&format!("Could not launch editor '{}':", editor.display())),
        "{stdout}"
    );
    assert!(!root.join("config.yaml").exists());
}

#[cfg(unix)]
fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[test]
fn config_edit_passes_the_path_to_the_editor_and_reports_its_status() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let editor = root.join("fixture-editor");
    let path = root.join("config.yaml");
    write_script(&editor, "printf '%s\\n' 'model: openai:gpt-5.5' > \"$1\"");
    let saved = invoke_ok(root, &["config", "edit"]);
    assert_line(&saved, "Config saved.");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "model: openai:gpt-5.5\n"
    );

    write_script(&editor, "false");
    let failed = invoke_ok(root, &["config", "edit"]);
    assert!(failed.contains("Editor exited with:"), "{failed}");
    assert!(!failed.contains("Config saved."), "{failed}");
    assert_eq!(fs::read_to_string(path).unwrap(), "model: openai:gpt-5.5\n");
}

#[test]
fn computer_use_reports_an_unavailable_driver_in_both_formats() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    let status = invoke_ok(root, &["computer-use", "", "true"]);
    let status = printed_json(&status);
    assert_eq!(status["available"], json!(false));
    let command = status["driver_command"].as_str().unwrap();
    assert!(!command.is_empty());

    let doctor = invoke_ok(root, &["computer-use", "doctor", "true"]);
    assert_eq!(
        printed_json(&doctor),
        json!({"ok": false, "error": "cua-driver unavailable", "driver_command": command})
    );

    for action in ["status", "doctor"] {
        let stdout = invoke_ok(root, &["computer-use", action]);
        assert_line(
            &stdout,
            &format!("cua-driver: not installed or not on PATH (looked for {command})"),
        );
        assert!(stdout.contains(computer_use_install_hint()), "{stdout}");
    }
}

#[test]
fn computer_use_install_aliases_print_platform_hint_and_unknown_actions_fail() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let hint = computer_use_install_hint();
    assert!(hint.starts_with("Install cua-driver with:\n"));
    assert!(hint.ends_with("Then run:\n  hermes computer-use doctor"));
    let script = if cfg!(windows) {
        "install.ps1"
    } else {
        "install.sh"
    };
    assert!(hint.contains(script));

    for action in ["install", "install-hint"] {
        let stdout = invoke_ok(root, &["computer-use", action]);
        assert!(stdout.contains(hint), "{stdout}");
    }

    let stdout = invoke(root, &["computer-use", "coverage-unknown"]);
    assert_line(
        &stdout,
        "coverage command result: config: Unknown computer-use action 'coverage-unknown'. Use status, doctor, manifest, or install-hint.",
    );
}

#[test]
fn computer_use_manifest_prints_consistent_json_and_text() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let status = printed_json(&invoke_ok(root, &["computer-use", "status", "true"]));
    let manifest = printed_json(&invoke_ok(root, &["computer-use", "manifest", "true"]));
    assert_eq!(manifest["driver_command"], status["driver_command"]);
    let driver = manifest["driver_command"].as_str().unwrap();
    let command = manifest["mcp_command"].as_str().unwrap();
    assert!(!command.is_empty());
    let args: Vec<&str> = manifest["mcp_args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    let text = invoke_ok(root, &["computer-use", "manifest"]);
    assert_line(&text, &format!("driver command: {driver}"));
    assert_line(
        &text,
        &format!("MCP invocation: {command} {}", args.join(" ")),
    );
}

#[cfg(unix)]
#[test]
fn computer_use_status_finds_a_local_fixture_and_doctor_reports_its_failure() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    write_script(
        &bin.join("cua-driver"),
        r#"case "${1-}" in
    --version) printf '%s\n' 'cua-driver 1.0.0' ;;
    --help) printf '%s\n' 'Usage: cua-driver mcp' ;;
esac"#,
    );

    let json_status = invoke_ok(root, &["computer-use", "status", "true"]);
    let status = printed_json(&json_status);
    assert_eq!(status["available"], json!(true));
    let command = status["driver_command"].as_str().unwrap();

    let text = invoke_ok(root, &["computer-use", "status"]);
    assert_line(&text, &format!("cua-driver: available ({command})"));
    assert_line(&text, "  doctor: hermes computer-use doctor");

    let failed = invoke(
        root,
        &["computer-use", "doctor", "true", "display", "accessibility"],
    );
    assert!(
        failed.contains("coverage command result: tool execution: "),
        "{failed}"
    );
}

#[test]
fn computer_use_report_prefers_camel_case_structured_checks() {
    let report = json!({
        "structuredContent": {
            "overall": "degraded",
            "platform": "fixture-platform",
            "driver_version": "2.1",
            "checks": [
                {"name": "display", "status": "warning", "message": "not ready", "hint": "retry"},
                {"name": "input", "status": "ok", "message": ""},
                {"name": 42, "hint": false}
            ]
        },
        "structured_content": {"overall": "must not win"},
        "content": [{"text": "must not appear"}]
    });
    assert_eq!(
        render_computer_use_health_report(&report),
        "cua-driver 2.1 on fixture-platform: degraded\n\
           \x20 - display: warning - not ready\n\
           \x20   hint: retry\n\
           \x20 - input: ok\n\
           \x20 - check: unknown"
    );
}

#[test]
fn computer_use_report_falls_back_to_text_and_handles_unwrapped_reports() {
    let wrapped = json!({
        "structured_content": {"platform": "fixture-platform", "overall": false, "driver_version": []},
        "content": [
            {"text": "first line"},
            {"text": "second line\n"},
            {"text": 7},
            {"text": "last line  \n\n"}
        ]
    });
    assert_eq!(
        render_computer_use_health_report(&wrapped),
        "cua-driver unknown-version on fixture-platform: unknown\nfirst line\nsecond line\nlast line"
    );

    let unwrapped = json!({
        "overall": "ok",
        "checks": [],
        "content": [{"text": "ignored because checks were supplied"}]
    });
    assert_eq!(
        render_computer_use_health_report(&unwrapped),
        "cua-driver unknown-version on unknown-platform: ok"
    );
}
