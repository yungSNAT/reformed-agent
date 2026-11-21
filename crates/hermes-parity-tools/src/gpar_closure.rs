//! Report closure of the four scoped GPAR queue tickets.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;

use crate::decimal_limit;
use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_report};
use clap::Args;

const TICKETS: [(u8, &str); 4] = [
    (20, "GPAR-01 tests+CI parity"),
    (21, "GPAR-02 skills parity"),
    (22, "GPAR-03 UX parity"),
    (23, "GPAR-04 gateway/plugin-memory parity"),
];
#[derive(Args, Debug)]
pub struct ClosureArgs {
    /// Repository root.
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Return failure for pending scoped commits without writing the reports.
    #[arg(long)]
    check: bool,
}

fn scoped_ticket(value: Option<&Py>) -> Result<Option<usize>, String> {
    let number = match value {
        None | Some(Py::Bool(_)) => return Ok(None),
        Some(Py::Int(number)) => u8::try_from(*number).ok(),
        Some(Py::Float(number)) if number.is_finite() => {
            let number = number.trunc();
            (0.0..=f64::from(u8::MAX))
                .contains(&number)
                .then_some(number as u8)
        }
        Some(Py::Str(text)) => u8::try_from(decimal_limit::parse(text)?.count).ok(),
        _ => return Err("target_ticket must be an integer".to_string()),
    };
    Ok(TICKETS.iter().position(|(id, _)| Some(*id) == number))
}

fn counts_json(counts: &BTreeMap<String, usize>) -> Py {
    Py::dict(
        counts
            .iter()
            .map(|(label, count)| (label.as_str(), (*count).into())),
    )
}

pub fn run(args: ClosureArgs) -> Result<i32, String> {
    let root = resolve_argument(&py::expanduser(&args.repo_root)?)?;
    let docs = root.join("docs/parity");
    let queue_path = docs.join("upstream-missing-queue.json");
    let data = fs::read_to_string(&queue_path)
        .map_err(|error| format!("{}: {error}", queue_path.display()))?;
    let queue = py::loads(&data)?;
    let Some(Py::List(commits)) = queue.get("commits") else {
        return Err("upstream queue commits must be an array".to_string());
    };
    let mut by_ticket: [BTreeMap<String, usize>; TICKETS.len()] = Default::default();
    let mut by_disposition = BTreeMap::new();
    for item in commits {
        if !item.is_dict() {
            return Err("upstream queue commit must be an object".to_string());
        }
        let Some(ticket) = scoped_ticket(item.get("target_ticket"))? else {
            continue;
        };
        let disposition = item
            .get("disposition")
            .map_or_else(|| "pending".to_string(), py::str_of);
        *by_ticket[ticket].entry(disposition.clone()).or_insert(0) += 1;
        *by_disposition.entry(disposition).or_insert(0) += 1;
    }
    let pending = by_disposition.get("pending").copied().unwrap_or(0);
    let passed = pending == 0;
    if args.check {
        if !passed {
            eprintln!("GPAR closure has {pending} pending scoped commits");
        }
        return Ok(i32::from(!passed));
    }
    let generated = py::utc_isoformat();
    let total: usize = by_disposition.values().sum();
    let payload = Py::dict([
        ("generated_at_utc", generated.as_str().into()),
        (
            "scope",
            Py::dict([
                (
                    "tickets",
                    Py::List(TICKETS.map(|(id, _)| usize::from(id).into()).into()),
                ),
                (
                    "labels",
                    Py::dict(TICKETS.map(|(id, name)| (id.to_string(), name.into()))),
                ),
            ]),
        ),
        (
            "summary",
            Py::dict([
                ("total_commits", total.into()),
                ("by_disposition", counts_json(&by_disposition)),
                ("total_pending", pending.into()),
                ("pass", passed.into()),
            ]),
        ),
        (
            "ticket_breakdown",
            Py::dict(TICKETS.iter().zip(&by_ticket).map(|((id, label), counts)| {
                (
                    id.to_string(),
                    Py::dict([
                        ("label", (*label).into()),
                        ("total", counts.values().sum::<usize>().into()),
                        (
                            "pending",
                            counts.get("pending").copied().unwrap_or(0).into(),
                        ),
                        ("by_disposition", counts_json(counts)),
                    ]),
                )
            })),
        ),
    ]);
    let json_path = docs.join("gpar-01-04-proof.json");
    let markdown_path = docs.join("gpar-01-04-proof.md");
    write_report(&json_path, py::dumps(&payload, Some(2), true) + "\n")?;
    let scope = TICKETS.map(|(id, _)| id.to_string()).join(", ");
    let gate = if passed { "PASS" } else { "FAIL" };
    let mut markdown = format!(
        "# GPAR-01..04 Parity Proof\n\nGenerated: `{generated}`\n\n- Scope tickets: `{scope}`\n- Total scoped commits: `{total}`\n- Pending scoped commits: `{pending}`\n- Gate: `{gate}`\n\n| Ticket | Label | Total | Pending | Ported | Superseded |\n| ---: | --- | ---: | ---: | ---: | ---: |\n"
    );
    for ((id, label), counts) in TICKETS.iter().zip(&by_ticket) {
        let total = counts.values().sum::<usize>();
        let pending = counts.get("pending").copied().unwrap_or(0);
        let ported = counts.get("ported").copied().unwrap_or(0);
        let superseded = counts.get("superseded").copied().unwrap_or(0);
        writeln!(
            markdown,
            "| #{id} | {label} | {total} | {pending} | {ported} | {superseded} |"
        )
        .expect("writing to a string");
    }
    markdown.push('\n');
    write_report(&markdown_path, markdown)?;
    println!("Wrote {}", py::path_str(&json_path));
    println!("Wrote {}", py::path_str(&markdown_path));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coerces_ticket_values_before_checking_scope() {
        for value in [Py::Int(20), Py::Float(20.9), "+0_20".into(), "٢٠".into()] {
            assert_eq!(scoped_ticket(Some(&value)), Ok(Some(0)));
        }
        assert_eq!(scoped_ticket(Some(&Py::Int(24))), Ok(None));
        assert_eq!(scoped_ticket(None), Ok(None));
        assert!(scoped_ticket(Some(&"2_".into())).is_err());
    }
}
