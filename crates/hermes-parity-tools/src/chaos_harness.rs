//! Deterministic adapter chaos harness runner and report comparison gate.
//! Ports of `scripts/run-adapter-chaos-harness.py` and
//! `scripts/compare-adapter-chaos-reports.py`.

use std::fs;
use std::path::Path;

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
use crate::script_helpers::{
    py_int, py_splitlines, resolve_repo_root, resolve_report_path, run_process, shlex_split,
    tail_chars, write_json_pretty,
};

const DEFAULT_CHAOS_COMMAND: &str =
    "cargo test -p hermes-agent chaos_harness_profiles_verify_retry_and_fallback -- --nocapture";

#[derive(Args, Debug)]
pub struct RunAdapterChaosArgs {
    /// Repository root (default: current directory).
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Command to run.
    #[arg(long, default_value = DEFAULT_CHAOS_COMMAND)]
    command: String,
    /// Optional explicit output report path.
    #[arg(long, default_value = "")]
    output: String,
}

#[derive(Args, Debug)]
pub struct CompareAdapterChaosArgs {
    /// Baseline chaos report JSON path
    #[arg(long)]
    baseline: String,
    /// Current chaos report JSON path
    #[arg(long)]
    current: String,
    /// Optional output report path
    #[arg(long, default_value = "")]
    output: String,
    /// Print JSON report to stdout
    #[arg(long)]
    json: bool,
}

fn extract_failure_excerpt(stdout: &str, stderr: &str) -> String {
    let combined = py::strip(&format!("{}\n{}", py::strip(stdout), py::strip(stderr))).to_string();
    let lower = combined.to_ascii_lowercase();
    let marker = "adapter chaos harness mismatches:";
    if let Some(idx) = lower.find(marker) {
        let slice = &combined[idx..];
        let end = slice
            .char_indices()
            .nth(4000)
            .map_or(slice.len(), |(i, _)| i);
        slice[..end].to_string()
    } else {
        tail_chars(&combined, 2000).to_string()
    }
}

fn extract_scenario_runs(stdout: &str) -> Vec<Py> {
    let marker = "adapter chaos harness results:";
    for line in py_splitlines(stdout) {
        let Some((_, payload)) = line.split_once(marker) else {
            continue;
        };
        let Ok(Py::List(items)) = py::loads(py::strip(payload)) else {
            return Vec::new();
        };
        return items.into_iter().filter(Py::is_dict).collect();
    }
    Vec::new()
}

pub fn run_harness(args: RunAdapterChaosArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    let output_path = resolve_report_path(&repo_root, &args.output, "adapter-chaos")?;
    let cmd = shlex_split(&args.command)?;
    let run = run_process(&cmd, &repo_root, None)?;

    let runs = extract_scenario_runs(&run.stdout);
    let mut pairs = vec![
        (
            "timestamp_utc",
            Py::from(format!(
                "{}Z",
                run.finished.strftime("%Y-%m-%dT%H:%M:%S+00:00")
            )),
        ),
        ("duration_seconds", Py::Float(run.duration_seconds)),
        ("repo_root", Py::from(py::path_str(&repo_root))),
        ("command", Py::strs(&cmd)),
        ("exit_code", Py::Int(i128::from(run.exit_code))),
        ("passed", Py::from(run.ok)),
        ("stdout_tail", Py::from(tail_chars(&run.stdout, 8000))),
        ("stderr_tail", Py::from(tail_chars(&run.stderr, 8000))),
        ("scenario_runs", Py::List(runs.clone())),
    ];

    if !runs.is_empty() {
        let max_attempts = runs
            .iter()
            .map(|e| py_int(e.get("actual").and_then(|a| a.get("attempts")), 0))
            .max()
            .unwrap_or(0);
        let max_fallback_calls = runs
            .iter()
            .map(|e| py_int(e.get("actual").and_then(|a| a.get("fallback_calls")), 0))
            .max()
            .unwrap_or(0);
        let error_outcomes = runs
            .iter()
            .filter(|e| {
                let outcome = e
                    .get("actual")
                    .and_then(|a| a.get("outcome"))
                    .map(py::str_of)
                    .unwrap_or_default();
                outcome.to_lowercase() != "success"
            })
            .count();
        pairs.push((
            "scenario_summary",
            Py::dict([
                ("count", Py::from(runs.len())),
                ("max_attempts", Py::Int(max_attempts)),
                ("max_fallback_calls", Py::Int(max_fallback_calls)),
                ("error_outcomes", Py::from(error_outcomes)),
            ]),
        ));
    }
    if run.exit_code != 0 {
        pairs.push((
            "failure_excerpt",
            Py::from(extract_failure_excerpt(&run.stdout, &run.stderr)),
        ));
    }

    let report = Py::dict(pairs);
    write_json_pretty(&output_path, &report, false)?;
    println!(
        "Adapter chaos harness {} (exit={}) report={}",
        if run.ok { "PASSED" } else { "FAILED" },
        run.exit_code,
        py::path_str(&output_path)
    );
    Ok(i32::from(!run.ok))
}

fn load_report(path: &Path) -> Result<Py, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("failed to read report {}: {e}", py::path_str(path)))?;
    let data = py::loads(&raw)
        .map_err(|e| format!("failed to parse report {}: {e}", py::path_str(path)))?;
    if !data.is_dict() {
        return Err(format!(
            "report {} must be a JSON object",
            py::path_str(path)
        ));
    }
    Ok(data)
}

fn outcome_rank(value: &str) -> i32 {
    match value.to_lowercase().as_str() {
        "success" => 0,
        "error" => 1,
        _ => 2,
    }
}

fn index_runs(report: &Py) -> Vec<(String, Py)> {
    let Some(Py::List(runs)) = report.get("scenario_runs") else {
        return Vec::new();
    };
    let mut indexed: Vec<(String, Py)> = Vec::new();
    for entry in runs {
        if !entry.is_dict() {
            continue;
        }
        let scenario = entry
            .get("scenario")
            .filter(|v| crate::report_io::truth(v))
            .map(py::str_of)
            .unwrap_or_default();
        let scenario = py::strip(&scenario).to_string();
        if scenario.is_empty() {
            continue;
        }
        if let Some((_, slot)) = indexed.iter_mut().find(|(k, _)| k == &scenario) {
            *slot = entry.clone();
        } else {
            indexed.push((scenario, entry.clone()));
        }
    }
    indexed
}

fn dict_or_empty(value: Option<&Py>) -> Py {
    match value {
        Some(v) if v.is_dict() => v.clone(),
        _ => Py::Dict(Vec::new()),
    }
}

pub fn run_compare(args: CompareAdapterChaosArgs) -> Result<i32, String> {
    let baseline_path = resolve_argument(&py::expanduser(&args.baseline)?)?;
    let current_path = resolve_argument(&py::expanduser(&args.current)?)?;
    let baseline = load_report(&baseline_path)?;
    let current = load_report(&current_path)?;

    let baseline_runs = index_runs(&baseline);
    let current_runs = index_runs(&current);
    let mut regressions: Vec<Py> = Vec::new();

    if !baseline_runs.is_empty() && current_runs.is_empty() {
        regressions.push(Py::dict([
            ("type", Py::from("missing_current_scenario_metrics")),
            (
                "reason",
                Py::from("baseline has scenario_runs but current does not"),
            ),
        ]));
    }

    for (scenario, base) in &baseline_runs {
        let Some((_, cur)) = current_runs.iter().find(|(s, _)| s == scenario) else {
            regressions.push(Py::dict([
                ("type", Py::from("scenario_missing")),
                ("scenario", Py::from(scenario.as_str())),
                (
                    "reason",
                    Py::from("scenario present in baseline but missing in current"),
                ),
            ]));
            continue;
        };
        let base_actual = dict_or_empty(base.get("actual"));
        let cur_actual = dict_or_empty(cur.get("actual"));

        let base_attempts = py_int(base_actual.get("attempts"), 0);
        let cur_attempts = py_int(cur_actual.get("attempts"), 0);
        if cur_attempts > base_attempts {
            regressions.push(Py::dict([
                ("type", Py::from("attempts_regression")),
                ("scenario", Py::from(scenario.as_str())),
                ("baseline", Py::Int(base_attempts)),
                ("current", Py::Int(cur_attempts)),
            ]));
        }

        let base_fallback = py_int(base_actual.get("fallback_calls"), 0);
        let cur_fallback = py_int(cur_actual.get("fallback_calls"), 0);
        if cur_fallback > base_fallback {
            regressions.push(Py::dict([
                ("type", Py::from("fallback_regression")),
                ("scenario", Py::from(scenario.as_str())),
                ("baseline", Py::Int(base_fallback)),
                ("current", Py::Int(cur_fallback)),
            ]));
        }

        let base_outcome = base_actual
            .get("outcome")
            .map(py::str_of)
            .unwrap_or_default();
        let cur_outcome = cur_actual
            .get("outcome")
            .map(py::str_of)
            .unwrap_or_default();
        if outcome_rank(&cur_outcome) > outcome_rank(&base_outcome) {
            regressions.push(Py::dict([
                ("type", Py::from("outcome_regression")),
                ("scenario", Py::from(scenario.as_str())),
                ("baseline", Py::from(base_outcome)),
                ("current", Py::from(cur_outcome)),
            ]));
        }

        let base_error = py::strip(
            &base_actual
                .get("error")
                .filter(|v| crate::report_io::truth(v))
                .map(py::str_of)
                .unwrap_or_default(),
        )
        .to_string();
        let cur_error = py::strip(
            &cur_actual
                .get("error")
                .filter(|v| crate::report_io::truth(v))
                .map(py::str_of)
                .unwrap_or_default(),
        )
        .to_string();
        if base_error.is_empty() && !cur_error.is_empty() {
            let cut = cur_error
                .char_indices()
                .nth(400)
                .map_or(cur_error.len(), |(i, _)| i);
            regressions.push(Py::dict([
                ("type", Py::from("new_error")),
                ("scenario", Py::from(scenario.as_str())),
                ("current_error", Py::from(&cur_error[..cut])),
            ]));
        }
    }

    let base_passed = baseline.get("passed").is_some_and(crate::report_io::truth);
    let cur_passed = current.get("passed").is_some_and(crate::report_io::truth);
    if base_passed && !cur_passed {
        regressions.push(Py::dict([
            ("type", Py::from("overall_pass_regression")),
            ("baseline_passed", Py::from(base_passed)),
            ("current_passed", Py::from(cur_passed)),
        ]));
    }

    let ok = regressions.is_empty();
    let reg_count = regressions.len();
    let reg_list = Py::List(regressions);
    let mut report = Py::dict([
        ("generated_at", Py::from(py::utc_isoformat())),
        ("baseline", Py::from(py::path_str(&baseline_path))),
        ("current", Py::from(py::path_str(&current_path))),
        ("ok", Py::from(ok)),
        ("regressions", reg_list.clone()),
    ]);

    let output_path = if args.output.is_empty() {
        None
    } else {
        let out = resolve_argument(&py::expanduser(&args.output)?)?;
        write_json_pretty(&out, &report, false)?;
        report.set("output", Py::from(py::path_str(&out)));
        Some(out)
    };

    if args.json {
        println!("{}", py::dumps(&report, Some(2), false));
    } else {
        let status = if ok { "PASSED" } else { "FAILED" };
        println!("Adapter chaos regression compare {status} (regressions={reg_count})");
        if let Some(out) = output_path {
            println!("Report: {}", py::path_str(&out));
        }
        if reg_count > 0 {
            println!("{}", py::dumps(&reg_list, Some(2), false));
        }
    }
    Ok(i32::from(!ok))
}

#[cfg(test)]
mod tests {
    use super::extract_failure_excerpt;

    #[test]
    fn extract_failure_excerpt_handles_unicode_case_expansion() {
        let prefix = "\u{0130}\u{00df}\u{212a} prefix ";
        let input = format!("{prefix}Adapter Chaos Harness Mismatches: detail line");
        let excerpt = extract_failure_excerpt(&input, "");
        assert_eq!(excerpt, "Adapter Chaos Harness Mismatches: detail line");
    }
}
