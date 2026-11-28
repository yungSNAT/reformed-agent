//! Render or update README upstream sync status block from `.sync-reports`.
//! Port of `scripts/generate-readme-sync-status.py`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
use crate::script_helpers::{py_int, py_splitlines, read_utf8, resolve_repo_root};

const BEGIN_MARKER: &str = "<!-- BEGIN:ULTRA_SYNC_STATUS -->";
const END_MARKER: &str = "<!-- END:ULTRA_SYNC_STATUS -->";
const UPSTREAM_SECTION: &str = "## Upstream Sync and Parity Upkeep";

#[derive(Args, Debug)]
pub struct ReadmeSyncStatusArgs {
    /// Repository root
    #[arg(long, default_value = ".")]
    repo_root: String,
    /// README path relative to repo root
    #[arg(long, default_value = "README.md")]
    readme: String,
    /// Sync report dir relative to repo root
    #[arg(long, default_value = ".sync-reports")]
    report_dir: String,
    /// Queue summary JSON path relative to repo root
    #[arg(long, default_value = "docs/parity/upstream-missing-queue.json")]
    queue_json: String,
    /// Global parity proof JSON path relative to repo root
    #[arg(long, default_value = "docs/parity/global-parity-proof.json")]
    proof_json: String,
    /// Workstream status JSON path relative to repo root
    #[arg(long, default_value = "docs/parity/workstream-status.json")]
    workstream_json: String,
    /// Do not write files; exit non-zero if README block would change
    #[arg(long)]
    check: bool,
}

fn load_json_dict(path: &Path) -> Py {
    if !path.exists() {
        return Py::Dict(Vec::new());
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|s| py::loads(&s).ok())
        .filter(Py::is_dict)
        .unwrap_or_else(|| Py::Dict(Vec::new()))
}

fn latest_upstream_sync_report(report_dir: &Path) -> Result<Option<PathBuf>, String> {
    let mut reports: Vec<PathBuf> = fs::read_dir(report_dir)
        .map_err(|e| format!("{}: {e}", report_dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("upstream-sync-") && n.ends_with(".txt"))
        })
        .collect();
    reports.sort();
    Ok(reports.pop())
}

fn parse_sync_report(path: &Path) -> Result<(BTreeMap<String, String>, usize), String> {
    let mut meta = BTreeMap::new();
    let mut pending_count = 0usize;
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut in_pending = false;
    let mut in_block = false;

    for line in py_splitlines(&text) {
        let stripped = py::strip(line);
        if !in_pending {
            if stripped.starts_with("## Pending Upstream Commits") {
                in_pending = true;
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let key = py::strip(k);
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '-')
                {
                    meta.insert(key.to_string(), py::strip(v).to_string());
                }
            }
            continue;
        }
        if stripped == "```" {
            if !in_block {
                in_block = true;
            } else {
                break;
            }
            continue;
        }
        if in_block && !stripped.is_empty() {
            pending_count += 1;
        }
    }
    Ok((meta, pending_count))
}

fn gate_status(value: Option<&Py>) -> &'static str {
    match value {
        Some(Py::Bool(true)) => "pass",
        Some(Py::Bool(false)) => "fail",
        _ => "unknown",
    }
}

fn str_or_unknown(value: Option<&Py>) -> String {
    match value {
        Some(v) if crate::report_io::truth(v) => py::str_of(v),
        _ => "unknown".to_string(),
    }
}

fn build_block(
    generated_at: &str,
    report: (&str, &str),
    report_meta: &BTreeMap<String, String>,
    report_pending_count: usize,
    queue: &Py,
    proof: &Py,
    workstream: &Py,
) -> String {
    let (report_rel, report_name) = report;
    let by_disp = queue
        .get("summary")
        .filter(|v| v.is_dict())
        .and_then(|s| s.get("by_disposition"))
        .filter(|v| v.is_dict());
    let pending = py_int(by_disp.and_then(|d| d.get("pending")), 0);
    let ported = py_int(by_disp.and_then(|d| d.get("ported")), 0);
    let superseded = py_int(by_disp.and_then(|d| d.get("superseded")), 0);

    let release_gate = gate_status(
        proof
            .get("release_gate")
            .filter(|v| v.is_dict())
            .and_then(|g| g.get("pass")),
    );
    let ci_gate = gate_status(
        proof
            .get("ci_gate")
            .filter(|v| v.is_dict())
            .and_then(|g| g.get("pass")),
    );

    let upstream_ref = str_or_unknown(workstream.get("upstream_ref"));
    let upstream_sha = str_or_unknown(workstream.get("upstream_sha"));
    let workstream_generated = str_or_unknown(workstream.get("generated_at_utc"));

    let timestamp_utc = report_meta
        .get("timestamp_utc")
        .map_or("unknown", String::as_str);
    let origin_sha = report_meta
        .get("origin_sha")
        .map_or("unknown", String::as_str);
    let upstream_sync_sha = report_meta
        .get("upstream_sha")
        .map_or("unknown", String::as_str);

    [
        BEGIN_MARKER.to_string(),
        "### Live Upstream Sync Status (auto-generated)".to_string(),
        String::new(),
        format!("- Generated at: `{generated_at}`"),
        format!("- Source report: [`{report_name}`](./{report_rel})"),
        format!("- Sync timestamp (`timestamp_utc`): `{timestamp_utc}`"),
        format!("- `origin/main` at sync: `{origin_sha}`"),
        format!("- `upstream/main` at sync: `{upstream_sync_sha}`"),
        format!("- Pending commits captured in report: `{report_pending_count}`"),
        format!(
            "- Queue summary (`docs/parity/upstream-missing-queue.json`): pending `{pending}`, ported `{ported}`, superseded `{superseded}`"
        ),
        format!(
            "- Parity gates (`docs/parity/global-parity-proof.json`): release `{release_gate}`, ci `{ci_gate}`"
        ),
        format!(
            "- Workstream snapshot (`docs/parity/workstream-status.json`): `{upstream_ref}` @ `{upstream_sha}` (generated `{workstream_generated}`)"
        ),
        END_MARKER.to_string(),
    ]
    .join("\n")
}

fn replace_or_insert_block(readme: &str, block: &str) -> String {
    if let (Some(start), Some(end_start)) = (readme.find(BEGIN_MARKER), readme.find(END_MARKER)) {
        if start <= end_start {
            let end = end_start + END_MARKER.len();
            return format!("{}{}{}", &readme[..start], block, &readme[end..]);
        }
    }
    if let Some(section_idx) = readme.find(UPSTREAM_SECTION) {
        if let Some(rel_nl) = readme[section_idx..].find('\n') {
            let section_end = section_idx + rel_nl + 1;
            return format!(
                "{}\n{}\n\n{}",
                &readme[..section_end],
                block,
                &readme[section_end..]
            );
        }
    }
    format!(
        "{}\n\n{}\n",
        readme.trim_end_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)),
        block
    )
}

pub fn run(args: ReadmeSyncStatusArgs) -> Result<i32, String> {
    let repo_root = resolve_repo_root(&args.repo_root)?;
    let readme_path = resolve_argument(&py::join(&repo_root, &args.readme))?;
    let report_dir = resolve_argument(&py::join(&repo_root, &args.report_dir))?;
    let queue_path = resolve_argument(&py::join(&repo_root, &args.queue_json))?;
    let proof_path = resolve_argument(&py::join(&repo_root, &args.proof_json))?;
    let workstream_path = resolve_argument(&py::join(&repo_root, &args.workstream_json))?;

    if !readme_path.exists() {
        return Err(format!("README not found: {}", py::path_str(&readme_path)));
    }
    if !report_dir.exists() {
        return Err(format!(
            "report directory not found: {}",
            py::path_str(&report_dir)
        ));
    }

    let report = latest_upstream_sync_report(&report_dir)?.ok_or_else(|| {
        format!(
            "no upstream-sync reports found in {}",
            py::path_str(&report_dir)
        )
    })?;
    let (report_meta, report_pending_count) = parse_sync_report(&report)?;

    let queue = load_json_dict(&queue_path);
    let proof = load_json_dict(&proof_path);
    let workstream = load_json_dict(&workstream_path);

    let report_stem = report
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let generated_at = report_meta
        .get("timestamp_utc")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| {
            report_stem
                .strip_prefix("upstream-sync-")
                .unwrap_or(report_stem)
                .to_string()
        });
    let report_rel = crate::website::rel_posix(&report, &repo_root);
    let report_name = report
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();

    let block = build_block(
        &generated_at,
        (&report_rel, report_name),
        &report_meta,
        report_pending_count,
        &queue,
        &proof,
        &workstream,
    );

    let original = read_utf8(&readme_path)?;
    let updated = replace_or_insert_block(&original, &block);

    if args.check {
        if original != updated {
            println!("README sync status block is stale.");
            return Ok(1);
        }
        println!("README sync status block is up-to-date.");
        return Ok(0);
    }

    if original != updated {
        fs::write(&readme_path, updated)
            .map_err(|e| format!("{}: {e}", py::path_str(&readme_path)))?;
        println!(
            "Updated README sync status block in {}",
            py::path_str(&readme_path)
        );
    } else {
        println!(
            "README sync status block already up-to-date in {}",
            py::path_str(&readme_path)
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn status_requires_local_inputs_and_keeps_current_readme_unmodified() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let readme = root.join("README.md");
        let reports = root.join(".sync-reports");
        let generate = || {
            run(ReadmeSyncStatusArgs {
                repo_root: root.to_str().unwrap().to_owned(),
                readme: "README.md".into(),
                report_dir: ".sync-reports".into(),
                queue_json: "queue.json".into(),
                proof_json: "proof.json".into(),
                workstream_json: "workstream.json".into(),
                check: false,
            })
        };

        assert!(generate().unwrap_err().contains("README not found"));
        let original = format!("# Fixture\n\n{UPSTREAM_SECTION}");
        fs::write(&readme, &original).unwrap();
        assert!(generate()
            .unwrap_err()
            .contains("report directory not found"));
        assert_eq!(fs::read_to_string(&readme).unwrap(), original);

        fs::create_dir(&reports).unwrap();
        fs::write(
            reports.join("upstream-sync-2026-01-02.txt"),
            "origin_sha: fixture-origin\n",
        )
        .unwrap();
        fs::write(
            root.join("workstream.json"),
            r#"{"upstream_ref":"refs/heads/fixture","upstream_sha":"test-sha","generated_at_utc":"2026-01-02T03:04:05Z"}"#,
        )
        .unwrap();
        assert_eq!(generate().unwrap(), 0);
        let updated = fs::read_to_string(&readme).unwrap();
        assert!(updated.starts_with(&original));
        assert!(updated.contains("Generated at: `2026-01-02`"));
        assert!(updated
            .contains("`refs/heads/fixture` @ `test-sha` (generated `2026-01-02T03:04:05Z`)"));

        fs::OpenOptions::new()
            .write(true)
            .open(&readme)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
            .unwrap();
        let modified = fs::metadata(&readme).unwrap().modified().unwrap();
        assert_eq!(generate().unwrap(), 0);
        assert_eq!(fs::read_to_string(&readme).unwrap(), updated);
        assert_eq!(fs::metadata(&readme).unwrap().modified().unwrap(), modified);
    }
}
