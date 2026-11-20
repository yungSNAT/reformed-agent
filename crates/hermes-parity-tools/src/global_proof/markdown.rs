use std::fmt::Write as _;

use crate::py::{self, Py};

use super::{entries, get, truth};

fn status(gate: &Py) -> &'static str {
    if gate.get("pass").is_some_and(truth) {
        "PASS"
    } else {
        "FAIL"
    }
}

fn display(report: &Py, key: &str) -> String {
    report.get(key).map_or_else(|| "0".into(), py::str_of)
}

fn line(report: &mut String, label: &str, data: &Py, key: &str) {
    let _ = writeln!(report, "- {label}: `{}`", display(data, key));
}

fn comparison(
    out: &mut String,
    payload: &Py,
    section: &str,
    heading: &str,
    label: &str,
    ratio: &str,
    key: &str,
) {
    let data = get(payload, section);
    let summary = get(data, "summary");
    let _ = writeln!(
        out,
        "\n## {heading}\n\n- {label}: **{}**",
        status(get(data, "gate"))
    );
    line(out, ratio, summary, key);
    for (label, field) in [
        ("Superior cases", "superior_cases"),
        ("Regressions", "regressions"),
        ("Gaps", "gaps"),
        ("Unverified cases", "unverified_cases"),
        ("Missing Rust test refs", "missing_rust_test_refs"),
    ] {
        line(out, label, summary, field);
    }
}

pub(super) fn render(payload: &Py) -> String {
    let timestamp = display(payload, "generated_at_utc");
    let mut out =
        format!("# Global Parity Proof\n\nGenerated: `{timestamp}`\n\n## Gate Status\n\n");
    for (label, section) in [("CI", "ci_gate"), ("Release", "release_gate")] {
        let gate = get(payload, section);
        let _ = writeln!(out, "- {label} gate: **{}**", status(gate));
        line(&mut out, &format!("{label} gate mode"), gate, "mode");
    }
    out.push_str("\n## Metrics\n\n| Metric | Value |\n| --- | ---: |\n");
    for (metric, value) in entries(get(payload, "metrics")) {
        let _ = writeln!(out, "| `{metric}` | {} |", py::str_of(value));
    }
    out.push_str("\n## GPAR Ticket Completion\n\n| Ticket | Complete |\n| --- | --- |\n");
    for (ticket, completed) in entries(get(payload, "gpar_completion")) {
        let _ = writeln!(
            out,
            "| `{ticket}` | {} |",
            if truth(completed) { "yes" } else { "no" }
        );
    }

    let coverage = get(payload, "test_coverage_audit_summary");
    let summary = get(coverage, "summary");
    let gate = get(coverage, "audit_gate");
    let _ = writeln!(
        out,
        "\n## Test Coverage Audit\n\n- Audit gate: **{}**",
        status(gate)
    );
    line(
        &mut out,
        "Tracked behavior coverage ratio",
        summary,
        "tracked_behavior_coverage_ratio",
    );
    line(&mut out, "Critical gaps", gate, "critical_gaps");
    line(
        &mut out,
        "Missing Rust test refs",
        summary,
        "missing_rust_test_refs",
    );

    let harness = get(payload, "sota_harness_summary");
    let summary = get(harness, "summary");
    let gate = get(harness, "gate");
    let _ = writeln!(
        out,
        "\n## SOTA Harness Matrix\n\n- Harness gate: **{}**",
        status(gate)
    );
    line(
        &mut out,
        "Domain coverage ratio",
        summary,
        "domain_coverage_ratio",
    );
    line(&mut out, "Critical gaps", gate, "critical_gaps");
    line(
        &mut out,
        "Missing Rust test refs",
        gate,
        "missing_rust_test_refs",
    );
    line(&mut out, "Direct Rust tests", summary, "direct_rust_tests");

    comparison(
        &mut out,
        payload,
        "behavioral_diff_summary",
        "Behavioral Similarity Diff",
        "Behavioral gate",
        "Similarity ratio",
        "behavioral_similarity_ratio",
    );
    comparison(
        &mut out,
        payload,
        "deep_problem_solving_summary",
        "Deep Problem-Solving Diff",
        "Deep problem-solving gate",
        "Deep problem-solving ratio",
        "deep_problem_solving_ratio",
    );

    let queue = get(payload, "queue_summary");
    let _ = writeln!(
        out,
        "\n## Queue Summary\n\n- Upstream missing commits tracked: `{}`.",
        display(queue, "total_commits")
    );
    let by_ticket = get(queue, "by_target_ticket");
    if by_ticket.is_dict() {
        out.push_str("- By target ticket:\n");
        let mut tickets: Vec<_> = entries(by_ticket).iter().collect();
        tickets.sort_by(|left, right| left.0.cmp(&right.0));
        for (ticket, count) in tickets {
            let _ = writeln!(out, "  - `#{ticket}`: `{}`", py::str_of(count));
        }
    }
    out.push('\n');
    out
}
