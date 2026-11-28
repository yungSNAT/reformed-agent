//! Run an integrated self-evolution loop and emit actionable recommendations.
//! Port of `scripts/run-self-evolution-loop.py`.

use std::fs;
use std::path::Path;

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
use crate::script_helpers::{
    extract_report_path, require_existing_dir, resolve_repo_root, resolve_report_path, run_bash_lc,
    tail_chars, write_json_pretty,
};

const DEFAULT_GOLDEN_CMD: &str = "python3 scripts/run-golden-parity-harness.py --auto-issue";
const DEFAULT_EVAL_CMD: &str =
    "cargo run --locked -q -p hermes-parity-tools -- run-eval-trend-gate --allow-missing-baseline --json";
const DEFAULT_ELITE_CMD: &str =
    "cargo run --locked -q -p hermes-parity-tools -- run-elite-sync-gate --json";

#[derive(Args, Debug)]
pub struct SelfEvolutionLoopArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Optional objective text included in the loop report
    #[arg(long, default_value = "")]
    objective: String,
    /// Golden parity harness command
    #[arg(long, default_value = DEFAULT_GOLDEN_CMD)]
    golden_cmd: String,
    /// Eval trend gate command
    #[arg(long, default_value = DEFAULT_EVAL_CMD)]
    eval_cmd: String,
    /// Consolidated elite gate command
    #[arg(long, default_value = DEFAULT_ELITE_CMD)]
    elite_cmd: String,
    /// Skip golden parity harness run
    #[arg(long)]
    skip_golden: bool,
    /// Skip eval trend gate run
    #[arg(long)]
    skip_eval: bool,
    /// Skip elite sync gate run
    #[arg(long)]
    skip_elite: bool,
    /// Optional explicit report path
    #[arg(long, default_value = "")]
    report_path: String,
    /// Print JSON report
    #[arg(long)]
    json: bool,
}

fn read_json_dict(path: &Path) -> Option<Py> {
    let raw = fs::read_to_string(path).ok()?;
    let parsed = py::loads(&raw).ok()?;
    parsed.is_dict().then_some(parsed)
}

fn run_slim(command: &str, repo_root: &Path) -> Result<(bool, Py), String> {
    let run = run_bash_lc(command, repo_root)?;
    let output = format!("{}\n{}", run.stdout, run.stderr);
    let report_path_raw = extract_report_path(
        &output,
        &[
            r"\[[^\]]+\]\s*Report:\s*(.+)",
            r"report=(\S+)",
            r#""report_path"\s*:\s*"([^"]+)""#,
        ],
    );
    let mut report_path = String::new();
    let mut report_payload: Option<Py> = None;
    if let Some(raw_path) = report_path_raw {
        let candidate = py::expanduser(&raw_path)?;
        let resolved = if candidate.is_absolute() {
            resolve_argument(&candidate)?
        } else {
            resolve_argument(&repo_root.join(candidate))?
        };
        report_path = py::path_str(&resolved);
        if resolved.exists() {
            report_payload = read_json_dict(&resolved);
        }
    }

    let report_ok = report_payload
        .as_ref()
        .and_then(|p| p.get("ok"))
        .and_then(|v| match v {
            Py::Bool(b) => Some(Py::Bool(*b)),
            _ => None,
        })
        .unwrap_or(Py::Null);

    let report_excerpt = match report_payload.as_ref() {
        Some(payload) => {
            let mut pairs = Vec::new();
            for key in [
                "summary",
                "missing_commands",
                "missing_tui_tests",
                "drift",
                "sections",
            ] {
                if let Some(val) = payload.get(key) {
                    pairs.push((key.to_string(), val.clone()));
                }
            }
            Py::Dict(pairs)
        }
        None => Py::Null,
    };

    let section = Py::dict([
        ("command", Py::from(command)),
        ("exit_code", Py::Int(i128::from(run.exit_code))),
        ("ok", Py::from(run.ok)),
        ("elapsed_ms", Py::Int(run.elapsed_ms)),
        ("stdout_tail", Py::from(tail_chars(&run.stdout, 4000))),
        ("stderr_tail", Py::from(tail_chars(&run.stderr, 4000))),
        ("report_path", Py::from(report_path)),
        ("report_ok", report_ok),
        ("report_excerpt", report_excerpt),
    ]);
    Ok((run.ok, section))
}

struct LoopRecommendation {
    id: &'static str,
    severity: &'static str,
    title: &'static str,
    reason: String,
    command: &'static str,
}

impl LoopRecommendation {
    fn to_py(&self) -> Py {
        Py::dict([
            ("id", Py::from(self.id)),
            ("severity", Py::from(self.severity)),
            ("title", Py::from(self.title)),
            ("reason", Py::from(self.reason.as_str())),
            ("command", Py::from(self.command)),
        ])
    }
}

fn build_recommendations(
    objective: &str,
    golden_ok: Option<bool>,
    eval_ok: Option<bool>,
    elite_ok: Option<bool>,
) -> Vec<LoopRecommendation> {
    let trimmed = py::strip(objective);
    let objective_hint = if trimmed.is_empty() {
        String::new()
    } else {
        format!(" Objective: {trimmed}.")
    };

    let mut out = Vec::new();
    if golden_ok == Some(false) {
        out.push(LoopRecommendation {
            id: "PARITY_DRIFT",
            severity: "P0",
            title: "Resolve command/TUI parity drift before feature work",
            reason: format!(
                "Golden parity harness failed; upstream command surface or required TUI contracts drifted.{objective_hint}"
            ),
            command: DEFAULT_GOLDEN_CMD,
        });
    }
    if eval_ok == Some(false) {
        out.push(LoopRecommendation {
            id: "EVAL_REGRESSION",
            severity: "P0",
            title: "Recover eval trend before promotion",
            reason: format!(
                "Eval trend gate failed; current behavior regressed against baseline.{objective_hint}"
            ),
            command: "hermes route-autotune plan --apply --json && cargo run --locked -q -p hermes-parity-tools -- run-eval-trend-gate --json",
        });
    }
    if elite_ok == Some(false) {
        out.push(LoopRecommendation {
            id: "ELITE_GATE_FAIL",
            severity: "P0",
            title: "Hold release and remediate elite gate failures",
            reason: format!(
                "Consolidated elite gate failed; one or more hardening/performance/parity sections failed.{objective_hint}"
            ),
            command: DEFAULT_ELITE_CMD,
        });
    }
    if out.is_empty() {
        out.push(LoopRecommendation {
            id: "PROMOTE_BASELINE",
            severity: "P2",
            title: "Promote current state as next baseline",
            reason: format!(
                "All enabled sections passed; safe to store this run as a quality baseline.{objective_hint}"
            ),
            command: "cargo run --locked -q -p hermes-parity-tools -- generate-global-parity-proof --repo-root .",
        });
    }
    out
}

pub fn run(args: SelfEvolutionLoopArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    require_existing_dir(&repo_root, "repo root")?;

    let report_path = resolve_report_path(&repo_root, &args.report_path, "self-evolution-loop")?;

    let mut section_pairs: Vec<(&str, Py)> = Vec::new();
    let mut golden_ok = None;
    let mut eval_ok = None;
    let mut elite_ok = None;

    if !args.skip_golden {
        let (ok, section) = run_slim(&args.golden_cmd, &repo_root)?;
        golden_ok = Some(ok);
        section_pairs.push(("golden_parity", section));
    }
    if !args.skip_eval {
        let (ok, section) = run_slim(&args.eval_cmd, &repo_root)?;
        eval_ok = Some(ok);
        section_pairs.push(("eval_trend", section));
    }
    if !args.skip_elite {
        let (ok, section) = run_slim(&args.elite_cmd, &repo_root)?;
        elite_ok = Some(ok);
        section_pairs.push(("elite_sync", section));
    }

    let total = section_pairs.len();
    let passed = [golden_ok, eval_ok, elite_ok]
        .into_iter()
        .flatten()
        .filter(|&ok| ok)
        .count();
    let failed = total.saturating_sub(passed);
    let ok = total == 0 || passed == total;
    let intelligence_index = if total > 0 {
        ((passed as f64 / total as f64) * 100.0 * 100.0).round_ties_even() / 100.0
    } else {
        100.0
    };
    let recommendations = build_recommendations(&args.objective, golden_ok, eval_ok, elite_ok);

    let mut report = Py::dict([
        ("generated_at", Py::from(py::utc_isoformat())),
        ("repo_root", Py::from(py::path_str(&repo_root))),
        ("objective", Py::from(py::strip(&args.objective))),
        ("ok", Py::from(ok)),
        (
            "summary",
            Py::dict([
                ("total_sections", Py::from(total)),
                ("passed_sections", Py::from(passed)),
                ("failed_sections", Py::from(failed)),
                ("intelligence_index", Py::Float(intelligence_index)),
            ]),
        ),
        ("sections", Py::dict(section_pairs)),
        (
            "recommendations",
            Py::List(
                recommendations
                    .iter()
                    .map(LoopRecommendation::to_py)
                    .collect(),
            ),
        ),
    ]);

    write_json_pretty(&report_path, &report, false)?;
    report.set("report_path", Py::from(py::path_str(&report_path)));

    if args.json {
        println!("{}", py::dumps(&report, Some(2), false));
    } else {
        let status = if ok { "PASSED" } else { "FAILED" };
        println!(
            "[self-evolution-loop] {status} (passed={passed}/{total} index={})",
            py::float_repr(intelligence_index)
        );
        println!(
            "[self-evolution-loop] Report: {}",
            py::path_str(&report_path)
        );
        if !recommendations.is_empty() {
            println!("[self-evolution-loop] Recommendations:");
            for rec in &recommendations {
                println!("- [{}] {} :: {}", rec.severity, rec.title, rec.command);
            }
        }
    }

    Ok(i32::from(!ok))
}
