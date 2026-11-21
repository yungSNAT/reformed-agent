//! Summarise the existing public parity artifacts for a release decision.

use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use clap::Args;
use regex::Regex;

use crate::decimal_limit;
use crate::git::git;
use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_report};

const SOURCES: [(&str, &str); 6] = [
    (
        "global_parity_proof",
        "docs/parity/global-parity-proof.json",
    ),
    (
        "upstream_missing_queue",
        "docs/parity/upstream-missing-queue.json",
    ),
    (
        "shared_diff_backlog",
        "docs/parity/shared-diff-backlog.json",
    ),
    (
        "test_coverage_audit",
        "docs/parity/test-coverage-audit.json",
    ),
    (
        "sota_harness_matrix",
        "docs/parity/sota-harness-matrix.json",
    ),
    (
        "global_parity_thresholds",
        "docs/parity/global-parity-thresholds.json",
    ),
];

#[derive(Args, Debug)]
pub struct ReadinessArgs {
    #[arg(long, default_value = ".")]
    repo_root: String,
    #[arg(long, default_value = ".sync-reports/release-readiness-summary.json")]
    output_json: String,
    #[arg(long, default_value = ".sync-reports/release-readiness-summary.md")]
    output_md: String,
    /// Exit nonzero if release readiness gates fail.
    #[arg(long)]
    check: bool,
}

fn read_artifact(root: &Path, name: &str) -> Result<Py, String> {
    match fs::read_to_string(root.join(name)) {
        Ok(text) => py::loads(&text).map_err(|error| format!("{name}: {error}")),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Py::dict::<&str>([])),
        Err(error) => Err(format!("{name}: {error}")),
    }
}

fn metric<'a>(payload: &'a Py, keys: &[&str]) -> Option<&'a Py> {
    keys.iter().try_fold(payload, |value, key| value.get(key))
}

fn integer(value: Option<&Py>) -> Result<i128, String> {
    match value {
        Some(Py::Int(value)) => Ok(*value),
        Some(Py::Bool(value)) => Ok(i128::from(*value)),
        Some(Py::Float(value)) if value.is_nan() => Ok(0),
        Some(Py::Float(value)) if (-2_f64.powi(127)..2_f64.powi(127)).contains(value) => {
            Ok(*value as i128)
        }
        Some(Py::Float(_)) => Err("integer metric exceeds the supported range".into()),
        Some(Py::Str(value)) => decimal_limit::parse(value).map_or(Ok(0), |limit| {
            limit
                .label
                .parse()
                .map_err(|_| "integer metric exceeds the supported range".into())
        }),
        _ => Ok(0),
    }
}

fn truth(value: Option<&Py>) -> bool {
    match value {
        None | Some(Py::Null | Py::Bool(false)) => false,
        Some(Py::Bool(true)) => true,
        Some(Py::Int(value)) => *value != 0,
        Some(Py::Float(value)) => *value != 0.0,
        Some(Py::Str(value)) => !value.is_empty(),
        Some(Py::List(value)) => !value.is_empty(),
        Some(Py::Dict(value)) => !value.is_empty(),
    }
}

fn state(pass: bool) -> &'static str {
    if pass {
        "PASS"
    } else {
        "FAIL"
    }
}

fn git_output(root: &Path, args: &[&str]) -> Result<String, String> {
    let result = git(root, args)?;
    if result.ok {
        Ok(py::strip(&result.stdout).to_owned())
    } else {
        Ok(String::new())
    }
}

fn workspace_version(root: &Path) -> Result<String, String> {
    static VERSION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?m)^version = "([^"]+)""#).unwrap());
    let text = match fs::read_to_string(root.join("Cargo.toml")) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok("unknown".into()),
        Err(error) => return Err(format!("Cargo.toml: {error}")),
    };
    Ok(VERSION
        .captures(&text)
        .map_or("unknown", |captures| captures.get(1).unwrap().as_str())
        .into())
}

fn upstream_watch(
    proof: &Py,
    thresholds: &Py,
    pending: i128,
    release_pass: bool,
) -> Result<Py, String> {
    if !proof.is_dict() || !thresholds.is_dict() {
        return Err("the proof and thresholds must contain JSON objects".into());
    }
    let mut alerts = Vec::new();
    for (field, threshold) in [
        ("max_commits_behind", "warn_commits_behind"),
        ("max_upstream_patch_missing", "warn_upstream_patch_missing"),
        ("max_files_only_upstream", "warn_files_only_upstream"),
    ] {
        let actual = integer(metric(proof, &["metrics", field]))?;
        let limit = integer(metric(thresholds, &["drift_alert_thresholds", threshold]))?;
        if limit > 0 && actual > limit {
            alerts.push(Py::dict([
                ("metric", field.into()),
                ("actual", Py::Int(actual)),
                ("warn_threshold", Py::Int(limit)),
                ("status", "watch".into()),
            ]));
        }
    }
    let ci_pass = truth(metric(proof, &["ci_gate", "pass"]));
    let pass = pending == 0 && release_pass;
    let status = if pass && (!ci_pass || !alerts.is_empty()) {
        "WATCH"
    } else {
        state(pass)
    };
    Ok(Py::dict([
        ("schema_version", Py::Int(1)),
        ("pass", pass.into()),
        ("status", status.into()),
        ("queue_pending", Py::Int(pending)),
        ("release_gate_pass", release_pass.into()),
        ("ci_gate_pass", ci_pass.into()),
        ("alerts", Py::List(alerts)),
        (
            "source_artifacts",
            Py::strs([SOURCES[0].1, SOURCES[1].1, SOURCES[5].1]),
        ),
    ]))
}

fn gate(id: &str, label: &str, actual: Py, limit: Option<Py>, pass: bool) -> Py {
    let mut gate = Py::dict([
        ("id", id.into()),
        ("label", label.into()),
        ("actual", actual),
        ("pass", pass.into()),
    ]);
    if let Some(limit) = limit {
        gate.set("limit", limit);
    }
    gate
}

fn build_summary(root: &Path) -> Result<(Py, bool), String> {
    let [proof, queue, shared, coverage, sota, thresholds] =
        SOURCES.map(|(_, name)| read_artifact(root, name));
    let (proof, queue, shared, coverage, sota, thresholds) =
        (proof?, queue?, shared?, coverage?, sota?, thresholds?);
    let pending = integer(metric(&queue, &["summary", "by_disposition", "pending"]))?;
    let release_pass = truth(metric(&proof, &["release_gate", "pass"]));
    let watch = upstream_watch(&proof, &thresholds, pending, release_pass)?;
    let watch_pass = truth(watch.get("pass"));
    let watch_status = watch.get("status").cloned().unwrap_or(Py::Null);
    let alert_count = match watch.get("alerts") {
        Some(Py::List(alerts)) => alerts.len(),
        _ => 0,
    };
    let count_checks = [
        ("queue_pending", "Upstream queue pending", pending),
        (
            "shared_pending_classification",
            "Shared diff pending classification",
            integer(metric(&shared, &["summary", "pending_classification"]))?,
        ),
        (
            "shared_pending_review",
            "Shared diff pending review",
            integer(metric(&shared, &["summary", "pending_review"]))?,
        ),
        (
            "coverage_critical_gaps",
            "Coverage critical gaps",
            integer(metric(&coverage, &["audit_gate", "critical_gaps"]))?,
        ),
        (
            "coverage_missing_rust_refs",
            "Coverage missing Rust refs",
            integer(metric(&coverage, &["summary", "missing_rust_test_refs"]))?,
        ),
        (
            "sota_critical_gaps",
            "SOTA harness critical gaps",
            integer(metric(&sota, &["gate", "critical_gaps"]))?,
        ),
        (
            "sota_missing_rust_refs",
            "SOTA harness missing Rust refs",
            integer(metric(&sota, &["gate", "missing_rust_test_refs"]))?,
        ),
    ];
    let mut checks = vec![gate(
        "release_gate",
        "Global release gate",
        state(release_pass).into(),
        None,
        release_pass,
    )];
    let mut summary = Py::dict::<&str>([]);
    for (id, label, count) in count_checks {
        summary.set(id, Py::Int(count));
        checks.push(gate(
            id,
            label,
            Py::Int(count),
            Some(Py::Int(0)),
            count == 0,
        ));
    }
    checks.push(gate(
        "upstream_delta_watch",
        "Upstream delta watch",
        watch_status.clone(),
        Some("PASS|WATCH".into()),
        watch_pass,
    ));
    for (key, value) in [
        ("release_gate_pass", release_pass.into()),
        ("upstream_delta_watch_pass", watch_pass.into()),
        ("upstream_delta_watch_status", watch_status),
        ("upstream_delta_watch_alerts", alert_count.into()),
    ] {
        summary.set(key, value);
    }

    let ok = checks.iter().all(|check| truth(check.get("pass")));
    Ok((
        Py::dict([
            ("schema_version", Py::Int(1)),
            ("workspace_version", workspace_version(root)?.into()),
            ("head", git_output(root, &["rev-parse", "HEAD"])?.into()),
            (
                "tag",
                git_output(root, &["describe", "--tags", "--exact-match", "HEAD"])?.into(),
            ),
            ("ok", ok.into()),
            ("checks", Py::List(checks)),
            (
                "source_artifacts",
                Py::dict(SOURCES.map(|(label, path)| (label, path.into()))),
            ),
            ("summary", summary),
            ("upstream_delta_watch", watch),
        ]),
        ok,
    ))
}

fn text(data: &Py, key: &str, fallback: &str) -> String {
    data.get(key).map_or_else(|| fallback.into(), py::str_of)
}

fn markdown(report: &Py) -> String {
    let status = state(truth(report.get("ok")));
    let version = text(report, "workspace_version", "unknown");
    let head: String = text(report, "head", "").chars().take(12).collect();
    let mut out = format!("# Hermes Agent Ultra Release Readiness Summary\n\nStatus: **{status}**\nWorkspace version: `{version}`\nHEAD: `{head}`\n");
    let tag = text(report, "tag", "");
    if !tag.is_empty() {
        let _ = writeln!(out, "Tag: `{tag}`");
    }
    out.push_str("\n## Gates\n\n| Gate | Actual | Limit | Status |\n| --- | ---: | ---: | --- |\n");
    if let Some(Py::List(checks)) = report.get("checks") {
        for check in checks {
            let _ = writeln!(
                out,
                "| {} | `{}` | `{}` | {} |",
                text(check, "label", ""),
                text(check, "actual", ""),
                text(check, "limit", "PASS"),
                state(truth(check.get("pass")))
            );
        }
    }
    out.push_str("\n## Source Artifacts\n\n");
    for (label, path) in SOURCES {
        let _ = writeln!(out, "- `{label}`: `{path}`");
    }
    out.push_str("\n## Upstream Delta Watch\n\n");
    if let Some(watch) = report.get("upstream_delta_watch") {
        let _ = writeln!(out, "- Status: `{}`\n- Queue pending: `{}`\n- CI gate pass: `{}`\n- Release gate pass: `{}`",
                         text(watch, "status", "unknown"), text(watch, "queue_pending", "0"),
                         text(watch, "ci_gate_pass", "False"), text(watch, "release_gate_pass", "False"));
        let count = match watch.get("alerts") {
            Some(Py::List(alerts)) => alerts.len(),
            _ => 0,
        };
        let _ = writeln!(out, "- Drift alerts: `{count}`");
    }
    out
}

pub fn run(args: ReadinessArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let (report, ok) = build_summary(&root)?;
    let [out_json, out_md] = [args.output_json, args.output_md]
        .map(|path| py::join(&root, &path).components().collect::<PathBuf>());
    for path in [&out_json, &out_md] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
    }
    write_report(&out_json, py::dumps(&report, Some(2), true) + "\n")?;
    write_report(&out_md, markdown(&report))?;
    println!("Wrote {}", py::path_str(&out_json));
    println!("Wrote {}", py::path_str(&out_md));
    println!("release_readiness={}", state(ok));
    Ok(i32::from(args.check && !ok))
}
