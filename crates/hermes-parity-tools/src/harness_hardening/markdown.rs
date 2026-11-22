use std::fmt::Write;

use super::{field, list, text};
use crate::py::Py;
use crate::report_io::truth;

fn heading(title: &str, payload: &Py) -> String {
    let generated = text(payload, "generated_at_utc", "");
    format!("# {title}\n\nGenerated: `{generated}`\n\n")
}

fn status(value: &Py) -> &'static str {
    if truth(value) {
        "PASS"
    } else {
        "FAIL"
    }
}

pub(super) fn trend(payload: &Py) -> Result<String, String> {
    let latest = field(payload, &["latest"]);
    let mut output = heading("Harness Trend Ledger", payload);
    output.push_str("## Latest\n\n| Metric | Value |\n| --- | ---: |\n");
    for key in [
        "queue_pending",
        "queue_total",
        "tracked_behavior_coverage_ratio",
        "tracked_behavior_rows",
        "covered_behavior_rows",
        "rust_test_functions",
        "coverage_manifest_entries",
        "sota_domain_coverage_ratio",
        "sota_direct_rust_tests",
        "sota_workflow_replay_steps",
        "sota_protocol_cases",
        "sota_fault_scenarios",
    ] {
        writeln!(output, "| `{key}` | {} |", text(latest, key, "0")).expect("string write");
    }
    output.push_str("\n## Entries\n\n| Recorded | Local head | Queue pending | Coverage | SOTA domains |\n| --- | --- | ---: | ---: | ---: |\n");
    for entry in list(field(payload, &["entries"])) {
        let head: String = text(entry, "local_head", "").chars().take(12).collect();
        writeln!(
            output,
            "| `{}` | `{head}` | {} | {} | {} |",
            text(entry, "recorded_at_utc", ""),
            text(entry, "queue_pending", "0"),
            text(entry, "tracked_behavior_coverage_ratio", "0"),
            text(entry, "sota_domain_coverage_ratio", "0"),
        )
        .expect("string write");
    }
    Ok(output)
}

fn group_size(group: &Py, key: &str) -> Result<usize, String> {
    match group.get(key) {
        None => Ok(0),
        Some(Py::List(items)) => Ok(items.len()),
        Some(Py::Dict(items)) => Ok(items.len()),
        Some(Py::Str(text)) => Ok(text.chars().count()),
        _ => Err(format!("replay evidence {key} does not have a length")),
    }
}

pub(super) fn replay(payload: &Py) -> Result<String, String> {
    let gate = field(payload, &["latest_gate_state"]);
    let mut output = heading("ContextLattice Replay Evidence Index", payload);
    output.push_str("## Gate State\n\n");
    for (title, key) in [
        ("Release", "release_gate_pass"),
        ("CI", "ci_gate_pass"),
        ("Test coverage", "test_coverage_gate_pass"),
        ("SOTA harness", "sota_harness_gate_pass"),
    ] {
        writeln!(
            output,
            "- {title} gate: **{}**",
            status(field(gate, &[key]))
        )
        .expect("string write");
    }
    writeln!(
        output,
        "- Queue pending: `{}`",
        text(gate, "queue_pending", "0")
    )
    .expect("string write");
    output.push_str("\n## Replay Evidence Groups\n\n| Domain | Status | Fixtures | Rust tests | ContextLattice topic |\n| --- | --- | ---: | ---: | --- |\n");
    for group in list(field(payload, &["replay_evidence_groups"])) {
        writeln!(
            output,
            "| `{}` | `{}` | {} | {} | `{}` |",
            text(group, "id", ""),
            text(group, "status", ""),
            group_size(group, "fixtures")?,
            group_size(group, "rust_tests")?,
            text(group, "contextlattice_topic", ""),
        )
        .expect("string write");
    }
    let template = field(payload, &["contextlattice_checkpoint_template"]);
    writeln!(
        output,
        "\n## Checkpoint Template\n\n`{}`",
        text(template, "command", "")
    )
    .expect("string write");
    Ok(output)
}

pub(super) fn budget(payload: &Py) -> Result<String, String> {
    let mut output = heading("Harness Budget", payload);
    writeln!(
        output,
        "Budget gate: **{}**\n",
        status(field(payload, &["gate", "pass"]))
    )
    .expect("string write");
    output.push_str(
        "| Budget | Actual | Operator | Limit | Status |\n| --- | ---: | --- | ---: | --- |\n",
    );
    for item in list(field(payload, &["budgets"])) {
        writeln!(
            output,
            "| `{}` | {} | `{}` | {} | {} |",
            text(item, "id", ""),
            text(item, "actual", "0"),
            text(item, "operator", ""),
            text(item, "limit", "0"),
            status(field(item, &["pass"])),
        )
        .expect("string write");
    }
    Ok(output)
}
