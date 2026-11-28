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
    let child = spawn_background_child(&script, task, None).unwrap();
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
