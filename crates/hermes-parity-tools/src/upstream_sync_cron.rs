//! The cron entry point for `sync-upstream` and its crontab installer.
//! Ports of `scripts/cron-upstream-sync.sh` (kept as a shim, since installed
//! crontab lines name it) and `scripts/install-upstream-sync-cron.sh`.
//!
//! Both read the same environment variables the scripts read, with the same
//! defaults; `REPO_ROOT` defaults to the current directory (the shim sets it
//! to the script's parent, as the script did).

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::process::{Command, Stdio};

use clap::Args;

use crate::sync_upstream::{env_or, exit_code, on_path, sync};

const LOG_TAG: &str = "[cron-upstream-sync]";
const MARKER: &str = "# hermes-upstream-sync";
const DEFAULT_SCHEDULE: &str = "17 */6 * * *";

#[derive(Args, Debug)]
pub struct CronArgs {}

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Cron schedule (default: `17 */6 * * *`); later arguments are ignored
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

fn current_dir() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// `$HOME`, which the scripts read under `set -u`.
fn home() -> Result<String, String> {
    std::env::var("HOME").map_err(|_| "HOME: unbound variable".to_string())
}

fn parent_dir(path: &str) -> &Path {
    match Path::new(path).parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// The settings both scripts read from the environment.
struct Settings {
    repo_root: String,
    strategy: String,
    report_dir: String,
    conflict_label: String,
    create_conflict_issue: String,
    strict_risk_gate: String,
    allow_risk_paths: String,
    risk_paths_file: String,
}

impl Settings {
    fn from_env() -> Self {
        let repo_root = env_or("REPO_ROOT", &current_dir());
        Self {
            strategy: env_or("SYNC_STRATEGY", "merge"),
            report_dir: env_or("REPORT_DIR", &format!("{repo_root}/.sync-reports")),
            conflict_label: env_or("CONFLICT_LABEL", "upstream-sync-conflict"),
            create_conflict_issue: env_or("CREATE_CONFLICT_ISSUE", "1"),
            strict_risk_gate: env_or("STRICT_RISK_GATE", "1"),
            allow_risk_paths: env_or("ALLOW_RISK_PATHS", "0"),
            risk_paths_file: env_or(
                "RISK_PATHS_FILE",
                &format!("{repo_root}/scripts/upstream-risk-paths.txt"),
            ),
            repo_root,
        }
    }

    /// The `sync-upstream` options the cron script passed.
    fn sync_args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "--repo-root",
            &self.repo_root,
            "--mode",
            "branch-pr",
            "--strategy",
            &self.strategy,
            "--report-dir",
            &self.report_dir,
            "--conflict-label",
            &self.conflict_label,
            "--risk-paths-file",
            &self.risk_paths_file,
            "--test-cmd",
            "cargo test -p hermes-gateway",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if self.create_conflict_issue == "0" {
            args.push("--no-conflict-issue".into());
        }
        args.push(if self.strict_risk_gate == "1" {
            "--strict-risk-gate".into()
        } else {
            "--no-strict-risk-gate".into()
        });
        if self.allow_risk_paths == "1" {
            args.push("--allow-risk-paths".into());
        }
        args
    }
}

pub fn run_cron(_args: CronArgs) -> Result<i32, String> {
    let settings = Settings::from_env();
    let lock_path = match std::env::var("LOCK_FILE") {
        Ok(v) if !v.is_empty() => v,
        _ => format!("{}/.hermes/locks/upstream-sync.lock", home()?),
    };
    // Cron has a minimal PATH; include common tool locations explicitly.
    let path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:{path}"),
    );
    fs::create_dir_all(parent_dir(&lock_path))
        .map_err(|e| format!("mkdir {}: {e}", parent_dir(&lock_path).display()))?;
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)
        .map_err(|e| format!("{lock_path}: {e}"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => {
            println!("{LOG_TAG} lock busy; skipping overlapping run");
            return Ok(0);
        }
        Err(fs::TryLockError::Error(e)) => return Err(format!("{lock_path}: {e}")),
    }
    if !Path::new(&settings.repo_root).join(".git").is_dir() {
        println!("{LOG_TAG} repository not found at {}", settings.repo_root);
        return Ok(1);
    }
    std::env::set_current_dir(&settings.repo_root)
        .map_err(|e| format!("cd {}: {e}", settings.repo_root))?;
    // The lock stays held until the sync returns, as the script's fd 9 did.
    let code = sync(settings.sync_args());
    drop(lock);
    Ok(code)
}

/// The crontab line the installer writes; it runs the cron shim, so lines
/// installed before the port and after it run the same way.
fn cron_entry(schedule: &str, settings: &Settings, cron_script: &str, log_file: &str) -> String {
    format!(
        "{schedule} REPO_ROOT='{}' SYNC_STRATEGY='{}' REPORT_DIR='{}' CONFLICT_LABEL='{}' CREATE_CONFLICT_ISSUE='{}' STRICT_RISK_GATE='{}' ALLOW_RISK_PATHS='{}' RISK_PATHS_FILE='{}' /usr/bin/env bash '{cron_script}' >> '{log_file}' 2>&1 {MARKER}",
        settings.repo_root,
        settings.strategy,
        settings.report_dir,
        settings.conflict_label,
        settings.create_conflict_issue,
        settings.strict_risk_gate,
        settings.allow_risk_paths,
        settings.risk_paths_file,
    )
}

/// The current crontab without lines holding the marker (`grep -v`).
fn kept_lines(current: &str) -> String {
    current
        .split_terminator('\n')
        .filter(|line| !line.contains(MARKER))
        .map(|line| format!("{line}\n"))
        .collect()
}

#[cfg(unix)]
fn make_executable(path: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(meta) = fs::metadata(path) {
        let mode = meta.permissions().mode() | 0o111;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
}

#[cfg(not(unix))]
fn make_executable(_path: &str) {}

pub fn run_install(args: InstallArgs) -> Result<i32, String> {
    let schedule = match args.args.first() {
        Some(s) if !s.is_empty() => s.clone(),
        _ => DEFAULT_SCHEDULE.to_string(),
    };
    let settings = Settings::from_env();
    let log_file = match std::env::var("LOG_FILE") {
        Ok(v) if !v.is_empty() => v,
        _ => format!("{}/.hermes/logs/upstream-sync.log", home()?),
    };
    let cron_script = format!("{}/scripts/cron-upstream-sync.sh", settings.repo_root);
    if !Path::new(&cron_script).is_file() {
        eprintln!("Missing script: {cron_script}");
        return Ok(1);
    }
    if !on_path("crontab") {
        eprintln!("crontab command not found");
        return Ok(1);
    }
    fs::create_dir_all(parent_dir(&log_file))
        .map_err(|e| format!("mkdir {}: {e}", parent_dir(&log_file).display()))?;
    make_executable(&cron_script);
    let entry = cron_entry(&schedule, &settings, &cron_script, &log_file);
    let current = Command::new("crontab")
        .arg("-l")
        .stderr(Stdio::null())
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    let tmp =
        std::env::temp_dir().join(format!("install-upstream-sync-cron.{}", std::process::id()));
    fs::write(&tmp, format!("{}{entry}\n", kept_lines(&current)))
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    let status = Command::new("crontab").arg(&tmp).status();
    let _ = fs::remove_file(&tmp);
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => return Ok(exit_code(s)),
        Err(e) => return Err(format!("crontab: {e}")),
    }
    println!("Installed upstream sync cron entry:");
    println!("  {entry}");
    println!();
    println!("Inspect with: crontab -l | grep hermes-upstream-sync");
    println!("Logs: {log_file}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(strict: &str, issue: &str, allow: &str) -> Settings {
        Settings {
            repo_root: "/r".into(),
            strategy: "merge".into(),
            report_dir: "/r/.sync-reports".into(),
            conflict_label: "upstream-sync-conflict".into(),
            create_conflict_issue: issue.into(),
            strict_risk_gate: strict.into(),
            allow_risk_paths: allow.into(),
            risk_paths_file: "/r/scripts/upstream-risk-paths.txt".into(),
        }
    }

    #[test]
    fn passes_the_cron_script_options() {
        let base = [
            "--repo-root",
            "/r",
            "--mode",
            "branch-pr",
            "--strategy",
            "merge",
            "--report-dir",
            "/r/.sync-reports",
            "--conflict-label",
            "upstream-sync-conflict",
            "--risk-paths-file",
            "/r/scripts/upstream-risk-paths.txt",
            "--test-cmd",
            "cargo test -p hermes-gateway",
        ];
        let with = |extra: &[&str]| -> Vec<String> {
            base.iter().chain(extra).map(|s| s.to_string()).collect()
        };
        assert_eq!(
            settings("1", "1", "0").sync_args(),
            with(&["--strict-risk-gate"])
        );
        assert_eq!(
            settings("0", "0", "1").sync_args(),
            with(&[
                "--no-conflict-issue",
                "--no-strict-risk-gate",
                "--allow-risk-paths"
            ])
        );
        // Only the exact values the script tested change an option.
        assert_eq!(
            settings("yes", "no", "true").sync_args(),
            with(&["--no-strict-risk-gate"])
        );
    }

    #[test]
    fn keeps_unmarked_crontab_lines() {
        assert_eq!(
            kept_lines("a\n0 * * * * x # hermes-upstream-sync\nb"),
            "a\nb\n"
        );
        assert_eq!(kept_lines(""), "");
    }
}
