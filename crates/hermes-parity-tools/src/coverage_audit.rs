//! Compare cited upstream behaviour with the checkout's Rust test evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::coverage_tests::{self, TestCatalogue};
use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_coverage_reports};
use crate::test_intent_mapping::{evidence, EvidencePath};

const INTENT_REPORT: &str = "docs/parity/test-intent-mapping.json";
const DOMAIN_SPECS: &str = "docs/parity/test-intent-domains.json";
const MATRIX: &str = "docs/parity/parity-matrix.json";
const QUEUE: &str = "docs/parity/upstream-missing-queue.json";
const DIVERGENCES: &str = "docs/parity/divergence-validation.json";
const MANIFESTS: [&str; 3] = [
    "docs/parity/python-test-suite-coverage.json",
    "docs/parity/hermes-cli-test-coverage.json",
    "docs/parity/ui-tui-source-coverage.json",
];

#[derive(Args, Debug)]
pub struct CoverageAuditArgs {
    /// Repository root path.
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Output JSON path relative to the repository root.
    #[arg(long, default_value = "docs/parity/test-coverage-audit.json")]
    out_json: String,
    /// Output Markdown path relative to the repository root.
    #[arg(long, default_value = "docs/parity/test-coverage-audit.md")]
    out_md: String,
    /// Evaluate the audit gate without writing reports. Fail on critical gaps.
    #[arg(long)]
    check: bool,
}

fn load(root: &Path, name: &str) -> Result<Py, String> {
    let path = root.join(name);
    let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    py::loads(&text).map_err(|error| format!("{}: {error}", path.display()))
}

fn read_rows<'a>(record: &'a Py, key: &str) -> &'a [Py] {
    match record.get(key) {
        Some(Py::List(list)) => list,
        _ => &[],
    }
}

fn property(record: &Py, key: &str, missing: &str) -> String {
    record
        .get(key)
        .map_or_else(|| missing.to_string(), py::str_of)
}

fn path_string(path: &Path) -> String {
    path.iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn truthy(value: &Py) -> bool {
    match value {
        Py::Null => false,
        Py::Bool(boolean) => *boolean,
        Py::Int(number) => *number != 0,
        Py::Float(number) => *number != 0.0,
        Py::Str(string) => !string.is_empty(),
        Py::List(items) => !items.is_empty(),
        Py::Dict(items) => !items.is_empty(),
    }
}

fn integer(row: &Py, key: &str) -> Result<i128, String> {
    match row.get(key).filter(|value| truthy(value)) {
        None => Ok(0),
        Some(Py::Bool(value)) => Ok(i128::from(*value)),
        Some(Py::Int(value)) => Ok(*value),
        Some(Py::Float(value)) if value.is_finite() => Ok(*value as i128),
        Some(Py::Str(value)) => value
            .trim()
            .parse()
            .map_err(|_| format!("invalid integer field {key}")),
        _ => Err(format!("invalid integer field {key}")),
    }
}

fn floating(row: &Py, key: &str) -> Result<f64, String> {
    match row.get(key).filter(|value| truthy(value)) {
        None => Ok(0.0),
        Some(Py::Bool(value)) => Ok(f64::from(u8::from(*value))),
        Some(Py::Int(value)) => Ok(*value as f64),
        Some(Py::Float(value)) => Ok(*value),
        Some(Py::Str(value)) => value
            .trim()
            .parse()
            .map_err(|_| format!("invalid float field {key}")),
        _ => Err(format!("invalid float field {key}")),
    }
}

struct Manifest {
    name: &'static str,
    entries: usize,
    missing_arrays: Vec<String>,
    deferred: Vec<String>,
    not_rechecked: Vec<String>,
    missing_refs: Vec<Py>,
    referenced: BTreeSet<(String, String)>,
    statuses: BTreeMap<String, usize>,
}

impl Manifest {
    fn from_rows(name: &'static str, rows: &[Py], tests: &TestCatalogue) -> Self {
        let mut manifest = Self {
            name,
            entries: rows.len(),
            missing_arrays: Vec::new(),
            deferred: Vec::new(),
            not_rechecked: Vec::new(),
            missing_refs: Vec::new(),
            referenced: BTreeSet::new(),
            statuses: BTreeMap::new(),
        };
        for row in rows.iter().filter(|row| row.is_dict()) {
            let path = property(row, "path", "unknown");
            let status = property(row, "status", "unknown");
            *manifest.statuses.entry(status.clone()).or_default() += 1;
            if row.get("not_rechecked_at_pin").is_some_and(truthy) {
                manifest.not_rechecked.push(path.clone());
            }
            let citations = row.get("rust_tests");
            if status == "deferred_not_covered"
                && matches!(citations, Some(Py::List(tests)) if tests.is_empty())
            {
                manifest.deferred.push(path);
                continue;
            }
            let Some(Py::List(citations)) = citations.filter(|item| truthy(item)) else {
                manifest.missing_arrays.push(path);
                continue;
            };
            for citation in citations {
                if !citation.is_dict() {
                    manifest
                        .missing_refs
                        .push(missing_reference(&path, "unknown", "unknown"));
                    continue;
                }
                let file = property(citation, "file", "");
                let name = property(citation, "name", "");
                let identified = !file.is_empty() && !name.is_empty();
                if identified {
                    manifest.referenced.insert((file.clone(), name.clone()));
                }
                let path_key = Path::new(&file);
                if !identified
                    || path_string(path_key) != file
                    || !tests
                        .get(path_key)
                        .is_some_and(|names| names.contains(&name))
                {
                    manifest
                        .missing_refs
                        .push(missing_reference(&path, &file, &name));
                }
            }
        }
        manifest
    }

    fn valid(&self) -> i128 {
        self.entries as i128
            - (self.missing_arrays.len() + self.deferred.len() + self.not_rechecked.len()) as i128
    }

    fn to_report(&self) -> Py {
        Py::dict([
            ("path", self.name.into()),
            ("entries", self.entries.into()),
            ("valid_entries_with_rust_tests", Py::Int(self.valid())),
            ("missing_rust_tests_entries", Py::strs(&self.missing_arrays)),
            ("deferred_entries", Py::strs(&self.deferred)),
            ("not_rechecked_entries", Py::strs(&self.not_rechecked)),
            (
                "missing_rust_test_refs",
                Py::List(self.missing_refs.clone()),
            ),
            ("referenced_rust_tests", self.referenced.len().into()),
            (
                "status_counts",
                Py::dict(
                    self.statuses
                        .iter()
                        .map(|(name, &count)| (name, count.into())),
                ),
            ),
        ])
    }
}

fn missing_reference(path: &str, file: &str, name: &str) -> Py {
    Py::dict([
        ("path", path.into()),
        ("file", file.into()),
        ("name", name.into()),
    ])
}

fn direct_evidence(path: &Path, tests: &TestCatalogue) -> bool {
    let text = path_string(path);
    tests.contains_key(path)
        || text.starts_with("tests/")
        || text.contains("/tests/")
        || text.contains("/fixtures/")
        || text.starts_with("crates/hermes-parity-tests/fixtures/")
}

struct Domain {
    id: String,
    scope: Py,
    evidence: BTreeMap<EvidencePath, PathBuf>,
    direct: usize,
}

impl Domain {
    fn classification(&self) -> &str {
        if self.evidence.is_empty() {
            "missing"
        } else if self.direct > 0 {
            "direct_rust_test"
        } else {
            "domain_contract"
        }
    }

    /// The committed report keeps what changes only when coverage does. Evidence counts and
    /// samples move whenever a matching file is added, so neither committed report keeps them.
    fn json(&self) -> String {
        let summary = Py::dict([
            ("id", self.id.as_str().into()),
            ("classification", self.classification().into()),
            ("mapped", (!self.evidence.is_empty()).into()),
            ("upstream_scope", self.scope.clone()),
        ]);
        let Py::Dict(fields) = summary else {
            unreachable!("constructed above")
        };
        indent_object(
            fields
                .into_iter()
                .map(|(key, value)| (key, py::dumps(&value, Some(2), true)))
                .collect(),
        )
    }
}

fn read_domains(root: &Path, tests: &TestCatalogue) -> Result<Vec<Domain>, String> {
    let Py::List(specs) = load(root, DOMAIN_SPECS)? else {
        return Err(format!("{DOMAIN_SPECS} must contain a list"));
    };
    specs
        .into_iter()
        .map(|spec| {
            let (Some(Py::Str(id)), Some(scope), Some(Py::List(patterns))) = (
                spec.get("id"),
                spec.get("upstream_scope"),
                spec.get("local_evidence_globs"),
            ) else {
                return Err(format!("invalid domain in {DOMAIN_SPECS}"));
            };
            let paths = evidence(root, patterns)?;
            let direct = paths
                .values()
                .filter(|path| direct_evidence(path, tests))
                .count();
            Ok(Domain {
                id: id.clone(),
                scope: scope.clone(),
                evidence: paths,
                direct,
            })
        })
        .collect()
}

fn string_array(items: impl Iterator<Item = String>) -> String {
    let items: Vec<_> = items
        .map(|item| format!("  {}", item.replace('\n', "\n  ")))
        .collect();
    if items.is_empty() {
        "[]".into()
    } else {
        format!("[\n{}\n]", items.join(",\n"))
    }
}

fn indent_object(fields: BTreeMap<String, String>) -> String {
    let fields: Vec<_> = fields
        .into_iter()
        .map(|(key, value)| {
            let key = py::dumps(&key.into(), None, false);
            format!("  {key}: {}", value.replace('\n', "\n  "))
        })
        .collect();
    format!("{{\n{}\n}}", fields.join(",\n"))
}

fn gap(
    kind: &str,
    message: impl Into<Py>,
    extras: impl IntoIterator<Item = (&'static str, Py)>,
) -> Py {
    Py::dict(
        [("kind", kind.into()), ("message", message.into())]
            .into_iter()
            .chain(extras),
    )
}

struct Inputs {
    queue: Py,
    intent: Py,
    matrix: Py,
    divergences: Py,
}

fn evaluate(
    root: &Path,
    domains: &[Domain],
    manifests: &[Manifest],
    inputs: &Inputs,
) -> Result<Py, String> {
    let mut critical = Vec::new();
    let mut advisory = Vec::new();
    let empty = Py::dict::<&str>([]);
    let queue = inputs.queue.get("summary").unwrap_or(&empty);
    let pending = integer(queue.get("by_disposition").unwrap_or(&empty), "pending")?;
    if pending != 0 {
        advisory.push(gap(
            "upstream_queue_pending",
            "upstream missing queue has pending rows; queue closure is enforced by global parity thresholds",
            [("pending", Py::Int(pending))],
        ));
    }
    let intent = inputs.intent.get("summary").unwrap_or(&empty);
    let unmapped = integer(intent, "unmapped_intents")?;
    if unmapped != 0 {
        critical.push(gap(
            "unmapped_test_intents",
            "test-intent mapping has unmapped domains",
            [("unmapped", Py::Int(unmapped))],
        ));
    }
    let divergences = inputs.divergences.get("summary").unwrap_or(&empty);
    for key in ["errors", "unowned", "review_overdue"] {
        let count = integer(divergences, key)?;
        if count != 0 {
            critical.push(gap(
                "divergence_governance",
                format!("divergence validation has {key}"),
                [("count", Py::Int(count))],
            ));
        }
    }
    for manifest in manifests {
        for (target, count, kind, message) in [
            (
                &mut critical,
                manifest.missing_arrays.len(),
                "coverage_manifest_missing_tests",
                "has entries without rust_tests",
            ),
            (
                &mut advisory,
                manifest.deferred.len(),
                "coverage_manifest_deferred",
                "has rows deferred without Rust test coverage",
            ),
        ] {
            if count != 0 {
                target.push(gap(
                    kind,
                    format!("{} {message}", manifest.name),
                    [("count", count.into())],
                ));
            }
        }
        if !manifest.not_rechecked.is_empty() {
            advisory.push(gap(
                "coverage_manifest_not_rechecked",
                format!("{} has rows whose upstream file changed after their Rust tests were cited, so they do not count as covered", manifest.name),
                [("count", manifest.not_rechecked.len().into())],
            ));
        }
        if !manifest.missing_refs.is_empty() {
            critical.push(gap(
                "coverage_manifest_missing_refs",
                format!("{} references missing Rust test functions", manifest.name),
                [("count", manifest.missing_refs.len().into())],
            ));
        }
    }
    for (class, gaps, kind, message) in [
        ("missing", &mut critical, "missing_domain_contracts", "test-intent domains have no local evidence"),
        ("domain_contract", &mut advisory, "domain_contract_without_direct_test_file",
            "some mapped intent domains are backed by source contracts without direct test-file evidence in the intent map"),
    ] {
        let ids: Vec<_> = domains.iter().filter(|row| row.classification() == class).map(|row| &row.id).collect();
        if !ids.is_empty() {
            gaps.push(gap(kind, message, [("ids", Py::strs(ids))]));
        }
    }
    let matrix = inputs.matrix.get("summary").unwrap_or(&empty);
    for (source, metric) in [
        ("commits_behind", "max_commits_behind"),
        ("upstream_patch_missing", "max_upstream_patch_missing"),
        ("files_only_upstream", "max_files_only_upstream"),
    ] {
        let value = floating(matrix, source)?;
        if value > 0.0 {
            advisory.push(gap(
                "nonzero_tree_drift",
                format!("{metric} remains nonzero in parity matrix"),
                [("metric", metric.into()), ("value", value.into())],
            ));
        }
    }
    let entries: usize = manifests.iter().map(|manifest| manifest.entries).sum();
    let valid: i128 = manifests.iter().map(Manifest::valid).sum();
    let tracked = entries + domains.len();
    let covered = domains
        .iter()
        .filter(|domain| !domain.evidence.is_empty())
        .count() as i128
        + valid;
    let ratio = if tracked == 0 {
        0.0
    } else {
        covered as f64 / tracked as f64
    };
    let ratio = format!("{ratio:.4}")
        .parse::<f64>()
        .expect("formatted finite coverage ratio");
    let summary = Py::dict([
        ("tracked_behavior_rows", tracked.into()),
        ("covered_behavior_rows", Py::Int(covered)),
        ("tracked_behavior_coverage_ratio", ratio.into()),
        ("coverage_manifest_entries", entries.into()),
        (
            "coverage_manifest_entries_with_valid_rust_tests",
            Py::Int(valid),
        ),
        (
            "coverage_manifest_deferred_entries",
            manifests
                .iter()
                .map(|row| row.deferred.len())
                .sum::<usize>()
                .into(),
        ),
        (
            "coverage_manifest_not_rechecked_entries",
            manifests
                .iter()
                .map(|row| row.not_rechecked.len())
                .sum::<usize>()
                .into(),
        ),
        (
            "missing_rust_test_refs",
            manifests
                .iter()
                .map(|row| row.missing_refs.len())
                .sum::<usize>()
                .into(),
        ),
        ("queue_pending", Py::Int(pending)),
        ("queue_total", Py::Int(integer(queue, "total_commits")?)),
        (
            "test_intents_total",
            Py::Int(integer(intent, "total_intents")?),
        ),
        (
            "test_intents_mapped",
            Py::Int(integer(intent, "mapped_intents")?),
        ),
        (
            "test_intent_mapping_ratio",
            floating(intent, "mapping_ratio")?.into(),
        ),
        (
            "divergence_errors",
            Py::Int(integer(divergences, "errors")?),
        ),
        (
            "divergence_unowned",
            Py::Int(integer(divergences, "unowned")?),
        ),
        (
            "divergence_review_overdue",
            Py::Int(integer(divergences, "review_overdue")?),
        ),
    ]);
    let (complete, next) = hardening_moves(root);
    Ok(Py::dict([
        ("schema_version", Py::Int(1)),
        ("audit_unit", "upstream_behavior_or_test_intent".into()),
        ("summary", summary),
        (
            "audit_gate",
            Py::dict([
                ("pass", critical.is_empty().into()),
                ("critical_gaps", critical.len().into()),
                ("advisory_gaps", advisory.len().into()),
            ]),
        ),
        (
            "source_artifacts",
            Py::dict([
                ("test_intent_mapping", INTENT_REPORT.into()),
                ("parity_matrix", MATRIX.into()),
                ("upstream_missing_queue", QUEUE.into()),
                ("divergence_validation", DIVERGENCES.into()),
                ("coverage_manifests", Py::strs(MANIFESTS)),
            ]),
        ),
        (
            "coverage_manifests",
            Py::List(manifests.iter().map(Manifest::to_report).collect()),
        ),
        ("critical_gaps", Py::List(critical)),
        ("advisory_gaps", Py::List(advisory)),
        ("completed_sigma_harness_moves", Py::List(complete)),
        ("next_sigma_harness_moves", Py::List(next)),
    ]))
}

fn hardening_moves(root: &Path) -> (Vec<Py>, Vec<Py>) {
    let mut complete = Vec::new();
    let mut next = Vec::new();
    for (title, rationale, artifacts) in [
        (
            "Coverage trend ledger",
            "Track behavior coverage over time so new upstream rows or local harness regressions create visible deltas before release prep.",
            ["docs/parity/harness-trend-ledger.json", "docs/parity/harness-trend-ledger.md"],
        ),
        (
            "ContextLattice replay evidence index",
            "Index passing and failing replay artifacts into ContextLattice so agents can retrieve exact harness evidence instead of rediscovering it from scratch.",
            ["docs/parity/contextlattice-replay-evidence-index.json", "docs/parity/contextlattice-replay-evidence-index.md"],
        ),
        (
            "Cross-version harness budget",
            "Record runtime and fixture-count budgets across releases so SOTA harness growth stays deterministic, bounded, and reviewable.",
            ["docs/parity/harness-budget.json", "docs/parity/harness-budget.md"],
        ),
    ] {
        let exists = artifacts.iter().all(|path| root.join(path).exists());
        let (rows, status) = if exists {(&mut complete, "complete")} else {(&mut next, "pending")};
        rows.push(Py::dict([
            ("title", title.into()),
            ("rationale", rationale.into()),
            ("artifacts", Py::strs(artifacts)),
            ("status", status.into()),
        ]));
    }
    (complete, next)
}

fn render_markdown(report: &Py, manifests: &[Manifest], domains: &[Domain]) -> String {
    let gate = report.get("audit_gate").expect("generated gate");
    let status = if gate.get("pass") == Some(&Py::Bool(true)) {
        "PASS"
    } else {
        "FAIL"
    };
    let critical = property(gate, "critical_gaps", "0");
    let advisory = property(gate, "advisory_gaps", "0");
    let mut markdown = format!(
        "# Test Coverage Audit\n\n## Gate\n\n- Audit gate: **{status}**\n- Critical gaps: `{critical}`\n- Advisory gaps: `{advisory}`\n\n## Summary\n\n| Metric | Value |\n| --- | ---: |\n"
    );
    let summary = report.get("summary").expect("generated summary");
    for key in [
        "tracked_behavior_rows",
        "covered_behavior_rows",
        "tracked_behavior_coverage_ratio",
        "coverage_manifest_entries",
        "coverage_manifest_entries_with_valid_rust_tests",
        "coverage_manifest_deferred_entries",
        "coverage_manifest_not_rechecked_entries",
        "missing_rust_test_refs",
        "queue_pending",
        "queue_total",
        "test_intents_total",
        "test_intents_mapped",
    ] {
        writeln!(markdown, "| `{key}` | {} |", property(summary, key, "0")).unwrap();
    }
    markdown.push_str("\n## Coverage Manifests\n\n| Manifest | Entries | Valid Rust-test entries | Deferred, not covered | Not rechecked at the pin | Referenced Rust tests | Missing refs |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for manifest in manifests {
        writeln!(
            markdown,
            "| `{}` | {} | {} | {} | {} | {} | {} |",
            manifest.name,
            manifest.entries,
            manifest.valid(),
            manifest.deferred.len(),
            manifest.not_rechecked.len(),
            manifest.referenced.len(),
            manifest.missing_refs.len(),
        )
        .unwrap();
    }
    markdown.push_str("\n## Test Intent Domains\n\n| Intent | Classification |\n| --- | --- |\n");
    for domain in domains {
        writeln!(
            markdown,
            "| `{}` | `{}` |",
            domain.id,
            domain.classification()
        )
        .unwrap();
    }
    for (title, key) in [
        ("Critical Gaps", "critical_gaps"),
        ("Advisory Gaps", "advisory_gaps"),
        (
            "Completed Sigma Harness Moves",
            "completed_sigma_harness_moves",
        ),
        ("Next Sigma Harness Moves", "next_sigma_harness_moves"),
    ] {
        write!(markdown, "\n## {title}\n\n").unwrap();
        let rows = read_rows(report, key);
        if rows.is_empty() {
            markdown.push_str("- none\n");
        }
        for row in rows {
            match key {
                "critical_gaps" | "advisory_gaps" => {
                    writeln!(
                        markdown,
                        "- `{}`: {}",
                        property(row, "kind", ""),
                        property(row, "message", "")
                    )
                    .unwrap();
                }
                "completed_sigma_harness_moves" => {
                    let artifacts = read_rows(row, "artifacts")
                        .iter()
                        .map(|value| format!("`{}`", py::str_of(value)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    writeln!(
                        markdown,
                        "- **{}**: {artifacts}",
                        property(row, "title", "")
                    )
                    .unwrap();
                }
                _ => {
                    writeln!(
                        markdown,
                        "- **{}**: {}",
                        property(row, "title", ""),
                        property(row, "rationale", "")
                    )
                    .unwrap();
                }
            }
        }
    }
    markdown
}

fn generate(root: &Path) -> Result<(String, String, bool), String> {
    let tests = coverage_tests::scan(root)?;
    let inputs = Inputs {
        queue: load(root, QUEUE)?,
        intent: load(root, INTENT_REPORT)?,
        matrix: load(root, MATRIX)?,
        divergences: load(root, DIVERGENCES)?,
    };
    let domains = read_domains(root, &tests)?;
    let manifests = MANIFESTS
        .into_iter()
        .map(|name| {
            let data = load(root, name)?;
            Ok(Manifest::from_rows(
                name,
                read_rows(&data, "entries"),
                &tests,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let report = evaluate(root, &domains, &manifests, &inputs)?;
    let pass = report.get("audit_gate").and_then(|gate| gate.get("pass")) == Some(&Py::Bool(true));
    let markdown = render_markdown(&report, &manifests, &domains);
    let Py::Dict(fields) = report else {
        unreachable!("generated report")
    };
    let mut fields = fields
        .into_iter()
        .map(|(key, value)| (key, py::dumps(&value, Some(2), true)))
        .collect::<BTreeMap<_, _>>();
    fields.insert(
        "intent_domains".into(),
        string_array(domains.iter().map(Domain::json)),
    );
    Ok((indent_object(fields) + "\n", markdown, pass))
}

pub fn run(args: CoverageAuditArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let (json, markdown, pass) = generate(&root)?;
    if args.check {
        return Ok(i32::from(!pass));
    }
    write_coverage_reports(&root, &args.out_json, &args.out_md, json, markdown)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_check_leaves_reports_untouched_for_both_gate_results() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("docs/parity")).unwrap();
        for input in MANIFESTS.into_iter().chain([MATRIX, QUEUE, DIVERGENCES]) {
            fs::write(root.join(input), "{}").unwrap();
        }
        fs::write(root.join(DOMAIN_SPECS), "[]").unwrap();
        fs::write(root.join("audit.json"), "previous JSON\n").unwrap();
        let args = |check| CoverageAuditArgs {
            repo_root: root.to_str().unwrap().into(),
            out_json: "audit.json".into(),
            out_md: "reports/audit.md".into(),
            check,
        };

        fs::write(root.join(INTENT_REPORT), "{}").unwrap();
        assert_eq!(run(args(true)).unwrap(), 0);
        fs::write(
            root.join(INTENT_REPORT),
            r#"{"summary":{"unmapped_intents":1}}"#,
        )
        .unwrap();
        assert_eq!(run(args(true)).unwrap(), 1);
        assert_eq!(
            fs::read(root.join("audit.json")).unwrap(),
            b"previous JSON\n"
        );
        assert!(!root.join("reports").exists());

        assert_eq!(run(args(false)).unwrap(), 0);
        let report = fs::read_to_string(root.join("audit.json")).unwrap();
        assert!(report.contains("unmapped_test_intents"));
        assert!(root.join("reports/audit.md").is_file());
    }

    /// Open pull requests conflicted on these committed reports whenever any of them added a
    /// test or a file, so the reports leave out the time and the counts that move with the tree.
    #[test]
    fn reports_stay_byte_identical_when_a_test_or_evidence_file_is_added() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("docs/parity")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        for input in MANIFESTS
            .into_iter()
            .chain([MATRIX, QUEUE, DIVERGENCES, INTENT_REPORT])
        {
            fs::write(root.join(input), "{}").unwrap();
        }
        fs::write(
            root.join(DOMAIN_SPECS),
            r#"[{"id":"alpha","upstream_scope":"a","local_evidence_globs":["tests/*.rs"]}]"#,
        )
        .unwrap();
        let reports = || {
            let args = CoverageAuditArgs {
                repo_root: root.to_str().unwrap().into(),
                out_json: "audit.json".into(),
                out_md: "audit.md".into(),
                check: false,
            };
            assert_eq!(run(args).unwrap(), 0);
            (
                fs::read_to_string(root.join("audit.json")).unwrap(),
                fs::read_to_string(root.join("audit.md")).unwrap(),
            )
        };
        fs::write(root.join("tests/first.rs"), "#[test]\nfn first() {}\n").unwrap();
        let (json, markdown) = reports();

        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(root.join("tests/second.rs"), "#[test]\nfn second() {}\n").unwrap();
        let (json_after, markdown_after) = reports();

        assert_eq!(json_after, json);
        assert_eq!(markdown_after, markdown);
        for volatile in ["generated_at_utc", "rust_test_functions", "evidence_count"] {
            assert!(!json.contains(volatile), "{volatile} in {json}");
        }
        assert!(json.contains("\"classification\": \"direct_rust_test\""));
    }
}

#[cfg(test)]
#[path = "coverage_audit_coverage_gen.rs"]
mod coverage_gen;
