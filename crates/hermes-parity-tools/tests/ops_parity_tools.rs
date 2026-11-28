#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_hermes-parity-tools")
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn path_with(prefix: &Path) -> std::ffi::OsString {
    let sys = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(
        std::iter::once(prefix.to_path_buf())
            .chain(std::env::split_paths(&sys).filter(|p| !p.as_os_str().is_empty())),
    )
    .unwrap()
}

fn run_cli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn git_fixture(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

#[test]
fn check_skills_index_validates_freshness_and_floors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Missing file returns 1 with error message on stderr.
    let missing = run_cli(root, &["check-skills-index", "--repo-root", "."]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("cannot read"));

    let api_dir = root.join("website/static/api");
    fs::create_dir_all(&api_dir).unwrap();
    let mut skills = Vec::new();
    for (src, n) in [
        ("skills.sh", 100),
        ("lobehub", 100),
        ("clawhub", 50),
        ("official", 50),
        ("github", 30),
        ("browse-sh", 50),
        ("extra", 1200),
    ] {
        for i in 0..n {
            skills.push(serde_json::json!({"name": format!("{src}-{i}"), "source": src}));
        }
    }

    let ok_payload = serde_json::json!({
        "generated_at": jiff::Timestamp::now().to_string(),
        "skills": skills,
    });
    fs::write(
        api_dir.join("skills-index.json"),
        serde_json::to_string(&ok_payload).unwrap(),
    )
    .unwrap();
    let pass = run_cli(root, &["check-skills-index", "--repo-root", "."]);
    assert_eq!(
        pass.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&pass.stderr)
    );
    assert!(String::from_utf8_lossy(&pass.stdout).contains("[skills-index] ok: 1580 skills"));

    let bad_payload = serde_json::json!({
        "generated_at": "2020-01-01T00:00:00Z",
        "skills": [{"source": "official"}],
    });
    fs::write(
        api_dir.join("skills-index.json"),
        serde_json::to_string(&bad_payload).unwrap(),
    )
    .unwrap();
    let fail = run_cli(root, &["check-skills-index", "--repo-root", "."]);
    assert_eq!(fail.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&fail.stderr);
    assert!(stderr.contains("total skills 1 < 1500"));
    assert!(stderr.contains("skills.sh: 0 < 100"));
}

#[test]
fn audit_background_queue_detects_and_repairs_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let jobs_dir = home.join("background_jobs");
    fs::create_dir_all(&jobs_dir).unwrap();

    fs::write(jobs_dir.join("bad.json"), "{not-json").unwrap();
    fs::write(
        jobs_dir.join("stale.json"),
        r#"{"id":"j1","status":"running","task":"alpha","started_at":"2020-01-01T00:00:00+00:00"}"#,
    )
    .unwrap();
    fs::write(
        jobs_dir.join("dup1.json"),
        r#"{"id":"d1","status":"queued","task":"same","updated_at":"2026-01-01T00:00:00+00:00"}"#,
    )
    .unwrap();
    fs::write(
        jobs_dir.join("dup2.json"),
        r#"{"id":"d2","status":"queued","task":"same","updated_at":"2026-01-02T00:00:00+00:00"}"#,
    )
    .unwrap();

    let report_path = tmp.path().join("audit.json");
    let first = run_cli(
        tmp.path(),
        &[
            "audit-background-queue",
            "--home",
            home.to_str().unwrap(),
            "--report",
            report_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(first.status.code(), Some(1));
    let parsed: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["totals"]["malformed"], 1);
    assert_eq!(parsed["totals"]["stale_running"], 1);
    assert_eq!(parsed["totals"]["duplicate_active"], 1);

    let repaired = run_cli(
        tmp.path(),
        &[
            "audit-background-queue",
            "--home",
            home.to_str().unwrap(),
            "--repair",
            "--report",
            report_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(repaired.status.code(), Some(1));
    let parsed2: Value = serde_json::from_slice(&repaired.stdout).unwrap();
    assert_eq!(parsed2["ok"], false);
    assert_eq!(parsed2["totals"]["repaired"], 2);
    assert!(jobs_dir.join("bad.json").exists());

    fs::remove_file(jobs_dir.join("bad.json")).unwrap();
    let clean = run_cli(
        tmp.path(),
        &[
            "audit-background-queue",
            "--home",
            home.to_str().unwrap(),
            "--report",
            report_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(clean.status.code(), Some(0));
    let parsed3: Value = serde_json::from_slice(&clean.stdout).unwrap();
    assert_eq!(parsed3["ok"], true);
    assert_eq!(parsed3["totals"]["stale_running"], 0);
    assert_eq!(parsed3["totals"]["duplicate_active"], 0);
}

#[test]
fn chaos_harness_and_compare_detect_regressions() {
    let tmp = tempfile::tempdir().unwrap();
    let base_out = tmp.path().join("base.json");
    let cur_out = tmp.path().join("cur.json");
    let cmp_out = tmp.path().join("cmp.json");

    let base_script = tmp.path().join("base.sh");
    let cur_script = tmp.path().join("cur.sh");
    write_executable(
        &base_script,
        r#"#!/bin/sh
printf 'adapter chaos harness results: [{"scenario":"s1","actual":{"attempts":1,"fallback_calls":0,"outcome":"success"}}]\n'
"#,
    );
    write_executable(
        &cur_script,
        r#"#!/bin/sh
printf 'adapter chaos harness results: [{"scenario":"s1","actual":{"attempts":2,"fallback_calls":1,"outcome":"error","error":"boom"}}]\n'
"#,
    );

    let run1 = run_cli(
        tmp.path(),
        &[
            "run-adapter-chaos-harness",
            "--repo-root",
            ".",
            "--command",
            base_script.to_str().unwrap(),
            "--output",
            base_out.to_str().unwrap(),
        ],
    );
    assert_eq!(run1.status.code(), Some(0));

    let run2 = run_cli(
        tmp.path(),
        &[
            "run-adapter-chaos-harness",
            "--repo-root",
            ".",
            "--command",
            cur_script.to_str().unwrap(),
            "--output",
            cur_out.to_str().unwrap(),
        ],
    );
    assert_eq!(run2.status.code(), Some(0));

    let cmp = run_cli(
        tmp.path(),
        &[
            "compare-adapter-chaos-reports",
            "--baseline",
            base_out.to_str().unwrap(),
            "--current",
            cur_out.to_str().unwrap(),
            "--output",
            cmp_out.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(cmp.status.code(), Some(1));
    let parsed: Value = serde_json::from_slice(&cmp.stdout).unwrap();
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["regressions"].as_array().unwrap().len(), 4);
}

#[test]
fn readme_sync_status_updates_and_checks_block() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join(".sync-reports")).unwrap();
    fs::create_dir_all(root.join("docs/parity")).unwrap();
    fs::write(
        root.join(".sync-reports/upstream-sync-20260101.txt"),
        "upstream_ref: upstream/main\nupstream_sha: abc123\npending_commits:\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "# Demo\n\n## Support\n\nText.\n").unwrap();

    let check_stale = run_cli(
        root,
        &["generate-readme-sync-status", "--repo-root", ".", "--check"],
    );
    assert_eq!(check_stale.status.code(), Some(1));

    let write_res = run_cli(root, &["generate-readme-sync-status", "--repo-root", "."]);
    assert_eq!(write_res.status.code(), Some(0));

    let check_fresh = run_cli(
        root,
        &["generate-readme-sync-status", "--repo-root", ".", "--check"],
    );
    assert_eq!(check_fresh.status.code(), Some(0));
    let readme = fs::read_to_string(root.join("README.md")).unwrap();
    assert!(readme.contains("<!-- BEGIN:ULTRA_SYNC_STATUS -->"));
    assert!(readme.contains("abc123"));
}

#[test]
fn readme_sync_status_uses_the_latest_report_and_preserves_surrounding_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let readme = root.join("README.md");
    let reports = root.join(".sync-reports");
    let command = ["generate-readme-sync-status", "--repo-root", "."];
    let original = "# Fixture\n\n## Upstream Sync and Parity Upkeep\n\nLocal notes.\n";
    fs::write(&readme, original).unwrap();
    fs::create_dir(&reports).unwrap();
    fs::write(reports.join("other.txt"), "not a sync report\n").unwrap();

    let missing = run_cli(root, &command);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no upstream-sync reports found"));
    assert_eq!(fs::read_to_string(&readme).unwrap(), original);

    let older = reports.join("upstream-sync-20260101.txt");
    let latest = reports.join("upstream-sync-20260102.txt");
    fs::write(&older, "upstream_sha: older-fixture\n").unwrap();
    fs::write(
        &latest,
        concat!(
            "timestamp_utc: 2026-01-02T03:04:05Z\r\nupstream_sha: latest-fixture\r\n",
            "## Pending Upstream Commits\r\n",
            "Ignore the heading text.\r\n```\r\nfirst\r\n\r\nsecond\r\n```\r\n",
            "Ignore the footer.\r\n"
        ),
    )
    .unwrap();
    for (report, modified) in [(older, 1_800_000_000), (latest, 1_700_000_000)] {
        fs::OpenOptions::new()
            .write(true)
            .open(report)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(modified))
            .unwrap();
    }
    let docs = root.join("docs/parity");
    fs::create_dir_all(&docs).unwrap();
    fs::write(
        docs.join("global-parity-proof.json"),
        r#"{"release_gate":{"pass":true},"ci_gate":{"pass":false}}"#,
    )
    .unwrap();

    let output = run_cli(root, &command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let contents = fs::read_to_string(&readme).unwrap();
    assert!(contents.starts_with("# Fixture\n\n## Upstream Sync and Parity Upkeep\n"));
    assert!(contents.ends_with("\nLocal notes.\n"));
    assert!(contents.contains("Source report: [`upstream-sync-20260102.txt`]"));
    assert!(contents.contains("Pending commits captured in report: `2`"));
    assert!(contents.contains("release `pass`, ci `fail`"));
    assert!(contents.contains("`latest-fixture`"));
    assert!(!contents.contains("older-fixture"));
}

#[test]
fn eval_trend_gate_and_hotpath_bench_work() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let missing_allow = run_cli(
        root,
        &[
            "run-eval-trend-gate",
            "--repo-root",
            ".",
            "--allow-missing-baseline",
            "--json",
        ],
    );
    assert_eq!(missing_allow.status.code(), Some(0));

    let base_eval = root.join("base.json");
    let cur_eval = root.join("cur.json");
    fs::write(
        &base_eval,
        r#"{"metrics":{"total":10,"pass_at_1":0.9,"total_duration":10.0,"total_cost_usd":1.0}}"#,
    )
    .unwrap();
    fs::write(
        &cur_eval,
        r#"{"metrics":{"total":10,"pass_at_1":0.5,"total_duration":10.0,"total_cost_usd":1.0}}"#,
    )
    .unwrap();
    let fail = run_cli(
        root,
        &[
            "run-eval-trend-gate",
            "--repo-root",
            ".",
            "--baseline",
            base_eval.to_str().unwrap(),
            "--current",
            cur_eval.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(fail.status.code(), Some(1));
    let parsed: Value = serde_json::from_slice(&fail.stdout).unwrap();
    assert_eq!(parsed["ok"], false);

    let hp_out = root.join("hp.json");
    let hp = run_cli(
        root,
        &[
            "run-zero-copy-hotpath-bench",
            "--repo-root",
            ".",
            "--command",
            "sh -c 'echo tool_policy_hot_path_ns_per_eval=4321'",
            "--output",
            hp_out.to_str().unwrap(),
        ],
    );
    assert_eq!(hp.status.code(), Some(0));
    let hp_json: Value = serde_json::from_str(&fs::read_to_string(&hp_out).unwrap()).unwrap();
    assert_eq!(hp_json["ns_per_eval"], 4321);
}

#[test]
fn eval_trend_gate_selects_recent_reports_and_enforces_duration_and_cost_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let evals = root.join("evals");
    fs::create_dir(&evals).unwrap();
    let baseline = evals.join("z-baseline.json");
    let current = evals.join("a-current.json");
    let report_path = root.join("trend.json");
    let args = ["run-eval-trend-gate", "--report-path", "trend.json"];

    fs::write(
        &baseline,
        r#"{"metrics":{"total":2,"pass_at_1":0.75,"total_duration":{"seconds":"2","nanoseconds":500000000},"total_cost_usd":0}}"#,
    )
    .unwrap();
    let missing = run_cli(root, &args);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stdout)
        .contains("[eval-trend-gate] FAILED (missing baseline/current report)"));
    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["reason"], "missing_eval_inputs");
    assert!(report["baseline_path"].is_null());
    assert_eq!(report["current_path"], baseline.to_str().unwrap());

    fs::write(
        &current,
        r#"{"metrics":{"total":2,"pass_at_1":0.75,"total_duration":" 5S ","total_cost_usd":0.25}}"#,
    )
    .unwrap();
    for (path, seconds) in [(&baseline, 1_700_000_000), (&current, 1_700_000_001)] {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
            .unwrap();
    }
    fs::create_dir(evals.join("a-directory.json")).unwrap();
    fs::write(evals.join("notes.txt"), "ignored").unwrap();

    let failed = run_cli(root, &args);
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stdout).contains(
        "[eval-trend-gate] FAILED (pass_drop=0.0000, duration_increase=1.000, cost_increase=1.000)"
    ));
    let report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert_eq!(report["baseline_path"], baseline.to_str().unwrap());
    assert_eq!(report["current_path"], current.to_str().unwrap());
    assert_eq!(report["baseline_metrics"]["mean_task_duration_secs"], 1.25);
    assert_eq!(report["current_metrics"]["mean_task_duration_secs"], 2.5);
    assert_eq!(
        report["checks"],
        serde_json::json!([
            {"name":"pass_at_1_drop","value":0.0,"limit":0.03,"ok":true},
            {"name":"mean_task_duration_increase","value":1.0,"limit":0.4,"ok":false},
            {"name":"total_cost_increase","value":1.0,"limit":0.5,"ok":false},
        ])
    );

    let allowed = run_cli(
        root,
        &[
            args.as_slice(),
            &[
                "--json",
                "--max-mean-task-duration-increase",
                "1",
                "--max-cost-increase",
                "1",
            ],
        ]
        .concat(),
    );
    assert_eq!(allowed.status.code(), Some(0));
    let mut printed: Value = serde_json::from_slice(&allowed.stdout).unwrap();
    assert_eq!(printed["ok"], true);
    let checks = printed["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 3);
    assert!(checks.iter().all(|check| check["ok"] == true));
    assert_eq!(
        printed.as_object_mut().unwrap().remove("report_path"),
        Some(Value::from(report_path.to_str().unwrap()))
    );
    let saved: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(saved, printed);
}

#[test]
fn redteam_gate_filters_severity_and_writes_sorted_report() {
    let tmp = tempfile::tempdir().unwrap();
    let suite = tmp.path().join("suite.json");
    let report = tmp.path().join("redteam.json");
    fs::write(
        &suite,
        serde_json::json!({
            "suite": "test-suite",
            "version": 2,
            "commands": [
                {"id": "pass-case", "severity": "high", "cmd": ["true"]},
                {"id": "low-fail", "severity": "low", "cmd": ["false"]}
            ]
        })
        .to_string(),
    )
    .unwrap();

    let strict = run_cli(
        tmp.path(),
        &[
            "run-redteam-gate",
            "--repo-root",
            ".",
            "--suite",
            suite.to_str().unwrap(),
            "--report-path",
            report.to_str().unwrap(),
            "--max-severity-allowed",
            "none",
        ],
    );
    assert_eq!(strict.status.code(), Some(1));

    let tolerated = run_cli(
        tmp.path(),
        &[
            "run-redteam-gate",
            "--repo-root",
            ".",
            "--suite",
            suite.to_str().unwrap(),
            "--report-path",
            report.to_str().unwrap(),
            "--max-severity-allowed",
            "low",
        ],
    );
    assert_eq!(tolerated.status.code(), Some(0));
    let body = fs::read_to_string(&report).unwrap();
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["summary"]["passed"], 1);
    assert_eq!(parsed["summary"]["failed_tolerated"], 1);
}

#[test]
fn differential_parity_gate_detects_rust_cli_drift() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let git = |args: &[&str]| git_fixture(root, args);

    git(&["init", "-b", "upstream"]);
    fs::create_dir_all(root.join("crates/hermes-cli/src")).unwrap();
    fs::write(
        root.join("crates/hermes-cli/src/cli.rs"),
        "pub enum Commands {\n    Chat,\n    Status,\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("crates/hermes-cli/src/main.rs"),
        "fn run_tools() {}\nfn run_gateway() {}\nfn run_auth() {}\nfn run_cron() {}\nfn run_webhook() {}\nfn run_profile() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("crates/hermes-cli/src/commands.rs"),
        "pub const SLASH_COMMANDS: &[(&str, &str)] = &[(\"/help\", \"help\")];\nfn handle_cli_memory() {}\nfn handle_cli_mcp() {}\nfn handle_cli_skills() {}\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "upstream baseline"]);

    git(&["checkout", "-b", "local"]);
    fs::write(
        root.join("crates/hermes-cli/src/cli.rs"),
        "pub enum Commands {\n    Chat,\n}\n",
    )
    .unwrap();
    git(&["commit", "-am", "drop status"]);

    let report = root.join("diff.json");
    let out = run_cli(
        root,
        &[
            "run-differential-parity-gate",
            "--repo-root",
            ".",
            "--upstream-ref",
            "upstream",
            "--local-ref",
            "local",
            "--report-path",
            report.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let parsed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["summary"]["missing_top_level"], 1);
    assert_eq!(
        parsed["drift"]["top_level"]["missing_in_local"],
        serde_json::json!(["status"])
    );
}

#[test]
fn differential_parity_gate_compares_legacy_slash_commands_and_reports_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let git = |args: &[&str]| git_fixture(root, args);
    git(&["init", "-b", "notes"]);
    git(&["commit", "--allow-empty", "-m", "Empty surface"]);

    git(&["checkout", "-b", "legacy"]);
    fs::create_dir(root.join("hermes_cli")).unwrap();
    fs::write(
        root.join("cli.py"),
        "from hermes_cli.commands import commands\n",
    )
    .unwrap();
    fs::write(
        root.join("hermes_cli/commands.py"),
        r#"commands = [
    CommandDef("help",
        aliases=("assist",),
    ),
]
"#,
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "Legacy slash commands"]);

    git(&["checkout", "-b", "rust", "notes"]);
    let cli_src = root.join("crates/hermes-cli/src");
    fs::create_dir_all(cli_src.join("commands")).unwrap();
    fs::write(
        cli_src.join("cli.rs"),
        "enum Commands {\n    ReadOnly,\n}\n",
    )
    .unwrap();
    fs::write(cli_src.join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(
        cli_src.join("commands/interactive.rs"),
        r#"const COMMANDS: &[Command] = &[Command { name: "/help" }, Command { name: "/review" }];"#,
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "Split Rust command registry"]);

    let run = |upstream: &str| {
        run_cli(
            root,
            &[
                "run-differential-parity-gate",
                "--local-ref",
                "rust",
                "--upstream-ref",
                upstream,
                "--report-path",
                "diff.json",
                "--fallback-cmd",
                "touch .fallback-used; printf 'fixture stdout'; printf 'fixture stderr' >&2; exit 7",
            ],
        )
    };
    let saved =
        || serde_json::from_slice::<Value>(&fs::read(root.join("diff.json")).unwrap()).unwrap();

    let compared = run("legacy");
    assert_eq!(compared.status.code(), Some(1), "{compared:?}");
    let report = saved();
    assert_eq!(report["ok"], false);
    assert_eq!(report["drift"]["surface_kind"], "rust_vs_python");
    assert_eq!(
        report["drift"]["slash_commands"],
        serde_json::json!({"missing_in_local": ["assist"], "extra_in_local": ["review"]})
    );
    assert_eq!(
        report["local_surface"]["top_level"],
        serde_json::json!(["read-only"])
    );
    assert_eq!(
        report["upstream_surface"]["slash_commands"],
        serde_json::json!(["assist", "help"])
    );
    assert_eq!(report["commits"]["behind"], 1);
    assert_eq!(report["commits"]["behind_gate_enforced"], false);
    assert!(!root.join(".fallback-used").exists());

    let missing = run("missing");
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stdout).contains("FAILED (missing ref)"));
    let report = saved();
    assert_eq!(report["reason"], "missing_git_ref");
    assert_eq!(report["local_ref_exists"], true);
    assert_eq!(report["upstream_ref_exists"], false);
    assert!(!root.join(".fallback-used").exists());

    let fallback = run("notes");
    assert_eq!(fallback.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&fallback.stdout).contains("FAILED (fallback)"));
    let report = saved();
    assert_eq!(report["reason"], "cli_surface_unavailable_fallback");
    assert_eq!(report["fallback"]["ok"], false);
    assert_eq!(report["fallback"]["exit_code"], 7);
    assert_eq!(report["fallback"]["stdout_tail"], "fixture stdout");
    assert_eq!(report["fallback"]["stderr_tail"], "fixture stderr");
    assert!(root.join(".fallback-used").is_file());
}

#[test]
fn performance_autopilot_reports_degraded_profiles_and_actions() {
    struct Case {
        profile: &'static str,
        hotpath: &'static str,
        eval: &'static str,
        mcp: &'static str,
        telemetry: &'static str,
        recommendations: &'static [&'static str],
        actions: &'static [&'static str],
        indexes: [f64; 3],
    }

    let healthy = r#"printf '%s' '{"health":{"ok":true},"context_pack":{"retrieval":{"source_counts":{"fixture":1}}}}'"#;
    let cases = [
        Case {
            profile: "safety",
            hotpath: "printf 'tool_policy_hot_path_ns_per_eval=unavailable'; exit 3",
            eval: "exit 4",
            mcp: "exit 5",
            telemetry: "printf 'no telemetry'; exit 6",
            recommendations: &[
                "HOTPATH_FAIL",
                "EVAL_TREND_FAIL",
                "MCP_STALE_RECOVERY_FAIL",
                "CONTEXTLATTICE_PREFLIGHT_FAIL",
                "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE",
            ],
            actions: &["HERMES_TOOL_POLICY_MODE=enforce", "HERMES_REPLAY_ENABLED=1"],
            indexes: [20.0, 0.0, 11.0],
        },
        Case {
            profile: "throughput",
            hotpath: "printf 'tool_policy_hot_path_ns_per_eval=24000'",
            eval: "true",
            mcp: "true",
            telemetry: r#"printf '%s\n' 'fixture preflight' '{"health":{"ok":false},"warnings":["fixture warning"],"context_pack":{"retrieval":{"source_counts":[],"fallback_counts":{"python_hot_path_total":4}}},"status":{"queue":{"pendingTotal":12}}}' 'fixture end'"#,
            recommendations: &[
                "HOTPATH_SLOW",
                "CONTEXTLATTICE_UNHEALTHY",
                "CONTEXTLATTICE_PYTHON_FALLBACK",
                "CONTEXTLATTICE_QUEUE_PRESSURE",
            ],
            actions: &[
                "HERMES_TOOL_POLICY_PRESET=standard",
                "HERMES_MODEL_CATALOG_GUARD=1",
            ],
            indexes: [90.0, 46.0, 70.2],
        },
        Case {
            profile: "quality",
            hotpath: "true",
            eval: "false",
            mcp: "true",
            telemetry: healthy,
            recommendations: &["EVAL_TREND_FAIL"],
            actions: &["HERMES_REPLAY_ENABLED=1", "HERMES_MODEL_AUTO_REMEDIATE=1"],
            indexes: [100.0, 60.0, 82.0],
        },
        Case {
            profile: "reliability",
            hotpath: "true",
            eval: "true",
            mcp: "false",
            telemetry: healthy,
            recommendations: &["MCP_STALE_RECOVERY_FAIL"],
            actions: &["HERMES_TOOL_POLICY_MODE=enforce"],
            indexes: [80.0, 88.0, 83.6],
        },
    ];

    for case in cases {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let args = [
            "run-performance-autopilot",
            "--hotpath-cmd",
            case.hotpath,
            "--eval-cmd",
            case.eval,
            "--mcp-cmd",
            case.mcp,
            "--contextlattice-cmd",
            case.telemetry,
            "--apply-env",
            "recommendations.txt",
        ];
        let strict = run_cli(root, &[args.as_slice(), &["--strict", "--json"]].concat());
        let ok = case.profile == "throughput";
        assert_eq!(strict.status.code(), Some(i32::from(!ok)), "{strict:?}");
        let report: Value = serde_json::from_slice(&strict.stdout).unwrap();
        assert_eq!(report["ok"], ok);
        assert_eq!(report["profile_recommendation"], case.profile);
        let recommendations: Vec<_> = report["recommendations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap())
            .collect();
        assert_eq!(recommendations, case.recommendations);
        for (key, value) in ["performance_index", "intelligence_index", "adaptive_index"]
            .into_iter()
            .zip(case.indexes)
        {
            assert_eq!(report[key], value, "{}: {key}", case.profile);
        }
        let applied = fs::read_to_string(root.join("recommendations.txt")).unwrap();
        assert!(applied.lines().any(|line| {
            line.strip_prefix("HERMES_PERF_AUTOPILOT_PROFILE=") == Some(case.profile)
        }));
        for action in case.actions {
            assert!(applied.lines().any(|line| line == *action), "{applied}");
        }
        let markdown = fs::read_to_string(report["report_markdown"].as_str().unwrap()).unwrap();
        assert!(markdown.contains(&format!("- profile_recommendation: `{}`", case.profile)));
        if case.profile == "safety" {
            let advisory = run_cli(root, &args);
            assert_eq!(advisory.status.code(), Some(0));
            assert!(String::from_utf8_lossy(&advisory.stdout)
                .contains("[performance-autopilot] FAILED"));
            assert!(markdown.contains("- `eval_trend`: FAIL (exit=4)"));
        }
    }
}

#[test]
fn self_evolution_loop_combines_gate_reports_and_honours_skip_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::write(
        root.join("golden.json"),
        r#"{"ok":false,"missing_commands":["inspect"],"unrelated":"omit from excerpt"}"#,
    )
    .unwrap();
    fs::write(
        root.join("eval.json"),
        r#"{"ok":true,"summary":{"passed":2}}"#,
    )
    .unwrap();
    fs::write(
        root.join("elite.json"),
        r#"{"ok":"unknown","sections":{"some_gate":{"ok":false}}}"#,
    )
    .unwrap();

    let failed = run_cli(
        root,
        &[
            "run-self-evolution-loop",
            "--objective",
            "  Fixture objective  ",
            "--golden-cmd",
            "printf '[fixture] Report: golden.json\\n'; exit 3",
            "--eval-cmd",
            r#"printf '{"report_path":"eval.json"}'"#,
            "--elite-cmd",
            "printf 'report=elite.json' >&2; exit 7",
            "--report-path",
            "loop.json",
            "--json",
        ],
    );
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    let report: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["objective"], "Fixture objective");
    assert_eq!(
        report["summary"],
        serde_json::json!({
            "total_sections":3,"passed_sections":1,"failed_sections":2,"intelligence_index":33.33
        })
    );
    let sections = &report["sections"];
    assert_eq!(sections["golden_parity"]["exit_code"], 3);
    assert_eq!(sections["golden_parity"]["report_ok"], false);
    assert_eq!(
        sections["golden_parity"]["report_excerpt"],
        serde_json::json!({"missing_commands":["inspect"]})
    );
    assert_eq!(sections["eval_trend"]["report_ok"], true);
    assert_eq!(
        sections["eval_trend"]["report_excerpt"]["summary"]["passed"],
        2
    );
    assert_eq!(sections["elite_sync"]["exit_code"], 7);
    assert!(sections["elite_sync"]["report_ok"].is_null());
    assert_eq!(
        sections["elite_sync"]["report_excerpt"]["sections"]["some_gate"]["ok"],
        false
    );
    let recommendations = report["recommendations"].as_array().unwrap();
    assert_eq!(recommendations.len(), 2);
    assert_eq!(recommendations[0]["id"], "PARITY_DRIFT");
    assert_eq!(recommendations[1]["id"], "ELITE_GATE_FAIL");
    assert_eq!(
        recommendations[1]["command"],
        "cargo run --locked -q -p hermes-parity-tools -- run-elite-sync-gate --json"
    );
    assert!(recommendations[0]["reason"]
        .as_str()
        .unwrap()
        .ends_with("Objective: Fixture objective."));

    let forbidden = "touch unexpectedly-ran";
    let skipped = run_cli(
        root,
        &[
            "run-self-evolution-loop",
            "--golden-cmd",
            forbidden,
            "--eval-cmd",
            forbidden,
            "--elite-cmd",
            forbidden,
            "--skip-golden",
            "--skip-eval",
            "--skip-elite",
            "--report-path",
            "skipped.json",
        ],
    );
    assert_eq!(skipped.status.code(), Some(0), "{skipped:?}");
    assert!(String::from_utf8_lossy(&skipped.stdout).contains("PASSED (passed=0/0"));
    assert!(!root.join("unexpectedly-ran").exists());
    let saved: Value =
        serde_json::from_slice(&fs::read(root.join("skipped.json")).unwrap()).unwrap();
    assert_eq!(saved["sections"], serde_json::json!({}));
    assert_eq!(saved["summary"]["intelligence_index"], 100.0);
    assert_eq!(saved["recommendations"][0]["id"], "PROMOTE_BASELINE");
}

#[test]
fn self_evolution_loop_default_elite_cmd_runs_parity_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    let mock_bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&mock_bin).unwrap();
    let args_log = root.join("cargo-args.txt");
    let profile = format!("export PATH='{}':\"$PATH\"\n", mock_bin.display());
    fs::write(home.join(".bash_profile"), &profile).unwrap();
    fs::write(home.join(".profile"), &profile).unwrap();
    fs::write(
        root.join("elite.json"),
        r#"{"ok":true,"summary":{"passed_sections":8,"total_sections":8}}"#,
    )
    .unwrap();
    write_executable(
        &mock_bin.join("cargo"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nprintf '[elite-sync-gate] Report: elite.json\\n'\n",
            args_log.display()
        ),
    );

    let out = Command::new(bin())
        .args([
            "run-self-evolution-loop",
            "--skip-golden",
            "--skip-eval",
            "--report-path",
            "loop.json",
            "--json",
        ])
        .current_dir(root)
        .env("HOME", &home)
        .env("PATH", path_with(&mock_bin))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        fs::read_to_string(&args_log).unwrap().trim(),
        "run --locked -q -p hermes-parity-tools -- run-elite-sync-gate --json"
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(
        report["sections"]["elite_sync"]["command"],
        "cargo run --locked -q -p hermes-parity-tools -- run-elite-sync-gate --json"
    );
    assert_eq!(report["sections"]["elite_sync"]["report_ok"], true);
}

#[test]
fn performance_autopilot_and_self_evolution_loop_compute_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let out_json = root.join("perf.json");
    let out_md = root.join("perf.md");
    let out_env = root.join("perf.env");

    let perf = run_cli(
        root,
        &[
            "run-performance-autopilot",
            "--repo-root",
            ".",
            "--hotpath-cmd",
            "echo tool_policy_hot_path_ns_per_eval=9000",
            "--eval-cmd",
            "true",
            "--mcp-cmd",
            "true",
            "--contextlattice-cmd",
            r#"echo '{"health":{"ok":true},"warnings":[],"context_pack":{"retrieval":{"source_counts":{"kb":1},"fallback_counts":{"python_hot_path_total":0}}},"status":{"queue":{"pendingTotal":1}}}'"#,
            "--output-json",
            out_json.to_str().unwrap(),
            "--output-md",
            out_md.to_str().unwrap(),
            "--apply-env",
            out_env.to_str().unwrap(),
            "--strict",
            "--json",
        ],
    );
    assert_eq!(perf.status.code(), Some(0));
    let perf_val: Value = serde_json::from_slice(&perf.stdout).unwrap();
    assert_eq!(perf_val["ok"], true);
    assert_eq!(perf_val["profile_recommendation"], "balanced");
    assert!(out_md.exists());
    assert!(fs::read_to_string(&out_env)
        .unwrap()
        .contains("HERMES_PERF_AUTOPILOT_STATUS=stable"));

    let loop_rep = root.join("loop.json");
    let evo = run_cli(
        root,
        &[
            "run-self-evolution-loop",
            "--repo-root",
            ".",
            "--objective",
            "smoke",
            "--skip-golden",
            "--skip-elite",
            "--eval-cmd",
            "false",
            "--report-path",
            loop_rep.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(evo.status.code(), Some(1));
    let evo_val: Value = serde_json::from_slice(&evo.stdout).unwrap();
    assert_eq!(evo_val["ok"], false);
    assert_eq!(evo_val["recommendations"][0]["id"], "EVAL_REGRESSION");
}

#[test]
fn behavioral_parity_suite_checks_tokens_without_trailing_newline() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("crates/hermes-cli/src/commands")).unwrap();
    fs::write(
        root.join("crates/hermes-cli/src/commands/mod.rs"),
        r#""/plan caps" "/autocompact governance" "/model failover" "/ops cockpit" "/skills quality" "/raw trace focus" "/raw trace graph""#,
    )
    .unwrap();
    fs::write(
        root.join("crates/hermes-cli/src/tui.rs"),
        "ActivityLaneMode::Cockpit Ctrl+O cockpit",
    )
    .unwrap();

    let report = root.join("beh.json");
    let pass = run_cli(
        root,
        &[
            "run-behavioral-parity-suite",
            "--repo-root",
            ".",
            "--report",
            report.to_str().unwrap(),
        ],
    );
    assert_eq!(pass.status.code(), Some(0));
    let bytes = fs::read_to_string(&report).unwrap();
    assert!(!bytes.ends_with('\n'));
    let parsed: Value = serde_json::from_str(&bytes).unwrap();
    assert_eq!(parsed["ok"], true);
}

#[test]
fn cargo_surface_and_rustsec_gates_use_toolchain_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mock_bin: PathBuf = root.join("mock_bin");
    fs::create_dir_all(&mock_bin).unwrap();
    fs::create_dir_all(root.join("crates/hermes-cli")).unwrap();
    fs::create_dir_all(root.join(".ci")).unwrap();

    fs::write(
        root.join("crates/hermes-cli/Cargo.toml"),
        "[[bin]]\nname = \"hermes\"\npath = \"src/main.rs\"\n[dependencies]\nhermes-core = {}\n[features]\ngateway-adapters-all = [\n  \"gateway-a\",\n]\n",
    )
    .unwrap();
    fs::write(
        root.join(".ci/clippy-allowlist.txt"),
        "demo|clippy::needless|src/lib.rs\n",
    )
    .unwrap();

    write_executable(
        &mock_bin.join("cargo"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  clippy)\n    printf '%s\\n' '{{\"reason\":\"compiler-message\",\"target\":{{\"name\":\"demo\"}},\"message\":{{\"level\":\"warning\",\"code\":{{\"code\":\"clippy::needless\"}},\"spans\":[{{\"file_name\":\"src/lib.rs\",\"is_primary\":true}}],\"message\":\"msg\"}}}}'\n    ;;\n  metadata)\n    printf '{{\"target_directory\":\"{}/target\"}}\\n'\n    ;;\n  tree)\n    printf 'demo v0.1.0\\n'\n    ;;\nesac\n",
            root.display()
        ),
    );

    let env_path = path_with(&mock_bin);

    let clippy_out = Command::new(bin())
        .args(["clippy-warning-gate", "--repo-root", ".", "--check"])
        .current_dir(root)
        .env("PATH", &env_path)
        .output()
        .unwrap();
    assert_eq!(clippy_out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&clippy_out.stdout).contains("[clippy-gate] pass"));

    let audit_out = Command::new(bin())
        .args(["audit-cargo-build-surface", "--repo-root", "."])
        .current_dir(root)
        .env("PATH", &env_path)
        .output()
        .unwrap();
    assert_eq!(audit_out.status.code(), Some(0));
    let audit_stdout = String::from_utf8_lossy(&audit_out.stdout);
    assert!(audit_stdout.contains("# Hermes Cargo Build Surface Audit"));
    assert!(audit_stdout.contains("- `hermes` -> `src/main.rs`"));
    assert!(audit_stdout.contains("- Default `gateway-adapters-all` feature count: `1`"));

    let list_out = run_cli(root, &["refresh-rustsec-patches", "list"]);
    assert_eq!(list_out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&list_out.stdout), "tungstenite\n");

    let bad_out = run_cli(root, &["refresh-rustsec-patches", "check", "nonexistent"]);
    assert_eq!(bad_out.status.code(), Some(2));
}

#[test]
fn rustsec_refresh_prunes_the_local_archive_and_check_reports_drift() {
    let sandbox = tempfile::tempdir().expect("temporary repository");
    let root = sandbox.path();
    let patches = root.join("third_party/rustsec-patches");
    let configured = patches.join("tungstenite-0.29.0");
    for dir in ["patches", "tungstenite-0.29.0"] {
        fs::create_dir_all(patches.join(dir)).expect("create patch directory");
    }
    fs::write(configured.join("stale.txt"), "previous vendored contents").unwrap();
    fs::write(
        patches.join("patches/tungstenite-0.29.0-use-getrandom.patch"),
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub const FIXTURE: u8 = 1;\n+pub const FIXTURE: u8 = 2;\n",
    )
    .unwrap();
    let archives = root.join("archives");
    let scratch = root.join("scratch");
    let mock_bin = root.join("mock-bin");
    for dir in [&archives, &scratch, &mock_bin] {
        fs::create_dir(dir).unwrap();
    }
    for version in ["0.29.0", "0.30.0"] {
        let name = format!("tungstenite-{version}");
        let package = archives.join(&name);
        fs::create_dir_all(package.join("src/snapshots")).unwrap();
        fs::create_dir(package.join("examples")).unwrap();
        for (path, body) in [
            ("src/lib.rs", "pub const FIXTURE: u8 = 1;\n"),
            ("src/snapshots/demo.snap", "snapshot"),
            ("examples/demo.rs", "fn main() {}\n"),
            ("Cargo.lock", ""),
            ("Cargo.toml", "[package]\nname = \"tungstenite\"\n"),
        ] {
            fs::write(package.join(path), body).unwrap();
        }
        let archive = Command::new("tar")
            .args(["-czf", &format!("{version}.crate"), &name])
            .current_dir(&archives)
            .output()
            .expect("create local crate archive");
        assert!(archive.status.success(), "{archive:?}");
    }
    let downloads = root.join("downloads");
    write_executable(
        &mock_bin.join("curl"),
        r#"#!/bin/sh
set -eu
test "$#" -eq 8
test "$7" = "-o"
case "$6" in
  https://crates.io/api/v1/crates/tungstenite/0.29.0/download) version=0.29.0 ;;
  https://crates.io/api/v1/crates/tungstenite/0.30.0/download) version=0.30.0 ;;
  *) exit 2 ;;
esac
printf '%s\n' "$version" >> "$FIXTURE_DOWNLOADS"
cp "$FIXTURE_ARCHIVES/$version.crate" "$8"
"#,
    );
    let fixture_path = path_with(&mock_bin);
    let run = |args: &[&str]| {
        Command::new(bin())
            .args(["refresh-rustsec-patches", "--repo-root", "."])
            .args(args)
            .current_dir(root)
            .env("PATH", &fixture_path)
            .env("TMPDIR", &scratch)
            .env("FIXTURE_ARCHIVES", &archives)
            .env("FIXTURE_DOWNLOADS", &downloads)
            .output()
            .expect("run patch refresh CLI")
    };

    let refresh = run(&["refresh", "tungstenite"]);
    assert!(refresh.status.success(), "{refresh:?}");
    assert!(String::from_utf8_lossy(&refresh.stdout).contains("refreshed tungstenite 0.29.0"));
    assert_eq!(
        fs::read_to_string(configured.join("src/lib.rs")).unwrap(),
        "pub const FIXTURE: u8 = 2;\n"
    );
    assert!(configured.join("Cargo.toml").is_file());
    for path in ["Cargo.lock", "examples", "src/snapshots", "stale.txt"] {
        assert!(!configured.join(path).exists(), "{path}");
    }
    let check = run(&["check", "all"]);
    assert!(check.status.success(), "{check:?}");
    assert_eq!(check.stdout, b"ok tungstenite 0.29.0\n");

    let drifted = configured.join("src/lib.rs");
    fs::write(&drifted, "local drift\n").unwrap();
    let check = run(&["check", "tungstenite"]);
    assert!(!check.status.success(), "{check:?}");
    assert!(String::from_utf8_lossy(&check.stderr).contains("diff mismatch in src/lib.rs"));
    assert_eq!(fs::read_to_string(&drifted).unwrap(), "local drift\n");

    let refresh = run(&["refresh", "tungstenite", "0.30.0"]);
    assert!(refresh.status.success(), "{refresh:?}");
    assert!(String::from_utf8_lossy(&refresh.stdout)
        .contains("from tungstenite-0.29.0 to tungstenite-0.30.0"));
    assert_eq!(
        fs::read_to_string(patches.join("tungstenite-0.30.0/src/lib.rs")).unwrap(),
        "pub const FIXTURE: u8 = 2;\n"
    );
    assert_eq!(fs::read_to_string(&drifted).unwrap(), "local drift\n");
    assert_eq!(
        fs::read_to_string(downloads).unwrap(),
        "0.29.0\n0.29.0\n0.29.0\n0.30.0\n"
    );
    assert!(fs::read_dir(scratch).unwrap().next().is_none());
}

#[test]
fn cargo_surface_gate_checks_registrations_and_stops_after_a_failed_replay() {
    let sandbox = tempfile::tempdir().expect("temporary fixture workspace");
    let root = sandbox.path().join("repo");
    let mock_bin = sandbox.path().join("mock-bin");
    let commands = root.join("crates/hermes-cli/src/commands");
    fs::create_dir_all(&commands).unwrap();
    fs::create_dir(&mock_bin).unwrap();
    let calls = sandbox.path().join("cargo-calls");
    write_executable(
        &mock_bin.join("cargo"),
        r#"#!/bin/sh
set -eu
test "$(pwd -P)" = "$FIXTURE_REPOSITORY"
printf '%s\n' "$*" >> "$FIXTURE_CARGO_CALLS"
if [ "${5:-}" = "$FIXTURE_FAILED_TEST" ]; then
  exit 7
fi
"#,
    );
    let fixture_path = path_with(&mock_bin);
    let repository = root.canonicalize().unwrap();
    let run = |subcommand: &str, fail_test: &str| {
        Command::new(bin())
            .args([subcommand, "--repo-root"])
            .arg(&repository)
            .current_dir(sandbox.path())
            .env("PATH", &fixture_path)
            .env("FIXTURE_REPOSITORY", &repository)
            .env("FIXTURE_CARGO_CALLS", &calls)
            .env("FIXTURE_FAILED_TEST", fail_test)
            .output()
            .expect("run surface gate")
    };
    let registrations = [
        "/commands",
        "/boot",
        "/walkthrough",
        "/triage",
        "/subconscious",
        "/integrations",
    ];
    let tests = [
        "test_p0_p1_surface_commands_registered_and_completable",
        "p0_walkthrough_and_integrations_commands_emit_expected_sections",
        "p0_compress_rules_set_and_apply_updates_runtime_env",
        "p1_trigger_triage_escalates_high_severity_events",
        "test_format_tool_message_lines_truncates_large_payload",
    ];
    let write_registry = |omitted| {
        let entries: String = registrations
            .iter()
            .copied()
            .filter(|name| Some(*name) != omitted)
            .map(|name| format!("\"{name}\","))
            .collect();
        fs::write(
            commands.join("registry.rs"),
            format!("const COMMANDS: &[&str] = &[{entries}];\n"),
        )
        .unwrap();
    };
    let write_tests = |omitted| {
        let definitions: String = tests
            .iter()
            .copied()
            .filter(|name| Some(*name) != omitted)
            .map(|name| format!("#[test]\nfn {name}() {{}}\n"))
            .collect();
        fs::write(root.join("crates/hermes-cli/src/tui.rs"), definitions).unwrap();
    };
    let gate = "run-openhuman-p0-p1-gate";
    write_tests(None);
    for registration in registrations {
        write_registry(Some(registration));
        let missing = run(gate, "none");
        assert_eq!(missing.status.code(), Some(1), "{missing:?}");
        assert!(
            String::from_utf8_lossy(&missing.stderr).contains(&format!(
                "missing slash command registration: {registration}"
            )),
            "{missing:?}"
        );
        assert!(!calls.exists());
    }
    write_registry(None);
    for required_test in tests {
        write_tests(Some(required_test));
        let missing = run(gate, "none");
        assert_eq!(missing.status.code(), Some(1), "{missing:?}");
        assert!(
            String::from_utf8_lossy(&missing.stderr)
                .contains(&format!("missing required test: {required_test}")),
            "{missing:?}"
        );
        assert!(!calls.exists());
    }

    write_tests(None);
    let expected: Vec<_> = tests
        .iter()
        .map(|name| format!("test -p hermes-cli --lib {name}"))
        .collect();
    let failure = run(gate, tests[2]);
    assert_eq!(failure.status.code(), Some(7), "{failure:?}");
    assert_eq!(
        fs::read_to_string(&calls)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        expected[..3]
    );
    fs::write(&calls, "").unwrap();
    let success = run(gate, "none");
    assert!(success.status.success(), "{success:?}");
    assert_eq!(
        fs::read_to_string(&calls)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        expected
    );

    fs::write(&calls, "").unwrap();
    let replay = run("run-deterministic-replay-suite", "e2e_replay_suite");
    assert_eq!(replay.status.code(), Some(7), "{replay:?}");
    assert_eq!(
        fs::read_to_string(calls).unwrap(),
        "test -p hermes-cli --test e2e_replay_suite -- --nocapture\n"
    );
}

#[test]
fn ultra_feature_gate_uses_one_target_directory_and_stops_on_failure() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().join("repo");
    let mock_bin = sandbox.path().join("bin");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&mock_bin).unwrap();
    write_executable(
        &mock_bin.join("cargo"),
        r#"#!/bin/sh
set -eu
printf '%s|%s\n' "$CARGO_TARGET_DIR" "$*" >> "$FIXTURE_CALLS"
if [ -n "$FIXTURE_FAILURE" ] && [ "${4:-}" = "$FIXTURE_FAILURE" ]; then
    exit 7
fi
"#,
    );

    let target = sandbox.path().join("custom-target");
    let steps = [
        "test -p hermes-tools tools::ultra_features --lib",
        "test -p hermes-tools register_builtins::tests::builtin_registry_registers_core_tool_surfaces_for_parity --lib",
        "fmt --all --check",
    ];
    let named_test =
        "register_builtins::tests::builtin_registry_registers_core_tool_surfaces_for_parity";
    for (mode, custom_target, failed, expected_exit, steps_run) in [
        ("default", false, "", 0, 3),
        ("override", true, named_test, 7, 2),
    ] {
        let calls = sandbox.path().join(mode);
        let mut command = Command::new(bin());
        command
            .args(["run-ultra-feature-benchmarks", "--repo-root"])
            .arg(&root)
            .env_clear()
            .env("PATH", &mock_bin)
            .env("HOME", sandbox.path())
            .env("FIXTURE_CALLS", &calls)
            .env("FIXTURE_FAILURE", failed)
            .current_dir(sandbox.path());
        if custom_target {
            command.env("CARGO_TARGET_DIR", &target);
        }
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env(
                "LLVM_PROFILE_FILE",
                std::env::current_dir().unwrap().join(profile),
            );
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(expected_exit), "{output:?}");
        let expected_target = if custom_target {
            target.to_str().unwrap()
        } else {
            "/Volumes/wd_black/hermes-agent-ultra/target"
        };
        let log = fs::read_to_string(calls).unwrap();
        let expected: Vec<_> = steps[..steps_run]
            .iter()
            .map(|step| format!("{expected_target}|{step}"))
            .collect();
        assert_eq!(log.lines().collect::<Vec<_>>(), expected);
    }
}
