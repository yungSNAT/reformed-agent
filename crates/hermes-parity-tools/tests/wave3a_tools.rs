use std::fs;
use std::io::Write as _;
use std::process::{Command, Stdio};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_hermes-parity-tools")
}

#[test]
fn publish_automation_summary_writes_local_fallback_from_stdin() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let local_log = root.join(".sync-reports/custom.log");

    let mut child = Command::new(bin())
        .args([
            "publish-automation-summary",
            "--repo-root",
            root.to_str().expect("utf8 root"),
            "--sink-order",
            "contextlattice,github,local",
            "--contextlattice-url",
            "http://127.0.0.1:9",
            "--context-timeout-secs",
            "0.5",
            "--github-issue",
            "0",
            "--local-path",
            local_log.to_str().expect("utf8 log"),
            "--preformatted-markdown",
            "-",
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"## Test Heading\n\nBody line.\n")
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["primary_sink"], "local");
    let logged = fs::read_to_string(&local_log).expect("read log");
    assert!(logged.contains("## Test Heading"));
    assert!(logged.contains("Body line."));
}

#[test]
fn publish_automation_summary_keeps_empty_env_defaults_empty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let out = Command::new(bin())
        .args([
            "publish-automation-summary",
            "--repo-root",
            root.to_str().expect("utf8 root"),
            "--json",
        ])
        .env("AUTOMATION_SUMMARY_SINK_ORDER", "")
        .env("AUTOMATION_SUMMARY_CONTEXT_TOPIC_PATH", "")
        .env("AUTOMATION_SUMMARY_KIND", "")
        .output()
        .expect("run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("json");
    // An empty sink order falls back to the local sink alone.
    assert_eq!(parsed["attempts"].as_array().map(Vec::len), Some(1));
    assert_eq!(parsed["primary_sink"], "local");
    assert_eq!(parsed["context"]["topic_path"], "");
    assert_eq!(parsed["metadata"]["summary_kind"], "");
}

#[test]
fn publish_automation_summary_handles_stdout_json_hyphen_flags_and_py_metadata() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let stdout_json = Command::new(bin())
        .args([
            "publish-automation-summary",
            "--repo-root",
            root.to_str().expect("utf8"),
            "--sink-order",
            "stdout",
            "--github-issue",
            "-1",
            "--title",
            "- Hyphen Heading",
            "--summary-body",
            "- Hyphen body line.",
            "--metadata-json",
            r#"{"status": true, "flag": true, "big": 18446744073709551615, "arr": [1, "a"]}"#,
            "--json",
        ])
        .output()
        .expect("stdout json");
    assert_eq!(
        stdout_json.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&stdout_json.stderr)
    );
    let parsed: Value = serde_json::from_slice(&stdout_json.stdout).expect("pure valid json");
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["primary_sink"], "stdout");
    assert_eq!(parsed["metadata"]["status"], true);
    assert!(String::from_utf8_lossy(&stdout_json.stderr).contains("## - Hyphen Heading"));

    let log_file = root.join("rel-meta.log");
    let local_out = Command::new(bin())
        .current_dir(root)
        .args([
            "publish-automation-summary",
            "--repo-root",
            ".",
            "--sink-order",
            "local",
            "--local-path",
            "rel-meta.log",
            "--title",
            "- Hyphen Heading",
            "--summary-body",
            "- Body",
            "--metadata-json",
            r#"{"status": true, "flag": true, "big": 18446744073709551615, "arr": [1, "a"]}"#,
            "--json",
        ])
        .output()
        .expect("local meta");
    assert_eq!(local_out.status.code(), Some(0));
    let local_json: Value = serde_json::from_slice(&local_out.stdout).expect("json");
    assert_eq!(local_json["local_path"], "rel-meta.log");
    assert_eq!(
        local_json["context"]["project"],
        root.file_name()
            .and_then(|s| s.to_str())
            .expect("root name")
    );
    let text = fs::read_to_string(&log_file).expect("read");
    assert!(text.contains("- status: `True`"));
    assert!(text.contains(r#"- metadata: `{"agent_id": "hermes_ultra_automation", "arr": [1, "a"], "big": 18446744073709551615, "flag": true}`"#));

    let help = Command::new(bin())
        .env("CONTEXTLATTICE_API_KEY", "super-secret-env-token-12345")
        .args(["publish-automation-summary", "--help"])
        .output()
        .expect("help");
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(!help_text.contains("super-secret-env-token-12345"));
}

#[test]
fn elite_sync_gate_aggregates_pass_and_rollback_on_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let report = root.join(".sync-reports/elite.json");

    let pass = Command::new(bin())
        .args([
            "run-elite-sync-gate",
            "--repo-root",
            root.to_str().expect("utf8"),
            "--redteam-cmd",
            "true",
            "--chaos-cmd",
            "true",
            "--hotpath-cmd",
            "true",
            "--parity-diff-cmd",
            "true",
            "--surface-coverage-cmd",
            "true",
            "--eval-trend-cmd",
            "true",
            "--release-security-cmd",
            "true",
            "--performance-autopilot-cmd",
            "true",
            "--rollback-cmd",
            "true",
            "--report-path",
            report.to_str().expect("utf8"),
            "--json",
        ])
        .output()
        .expect("run");
    assert_eq!(
        pass.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&pass.stderr)
    );
    let p_json: Value = serde_json::from_slice(&pass.stdout).expect("json");
    assert_eq!(p_json["ok"], true);
    assert_eq!(p_json["summary"]["passed_sections"], 8);

    let fail = Command::new(bin())
        .args([
            "run-elite-sync-gate",
            "--repo-root",
            root.to_str().expect("utf8"),
            "--redteam-cmd",
            "false",
            "--chaos-cmd",
            "true",
            "--hotpath-cmd",
            "true",
            "--parity-diff-cmd",
            "true",
            "--surface-coverage-cmd",
            "true",
            "--eval-trend-cmd",
            "true",
            "--release-security-cmd",
            "true",
            "--performance-autopilot-cmd",
            "true",
            "--rollback-cmd",
            "true",
            "--report-path",
            report.to_str().expect("utf8"),
            "--json",
        ])
        .output()
        .expect("run");
    assert_eq!(fail.status.code(), Some(1));
    let f_json: Value = serde_json::from_slice(&fail.stdout).expect("json");
    assert_eq!(f_json["ok"], false);
    assert_eq!(f_json["summary"]["failed_sections"], 1);
    assert_eq!(f_json["rollback"]["ok"], true);
}

#[test]
fn golden_parity_harness_scans_tui_dir_aliases_and_negative_allow_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let local = tmp.path().join("local");
    let upstream = tmp.path().join("upstream");
    fs::create_dir_all(local.join("crates/hermes-cli/src/commands")).expect("mkdir");
    fs::create_dir_all(local.join("crates/hermes-cli/src/tui")).expect("mkdir tui");
    fs::create_dir_all(upstream.join("hermes_cli")).expect("mkdir");

    fs::write(
        upstream.join("hermes_cli/commands.py"),
        r#"
"""
Docstring escape check: \é
CommandDef("ignored_docstring_cmd", "not real")
"/ignored-dict": "ignored"
"""
# CommandDef("ignored_comment_cmd", "commented out")
# '/ignored-comment-key': "commented out"
COMMAND_REGISTRY: list[CommandDef] = [
    CommandDef('model', "Switch model (provider)", "Configuration",
               aliases=('m', 'mod')),
    CommandDef("raw", "Raw output", "Info"),
    CommandDef("policy", "Policy rules", "Info", cli_only=True),
    CommandDef("sethome", "Set the home channel", "Session",
               gateway_only=True, aliases=("set-home",)),
]
SUBCOMMANDS = {"/raw": ["trace"]}
"#,
    )
    .expect("write upstream");
    fs::write(
        local.join("crates/hermes-cli/src/commands/mod.rs"),
        r#"
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/model", "Model capability explain why-not"),
    ("/m", "Alias m"),
    ("/mod", "Alias mod"),
    ("/raw", "Raw trace deterministic verify export"),
    ("/policy", "Policy profile strict"),
    ("/objective", "Objective contract profile ledger"),
];
"#,
    )
    .expect("write local cmds");
    fs::write(
        local.join("crates/hermes-cli/src/tui.rs"),
        r#"
fn test_completion_popup_hidden_when_slash_deleted() {}
fn test_completion_popup_hidden_when_modal_or_processing_active() {}
fn test_transcript_hides_system_messages() {}
fn test_transcript_wrap_width_caps_at_80() {}
"#,
    )
    .expect("write tui.rs");
    fs::write(
        local.join("crates/hermes-cli/src/tui/extra.rs"),
        r#"
fn test_find_anchor_line_index_prefers_near_expected_window() {}
fn test_submit_shortcuts_are_detected() {}
fn test_stream_handle() {}
fn test_is_ctrl_c_detection() {}
"#,
    )
    .expect("write tui/extra.rs");

    let report = local.join(".sync-reports/golden.json");
    let out = Command::new(bin())
        .args([
            "run-golden-parity-harness",
            "--repo-root",
            local.to_str().expect("utf8"),
            "--upstream-root",
            upstream.to_str().expect("utf8"),
            "--report",
            report.to_str().expect("utf8"),
            "--allow-missing",
            "-1",
        ])
        .output()
        .expect("run golden");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rpt: Value = serde_json::from_slice(&fs::read(&report).expect("read")).expect("json");
    assert_eq!(rpt["ok"], true);
    assert_eq!(rpt["missing_commands"], serde_json::json!([]));
    // Gateway-only upstream commands are outside the runtime COMMANDS table.
    assert_eq!(rpt["extra_commands"], serde_json::json!(["/objective"]));
    assert_eq!(rpt["missing_tui_tests"], serde_json::json!([]));
}
