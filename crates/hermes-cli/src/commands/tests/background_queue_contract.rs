use super::*;
use serde_json::json;

#[tokio::test]
async fn background_status_tail_and_stop_use_persisted_job_records() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::background_queue_contract::background_status_tail_and_stop_use_persisted_job_records",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    for (args, expected) in [
        (&[][..], "Usage: /background <message>"),
        (&["list"][..], "No background jobs found."),
        (&["tail"][..], "No jobs available yet."),
        (&["stop"][..], "No running/queued jobs found."),
        (
            &["tail", "missing"][..],
            "Background job 'missing' not found",
        ),
        (
            &["event"][..],
            "Usage: /background event <source> <payload>",
        ),
        (
            &["event", "fixture", " "][..],
            "Usage: /background event <source> <payload>",
        ),
    ] {
        let output = command_output(&mut app, "/background", args).await;
        assert!(output.contains(expected), "{args:?}: {output}");
    }

    let jobs = background_jobs_dir().unwrap();
    let queued_path = jobs.join("002-pending.json");
    std::fs::write(
        &queued_path,
        json!({"id":"002-pending","status":"queued","attempts":2,"task":"Local queued task"})
            .to_string(),
    )
    .unwrap();
    let log_path = jobs.join("fixture.log");
    std::fs::write(&log_path, "old\none\ntwo\nthree\nfour\nfive\n").unwrap();
    let finished_path = jobs.join("001-finished.json");
    let finished = json!({
        "id":"001-finished","status":"completed","attempts":1,"task":"Completed local task",
        "created_at":"2026-01-01T10:00:00Z","started_at":"2026-01-01T10:01:00Z",
        "finished_at":"2026-01-01T10:02:00Z","log_path":log_path
    })
    .to_string();
    std::fs::write(&finished_path, &finished).unwrap();
    let status = command_output(&mut app, "/background", &["status"]).await;
    assert!(
        status.contains("queued=1 running=0 completed=1 failed=0"),
        "{status}"
    );
    assert!(
        status
            .find("002-pending [queued]")
            .zip(status.find("001-finished [completed]"))
            .is_some_and(|(queued, finished)| queued < finished),
        "{status}"
    );
    let newest = command_output(&mut app, "/background", &["tail"]).await;
    assert!(newest.contains("id: 002-pending"), "{newest}");
    assert!(newest.contains("(log file does not exist yet)"), "{newest}");
    let tail = command_output(&mut app, "/background", &["logs", "001-FIN", "1"]).await;
    assert!(tail.contains("started_at: 2026-01-01T10:01:00Z"), "{tail}");
    assert!(tail.contains("finished_at: 2026-01-01T10:02:00Z"), "{tail}");
    assert!(
        tail.ends_with("--- log tail (5) ---\none\ntwo\nthree\nfour\nfive"),
        "{tail}"
    );

    let stopped = command_output(&mut app, "/background", &["stop"]).await;
    assert!(
        stopped.contains("Canceled background job 002-pending"),
        "{stopped}"
    );
    assert!(
        stopped.contains("No active child pid recorded."),
        "{stopped}"
    );
    let saved = read_json_map(&queued_path);
    assert_eq!(saved["status"], "canceled");
    assert_eq!(saved["task"], "Local queued task");
    assert_eq!(saved["attempts"], 2);
    assert_eq!(saved["error"], "canceled by operator");
    assert!(saved["pid"].is_null());
    assert!(saved["finished_at"]
        .as_str()
        .is_some_and(|date| !date.is_empty()));
    let stopped = command_output(&mut app, "/background", &["stop", "001-fin"]).await;
    assert!(
        stopped.contains("001-finished already completed"),
        "{stopped}"
    );
    assert_eq!(std::fs::read_to_string(finished_path).unwrap(), finished);
}

#[tokio::test]
async fn background_event_records_a_child_failure_and_clears_its_pid() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::background_queue_contract::background_event_records_a_child_failure_and_clears_its_pid",
    ) {
        return;
    }
    let mut app = build_test_app_with_stream(&hermes_config::hermes_home()).await;
    let notify = command_output(
        &mut app,
        "/background",
        &["event", "fixture", "routine update"],
    )
    .await;
    assert!(notify.contains("queue_action: notify-only"), "{notify}");
    assert!(collect_background_jobs(5).is_empty());

    // The scheduler launches this test binary. Its harness rejects the CLI
    // arguments, keeping the fixture local whilst exercising child completion.
    let queued = command_output(
        &mut app,
        "/background",
        &["event", "fixture", "fixture panic"],
    )
    .await;
    let jobs = collect_background_jobs(5);
    let [job] = jobs.as_slice() else {
        panic!("expected one fixture job, got {jobs:?}");
    };
    assert!(
        queued.contains(&format!("background job queued id={}", job.id)),
        "{queued}"
    );
    assert_eq!(job.task, "fixture panic");
    let saved = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let saved = read_json_map(&job.status_path);
            if saved.get("status").and_then(serde_json::Value::as_str) == Some("failed") {
                break saved;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("fixture child should finish");
    assert_eq!(saved["attempts"], 1);
    assert!(saved["pid"].is_null());
    let exit_code = saved["exit_code"].as_i64().expect("child exit code");
    assert_ne!(exit_code, 0);
    let log = std::fs::read_to_string(&job.log_path).unwrap();
    assert!(log.contains("task: fixture panic"), "{log}");
    assert!(log.contains(&format!("exit_code: {exit_code}\n")), "{log}");
    let (_, stderr) = log.split_once("[stderr]\n").expect("child stderr section");
    assert!(stderr.contains("query"), "{log}");
}
