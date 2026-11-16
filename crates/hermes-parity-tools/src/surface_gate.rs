//! Fail when required upstream surface files are missing locally. Port of
//! `scripts/run-upstream-surface-coverage-gate.py`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use serde_json::Value;

use crate::git::{git, objects_exist};
use crate::py::{self, Py};

const DEFAULT_DIVERGENCE_FILE: &str = "docs/parity/intentional-divergence.json";
const DEFAULT_PREFIXES: [&str; 6] = [
    "skills",
    "optional-skills",
    "plugins",
    "website",
    "ui-tui",
    "docs",
];
const RUST_ONLY_DENY_PREFIXES: [&str; 4] = ["tests", "tests/", "test", "test/"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum LocalMode {
    Ref,
    Worktree,
}

impl LocalMode {
    fn as_str(self) -> &'static str {
        match self {
            LocalMode::Ref => "ref",
            LocalMode::Worktree => "worktree",
        }
    }
}

#[derive(Args, Debug)]
pub struct SurfaceGateArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Local ref to validate
    #[arg(long, default_value = "HEAD")]
    local_ref: String,
    /// Check files against a git ref or current worktree paths
    #[arg(long, value_enum, default_value = "ref")]
    local_mode: LocalMode,
    /// Upstream ref to compare against
    #[arg(long, default_value = "upstream/main")]
    upstream_ref: String,
    /// Required prefix to validate (repeatable). Defaults to core upstream surfaces.
    #[arg(long)]
    prefix: Vec<String>,
    /// Optional explicit report output path
    #[arg(long, default_value = "")]
    report_path: String,
    /// Approved divergence registry used to classify non-applicable missing
    /// upstream paths. Pass an empty string to disable.
    #[arg(long, default_value = DEFAULT_DIVERGENCE_FILE)]
    intentional_divergence: String,
    /// Allow parity coverage checks over upstream test prefixes. Default is
    /// strict Rust-only parity mode (tests blocked).
    #[arg(long)]
    allow_python_test_surfaces: bool,
    /// Print JSON report
    #[arg(long)]
    json: bool,
}

/// `prefix.strip().lstrip("./")` with doubled slashes collapsed: any run of
/// leading dots and slashes goes, as Python's character-set `lstrip` does.
pub fn normalize_prefix(prefix: &str) -> String {
    let mut value = py::strip(prefix).trim_start_matches(['.', '/']).to_string();
    while value.contains("//") {
        value = value.replace("//", "/");
    }
    value
}

pub fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    let norm = normalize_prefix(prefix);
    let norm = norm.trim_end_matches('/');
    if norm.is_empty() {
        return false;
    }
    path == norm
        || path
            .strip_prefix(norm)
            .is_some_and(|rest| rest.starts_with('/'))
}

pub fn rust_only_prefix_violations(prefixes: &[String]) -> Vec<String> {
    let mut violations: Vec<String> = prefixes
        .iter()
        .filter(|raw| {
            let prefix = normalize_prefix(raw);
            !prefix.is_empty()
                && RUST_ONLY_DENY_PREFIXES
                    .iter()
                    .any(|deny| prefix == *deny || prefix.starts_with(deny))
        })
        .cloned()
        .collect();
    violations.sort();
    violations.dedup();
    violations
}

#[derive(Debug, Clone)]
pub struct DivergenceItem {
    pub id: String,
    pub status: String,
    pub workstream: String,
    pub path_prefixes: Vec<String>,
}

pub fn load_divergence_items(
    repo_root: &Path,
    divergence_file: &str,
) -> Result<Vec<DivergenceItem>, String> {
    if py::strip(divergence_file).is_empty() {
        return Ok(Vec::new());
    }
    let path = py::resolve(&py::join(repo_root, divergence_file));
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let raw: Value = serde_json::from_str(&text)
        .map_err(|e| format!("invalid JSON in {}: {e}", path.display()))?;
    let Value::Object(raw) = raw else {
        return Err(format!(
            "invalid intentional divergence payload: {}",
            path.display()
        ));
    };
    let items = match raw.get("items") {
        None => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => {
            return Err(format!(
                "invalid intentional divergence payload: {}",
                py::path_str(&path)
            ))
        }
    };
    let field = |item: &serde_json::Map<String, Value>, key: &str| {
        item.get(key).map(py::str_of).unwrap_or_default()
    };
    Ok(items
        .iter()
        .filter_map(|item| {
            let item = item.as_object()?;
            let status = item.get("status")?.as_str()?;
            if status != "approved" && status != "temporary" {
                return None;
            }
            let prefixes = match item.get("path_prefixes") {
                None => Vec::new(),
                Some(Value::Array(prefixes)) => prefixes.iter().map(py::str_of).collect(),
                Some(_) => return None,
            };
            Some(DivergenceItem {
                id: field(item, "id"),
                status: status.to_string(),
                workstream: field(item, "workstream"),
                path_prefixes: prefixes,
            })
        })
        .collect())
}

pub struct Owner<'a> {
    item: &'a DivergenceItem,
    matched_prefix: String,
}

pub fn find_divergence_owner<'a>(path: &str, items: &'a [DivergenceItem]) -> Option<Owner<'a>> {
    items.iter().find_map(|item| {
        item.path_prefixes
            .iter()
            .find(|prefix| path_matches_prefix(path, prefix))
            .map(|prefix| Owner {
                item,
                matched_prefix: normalize_prefix(prefix).trim_end_matches('/').to_string(),
            })
    })
}

fn list_files(repo_root: &Path, rev: &str, prefix: &str) -> Result<Vec<String>, String> {
    let out = git(repo_root, &["ls-tree", "-r", "--name-only", rev, prefix])?;
    Ok(out
        .stdout
        .lines()
        .map(py::strip)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// The upstream file listing and local presence check `build_report` reads.
pub trait Trees {
    fn upstream_files(&self, prefix: &str) -> Result<Vec<String>, String>;
    fn present_locally(&self, paths: &[String]) -> Result<Vec<bool>, String>;
}

struct GitTrees<'a> {
    repo_root: &'a Path,
    local_ref: &'a str,
    local_mode: LocalMode,
    upstream_ref: &'a str,
}

impl Trees for GitTrees<'_> {
    fn upstream_files(&self, prefix: &str) -> Result<Vec<String>, String> {
        list_files(self.repo_root, self.upstream_ref, prefix)
    }

    fn present_locally(&self, paths: &[String]) -> Result<Vec<bool>, String> {
        match self.local_mode {
            LocalMode::Worktree => Ok(paths
                .iter()
                .map(|path| self.repo_root.join(path).exists())
                .collect()),
            LocalMode::Ref => objects_exist(self.repo_root, self.local_ref, paths),
        }
    }
}

pub struct ReportInput<'a> {
    pub repo_root: &'a Path,
    pub local_ref: &'a str,
    pub local_mode: LocalMode,
    pub upstream_ref: &'a str,
    pub prefixes: &'a [String],
    pub divergence_items: &'a [DivergenceItem],
}

fn ratio(part: usize, whole: usize) -> Py {
    Py::Float(if whole == 0 {
        1.0
    } else {
        part as f64 / whole as f64
    })
}

pub fn build_report(input: &ReportInput, trees: &dyn Trees) -> Result<Py, String> {
    let mut by_prefix = Py::Dict(Vec::new());
    let mut missing_total: Vec<String> = Vec::new();
    let mut raw_missing_total = 0usize;
    let mut divergence_paths: Vec<(String, Py)> = Vec::new();
    let mut divergence_counts: BTreeMap<String, usize> = BTreeMap::new();
    // Per prefix, as the report keeps it: a repeated prefix replaces its entry.
    let mut prefix_counts: Vec<(String, [usize; 2])> = Vec::new();

    for prefix in input.prefixes {
        let upstream_files = trees.upstream_files(prefix)?;
        let present = trees.present_locally(&upstream_files)?;
        let raw_missing: Vec<&String> = upstream_files
            .iter()
            .zip(&present)
            .filter(|(_, here)| !**here)
            .map(|(path, _)| path)
            .collect();
        let mut actionable: Vec<String> = Vec::new();
        let mut diverged: Vec<Py> = Vec::new();
        for path in &raw_missing {
            match find_divergence_owner(path, input.divergence_items) {
                None => actionable.push((*path).clone()),
                Some(owner) => {
                    let entry = Py::dict([
                        ("path", Py::from(path.as_str())),
                        ("id", Py::from(owner.item.id.as_str())),
                        ("status", Py::from(owner.item.status.as_str())),
                        ("workstream", Py::from(owner.item.workstream.as_str())),
                        ("matched_prefix", Py::from(owner.matched_prefix)),
                    ]);
                    divergence_paths.push(((*path).clone(), entry.clone()));
                    diverged.push(entry);
                    *divergence_counts.entry(owner.item.id.clone()).or_default() += 1;
                }
            }
        }
        let total = upstream_files.len();
        let present_count = total - raw_missing.len();
        let effective_present = total - actionable.len();
        by_prefix.set(
            prefix,
            Py::dict([
                ("upstream_file_count", Py::from(total)),
                ("present_locally", Py::from(present_count)),
                ("raw_missing_count", Py::from(raw_missing.len())),
                ("intentional_divergence_count", Py::from(diverged.len())),
                ("missing_count", Py::from(actionable.len())),
                ("coverage_ratio", ratio(present_count, total)),
                ("effective_coverage_ratio", ratio(effective_present, total)),
                ("missing_sample", Py::strs(actionable.iter().take(50))),
                (
                    "intentional_divergence_sample",
                    Py::List(diverged.into_iter().take(50).collect()),
                ),
            ]),
        );
        match prefix_counts.iter_mut().find(|(p, _)| p == prefix) {
            Some(slot) => slot.1 = [total, present_count],
            None => prefix_counts.push((prefix.clone(), [total, present_count])),
        }
        raw_missing_total += raw_missing.len();
        missing_total.extend(actionable);
    }

    let summary = Py::dict([
        ("prefixes_checked", Py::strs(input.prefixes)),
        (
            "upstream_file_count",
            Py::from(prefix_counts.iter().map(|(_, c)| c[0]).sum::<usize>()),
        ),
        (
            "present_locally",
            Py::from(prefix_counts.iter().map(|(_, c)| c[1]).sum::<usize>()),
        ),
        ("raw_missing_total", Py::from(raw_missing_total)),
        (
            "intentional_divergence_total",
            Py::from(divergence_paths.len()),
        ),
        ("missing_total", Py::from(missing_total.len())),
    ]);
    let ok = missing_total.is_empty();
    missing_total.sort();
    // Python's sort is stable, so equal paths keep their order.
    divergence_paths.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(Py::dict([
        ("generated_at", Py::from(py::utc_isoformat())),
        ("repo_root", Py::from(py::path_str(input.repo_root))),
        ("local_ref", Py::from(input.local_ref)),
        ("local_mode", Py::from(input.local_mode.as_str())),
        ("upstream_ref", Py::from(input.upstream_ref)),
        ("ok", Py::from(ok)),
        ("summary", summary),
        ("by_prefix", by_prefix),
        ("missing_paths", Py::strs(&missing_total)),
        (
            "intentional_divergence_paths",
            Py::List(divergence_paths.into_iter().map(|(_, e)| e).collect()),
        ),
        (
            "intentional_divergence_by_id",
            Py::Dict(
                divergence_counts
                    .into_iter()
                    .map(|(id, n)| (id, Py::from(n)))
                    .collect(),
            ),
        ),
    ]))
}

fn report_path(args: &SurfaceGateArgs, repo_root: &Path) -> Result<PathBuf, String> {
    let path = if args.report_path.is_empty() {
        repo_root.join(".sync-reports").join(format!(
            "upstream-surface-coverage-gate-{}.json",
            py::utc_stamp()
        ))
    } else {
        py::resolve(&py::expanduser(&args.report_path))
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    Ok(path)
}

fn write_and_print(
    args: &SurfaceGateArgs,
    mut report: Py,
    path: &Path,
    summary_line: &str,
) -> Result<(), String> {
    std::fs::write(path, py::dumps(&report, Some(2), false) + "\n")
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    report.set("report_path", Py::from(py::path_str(path)));
    if args.json {
        println!("{}", py::dumps(&report, Some(2), false));
    } else {
        println!("[upstream-surface-coverage] {summary_line}");
        println!("[upstream-surface-coverage] Report: {}", py::path_str(path));
    }
    Ok(())
}

pub fn run(args: SurfaceGateArgs) -> Result<i32, String> {
    let repo_root = py::resolve(&py::expanduser(&args.repo_root));
    if !repo_root.exists() {
        return Err(format!(
            "repo root does not exist: {}",
            py::path_str(&repo_root)
        ));
    }
    let prefixes: Vec<String> = if args.prefix.is_empty() {
        DEFAULT_PREFIXES.iter().map(|p| p.to_string()).collect()
    } else {
        args.prefix.clone()
    };
    let violations = if args.allow_python_test_surfaces {
        Vec::new()
    } else {
        rust_only_prefix_violations(&prefixes)
    };
    if !violations.is_empty() {
        let report = Py::dict([
            ("generated_at", Py::from(py::utc_isoformat())),
            ("repo_root", Py::from(py::path_str(&repo_root))),
            ("local_ref", Py::from(args.local_ref.as_str())),
            ("local_mode", Py::from(args.local_mode.as_str())),
            ("upstream_ref", Py::from(args.upstream_ref.as_str())),
            ("ok", Py::from(false)),
            ("reason", Py::from("rust_only_parity_guard_blocked_prefix")),
            (
                "policy",
                Py::dict([
                    ("rust_only", Py::from(true)),
                    (
                        "allow_python_test_surfaces",
                        Py::from(args.allow_python_test_surfaces),
                    ),
                    ("blocked_prefixes", Py::strs(&violations)),
                ]),
            ),
        ]);
        let path = report_path(&args, &repo_root)?;
        write_and_print(
            &args,
            report,
            &path,
            "FAILED (rust-only parity guard blocked test prefixes)",
        )?;
        return Ok(1);
    }

    let divergence_items = load_divergence_items(&repo_root, &args.intentional_divergence)?;
    let trees = GitTrees {
        repo_root: &repo_root,
        local_ref: &args.local_ref,
        local_mode: args.local_mode,
        upstream_ref: &args.upstream_ref,
    };
    let mut report = build_report(
        &ReportInput {
            repo_root: &repo_root,
            local_ref: &args.local_ref,
            local_mode: args.local_mode,
            upstream_ref: &args.upstream_ref,
            prefixes: &prefixes,
            divergence_items: &divergence_items,
        },
        &trees,
    )?;
    report.set(
        "policy",
        Py::dict([
            ("rust_only", Py::from(true)),
            (
                "allow_python_test_surfaces",
                Py::from(args.allow_python_test_surfaces),
            ),
            ("blocked_prefixes", Py::List(Vec::new())),
            (
                "intentional_divergence",
                Py::from(args.intentional_divergence.as_str()),
            ),
            ("active_divergence_items", Py::from(divergence_items.len())),
        ]),
    );
    let ok = report.get("ok") == Some(&Py::Bool(true));
    let count = |key: &str| match report.get("summary").and_then(|s| s.get(key)) {
        Some(Py::Int(n)) => *n,
        _ => 0,
    };
    let line = format!(
        "{} (missing={} checked={})",
        if ok { "PASSED" } else { "FAILED" },
        count("missing_total"),
        count("upstream_file_count")
    );
    let path = report_path(&args, &repo_root)?;
    write_and_print(&args, report, &path, &line)?;
    Ok(if ok { 0 } else { 1 })
}

#[cfg(test)]
mod tests;
