//! `worker`: claims queued push events and runs `scripts/sync-upstream.sh`
//! for each (`worker_loop` and `run_sync_for_event` in the script).
//!
//! Not ported yet, so refused rather than skipped: the CLI-surface and
//! global parity drift checks and the automation-summary publishing. Until
//! they are, the worker runs only with all 3 turned off.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::event::{valid_delivery_id, SyncEvent};
use crate::queue::SqliteQueue;
use crate::upkeep::{self, UpkeepConfig};
use crate::{log, Level};

pub struct SyncOptions {
    pub strategy: String,
    pub allow_risk_paths: bool,
    pub conflict_label: String,
    pub no_tests: bool,
    pub no_pr: bool,
    pub draft_pr: bool,
    pub pr_labels: String,
    pub run_redteam_gate: bool,
    pub redteam_cmd: String,
    pub run_elite_gate: bool,
    pub elite_cmd: String,
    pub elite_rollback_cmd: String,
    pub timeout: Duration,
}

pub struct WorkerConfig {
    pub repo_root: PathBuf,
    pub poll_interval: Duration,
    pub max_attempts: i64,
    pub sync: SyncOptions,
    pub upkeep: UpkeepConfig,
    pub assist_cmd: Option<String>,
}

/// One child run: exit code (`None` when it timed out or was killed by a
/// signal) and its stdout and stderr.
pub struct ChildRun {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Runs a command with output captured, killing it after `timeout`. The
/// script let `subprocess.TimeoutExpired` end the worker, which left the
/// event `processing` for ever; here a timeout is a failed attempt.
pub fn run_child(mut command: Command, timeout: Duration) -> std::io::Result<ChildRun> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let drain = |stream: Option<Box<dyn std::io::Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut s) = stream {
                let _ = s.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (Some(status), false);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break (None, true);
        }
        thread::sleep(Duration::from_millis(100));
    };
    Ok(ChildRun {
        code: status.and_then(|s| s.code()),
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        timed_out,
    })
}

/// The `sync-upstream.sh` command line `run_sync_for_event` built.
pub fn sync_command(repo_root: &Path, report_dir: &Path, options: &SyncOptions) -> Vec<String> {
    let mut cmd: Vec<String> = [
        "bash".to_string(),
        repo_root
            .join("scripts")
            .join("sync-upstream.sh")
            .display()
            .to_string(),
        "--repo-root".to_string(),
        repo_root.display().to_string(),
        "--mode".to_string(),
        "branch-pr".to_string(),
        "--strategy".to_string(),
        options.strategy.clone(),
        "--report-dir".to_string(),
        report_dir.display().to_string(),
        "--conflict-label".to_string(),
        options.conflict_label.clone(),
        // `--strict-risk-gate` was a store_true flag defaulting to true, so
        // the script always passed it.
        "--strict-risk-gate".to_string(),
    ]
    .into();
    let mut flag = |on: bool, yes: &str, no: Option<&str>| match (on, no) {
        (true, _) => cmd.push(yes.to_string()),
        (false, Some(no)) => cmd.push(no.to_string()),
        (false, None) => {}
    };
    flag(options.allow_risk_paths, "--allow-risk-paths", None);
    flag(options.no_tests, "--no-tests", None);
    flag(
        options.run_redteam_gate,
        "--redteam-gate",
        Some("--no-redteam-gate"),
    );
    let valued = |cmd: &mut Vec<String>, name: &str, value: &str| {
        let value = hermes_py::strip(value);
        if !value.is_empty() {
            cmd.extend([name.to_string(), value.to_string()]);
        }
    };
    valued(&mut cmd, "--redteam-cmd", &options.redteam_cmd);
    cmd.push(
        if options.run_elite_gate {
            "--elite-gate"
        } else {
            "--no-elite-gate"
        }
        .to_string(),
    );
    valued(&mut cmd, "--elite-cmd", &options.elite_cmd);
    valued(
        &mut cmd,
        "--elite-rollback-cmd",
        &options.elite_rollback_cmd,
    );
    if options.no_pr {
        cmd.push("--no-pr".to_string());
    }
    if options.draft_pr {
        cmd.push("--draft-pr".to_string());
    }
    valued(&mut cmd, "--pr-labels", &options.pr_labels);
    cmd
}

/// The report path: the last `[sync-upstream] Report: ` line, else the
/// newest `upstream-sync-*.txt` in the report folder.
pub fn find_report(output: &str, report_dir: &Path) -> String {
    let named = output
        .lines()
        .map(hermes_py::strip)
        .filter(|l| l.starts_with("[sync-upstream] Report: "))
        .filter_map(|l| {
            l.split_once("Report:")
                .map(|(_, p)| hermes_py::strip(p).to_string())
        })
        .next_back();
    if let Some(path) = named {
        return path;
    }
    let newest = std::fs::read_dir(report_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("upstream-sync-") && name.ends_with(".txt")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(modified, _)| *modified);
    newest
        .map(|(_, p)| p.display().to_string())
        .unwrap_or_default()
}

/// `parse_report_status`: `risk_blocked` when the risk gate blocked,
/// otherwise the report's `status:` value, else `unknown`.
pub fn report_status(report_path: &str) -> String {
    let Ok(text) = std::fs::read(report_path).map(|b| String::from_utf8_lossy(&b).into_owned())
    else {
        return "unknown".to_string();
    };
    let mut status = "unknown".to_string();
    let mut risk = String::new();
    for line in text.lines().map(hermes_py::strip) {
        if let Some(v) = line.strip_prefix("status:") {
            status = hermes_py::strip(v).to_string();
        }
        if let Some(v) = line.strip_prefix("risk_gate_status:") {
            risk = hermes_py::strip(v).to_string();
        }
    }
    if risk == "blocked" {
        "risk_blocked".to_string()
    } else {
        status
    }
}

/// `maybe_run_assist_hook`: `bash -lc <cmd>` with the event in the
/// environment, 120 s, output discarded.
pub fn run_assist_hook(config: &WorkerConfig, event: &SyncEvent, report_path: &str, outcome: &str) {
    let Some(assist) = &config.assist_cmd else {
        return;
    };
    let mut command = Command::new("bash");
    command
        .args(["-lc", assist])
        .current_dir(&config.repo_root)
        .env("UPSTREAM_SYNC_DELIVERY_ID", &event.delivery_id)
        .env("UPSTREAM_SYNC_REPOSITORY", &event.repository)
        .env("UPSTREAM_SYNC_REF", &event.git_ref)
        .env("UPSTREAM_SYNC_AFTER_SHA", &event.after_sha)
        .env("UPSTREAM_SYNC_OUTCOME", outcome)
        .env("UPSTREAM_SYNC_REPORT_PATH", report_path);
    if let Err(e) = run_child(command, Duration::from_secs(120)) {
        log(Level::Warning, &format!("assist hook failed: {e}"));
    }
}

/// Handles one claimed event and records its outcome in the queue.
pub fn process(
    config: &WorkerConfig,
    queue: &SqliteQueue,
    id: i64,
    attempts: i64,
    event: &SyncEvent,
) -> Result<(), String> {
    if !valid_delivery_id(&event.delivery_id) {
        // Rows the script's listener queued were not checked.
        return queue.mark_done(id, "dead", Some("invalid delivery id"));
    }
    let report_dir = config.repo_root.join(".sync-reports");
    let argv = sync_command(&config.repo_root, &report_dir, &config.sync);
    log(
        Level::Info,
        &format!(
            "running sync for delivery={} repo={} ref={} after={}",
            event.delivery_id,
            event.repository,
            event.git_ref,
            event.after_sha.chars().take(12).collect::<String>()
        ),
    );
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).current_dir(&config.repo_root);
    let timeout = config.sync.timeout.max(Duration::from_secs(60));
    let (code, output) = match run_child(command, timeout) {
        Ok(run) => {
            let mut output = format!("{}\n{}", run.stdout, run.stderr);
            if run.timed_out {
                output.push_str(&format!(
                    "\nsync-upstream.sh timed out after {} s",
                    timeout.as_secs()
                ));
            }
            (run.code, output)
        }
        Err(e) => (None, format!("could not run sync-upstream.sh: {e}")),
    };
    let report_path = find_report(&output, &report_dir);
    let outcome = report_status(&report_path);
    let created =
        upkeep::create_commit_issues(&config.repo_root, &report_dir, event, &config.upkeep);
    if code == Some(0) {
        let note =
            format!("{outcome}; cli_drift=none; global_drift=none; upkeep_created={created}");
        return queue.mark_done(id, "done", Some(&note));
    }
    if outcome == "risk_blocked" || outcome == "conflict" {
        queue.mark_done(
            id,
            &outcome,
            Some(&format!("{outcome}; upkeep_created={created}")),
        )?;
        run_assist_hook(config, event, &report_path, &outcome);
        return Ok(());
    }
    let tail: String = {
        let chars: Vec<char> = output.chars().collect();
        chars[chars.len().saturating_sub(1000)..].iter().collect()
    };
    queue.mark_retry_or_dead(id, attempts, config.max_attempts, &tail)?;
    if attempts >= config.max_attempts {
        run_assist_hook(config, event, &report_path, "dead");
    }
    Ok(())
}

/// Claims and processes events for ever, sleeping between empty polls.
pub fn run(config: &WorkerConfig, queue: &SqliteQueue) -> Result<(), String> {
    std::fs::create_dir_all(config.repo_root.join(".sync-reports"))
        .map_err(|e| format!("could not create the report folder: {e}"))?;
    loop {
        match queue.claim_next()? {
            None => thread::sleep(config.poll_interval),
            Some(item) => process(config, queue, item.id, item.attempts, &item.event)?,
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
