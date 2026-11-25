#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?} failed");
    };

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
