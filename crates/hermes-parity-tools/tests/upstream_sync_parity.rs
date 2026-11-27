//! The upstream sync commands, run in a sandbox with fake `cargo`, `gh` and
//! `crontab` on `PATH`, an `upstream` and an `origin` bare repository and a
//! work clone. Each case records stdout, stderr, the exit code, every fake
//! tool call, the report and home files and the resulting refs, with the
//! sandbox's own paths, commit ids and times masked, and compares that record
//! with `tests/fixtures/upstream_sync/<case>.txt`. Those files are the records
//! the shell scripts (`scripts/sync-upstream.sh`, `scripts/cron-upstream-sync.sh`
//! and `scripts/install-upstream-sync-cron.sh`) produced for the same cases
//! before the port, captured whilst both ran side by side.
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

const FAKE_CARGO: &str = r#"#!/usr/bin/env bash
printf 'cargo %s\n' "$*" >> "$FAKE_LOG"
if [[ -n "${FAKE_CARGO_FAIL:-}" && "$*" == *"$FAKE_CARGO_FAIL"* ]]; then
  echo "fake cargo failing: $*"
  echo "fake cargo stderr" >&2
  echo "second line"
  exit 3
fi
case "$*" in
  *redteam*) echo "redteam line one"; echo "[redteam-gate] Report: /reports/redteam.json" ;;
  *elite*) echo "[elite-sync-gate] Report: /reports/elite-a.json"; echo "[elite-sync-gate] Report: /reports/elite-b.json" ;;
  *docs*) if [[ -n "${FAKE_DOC_EDIT:-}" ]]; then echo "refreshed" >> README.md; fi; echo "docs ok" ;;
esac
echo "fake cargo ok"
echo "fake cargo note" >&2
"#;

const FAKE_GH: &str = r#"#!/usr/bin/env bash
sub="$1 ${2:-}"
{
  printf 'gh'
  body=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == --body-file ]]; then body="$2"; printf ' --body-file <BODY>'; shift 2; continue; fi
    printf ' [%s]' "$1"; shift
  done
  printf '\n'
  if [[ -n "$body" ]]; then sed 's/^/  | /' "$body"; fi
} >> "$FAKE_LOG"
case "$sub" in
  "issue create") [[ -n "${FAKE_GH_FAIL:-}" ]] && exit 1; echo "https://example.invalid/issues/7" ;;
  "pr create") [[ -n "${FAKE_GH_FAIL:-}" ]] && { echo "pr create failed" >&2; exit 1; }; echo "https://example.invalid/pull/8" ;;
  "label create") echo "label noise" ;;
esac
exit 0
"#;

/// Real git, with the messages of the commands whose output the tools do not
/// read sent to a side log, so the records do not depend on git's wording.
const GIT_WRAPPER: &str = r#"#!/usr/bin/env bash
case "$1" in
  rev-parse|remote|status|log|diff|rev-list|merge-base) exec "$REAL_GIT" "$@" ;;
esac
"$REAL_GIT" "$@" >> "$GIT_NOISE" 2>&1
"#;

const FAKE_CRONTAB: &str = r#"#!/usr/bin/env bash
printf 'crontab %s\n' "$*" >> "$FAKE_LOG"
store="$HOME/crontab.txt"
if [[ "$1" == -l ]]; then
  if [[ -f "$store" ]]; then cat "$store"; exit 0; fi
  echo "no crontab for t" >&2; exit 1
fi
cp "$1" "$store"
"#;

const RISK_PATHS: &str = "# comment\n  # indented comment\n\ncrates/hermes-auth/**\nscripts/setup-*.sh\ndocs/[nN]otes.md\n";

/// Host tools the scripts and git need; `gh`, `cargo` and `crontab` are
/// left out so only the fakes answer.
const HOST_TOOLS: [&str; 28] = [
    "bash", "sh", "env", "git", "mkdir", "mktemp", "rm", "sed", "awk", "xargs", "python3", "cat",
    "dirname", "flock", "grep", "chmod", "date", "cp", "id", "sleep", "tr", "head", "tail", "ls",
    "uname", "basename", "readlink", "echo",
];

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    /// Upstream equals origin's base; origin has a fork commit on top.
    NoUpdates,
    /// Upstream adds 2 files nothing in the risk list matches.
    Clean,
    /// Upstream touches 3 risk-listed paths.
    Risk,
    /// Upstream and origin both change README.md.
    Conflict,
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new(scenario: Scenario) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical");
        for dir in ["bin", "sysbin", "home", "tmp", "reports", "seed/scripts"] {
            fs::create_dir_all(root.join(dir)).expect("mkdir");
        }
        for (name, text) in [
            ("cargo", FAKE_CARGO),
            ("gh", FAKE_GH),
            ("crontab", FAKE_CRONTAB),
            ("git", GIT_WRAPPER),
        ] {
            let path = root.join("bin").join(name);
            fs::write(&path, text).expect("write fake");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let host_path = std::env::var("PATH").unwrap_or_default();
        for tool in HOST_TOOLS {
            if let Some(found) = std::env::split_paths(&host_path)
                .map(|dir| dir.join(tool))
                .find(|p| p.is_file())
            {
                std::os::unix::fs::symlink(found, root.join("sysbin").join(tool)).expect("link");
            }
        }
        let sandbox = Self { _tmp: tmp, root };
        sandbox.seed(scenario);
        sandbox
    }

    fn path(&self, rel: &str) -> String {
        self.root.join(rel).to_string_lossy().into_owned()
    }

    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(self.root.join("repo"))
            .env_clear()
            .env(
                "PATH",
                format!("{}:{}", self.path("bin"), self.path("sysbin")),
            )
            .env("HOME", self.path("home"))
            .env("TMPDIR", self.path("tmp"))
            .env("LC_ALL", "C")
            .env("FAKE_LOG", self.path("calls.log"))
            .env("REAL_GIT", self.path("sysbin/git"))
            .env("GIT_NOISE", self.path("git-noise.log"))
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

    fn git_in(&self, dir: &str, args: &[&str]) -> String {
        let mut cmd = self.command(&self.path("sysbin/git"));
        cmd.current_dir(self.root.join(dir)).args(args);
        let out = cmd.output().expect("git");
        assert!(out.status.success(), "git {args:?} in {dir}: {out:?}");
        String::from_utf8(out.stdout).expect("utf8")
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    fn commit(&self, subject: &str, files: &[(&str, &str)]) {
        for (rel, text) in files {
            self.write(&format!("seed/{rel}"), text);
        }
        self.git_in("seed", &["add", "-A"]);
        self.git_in("seed", &["commit", "-q", "-m", subject]);
    }

    fn seed(&self, scenario: Scenario) {
        fs::create_dir_all(self.root.join("remotes/NousResearch")).expect("mkdir");
        self.git_in("seed", &["init", "-q", "-b", "main"]);
        self.write("seed/README.md", "base\n");
        self.write("seed/.gitignore", ".sync-reports/\n");
        self.write("seed/docs/parity/PARITY_DASHBOARD.md", "dashboard\n");
        self.write("seed/scripts/upstream-risk-paths.txt", RISK_PATHS);
        self.commit("base", &[]);
        let upstream = self.path("remotes/NousResearch/hermes-agent.git");
        let origin = self.path("remotes/origin.git");
        self.git_in("seed", &["init", "-q", "--bare", &upstream]);
        self.git_in("seed", &["init", "-q", "--bare", "-b", "main", &origin]);
        self.git_in("seed", &["push", "-q", &upstream, "main"]);
        self.git_in("seed", &["push", "-q", &origin, "main"]);
        self.git_in("seed", &["checkout", "-q", "-b", "fork"]);
        let fork_readme = if scenario == Scenario::Conflict {
            "fork\n"
        } else {
            "base\n"
        };
        self.commit(
            "fork: carry",
            &[("fork.txt", "fork\n"), ("README.md", fork_readme)],
        );
        self.git_in("seed", &["push", "-q", &origin, "fork:main"]);
        self.git_in("seed", &["checkout", "-q", "main"]);
        match scenario {
            Scenario::NoUpdates => {}
            Scenario::Clean => {
                self.commit("upstream: add guide", &[("docs/guide.md", "guide\n")]);
                self.commit("upstream: add tool", &[("tools/a.txt", "a\n")]);
            }
            Scenario::Risk => {
                self.commit(
                    "upstream: touch risky paths",
                    &[
                        ("crates/hermes-auth/src/lib.rs", "// auth\n"),
                        ("scripts/setup-x.sh", "echo\n"),
                        ("docs/notes.md", "notes\n"),
                        ("docs/other.md", "other\n"),
                    ],
                );
            }
            Scenario::Conflict => {
                self.commit("upstream: change readme", &[("README.md", "upstream\n")]);
            }
        }
        self.git_in("seed", &["push", "-q", &upstream, "main"]);
        self.git_in(".", &["clone", "-q", &origin, "repo"]);
        self.git_in("repo", &["remote", "add", "upstream", &upstream]);
    }

    /// Run one implementation and return its masked record.
    /// `cron` cases run with the cron `PATH` in front, so real git answers
    /// there: its fetch lines and the host's `gh` notice are left out.
    fn record(&self, mut cmd: Command, cron: bool) -> String {
        let (out_path, err_path) = (self.root.join("stdout.txt"), self.root.join("stderr.txt"));
        cmd.stdout(fs::File::create(&out_path).expect("stdout file"))
            .stderr(fs::File::create(&err_path).expect("stderr file"));
        let started = std::time::Instant::now();
        let status = cmd.status().expect("run");
        let took = started.elapsed();
        assert!(took.as_secs() < 30, "run took {took:?}");
        let mut stdout = fs::read_to_string(&out_path).expect("stdout");
        let keep = |l: &&str| {
            !(l.contains("gh CLI not found") || l.starts_with("From ") || l.starts_with(" * "))
        };
        if cron {
            stdout = stdout
                .lines()
                .filter(keep)
                .map(|l| format!("{l}\n"))
                .collect();
        }
        let mut stderr = fs::read_to_string(&err_path).expect("stderr");
        if cron {
            stderr = stderr
                .lines()
                .filter(keep)
                .map(|l| format!("{l}\n"))
                .collect();
        }
        let calls = fs::read_to_string(self.root.join("calls.log")).unwrap_or_default();
        let mut text = format!(
            "exit={:?}\n--- stdout\n{stdout}--- stderr\n{stderr}--- calls\n{calls}",
            status.code()
        );
        let mut files = BTreeMap::new();
        for dir in ["home", "reports", "tmp", "repo/.sync-reports"] {
            collect(&self.root.join(dir), &self.root, &mut files);
        }
        for (name, body) in files {
            text.push_str(&format!("--- file {name}\n{body}"));
        }
        let cron_script = self.root.join("repo/scripts/cron-upstream-sync.sh");
        if let Ok(meta) = fs::metadata(&cron_script) {
            text.push_str(&format!(
                "--- cron script mode {:o}\n",
                meta.permissions().mode() & 0o777
            ));
        }
        for repo in ["remotes/origin.git", "repo"] {
            text.push_str(&format!(
                "--- refs {repo}\n{}",
                self.git_in(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
            ));
        }
        text.push_str(&format!(
            "--- status\n{}",
            self.git_in("repo", &["status", "--porcelain=v1", "-b"])
        ));
        self.mask(&text)
    }

    fn mask(&self, text: &str) -> String {
        let root = self.root.to_string_lossy();
        let text = text.replace(root.as_ref(), "<ROOT>");
        let tmp = Regex::new(r"<ROOT>/tmp/[^\s'`]+").expect("regex");
        let text = tmp.replace_all(&text, "<TMP>").into_owned();
        // Every commit in the sandbox, labelled by its subject.
        let mut labels: Vec<(String, String)> = Vec::new();
        for repo in ["seed", "remotes/origin.git", "repo"] {
            for line in self
                .git_in(repo, &["log", "--all", "--format=%H %s"])
                .lines()
            {
                let (sha, subject) = line.split_once(' ').unwrap_or((line, ""));
                labels.push((sha.to_string(), format!("<C:{subject}>")));
            }
        }
        let ids = Regex::new(r"\b[0-9a-f]{7,40}\b").expect("regex");
        let text = ids.replace_all(&text, |caps: &regex::Captures| {
            let id = &caps[0];
            labels
                .iter()
                .find(|(sha, _)| sha.starts_with(id))
                .map_or_else(|| id.to_string(), |(_, label)| label.clone())
        });
        let stamp = Regex::new(r"\d{8}-\d{6}").expect("regex");
        stamp.replace_all(&text, "<TS>").into_owned()
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

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Sync,
    Cron,
    Install,
}

impl Tool {
    fn subcommand(self) -> &'static str {
        match self {
            Tool::Sync => "sync-upstream",
            Tool::Cron => "run-upstream-sync-cron",
            Tool::Install => "install-upstream-sync-cron",
        }
    }
}

/// What a case sets up and runs.
struct Case {
    name: &'static str,
    tool: Tool,
    scenario: Scenario,
    args: Vec<String>,
    env: Vec<(&'static str, String)>,
    setup: fn(&Sandbox),
    /// When set, stderr must contain this text and is not compared byte for
    /// byte (the script's bash printed its own `${2:?}` message).
    stderr_contains: Option<&'static str>,
}

fn case(name: &'static str, tool: Tool, scenario: Scenario, args: &[&str]) -> Case {
    Case {
        name,
        tool,
        scenario,
        args: args.iter().map(|s| s.to_string()).collect(),
        env: Vec::new(),
        setup: |_| {},
        stderr_contains: None,
    }
}

impl Case {
    fn env(mut self, key: &'static str, value: &str) -> Self {
        self.env.push((key, value.to_string()));
        self
    }

    fn setup(mut self, setup: fn(&Sandbox)) -> Self {
        self.setup = setup;
        self
    }

    fn stderr_contains(mut self, text: &'static str) -> Self {
        self.stderr_contains = Some(text);
        self
    }
}

/// The sync arguments every full-run case starts from: fake gate commands by
/// absolute path, so a login shell's `PATH` cannot pick another `cargo`.
const SYNC_ARGS: [&str; 12] = [
    "--repo-root",
    "<ROOT>/repo",
    "--report-dir",
    "<ROOT>/reports",
    "--test-cmd",
    "<ROOT>/bin/cargo test -p hermes-gateway",
    "--redteam-cmd",
    "<ROOT>/bin/cargo redteam",
    "--elite-cmd",
    "<ROOT>/bin/cargo elite",
    "--doc-refresh-cmd",
    "<ROOT>/bin/cargo docs",
];

fn run(case: &Case) -> String {
    let sandbox = Sandbox::new(case.scenario);
    if case.tool == Tool::Install {
        sandbox.write("repo/scripts/cron-upstream-sync.sh", "#!/bin/sh\n");
    }
    (case.setup)(&sandbox);
    let root = sandbox.root.to_string_lossy().into_owned();
    let mut cmd = sandbox.command(bin());
    cmd.arg(case.tool.subcommand());
    cmd.args(case.args.iter().map(|a| a.replace("<ROOT>", &root)));
    for (k, v) in &case.env {
        cmd.env(k, v.replace("<ROOT>", &root));
    }
    let _lock = (case.name == "cron_lock_busy").then(|| {
        let dir = sandbox.root.join("home/.hermes/locks");
        fs::create_dir_all(&dir).expect("lock dir");
        let file = fs::File::create(dir.join("upstream-sync.lock")).expect("lock file");
        file.try_lock().expect("hold lock");
        file
    });
    let mut record = sandbox.record(cmd, case.tool == Tool::Cron);
    if let Some(text) = case.stderr_contains {
        let (head, rest) = record.split_once("--- stderr\n").expect("stderr section");
        let (stderr, tail) = rest.split_once("--- calls\n").expect("calls section");
        assert!(
            stderr.contains(text),
            "case {}: stderr {stderr:?}",
            case.name
        );
        record = format!("{head}--- stderr\n<STDERR>\n--- calls\n{tail}");
    }
    record
}

fn check(case: Case) {
    let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/upstream_sync")
        .join(format!("{}.txt", case.name));
    let expected = fs::read_to_string(&expected).expect("fixture");
    assert_eq!(run(&case), expected, "case {}", case.name);
}

fn sync(name: &'static str, scenario: Scenario, extra: &[&str]) -> Case {
    let mut c = case(name, Tool::Sync, scenario, &SYNC_ARGS);
    c.args.extend(extra.iter().map(|s| s.to_string()));
    c
}

#[test]
fn sync_help() {
    check(case(
        "sync_help",
        Tool::Sync,
        Scenario::NoUpdates,
        &["--help"],
    ));
}

#[test]
fn sync_unknown_flag() {
    check(case(
        "sync_unknown_flag",
        Tool::Sync,
        Scenario::NoUpdates,
        &["--bogus"],
    ));
}

#[test]
fn sync_missing_value() {
    check(
        case(
            "sync_missing_value",
            Tool::Sync,
            Scenario::NoUpdates,
            &["--origin"],
        )
        .stderr_contains("missing value for --origin"),
    );
}

#[test]
fn sync_empty_value() {
    check(
        case(
            "sync_empty_value",
            Tool::Sync,
            Scenario::NoUpdates,
            &["--test-cmd", ""],
        )
        .stderr_contains("missing value for --test-cmd"),
    );
}

#[test]
fn sync_bad_mode() {
    check(sync("sync_bad_mode", Scenario::NoUpdates, &["--mode", "x"]));
}

#[test]
fn sync_bad_strategy() {
    check(sync(
        "sync_bad_strategy",
        Scenario::NoUpdates,
        &["--strategy", "rebase"],
    ));
}

#[test]
fn sync_non_official_upstream() {
    check(
        sync("sync_non_official_upstream", Scenario::Clean, &[])
            .env("EXPECTED_UPSTREAM_REPO", "Acme/Other"),
    );
}

#[test]
fn sync_non_official_upstream_allowed() {
    check(
        sync(
            "sync_non_official_upstream_allowed",
            Scenario::Clean,
            &["--dry-run"],
        )
        .env("EXPECTED_UPSTREAM_REPO", "Acme/Other")
        .env("ALLOW_NON_OFFICIAL_UPSTREAM", "1"),
    );
}

#[test]
fn sync_missing_origin_remote() {
    check(sync(
        "sync_missing_origin_remote",
        Scenario::Clean,
        &["--origin", "nope"],
    ));
}

#[test]
fn sync_not_a_repository() {
    check(sync(
        "sync_not_a_repository",
        Scenario::Clean,
        &["--repo-root", "<ROOT>/home"],
    ));
}

#[test]
fn sync_dirty_tree() {
    check(
        sync("sync_dirty_tree", Scenario::Clean, &[])
            .setup(|s| s.write("repo/stray.txt", "stray\n")),
    );
}

#[test]
fn sync_no_updates() {
    check(sync("sync_no_updates", Scenario::NoUpdates, &[]));
}

#[test]
fn sync_dry_run_defaults() {
    check(case(
        "sync_dry_run_defaults",
        Tool::Sync,
        Scenario::Clean,
        &["--repo-root", "<ROOT>/repo", "--dry-run"],
    ));
}

#[test]
fn sync_dry_run_env_defaults() {
    check(
        case(
            "sync_dry_run_env_defaults",
            Tool::Sync,
            Scenario::Clean,
            &["--repo-root", "<ROOT>/repo", "--dry-run"],
        )
        .env("RUN_REDTEAM_GATE", "0")
        .env("REDTEAM_CMD", "red")
        .env("RUN_ELITE_GATE", "1")
        .env("ELITE_CMD", "elite")
        .env("ELITE_ROLLBACK_CMD", "undo")
        .env("PR_LABELS", "a,b")
        .env("STRICT_RISK_GATE", "1"),
    );
}

#[test]
fn sync_risk_pass_dry_run() {
    check(sync(
        "sync_risk_pass_dry_run",
        Scenario::Clean,
        &["--strict-risk-gate", "--dry-run"],
    ));
}

#[test]
fn sync_risk_blocked_dry_run() {
    check(sync(
        "sync_risk_blocked_dry_run",
        Scenario::Risk,
        &["--strict-risk-gate", "--dry-run"],
    ));
}

#[test]
fn sync_risk_blocked() {
    check(sync(
        "sync_risk_blocked",
        Scenario::Risk,
        &["--strict-risk-gate", "--conflict-label", "risky"],
    ));
}

#[test]
fn sync_risk_blocked_issue_fails() {
    check(
        sync(
            "sync_risk_blocked_issue_fails",
            Scenario::Risk,
            &["--strict-risk-gate"],
        )
        .env("FAKE_GH_FAIL", "1"),
    );
}

#[test]
fn sync_risk_blocked_no_issue() {
    check(sync(
        "sync_risk_blocked_no_issue",
        Scenario::Risk,
        &["--strict-risk-gate", "--no-conflict-issue"],
    ));
}

#[test]
fn sync_risk_no_strict_after_strict() {
    check(sync(
        "sync_risk_no_strict_after_strict",
        Scenario::Risk,
        &["--strict-risk-gate", "--no-strict-risk-gate", "--dry-run"],
    ));
}

#[test]
fn sync_risk_bypassed() {
    check(sync(
        "sync_risk_bypassed",
        Scenario::Risk,
        &["--strict-risk-gate", "--allow-risk-paths"],
    ));
}

#[test]
fn sync_risk_file_missing() {
    check(sync(
        "sync_risk_file_missing",
        Scenario::Risk,
        &["--strict-risk-gate", "--risk-paths-file", "<ROOT>/none.txt"],
    ));
}

#[test]
fn sync_merge_no_pr() {
    check(sync("sync_merge_no_pr", Scenario::Clean, &["--no-pr"]));
}

#[test]
fn sync_merge_draft_pr_labels() {
    check(sync(
        "sync_merge_draft_pr_labels",
        Scenario::Clean,
        &["--draft-pr", "--pr-labels", " a , ,b  c,"],
    ));
}

#[test]
fn sync_pr_create_fails() {
    check(sync("sync_pr_create_fails", Scenario::Clean, &[]).env("FAKE_GH_FAIL", "1"));
}

#[test]
fn sync_no_gh() {
    check(
        sync("sync_no_gh", Scenario::Clean, &[])
            .setup(|s| fs::remove_file(s.root.join("bin/gh")).expect("rm gh")),
    );
}

#[test]
fn sync_queue_summary() {
    check(
        sync("sync_queue_summary", Scenario::Clean, &[]).setup(|s| {
            s.write(
                "repo/docs/parity/upstream-missing-queue.json",
                r#"[{"state":"queued","classification":"Actionable"},{"status":"OPEN"},{"state":0,"status":"todo"},{"state":1,"status":"todo"},"x",{"class":"required"}]"#,
            );
            s.git_in("repo", &["add", "-A"]);
            s.git_in("repo", &["commit", "-q", "-m", "queue"]);
        }),
    );
}

#[test]
fn sync_queue_summary_entries() {
    check(
        sync("sync_queue_summary_entries", Scenario::Clean, &[]).setup(|s| {
            s.write(
                "repo/docs/parity/upstream-missing-queue.json",
                r#"{"items":[],"entries":[{"state":"in_progress"},{"classification":"functional"}]}"#,
            );
            s.git_in("repo", &["add", "-A"]);
            s.git_in("repo", &["commit", "-q", "-m", "queue"]);
        }),
    );
}

#[test]
fn sync_queue_summary_invalid() {
    check(
        sync("sync_queue_summary_invalid", Scenario::Clean, &[]).setup(|s| {
            s.write("repo/docs/parity/upstream-missing-queue.json", "{not json");
            s.git_in("repo", &["add", "-A"]);
            s.git_in("repo", &["commit", "-q", "-m", "queue"]);
        }),
    );
}

#[test]
fn sync_cherry_pick() {
    check(sync(
        "sync_cherry_pick",
        Scenario::Clean,
        &["--strategy", "cherry-pick", "--no-pr"],
    ));
}

#[test]
fn sync_merge_conflict() {
    check(sync("sync_merge_conflict", Scenario::Conflict, &[]));
}

#[test]
fn sync_merge_conflict_no_issue() {
    check(sync(
        "sync_merge_conflict_no_issue",
        Scenario::Conflict,
        &["--no-conflict-issue"],
    ));
}

#[test]
fn sync_cherry_pick_conflict() {
    check(sync(
        "sync_cherry_pick_conflict",
        Scenario::Conflict,
        &["--strategy", "cherry-pick"],
    ));
}

#[test]
fn sync_test_cmd_fails() {
    check(
        sync("sync_test_cmd_fails", Scenario::Clean, &[])
            .env("FAKE_CARGO_FAIL", "test -p hermes-gateway"),
    );
}

#[test]
fn sync_fork_carry_fails() {
    check(
        sync("sync_fork_carry_fails", Scenario::Clean, &[])
            .env("FAKE_CARGO_FAIL", "check-fork-carry"),
    );
}

#[test]
fn sync_redteam_fails() {
    check(sync("sync_redteam_fails", Scenario::Clean, &[]).env("FAKE_CARGO_FAIL", "redteam"));
}

#[test]
fn sync_elite_ok() {
    check(sync(
        "sync_elite_ok",
        Scenario::Clean,
        &["--elite-gate", "--no-pr"],
    ));
}

#[test]
fn sync_elite_fails_with_rollback() {
    check(
        sync(
            "sync_elite_fails_with_rollback",
            Scenario::Clean,
            &[
                "--elite-gate",
                "--elite-rollback-cmd",
                "git reset --hard 'HEAD~1' && echo \"$x\" #~a,b",
            ],
        )
        .env("FAKE_CARGO_FAIL", "elite"),
    );
}

#[test]
fn sync_doc_refresh_commit() {
    check(sync("sync_doc_refresh_commit", Scenario::Clean, &["--no-pr"]).env("FAKE_DOC_EDIT", "1"));
}

#[test]
fn sync_doc_refresh_fails() {
    check(sync("sync_doc_refresh_fails", Scenario::Clean, &[]).env("FAKE_CARGO_FAIL", "docs"));
}

#[test]
fn sync_direct_main() {
    check(sync(
        "sync_direct_main",
        Scenario::Clean,
        &["--mode", "direct-main"],
    ));
}

#[test]
fn sync_skip_all() {
    check(sync(
        "sync_skip_all",
        Scenario::Clean,
        &[
            "--no-tests",
            "--no-redteam-gate",
            "--redteam-gate",
            "--no-redteam-gate",
            "--no-elite-gate",
            "--no-doc-refresh",
        ],
    ));
}

fn cron(name: &'static str, scenario: Scenario) -> Case {
    case(name, Tool::Cron, scenario, &[]).env("REPO_ROOT", "<ROOT>/repo")
}

#[test]
fn cron_missing_git() {
    check(cron("cron_missing_git", Scenario::NoUpdates).env("REPO_ROOT", "<ROOT>/nowhere"));
}

#[test]
fn cron_lock_busy() {
    check(cron("cron_lock_busy", Scenario::NoUpdates));
}

#[test]
fn cron_defaults_no_updates() {
    check(cron("cron_defaults_no_updates", Scenario::NoUpdates));
}

#[test]
fn cron_env_risk_blocked() {
    check(
        cron("cron_env_risk_blocked", Scenario::Risk)
            .env("SYNC_STRATEGY", "cherry-pick")
            .env("REPORT_DIR", "<ROOT>/reports")
            .env("CONFLICT_LABEL", "cron-label")
            .env("CREATE_CONFLICT_ISSUE", "0")
            .env("LOCK_FILE", "<ROOT>/home/locks/l.lock")
            .env(
                "RISK_PATHS_FILE",
                "<ROOT>/repo/scripts/upstream-risk-paths.txt",
            ),
    );
}

fn install(name: &'static str, args: &[&str]) -> Case {
    case(name, Tool::Install, Scenario::NoUpdates, args).env("REPO_ROOT", "<ROOT>/repo")
}

#[test]
fn install_default() {
    check(install("install_default", &[]));
}

#[test]
fn install_empty_schedule() {
    check(install("install_empty_schedule", &["", "ignored"]));
}

#[test]
fn install_replaces_marker_line() {
    check(
        install("install_replaces_marker_line", &["5 4 * * 1"])
            .env("SYNC_STRATEGY", "cherry-pick")
            .env("STRICT_RISK_GATE", "0")
            .env("LOG_FILE", "<ROOT>/home/custom/log.txt")
            .setup(|s| {
                s.write(
                    "home/crontab.txt",
                    "MAILTO=x\n1 2 * * * other job\n0 * * * * old # hermes-upstream-sync\n3 3 * * * last",
                )
            }),
    );
}

#[test]
fn install_missing_crontab() {
    check(
        install("install_missing_crontab", &[])
            .setup(|s| fs::remove_file(s.root.join("bin/crontab")).expect("rm crontab")),
    );
}

#[test]
fn install_missing_cron_script() {
    check(install("install_missing_cron_script", &[]).setup(|s| {
        fs::remove_file(s.root.join("repo/scripts/cron-upstream-sync.sh")).expect("rm")
    }));
}
