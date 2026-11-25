//! Run consolidated ELITE sync gates and emit a single pass/fail report.
//! Port of `scripts/run-elite-sync-gate.py`.

use std::path::Path;

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
use crate::script_helpers::{
    extract_report_path, require_existing_dir, resolve_repo_root, resolve_report_path, run_bash_lc,
    shlex_quote, tail_chars, write_json_pretty,
};

#[derive(Args, Debug)]
pub struct EliteSyncGateArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Red-team gate command
    #[arg(
        long,
        default_value = "cargo run --locked -q -p hermes-parity-tools -- run-redteam-gate --max-severity-allowed none"
    )]
    redteam_cmd: String,
    /// Adapter chaos harness command
    #[arg(
        long,
        default_value = "cargo run --locked -q -p hermes-parity-tools -- run-adapter-chaos-harness"
    )]
    chaos_cmd: String,
    /// Zero-copy hot path benchmark command
    #[arg(
        long,
        default_value = "cargo run --locked -q -p hermes-parity-tools -- run-zero-copy-hotpath-bench"
    )]
    hotpath_cmd: String,
    /// Differential parity command
    #[arg(
        long,
        default_value = "cargo run --locked -q -p hermes-parity-tools -- run-differential-parity-gate --max-commits-behind 0"
    )]
    parity_diff_cmd: String,
    /// Required upstream surface coverage command
    #[arg(
        long,
        default_value = "cargo run --locked -q -p hermes-parity-tools -- run-upstream-surface-coverage-gate --upstream-ref upstream/main"
    )]
    surface_coverage_cmd: String,
    /// Eval trend gate command
    #[arg(
        long,
        default_value = "cargo run --locked -q -p hermes-parity-tools -- run-eval-trend-gate --allow-missing-baseline"
    )]
    eval_trend_cmd: String,
    /// Release secret-scan gate command
    #[arg(
        long,
        default_value = "cargo run --locked -q -p hermes-parity-tools -- run-security-release-gate-v2 --repo-root . --report .sync-reports/security-release-gate-v2.json"
    )]
    release_security_cmd: String,
    /// Performance autopilot command
    #[arg(
        long,
        default_value = "cargo run --locked -q -p hermes-parity-tools -- run-performance-autopilot --repo-root . --strict"
    )]
    performance_autopilot_cmd: String,
    /// Optional baseline adapter chaos report for regression comparison
    #[arg(long, default_value = "")]
    chaos_baseline: String,
    /// Chaos compare command
    #[arg(
        long,
        default_value = "cargo run --locked -q -p hermes-parity-tools -- compare-adapter-chaos-reports"
    )]
    chaos_compare_cmd: String,
    /// Optional explicit report path
    #[arg(long, default_value = "")]
    report_path: String,
    /// Optional rollback command to execute when gate fails
    #[arg(long, default_value = "")]
    rollback_cmd: String,
    /// Print JSON report
    #[arg(long)]
    json: bool,
}

fn slim(cmd: &str, repo_root: &Path) -> Result<(bool, Option<String>, Py), String> {
    let raw = run_bash_lc(cmd, repo_root)?;
    let merged = format!("{}\n{}", raw.stdout, raw.stderr);
    let report_path = extract_report_path(
        &merged,
        &[
            r"\[redteam-gate\] Report:\s*(.+)",
            r"report=(\S+)",
            r"Report:\s*(\S+)",
        ],
    );
    let payload = Py::dict([
        ("command", Py::from(cmd)),
        ("exit_code", Py::Int(i128::from(raw.exit_code))),
        ("ok", Py::from(raw.ok)),
        ("elapsed_ms", Py::Int(raw.elapsed_ms)),
        ("stdout_tail", Py::from(tail_chars(&raw.stdout, 4000))),
        ("stderr_tail", Py::from(tail_chars(&raw.stderr, 4000))),
        (
            "report_path",
            report_path.as_deref().map_or(Py::Null, Py::from),
        ),
    ]);
    Ok((raw.ok, report_path, payload))
}

fn compare_chaos(
    args: &EliteSyncGateArgs,
    repo_root: &Path,
    report_path: &Path,
    chaos_report: Option<&str>,
) -> Result<(bool, Py), String> {
    let baseline = resolve_argument(&py::expanduser(py::strip(&args.chaos_baseline))?)?;
    let Some(current) = chaos_report.filter(|s| !s.is_empty()) else {
        return Ok((
            false,
            Py::dict([
                ("ok", Py::from(false)),
                (
                    "reason",
                    Py::from("baseline report missing or current chaos report unavailable"),
                ),
            ]),
        ));
    };
    if !baseline.exists() {
        return Ok((
            false,
            Py::dict([
                ("ok", Py::from(false)),
                (
                    "reason",
                    Py::from("baseline report missing or current chaos report unavailable"),
                ),
            ]),
        ));
    }
    let stem = report_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("elite-sync-gate");
    let compare_out = report_path.with_file_name(format!("{stem}-chaos-compare.json"));
    let compare_cmd = format!(
        "{} --baseline {} --current {} --output {} --json",
        args.chaos_compare_cmd,
        shlex_quote(&py::path_str(&baseline)),
        shlex_quote(current),
        shlex_quote(&py::path_str(&compare_out))
    );
    let (ok, _, mut section) = slim(&compare_cmd, repo_root)?;
    section.set("report_path", Py::from(py::path_str(&compare_out)));
    Ok((ok, section))
}

fn build_gate_report(
    args: &EliteSyncGateArgs,
    repo_root: &Path,
    sections: Vec<(String, Py)>,
    section_oks: &[bool],
) -> Result<(bool, usize, usize, Py), String> {
    let total = section_oks.len();
    let passed = section_oks.iter().filter(|ok| **ok).count();
    let failed = total - passed;
    let gate_ok = failed == 0;
    let rollback = if !gate_ok && !py::strip(&args.rollback_cmd).is_empty() {
        let (_, _, rb) = slim(py::strip(&args.rollback_cmd), repo_root)?;
        rb
    } else {
        Py::Null
    };
    let report = Py::dict([
        ("generated_at", Py::from(py::utc_isoformat())),
        ("repo_root", Py::from(py::path_str(repo_root))),
        ("ok", Py::from(gate_ok)),
        (
            "summary",
            Py::dict([
                ("total_sections", Py::from(total)),
                ("passed_sections", Py::from(passed)),
                ("failed_sections", Py::from(failed)),
            ]),
        ),
        ("sections", Py::Dict(sections)),
        ("rollback", rollback),
    ]);
    Ok((gate_ok, passed, total, report))
}

struct GateSections {
    sections: Vec<(String, Py)>,
    passed: Vec<bool>,
}

fn collect_gate_sections(
    args: &EliteSyncGateArgs,
    repo_root: &Path,
    report_path: &Path,
) -> Result<GateSections, String> {
    let specs = [
        ("redteam", args.redteam_cmd.as_str()),
        ("chaos", args.chaos_cmd.as_str()),
        ("hotpath", args.hotpath_cmd.as_str()),
        ("parity_differential", args.parity_diff_cmd.as_str()),
        ("surface_coverage", args.surface_coverage_cmd.as_str()),
        ("eval_trend", args.eval_trend_cmd.as_str()),
        ("release_security", args.release_security_cmd.as_str()),
        (
            "performance_autopilot",
            args.performance_autopilot_cmd.as_str(),
        ),
    ];
    let mut sections: Vec<(String, Py)> = Vec::with_capacity(specs.len() + 1);
    let mut passed: Vec<bool> = Vec::with_capacity(specs.len() + 1);
    let mut chaos_report_path: Option<String> = None;
    for (key, cmd) in specs {
        let (ok, rpt, payload) = slim(cmd, repo_root)?;
        if key == "chaos" {
            chaos_report_path = rpt;
        }
        passed.push(ok);
        sections.push((key.to_string(), payload));
    }
    if !py::strip(&args.chaos_baseline).is_empty() {
        let (cmp_ok, cmp_payload) =
            compare_chaos(args, repo_root, report_path, chaos_report_path.as_deref())?;
        passed.push(cmp_ok);
        sections.push(("chaos_compare".to_string(), cmp_payload));
    }
    Ok(GateSections { sections, passed })
}

pub fn run(args: EliteSyncGateArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    require_existing_dir(&repo_root, "repo root")?;
    let report_path = resolve_report_path(&repo_root, &args.report_path, "elite-sync-gate")?;
    let collected = collect_gate_sections(&args, &repo_root, &report_path)?;
    let (gate_ok, passed, total, mut report) =
        build_gate_report(&args, &repo_root, collected.sections, &collected.passed)?;
    write_json_pretty(&report_path, &report, false)?;
    report.set("report_path", Py::from(py::path_str(&report_path)));
    if args.json {
        println!("{}", py::dumps(&report, Some(2), false));
    } else {
        let status = if gate_ok { "PASSED" } else { "FAILED" };
        println!("[elite-sync-gate] {status} (passed={passed}/{total})");
        println!("[elite-sync-gate] Report: {}", py::path_str(&report_path));
    }
    Ok(if gate_ok { 0 } else { 1 })
}
