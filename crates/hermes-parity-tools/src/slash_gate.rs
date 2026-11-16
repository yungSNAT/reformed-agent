//! Fail when upstream slash-command surface drifts without explicit
//! divergence. Port of `scripts/run-upstream-slash-parity-gate.py`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::Args;
use regex::Regex;
use serde_json::Value;

use crate::git::git;
use crate::py::{self, Py};

#[derive(Args, Debug)]
pub struct SlashGateArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Local git ref
    #[arg(long, default_value = "HEAD")]
    local_ref: String,
    /// Upstream git ref
    #[arg(long, default_value = "upstream/main")]
    upstream_ref: String,
    /// Divergence allowlist JSON path
    #[arg(long, default_value = "docs/parity/slash-command-divergence.json")]
    allowlist: String,
    /// Optional explicit report output path
    #[arg(long, default_value = "")]
    report_path: String,
    /// Print JSON report
    #[arg(long)]
    json: bool,
}

fn git_ref_exists(repo_root: &Path, rev: &str) -> Result<bool, String> {
    Ok(git(repo_root, &["rev-parse", "--verify", rev])?.ok)
}

fn git_show_file(repo_root: &Path, rev: &str, path: &str) -> Result<Option<String>, String> {
    let out = git(repo_root, &["show", &format!("{rev}:{path}")])?;
    Ok(out.ok.then_some(out.stdout))
}

fn git_show_text_surface(
    repo_root: &Path,
    rev: &str,
    paths: &[&str],
) -> Result<Option<String>, String> {
    let mut parts = Vec::new();
    for path in paths {
        if path.ends_with(".rs") {
            if let Some(single) = git_show_file(repo_root, rev, path)? {
                parts.push(single);
                continue;
            }
        }
        let listed = git(
            repo_root,
            &["ls-tree", "-r", "--name-only", rev, "--", path],
        )?;
        if !listed.ok || py::strip(&listed.stdout).is_empty() {
            continue;
        }
        let mut files: Vec<&str> = listed
            .stdout
            .lines()
            .filter(|line| line.ends_with(".rs"))
            .collect();
        files.sort_unstable();
        for rel in files {
            if let Some(content) = git_show_file(repo_root, rev, rel)? {
                parts.push(content);
            }
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("\n")))
}

pub fn extract_upstream_slash_commands(py_source: &str) -> BTreeSet<String> {
    let pattern = Regex::new(r#"CommandDef\("([a-z0-9_-]+)""#).expect("valid regex");
    pattern
        .captures_iter(py_source)
        .map(|c| format!("/{}", &c[1]))
        .collect()
}

pub fn extract_local_slash_commands(rs_source: &str) -> BTreeSet<String> {
    let Some(start) = rs_source.find("pub const SLASH_COMMANDS") else {
        return BTreeSet::new();
    };
    let Some(len) = rs_source[start..].find("];") else {
        return BTreeSet::new();
    };
    let block = &rs_source[start..start + len];
    let pattern = Regex::new(r#""(/[^"]+)""#).expect("valid regex");
    pattern
        .captures_iter(block)
        .map(|c| c[1].to_string())
        .collect()
}

/// `load_allowlist`: the `allowed_missing_upstream_commands` list of a JSON
/// object, or a bare list; iterating as Python does, so a string yields its
/// characters and an object its keys.
pub fn load_allowlist(path: &Path) -> Result<BTreeSet<String>, String> {
    if !path.exists() {
        return Ok(BTreeSet::new());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let raw: Value = serde_json::from_str(&text)
        .map_err(|e| format!("invalid JSON in {}: {e}", path.display()))?;
    let values = match &raw {
        Value::Object(map) => map
            .get("allowed_missing_upstream_commands")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
        Value::Array(_) => raw.clone(),
        _ => Value::Array(Vec::new()),
    };
    let items: Vec<String> = match values {
        Value::Array(items) => items.iter().map(py::str_of).collect(),
        Value::String(s) => s.chars().map(String::from).collect(),
        Value::Object(map) => map.keys().cloned().collect(),
        other => {
            return Err(format!(
                "'{}' object is not iterable",
                match other {
                    Value::Null => "NoneType",
                    Value::Bool(_) => "bool",
                    _ => "number",
                }
            ))
        }
    };
    Ok(items
        .into_iter()
        .filter(|v| !py::strip(v).is_empty())
        .collect())
}

fn default_report_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sync-reports").join(format!(
        "upstream-slash-parity-gate-{}.json",
        py::utc_stamp()
    ))
}

fn write_report(path: &Path, report: &Py) -> Result<(), String> {
    std::fs::write(path, py::dumps(report, Some(2), false) + "\n")
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

fn fail(args: &SlashGateArgs, report: &Py, report_path: &Path, what: &str) -> Result<i32, String> {
    write_report(report_path, report)?;
    if args.json {
        println!("{}", py::dumps(report, Some(2), false));
    } else {
        println!("[upstream-slash-parity] FAILED ({what})");
        println!(
            "[upstream-slash-parity] Report: {}",
            py::path_str(report_path)
        );
    }
    Ok(1)
}

pub fn run(args: SlashGateArgs) -> Result<i32, String> {
    let repo_root = py::resolve(&py::expanduser(&args.repo_root));
    let allowlist_path = py::join(&repo_root, &args.allowlist);
    let report_path = if args.report_path.is_empty() {
        default_report_path(&repo_root)
    } else {
        py::resolve(&py::expanduser(&args.report_path))
    };
    if let Some(parent) = report_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }

    let refs_ok = git_ref_exists(&repo_root, &args.local_ref)?
        && git_ref_exists(&repo_root, &args.upstream_ref)?;
    if !refs_ok {
        let report = Py::dict([
            ("generated_at", Py::from(py::utc_isoformat())),
            ("ok", Py::from(false)),
            ("reason", Py::from("missing_git_ref")),
            ("local_ref", Py::from(args.local_ref.as_str())),
            ("upstream_ref", Py::from(args.upstream_ref.as_str())),
        ]);
        return fail(&args, &report, &report_path, "missing git ref");
    }

    let upstream_source = git_show_file(&repo_root, &args.upstream_ref, "hermes_cli/commands.py")?;
    let local_source = git_show_text_surface(
        &repo_root,
        &args.local_ref,
        &[
            "crates/hermes-cli/src/commands.rs",
            "crates/hermes-cli/src/commands",
        ],
    )?;
    let (Some(upstream_source), Some(local_source)) = (&upstream_source, &local_source) else {
        let report = Py::dict([
            ("generated_at", Py::from(py::utc_isoformat())),
            ("ok", Py::from(false)),
            ("reason", Py::from("missing_surface_file")),
            ("local_ref", Py::from(args.local_ref.as_str())),
            ("upstream_ref", Py::from(args.upstream_ref.as_str())),
            (
                "missing_upstream_source",
                Py::from(upstream_source.is_none()),
            ),
            ("missing_local_source", Py::from(local_source.is_none())),
        ]);
        return fail(&args, &report, &report_path, "missing source file");
    };

    let upstream_cmds = extract_upstream_slash_commands(upstream_source);
    let local_cmds = extract_local_slash_commands(local_source);
    let allowlisted = load_allowlist(&allowlist_path)?;

    let missing_all: Vec<&String> = upstream_cmds.difference(&local_cmds).collect();
    let missing_unallowlisted: Vec<&String> = missing_all
        .iter()
        .copied()
        .filter(|cmd| !allowlisted.contains(*cmd))
        .collect();
    let extra_local: Vec<&String> = local_cmds.difference(&upstream_cmds).collect();
    let stale_allowlist: Vec<&String> = allowlisted
        .iter()
        .filter(|cmd| local_cmds.contains(*cmd))
        .collect();
    let allowlisted_missing: Vec<&String> = missing_all
        .iter()
        .copied()
        .filter(|cmd| allowlisted.contains(*cmd))
        .collect();

    let ok = missing_unallowlisted.is_empty();
    let mut report = Py::dict([
        ("generated_at", Py::from(py::utc_isoformat())),
        ("ok", Py::from(ok)),
        ("local_ref", Py::from(args.local_ref.as_str())),
        ("upstream_ref", Py::from(args.upstream_ref.as_str())),
        ("allowlist_path", Py::from(py::path_str(&allowlist_path))),
        (
            "counts",
            Py::dict([
                ("upstream", Py::from(upstream_cmds.len())),
                ("local", Py::from(local_cmds.len())),
                ("missing_total", Py::from(missing_all.len())),
                (
                    "missing_unallowlisted",
                    Py::from(missing_unallowlisted.len()),
                ),
                ("local_extra", Py::from(extra_local.len())),
                ("stale_allowlist_entries", Py::from(stale_allowlist.len())),
            ]),
        ),
        ("missing_total", Py::strs(&missing_all)),
        ("missing_unallowlisted", Py::strs(&missing_unallowlisted)),
        ("allowlisted_missing", Py::strs(&allowlisted_missing)),
        ("local_extra", Py::strs(&extra_local)),
        ("stale_allowlist_entries", Py::strs(&stale_allowlist)),
    ]);
    write_report(&report_path, &report)?;
    report.set("report_path", Py::from(py::path_str(&report_path)));

    if args.json {
        println!("{}", py::dumps(&report, Some(2), false));
    } else {
        println!(
            "[upstream-slash-parity] {} (missing_unallowlisted={} missing_total={} local_extra={})",
            if ok { "PASSED" } else { "FAILED" },
            missing_unallowlisted.len(),
            missing_all.len(),
            extra_local.len()
        );
        println!(
            "[upstream-slash-parity] Report: {}",
            py::path_str(&report_path)
        );
    }
    Ok(if ok { 0 } else { 1 })
}

#[cfg(test)]
mod tests;
