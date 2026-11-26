use super::*;
use crate::queue::Publisher;

fn options() -> SyncOptions {
    SyncOptions {
        strategy: "merge".to_string(),
        allow_risk_paths: false,
        conflict_label: "upstream-sync-conflict".to_string(),
        no_tests: false,
        no_pr: false,
        draft_pr: false,
        pr_labels: "upstream-sync,parity-sync".to_string(),
        run_redteam_gate: true,
        redteam_cmd: "cargo run --locked -q -p hermes-parity-tools -- run-redteam-gate".to_string(),
        run_elite_gate: false,
        elite_cmd: "python3 scripts/run-elite-sync-gate.py".to_string(),
        elite_rollback_cmd: String::new(),
        timeout: Duration::from_secs(1800),
    }
}

#[test]
fn sync_command_matches_the_scripts_defaults() {
    let cmd = sync_command(Path::new("/r"), Path::new("/r/.sync-reports"), &options());
    let script = Path::new("/r")
        .join("scripts")
        .join("sync-upstream.sh")
        .display()
        .to_string();
    assert_eq!(
        cmd,
        [
            "bash",
            &script,
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
            "--strict-risk-gate",
            "--redteam-gate",
            "--redteam-cmd",
            "cargo run --locked -q -p hermes-parity-tools -- run-redteam-gate",
            "--no-elite-gate",
            "--elite-cmd",
            "python3 scripts/run-elite-sync-gate.py",
            "--pr-labels",
            "upstream-sync,parity-sync",
        ]
    );
}

#[test]
fn sync_command_carries_every_switch() {
    let mut o = options();
    o.allow_risk_paths = true;
    o.no_tests = true;
    o.run_redteam_gate = false;
    o.redteam_cmd = "  ".to_string();
    o.run_elite_gate = true;
    o.elite_rollback_cmd = " undo ".to_string();
    o.no_pr = true;
    o.draft_pr = true;
    o.pr_labels = String::new();
    let cmd = sync_command(Path::new("/r"), Path::new("/d"), &o);
    assert_eq!(
        cmd[13..],
        [
            "--allow-risk-paths",
            "--no-tests",
            "--no-redteam-gate",
            "--elite-gate",
            "--elite-cmd",
            "python3 scripts/run-elite-sync-gate.py",
            "--elite-rollback-cmd",
            "undo",
            "--no-pr",
            "--draft-pr",
        ]
    );
}

#[test]
fn report_path_is_the_last_report_line_else_the_newest_file() {
    let dir = tempfile::tempdir().unwrap();
    let output = "x\n  [sync-upstream] Report: /a.txt\n[sync-upstream] Report:  /b.txt \n";
    assert_eq!(find_report(output, dir.path()), "/b.txt");
    assert_eq!(find_report("nothing", dir.path()), "");
    let old = dir.path().join("upstream-sync-1.txt");
    std::fs::write(&old, "").unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let new = dir.path().join("upstream-sync-2.txt");
    std::fs::write(&new, "").unwrap();
    std::fs::write(dir.path().join("other.txt"), "").unwrap();
    assert_eq!(
        find_report("nothing", dir.path()),
        new.display().to_string()
    );
}

#[test]
fn report_status_prefers_a_blocked_risk_gate() {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("r.txt");
    let status = |text: &str| {
        std::fs::write(&report, text).unwrap();
        report_status(&report.display().to_string())
    };
    assert_eq!(status("status: conflict\n"), "conflict");
    assert_eq!(
        status("status: success\nrisk_gate_status: blocked\n"),
        "risk_blocked"
    );
    assert_eq!(
        status("  status:  pr_opened  \nrisk_gate_status: pass"),
        "pr_opened"
    );
    assert_eq!(status("nothing"), "unknown");
    assert_eq!(report_status(""), "unknown");
    assert_eq!(
        report_status(&dir.path().join("missing").display().to_string()),
        "unknown"
    );
}

#[cfg(unix)]
mod with_bash {
    use super::*;

    fn event(delivery_id: &str) -> SyncEvent {
        SyncEvent {
            delivery_id: delivery_id.to_string(),
            event_name: "push".to_string(),
            repository: "NousResearch/hermes-agent".to_string(),
            git_ref: "refs/heads/main".to_string(),
            before_sha: String::new(),
            after_sha: "b".repeat(40),
            compare_url: String::new(),
            pushed_at: "2026-11-26T00:00:00+00:00".to_string(),
            payload: hermes_py::loads("{}").unwrap(),
        }
    }

    /// A repository whose `scripts/sync-upstream.sh` runs `body`.
    fn fixture(
        body: &str,
        max_attempts: i64,
        timeout: Duration,
    ) -> (tempfile::TempDir, WorkerConfig, SqliteQueue) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        std::fs::create_dir_all(repo.join(".sync-reports")).unwrap();
        std::fs::write(repo.join("scripts/sync-upstream.sh"), body).unwrap();
        let hook_log = dir.path().join("hook.log");
        let mut sync = options();
        sync.timeout = timeout;
        let config = WorkerConfig {
            repo_root: repo,
            poll_interval: Duration::from_millis(10),
            max_attempts,
            sync,
            upkeep: UpkeepConfig {
                enabled: false,
                labels: Vec::new(),
                parent_issue: 0,
                state_path: dir.path().join("state.json"),
                max_commits_per_event: 40,
                allow_python_test_surfaces: false,
            },
            assist_cmd: Some(format!(
                "echo \"$UPSTREAM_SYNC_OUTCOME $UPSTREAM_SYNC_DELIVERY_ID\" >> {}",
                hook_log.display()
            )),
        };
        let queue = SqliteQueue::open(&dir.path().join("q.db")).unwrap();
        (dir, config, queue)
    }

    fn row(dir: &Path) -> (String, Option<String>) {
        rusqlite::Connection::open(dir.join("q.db"))
            .unwrap()
            .query_row("SELECT status, last_error FROM queue_events", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    }

    fn claim_and_process(config: &WorkerConfig, queue: &SqliteQueue) {
        let item = queue.claim_next().unwrap().unwrap();
        process(config, queue, item.id, item.attempts, &item.event).unwrap();
    }

    #[test]
    fn success_marks_the_event_done() {
        let script = "r=\"$8/upstream-sync-1.txt\"; echo 'status: pr_opened' > \"$r\"; echo \"[sync-upstream] Report: $r\"";
        let (dir, config, queue) = fixture(script, 3, Duration::from_secs(60));
        queue.enqueue(&event("ok-1")).unwrap();
        claim_and_process(&config, &queue);
        assert_eq!(
            row(dir.path()),
            (
                "done".into(),
                Some("pr_opened; cli_drift=none; global_drift=none; upkeep_created=0".into())
            )
        );
        assert!(!dir.path().join("hook.log").exists());
    }

    #[test]
    fn a_blocked_risk_gate_is_terminal_and_calls_the_assist_hook() {
        let script = "printf 'status: x\\nrisk_gate_status: blocked\\n' > \"$8/upstream-sync-1.txt\"; exit 3";
        let (dir, config, queue) = fixture(script, 3, Duration::from_secs(60));
        queue.enqueue(&event("risk-1")).unwrap();
        claim_and_process(&config, &queue);
        assert_eq!(
            row(dir.path()),
            (
                "risk_blocked".into(),
                Some("risk_blocked; upkeep_created=0".into())
            )
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hook.log")).unwrap(),
            "risk_blocked risk-1\n"
        );
    }

    #[test]
    fn failures_retry_then_die_with_the_output_tail() {
        let (dir, config, queue) = fixture("echo boom >&2; exit 1", 2, Duration::from_secs(60));
        queue.enqueue(&event("fail-1")).unwrap();
        claim_and_process(&config, &queue);
        assert_eq!(row(dir.path()), ("pending".into(), Some("\nboom\n".into())));
        claim_and_process(&config, &queue);
        assert_eq!(row(dir.path()).0, "dead");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hook.log")).unwrap(),
            "dead fail-1\n"
        );
    }

    #[test]
    fn a_hung_child_is_killed_at_its_timeout() {
        let started = Instant::now();
        let mut command = Command::new("bash");
        command.args(["-c", "echo partial; sleep 30"]);
        let run = run_child(command, Duration::from_millis(300)).unwrap();
        assert!(run.timed_out && run.code.is_none());
        assert_eq!(run.stdout, "partial\n");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn an_unsafe_delivery_id_from_an_older_listener_is_not_run() {
        let (dir, config, queue) = fixture("touch \"$2/ran\"", 3, Duration::from_secs(60));
        queue.enqueue(&event("../escape")).unwrap();
        claim_and_process(&config, &queue);
        assert_eq!(
            row(dir.path()),
            ("dead".into(), Some("invalid delivery id".into()))
        );
        assert!(!config.repo_root.join("ran").exists());
    }
}
