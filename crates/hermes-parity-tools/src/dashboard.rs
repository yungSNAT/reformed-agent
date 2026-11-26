//! Render the parity dashboard from the published evidence files.

use std::fs;
use std::path::Path;

use clap::Args;

use crate::decimal_limit;
use crate::py::{self, Py};
use crate::report_io::{resolve_argument, truth, write_report};

const PARITY_MATRIX: &str = "docs/parity/parity-matrix.json";
const WORKSTREAMS: &str = "docs/parity/workstream-status.json";
const QUEUE: &str = "docs/parity/upstream-missing-queue.json";
const GLOBAL_PROOF: &str = "docs/parity/global-parity-proof.json";
const COVERAGE: &str = "docs/parity/test-coverage-audit.json";
const HARNESS: &str = "docs/parity/sota-harness-matrix.json";
const BEHAVIOURAL: &str = "docs/parity/behavioral-similarity-diff.json";
const DEEP_PROOF: &str = "docs/parity/deep-problem-solving-diff.json";

#[derive(Args, Debug)]
pub struct DashboardArgs {
    #[arg(long, default_value = ".")]
    repo_root: String,
    #[arg(long, default_value = PARITY_MATRIX)]
    parity_matrix: String,
    #[arg(long, default_value = WORKSTREAMS)]
    workstream_status: String,
    #[arg(long, default_value = QUEUE)]
    queue_json: String,
    #[arg(long, default_value = GLOBAL_PROOF)]
    proof_json: String,
    #[arg(long, default_value = COVERAGE)]
    test_coverage_audit: String,
    #[arg(long, default_value = HARNESS)]
    sota_harness_matrix: String,
    #[arg(long, default_value = BEHAVIOURAL)]
    behavioral_diff: String,
    #[arg(long, default_value = DEEP_PROOF)]
    deep_problem_solving_diff: String,
    #[arg(long, default_value = "docs/parity/PARITY_DASHBOARD.md")]
    output: String,
}

struct Sources {
    matrix: Py,
    streams: Py,
    queue: Py,
    proof: Py,
    coverage: Py,
    harness: Py,
    behavioural: Py,
    deep: Py,
}

fn load(root: &Path, name: &str) -> Result<Py, String> {
    let path = resolve_argument(&py::join(root, name))?;
    Ok(fs::read_to_string(path)
        .ok()
        .and_then(|source| py::loads(&source).ok())
        .filter(Py::is_dict)
        .unwrap_or(Py::Null))
}

fn text(value: Option<&Py>, default: &str) -> String {
    value.map(py::str_of).unwrap_or_else(|| default.to_string())
}

fn snapshot(value: &Py, key: &str) -> String {
    text(value.get(key).filter(|value| truth(value)), "unknown")
}

fn child<'a>(value: &'a Py, key: &str) -> &'a Py {
    value.get(key).unwrap_or(&Py::Null)
}

fn gate_status(gate: &Py) -> &'static str {
    match gate.get("pass") {
        Some(Py::Bool(true)) => "PASS",
        Some(Py::Bool(false)) => "FAIL",
        _ => "UNKNOWN",
    }
}

fn checks(gate: &Py, target: &str) -> String {
    let Some(Py::List(checks)) = gate.get("checks") else {
        return "none".to_string();
    };
    let matches: Vec<_> = checks
        .iter()
        .filter(|check| {
            check.is_dict() && text(check.get("status"), "unknown").to_lowercase() == target
        })
        .map(|check| {
            format!(
                "{} (actual={}, limit={})",
                text(check.get("metric"), "unknown_metric"),
                text(check.get("actual"), "n/a"),
                text(check.get("limit"), "n/a"),
            )
        })
        .collect();
    if matches.is_empty() {
        "none".to_string()
    } else {
        matches.join("; ")
    }
}

fn integer(value: &Py) -> Result<String, String> {
    if !truth(value) {
        return Ok("0".to_string());
    }
    match value {
        Py::Bool(value) => Ok(usize::from(*value).to_string()),
        Py::Int(value) => Ok(value.to_string()),
        Py::Float(value) if value.is_finite() => {
            let integer = value.trunc();
            Ok(if integer == 0.0 {
                "0".to_string()
            } else {
                format!("{integer:.0}")
            })
        }
        Py::Str(value) => decimal_limit::parse(value).map(|number| number.label),
        _ => Err("expected an integer dashboard metric".to_string()),
    }
}

fn fraction(value: &Py) -> Result<String, String> {
    if !truth(value) {
        return Ok("0.0000".to_string());
    }
    let number = match value {
        Py::Bool(value) => f64::from(u8::from(*value)),
        Py::Int(value) => *value as f64,
        Py::Float(value) => *value,
        Py::Str(value) => decimal_limit::parse_float(value).map_err(|error| match error {
            "invalid decimal separator" => "invalid decimal separator in dashboard metric",
            _ => "expected a decimal dashboard metric",
        })?,
        _ => return Err("expected a decimal dashboard metric".to_string()),
    };
    Ok(if number.is_finite() {
        format!("{number:.4}")
    } else {
        py::float_repr(number).to_lowercase()
    })
}

fn table(lines: &mut Vec<String>, heading: &str, columns: &str, rules: &str) {
    lines.extend([
        String::new(),
        format!("## {heading}"),
        String::new(),
        columns.into(),
        rules.into(),
    ]);
}

fn metric_table(lines: &mut Vec<String>, heading: &str) {
    table(lines, heading, "| Metric | Value |", "| --- | ---: |");
}

fn metrics(
    lines: &mut Vec<String>,
    source: &Py,
    fields: &[(&str, &str)],
    ratio_key: &str,
) -> Result<(), String> {
    for (title, key) in fields {
        let value = child(source, key);
        let formatted = if *key == ratio_key {
            fraction(value)?
        } else {
            integer(value)?
        };
        lines.push(format!("| {title} | {formatted} |"));
    }
    Ok(())
}

fn comparison_table(
    lines: &mut Vec<String>,
    heading: &str,
    summary: &Py,
    ratio_key: &str,
    ratio_title: &str,
) -> Result<(), String> {
    metric_table(lines, heading);
    metrics(
        lines,
        summary,
        &[
            ("Total cases", "total_cases"),
            ("Equal or better cases", "equal_or_better_cases"),
            ("Superior cases", "superior_cases"),
            (ratio_title, ratio_key),
            ("Regressions", "regressions"),
            ("Gaps", "gaps"),
            ("Unverified cases", "unverified_cases"),
            ("Missing Rust test refs", "missing_rust_test_refs"),
        ],
        ratio_key,
    )
}

fn render(sources: &Sources) -> Result<String, String> {
    let workstream_time = snapshot(&sources.streams, "generated_at_utc");
    let matrix_time = snapshot(&sources.matrix, "generated_at_utc");
    let queue_time = snapshot(&sources.queue, "generated_at_utc");
    let proof_time = snapshot(&sources.proof, "generated_at_utc");
    let derived_time = [&proof_time, &queue_time, &matrix_time, &workstream_time]
        .into_iter()
        .map(String::as_str)
        .find(|value| !value.is_empty() && *value != "unknown")
        .unwrap_or("unknown");
    let mut lines = vec![
        "# Parity Dashboard".into(),
        String::new(),
        format!("_Generated from source artifacts: `{derived_time}`_"),
        String::new(),
        "## Snapshot".into(),
        String::new(),
        format!(
            "- Upstream target: `{}` @ `{}`",
            snapshot(&sources.streams, "upstream_ref"),
            snapshot(&sources.streams, "upstream_sha")
        ),
        format!("- Workstream snapshot generated: `{workstream_time}`"),
        format!("- Parity matrix generated: `{matrix_time}`"),
        format!("- Queue snapshot generated: `{queue_time}`"),
        format!("- Proof snapshot generated: `{proof_time}`"),
        String::new(),
        "## Gate Status".into(),
        String::new(),
    ];
    let release_gate = child(&sources.proof, "release_gate");
    let ci_gate = child(&sources.proof, "ci_gate");
    let coverage_gate = child(&sources.coverage, "audit_gate");
    let harness_gate = child(&sources.harness, "gate");
    for (title, gate) in [
        ("Release gate", release_gate),
        ("CI/tree-drift gate", ci_gate),
        ("Test coverage audit", coverage_gate),
        ("SOTA harness matrix", harness_gate),
        (
            "Behavioral similarity diff",
            child(&sources.behavioural, "gate"),
        ),
        ("Deep problem-solving diff", child(&sources.deep, "gate")),
    ] {
        lines.push(format!("- {title}: **{}**", gate_status(gate)));
    }
    for (title, gate, state) in [
        ("Release gate failures", release_gate, "fail"),
        ("CI gate failures", ci_gate, "fail"),
        ("CI gate warnings", ci_gate, "warn"),
    ] {
        lines.push(format!("- {title}: {}", checks(gate, state)));
    }

    metric_table(&mut lines, "Test Coverage Audit");
    metrics(
        &mut lines,
        child(&sources.coverage, "summary"),
        &[
            ("Tracked behavior rows", "tracked_behavior_rows"),
            ("Covered behavior rows", "covered_behavior_rows"),
            (
                "Tracked behavior coverage ratio",
                "tracked_behavior_coverage_ratio",
            ),
            ("Missing Rust test refs", "missing_rust_test_refs"),
        ],
        "tracked_behavior_coverage_ratio",
    )?;
    metrics(
        &mut lines,
        coverage_gate,
        &[("Critical gaps", "critical_gaps")],
        "",
    )?;

    metric_table(&mut lines, "SOTA Harness Matrix");
    metrics(
        &mut lines,
        child(&sources.harness, "summary"),
        &[
            ("Domains total", "domains_total"),
            ("Domains passing", "domains_passing"),
            ("Domain coverage ratio", "domain_coverage_ratio"),
            ("Direct Rust tests", "direct_rust_tests"),
        ],
        "domain_coverage_ratio",
    )?;
    metrics(
        &mut lines,
        harness_gate,
        &[
            ("Critical gaps", "critical_gaps"),
            ("Missing Rust test refs", "missing_rust_test_refs"),
        ],
        "",
    )?;

    comparison_table(
        &mut lines,
        "Behavioral Similarity Diff",
        child(&sources.behavioural, "summary"),
        "behavioral_similarity_ratio",
        "Similarity ratio",
    )?;
    comparison_table(
        &mut lines,
        "Deep Problem-Solving Diff",
        child(&sources.deep, "summary"),
        "deep_problem_solving_ratio",
        "Deep problem-solving ratio",
    )?;

    let queue_summary = child(&sources.queue, "summary");
    metric_table(&mut lines, "Queue Summary");
    metrics(
        &mut lines,
        queue_summary,
        &[("Total commits in queue", "total_commits")],
        "",
    )?;
    metrics(
        &mut lines,
        child(queue_summary, "by_disposition"),
        &[
            ("Pending", "pending"),
            ("Ported", "ported"),
            ("Superseded", "superseded"),
        ],
        "",
    )?;
    let matrix_summary = child(&sources.matrix, "summary");
    metric_table(&mut lines, "Tree/Patch Drift");
    for key in [
        "commits_behind",
        "commits_ahead",
        "upstream_patch_missing",
        "upstream_patch_represented",
        "local_patch_unique",
        "files_only_upstream",
        "files_only_local",
        "files_shared_identical",
        "files_shared_different",
    ] {
        lines.push(format!(
            "| {key} | {} |",
            integer(child(matrix_summary, key))?
        ));
    }

    table(
        &mut lines,
        "Workstream States",
        "| State | Count |",
        "| --- | ---: |",
    );
    if let Py::Dict(states) = child(&sources.streams, "states") {
        let mut states: Vec<_> = states.iter().collect();
        states.sort_by(|left, right| left.0.cmp(&right.0));
        for (state, count) in states {
            lines.push(format!("| {state} | {} |", integer(count)?));
        }
    }
    if let Py::List(streams) = child(&sources.streams, "workstreams") {
        if !streams.is_empty() {
            table(
                &mut lines,
                "Workstream Detail",
                "| WS | Title | State |",
                "| --- | --- | --- |",
            );
            for stream in streams.iter().filter(|stream| stream.is_dict()) {
                lines.push(format!(
                    "| {} | {} | {} |",
                    text(stream.get("workstream"), "?"),
                    text(stream.get("title"), ""),
                    text(stream.get("state"), ""),
                ));
            }
        }
    }
    lines.extend([String::new(), "## Source Artifacts".into(), String::new()]);
    for artifact in [
        PARITY_MATRIX,
        WORKSTREAMS,
        QUEUE,
        GLOBAL_PROOF,
        COVERAGE,
        HARNESS,
        BEHAVIOURAL,
        DEEP_PROOF,
    ] {
        lines.push(format!("- `{artifact}`"));
    }
    lines.push(String::new());
    Ok(lines.join("\n") + "\n")
}

pub fn run(args: DashboardArgs) -> Result<i32, String> {
    let root = resolve_argument(&py::expanduser(&args.repo_root)?)?;
    let sources = Sources {
        matrix: load(&root, &args.parity_matrix)?,
        streams: load(&root, &args.workstream_status)?,
        queue: load(&root, &args.queue_json)?,
        proof: load(&root, &args.proof_json)?,
        coverage: load(&root, &args.test_coverage_audit)?,
        harness: load(&root, &args.sota_harness_matrix)?,
        behavioural: load(&root, &args.behavioral_diff)?,
        deep: load(&root, &args.deep_problem_solving_diff)?,
    };
    let output = resolve_argument(&py::join(&root, &args.output))?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", output.display()))?;
    }
    write_report(&output, render(&sources)?)?;
    println!("Wrote parity dashboard: {}", py::path_str(&output));
    Ok(0)
}

#[cfg(test)]
#[path = "dashboard_coverage_gen.rs"]
mod coverage_gen;
