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
fn elite_sync_gate_aggregates_pass_and_rollback_on_failure() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let report = root.join(".sync-reports/elite.json");

    let ok_cmd = if cfg!(windows) {
        "cmd /C exit 0"
    } else {
        "true"
    };
    let fail_cmd = if cfg!(windows) {
        "cmd /C exit 1"
    } else {
        "false"
    };

    let pass = Command::new(bin())
        .args([
            "run-elite-sync-gate",
            "--repo-root",
            root.to_str().expect("utf8"),
            "--redteam-cmd",
            ok_cmd,
            "--chaos-cmd",
            ok_cmd,
            "--hotpath-cmd",
            ok_cmd,
            "--parity-diff-cmd",
            ok_cmd,
            "--surface-coverage-cmd",
            ok_cmd,
            "--eval-trend-cmd",
            ok_cmd,
            "--release-security-cmd",
            ok_cmd,
            "--performance-autopilot-cmd",
            ok_cmd,
            "--report-path",
            report.to_str().expect("utf8"),
            "--json",
        ])
        .output()
        .expect("run");
    assert_eq!(pass.status.code(), Some(0));
    let p_json: Value = serde_json::from_slice(&pass.stdout).expect("json");
    assert_eq!(p_json["ok"], true);
    assert_eq!(p_json["summary"]["passed_sections"], 8);

    let fail = Command::new(bin())
        .args([
            "run-elite-sync-gate",
            "--repo-root",
            root.to_str().expect("utf8"),
            "--redteam-cmd",
            fail_cmd,
            "--chaos-cmd",
            ok_cmd,
            "--hotpath-cmd",
            ok_cmd,
            "--parity-diff-cmd",
            ok_cmd,
            "--surface-coverage-cmd",
            ok_cmd,
            "--eval-trend-cmd",
            ok_cmd,
            "--release-security-cmd",
            ok_cmd,
            "--performance-autopilot-cmd",
            ok_cmd,
            "--rollback-cmd",
            ok_cmd,
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
fn golden_parity_harness_validates_commands_contracts_and_tui_tests() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let local = tmp.path().join("local");
    let upstream = tmp.path().join("upstream");
    fs::create_dir_all(local.join("crates/hermes-cli/src/commands")).expect("mkdir");
    fs::create_dir_all(upstream.join("hermes_cli")).expect("mkdir");

    fs::write(
        upstream.join("hermes_cli/commands.py"),
        r#"
CommandDef("model", "Model selection")
CommandDef("raw", "Raw determinism")
CommandDef("policy", "Policy rules")
CommandDef("objective", "Objective management")
"#,
    )
    .expect("write upstream");
    fs::write(
        local.join("crates/hermes-cli/src/commands/mod.rs"),
        r#"
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/model", "Model capability explain why-not"),
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
fn test_find_anchor_line_index_prefers_near_expected_window() {}
fn test_submit_shortcuts_are_detected() {}
fn test_stream_handle() {}
fn test_is_ctrl_c_detection() {}
"#,
    )
    .expect("write tui");

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
            "0",
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
    assert_eq!(rpt["missing_tui_tests"], serde_json::json!([]));
}
