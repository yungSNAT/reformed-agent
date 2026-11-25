//! Audit and optionally repair Hermes background job queue manifests.
//! Port of `scripts/audit_background_queue.py`.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use jiff::Timestamp;
use sha2::{Digest, Sha256};

use crate::py::{self, Py};
use crate::report_io::resolve_argument;
use crate::script_helpers::{resolve_report_path, write_json_pretty};

#[derive(Args, Debug)]
pub struct AuditBackgroundQueueArgs {
    /// Hermes home directory override
    #[arg(long, default_value = "")]
    home: String,
    /// Mark running jobs older than this threshold as stale
    #[arg(long, default_value_t = 1800)]
    stale_running_seconds: i64,
    /// Apply safe repairs
    #[arg(long)]
    repair: bool,
    /// Optional report output path (defaults to .sync-reports/background-queue-audit-*.json)
    #[arg(long, default_value = "")]
    report: String,
    /// Print report JSON
    #[arg(long)]
    json: bool,
}

fn resolve_home(home_arg: &str) -> Result<PathBuf, String> {
    if home_arg.is_empty() {
        resolve_argument(&py::expanduser("~/.hermes-agent-ultra")?)
    } else {
        resolve_argument(&py::expanduser(home_arg)?)
    }
}

fn parse_time(value: Option<&Py>) -> Option<Timestamp> {
    let Some(Py::Str(raw)) = value else {
        return None;
    };
    let trimmed = py::strip(raw);
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.replace('Z', "+00:00");
    normalized.parse::<Timestamp>().ok()
}

fn job_key(task: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(py::strip(task).as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(16);
    for byte in &digest[..8] {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn load_job(path: &Path) -> Result<Py, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read_error: {e}"))?;
    let parsed = py::loads(&raw).map_err(|e| format!("json_error: {e}"))?;
    if !parsed.is_dict() {
        return Err("not_an_object".to_string());
    }
    Ok(parsed)
}

fn write_job(path: &Path, payload: &Py) -> Result<(), String> {
    let text = py::dumps(payload, Some(2), false) + "\n";
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn run(args: AuditBackgroundQueueArgs) -> Result<i32, String> {
    let home = resolve_home(&args.home)?;
    let jobs_dir = home.join("background_jobs");
    let repo_root = std::env::current_dir().map_err(|e| e.to_string())?;

    let mut files = 0usize;
    let mut malformed_count = 0usize;
    let mut stale_count = 0usize;
    let mut duplicate_count = 0usize;
    let mut repaired_count = 0usize;
    let mut malformed_rows: Vec<Py> = Vec::new();
    let mut stale_rows: Vec<Py> = Vec::new();
    let mut duplicate_rows: Vec<Py> = Vec::new();
    let mut note: Option<&str> = None;
    let ok: bool;

    if !jobs_dir.exists() {
        note = Some("jobs directory not found");
        ok = true;
    } else {
        let now = Timestamp::now();
        let mut paths: Vec<PathBuf> = fs::read_dir(&jobs_dir)
            .map_err(|e| format!("{}: {e}", jobs_dir.display()))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();

        let mut active_by_task: Vec<(String, Vec<(PathBuf, Py)>)> = Vec::new();

        for path in paths {
            files += 1;
            let mut payload = match load_job(&path) {
                Ok(payload) => payload,
                Err(err) => {
                    malformed_count += 1;
                    malformed_rows.push(Py::dict([
                        ("path", Py::from(py::path_str(&path))),
                        ("error", Py::from(err)),
                    ]));
                    continue;
                }
            };

            let status =
                py::strip(&py::str_of(payload.get("status").unwrap_or(&"".into()))).to_lowercase();
            let task =
                py::strip(&py::str_of(payload.get("task").unwrap_or(&"".into()))).to_string();
            let started_at = parse_time(payload.get("started_at"));

            if status == "running" {
                if let Some(started) = started_at {
                    let age = now.duration_since(started).as_secs_f64();
                    if age > args.stale_running_seconds as f64 {
                        stale_count += 1;
                        stale_rows.push(Py::dict([
                            ("path", Py::from(py::path_str(&path))),
                            ("status", Py::from(status.as_str())),
                            ("age_seconds", Py::Int(age as i128)),
                        ]));
                        if args.repair {
                            payload.set("status", Py::from("failed"));
                            payload.set("finished_at", Py::from(py::isoformat(now)));
                            payload.set(
                                "error",
                                Py::from(format!(
                                    "queue_audit_repair: stale running job (> {}s)",
                                    args.stale_running_seconds
                                )),
                            );
                            write_job(&path, &payload)?;
                            repaired_count += 1;
                        }
                    }
                }
            }

            if (status == "queued" || status == "running") && !task.is_empty() {
                let key = job_key(&task);
                if let Some((_, list)) = active_by_task.iter_mut().find(|(k, _)| k == &key) {
                    list.push((path, payload));
                } else {
                    active_by_task.push((key, vec![(path, payload)]));
                }
            }
        }

        for (key, mut rows) in active_by_task {
            if rows.len() <= 1 {
                continue;
            }
            rows.sort_by_key(|(_, payload)| {
                py::str_of(payload.get("created_at").unwrap_or(&"".into()))
            });
            let keep_path = rows[0].0.clone();
            let keep_name = keep_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let dup_paths: Vec<String> = rows[1..].iter().map(|(p, _)| py::path_str(p)).collect();
            duplicate_count += dup_paths.len();
            duplicate_rows.push(Py::dict([
                ("task_key", Py::from(key)),
                ("keep", Py::from(py::path_str(&keep_path))),
                ("duplicates", Py::strs(&dup_paths)),
            ]));
            if args.repair {
                for (dup_path, dup_payload) in &mut rows[1..] {
                    dup_payload.set("status", Py::from("failed"));
                    dup_payload.set("finished_at", Py::from(py::utc_isoformat()));
                    dup_payload.set(
                        "error",
                        Py::from(format!(
                            "queue_audit_repair: duplicate active task of {keep_name}"
                        )),
                    );
                    write_job(dup_path, dup_payload)?;
                    repaired_count += 1;
                }
            }
        }

        ok = malformed_count == 0 && stale_count == 0 && duplicate_count == 0;
    }

    let mut pairs = vec![
        ("generated_at", Py::from(py::utc_isoformat())),
        ("home", Py::from(py::path_str(&home))),
        ("jobs_dir", Py::from(py::path_str(&jobs_dir))),
        ("repair", Py::from(args.repair)),
        (
            "totals",
            Py::dict([
                ("files", Py::from(files)),
                ("malformed", Py::from(malformed_count)),
                ("stale_running", Py::from(stale_count)),
                ("duplicate_active", Py::from(duplicate_count)),
                ("repaired", Py::from(repaired_count)),
            ]),
        ),
        ("malformed", Py::List(malformed_rows)),
        ("stale_running", Py::List(stale_rows)),
        ("duplicate_active", Py::List(duplicate_rows)),
    ];
    if let Some(msg) = note {
        pairs.push(("note", Py::from(msg)));
    }
    pairs.push(("ok", Py::from(ok)));
    let mut findings = Py::dict(pairs);

    let report_path = resolve_report_path(&repo_root, &args.report, "background-queue-audit")?;
    write_json_pretty(&report_path, &findings, false)?;
    findings.set("report_path", Py::from(py::path_str(&report_path)));

    if args.json {
        println!("{}", py::dumps(&findings, Some(2), false));
    } else {
        println!(
            "[background-queue-audit] {} files={} malformed={} stale={} duplicates={} repaired={}",
            if ok { "PASS" } else { "WARN" },
            files,
            malformed_count,
            stale_count,
            duplicate_count,
            repaired_count
        );
        println!(
            "[background-queue-audit] Report: {}",
            py::path_str(&report_path)
        );
    }

    Ok(i32::from(!ok))
}
