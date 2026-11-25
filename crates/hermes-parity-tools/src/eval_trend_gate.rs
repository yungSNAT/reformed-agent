//! Evaluate benchmark trend regressions and gate on configurable thresholds.
//! Port of `scripts/run-eval-trend-gate.py`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
use crate::script_helpers::{
    py_float, read_utf8, resolve_repo_root, resolve_report_path, write_json_pretty,
};

#[derive(Args, Debug)]
pub struct EvalTrendGateArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Current eval run JSON (defaults to latest in evals/)
    #[arg(long, default_value = "")]
    current: String,
    /// Baseline eval run JSON (defaults to previous in evals/)
    #[arg(long, default_value = "")]
    baseline: String,
    /// Pass when baseline/current runs are unavailable
    #[arg(long)]
    allow_missing_baseline: bool,
    /// Maximum allowed absolute pass@1 drop
    #[arg(long, default_value_t = 0.03)]
    max_pass_at_1_drop: f64,
    /// Maximum allowed fractional increase in mean task duration
    #[arg(long, default_value_t = 0.40)]
    max_mean_task_duration_increase: f64,
    /// Maximum allowed fractional increase in total cost
    #[arg(long, default_value_t = 0.50)]
    max_cost_increase: f64,
    /// Optional report path
    #[arg(long, default_value = "")]
    report_path: String,
    /// Print JSON report
    #[arg(long)]
    json: bool,
}

fn duration_to_secs(raw: Option<&Py>) -> f64 {
    match raw {
        None | Some(Py::Null) => 0.0,
        Some(Py::Int(i)) => *i as f64,
        Some(Py::Float(f)) => *f,
        Some(Py::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(Py::Str(s)) => {
            let trimmed = py::strip(s).to_lowercase();
            let text = trimmed.strip_suffix('s').unwrap_or(&trimmed);
            text.parse::<f64>().unwrap_or(0.0)
        }
        Some(Py::Dict(_)) => {
            let dict = raw.unwrap();
            let secs = dict
                .get("secs")
                .filter(|v| crate::report_io::truth(v))
                .or_else(|| dict.get("seconds").filter(|v| crate::report_io::truth(v)));
            let nanos = dict
                .get("nanos")
                .filter(|v| crate::report_io::truth(v))
                .or_else(|| {
                    dict.get("nanoseconds")
                        .filter(|v| crate::report_io::truth(v))
                });
            let secs_v = match secs {
                None | Some(Py::Null) => 0.0,
                Some(v) => py_float(Some(v), f64::NAN),
            };
            let nanos_v = match nanos {
                None | Some(Py::Null) => 0.0,
                Some(v) => py_float(Some(v), f64::NAN),
            };
            if secs_v.is_nan() || nanos_v.is_nan() {
                0.0
            } else {
                secs_v + nanos_v / 1_000_000_000.0
            }
        }
        _ => 0.0,
    }
}

struct EvalMetrics {
    total: f64,
    pass_at_1: f64,
    mean_task_duration_secs: f64,
    total_cost_usd: f64,
}

impl EvalMetrics {
    fn to_py(&self) -> Py {
        Py::dict([
            ("total", Py::Float(self.total)),
            ("pass_at_1", Py::Float(self.pass_at_1)),
            (
                "mean_task_duration_secs",
                Py::Float(self.mean_task_duration_secs),
            ),
            ("total_cost_usd", Py::Float(self.total_cost_usd)),
        ])
    }
}

fn extract_metrics(record: &Py) -> EvalMetrics {
    let metrics = record
        .get("metrics")
        .filter(|v| v.is_dict())
        .cloned()
        .unwrap_or_else(|| Py::Dict(Vec::new()));
    let total = 1.0_f64.max(py_float(metrics.get("total"), 0.0));
    let total_duration = duration_to_secs(metrics.get("total_duration"));
    EvalMetrics {
        total,
        pass_at_1: py_float(metrics.get("pass_at_1"), 0.0),
        mean_task_duration_secs: total_duration / total,
        total_cost_usd: py_float(metrics.get("total_cost_usd"), 0.0),
    }
}

fn find_latest_eval_files(evals_dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !evals_dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<(SystemTime, PathBuf)> = fs::read_dir(evals_dir)
        .map_err(|e| format!("{}: {e}", evals_dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "json"))
        .map(|p| {
            let mtime = p
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (mtime, p)
        })
        .collect();
    files.sort_by_key(|a| std::cmp::Reverse(a.0));
    Ok(files.into_iter().map(|(_, p)| p).collect())
}

fn rel_change(current: f64, baseline: f64) -> f64 {
    if baseline <= 0.0 {
        if current <= 0.0 {
            0.0
        } else {
            1.0
        }
    } else {
        (current - baseline) / baseline
    }
}

pub fn run(args: EvalTrendGateArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    let report_path = resolve_report_path(&repo_root, &args.report_path, "eval-trend-gate")?;

    let latest = find_latest_eval_files(&repo_root.join("evals"))?;
    let mut current_path = if args.current.is_empty() {
        None
    } else {
        Some(resolve_argument(&py::expanduser(&args.current)?)?)
    };
    let mut baseline_path = if args.baseline.is_empty() {
        None
    } else {
        Some(resolve_argument(&py::expanduser(&args.baseline)?)?)
    };
    if current_path.is_none() && !latest.is_empty() {
        current_path = Some(latest[0].clone());
    }
    if baseline_path.is_none() && latest.len() > 1 {
        baseline_path = Some(latest[1].clone());
    }

    let inputs_ok = current_path.as_ref().is_some_and(|p| p.exists())
        && baseline_path.as_ref().is_some_and(|p| p.exists());
    if !inputs_ok {
        let ok = args.allow_missing_baseline;
        let report = Py::dict([
            ("generated_at", Py::from(py::utc_isoformat())),
            ("ok", Py::from(ok)),
            ("reason", Py::from("missing_eval_inputs")),
            (
                "allow_missing_baseline",
                Py::from(args.allow_missing_baseline),
            ),
            (
                "current_path",
                current_path
                    .as_ref()
                    .map_or(Py::Null, |p| Py::from(py::path_str(p))),
            ),
            (
                "baseline_path",
                baseline_path
                    .as_ref()
                    .map_or(Py::Null, |p| Py::from(py::path_str(p))),
            ),
        ]);
        write_json_pretty(&report_path, &report, false)?;
        if args.json {
            println!("{}", py::dumps(&report, Some(2), false));
        } else {
            let status = if ok { "PASSED" } else { "FAILED" };
            println!("[eval-trend-gate] {status} (missing baseline/current report)");
            println!("[eval-trend-gate] Report: {}", py::path_str(&report_path));
        }
        return Ok(i32::from(!ok));
    }

    let current_path = current_path.expect("checked exists");
    let baseline_path = baseline_path.expect("checked exists");
    let current = py::loads(&read_utf8(&current_path)?)?;
    let baseline = py::loads(&read_utf8(&baseline_path)?)?;
    let current_metrics = extract_metrics(&current);
    let baseline_metrics = extract_metrics(&baseline);

    let pass_drop = baseline_metrics.pass_at_1 - current_metrics.pass_at_1;
    let duration_increase = rel_change(
        current_metrics.mean_task_duration_secs,
        baseline_metrics.mean_task_duration_secs,
    );
    let cost_increase = rel_change(
        current_metrics.total_cost_usd,
        baseline_metrics.total_cost_usd,
    );

    let check_specs = [
        ("pass_at_1_drop", pass_drop, args.max_pass_at_1_drop),
        (
            "mean_task_duration_increase",
            duration_increase,
            args.max_mean_task_duration_increase,
        ),
        ("total_cost_increase", cost_increase, args.max_cost_increase),
    ];
    let gate_ok = check_specs.iter().all(|(_, val, limit)| val <= limit);
    let checks = Py::List(
        check_specs
            .into_iter()
            .map(|(name, value, limit)| {
                Py::dict([
                    ("name", Py::from(name)),
                    ("value", Py::Float(value)),
                    ("limit", Py::Float(limit)),
                    ("ok", Py::from(value <= limit)),
                ])
            })
            .collect(),
    );

    let mut report = Py::dict([
        ("generated_at", Py::from(py::utc_isoformat())),
        ("ok", Py::from(gate_ok)),
        ("current_path", Py::from(py::path_str(&current_path))),
        ("baseline_path", Py::from(py::path_str(&baseline_path))),
        ("current_metrics", current_metrics.to_py()),
        ("baseline_metrics", baseline_metrics.to_py()),
        ("checks", checks),
    ]);
    write_json_pretty(&report_path, &report, false)?;
    report.set("report_path", Py::from(py::path_str(&report_path)));

    if args.json {
        println!("{}", py::dumps(&report, Some(2), false));
    } else {
        let status = if gate_ok { "PASSED" } else { "FAILED" };
        println!(
            "[eval-trend-gate] {status} (pass_drop={pass_drop:.4}, duration_increase={duration_increase:.3}, cost_increase={cost_increase:.3})"
        );
        println!("[eval-trend-gate] Report: {}", py::path_str(&report_path));
    }
    Ok(i32::from(!gate_ok))
}
