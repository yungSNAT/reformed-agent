//! Deterministic adversarial regression gate for Hermes Agent Ultra.
//! Port of `scripts/run-redteam-gate.py`.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
use crate::script_helpers::{py_int, run_process, shlex_join, tail_lines, write_json_pretty};

#[derive(Args, Debug)]
pub struct RedteamGateArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Suite JSON path (relative to repo root or absolute)
    #[arg(long, default_value = "scripts/redteam-cases.json")]
    suite: String,
    /// Optional explicit report JSON path
    #[arg(long, default_value = "")]
    report_path: String,
    /// Allow failures up to this severity (none|info|low|medium|high|critical)
    #[arg(long, default_value = "none")]
    max_severity_allowed: String,
}

fn severity_rank(severity: &str) -> i32 {
    match severity {
        "none" => -1,
        "info" => 0,
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        "critical" => 4,
        _ => i32::MIN,
    }
}

fn normalize_severity(raw: Option<&Py>) -> &'static str {
    let text = match raw {
        Some(v) if crate::report_io::truth(v) => py::strip(&py::str_of(v)).to_lowercase(),
        _ => String::new(),
    };
    match text.as_str() {
        "none" => "none",
        "info" => "info",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "critical" => "critical",
        _ => "medium",
    }
}

fn normalize_severity_str(raw: &str) -> &'static str {
    match py::strip(raw).to_lowercase().as_str() {
        "none" => "none",
        "info" => "info",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "critical" => "critical",
        _ => "medium",
    }
}

fn run_entry(entry: &Py, repo_root: &Path) -> Result<Py, String> {
    let entry_id = entry
        .get("id")
        .cloned()
        .unwrap_or_else(|| Py::from("unknown"));
    let cmd_list = match entry.get("cmd") {
        Some(Py::List(items))
            if !items.is_empty() && items.iter().all(|c| matches!(c, Py::Str(_))) =>
        {
            items
                .iter()
                .map(|c| match c {
                    Py::Str(s) => s.clone(),
                    _ => unreachable!(),
                })
                .collect::<Vec<_>>()
        }
        _ => {
            return Ok(Py::dict([
                ("id", entry_id),
                ("ok", Py::from(false)),
                ("error", Py::from("invalid command definition")),
            ]));
        }
    };

    let timeout_sec = py_int(entry.get("timeout_sec"), 180).max(30) as u64;
    let severity = normalize_severity(entry.get("severity"));
    let joined = shlex_join(&cmd_list);
    let res = run_process(&cmd_list, repo_root, Some(timeout_sec))?;

    if res.timed_out {
        return Ok(Py::dict([
            ("id", entry_id),
            ("severity", Py::from(severity)),
            ("command", Py::from(joined)),
            ("exit_code", Py::Int(124)),
            ("ok", Py::from(false)),
            ("elapsed_ms", Py::Int(res.elapsed_ms)),
            ("error", Py::from(format!("timeout after {timeout_sec}s"))),
        ]));
    }

    Ok(Py::dict([
        ("id", entry_id),
        ("severity", Py::from(severity)),
        ("command", Py::from(joined)),
        ("exit_code", Py::Int(i128::from(res.exit_code))),
        ("ok", Py::from(res.ok)),
        ("elapsed_ms", Py::Int(res.elapsed_ms)),
        ("stdout_tail", Py::strs(tail_lines(&res.stdout, 12))),
        ("stderr_tail", Py::strs(tail_lines(&res.stderr, 12))),
    ]))
}

pub fn run(args: RedteamGateArgs) -> Result<i32, String> {
    let max_allowed = normalize_severity_str(&args.max_severity_allowed);
    let max_allowed_rank = severity_rank(max_allowed);

    let repo_root = resolve_argument(Path::new(&args.repo_root))?;
    let suite_arg = Path::new(&args.suite);
    let suite_path = if suite_arg.is_absolute() {
        suite_arg.to_path_buf()
    } else {
        py::join(&repo_root, &args.suite)
    };

    let suite_raw = fs::read_to_string(&suite_path).map_err(|e| {
        format!(
            "failed to parse suite file {}: {e}",
            py::path_str(&suite_path)
        )
    })?;
    let suite = py::loads(&suite_raw).map_err(|e| {
        format!(
            "failed to parse suite file {}: {e}",
            py::path_str(&suite_path)
        )
    })?;

    let commands = match suite.get("commands") {
        Some(Py::List(items)) if !items.is_empty() => items,
        _ => return Err("suite has no commands".to_string()),
    };

    let mut results = Vec::with_capacity(commands.len());
    for entry in commands {
        results.push(run_entry(entry, &repo_root)?);
    }

    let passed = results
        .iter()
        .filter(|r| r.get("ok").is_some_and(crate::report_io::truth))
        .count();
    let failed = results.len() - passed;

    let mut blocking_failures = Vec::new();
    let mut tolerated_failures = Vec::new();
    for result in &results {
        if result.get("ok").is_some_and(crate::report_io::truth) {
            continue;
        }
        let rank = severity_rank(normalize_severity(result.get("severity")));
        if rank > max_allowed_rank {
            blocking_failures.push(result.clone());
        } else {
            tolerated_failures.push(result.clone());
        }
    }

    let report = Py::dict([
        ("generated_at", Py::from(py::utc_isoformat())),
        (
            "suite",
            suite
                .get("suite")
                .cloned()
                .unwrap_or_else(|| Py::from("redteam")),
        ),
        (
            "suite_version",
            suite.get("version").cloned().unwrap_or(Py::Int(1)),
        ),
        ("max_severity_allowed", Py::from(max_allowed)),
        ("repo_root", Py::from(py::path_str(&repo_root))),
        ("suite_file", Py::from(py::path_str(&suite_path))),
        (
            "summary",
            Py::dict([
                ("total", Py::from(results.len())),
                ("passed", Py::from(passed)),
                ("failed", Py::from(failed)),
                ("failed_blocking", Py::from(blocking_failures.len())),
                ("failed_tolerated", Py::from(tolerated_failures.len())),
                ("ok", Py::from(blocking_failures.is_empty())),
            ]),
        ),
        ("results", Py::List(results.clone())),
        ("blocking_failures", Py::List(blocking_failures.clone())),
        ("tolerated_failures", Py::List(tolerated_failures.clone())),
    ]);

    let report_path: PathBuf = if !args.report_path.is_empty() {
        let p = Path::new(&args.report_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            py::join(&repo_root, &args.report_path)
        }
    } else {
        let out_dir = repo_root.join(".sync-reports");
        fs::create_dir_all(&out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
        out_dir.join(format!("redteam-gate-{}.json", py::utc_stamp()))
    };

    write_json_pretty(&report_path, &report, true)?;

    println!("[redteam-gate] Report: {}", py::path_str(&report_path));
    println!(
        "[redteam-gate] Summary: total={} passed={} failed={} blocking={} tolerated={} max_allowed={}",
        results.len(),
        passed,
        failed,
        blocking_failures.len(),
        tolerated_failures.len(),
        max_allowed
    );

    Ok(i32::from(!blocking_failures.is_empty()))
}
