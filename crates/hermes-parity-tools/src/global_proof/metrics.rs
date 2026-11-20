use std::collections::BTreeSet;
use std::path::Path;

use crate::decimal_limit;
use crate::py::{self, Py};

use super::{get, number, read_json, truth, Metrics, Reports};

fn value(report: &Py, key: &str) -> Result<f64, String> {
    report.get(key).map_or(Ok(0.0), number)
}

fn count(report: &Py, key: &str) -> Result<f64, String> {
    match report.get(key) {
        None => Ok(0.0),
        Some(Py::Str(text)) => decimal_limit::parse(text).and_then(|limit| {
            limit
                .label
                .parse()
                .map_err(|_| format!("invalid integer {text:?}"))
        }),
        Some(value @ (Py::Int(_) | Py::Bool(_))) => number(value),
        Some(Py::Float(value)) if value.is_finite() => Ok(value.trunc()),
        _ => Err(format!("{key} must be convertible to an integer")),
    }
}

fn required<'a>(report: &'a Py, key: &str) -> Result<&'a Py, String> {
    report
        .get(key)
        .ok_or_else(|| format!("report is missing {key}"))
}

fn required_number(report: &Py, key: &str) -> Result<f64, String> {
    number(required(report, key)?)
}

#[derive(Clone, Copy)]
enum RowCount {
    Integer(i128),
    Float(f64),
}

impl RowCount {
    fn read(report: &Py, key: &str) -> Result<Self, String> {
        match required(report, key)? {
            Py::Int(value) => Ok(Self::Integer(*value)),
            Py::Bool(value) => Ok(Self::Integer(i128::from(*value))),
            Py::Float(value) => Ok(Self::Float(*value)),
            _ => Err(format!("{key} must be numeric")),
        }
    }

    fn subtract(self, other: Self) -> Result<Self, String> {
        match (self, other) {
            (Self::Integer(left), Self::Integer(right)) => left
                .checked_sub(right)
                .map(Self::Integer)
                .ok_or_else(|| "coverage count exceeds the integer range".to_owned()),
            _ => Ok(Self::Float(self.as_float() - other.as_float())),
        }
    }

    fn as_float(self) -> f64 {
        match self {
            Self::Integer(value) => value as f64,
            Self::Float(value) => value,
        }
    }
}

fn required_list<'a>(report: &'a Py, key: &str) -> Result<&'a [Py], String> {
    match required(report, key)? {
        Py::List(items) => Ok(items),
        _ => Err(format!("{key} must contain a list")),
    }
}

fn evidence_rows<'a>(report: &'a Py, key: &str) -> Result<&'a [Py], String> {
    match report.get(key) {
        None => Ok(&[]),
        Some(Py::List(rows)) if rows.iter().all(Py::is_dict) => Ok(rows),
        Some(Py::Dict(rows)) if rows.is_empty() => Ok(&[]),
        Some(Py::Str(rows)) if rows.is_empty() => Ok(&[]),
        _ => Err(format!("{key} must contain a list of evidence objects")),
    }
}

fn required_text<'a>(report: &'a Py, key: &str) -> Result<&'a str, String> {
    match required(report, key)? {
        Py::Str(text) => Ok(text),
        _ => Err(format!("{key} must contain text")),
    }
}

fn is_text(value: Option<&Py>, expected: &str) -> bool {
    matches!(value, Some(Py::Str(text)) if text == expected)
}

fn passed(report: &Py, key: &str) -> bool {
    get(report, key).get("pass").is_some_and(truth)
}

fn path_set(rows: &[Py]) -> BTreeSet<String> {
    rows.iter()
        .map(|row| row.get("path").map_or_else(String::new, py::str_of))
        .collect()
}

pub(super) fn workstream_completion(reports: &Reports) -> Result<Py, String> {
    let intents = evidence_rows(&reports.intents, "intents")?;
    let mapped = |id| {
        intents
            .iter()
            .any(|row| is_text(row.get("id"), id) && row.get("mapped").is_some_and(truth))
    };
    let ux_divergence = evidence_rows(&reports.parity, "intentional_divergence")?
        .iter()
        .any(|row| is_text(row.get("id"), "rust-cli-tui-primary-ux-surface"));
    let shared_classification = path_set(evidence_rows(&reports.shared_classification, "items")?);
    let shared_different = path_set(evidence_rows(&reports.parity, "top_shared_different")?);
    let divergences = get(&reports.divergence, "summary");
    let divergences_clear = count(divergences, "errors")? == 0.0
        && count(divergences, "unowned")? == 0.0
        && count(divergences, "review_overdue")? == 0.0;
    let complete = [
        value(get(&reports.intents, "summary"), "mapping_ratio")? >= 0.9,
        mapped("skills-management-contract"),
        ux_divergence,
        count(get(&reports.adapters, "summary"), "non_rust_native")? == 0.0,
        mapped("environment-lifecycle-contract") && mapped("tool-call-parser-contract"),
        shared_different.is_subset(&shared_classification),
        count(get(&reports.queue, "summary"), "total_commits")? > 0.0,
        divergences_clear,
        passed(&reports.coverage, "audit_gate"),
        passed(&reports.behavioral, "gate"),
        passed(&reports.deep, "gate"),
    ];
    Ok(Py::dict(complete.into_iter().enumerate().map(
        |(index, result)| (format!("GPAR-{:02}", index + 1), result.into()),
    )))
}

fn overdue_rows(root: &Path, reports: &Reports) -> Result<f64, String> {
    let now = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    let today = now.date().to_string();
    let due =
        |row: &Py| required_text(row, "review_date").map(|date| usize::from(date < today.as_str()));
    let mut overdue = 0;
    for row in required_list(&reports.shared_classification, "items")? {
        if is_text(Some(required(row, "classification")?), "deferred") {
            overdue += due(row)?;
        }
    }
    for manifest in required_list(&reports.coverage, "coverage_manifests")? {
        let data = read_json(root, required_text(manifest, "path")?)?;
        for row in required_list(&data, "entries")? {
            if is_text(Some(required(row, "status")?), "deferred_not_covered") {
                overdue += due(row)?;
            }
        }
    }
    Ok(overdue as f64)
}

pub(super) fn calculate(root: &Path, reports: &Reports) -> Result<Metrics, String> {
    let parity = get(&reports.parity, "summary");
    let divergence = get(&reports.divergence, "summary");
    let intents = get(&reports.intents, "summary");
    let coverage = get(&reports.coverage, "summary");
    let coverage_gate = get(&reports.coverage, "audit_gate");
    let harness = get(&reports.harness, "summary");
    let harness_gate = get(&reports.harness, "gate");
    let behavioral = get(&reports.behavioral, "summary");
    let deep = get(&reports.deep, "summary");
    let dispositions = get(get(&reports.queue, "summary"), "by_disposition");
    let not_rechecked = RowCount::read(coverage, "coverage_manifest_not_rechecked_entries")?;
    // Deferred and not rechecked rows are tracked separately by parity governance.
    let unaccounted = RowCount::read(coverage, "tracked_behavior_rows")?
        .subtract(RowCount::read(coverage, "covered_behavior_rows")?)?
        .subtract(RowCount::read(
            coverage,
            "coverage_manifest_deferred_entries",
        )?)?
        .subtract(not_rechecked)?
        .as_float();
    let deferred_shared = required_number(
        required(&reports.shared_backlog, "summary")?,
        "deferred_review",
    )?;

    Ok(vec![
        ("max_commits_behind", value(parity, "commits_behind")?),
        (
            "max_upstream_patch_missing",
            value(parity, "upstream_patch_missing")?,
        ),
        (
            "max_files_only_upstream",
            value(parity, "files_only_upstream")?,
        ),
        ("max_unowned_divergences", value(divergence, "unowned")?),
        (
            "max_divergence_review_overdue",
            value(divergence, "review_overdue")?,
        ),
        (
            "min_test_intent_mapping_ratio",
            value(intents, "mapping_ratio")?,
        ),
        (
            "min_test_coverage_tracked_behavior_ratio",
            value(coverage, "tracked_behavior_coverage_ratio")?,
        ),
        (
            "max_test_coverage_audit_critical_gaps",
            value(coverage_gate, "critical_gaps")?,
        ),
        ("max_test_coverage_unaccounted_rows", unaccounted),
        ("max_shared_diff_deferred_rows", deferred_shared),
        (
            "max_test_coverage_not_rechecked_rows",
            not_rechecked.as_float(),
        ),
        ("max_deferred_review_overdue", overdue_rows(root, reports)?),
        (
            "max_test_coverage_missing_rust_refs",
            value(coverage, "missing_rust_test_refs")?,
        ),
        (
            "min_sota_harness_domain_coverage_ratio",
            value(harness, "domain_coverage_ratio")?,
        ),
        (
            "max_sota_harness_critical_gaps",
            value(harness_gate, "critical_gaps")?,
        ),
        (
            "max_sota_harness_missing_rust_refs",
            value(harness_gate, "missing_rust_test_refs")?,
        ),
        (
            "min_behavioral_similarity_ratio",
            value(behavioral, "behavioral_similarity_ratio")?,
        ),
        (
            "min_behavioral_superiority_cases",
            value(behavioral, "superior_cases")?,
        ),
        (
            "max_behavioral_regressions",
            value(behavioral, "regressions")?,
        ),
        ("max_behavioral_gaps", value(behavioral, "gaps")?),
        (
            "max_behavioral_unverified_cases",
            value(behavioral, "unverified_cases")?,
        ),
        (
            "max_behavioral_missing_rust_refs",
            value(behavioral, "missing_rust_test_refs")?,
        ),
        (
            "min_deep_problem_solving_ratio",
            value(deep, "deep_problem_solving_ratio")?,
        ),
        (
            "min_deep_problem_solving_superiority_cases",
            value(deep, "superior_cases")?,
        ),
        (
            "min_deep_problem_solving_total_cases",
            value(deep, "total_cases")?,
        ),
        (
            "max_deep_problem_solving_regressions",
            value(deep, "regressions")?,
        ),
        ("max_deep_problem_solving_gaps", value(deep, "gaps")?),
        (
            "max_deep_problem_solving_unverified_cases",
            value(deep, "unverified_cases")?,
        ),
        (
            "max_deep_problem_solving_missing_rust_refs",
            value(deep, "missing_rust_test_refs")?,
        ),
        ("max_queue_pending_commits", value(dispositions, "pending")?),
    ])
}
