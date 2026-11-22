use super::*;
use std::fs;

fn sandbox_home(test_name: &str, mode: &str) -> Option<PathBuf> {
    const MARKER: &str = "HERMES_TRIAGE_COVERAGE_TEST_HOME";

    // Run home-dependent cases in a separate test process so parallel tests
    // cannot see these environment overrides.
    if let Some(home) = std::env::var_os(MARKER) {
        let home = PathBuf::from(home);
        assert_eq!(hermes_config::hermes_home(), home);
        return Some(home);
    }

    let home = tempfile::tempdir().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(format!("{module}::{test_name}"))
        .args(["--nocapture", "--color", "never"])
        .current_dir(home.path())
        .env(MARKER, home.path())
        .env("HERMES_HOME", home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("XDG_DATA_HOME", home.path())
        .env("XDG_STATE_HOME", home.path())
        .env("HERMES_TRIGGER_TRIAGE_MODE", mode)
        .env_remove("HERMES_SUBCONSCIOUS_PROFILE")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "sandboxed test {test_name}: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

fn feedback_entry(
    source: &str,
    outcome: &str,
    delta: i32,
    note: &str,
) -> TriggerTriageLearningEntry {
    TriggerTriageLearningEntry {
        at: "2026-01-01T00:00:00Z".to_owned(),
        source: source.to_owned(),
        outcome: outcome.to_owned(),
        decision: "notify".to_owned(),
        severity: 2,
        bias_delta: delta,
        note: note.to_owned(),
    }
}

#[test]
fn feedback_aliases_and_decision_labels() {
    let cases: &[(&[&str], Option<i32>)] = &[
        (
            &["critical", "escalate", "confirmed", "true-positive", "tp"],
            Some(2),
        ),
        (&["useful", "good", "notify", "watch"], Some(1)),
        (&["neutral", "mixed"], Some(0)),
        (&["false-positive", "fp", "noise", "noisy"], Some(-2)),
        (&["drop", "ignore", "spam"], Some(-1)),
        (&["", "unrecognized"], None),
    ];
    for &(aliases, expected) in cases {
        for alias in aliases {
            assert_eq!(
                triage_feedback_delta(&format!("  {}\t", alias.to_ascii_uppercase())),
                expected,
                "feedback alias {alias:?}"
            );
        }
    }

    for (decision, label) in [
        (TriggerTriageDecision::Drop, "drop"),
        (TriggerTriageDecision::Notify, "notify"),
        (TriggerTriageDecision::Escalate, "escalate"),
        (TriggerTriageDecision::AgentRun, "agent-run"),
    ] {
        assert_eq!(decision.as_str(), label);
    }
}

#[test]
fn triage_thresholds_and_assessment_output() {
    let Some(_home) = sandbox_home("triage_thresholds_and_assessment_output", " \t ") else {
        return;
    };

    assert_eq!(trigger_triage_mode(), "off");
    for (source, payload, severity, decision, approval) in [
        ("  CRON  ", " ok ", 1, TriggerTriageDecision::Drop, false),
        ("cron", "timeout", 2, TriggerTriageDecision::Notify, false),
        ("worker", "123456", 0, TriggerTriageDecision::Notify, false),
        ("worker", "panic", 4, TriggerTriageDecision::AgentRun, false),
        (
            "cron",
            "panic error",
            7,
            TriggerTriageDecision::Escalate,
            true,
        ),
    ] {
        let assessment = evaluate_trigger_triage(source, payload);
        assert_eq!(assessment.source, source.trim());
        assert_eq!(assessment.payload, payload.trim());
        assert_eq!(assessment.severity, severity, "{source}: {payload}");
        assert_eq!(assessment.decision, decision, "{source}: {payload}");
        assert_eq!(assessment.requires_approval, approval);
    }

    let rendered =
        render_trigger_triage_assessment(&evaluate_trigger_triage("cron", "panic error"));
    assert_eq!(
        rendered,
        concat!(
            "Trigger triage assessment\n",
            "------------------------\n",
            "source: cron\n",
            "payload: panic error\n",
            "severity: 7\n",
            "decision: escalate\n",
            "requires_approval: true\n",
            "reasons:\n",
            "- runtime panic or crash\n",
            "- error signal\n",
            "- scheduled trigger\n",
        )
    );

    let prefix = "é".repeat(225);
    let payload = format!("{prefix}UNRENDERED");
    let rendered = render_trigger_triage_assessment(&evaluate_trigger_triage("worker", &payload));
    assert!(rendered.contains(&format!("payload: {}", "é".repeat(200))));
    assert!(!rendered.contains(&prefix));
    assert!(!rendered.contains("UNRENDERED"));
    assert!(rendered.ends_with("decision: notify\nrequires_approval: false\nreasons: none\n"));
}

#[test]
fn relaxed_mode_and_subconscious_scoring() {
    let Some(_home) = sandbox_home("relaxed_mode_and_subconscious_scoring", "  ReLaXeD  ") else {
        return;
    };

    assert_eq!(trigger_triage_mode(), "relaxed");
    let assessment = evaluate_trigger_triage(" worker ", " PANIC ");
    assert_eq!(assessment.severity, 3);
    assert_eq!(assessment.decision, TriggerTriageDecision::Notify);
    assert!(!assessment.requires_approval);
    assert_eq!(assessment.reasons, ["runtime panic or crash"]);

    let quiet = evaluate_trigger_triage("worker", "ok");
    assert_eq!(quiet.severity, -1);
    assert_eq!(quiet.decision, TriggerTriageDecision::Drop);

    // Include every available contract term without assuming the sandbox
    // comes with a particular default contract.
    let mut prompt = "VERIFY WALLET".to_owned();
    let mut overlap = 0.0_f64;
    if let Ok(terms) = utility_terms_from_contract() {
        for (term, weight) in terms {
            prompt.push(' ');
            prompt.push_str(&term);
            overlap += weight.max(0.0);
        }
    }
    let expected = 3.0 + overlap.min(2.5);
    assert!((score_subconscious_task(&prompt) - expected).abs() < 1e-10);
    assert!((score_subconscious_task(&prompt.to_ascii_lowercase()) - expected).abs() < 1e-10);
}

#[test]
fn feedback_persistence_evicts_old_entries_and_ranks_sources() {
    let Some(home) = sandbox_home(
        "feedback_persistence_evicts_old_entries_and_ranks_sources",
        "off",
    ) else {
        return;
    };

    let path = home.join("triage").join("learning.json");
    let empty = render_trigger_triage_learning_status();
    assert!(empty.contains("entries: 0\nsource_bias: none\n"));
    assert!(!empty.contains("last_feedback:"));

    let assessment = evaluate_trigger_triage("  WEBHOOK  ", "panic");
    assert_eq!(assessment.severity, 5);
    assert_eq!(assessment.decision, TriggerTriageDecision::AgentRun);
    assert!(matches!(
        append_triage_learning_feedback("webhook", "panic", "bogus", &assessment),
        Err(AgentError::Config(message)) if message.contains("Unknown triage feedback outcome")
    ));
    assert!(!path.exists());

    let state = TriggerTriageLearningState {
        entries: (0..400)
            .map(|i| feedback_entry(&format!("source-{i}"), "neutral", 0, ""))
            .collect(),
    };
    save_trigger_triage_learning_state(&state).unwrap();
    assert!(path.is_file());

    let entry = append_triage_learning_feedback(
        "  WEBHOOK  ",
        " one   two\tthree four five six seven eight nine ten eleven ",
        "  WATCH  ",
        &assessment,
    )
    .unwrap();
    assert_eq!(entry.source, "webhook");
    assert_eq!(entry.outcome, "watch");
    assert_eq!(entry.decision, "agent-run");
    assert_eq!(entry.severity, 5);
    assert_eq!(entry.bias_delta, 1);
    assert_eq!(
        entry.note,
        "one two three four five six seven eight nine ten"
    );

    let persisted = load_trigger_triage_learning_state();
    assert_eq!(persisted.entries.len(), 400);
    assert_eq!(persisted.entries[0].source, "source-1");
    let last = persisted.entries.last().unwrap();
    assert_eq!(last.at, entry.at);
    assert_eq!(last.note, entry.note);

    let status = render_trigger_triage_learning_status();
    assert!(status.contains("entries: 400\nsource_bias:\n- webhook => +1\n"));
    assert_eq!(
        status.lines().filter(|line| line.starts_with("- ")).count(),
        6
    );
    assert!(status.ends_with(&format!(
        "last_feedback: {} source=webhook outcome=watch delta=+1\n",
        entry.at
    )));

    let learned = evaluate_trigger_triage("webhook", "panic");
    assert_eq!(learned.severity, 6);
    assert_eq!(learned.decision, TriggerTriageDecision::AgentRun);
    assert!(learned.reasons.ends_with(&[
        "learning bias applied (+1)".to_owned(),
        "source feedback watch (1)".to_owned(),
    ]));
}

#[test]
fn learning_bias_limits_history_and_matches_notes_across_sources() {
    let Some(_home) = sandbox_home(
        "learning_bias_limits_history_and_matches_notes_across_sources",
        "off",
    ) else {
        return;
    };

    let mut entries = vec![feedback_entry("sensor", "critical", 2, "")];
    entries.extend((0..111).map(|_| feedback_entry("bystander", "useful", 1, " \t ")));
    entries.extend([
        feedback_entry("negative", "fp", -2, ""),
        feedback_entry("negative", "noise", -2, ""),
        feedback_entry("positive", "confirmed", 2, ""),
        feedback_entry("positive", "confirmed", 2, ""),
        feedback_entry("other", "useful", 1, "spool"),
        feedback_entry("sensor", "drop", -1, ""),
        feedback_entry("other", "fp", -2, "Spool"),
        feedback_entry("other", "confirmed", 2, " OverHeat "),
        feedback_entry("SeNsOr", "useful", 1, "spool"),
    ]);
    save_trigger_triage_learning_state(&TriggerTriageLearningState { entries }).unwrap();

    let (bias, reasons) = triage_learning_bias("  SENSOR  ", "  SPOOL and OVERHEAT  ");
    assert_eq!(bias, 1);
    assert_eq!(
        reasons,
        [
            "source feedback useful (1)",
            "matched prior note ' OverHeat '",
            "matched prior note 'Spool'",
        ]
    );
    assert_eq!(triage_learning_bias("sensor", "unrelated").0, 0);
    assert_eq!(triage_learning_bias("negative", "unrelated").0, -3);
    assert_eq!(triage_learning_bias("positive", "unrelated").0, 3);
}

#[test]
fn subconscious_profiles_enforce_risk_and_approval() {
    assert_eq!(
        SubconsciousProfile::parse(" STRICT "),
        Some(SubconsciousProfile::Strict)
    );
    assert_eq!(
        SubconsciousProfile::parse(" Standard "),
        Some(SubconsciousProfile::Balanced)
    );
    assert_eq!(
        SubconsciousProfile::parse("BALANCED"),
        Some(SubconsciousProfile::Balanced)
    );
    assert_eq!(
        SubconsciousProfile::parse(" DeV "),
        Some(SubconsciousProfile::Dev)
    );
    assert_eq!(SubconsciousProfile::parse("unknown"), None);
    assert_eq!(SubconsciousProfile::Balanced.as_str(), "balanced");
    assert_eq!(SubconsciousProfile::Dev.as_str(), "dev");

    for prompt in [
        "RM -RF data",
        "DELETE records",
        "rotate key",
        "prod",
        "mainnet",
    ] {
        assert_eq!(risk_for_prompt(prompt), ("high", true), "{prompt}");
    }
    for prompt in ["live trading", "WALLET audit", "deploy staging"] {
        assert_eq!(risk_for_prompt(prompt), ("medium", true), "{prompt}");
    }
    assert_eq!(risk_for_prompt("summarize notes"), ("low", false));

    let mut task = SubconsciousTask {
        id: "sc-fixture".to_owned(),
        source: "manual".to_owned(),
        prompt: "summarize notes".to_owned(),
        score: 1.0,
        risk: "LOW".to_owned(),
        requires_approval: false,
        status: "pending".to_owned(),
        job_id: None,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    assert_eq!(
        subconscious_guard_allows(SubconsciousProfile::Strict, &task),
        (true, "strict profile allows low-risk task".to_owned())
    );
    task.requires_approval = true;
    assert!(!subconscious_guard_allows(SubconsciousProfile::Strict, &task).0);

    task.risk = "medium".to_owned();
    assert_eq!(
        subconscious_guard_allows(SubconsciousProfile::Balanced, &task),
        (true, "balanced profile allows low/medium risk".to_owned())
    );
    task.risk = "HIGH".to_owned();
    assert_eq!(
        subconscious_guard_allows(SubconsciousProfile::Balanced, &task),
        (
            false,
            "balanced profile blocks high-risk subconscious runs".to_owned()
        )
    );
    assert_eq!(
        subconscious_guard_allows(SubconsciousProfile::Dev, &task),
        (true, "dev profile allows execution".to_owned())
    );
}

#[test]
fn subconscious_state_round_trips_and_reports_an_unusable_parent() {
    let Some(home) = sandbox_home(
        "subconscious_state_round_trips_and_reports_an_unusable_parent",
        "",
    ) else {
        return;
    };

    assert_eq!(subconscious_profile_env(), SubconsciousProfile::Balanced);
    assert!(load_subconscious_state().tasks.is_empty());

    let parent = home.join("subconscious");
    fs::write(&parent, "not a directory").unwrap();
    assert!(matches!(
        save_subconscious_state(&SubconsciousQueueState::default()),
        Err(AgentError::Io(message)) if message.contains("Failed to create")
    ));
    fs::remove_file(&parent).unwrap();

    let state: SubconsciousQueueState = serde_json::from_value(serde_json::json!({
        "tasks": [{
            "id": "sc-fixture",
            "source": "manual",
            "prompt": "verify notes",
            "score": 1.8,
            "risk": "low",
            "requires_approval": false,
            "status": "pending",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }]
    }))
    .unwrap();
    save_subconscious_state(&state).unwrap();

    let loaded = load_subconscious_state();
    assert_eq!(loaded.tasks.len(), 1);
    assert_eq!(loaded.tasks[0].id, "sc-fixture");
    assert_eq!(loaded.tasks[0].prompt, "verify notes");
    assert_eq!(loaded.tasks[0].status, "pending");
    assert!(loaded.tasks[0].job_id.is_none());

    fs::write(parent.join("queue.json"), "not JSON").unwrap();
    assert!(load_subconscious_state().tasks.is_empty());
}

#[test]
fn claim_persists_running_state_and_does_not_reclaim_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("job.json");
    assert!(claim_queued_background_job(&path).unwrap().is_none());

    fs::write(&path, "[]").unwrap();
    assert!(claim_queued_background_job(&path).unwrap().is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), "[]");

    for (input, attempts) in [
        (serde_json::json!({"task": "one"}), 1),
        (serde_json::json!({"status": "QUEUED", "attempts": 3}), 4),
        (
            serde_json::json!({"status": "queued", "attempts": u64::MAX}),
            u64::MAX,
        ),
    ] {
        let original = input.as_object().unwrap();
        write_json_map(&path, original).unwrap();
        assert_eq!(&read_json_map(&path), original);

        let claimed = claim_queued_background_job(&path).unwrap().unwrap();
        assert_eq!(claimed["status"], "running");
        assert_eq!(claimed["attempts"], attempts);
        assert!(claimed["started_at"]
            .as_str()
            .is_some_and(|at| !at.is_empty()));
        assert_eq!(claimed.get("task"), original.get("task"));
        assert_eq!(read_json_map(&path), claimed);
        assert!(claim_queued_background_job(&path).unwrap().is_none());
        assert_eq!(read_json_map(&path), claimed);
    }

    assert_eq!(
        write_json_map(
            &directory.path().join("missing").join("job.json"),
            &read_json_map(&path)
        )
        .unwrap_err()
        .kind(),
        std::io::ErrorKind::NotFound
    );
}

#[test]
fn background_queue_cancellation_and_recovery_use_persisted_status() {
    let Some(home) = sandbox_home(
        "background_queue_cancellation_and_recovery_use_persisted_status",
        "",
    ) else {
        return;
    };

    assert!(matches!(
        queue_background_job(" \t "),
        Err(AgentError::Config(message)) if message == "Background task cannot be empty."
    ));
    assert_eq!(recover_queued_background_jobs(3).unwrap(), 0);
    let path = home.join("background_jobs");
    fs::write(&path, "not a directory").unwrap();
    assert!(matches!(
        background_jobs_dir(),
        Err(AgentError::Io(message))
            if message.contains("Failed to create background job directory")
                && message.contains(path.to_str().unwrap())
    ));
    fs::remove_file(&path).unwrap();
    let directory = background_jobs_dir().unwrap();
    assert_eq!(directory, path);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(background_jobs_dir().unwrap(), directory);
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    assert!(terminate_background_job("missing-fixture")
        .unwrap()
        .contains("not found"));

    // Enter a current-thread runtime but never drive it. This exercises
    // queuing without letting detached jobs launch the current executable.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _runtime_guard = runtime.enter();
    let job = queue_background_job("  summarize scratch notes  ").unwrap();
    assert_eq!(job.task, "summarize scratch notes");
    assert_eq!(job.status_path, directory.join(format!("{}.json", job.id)));
    assert_eq!(job.log_path, directory.join(format!("{}.log", job.id)));
    let queued = read_json_map(&job.status_path);
    assert_eq!(queued["id"], job.id);
    assert_eq!(queued["task"], job.task);
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["attempts"], 0);
    assert!(queued["started_at"].is_null());

    let canceled = terminate_background_job(&job.id).unwrap();
    assert!(canceled.starts_with(&format!("Canceled background job {}\n", job.id)));
    assert!(canceled.ends_with("No active child pid recorded."));
    let persisted = read_json_map(&job.status_path);
    assert_eq!(persisted["status"], "canceled");
    assert_eq!(persisted["error"], "canceled by operator");
    assert_eq!(persisted["pid"], serde_json::Value::Null);
    assert!(persisted["finished_at"]
        .as_str()
        .is_some_and(|at| !at.is_empty()));
    assert!(terminate_background_job(&job.id)
        .unwrap()
        .contains("already canceled"));
    assert_eq!(read_json_map(&job.status_path), persisted);
    assert!(!job.log_path.exists());

    #[cfg(target_os = "linux")]
    {
        // Linux's maximum possible PID is below i32::MAX.
        let absent_pid = i32::MAX as u32;
        assert!(!process_running(absent_pid));
        assert_eq!(
            terminate_pid(absent_pid).unwrap_err().raw_os_error(),
            Some(libc::ESRCH)
        );

        let mut stale = queued;
        stale.insert("status".to_owned(), serde_json::json!("running"));
        stale.insert("pid".to_owned(), serde_json::json!(absent_pid));
        write_json_map(&job.status_path, &stale).unwrap();
        assert!(terminate_background_job(&job.id)
            .unwrap()
            .ends_with(&format!("Pid {absent_pid} was not running.")));
        assert_eq!(
            read_json_map(&job.status_path)["pid"],
            serde_json::Value::Null
        );
    }

    fs::write(
        directory.join("ignored.log"),
        r#"{"status":"queued","task":"ignore"}"#,
    )
    .unwrap();
    fs::write(
        directory.join("blank.json"),
        r#"{"status":"queued","task":"  "}"#,
    )
    .unwrap();
    fs::write(
        directory.join("done.json"),
        r#"{"status":"completed","task":"done"}"#,
    )
    .unwrap();
    assert_eq!(recover_queued_background_jobs(8).unwrap(), 0);

    let first = directory.join("first.json");
    let second = directory.join("second.json");
    fs::write(&first, r#"{"status":"QUEUED","task":" first "}"#).unwrap();
    fs::write(
        &second,
        r#"{"status":"queued","task":" second ","log_path":"second.log"}"#,
    )
    .unwrap();
    assert_eq!(recover_queued_background_jobs(0).unwrap(), 1);
    assert_eq!(recover_queued_background_jobs(8).unwrap(), 2);
    assert_eq!(read_json_map(&first)["status"], "QUEUED");
    assert_eq!(read_json_map(&second)["status"], "queued");
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn background_child_receives_stdin_and_only_the_fixture_is_terminated() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    use std::time::Duration;

    let Some(home) = sandbox_home(
        "background_child_receives_stdin_and_only_the_fixture_is_terminated",
        "",
    ) else {
        return;
    };

    let script = home.join("read-task");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" \"$HERMES_HOME\"\n\
         while IFS= read -r line; do printf '%s\\n' \"$line\"; done\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

    let task = "  first line\nsecond line\n";
    let child = spawn_background_child(&script, task).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("chat\n--query\n-\n{}\n{task}", home.display())
    );

    let mut waiting_child = tokio::process::Command::new("/bin/sh")
        .args(["-c", "IFS= read -r ignored"])
        .stdin(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // Keep the fixture's input open so it waits until signaled.
    let _stdin = waiting_child.stdin.take().unwrap();
    let pid = waiting_child.id().unwrap();
    assert!(process_running(pid));

    // On this current-thread runtime, the scheduled task cannot claim this
    // record before we mark it canceled: there is no await in between.
    let job = queue_background_job("fixture").unwrap();
    let mut running = read_json_map(&job.status_path);
    running.insert("status".to_owned(), serde_json::json!("running"));
    running.insert("pid".to_owned(), serde_json::json!(pid));
    write_json_map(&job.status_path, &running).unwrap();
    let message = terminate_background_job(&job.id).unwrap();
    assert!(message.ends_with(&format!("Sent SIGTERM to pid {pid}.")));

    let status = tokio::time::timeout(Duration::from_secs(5), waiting_child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.signal(), Some(libc::SIGTERM));
    let canceled = read_json_map(&job.status_path);
    assert_eq!(canceled["status"], "canceled");
    assert_eq!(canceled["pid"], serde_json::Value::Null);
    assert!(!job.log_path.exists());
}
