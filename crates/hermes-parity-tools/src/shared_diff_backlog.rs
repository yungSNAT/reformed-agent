//! Classify the paths with different blobs in both trees.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use clap::Args;

use crate::git::{ensure_remote, git_text, ls_tree_blobs};
use crate::parity_matrix::{path_matches, workstream_for as matrix_workstream};
use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_reports};

#[derive(Args, Debug)]
pub struct SharedDiffBacklogArgs {
    /// Repository root path
    #[arg(long, default_value = ".")]
    repo_root: String,
    #[arg(long, default_value = "main")]
    local_ref: String,
    #[arg(long, default_value = "upstream")]
    upstream_remote: String,
    #[arg(
        long,
        default_value = "https://github.com/NousResearch/hermes-agent.git"
    )]
    upstream_url: String,
    #[arg(long, default_value = "main")]
    upstream_branch: String,
    #[arg(long)]
    no_fetch: bool,
    /// Shared-different classification registry relative to the repository root
    #[arg(
        long,
        default_value = "docs/parity/shared-different-classification.json"
    )]
    classification: String,
    #[arg(long, default_value = "docs/parity/shared-diff-backlog.json")]
    out_json: String,
    #[arg(long, default_value = "docs/parity/shared-diff-backlog.md")]
    out_md: String,
}

// The matrix routes docs to WS5. The backlog has always treated them as WS8.
pub(crate) fn workstream_for(path: &str) -> &'static str {
    if path_matches(path, "docs") {
        "WS8"
    } else {
        matrix_workstream(path)
    }
}

fn text(item: &Py, name: &str, default: &str) -> String {
    item.get(name)
        .map_or_else(|| default.to_owned(), py::str_of)
}

fn longest_prefix_match<'a>(path: &str, items: &'a [Py]) -> Option<&'a Py> {
    let mut best = None;
    let mut best_len = 0;
    for item in items {
        let prefix = text(item, "path", "");
        let prefix = py::strip(&prefix).trim_end_matches('/');
        if prefix.len() > best_len && path_matches(path, prefix) {
            best = Some(item);
            best_len = prefix.len();
        }
    }
    best
}

fn rust_requirement(path: &str, classification: &str) -> &'static str {
    if matches!(
        classification,
        "branding_only" | "policy_only" | "intentional_divergence"
    ) {
        return "not_required_for_runtime";
    }
    for (prefixes, requirement) in [
        (&["tests/"][..], "rust_equivalent_or_contract_test_required"),
        (
            &["plugins/", "tools/", "gateway/", "environments/"][..],
            "rust_native_runtime_parity_required_if_needed",
        ),
        (
            &["ui-tui/", "website/", "web/"][..],
            "rust_primary_ux_or_documented_divergence_required",
        ),
        (
            &["skills/", "optional-skills/"][..],
            "skill_catalog_contract_required_if_needed",
        ),
    ] {
        if prefixes.iter().any(|prefix| path.starts_with(prefix)) {
            return requirement;
        }
    }
    if path.ends_with(".py") {
        "rust_port_required_if_runtime_surface"
    } else {
        "needs_rust_relevance_review"
    }
}

fn action_for(classification: &str, requirement: &str) -> &'static str {
    match classification {
        "branding_only" | "policy_only" => "No runtime implementation; keep rationale current.",
        "intentional_divergence" => "No vendoring; verify approved divergence remains current.",
        "deferred" => "Review against upstream by the review date; see the rationale and ledger line.",
        "functional" => match requirement {
            "rust_equivalent_or_contract_test_required" => "Map upstream test intent to Rust/Python contract coverage; add missing regression tests.",
            "rust_primary_ux_or_documented_divergence_required" => "Compare UX behavior; port necessary interaction semantics or document stronger local UX.",
            "skill_catalog_contract_required_if_needed" => "Verify skill catalog/runtime loader coverage; add native contract tests if missing.",
            _ => "Inspect upstream/local behavior; implement Rust-native parity only if local coverage is missing.",
        },
        _ => "Add classification, owner, issue, and surgical plan before implementation.",
    }
}

struct Entry<'a> {
    path: &'a str,
    classification: String,
    source: Option<&'a Py>,
    local_blob: &'a str,
    upstream_blob: &'a str,
}

impl Entry<'_> {
    fn workstream(&self) -> &'static str {
        workstream_for(self.path)
    }

    fn status(&self) -> &'static str {
        match self.classification.as_str() {
            "branding_only" | "policy_only" => "cleared_non_runtime",
            "intentional_divergence" => "cleared_intentional_divergence",
            "functional" => "pending_review",
            "deferred" => "deferred_review",
            _ => "pending_classification",
        }
    }

    fn requirement(&self) -> &'static str {
        rust_requirement(self.path, &self.classification)
    }

    fn action(&self) -> &'static str {
        action_for(&self.classification, self.requirement())
    }

    fn source_text(&self, name: &str) -> String {
        self.source
            .map(|value| text(value, name, ""))
            .unwrap_or_default()
    }

    fn report(&self) -> Py {
        let (necessity, existing_coverage) = match self.classification.as_str() {
            "functional" => ("necessary_review", "claimed_by_shared_diff_classification"),
            "branding_only" | "policy_only" => (
                "not_runtime_required",
                "claimed_by_shared_diff_classification",
            ),
            "intentional_divergence" => (
                "approved_divergence",
                "claimed_by_shared_diff_classification",
            ),
            "deferred" => ("deferred", "not_established"),
            _ if self.source.is_some() => ("unknown", "claimed_by_shared_diff_classification"),
            _ => ("unknown", "unclaimed"),
        };
        let mut report = Py::dict([
            ("path", self.path.into()),
            ("workstream", self.workstream().into()),
            ("status", self.status().into()),
            ("classification", self.classification.as_str().into()),
            ("classification_path", self.source_text("path").into()),
            ("necessity", necessity.into()),
            ("rust_requirement", self.requirement().into()),
            ("existing_coverage", existing_coverage.into()),
            ("owner", self.source_text("owner").into()),
            (
                "ticket",
                self.source
                    .and_then(|value| value.get("ticket"))
                    .cloned()
                    .unwrap_or_else(|| "".into()),
            ),
            ("action", self.action().into()),
            ("local_blob", self.local_blob.into()),
            ("upstream_blob", self.upstream_blob.into()),
        ]);
        if self.classification == "deferred" {
            for field in ["rationale", "ledger", "review_date"] {
                report.set(field, self.source_text(field).into());
            }
        }
        report
    }
}

const STATUS_METRICS: &[(&str, &str)] = &[
    ("pending_classification", "Pending classification"),
    ("pending_review", "Pending functional review"),
    ("deferred_review", "Deferred review (counted, not cleared)"),
    ("cleared_non_runtime", "Cleared non-runtime"),
    (
        "cleared_intentional_divergence",
        "Cleared intentional divergence",
    ),
];

#[derive(Default)]
struct Summary<'a> {
    status: BTreeMap<&'static str, usize>,
    workstream: BTreeMap<&'static str, usize>,
    classification: BTreeMap<&'a str, usize>,
    requirement: BTreeMap<&'static str, usize>,
}

impl<'a> Summary<'a> {
    fn build(entries: &'a [Entry<'_>]) -> Self {
        let mut result = Self::default();
        for entry in entries {
            *result.status.entry(entry.status()).or_default() += 1;
            *result.workstream.entry(entry.workstream()).or_default() += 1;
            *result
                .classification
                .entry(&entry.classification)
                .or_default() += 1;
            *result.requirement.entry(entry.requirement()).or_default() += 1;
        }
        result
    }

    fn status_count(&self, key: &str) -> usize {
        self.status.get(key).copied().unwrap_or(0)
    }

    fn report(&self, total: usize) -> Py {
        let mut report = Py::dict([("total_shared_different", total.into())]);
        for &(metric, _) in STATUS_METRICS {
            report.set(metric, self.status_count(metric).into());
        }
        for (key, counts) in [
            ("by_status", &self.status),
            ("by_workstream", &self.workstream),
            ("by_classification", &self.classification),
            ("by_rust_requirement", &self.requirement),
        ] {
            report.set(
                key,
                Py::dict(counts.iter().map(|(&label, &count)| (label, count.into()))),
            );
        }
        report
    }
}

fn render_markdown(generated: &str, entries: &[Entry<'_>], summary: &Summary<'_>) -> String {
    let mut report = format!("# Shared-Different Backlog\n\nGenerated: `{generated}`\n\n## Summary\n\n- Total shared-different paths: `{}`\n", entries.len());
    for (metric, title) in STATUS_METRICS {
        writeln!(report, "- {title}: `{}`", summary.status_count(metric)).unwrap();
    }
    for (heading, label, counts) in [
        ("Status", "Status", &summary.status),
        ("Workstream", "Workstream", &summary.workstream),
    ] {
        writeln!(
            report,
            "\n## {heading} Counts\n\n| {label} | Count |\n| --- | ---: |"
        )
        .unwrap();
        for (key, count) in counts {
            writeln!(report, "| `{key}` | {count} |").unwrap();
        }
    }
    report.push_str(
        "\n## Pending Classification\n\n| Path | Workstream | Action |\n| --- | --- | --- |\n",
    );
    for entry in entries
        .iter()
        .filter(|entry| entry.status() == "pending_classification")
    {
        writeln!(
            report,
            "| `{}` | `{}` | {} |",
            entry.path,
            entry.workstream(),
            entry.action()
        )
        .unwrap();
    }
    report.push_str("\n## Pending Functional Review By Prefix\n\n| Classification Path | Count |\n| --- | ---: |\n");
    let mut counts = BTreeMap::new();
    for (index, entry) in entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.status() == "pending_review")
    {
        counts
            .entry(entry.source_text("path"))
            .or_insert((index, 0))
            .1 += 1;
    }
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_by_key(|(_, (index, count))| (Reverse(*count), *index));
    for (prefix, (_, count)) in counts {
        writeln!(report, "| `{prefix}` | {count} |").unwrap();
    }
    report.push_str("\n## Deferred\n\nNot yet shown to match in Rust; each row is counted and has a review date.\n\n| Path | Review date | Ledger | Reason |\n| --- | --- | --- | --- |\n");
    for entry in entries
        .iter()
        .filter(|entry| entry.status() == "deferred_review")
    {
        writeln!(
            report,
            "| `{}` | {} | {} | {} |",
            entry.path,
            entry.source_text("review_date"),
            entry.source_text("ledger"),
            entry.source_text("rationale")
        )
        .unwrap();
    }
    report
}

pub fn run(args: SharedDiffBacklogArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    ensure_remote(&root, &args.upstream_remote, &args.upstream_url)?;
    if !args.no_fetch {
        let branch = &args.upstream_branch;
        let remote = &args.upstream_remote;
        let refspec = format!("+refs/heads/{branch}:refs/remotes/{remote}/{branch}");
        git_text(&root, &["fetch", "--no-tags", remote, &refspec], true)?;
    }

    let classification = resolve_argument(&py::join(&root, &args.classification))?;
    let document = fs::read_to_string(&classification)
        .map_err(|error| format!("{}: {error}", classification.display()))?;
    let document = py::loads(&document)?;
    if !document.is_dict() {
        return Err("expected a classification object".to_owned());
    }
    let items: &[Py] = match document.get("items") {
        Some(Py::List(items)) if items.iter().all(Py::is_dict) => items,
        Some(_) => return Err("expected classification items to be objects".to_owned()),
        None => &[],
    };
    let upstream_ref = format!("{}/{}", args.upstream_remote, args.upstream_branch);
    let local = ls_tree_blobs(&root, &args.local_ref, false)?;
    let upstream = ls_tree_blobs(&root, &upstream_ref, false)?;
    let entries: Vec<_> = local
        .iter()
        .filter_map(|(path, local_blob)| {
            let upstream_blob = upstream
                .get(path)
                .filter(|upstream_blob| *upstream_blob != local_blob)?;
            let source = longest_prefix_match(path, items);
            let classification = source.map_or_else(
                || "unclassified".to_owned(),
                |item| text(item, "classification", "unclassified"),
            );
            Some(Entry {
                path,
                classification,
                source,
                local_blob,
                upstream_blob,
            })
        })
        .collect();
    let summary = Summary::build(&entries);
    let generated = py::utc_isoformat();
    let refs = Py::dict([
        ("local_ref", args.local_ref.as_str().into()),
        (
            "local_sha",
            git_text(&root, &["rev-parse", &args.local_ref], true)?.into(),
        ),
        ("upstream_ref", upstream_ref.as_str().into()),
        (
            "upstream_sha",
            git_text(&root, &["rev-parse", &upstream_ref], true)?.into(),
        ),
    ]);
    let summary_json = summary.report(entries.len());
    let report = Py::dict([
        ("generated_at_utc", generated.as_str().into()),
        ("refs", refs),
        ("summary", summary_json.clone()),
        (
            "entries",
            Py::List(entries.iter().map(Entry::report).collect()),
        ),
    ]);
    write_reports(
        &root,
        &args.out_json,
        &args.out_md,
        py::dumps(&report, Some(2), true) + "\n",
        render_markdown(&generated, &entries, &summary),
    )?;
    println!("{}", py::dumps(&summary_json, None, true));
    Ok(0)
}
