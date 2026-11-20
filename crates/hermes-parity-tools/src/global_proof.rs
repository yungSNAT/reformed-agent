//! Combine the existing parity reports and evaluate CI and release thresholds.

mod gate;
mod markdown;
mod metrics;

use std::fs;
use std::path::{Component, Path, PathBuf};

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_reports};
use gate::{apply_ci_rules, Gate};

type Metrics = Vec<(&'static str, f64)>;

const EMPTY: Py = Py::Dict(Vec::new());

#[derive(Args, Debug)]
pub struct GlobalProofArgs {
    #[arg(long, default_value = ".")]
    repo_root: String,
    #[arg(long, default_value = "docs/parity/global-parity-thresholds.json")]
    thresholds: String,
    #[arg(long, default_value = "docs/parity/parity-matrix.json")]
    parity_matrix: String,
    #[arg(long, default_value = "docs/parity/workstream-status.json")]
    workstream_status: String,
    #[arg(long, default_value = "docs/parity/test-intent-mapping.json")]
    intent_mapping: String,
    #[arg(long, default_value = "docs/parity/adapter-feature-matrix.json")]
    adapter_matrix: String,
    #[arg(
        long,
        default_value = "docs/parity/shared-different-classification.json"
    )]
    shared_diff_classification: String,
    #[arg(long, default_value = "docs/parity/shared-diff-backlog.json")]
    shared_diff_backlog: String,
    #[arg(long, default_value = "docs/parity/divergence-validation.json")]
    divergence_validation: String,
    #[arg(long, default_value = "docs/parity/upstream-missing-queue.json")]
    patch_queue: String,
    #[arg(long, default_value = "docs/parity/test-coverage-audit.json")]
    test_coverage_audit: String,
    #[arg(long, default_value = "docs/parity/sota-harness-matrix.json")]
    sota_harness_matrix: String,
    #[arg(long, default_value = "docs/parity/behavioral-similarity-diff.json")]
    behavioral_diff: String,
    #[arg(long, default_value = "docs/parity/deep-problem-solving-diff.json")]
    deep_problem_solving_diff: String,
    #[arg(long, default_value = "docs/parity/global-parity-proof.json")]
    out_json: String,
    #[arg(long, default_value = "docs/parity/global-parity-proof.md")]
    out_md: String,
    /// Evaluate the CI gate without writing reports.
    #[arg(long)]
    check_ci: bool,
    /// Evaluate the release gate without writing reports.
    #[arg(long)]
    check_release: bool,
}

fn source_path(raw: &str) -> String {
    let components = Path::new(raw)
        .components()
        .filter(|part| *part != Component::CurDir);
    let path: PathBuf = components.collect();
    py::path_str(if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        &path
    })
}

fn read_json(root: &Path, name: &str) -> Result<Py, String> {
    let path = resolve_argument(&py::join(root, name))?;
    let contents =
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let value = py::loads(&contents).map_err(|error| format!("{}: {error}", path.display()))?;
    if !value.is_dict() {
        return Err(format!("{}: expected a JSON object", path.display()));
    }
    Ok(value)
}

fn get<'a>(value: &'a Py, key: &str) -> &'a Py {
    value.get(key).unwrap_or(&EMPTY)
}

fn entries(value: &Py) -> &[(String, Py)] {
    if let Py::Dict(entries) = value {
        entries
    } else {
        &[]
    }
}

fn list(value: &Py) -> &[Py] {
    if let Py::List(items) = value {
        items
    } else {
        &[]
    }
}

fn items(value: &Py) -> Vec<Py> {
    match value {
        Py::List(items) => items.clone(),
        Py::Dict(entries) => entries.iter().map(|(key, _)| key.clone().into()).collect(),
        Py::Str(value) => value
            .chars()
            .map(|character| character.to_string().into())
            .collect(),
        _ => Vec::new(),
    }
}

fn truth(value: &Py) -> bool {
    match value {
        Py::Null => false,
        Py::Bool(value) => *value,
        Py::Int(value) => *value != 0,
        Py::Float(value) => *value != 0.0,
        Py::Str(value) => !value.is_empty(),
        Py::List(value) => !value.is_empty(),
        Py::Dict(value) => !value.is_empty(),
    }
}

fn number(value: &Py) -> Result<f64, String> {
    match value {
        Py::Int(value) => Ok(*value as f64),
        Py::Float(value) => Ok(*value),
        Py::Bool(value) => Ok(f64::from(*value)),
        Py::Str(value) => py::strip(value)
            .parse()
            .map_err(|_| format!("invalid number {value:?}")),
        _ => Err(format!("expected a number, got {}", py::repr(value))),
    }
}

struct Reports {
    thresholds: Py,
    parity: Py,
    workstreams: Py,
    intents: Py,
    adapters: Py,
    shared_classification: Py,
    shared_backlog: Py,
    divergence: Py,
    queue: Py,
    coverage: Py,
    harness: Py,
    behavioral: Py,
    deep: Py,
}

impl Reports {
    fn read(root: &Path, args: &GlobalProofArgs) -> Result<Self, String> {
        Ok(Self {
            thresholds: read_json(root, &args.thresholds)?,
            parity: read_json(root, &args.parity_matrix)?,
            workstreams: read_json(root, &args.workstream_status)?,
            intents: read_json(root, &args.intent_mapping)?,
            adapters: read_json(root, &args.adapter_matrix)?,
            shared_classification: read_json(root, &args.shared_diff_classification)?,
            shared_backlog: read_json(root, &args.shared_diff_backlog)?,
            divergence: read_json(root, &args.divergence_validation)?,
            queue: read_json(root, &args.patch_queue)?,
            coverage: read_json(root, &args.test_coverage_audit)?,
            harness: read_json(root, &args.sota_harness_matrix)?,
            behavioral: read_json(root, &args.behavioral_diff)?,
            deep: read_json(root, &args.deep_problem_solving_diff)?,
        })
    }
}

fn summary_and_gate(report: &Py, gate: &str) -> Py {
    Py::dict([
        ("summary", get(report, "summary").clone()),
        (gate, get(report, gate).clone()),
    ])
}

fn sources(args: &GlobalProofArgs) -> Py {
    Py::dict(
        [
            ("thresholds", &args.thresholds),
            ("parity_matrix", &args.parity_matrix),
            ("workstream_status", &args.workstream_status),
            ("intent_mapping", &args.intent_mapping),
            ("adapter_matrix", &args.adapter_matrix),
            (
                "shared_diff_classification",
                &args.shared_diff_classification,
            ),
            ("divergence_validation", &args.divergence_validation),
            ("patch_queue", &args.patch_queue),
            ("test_coverage_audit", &args.test_coverage_audit),
            ("sota_harness_matrix", &args.sota_harness_matrix),
            ("behavioral_diff", &args.behavioral_diff),
            ("deep_problem_solving_diff", &args.deep_problem_solving_diff),
        ]
        .map(|(key, value)| (key, source_path(value).into())),
    )
}

pub fn run(args: GlobalProofArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let reports = Reports::read(&root, &args)?;
    let metrics = metrics::calculate(&root, &reports)?;
    let completion = metrics::workstream_completion(&reports)?;
    let ci_thresholds = get(&reports.thresholds, "ci_thresholds");
    let release_thresholds = get(&reports.thresholds, "release_thresholds");
    let mut ci = Gate::evaluate(&metrics, ci_thresholds)?;
    let mut release = Gate::evaluate(&metrics, release_thresholds)?;
    release.require_workstreams(release_thresholds, &completion)?;
    apply_ci_rules(&metrics, &mut ci, &release, ci_thresholds)?;

    if args.check_ci || args.check_release {
        for path in [&args.out_json, &args.out_md] {
            println!(
                "Checked {}",
                py::path_str(&resolve_argument(&py::join(&root, path))?)
            );
        }
        return Ok(i32::from(
            (args.check_ci && !ci.pass) || (args.check_release && !release.pass),
        ));
    }

    let timestamp = py::utc_isoformat();
    let metrics = Py::dict(metrics.into_iter().map(|(key, value)| (key, value.into())));
    let payload = Py::dict([
        ("generated_at_utc", timestamp.into()),
        ("program", get(&reports.thresholds, "program").clone()),
        ("metrics", metrics),
        ("gpar_completion", completion),
        (
            "ws_legacy_states",
            get(&reports.workstreams, "states").clone(),
        ),
        ("ci_gate", ci.into_report()),
        ("release_gate", release.into_report()),
        ("sources", sources(&args)),
        ("queue_summary", get(&reports.queue, "summary").clone()),
        (
            "test_coverage_audit_summary",
            summary_and_gate(&reports.coverage, "audit_gate"),
        ),
        (
            "sota_harness_summary",
            summary_and_gate(&reports.harness, "gate"),
        ),
        (
            "behavioral_diff_summary",
            summary_and_gate(&reports.behavioral, "gate"),
        ),
        (
            "deep_problem_solving_summary",
            summary_and_gate(&reports.deep, "gate"),
        ),
    ]);
    let json = py::dumps(&payload, Some(2), true) + "\n";
    write_reports(
        &root,
        &args.out_json,
        &args.out_md,
        json,
        markdown::render(&payload),
    )?;
    Ok(0)
}
