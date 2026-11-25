//! Run zero-copy hot-path benchmark tests and emit a JSON report.
//! Port of `scripts/run-zero-copy-hotpath-bench.py`.

use clap::Args;
use regex::Regex;

use crate::py::{self, Py};
use crate::script_helpers::{
    isoformat_secs, resolve_repo_root, resolve_report_path, run_process, shlex_split, tail_chars,
    write_json_pretty,
};

const DEFAULT_COMMAND: &str =
    "cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture";

#[derive(Args, Debug)]
pub struct ZeroCopyHotpathArgs {
    /// Repo root (default: current dir)
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Benchmark command
    #[arg(long, default_value = DEFAULT_COMMAND)]
    command: String,
    /// Optional explicit output file
    #[arg(long, default_value = "")]
    output: String,
}

fn parse_ns_per_eval(stdout: &str) -> Option<i128> {
    let re = Regex::new(r"tool_policy_hot_path_ns_per_eval=(\d+)").expect("valid regex");
    re.captures(stdout)
        .and_then(|cap| cap.get(1))
        .and_then(|m| m.as_str().parse::<i128>().ok())
}

pub fn run(args: ZeroCopyHotpathArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    let output_path = resolve_report_path(&repo_root, &args.output, "zero-copy-hotpath")?;
    let cmd = shlex_split(&args.command)?;
    let run = run_process(&cmd, &repo_root, None)?;

    let ns_per_eval = parse_ns_per_eval(&run.stdout);
    let report = Py::dict([
        ("timestamp_utc", Py::from(isoformat_secs(run.finished))),
        ("duration_seconds", Py::Float(run.duration_seconds)),
        ("repo_root", Py::from(py::path_str(&repo_root))),
        ("command", Py::strs(&cmd)),
        ("exit_code", Py::Int(i128::from(run.exit_code))),
        ("passed", Py::from(run.ok)),
        ("ns_per_eval", ns_per_eval.map_or(Py::Null, Py::Int)),
        ("stdout_tail", Py::from(tail_chars(&run.stdout, 8000))),
        ("stderr_tail", Py::from(tail_chars(&run.stderr, 8000))),
    ]);
    write_json_pretty(&output_path, &report, false)?;

    let ns_display = ns_per_eval.map_or_else(|| "None".to_string(), |n| n.to_string());
    println!(
        "Zero-copy hot-path bench {} (ns_per_eval={ns_display}) report={}",
        if run.ok { "PASSED" } else { "FAILED" },
        py::path_str(&output_path)
    );
    Ok(i32::from(!run.ok))
}
