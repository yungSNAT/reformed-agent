use std::fmt::Write;

use crate::py::{self, Py};

use super::{as_list, field_text, Evidence, ProofKind};

const METRICS: [&str; 10] = [
    "total_cases",
    "equal_or_better_cases",
    "superior_cases",
    "equivalent_cases",
    "intentional_divergence_cases",
    "regressions",
    "gaps",
    "unverified_cases",
    "missing_rust_test_refs",
    "missing_source_artifacts",
];

pub(super) fn render(
    kind: &ProofKind,
    generated: &str,
    evidence: &Evidence,
    cases: &[Py],
) -> String {
    let summary = &evidence.summary;
    let gate = if evidence.failures.is_empty() {
        "PASS"
    } else {
        "FAIL"
    };
    let title = kind.report_title;
    let mut output =
        format!("# {title}\n\nGenerated: `{generated}`\n\n## Gate\n\n- Gate: **{gate}**\n");
    for (title, key) in [
        (kind.ratio_title, kind.ratio_key),
        ("Superior cases", "superior_cases"),
        ("Regressions", "regressions"),
        ("Gaps", "gaps"),
        ("Unverified cases", "unverified_cases"),
        ("Missing Rust test refs", "missing_rust_test_refs"),
    ] {
        writeln!(output, "- {title}: `{}`", field_text(summary, key)).expect("string write");
    }
    output.push_str("\n## Summary\n\n| Metric | Value |\n| --- | ---: |\n");
    for key in METRICS.into_iter().chain([kind.ratio_key]) {
        writeln!(output, "| `{key}` | {} |", field_text(summary, key)).expect("string write");
    }

    for (title, label, key) in [
        (
            "Classification Counts",
            "Classification",
            "classification_counts",
        ),
        ("Domain Counts", "Domain", "domain_counts"),
    ] {
        writeln!(
            output,
            "\n## {title}\n\n| {label} | Count |\n| --- | ---: |"
        )
        .expect("string write");
        if let Some(Py::Dict(counts)) = summary.get(key) {
            for (name, count) in counts {
                writeln!(output, "| `{name}` | {} |", py::str_of(count)).expect("string write");
            }
        }
    }
    output.push_str(
        "\n## Cases\n\n| Case | Domain | Classification | Score |\n| --- | --- | --- | ---: |\n",
    );
    for case in cases {
        writeln!(
            output,
            "| `{}` | `{}` | `{}` | {} |",
            field_text(case, "id"),
            field_text(case, "domain"),
            field_text(case, "classification"),
            field_text(case, "score"),
        )
        .expect("string write");
    }
    output.push_str("\n## Gate Failures\n\n");
    if evidence.failures.is_empty() {
        output.push_str("- none\n");
    } else {
        for failure in &evidence.failures {
            writeln!(
                output,
                "- `{}`: {}",
                field_text(failure, "kind"),
                field_text(failure, "message")
            )
            .expect("string write");
        }
    }

    output.push_str("\n## Case Issues\n\n");
    let mut issues = cases
        .iter()
        .flat_map(|case| as_list(case.get("issues")))
        .peekable();
    if issues.peek().is_none() {
        output.push_str("- none\n");
    } else {
        for issue in issues {
            writeln!(
                output,
                "- `{}` `{}`: {}",
                field_text(issue, "case_id"),
                field_text(issue, "kind"),
                field_text(issue, "message"),
            )
            .expect("string write");
        }
    }
    output.push('\n');
    output
}
