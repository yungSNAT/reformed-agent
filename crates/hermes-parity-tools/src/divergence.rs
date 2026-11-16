//! Validate intentional-divergence registry quality and review freshness.
//! Port of `scripts/validate-intentional-divergence.py`.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{Datelike as _, NaiveDate, Weekday};
use clap::Args;
use serde_json::{Map, Value};

use crate::git::{git, git_checked};
use crate::py::{self, Py};

const ALLOWED_STATUS: [&str; 4] = ["approved", "temporary", "pending_review", "deprecated"];
const REQUIRED_FIELDS: [&str; 10] = [
    "id",
    "status",
    "workstream",
    "summary",
    "owner",
    "ticket",
    "last_reviewed",
    "review_date",
    "rationale",
    "path_prefixes",
];

#[derive(Args, Debug)]
pub struct DivergenceArgs {
    /// Repository root path
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// Intentional divergence registry JSON path (relative to repo root)
    #[arg(long, default_value = "docs/parity/intentional-divergence.json")]
    divergence_file: String,
    /// Git remote name for official upstream
    #[arg(long, default_value = "upstream")]
    upstream_remote: String,
    /// URL used if upstream remote is missing
    #[arg(
        long,
        default_value = "https://github.com/NousResearch/hermes-agent.git"
    )]
    upstream_url: String,
    /// Upstream ref used for path coverage checks
    #[arg(long, default_value = "upstream/main")]
    upstream_ref: String,
    /// Skip fetching upstream ref
    #[arg(long)]
    no_fetch: bool,
    /// Validation report JSON path (relative to repo root)
    #[arg(long, default_value = "docs/parity/divergence-validation.json")]
    output_json: String,
    /// Exit non-zero when errors/warnings violate gate
    #[arg(long)]
    check: bool,
    /// Do not fail check mode on warnings (errors still fail)
    #[arg(long)]
    allow_warnings: bool,
}

/// `dt.date.fromisoformat` as CPython 3.11 and later read it (the C
/// `parse_isoformat_date`): a string of 7, 8 or 10 bytes holding `YYYY-MM-DD`,
/// `YYYYMMDD` or an ISO week date (`YYYY-Www-D`, `YYYYWwwD`, the day optional).
/// Like CPython, it ignores bytes after the date in a 10-byte string.
pub fn parse_iso_date(value: &str) -> Option<NaiveDate> {
    let b = value.as_bytes();
    if ![7, 8, 10].contains(&b.len()) {
        return None;
    }
    let digits = |from: usize, n: usize| -> Option<u32> {
        let part = b.get(from..from + n)?;
        part.iter().try_fold(0u32, |acc, d| {
            d.is_ascii_digit().then(|| acc * 10 + u32::from(d - b'0'))
        })
    };
    let year = digits(0, 4)?;
    let sep = b[4] == b'-';
    let mut pos = 4 + usize::from(sep);
    if b.get(pos) == Some(&b'W') {
        pos += 1;
        let week = digits(pos, 2)?;
        pos += 2;
        let mut day = 1;
        if pos < b.len() {
            if sep {
                if b[pos] != b'-' {
                    return None;
                }
                pos += 1;
            }
            day = digits(pos, 1)?;
        }
        let weekday = match day {
            1 => Weekday::Mon,
            2 => Weekday::Tue,
            3 => Weekday::Wed,
            4 => Weekday::Thu,
            5 => Weekday::Fri,
            6 => Weekday::Sat,
            7 => Weekday::Sun,
            _ => return None,
        };
        if !(1..=9999).contains(&year) {
            return None;
        }
        let date = NaiveDate::from_isoywd_opt(year as i32, week, weekday)?;
        return (1..=9999).contains(&date.year()).then_some(date);
    }
    let month = digits(pos, 2)?;
    pos += 2;
    if sep {
        if b.get(pos) != Some(&b'-') {
            return None;
        }
        pos += 1;
    }
    let day = digits(pos, 2)?;
    if !(1..=9999).contains(&year) {
        return None;
    }
    NaiveDate::from_ymd_opt(year as i32, month, day)
}

fn str_field(item: &Map<String, Value>, key: &str) -> String {
    item.get(key).map(py::str_of).unwrap_or_default()
}

/// Python's `isinstance(ticket, int) and ticket > 0`; a JSON `true` is the
/// int 1 to Python.
fn ticket_is_valid(ticket: Option<&Value>) -> bool {
    match ticket {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64().is_none_or(|i| i > 0),
        _ => false,
    }
}

pub fn validate_item(
    item: &Map<String, Value>,
    all_paths: &BTreeSet<String>,
    now: NaiveDate,
) -> Py {
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for field in REQUIRED_FIELDS {
        if !item.contains_key(field) {
            errors.push(format!("missing field: {field}"));
        }
    }

    let status = str_field(item, "status");
    if !status.is_empty() && !ALLOWED_STATUS.contains(&status.as_str()) {
        errors.push(format!("invalid status: {status}"));
    }

    let owner = py::strip(&str_field(item, "owner")).to_string();
    if owner.is_empty() {
        errors.push("owner must be non-empty".to_string());
    }

    let ticket = item.get("ticket");
    if !ticket_is_valid(ticket) {
        errors.push("ticket must be a positive integer".to_string());
    }

    let review_date_raw = py::strip(&str_field(item, "review_date")).to_string();
    let mut review_date = None;
    if review_date_raw.is_empty() {
        errors.push("review_date must be non-empty".to_string());
    } else {
        review_date = parse_iso_date(&review_date_raw);
        if review_date.is_none() {
            errors.push("review_date must be ISO-8601 date (YYYY-MM-DD)".to_string());
        }
    }

    let last_reviewed_raw = py::strip(&str_field(item, "last_reviewed")).to_string();
    if last_reviewed_raw.is_empty() {
        errors.push("last_reviewed must be non-empty".to_string());
    } else if parse_iso_date(&last_reviewed_raw).is_none() {
        errors.push("last_reviewed must be ISO-8601 date (YYYY-MM-DD)".to_string());
    }

    let rationale = str_field(item, "rationale");
    if py::strip(&rationale).chars().count() < 12 {
        errors.push("rationale too short".to_string());
    }

    let prefixes: Vec<String> = match item.get("path_prefixes") {
        Some(Value::Array(prefixes)) if !prefixes.is_empty() => prefixes
            .iter()
            .map(py::str_of)
            .filter(|p| !py::strip(p).is_empty())
            .map(|p| p.trim_end_matches('/').to_string())
            .collect(),
        _ => {
            errors.push("path_prefixes must be a non-empty list".to_string());
            Vec::new()
        }
    };

    let matched: Vec<&String> = all_paths
        .iter()
        .filter(|path| {
            prefixes.iter().any(|prefix| {
                *path == prefix
                    || path
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
        .collect();
    if matched.is_empty() {
        warnings.push("no paths currently matched in local/upstream trees".to_string());
    }

    let overdue = review_date.is_some_and(|d| d < now);
    if overdue {
        warnings.push("review_date is overdue".to_string());
    }

    Py::dict([
        ("id", Py::from(str_field(item, "id"))),
        ("status", Py::from(status)),
        ("owner", Py::from(owner)),
        ("ticket", ticket.map_or(Py::Null, py::from_json)),
        ("review_date", Py::from(review_date_raw)),
        ("last_reviewed", Py::from(last_reviewed_raw)),
        ("matched_files", Py::from(matched.len())),
        ("matched_sample", Py::strs(matched.iter().take(20))),
        ("errors", Py::strs(&errors)),
        ("warnings", Py::strs(&warnings)),
        ("overdue", Py::from(overdue)),
    ])
}

fn ls_tree_paths(repo_root: &Path, rev: &str) -> Result<BTreeSet<String>, String> {
    let out = git(repo_root, &["ls-tree", "-r", "--name-only", rev])?;
    Ok(py::strip(&out.stdout)
        .lines()
        .map(py::strip)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn ensure_remote(repo_root: &Path, remote: &str, url: &str) -> Result<(), String> {
    let out = git(repo_root, &["remote"])?;
    if py::strip(&out.stdout).lines().any(|line| line == remote) {
        return Ok(());
    }
    git_checked(repo_root, &["remote", "add", remote, url]).map(|_| ())
}

fn branch_from_remote_ref<'a>(remote: &str, upstream_ref: &'a str) -> &'a str {
    match upstream_ref
        .strip_prefix(remote)
        .and_then(|r| r.strip_prefix('/'))
    {
        Some(branch) if !branch.is_empty() => branch,
        _ => "main",
    }
}

fn fetch_remote_branch(repo_root: &Path, remote: &str, branch: &str) -> Result<(), String> {
    let remote_ref = format!("{remote}/{branch}");
    let refspec = format!("refs/heads/{branch}:refs/remotes/{remote}/{branch}");
    let out = git(repo_root, &["fetch", "--no-tags", remote, &refspec])?;
    if out.ok {
        return Ok(());
    }
    let existing = git(repo_root, &["rev-parse", "--verify", &remote_ref])?;
    if !py::strip(&existing.stdout).is_empty() {
        eprintln!(
            "warning: scoped fetch for {remote_ref} failed; using existing ref: {}",
            py::strip(&out.stderr)
        );
        return Ok(());
    }
    Err(format!(
        "git fetch {remote} {branch} failed: {}",
        py::strip(&out.stderr)
    ))
}

fn repo_relative(path: &Path, repo_root: &Path) -> String {
    match path.strip_prefix(repo_root) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => py::path_str(path),
    }
}

pub fn run(args: DivergenceArgs) -> Result<i32, String> {
    let repo_root = py::resolve(Path::new(&args.repo_root));
    let div_path = py::resolve(&py::join(&repo_root, &args.divergence_file));
    let out_json = py::resolve(&py::join(&repo_root, &args.output_json));
    let now = chrono::Local::now().date_naive();

    ensure_remote(&repo_root, &args.upstream_remote, &args.upstream_url)?;
    if !args.no_fetch {
        fetch_remote_branch(
            &repo_root,
            &args.upstream_remote,
            branch_from_remote_ref(&args.upstream_remote, &args.upstream_ref),
        )?;
    }

    let local_paths = ls_tree_paths(&repo_root, "HEAD")?;
    let upstream_paths = ls_tree_paths(&repo_root, &args.upstream_ref)?;
    let all_paths: BTreeSet<String> = local_paths.union(&upstream_paths).cloned().collect();

    let text = std::fs::read_to_string(&div_path)
        .map_err(|e| format!("could not read {}: {e}", div_path.display()))?;
    let raw: Value = serde_json::from_str(&text)
        .map_err(|e| format!("invalid JSON in {}: {e}", div_path.display()))?;
    let invalid = || format!("invalid items payload in {}", py::path_str(&div_path));
    let items = match raw.as_object().ok_or_else(invalid)?.get("items") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => return Err(invalid()),
    };

    let mut rows = Vec::new();
    for item in &items {
        let item = item
            .as_object()
            .ok_or_else(|| format!("registry item is not an object: {item}"))?;
        rows.push(validate_item(item, &all_paths, now));
    }
    let list_len = |row: &Py, key: &str| match row.get(key) {
        Some(Py::List(items)) => items.len(),
        _ => 0,
    };
    let errors: usize = rows.iter().map(|r| list_len(r, "errors")).sum();
    let warnings: usize = rows.iter().map(|r| list_len(r, "warnings")).sum();
    let unowned = rows
        .iter()
        .filter(|r| matches!(r.get("owner"), Some(Py::Str(o)) if py::strip(o).is_empty()))
        .count();
    let overdue = rows
        .iter()
        .filter(|r| r.get("overdue") == Some(&Py::Bool(true)))
        .count();

    let summary = Py::dict([
        ("items", Py::from(rows.len())),
        ("errors", Py::from(errors)),
        ("warnings", Py::from(warnings)),
        ("unowned", Py::from(unowned)),
        ("review_overdue", Py::from(overdue)),
        ("local_paths", Py::from(local_paths.len())),
        ("upstream_paths", Py::from(upstream_paths.len())),
    ]);
    let payload = Py::dict([
        ("generated_at_utc", Py::from(py::utc_isoformat())),
        (
            "divergence_file",
            Py::from(repo_relative(&div_path, &repo_root)),
        ),
        ("summary", summary.clone()),
        ("items", Py::List(rows)),
    ]);

    if let Some(parent) = out_json.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(&out_json, py::dumps(&payload, Some(2), true) + "\n")
        .map_err(|e| format!("could not write {}: {e}", out_json.display()))?;

    println!("Wrote {}", py::path_str(&out_json));
    println!("Summary: {}", py::dumps(&summary, None, true));

    if args.check && (errors > 0 || (warnings > 0 && !args.allow_warnings)) {
        return Ok(1);
    }
    Ok(0)
}

#[cfg(test)]
mod tests;
