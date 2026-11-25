//! Golden parity harness for CLI command surface and key TUI flow contracts.
//! Port of `scripts/run-golden-parity-harness.py`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;
use regex::Regex;

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
use crate::script_helpers::{
    default_sync_report_path, read_rust_surface, resolve_repo_root, write_json_pretty,
};
use crate::slash_gate::extract_upstream_slash_commands;

const TITLE_PREFIX: &str = "[Parity] Golden Harness Drift";

pub const REQUIRED_TUI_TESTS: &[&str] = &[
    "test_completion_popup_hidden_when_slash_deleted",
    "test_completion_popup_hidden_when_modal_or_processing_active",
    "test_transcript_hides_system_messages",
    "test_transcript_wrap_width_caps_at_80",
    "test_find_anchor_line_index_prefers_near_expected_window",
    "test_submit_shortcuts_are_detected",
    "test_stream_handle",
    "test_is_ctrl_c_detection",
];

const REQUIRED_COMMAND_CONTRACTS: &[(&str, &[&str])] = &[
    ("/model", &["capability", "explain", "why-not"]),
    ("/raw", &["trace", "deterministic", "verify", "export"]),
    ("/policy", &["profile", "strict"]),
    ("/objective", &["contract", "profile", "ledger"]),
];

#[derive(Args, Debug)]
pub struct GoldenParityArgs {
    /// Hermes Ultra repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Path to upstream NousResearch hermes-agent checkout
    #[arg(long, default_value = "../hermes-agent")]
    upstream_root: String,
    /// Optional report output path
    #[arg(long, default_value = "")]
    report: String,
    /// Maximum allowed missing upstream commands before failing
    #[arg(long, default_value_t = -1)]
    allow_missing: i64,
    /// Optional baseline JSON with max_missing_commands field
    #[arg(long, default_value = "docs/parity/golden-harness-baseline.json")]
    baseline: String,
    /// Auto-open/update a parity drift issue when failures are detected
    #[arg(long)]
    auto_issue: bool,
}

pub fn parse_local_slash_commands(commands_path: &Path) -> Result<BTreeSet<String>, String> {
    let raw = read_rust_surface(commands_path)?;
    let re = Regex::new(r#"\(\s*"(/[^"]+)"\s*,"#).expect("valid regex");
    Ok(re
        .captures_iter(&raw)
        .map(|c| c[1].trim().to_string())
        .filter(|s| s.starts_with('/'))
        .collect())
}

pub fn parse_local_command_descriptions(
    commands_path: &Path,
) -> Result<BTreeMap<String, String>, String> {
    let raw = read_rust_surface(commands_path)?;
    let re = Regex::new(r#"\(\s*"(/[^"]+)"\s*,\s*"([^"]*)"\s*,?\s*\)"#).expect("valid regex");
    Ok(re
        .captures_iter(&raw)
        .map(|c| (c[1].trim().to_string(), c[2].trim().to_string()))
        .filter(|(cmd, _)| cmd.starts_with('/'))
        .collect())
}

pub fn parse_upstream_commands(upstream_root: &Path) -> Result<BTreeSet<String>, String> {
    let commands_py = upstream_root.join("hermes_cli/commands.py");
    let raw = fs::read_to_string(&commands_py)
        .map_err(|e| format!("failed to read upstream {}: {e}", commands_py.display()))?;
    let mut cmds = extract_upstream_slash_commands(&raw);
    let dict_re = Regex::new(r#""(/[a-zA-Z0-9_-]+)"\s*:"#).expect("valid regex");
    for cap in dict_re.captures_iter(&raw) {
        cmds.insert(cap[1].trim().to_string());
    }
    if cmds.is_empty() {
        return Err(format!(
            "failed to extract any COMMANDS from {}",
            commands_py.display()
        ));
    }
    Ok(cmds)
}

const TUI_FN_PATTERN: &str = r"fn\s+([a-zA-Z0-9_]+)\s*\(";

pub fn parse_tui_tests(tui_rs: &Path) -> Result<BTreeSet<String>, String> {
    let raw = read_rust_surface(tui_rs)?;
    let re = Regex::new(TUI_FN_PATTERN).expect("valid regex");
    Ok(re.captures_iter(&raw).map(|c| c[1].to_string()).collect())
}

pub fn load_allow_missing(allow_missing: i64, baseline: &str, repo_root: &Path) -> i64 {
    if allow_missing >= 0 {
        return allow_missing;
    }
    let bpath = Path::new(baseline);
    let full = if bpath.is_absolute() {
        bpath.to_path_buf()
    } else {
        repo_root.join(bpath)
    };
    let Ok(raw) = fs::read_to_string(&full) else {
        return 0;
    };
    let Ok(parsed) = py::loads(&raw) else {
        return 0;
    };
    match parsed.get("max_missing_commands") {
        Some(Py::Int(n)) if *n >= 0 => *n as i64,
        _ => 0,
    }
}

fn check_command_contracts(local_desc: &BTreeMap<String, String>) -> Vec<String> {
    let mut failures = Vec::new();
    for (command, required_terms) in REQUIRED_COMMAND_CONTRACTS {
        let desc_lc = local_desc
            .get(*command)
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let missing: Vec<&str> = required_terms
            .iter()
            .copied()
            .filter(|t| !desc_lc.contains(&t.to_ascii_lowercase()))
            .collect();
        if !missing.is_empty() {
            let quoted: Vec<String> = missing.iter().map(|m| format!("'{m}'")).collect();
            failures.push(format!(
                "{command}: description missing required terms [{}]",
                quoted.join(", ")
            ));
        }
    }
    failures
}

fn run_gh(repo_root: &Path, args: &[&str]) -> Option<std::process::Output> {
    Command::new("gh")
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()
}

fn append_bullet_section(lines: &mut Vec<String>, header: &str, items: &[String], backtick: bool) {
    if items.is_empty() {
        return;
    }
    lines.push(header.to_string());
    lines.extend(items.iter().take(50).map(|item| {
        if backtick {
            format!("- `{item}`")
        } else {
            format!("- {item}")
        }
    }));
    lines.push(String::new());
}

fn build_issue_body(
    missing: &[String],
    missing_tui: &[String],
    contract_failures: &[String],
) -> String {
    let mut lines = vec![
        "Golden parity harness detected drift.".to_string(),
        String::new(),
        format!("- Missing upstream commands: `{}`", missing.len()),
        format!("- Missing required TUI tests: `{}`", missing_tui.len()),
        format!("- Command contract failures: `{}`", contract_failures.len()),
        String::new(),
    ];
    append_bullet_section(&mut lines, "Missing commands:", missing, true);
    append_bullet_section(&mut lines, "Missing TUI tests:", missing_tui, true);
    append_bullet_section(
        &mut lines,
        "Command contract failures:",
        contract_failures,
        false,
    );
    lines.join("\n").trim().to_string()
}

fn find_existing_issue(repo_root: &Path) -> Option<String> {
    let search = format!("{TITLE_PREFIX} in:title");
    let list_out = run_gh(
        repo_root,
        &[
            "issue",
            "list",
            "--state",
            "open",
            "--search",
            &search,
            "--json",
            "number,title",
            "--limit",
            "1",
        ],
    )?;
    if !list_out.status.success() {
        return None;
    }
    let Py::List(rows) = py::loads(&String::from_utf8_lossy(&list_out.stdout)).ok()? else {
        return None;
    };
    rows.first().and_then(|r| r.get("number")).map(py::str_of)
}

fn maybe_open_issue(
    repo_root: &Path,
    missing: &[String],
    missing_tui: &[String],
    contract_failures: &[String],
) {
    if !run_gh(repo_root, &["--version"]).is_some_and(|v| v.status.success()) {
        return;
    }
    let body = build_issue_body(missing, missing_tui, contract_failures);
    if let Some(num) = find_existing_issue(repo_root) {
        let _ = run_gh(repo_root, &["issue", "comment", &num, "--body", &body]);
        return;
    }
    let date: String = py::utc_isoformat().chars().take(10).collect();
    let title = format!("{TITLE_PREFIX} ({date})");
    let created = run_gh(
        repo_root,
        &[
            "issue", "create", "--title", &title, "--body", &body, "--label", "parity",
        ],
    );
    if !created.is_some_and(|o| o.status.success()) {
        let _ = run_gh(
            repo_root,
            &["issue", "create", "--title", &title, "--body", &body],
        );
    }
}

fn resolve_golden_roots(args: &GoldenParityArgs) -> Result<(PathBuf, PathBuf), String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    let up_raw = py::expanduser(&args.upstream_root)?;
    let up_joined = if up_raw.is_absolute() {
        up_raw
    } else {
        repo_root.join(up_raw)
    };
    let upstream_root = resolve_argument(&up_joined)?;
    if !repo_root.exists() {
        return Err(format!(
            "repo root does not exist: {}",
            py::path_str(&repo_root)
        ));
    }
    if !upstream_root.exists() {
        return Err(format!(
            "upstream root does not exist: {}",
            py::path_str(&upstream_root)
        ));
    }
    Ok((repo_root, upstream_root))
}

struct ParitySummary {
    missing: Vec<String>,
    extra_count: usize,
    missing_tui: Vec<String>,
    contract_failures: Vec<String>,
    ok: bool,
    report: Py,
}

fn collect_parity_summary(
    repo_root: &Path,
    upstream_root: &Path,
    allow_missing: i64,
) -> Result<ParitySummary, String> {
    let commands_dir = repo_root.join("crates/hermes-cli/src/commands");
    let local = parse_local_slash_commands(&commands_dir)?;
    let local_desc = parse_local_command_descriptions(&commands_dir)?;
    let upstream = parse_upstream_commands(upstream_root)?;
    let missing: Vec<String> = upstream.difference(&local).cloned().collect();
    let extra: Vec<String> = local.difference(&upstream).cloned().collect();
    let extra_count = extra.len();
    let contract_failures = check_command_contracts(&local_desc);
    let tui_tests = parse_tui_tests(&repo_root.join("crates/hermes-cli/src/tui.rs"))?;
    let missing_tui: Vec<String> = REQUIRED_TUI_TESTS
        .iter()
        .filter(|t| !tui_tests.contains(**t))
        .map(|s| (*s).to_string())
        .collect();
    let ok = (missing.len() as i64) <= allow_missing
        && missing_tui.is_empty()
        && contract_failures.is_empty();
    let report = Py::dict([
        ("generated_at", Py::from(py::utc_isoformat())),
        ("repo_root", Py::from(py::path_str(repo_root))),
        ("upstream_root", Py::from(py::path_str(upstream_root))),
        ("missing_commands", Py::strs(&missing)),
        ("extra_commands", Py::strs(&extra)),
        ("missing_tui_tests", Py::strs(&missing_tui)),
        ("command_contract_failures", Py::strs(&contract_failures)),
        ("allow_missing_commands", Py::Int(i128::from(allow_missing))),
        ("ok", Py::from(ok)),
    ]);
    Ok(ParitySummary {
        missing,
        extra_count,
        missing_tui,
        contract_failures,
        ok,
        report,
    })
}

pub fn run(args: GoldenParityArgs) -> Result<i32, String> {
    let (repo_root, upstream_root) = resolve_golden_roots(&args)?;
    let allow_missing = load_allow_missing(args.allow_missing, &args.baseline, &repo_root);
    let summary = collect_parity_summary(&repo_root, &upstream_root, allow_missing)?;
    let report_path: PathBuf = if args.report.is_empty() {
        default_sync_report_path(&repo_root, "golden-parity-harness")
    } else {
        resolve_argument(&py::expanduser(&args.report)?)?
    };
    write_json_pretty(&report_path, &summary.report, false)?;
    println!(
        "[golden-parity] {} (missing={} extra={} missing_tui_tests={} contract_failures={} allow_missing={})",
        if summary.ok { "PASSED" } else { "FAILED" },
        summary.missing.len(),
        summary.extra_count,
        summary.missing_tui.len(),
        summary.contract_failures.len(),
        allow_missing
    );
    println!("[golden-parity] Report: {}", py::path_str(&report_path));
    if args.auto_issue && !summary.ok {
        maybe_open_issue(
            &repo_root,
            &summary.missing,
            &summary.missing_tui,
            &summary.contract_failures,
        );
    }
    Ok(if summary.ok { 0 } else { 1 })
}
