//! The nightly elite gate and its launchd commands, run in a sandbox with fake
//! `cargo` and `launchctl` on `PATH` and a throwaway git repository. Each case
//! records stdout, stderr, the exit code, every file written and every fake
//! tool call, with temporary paths and times masked, and compares that record
//! with the one the replaced shell script produced.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_hermes-parity-tools")
}

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

const FAKE_CARGO: &str = r#"#!/usr/bin/env bash
for a in "$@"; do
  if [[ "$a" == publish-automation-summary ]]; then
    while [[ "$1" != "--" ]]; do shift; done
    shift
    exec "$HERMES_PARITY_BIN" "$@"
  fi
done
printf 'cargo %s (cwd=%s)\n' "$*" "$PWD" >> "$FAKE_LOG"
if [[ -n "${FAKE_CARGO_FAIL:-}" && "$*" == *"$FAKE_CARGO_FAIL"* ]]; then
  echo "fake cargo failing: $*" >&2
  exit "${FAKE_CARGO_RC:-1}"
fi
if [[ -n "${FAKE_CARGO_LEAK:-}" && "$*" == "test -p hermes-cli" ]]; then
  ( sleep 30 & )
fi
echo "fake cargo ok: $*"
echo "fake cargo note" >&2
"#;

const FAKE_LAUNCHCTL: &str = r#"#!/usr/bin/env bash
printf 'launchctl %s\n' "$*" >> "$FAKE_LOG"
case "$1" in
  print)
    if [[ -f "${FAKE_PRINT:-/nonexistent}" ]]; then cat "$FAKE_PRINT"; exit 0; fi
    echo "Could not find service" >&2; exit 113 ;;
  bootout) echo "Boot-out failed: 5: Input/output error" >&2; exit 5 ;;
  bootstrap) echo "bootstrap says hi"; exit "${FAKE_BOOTSTRAP_RC:-0}" ;;
esac
"#;

struct Sandbox {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical");
        for dir in ["bin", "home", "repo/scripts"] {
            fs::create_dir_all(root.join(dir)).expect("mkdir");
        }
        for (name, text) in [("cargo", FAKE_CARGO), ("launchctl", FAKE_LAUNCHCTL)] {
            let path = root.join("bin").join(name);
            fs::write(&path, text).expect("write fake");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        fs::copy(
            source_root().join("scripts/publish_automation_summary.py"),
            root.join("repo/scripts/publish_automation_summary.py"),
        )
        .expect("copy publisher");
        let sandbox = Self { _tmp: tmp, root };
        sandbox.git(&["init", "-q", "-b", "main"]);
        sandbox.git(&["add", "-A"]);
        sandbox.git(&["commit", "-q", "-m", "init"]);
        sandbox
    }

    fn path(&self, rel: &str) -> String {
        self.root.join(rel).to_string_lossy().into_owned()
    }

    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        let path = format!(
            "{}:{}",
            self.path("bin"),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.current_dir(self.root.join("repo"))
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.path("home"))
            .env("FAKE_LOG", self.path("calls.log"))
            .env("FAKE_PRINT", self.path("print.txt"))
            .env("HERMES_PARITY_BIN", bin())
            .env("AUTOMATION_SUMMARY_SINK_ORDER", "local")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd
    }

    fn git(&self, args: &[&str]) {
        let out = self.command("git").args(args).output().expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    /// Run one implementation and return its masked record.
    /// stdout and stderr go to files, so a process left holding them cannot
    /// stall the record; a run that takes 20 seconds or more fails the case.
    fn record(&self, mut cmd: Command) -> String {
        let (out_path, err_path) = (self.root.join("stdout.txt"), self.root.join("stderr.txt"));
        cmd.stdout(fs::File::create(&out_path).expect("stdout file"))
            .stderr(fs::File::create(&err_path).expect("stderr file"));
        let started = std::time::Instant::now();
        let status = cmd.status().expect("run");
        let took = started.elapsed();
        assert!(took.as_secs() < 20, "run took {took:?}");
        let stdout = fs::read(&out_path).expect("stdout");
        let stderr = fs::read(&err_path).expect("stderr");
        let mut files = BTreeMap::new();
        collect(&self.root.join("home"), &self.root, &mut files);
        let calls = fs::read_to_string(self.root.join("calls.log")).unwrap_or_default();
        let mut text = format!(
            "exit={:?}\n--- stdout\n{}--- stderr\n{}--- calls\n{calls}",
            status.code(),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        );
        for (name, body) in files {
            text.push_str(&format!("--- file {name}\n{body}"));
        }
        self.mask(&text)
    }

    fn mask(&self, text: &str) -> String {
        let root = self.root.to_string_lossy();
        let text = text.replace(root.as_ref(), "<ROOT>");
        let uid = String::from_utf8(Command::new("id").arg("-u").output().expect("id").stdout)
            .expect("uid");
        let text = text.replace(&format!("gui/{}", uid.trim()), "gui/<UID>");
        let program = Regex::new(r"(?s)<key>ProgramArguments</key>.*?</array>").expect("regex");
        let text = program.replace_all(&text, "<PROGRAM>");
        let stamp = Regex::new(r"\d{8}T\d{6}Z").expect("regex");
        let text = stamp.replace_all(&text, "<STAMP>");
        let time =
            Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|\+00:00)?").expect("regex");
        time.replace_all(&text, "<TIME>").into_owned()
    }
}

fn collect(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, root, out);
        } else {
            let name = path.strip_prefix(root).expect("prefix").to_string_lossy();
            let body = fs::read(&path).expect("read");
            out.insert(
                name.into_owned(),
                String::from_utf8_lossy(&body).into_owned(),
            );
        }
    }
}

/// What a case sets up and runs.
struct Case {
    name: &'static str,
    script: &'static str,
    args: Vec<String>,
    env: Vec<(&'static str, String)>,
    setup: fn(&Sandbox),
}

fn run_case(case: &Case) -> (String, String) {
    let run = |shell: bool| {
        let sandbox = Sandbox::new();
        (case.setup)(&sandbox);
        let mut cmd = if shell {
            let mut c = sandbox.command("bash");
            c.arg(source_root().join(format!("scripts/{}.sh", case.script)));
            c
        } else {
            let mut c = sandbox.command(bin());
            c.arg(case.script);
            c
        };
        let args: Vec<String> = case
            .args
            .iter()
            .map(|a| a.replace("<ROOT>", &sandbox.root.to_string_lossy()))
            .collect();
        cmd.args(&args);
        for (k, v) in &case.env {
            cmd.env(k, v.replace("<ROOT>", &sandbox.root.to_string_lossy()));
        }
        sandbox.record(cmd)
    };
    (run(true), run(false))
}

fn check(case: Case) {
    let (shell, rust) = run_case(&case);
    if let Ok(dir) = std::env::var("ELITE_GATE_BLESS") {
        fs::create_dir_all(&dir).expect("bless dir");
        fs::write(Path::new(&dir).join(format!("{}.txt", case.name)), &shell).expect("bless");
    }
    assert_eq!(rust, shell, "case {}", case.name);
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn nightly(
    name: &'static str,
    extra: &[&str],
    env: Vec<(&'static str, String)>,
    setup: fn(&Sandbox),
) {
    let mut args = strings(&[
        "--repo-root",
        "<ROOT>/repo",
        "--log-dir",
        "<ROOT>/home/logs",
    ]);
    args.extend(strings(extra));
    check(Case {
        name,
        script: "run-nightly-elite-gate",
        args,
        env,
        setup,
    });
}

fn nothing(_: &Sandbox) {}

#[test]
fn nightly_gate_passes_every_step() {
    nightly("nightly_ok", &["--skip-pull"], vec![], nothing);
}

#[test]
fn nightly_gate_refuses_another_branch() {
    nightly("nightly_branch", &["--skip-pull"], vec![], |s| {
        s.git(&["checkout", "-q", "-b", "feature"]);
    });
}

#[test]
fn nightly_gate_refuses_a_dirty_tree() {
    nightly("nightly_dirty", &["--skip-pull"], vec![], |s| {
        s.write("repo/stray.txt", "x\n");
    });
}

#[test]
fn nightly_gate_allows_branch_and_dirt_when_asked() {
    let extra = ["--skip-pull", "--allow-non-main", "--allow-dirty"];
    nightly("nightly_allowed", &extra, vec![], |s| {
        s.git(&["checkout", "-q", "-b", "feature"]);
        s.write("repo/stray.txt", "x\n");
    });
}

#[test]
fn nightly_gate_reports_the_failing_test_step() {
    let env = vec![
        ("FAKE_CARGO_FAIL", "test -p hermes-cli".to_string()),
        ("FAKE_CARGO_RC", "3".to_string()),
    ];
    nightly("nightly_test_fail", &["--skip-pull"], env, nothing);
}

#[test]
fn nightly_gate_reports_the_failing_last_step() {
    let env = vec![
        ("FAKE_CARGO_FAIL", "audit-background-queue".to_string()),
        ("FAKE_CARGO_RC", "7".to_string()),
    ];
    nightly("nightly_audit_fail", &["--skip-pull"], env, nothing);
}

#[test]
fn nightly_gate_does_not_wait_for_processes_a_step_leaves_running() {
    let env = vec![("FAKE_CARGO_LEAK", "1".to_string())];
    nightly("nightly_leftover", &["--skip-pull"], env, nothing);
}

#[test]
fn nightly_gate_pulls_origin_main() {
    nightly("nightly_pull", &[], vec![], |s| {
        let origin = s.path("origin.git");
        s.git(&["clone", "-q", "--bare", ".", &origin]);
        s.git(&["remote", "add", "origin", &origin]);
        s.git(&["fetch", "-q", "origin"]);
        s.git(&["branch", "-q", "--set-upstream-to=origin/main"]);
    });
}

#[test]
fn nightly_gate_stops_when_the_pull_fails() {
    nightly("nightly_pull_fail", &[], vec![], nothing);
}

#[test]
fn nightly_gate_warns_when_publishing_fails() {
    let env = vec![("AUTOMATION_SUMMARY_LOCAL_PATH", "<ROOT>/home".to_string())];
    nightly("nightly_publish_fail", &["--skip-pull"], env, nothing);
}

fn install(name: &'static str, extra: &[&str], env: Vec<(&'static str, String)>) {
    let mut args = strings(&["--repo-root", "<ROOT>/repo"]);
    args.extend(strings(extra));
    check(Case {
        name,
        script: "install-elite-gate-launchd",
        args,
        env,
        setup: install_runner,
    });
}

/// The script made its runner executable and failed without it.
fn install_runner(s: &Sandbox) {
    s.write(
        "repo/scripts/run-nightly-elite-gate.sh",
        "#!/bin/sh
",
    );
}

#[test]
fn install_writes_the_plist_without_loading() {
    install("install_no_load", &["--no-load"], vec![]);
}

#[test]
fn install_loads_the_agent() {
    let extra = [
        "--hour",
        "7",
        "--minute",
        "5",
        "--label-prefix",
        "org.example",
    ];
    install("install_load", &extra, vec![]);
}

#[test]
fn install_stops_when_bootstrap_fails() {
    install(
        "install_bootstrap_fail",
        &[],
        vec![("FAKE_BOOTSTRAP_RC", "5".to_string())],
    );
}

#[test]
fn install_honours_custom_folders() {
    let extra = [
        "--no-load",
        "--agents-dir",
        "<ROOT>/home/agents",
        "--log-dir",
        "<ROOT>/home/l",
    ];
    install("install_folders", &extra, vec![]);
}

#[test]
fn install_rejects_bad_schedules() {
    install("install_hour_24", &["--hour", "24"], vec![]);
    install("install_hour_text", &["--hour", "abc"], vec![]);
    install("install_minute_60", &["--minute", "60"], vec![]);
    install("install_minute_negative", &["--minute", "-1"], vec![]);
}

fn status(name: &'static str, extra: &[&str], setup: fn(&Sandbox)) {
    check(Case {
        name,
        script: "status-elite-gate-launchd",
        args: strings(extra),
        env: vec![],
        setup,
    });
}

fn status_logs(s: &Sandbox) {
    let out: String = (1..=40).map(|i| format!("out line {i}\n")).collect();
    s.write(
        "home/.hermes-agent-ultra/logs/elite-gate-launchd.out.log",
        &out,
    );
    s.write(
        "home/.hermes-agent-ultra/logs/elite-gate-launchd.err.log",
        "e1\ne2\ne3",
    );
    s.write(
        "home/.hermes-agent-ultra/logs/elite-gate/nightly-elite-gate-latest.json",
        "{\n  \"ok\": true\n}\n",
    );
}

#[test]
fn status_reports_an_unloaded_agent() {
    status("status_not_loaded", &[], nothing);
}

#[test]
fn status_reports_pid_and_log_tails() {
    status("status_loaded", &["--tail", "3"], |s| {
        status_logs(s);
        s.write(
            "print.txt",
            "gui/501/x = {\n\tstate = running\n\tpid = 4242\n}\n",
        );
    });
}

#[test]
fn status_verbose_prints_the_record_head() {
    status(
        "status_verbose",
        &["--verbose", "--log-dir", "<ROOT>/home/none"],
        |s| {
            let record: String = (1..=200).map(|i| format!("\trow {i}\n")).collect();
            s.write("print.txt", &format!("\tpid = abc\n{record}"));
        },
    );
}

#[test]
fn status_handles_a_record_without_a_pid_and_no_tail() {
    status("status_no_pid", &["--tail", "0"], |s| {
        status_logs(s);
        s.write(
            "print.txt",
            "gui/501/x = {
	state = waiting
}
",
        );
    });
}

fn uninstall(name: &'static str, setup: fn(&Sandbox)) {
    check(Case {
        name,
        script: "uninstall-elite-gate-launchd",
        args: vec![],
        env: vec![],
        setup,
    });
}

#[test]
fn uninstall_removes_the_plist() {
    uninstall("uninstall_present", |s| {
        s.write(
            "home/Library/LaunchAgents/com.hermes_agent_ultra.nightly_elite_gate.plist",
            "<plist/>\n",
        );
    });
}

#[test]
fn uninstall_succeeds_without_a_plist() {
    uninstall("uninstall_absent", nothing);
}
