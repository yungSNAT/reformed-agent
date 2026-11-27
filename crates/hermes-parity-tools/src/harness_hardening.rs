//! Render the parity harness trend, replay index and review budget.

mod markdown;

use std::fs;
use std::path::Path;

use clap::Args;

use crate::coverage_tests;
use crate::decimal_limit;
use crate::git::git;
use crate::py::{self, Py};
use crate::report_io::{resolve_argument, truth, write_report};

const TREND: &str = "docs/parity/harness-trend-ledger";
const REPLAY: &str = "docs/parity/contextlattice-replay-evidence-index";
const BUDGET: &str = "docs/parity/harness-budget";
const PROOF: &str = "docs/parity/global-parity-proof.json";
const COVERAGE: &str = "docs/parity/test-coverage-audit.json";
const QUEUE: &str = "docs/parity/upstream-missing-queue.json";
const SOTA: &str = "docs/parity/sota-harness-matrix.json";
const THRESHOLDS: &str = "docs/parity/global-parity-thresholds.json";

#[derive(Args, Debug)]
pub struct HarnessHardeningArgs {
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Return failure for an unmet budget without writing the reports.
    #[arg(long)]
    check: bool,
}

fn load(root: &Path, path: &str) -> Result<Py, String> {
    let path = root.join(path);
    if !path.exists() {
        return Ok(Py::dict([] as [(&str, Py); 0]));
    }
    let source =
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    py::loads(&source)
}

fn field<'a>(value: &'a Py, keys: &[&str]) -> &'a Py {
    keys.iter()
        .try_fold(value, |value, key| value.get(key))
        .unwrap_or(&Py::Null)
}

fn list(value: &Py) -> &[Py] {
    match value {
        Py::List(items) => items,
        _ => &[],
    }
}

fn integer(value: &Py) -> Result<Py, String> {
    let integer = match value {
        Py::Bool(value) => i128::from(*value),
        Py::Int(value) => *value,
        Py::Float(value) if value.is_infinite() => {
            return Err("cannot convert an infinite harness metric to an integer".into());
        }
        Py::Float(value) => *value as i128,
        Py::Str(value) => decimal_limit::parse(value)
            .ok()
            .and_then(|number| number.label.parse().ok())
            .unwrap_or(0),
        _ => 0,
    };
    Ok(Py::Int(integer))
}

fn decimal(value: &Py) -> f64 {
    match value {
        Py::Bool(value) => f64::from(u8::from(*value)),
        Py::Int(value) => *value as f64,
        Py::Float(value) => *value,
        Py::Str(value) => decimal_limit::parse_float(value).unwrap_or(0.0),
        _ => 0.0,
    }
}

fn source_artifacts() -> Py {
    Py::dict([
        ("global_parity_proof", PROOF.into()),
        ("harness_budget", format!("{BUDGET}.json").into()),
        ("harness_trend_ledger", format!("{TREND}.json").into()),
        ("sota_harness_matrix", SOTA.into()),
        ("test_coverage_audit", COVERAGE.into()),
        ("upstream_missing_queue", QUEUE.into()),
    ])
}

fn optional_git(root: &Path, arguments: &[&str]) -> Result<String, String> {
    let result = git(root, arguments)?;
    Ok(if result.ok {
        py::strip(&result.stdout).into()
    } else {
        String::new()
    })
}

/// `rust_test_functions` is counted from the checkout: the committed coverage audit leaves
/// it out because it changes with every added test.
fn snapshot(
    root: &Path,
    coverage: &Py,
    proof: &Py,
    queue: &Py,
    sota: &Py,
    rust_test_functions: usize,
) -> Result<Py, String> {
    let mut head = optional_git(root, &["rev-parse", "HEAD"])?;
    if !optional_git(root, &["status", "--short"])?.is_empty() {
        head.push_str("+worktree");
    }
    let upstream = optional_git(root, &["rev-parse", "upstream/main"])?;
    let dispositions = field(queue, &["summary", "by_disposition"]);
    if !matches!(dispositions, Py::Null | Py::Dict(_)) {
        return Err("queue dispositions must be an object".into());
    }
    let mut snapshot = Py::dict([
        ("recorded_at_utc", py::utc_isoformat().into()),
        ("local_head", head.into()),
        ("upstream_head", upstream.into()),
        (
            "release_gate_pass",
            truth(field(proof, &["release_gate", "pass"])).into(),
        ),
        (
            "ci_gate_pass",
            truth(field(proof, &["ci_gate", "pass"])).into(),
        ),
        ("queue_pending", integer(field(dispositions, &["pending"]))?),
        (
            "queue_total",
            integer(field(queue, &["summary", "total_commits"]))?,
        ),
        (
            "tracked_behavior_coverage_ratio",
            decimal(field(
                coverage,
                &["summary", "tracked_behavior_coverage_ratio"],
            ))
            .into(),
        ),
        (
            "sota_domain_coverage_ratio",
            decimal(field(sota, &["summary", "domain_coverage_ratio"])).into(),
        ),
    ]);
    for name in [
        "tracked_behavior_rows",
        "covered_behavior_rows",
        "coverage_manifest_entries",
        "missing_rust_test_refs",
    ] {
        snapshot.set(name, integer(field(coverage, &["summary", name]))?);
    }
    snapshot.set("rust_test_functions", rust_test_functions.into());
    for (name, section, key) in [
        ("sota_direct_rust_tests", "summary", "direct_rust_tests"),
        (
            "sota_workflow_replay_steps",
            "summary",
            "workflow_replay_steps",
        ),
        ("sota_protocol_cases", "summary", "protocol_cases"),
        ("sota_fault_scenarios", "summary", "fault_scenarios"),
        ("sota_critical_gaps", "gate", "critical_gaps"),
        (
            "sota_missing_rust_test_refs",
            "gate",
            "missing_rust_test_refs",
        ),
    ] {
        snapshot.set(name, integer(field(sota, &[section, key]))?);
    }
    Ok(snapshot)
}

fn artifact(purpose: &str) -> Py {
    Py::dict([
        ("generated_at_utc", py::utc_isoformat().into()),
        ("schema_version", Py::Int(1)),
        ("purpose", purpose.into()),
        ("source_artifacts", source_artifacts()),
    ])
}

fn text(value: &Py, key: &str, default: &str) -> String {
    value
        .get(key)
        .map(py::str_of)
        .unwrap_or_else(|| default.into())
}

fn trend(existing: &Py, latest: &Py) -> Py {
    let head = text(latest, "local_head", "");
    let mut entries: Vec<_> = list(field(existing, &["entries"]))
        .iter()
        .filter(|entry| entry.is_dict() && text(entry, "local_head", "") != head)
        .cloned()
        .collect();
    entries.push(latest.clone());
    let excess = entries.len().saturating_sub(64);
    entries.drain(..excess);
    let previous = entries.iter().rev().nth(1);
    let deltas = Py::dict(previous.into_iter().flat_map(|entry| {
        [
            "queue_pending",
            "tracked_behavior_rows",
            "covered_behavior_rows",
            "rust_test_functions",
            "coverage_manifest_entries",
            "sota_direct_rust_tests",
            "sota_workflow_replay_steps",
            "sota_protocol_cases",
            "sota_fault_scenarios",
        ]
        .map(|key| {
            (
                key,
                Py::Float(decimal(field(latest, &[key])) - decimal(field(entry, &[key]))),
            )
        })
    }));
    let mut result = artifact("Track parity coverage and SOTA harness movement over time.");
    result.set("latest", latest.clone());
    result.set("delta_from_previous_entry", deltas);
    result.set("entries", Py::List(entries));
    result
}

fn replay(coverage: &Py, proof: &Py, queue: &Py, sota: &Py) -> Result<Py, String> {
    let domains = match sota.get("domains") {
        None | Some(Py::Str(_) | Py::Dict(_)) => &[][..],
        Some(Py::List(items)) => items,
        _ => return Err("SOTA harness domains must be iterable".into()),
    };
    let groups = domains.iter().filter(|domain| domain.is_dict()).map(|domain| {
        let id = text(domain, "id", "unknown");
        let topic = format!("hermes-agent-ultra/parity/sota/{id}");
        let query = format!("Hermes Agent Ultra SOTA harness evidence for {id} including fixtures, Rust tests, failures, and queue status");
        let mut group = Py::dict([
            ("id", id.as_str().into()),
            ("title", text(domain, "title", &id).into()),
            ("status", text(domain, "status", "unknown").into()),
            ("why", text(domain, "why", "").into()),
            ("contextlattice_topic", topic.into()),
            ("retrieval_query", query.into()),
        ]);
        for key in ["fixtures", "rust_tests", "required_capabilities"] {
            group.set(key, domain.get(key).cloned().unwrap_or(Py::List(Vec::new())));
        }
        group
    });
    let mut result = artifact("Index replay, protocol, and fault-injection evidence so ContextLattice checkpoints can point to exact local artifacts.");
    result.set(
        "latest_gate_state",
        Py::dict([
            (
                "release_gate_pass",
                truth(field(proof, &["release_gate", "pass"])).into(),
            ),
            (
                "ci_gate_pass",
                truth(field(proof, &["ci_gate", "pass"])).into(),
            ),
            (
                "test_coverage_gate_pass",
                truth(field(coverage, &["audit_gate", "pass"])).into(),
            ),
            (
                "sota_harness_gate_pass",
                truth(field(sota, &["gate", "pass"])).into(),
            ),
            (
                "queue_pending",
                integer(field(queue, &["summary", "by_disposition", "pending"]))?,
            ),
        ]),
    );
    result.set("replay_evidence_groups", Py::List(groups.collect()));
    result.set("contextlattice_checkpoint_template", Py::dict([
        ("project", "hermes-agent-ultra".into()),
        ("topic", "parity/sota".into()),
        ("command", "contextlattice_write -p hermes-agent-ultra -t parity/sota -f docs/parity/contextlattice-replay-evidence-index.md --stdin".into()),
    ]));
    Ok(result)
}

fn budget(snapshot: &Py, thresholds: &Py) -> Result<Py, String> {
    let release = field(thresholds, &["release_thresholds"]);
    let values = release.get("functional_parity");
    if values.is_some_and(|value| !value.is_dict()) {
        return Err("functional_parity thresholds must be an object".into());
    }
    let limit = |key| values.and_then(|value| value.get(key));
    let rows = [
        ("queue_pending", integer(limit("max_queue_pending_commits").unwrap_or(&Py::Int(0)))?, "<=",
         "release queue must stay fully triaged"),
        ("tracked_behavior_coverage_ratio", decimal(limit("min_test_coverage_tracked_behavior_ratio").unwrap_or(&Py::Float(1.0))).into(), ">=",
         "all tracked behavior rows must remain covered"),
        ("sota_domain_coverage_ratio", decimal(limit("min_sota_harness_domain_coverage_ratio").unwrap_or(&Py::Float(1.0))).into(), ">=",
         "all SOTA harness domains must remain represented"),
        ("rust_test_functions", Py::Int(8000), "<=",
         "cross-version review required before the post-parity Rust test surface grows another large tranche"),
        ("coverage_manifest_entries", Py::Int(600), "<=",
         "coverage manifest growth must stay reviewable across releases"),
        ("sota_workflow_replay_steps", Py::Int(12), "<=",
         "workflow replay growth needs explicit review before it becomes slow"),
        ("sota_protocol_cases", Py::Int(20), "<=",
         "protocol fixture cases should remain bounded and auditable"),
        ("sota_fault_scenarios", Py::Int(16), "<=",
         "fault-injection expansion should stay deterministic"),
    ];
    let mut failures = Vec::new();
    let budgets = rows
        .into_iter()
        .map(|(id, limit, operator, reason)| {
            let actual = field(snapshot, &[id]);
            let pass = if operator == "<=" {
                decimal(actual) <= decimal(&limit)
            } else {
                decimal(actual) >= decimal(&limit)
            };
            if !pass {
                failures.push(Py::from(id));
            }
            Py::dict([
                ("id", id.into()),
                ("actual", actual.clone()),
                ("limit", limit),
                ("operator", operator.into()),
                ("reason", reason.into()),
                ("pass", pass.into()),
            ])
        })
        .collect();
    let mut result = artifact("Cross-version review budget for SOTA harness and coverage growth.");
    result.set("snapshot", snapshot.clone());
    result.set("budgets", Py::List(budgets));
    result.set(
        "gate",
        Py::dict([
            ("pass", failures.is_empty().into()),
            ("failures", Py::List(failures)),
        ]),
    );
    Ok(result)
}

pub fn run(args: HarnessHardeningArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let coverage = load(&root, COVERAGE)?;
    let proof = load(&root, PROOF)?;
    let queue = load(&root, QUEUE)?;
    let sota = load(&root, SOTA)?;
    let thresholds = load(&root, THRESHOLDS)?;
    let tests = coverage_tests::scan(&root)?;
    let functions = tests.values().map(|names| names.len()).sum();
    let snapshot = snapshot(&root, &coverage, &proof, &queue, &sota, functions)?;
    let existing = load(&root, &format!("{TREND}.json"))?;
    if !existing.is_dict() || !sota.is_dict() || !thresholds.is_dict() {
        return Err("harness sources must contain JSON objects".into());
    }
    let trend = trend(&existing, &snapshot);
    let replay = replay(&coverage, &proof, &queue, &sota)?;
    let budget = budget(&snapshot, &thresholds)?;
    if args.check {
        for base in [TREND, REPLAY, BUDGET] {
            for suffix in ["json", "md"] {
                println!(
                    "Checked {}",
                    py::path_str(&root.join(format!("{base}.{suffix}")))
                );
            }
        }
        return Ok(i32::from(!truth(field(&budget, &["gate", "pass"]))));
    }
    for (base, report, render) in [
        (
            TREND,
            &trend,
            markdown::trend as fn(&Py) -> Result<String, String>,
        ),
        (REPLAY, &replay, markdown::replay),
        (BUDGET, &budget, markdown::budget),
    ] {
        let path = root.join(format!("{base}.json"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        write_report(&path, py::dumps(report, Some(2), true) + "\n")?;
        write_report(&root.join(format!("{base}.md")), render(report)?)?;
    }
    for base in [TREND, REPLAY, BUDGET] {
        for suffix in ["json", "md"] {
            println!(
                "Wrote {}",
                py::path_str(&root.join(format!("{base}.{suffix}")))
            );
        }
    }
    Ok(0)
}

#[cfg(test)]
#[path = "harness_hardening_coverage_gen.rs"]
mod coverage_gen;
