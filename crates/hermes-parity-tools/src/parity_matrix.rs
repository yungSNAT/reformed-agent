//! Compare Git trees and patch equivalence across local and upstream history.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use clap::Args;
use regex::Regex;

use crate::decimal_limit::{self, DecimalLimit};
use crate::git::{
    ensure_remote, fetch_remote_branch, git_text, ls_tree_blobs, python_lines, python_words,
};
use crate::py::{self, Py};
use crate::report_io::{resolve_argument, write_report};

mod markdown;

#[derive(Args, Debug)]
pub struct ParityMatrixArgs {
    /// Path to repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Upstream remote name
    #[arg(long, default_value = "upstream")]
    upstream_remote: String,
    /// Upstream URL used when the remote is missing
    #[arg(
        long,
        default_value = "https://github.com/NousResearch/hermes-agent.git"
    )]
    upstream_url: String,
    /// Local ref to compare
    #[arg(long, default_value = "main")]
    local_ref: String,
    /// Upstream branch name
    #[arg(long, default_value = "main")]
    upstream_branch: String,
    /// Top bucket rows per section
    #[arg(long, default_value = "40", allow_hyphen_values = true, value_parser = decimal_limit::parse)]
    top_n: DecimalLimit,
    /// Do not fetch upstream before computing
    #[arg(long)]
    no_fetch: bool,
    /// Intentional divergence registry relative to repository root
    #[arg(long, default_value = "docs/parity/intentional-divergence.json")]
    intentional_divergence: String,
    /// Output JSON path relative to repository root
    #[arg(long, default_value = "docs/parity/parity-matrix.json")]
    output_json: String,
    /// Output Markdown path relative to repository root
    #[arg(long, default_value = "docs/parity/parity-matrix.md")]
    output_md: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Risk {
    Medium,
    High,
    Critical,
}

impl Risk {
    fn label(self) -> &'static str {
        match self {
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    fn with_gap(self, count: usize) -> Self {
        match count {
            1000.. => Self::Critical,
            400.. => self.max(Self::High),
            120.. => self.max(Self::Medium),
            _ => self,
        }
    }
}

struct Workstream {
    issue: usize,
    name: &'static str,
    risk: Risk,
    upstream_only: usize,
    shared_different: usize,
    local_only: usize,
}

impl Workstream {
    fn new(issue: usize, name: &'static str, risk: Risk) -> Self {
        Self {
            issue,
            name,
            risk,
            upstream_only: 0,
            shared_different: 0,
            local_only: 0,
        }
    }

    fn gap(&self) -> usize {
        self.upstream_only + self.shared_different
    }

    fn report(&self, id: &str) -> Py {
        let effort = match self.gap() {
            0..=25 => "S",
            26..=100 => "M",
            101..=400 => "L",
            _ => "XL",
        };
        Py::dict([
            ("workstream", id.into()),
            ("issue", self.issue.into()),
            ("name", self.name.into()),
            ("upstream_only", self.upstream_only.into()),
            ("shared_different", self.shared_different.into()),
            ("local_only", self.local_only.into()),
            ("risk", self.risk.with_gap(self.gap()).label().into()),
            ("effort", effort.into()),
        ])
    }
}

const PATH_TO_WORKSTREAM: &[(&str, &str)] = &[
    ("crates/hermes-agent/src/memory_plugins", "WS3"),
    ("crates/hermes-agent/src/runtime", "WS2"),
    ("crates/hermes-agent/src/config", "WS2"),
    ("crates/hermes-agent/src/gateway", "WS2"),
    ("crates/hermes-agent", "WS2"),
    ("crates/hermes-cli", "WS2"),
    ("crates/hermes-config", "WS2"),
    ("crates/hermes-gateway", "WS2"),
    ("crates/hermes-tools", "WS3"),
    ("crates/hermes-platform", "WS3"),
    ("crates/hermes-plugins", "WS3"),
    ("crates/hermes-mcp", "WS3"),
    ("crates/hermes-secrets", "WS7"),
    ("crates/hermes-security", "WS7"),
    ("scripts/upstream_webhook_sync.py", "WS7"),
    ("scripts/run-upstream-webhook", "WS7"),
    ("scripts/install-upstream-webhook", "WS7"),
    ("scripts/setup-upstream-webhook", "WS7"),
    ("scripts/sync-upstream.sh", "WS7"),
    ("scripts/cron-upstream-sync.sh", "WS7"),
    ("crates/hermes-parity-tools/src/sync_upstream.rs", "WS7"),
    (
        "crates/hermes-parity-tools/src/upstream_sync_cron.rs",
        "WS7",
    ),
    ("scripts/upstream-risk-paths.txt", "WS7"),
    ("gateway", "WS2"),
    ("plugins", "WS3"),
    ("tools", "WS3"),
    ("environments", "WS3"),
    ("web", "WS5"),
    ("packaging", "WS8"),
    ("tests", "WS6"),
    (".github/workflows", "WS6"),
    ("skills", "WS4"),
    ("optional-skills", "WS4"),
    ("ui-tui", "WS5"),
    ("website", "WS5"),
    ("docs", "WS5"),
];

pub(crate) fn path_matches(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

pub(crate) fn workstream_for(path: &str) -> &'static str {
    PATH_TO_WORKSTREAM
        .iter()
        .find_map(|&(prefix, id)| path_matches(path, prefix).then_some(id))
        .unwrap_or("WS8")
}

fn workstream_matrix(trees: &TreeComparison<'_>) -> Py {
    use Risk::{Critical, High, Medium};
    let mut rows = BTreeMap::from([
        ("WS2", Workstream::new(6, "Core runtime parity", Critical)),
        ("WS3", Workstream::new(7, "Tools and adapters parity", High)),
        ("WS4", Workstream::new(8, "Skills parity", Medium)),
        ("WS5", Workstream::new(9, "UX parity", Medium)),
        ("WS6", Workstream::new(10, "Tests and CI parity", High)),
        (
            "WS7",
            Workstream::new(11, "Security/secrets/store/webhook parity", Critical),
        ),
        (
            "WS8",
            Workstream::new(12, "Compatibility and divergence policy", Medium),
        ),
    ]);
    for path in &trees.upstream_only {
        rows.get_mut(workstream_for(path)).unwrap().upstream_only += 1;
    }
    for path in &trees.local_only {
        rows.get_mut(workstream_for(path)).unwrap().local_only += 1;
    }
    for path in &trees.shared_different {
        rows.get_mut(workstream_for(path)).unwrap().shared_different += 1;
    }
    let mut rows: Vec<_> = rows.into_iter().collect();
    rows.sort_by_key(|(id, row)| (Reverse(row.gap()), *id));
    Py::List(rows.iter().map(|(id, row)| row.report(id)).collect())
}

struct TreeComparison<'a> {
    upstream_only: Vec<&'a str>,
    local_only: Vec<&'a str>,
    shared_different: Vec<&'a str>,
    shared_identical: usize,
}

impl<'a> TreeComparison<'a> {
    fn compare(
        local: &'a BTreeMap<String, String>,
        upstream: &'a BTreeMap<String, String>,
    ) -> Self {
        let mut result = Self {
            upstream_only: Vec::new(),
            local_only: Vec::new(),
            shared_different: Vec::new(),
            shared_identical: 0,
        };
        for (path, local_hash) in local {
            match upstream.get(path) {
                None => result.local_only.push(path),
                Some(upstream_hash) if local_hash == upstream_hash => result.shared_identical += 1,
                Some(_) => result.shared_different.push(path),
            }
        }
        result.upstream_only = upstream
            .keys()
            .filter(|path| !local.contains_key(*path))
            .map(String::as_str)
            .collect();
        result
    }
}

fn top_buckets(paths: &[&str], limit: usize) -> Py {
    // Counter.most_common preserves the first encounter when counts tie.
    let mut positions = HashMap::new();
    let mut buckets: Vec<(&str, usize)> = Vec::new();
    for path in paths
        .iter()
        .map(|path| py::strip(path))
        .filter(|path| !path.is_empty())
    {
        let bucket = path
            .match_indices('/')
            .nth(1)
            .map_or(path, |(end, _)| &path[..end]);
        let position = *positions.entry(bucket).or_insert_with(|| {
            buckets.push((bucket, 0));
            buckets.len() - 1
        });
        buckets[position].1 += 1;
    }
    buckets.sort_by_key(|&(_, count)| Reverse(count));
    Py::List(
        buckets
            .into_iter()
            .take(limit)
            .map(|(bucket, count)| Py::dict([("path", bucket.into()), ("count", count.into())]))
            .collect(),
    )
}

#[derive(Default)]
struct PatchCounts {
    missing: Vec<String>,
    represented: Vec<String>,
}

fn cherry(root: &Path, upstream: &str, local: &str) -> Result<PatchCounts, String> {
    let text = git_text(root, &["cherry", upstream, local], false)?;
    let mut counts = PatchCounts::default();
    for line in python_lines(&text) {
        let mut parts = python_words(line);
        match (parts.next(), parts.next(), parts.next()) {
            (Some("+"), Some(hash), None) => counts.missing.push(hash.to_owned()),
            (Some("-"), Some(hash), None) => counts.represented.push(hash.to_owned()),
            _ => (),
        }
    }
    Ok(counts)
}

fn diff_stats(root: &Path, local: &str, upstream: &str) -> Result<[u64; 3], String> {
    let mut text = git_text(root, &["diff", "--shortstat", local, upstream], false)?;
    if text.is_empty() {
        text = git_text(
            root,
            &["diff", "--shortstat", &format!("{local}...{upstream}")],
            false,
        )?;
    }
    let mut stats = [0; 3];
    for (slot, pattern) in stats.iter_mut().zip([
        r"(\d+)[\s\x1c-\x1f]+files? changed",
        r"(\d+)[\s\x1c-\x1f]+insertions?\(\+\)",
        r"(\d+)[\s\x1c-\x1f]+deletions?\(-\)",
    ]) {
        if let Some(capture) = Regex::new(pattern)
            .expect("valid shortstat pattern")
            .captures(&text)
        {
            *slot = capture[1]
                .parse::<u64>()
                .map_err(|error| format!("invalid shortstat: {error}"))?;
        }
    }
    Ok(stats)
}

fn divergence_coverage(
    path: &Path,
    trees: &TreeComparison<'_>,
) -> Result<(Vec<Py>, usize), String> {
    if !path.exists() {
        return Ok((Vec::new(), 0));
    }
    let file = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let registry = py::loads(&file)?;
    if !registry.is_dict() {
        return Err(format!("{}: expected a registry object", path.display()));
    }
    let Some(Py::List(items)) = registry.get("items") else {
        return Ok((Vec::new(), 0));
    };
    let candidate: BTreeSet<_> = trees
        .local_only
        .iter()
        .chain(&trees.upstream_only)
        .chain(&trees.shared_different)
        .copied()
        .collect();
    let mut covered = BTreeSet::new();
    let mut report = Vec::new();
    for item in items.iter().filter(|item| item.is_dict()) {
        let value =
            |key, default: &str| item.get(key).map_or_else(|| default.to_owned(), py::str_of);
        let prefixes: Vec<_> = match item.get("path_prefixes") {
            Some(Py::List(prefixes)) => prefixes
                .iter()
                .filter(|value| is_truthy(value))
                .map(py::str_of)
                .filter(|value| !py::strip(value).is_empty())
                .map(|value| value.trim_end_matches('/').to_owned())
                .collect(),
            _ => Vec::new(),
        };
        let matches: Vec<_> = candidate
            .iter()
            .copied()
            .filter(|path| prefixes.iter().any(|prefix| path_matches(path, prefix)))
            .collect();
        covered.extend(matches.iter().copied());
        report.push(Py::dict([
            ("id", value("id", "").into()),
            ("status", value("status", "").into()),
            ("workstream", value("workstream", "WS8").into()),
            ("summary", value("summary", "").into()),
            ("matched_files", matches.len().into()),
            ("matched_sample", Py::strs(matches.into_iter().take(20))),
        ]));
    }
    Ok((report, covered.len()))
}

fn is_truthy(value: &Py) -> bool {
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

pub fn run(args: ParityMatrixArgs) -> Result<i32, String> {
    let root = resolve_argument(Path::new(&args.repo_root))?;
    let local = &args.local_ref;
    let upstream = format!("{}/{}", args.upstream_remote, args.upstream_branch);
    ensure_remote(&root, &args.upstream_remote, &args.upstream_url)?;
    if !args.no_fetch {
        fetch_remote_branch(&root, &args.upstream_remote, &args.upstream_branch)?;
    }

    let local_sha = git_text(&root, &["rev-parse", local], true)?;
    let upstream_sha = git_text(&root, &["rev-parse", &upstream], true)?;
    let merge_base = git_text(&root, &["merge-base", local, &upstream], false)?;
    let counts = git_text(
        &root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{upstream}...{local}"),
        ],
        true,
    )?;
    let mut parts = python_words(&counts);
    let (Some(behind), Some(ahead), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!("unexpected rev-list output: {counts:?}"));
    };
    let (behind, ahead) = (
        behind.parse::<usize>().map_err(|error| error.to_string())?,
        ahead.parse::<usize>().map_err(|error| error.to_string())?,
    );
    let local_tree = ls_tree_blobs(&root, local, true)?;
    let upstream_tree = ls_tree_blobs(&root, &upstream, true)?;
    let trees = TreeComparison::compare(&local_tree, &upstream_tree);
    let [files_changed, insertions, deletions] = diff_stats(&root, local, &upstream)?;
    let upstream_patches = cherry(&root, local, &upstream)?;
    let local_patches = cherry(&root, &upstream, local)?;
    let registry = resolve_argument(&py::join(&root, &args.intentional_divergence))?;
    let (divergences, covered_files) = divergence_coverage(&registry, &trees)?;

    let top_upstream = top_buckets(&trees.upstream_only, args.top_n.count);
    let top_local = top_buckets(&trees.local_only, args.top_n.count);
    let report = Py::dict([
        ("generated_at_utc", py::utc_isoformat().into()),
        (
            "refs",
            Py::dict([
                ("local_ref", local.as_str().into()),
                ("local_sha", local_sha.into()),
                ("upstream_ref", upstream.into()),
                ("upstream_sha", upstream_sha.into()),
                (
                    "merge_base",
                    if merge_base.is_empty() {
                        Py::Null
                    } else {
                        merge_base.into()
                    },
                ),
            ]),
        ),
        (
            "summary",
            Py::dict([
                ("commits_behind", behind.into()),
                ("commits_ahead", ahead.into()),
                (
                    "upstream_patch_missing",
                    upstream_patches.missing.len().into(),
                ),
                (
                    "upstream_patch_represented",
                    upstream_patches.represented.len().into(),
                ),
                ("local_patch_unique", local_patches.missing.len().into()),
                ("files_only_upstream", trees.upstream_only.len().into()),
                ("files_only_local", trees.local_only.len().into()),
                ("files_shared_identical", trees.shared_identical.into()),
                (
                    "files_shared_different",
                    trees.shared_different.len().into(),
                ),
                ("tree_files_changed", Py::Int(i128::from(files_changed))),
                ("tree_insertions", Py::Int(i128::from(insertions))),
                ("tree_deletions", Py::Int(i128::from(deletions))),
            ]),
        ),
        ("top_upstream_only", top_upstream.clone()),
        (
            "top_shared_different",
            top_buckets(&trees.shared_different, args.top_n.count),
        ),
        ("top_local_only", top_local.clone()),
        ("top_missing_from_local", top_upstream),
        ("top_unique_to_local", top_local),
        ("workstream_matrix", workstream_matrix(&trees)),
        (
            "commit_mapping",
            Py::dict([
                (
                    "upstream_patch_missing",
                    upstream_patches.missing.len().into(),
                ),
                (
                    "upstream_patch_represented",
                    upstream_patches.represented.len().into(),
                ),
                ("local_patch_unique", local_patches.missing.len().into()),
                (
                    "local_patch_equivalent_to_upstream",
                    local_patches.represented.len().into(),
                ),
                (
                    "upstream_patch_missing_sample",
                    Py::strs(upstream_patches.missing.iter().take(20)),
                ),
                (
                    "upstream_patch_represented_sample",
                    Py::strs(upstream_patches.represented.iter().take(20)),
                ),
                (
                    "local_patch_unique_sample",
                    Py::strs(local_patches.missing.iter().take(20)),
                ),
                (
                    "local_patch_equivalent_sample",
                    Py::strs(local_patches.represented.iter().take(20)),
                ),
                ("intentional_divergence_items", divergences.len().into()),
                ("intentional_divergence_covered_files", covered_files.into()),
            ]),
        ),
        ("intentional_divergence", Py::List(divergences)),
    ]);
    let json_path = py::join(&root, &args.output_json);
    let md_path = py::join(&root, &args.output_md);
    for path in [&json_path, &md_path] {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
    }
    write_report(&json_path, py::dumps(&report, Some(2), false) + "\n")?;
    write_report(&md_path, markdown::render(&report, &args.top_n.label))?;
    println!("Wrote JSON: {}", py::path_str(&json_path));
    println!("Wrote Markdown: {}", py::path_str(&md_path));
    println!("Commits behind/ahead: {behind}/{ahead}");
    Ok(0)
}

#[cfg(test)]
#[path = "parity_matrix_coverage_gen.rs"]
mod coverage_gen;
