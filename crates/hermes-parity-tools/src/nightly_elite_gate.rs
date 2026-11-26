//! Run the nightly elite validation gate on local `main`.
//! Port of `scripts/run-nightly-elite-gate.sh`.
//!
//! Every gate step runs as a child process with stdout and stderr joined, and
//! that stream goes to this command's stdout and to the run's log file, as the
//! script's `tee` did. Gate steps still go through `cargo run` so a nightly run
//! after `git pull` tests the pulled code, not this binary.

use std::fs::{self, File};
use std::io::{self, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use clap::Args;
use jiff::Timestamp;

const CHECKS: [&str; 6] = [
    "cargo test -p hermes-cli",
    "crates/hermes-parity-tools (run-deterministic-replay-suite)",
    "crates/hermes-parity-tools (run-security-release-gate-v2)",
    "crates/hermes-parity-tools (run-performance-autopilot)",
    "crates/hermes-parity-tools (run-upstream-surface-coverage-gate)",
    "crates/hermes-parity-tools (audit-background-queue)",
];

#[derive(Args, Debug)]
pub struct NightlyEliteGateArgs {
    /// Repository root (default: the current directory)
    #[arg(long)]
    repo_root: Option<String>,
    /// Log directory (default: ~/.hermes-agent-ultra/logs/elite-gate)
    #[arg(long)]
    log_dir: Option<String>,
    /// Do not run `git pull --ff-only origin main`
    #[arg(long)]
    skip_pull: bool,
    /// Allow execution outside `main` (for local testing)
    #[arg(long)]
    allow_non_main: bool,
    /// Allow execution with a dirty worktree (for local testing)
    #[arg(long)]
    allow_dirty: bool,
}

/// `${NAME:-default}`: an unset or empty variable takes the default.
fn env_or(name: &str, default: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => value,
        _ => default.to_string(),
    }
}

pub(crate) fn home_dir() -> Result<String, String> {
    match std::env::var("HOME") {
        Ok(home) => Ok(home),
        Err(_) => Err("HOME: unbound variable".to_string()),
    }
}

/// `basename` as the shell command prints it.
fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() {
            String::new()
        } else {
            "/".to_string()
        };
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

/// A path argument made absolute against the starting directory, so children
/// that run inside the repository root and a launchd plist both read it right.
pub(crate) fn absolute_arg(flag: &str, raw: &str) -> Result<String, String> {
    if raw.is_empty() {
        return Err(format!("missing value for {flag}"));
    }
    std::path::absolute(raw)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|e| format!("{flag} {raw}: {e}"))
}

/// Repository root: the argument made absolute, or the current directory.
pub(crate) fn repo_root_arg(raw: Option<&str>) -> Result<String, String> {
    absolute_arg("--repo-root", raw.unwrap_or("."))
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(unix)]
pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

#[cfg(not(unix))]
pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// How long a step's leftover background processes may keep writing to the
/// gate's output after the step itself exits. The script's `tee` waited only
/// for the step, so a server or daemon a step leaves running must not hold
/// the gate either.
const LEFTOVER_OUTPUT_GRACE: Duration = Duration::from_secs(1);

/// Writes every byte to this command's stdout and to the log file.
#[derive(Clone)]
struct Tee {
    log: Arc<Mutex<File>>,
}

impl Tee {
    fn bytes(&self, data: &[u8]) {
        let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = io::stdout().lock();
        let _ = out.write_all(data);
        let _ = out.flush();
        let _ = log.write_all(data);
    }

    fn line(&self, text: &str) {
        self.bytes(
            format!(
                "{text}
"
            )
            .as_bytes(),
        );
    }

    /// Run a step with stdout and stderr joined into the tee; returns its exit
    /// code once the step exits, without waiting for processes it left behind.
    fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> i32 {
        let spawned = io::pipe().and_then(|(mut reader, writer)| {
            // The parent's copies of the writer drop with the Command.
            let mut child = Command::new(program)
                .args(args)
                .current_dir(cwd)
                .stdin(Stdio::inherit())
                .stdout(writer.try_clone()?)
                .stderr(writer)
                .spawn()?;
            let tee = self.clone();
            let (done_tx, done_rx) = mpsc::channel();
            thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => tee.bytes(&buf[..n]),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                let _ = done_tx.send(());
            });
            let status = child.wait()?;
            // Output still in the pipe arrives within the grace period; a
            // leftover holder of the pipe is left to its detached reader.
            let _ = done_rx.recv_timeout(LEFTOVER_OUTPUT_GRACE);
            Ok(status)
        });
        match spawned {
            Ok(status) => exit_code(status),
            Err(e) => {
                self.line(&format!("[elite-gate] error: cannot run {program}: {e}"));
                127
            }
        }
    }

    /// `$(git ...)`: stdout captured with trailing newlines removed, stderr to the tee.
    fn capture_git(&self, cwd: &Path, args: &[&str]) -> String {
        match Command::new("git")
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::inherit())
            .output()
        {
            Ok(out) => {
                self.bytes(&out.stderr);
                String::from_utf8_lossy(&out.stdout)
                    .trim_end_matches('\n')
                    .to_string()
            }
            Err(e) => {
                self.line(&format!("[elite-gate] error: cannot run git: {e}"));
                String::new()
            }
        }
    }
}

fn utc_now() -> String {
    Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn checks_json() -> String {
    CHECKS
        .iter()
        .map(|c| format!("    {}", json_string(c)))
        .collect::<Vec<_>>()
        .join(",\n")
}

struct Run<'a> {
    repo_root: &'a str,
    root: &'a Path,
    args: &'a NightlyEliteGateArgs,
    log_path: String,
    summary_path: String,
    failed_step: &'static str,
}

impl Run<'_> {
    fn gate(&mut self, tee: &Tee) -> i32 {
        tee.line(&format!("[elite-gate] started at {}", utc_now()));
        tee.line(&format!("[elite-gate] repo={}", self.repo_root));
        if !self.root.is_dir() {
            tee.line(&format!(
                "[elite-gate] error: repository root not found: {}",
                self.repo_root
            ));
            return 1;
        }
        let root = self.root;

        self.failed_step = "branch_check";
        let branch = tee.capture_git(root, &["rev-parse", "--abbrev-ref", "HEAD"]);
        if branch != "main" && !self.args.allow_non_main {
            tee.line(&format!(
                "[elite-gate] error: expected main branch, got '{branch}'"
            ));
            return 10;
        }

        self.failed_step = "dirty_check";
        let status = tee.capture_git(root, &["status", "--short"]);
        if !status.is_empty() && !self.args.allow_dirty {
            tee.line("[elite-gate] error: working tree is dirty; refusing nightly gate");
            return 11;
        }

        if !self.args.skip_pull {
            self.failed_step = "git_pull";
            let rc = tee.run(root, "git", &["pull", "--ff-only", "origin", "main"]);
            if rc != 0 {
                return rc;
            }
        }

        let tools = ["run", "--locked", "-q", "-p", "hermes-parity-tools", "--"];
        let repo = self.repo_root;
        let steps: [(&'static str, &str, Vec<&str>); 6] = [
            (
                "cargo_test",
                "running cargo test -p hermes-cli",
                vec!["test", "-p", "hermes-cli"],
            ),
            (
                "deterministic_replay",
                "running deterministic replay suite",
                vec!["run-deterministic-replay-suite", "--repo-root", repo],
            ),
            (
                "security_release_gate_v2",
                "running security release gate v2",
                vec!["run-security-release-gate-v2", "--repo-root", repo],
            ),
            (
                "performance_autopilot",
                "running performance autopilot",
                vec!["run-performance-autopilot", "--repo-root", repo],
            ),
            (
                "upstream_surface_coverage",
                "running upstream surface coverage gate",
                vec![
                    "run-upstream-surface-coverage-gate",
                    "--repo-root",
                    repo,
                    "--upstream-ref",
                    "upstream/main",
                ],
            ),
            (
                "background_queue_audit",
                "auditing background queue health",
                vec!["audit-background-queue"],
            ),
        ];
        for (step, message, tail) in steps {
            self.failed_step = step;
            tee.line(&format!("[elite-gate] {message}"));
            let argv: Vec<&str> = if step == "cargo_test" {
                tail
            } else {
                tools.iter().copied().chain(tail).collect()
            };
            let rc = tee.run(root, "cargo", &argv);
            if rc != 0 {
                return rc;
            }
        }

        self.failed_step = "write_summary";
        let head_sha = tee.capture_git(root, &["rev-parse", "HEAD"]);
        let summary = format!(
            "{{\n  \"ok\": true,\n  \"timestamp_utc\": \"{}\",\n  \"repo_root\": {},\n  \"head_sha\": {},\n  \"log_path\": {},\n  \"checks\": [\n{}\n  ]\n}}\n",
            utc_now(),
            json_string(self.repo_root),
            json_string(&head_sha),
            json_string(&self.log_path),
            checks_json()
        );
        if let Err(e) = fs::write(&self.summary_path, summary) {
            tee.line(&format!(
                "[elite-gate] error: cannot write {}: {e}",
                self.summary_path
            ));
        }
        tee.line("[elite-gate] complete");
        tee.line(&format!("[elite-gate] summary={}", self.summary_path));
        0
    }
}

fn write_or_warn(path: &str, contents: String) {
    if let Err(e) = fs::write(path, contents) {
        eprintln!("[elite-gate] error: cannot write {path}: {e}");
    }
}

pub fn run(args: NightlyEliteGateArgs) -> Result<i32, String> {
    let repo_root = repo_root_arg(args.repo_root.as_deref())?;
    let log_dir = match &args.log_dir {
        Some(dir) => absolute_arg("--log-dir", dir)?,
        None => format!("{}/.hermes-agent-ultra/logs/elite-gate", home_dir()?),
    };
    let sink_order = env_or(
        "AUTOMATION_SUMMARY_SINK_ORDER",
        "contextlattice,github,local",
    );
    let context_project = env_or("AUTOMATION_SUMMARY_CONTEXT_PROJECT", &basename(&repo_root));
    let topic_path = env_or(
        "AUTOMATION_SUMMARY_CONTEXT_TOPIC_PATH",
        "ops/nightly-elite-gate",
    );
    let file_name = env_or(
        "AUTOMATION_SUMMARY_CONTEXT_FILE_NAME",
        "ops/nightly-elite-gate.md",
    );
    let agent_id = env_or(
        "AUTOMATION_SUMMARY_CONTEXT_AGENT_ID",
        "hermes_ultra_nightly",
    );
    let github_issue = env_or("AUTOMATION_SUMMARY_GITHUB_ISSUE", "0");
    let local_path = env_or(
        "AUTOMATION_SUMMARY_LOCAL_PATH",
        &format!("{log_dir}/nightly-elite-gate-fallback.log"),
    );

    fs::create_dir_all(&log_dir).map_err(|e| format!("mkdir: {log_dir}: {e}"))?;
    let stamp = Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string();
    let log_path = format!("{log_dir}/nightly-elite-gate-{stamp}.log");
    let summary_path = format!("{log_dir}/nightly-elite-gate-latest.json");
    let body_path = format!("{log_dir}/nightly-elite-gate-summary-{stamp}.md");
    let meta_path = format!("{log_dir}/nightly-elite-gate-summary-{stamp}.json");
    let log = File::create(&log_path).map_err(|e| format!("{log_path}: {e}"))?;
    let tee = Tee {
        log: Arc::new(Mutex::new(log)),
    };

    let root = PathBuf::from(&repo_root);
    let mut run = Run {
        repo_root: &repo_root,
        root: &root,
        args: &args,
        log_path: log_path.clone(),
        summary_path: summary_path.clone(),
        failed_step: "bootstrap",
    };
    let rc = run.gate(&tee);
    let failed_step = run.failed_step;

    let head_sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .stderr(Stdio::null())
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim_end_matches('\n')
                .to_string()
        })
        .unwrap_or_default();

    if rc != 0 {
        write_or_warn(
            &summary_path,
            format!(
                "{{\n  \"ok\": false,\n  \"timestamp_utc\": \"{}\",\n  \"repo_root\": {},\n  \"head_sha\": {},\n  \"log_path\": {},\n  \"failed_step\": \"{failed_step}\",\n  \"exit_code\": {rc},\n  \"checks\": [\n{}\n  ]\n}}\n",
                utc_now(),
                json_string(&repo_root),
                json_string(&head_sha),
                json_string(&log_path),
                checks_json()
            ),
        );
    }

    let bullets: String = [
        "`cargo test -p hermes-cli`",
        "`crates/hermes-parity-tools` (`run-deterministic-replay-suite`)",
        "`crates/hermes-parity-tools` (`run-security-release-gate-v2`)",
        "`crates/hermes-parity-tools` (`run-performance-autopilot`)",
        "`crates/hermes-parity-tools` (`run-upstream-surface-coverage-gate`)",
        "`crates/hermes-parity-tools` (`audit-background-queue`)",
    ]
    .iter()
    .map(|c| format!("  - {c}\n"))
    .collect();
    write_or_warn(
        &body_path,
        format!(
            "Nightly elite gate completed.\n\n- exit_code: `{rc}`\n- failed_step: `{failed_step}`\n- summary_path: `{summary_path}`\n- log_path: `{log_path}`\n- checks:\n{bullets}"
        ),
    );
    write_or_warn(
        &meta_path,
        format!(
            "{{\n  \"head_sha\": {},\n  \"log_path\": {},\n  \"summary_path\": {},\n  \"exit_code\": {rc},\n  \"failed_step\": \"{failed_step}\",\n  \"checks\": [\n{}\n  ]\n}}\n",
            json_string(&head_sha),
            json_string(&log_path),
            json_string(&summary_path),
            checks_json()
        ),
    );

    let status = if rc == 0 { "ok" } else { "failed" };
    let published = std::env::current_exe().and_then(|exe| {
        Command::new(exe)
            .args([
                "publish-automation-summary",
                "--repo-root",
                &repo_root,
                "--summary-kind",
                "nightly_elite_gate",
                "--status",
                status,
                "--title",
                "Nightly Elite Gate Summary",
                "--summary-body-file",
                &body_path,
                "--metadata-file",
                &meta_path,
                "--sink-order",
                &sink_order,
                "--context-project",
                &context_project,
                "--context-topic-path",
                &topic_path,
                "--context-file-name",
                &file_name,
                "--context-agent-id",
                &agent_id,
                "--github-issue",
                &github_issue,
                "--local-path",
                &local_path,
                "--json",
            ])
            .current_dir(&root)
            .stdout(Stdio::null())
            .status()
    });
    if !matches!(published, Ok(s) if s.success()) {
        eprintln!("[elite-gate] warning: summary publish failed; fallback file={local_path}");
    }
    Ok(rc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_matches_the_shell_command() {
        assert_eq!(basename("/a/b/repo"), "repo");
        assert_eq!(basename("/a/b/repo/"), "repo");
        assert_eq!(basename("repo"), "repo");
        assert_eq!(basename("/"), "/");
        assert_eq!(basename(""), "");
    }

    #[test]
    fn path_arguments_become_absolute_and_empty_ones_fail() {
        let cwd = std::env::current_dir().expect("cwd");
        let rel = absolute_arg("--log-dir", "logs/x").expect("absolute");
        assert_eq!(rel, cwd.join("logs/x").to_string_lossy());
        assert_eq!(repo_root_arg(None).expect("cwd"), cwd.to_string_lossy());
        assert_eq!(
            absolute_arg("--repo-root", ""),
            Err("missing value for --repo-root".to_string())
        );
    }

    #[test]
    fn checks_render_as_a_json_array_body() {
        let parsed: serde_json::Value =
            serde_json::from_str(&format!("[\n{}\n]", checks_json())).expect("json");
        assert_eq!(parsed.as_array().map(Vec::len), Some(6));
        assert_eq!(parsed[0], "cargo test -p hermes-cli");
    }
}
