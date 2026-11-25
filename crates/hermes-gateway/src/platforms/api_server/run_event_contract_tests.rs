#[tokio::test]
async fn api_run_snapshot_keeps_waiting_after_nonterminal_events() {
    let run_id = "run_fixture";
    let store = Arc::new(RwLock::new(RunStore::default()));
    store.write().await.insert(RunRecord::new(
        run_id.into(),
        "fixture-session".into(),
        "fixture-user".into(),
        None,
        None,
        None,
    ));
    let snapshot = wait_for_run_event_snapshot(Arc::clone(&store), run_id);
    tokio::pin!(snapshot);
    assert!(futures::poll!(snapshot.as_mut()).is_pending());

    store
        .write()
        .await
        .update(run_id, |record| {
            record.status = "running".into();
            record.push_event("run.running", None);
        })
        .expect("active run");
    assert!(futures::poll!(snapshot.as_mut()).is_pending());

    store
        .write()
        .await
        .update(run_id, |record| {
            record.status = "completed".into();
            record.push_event("run.completed", Some(serde_json::json!({"output":"done"})));
        })
        .expect("completed run");
    let resolved = tokio::time::timeout(Duration::from_secs(2), snapshot)
        .await
        .expect("completion wakes event reader")
        .expect("completed snapshot");
    assert_eq!(resolved.status, "completed");
    let types: Vec<_> = resolved
        .events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["run.queued", "run.running", "run.completed"]);
    assert_eq!(resolved.events[2]["output"], "done");
}

#[tokio::test]
async fn api_run_dispatch_failures_appear_in_events_and_release_pending_state() {
    for (configured, message) in [
        (false, "Gateway inbound pipeline is not configured"),
        (true, "Gateway inbound queue is unavailable"),
    ] {
        let (tx, rx) = mpsc::channel(1);
        let state = ApiTestState::new(tx);
        drop(rx);
        if !configured {
            *state.inbound_tx.write().await = None;
        }
        let start = state
            .roundtrip(
                json_request(
                    "POST",
                    "/v1/runs",
                    serde_json::json!({
                        "input":"fixture prompt",
                        "session_id":"fixture-session",
                        "model":"fixture-model",
                        "provider":"fixture-provider",
                        "personality":"fixture-personality"
                    }),
                ),
                None,
            )
            .await;
        assert!(start.starts_with("HTTP/1.1 202 Accepted"), "{start}");
        let started = json_body(&start);
        let run_id = started["run_id"].as_str().unwrap();
        let events = state
            .roundtrip(
                empty_request("GET", &format!("/v1/runs/{run_id}/events")),
                None,
            )
            .await;
        assert!(events.starts_with("HTTP/1.1 200 OK"), "{events}");
        assert!(events.contains("Content-Type: text/event-stream"));
        let (_, tail) = events.split_once("event: run.failed\ndata: ").unwrap();
        let (event, termination) = tail.split_once("\n\n").unwrap();
        let event: serde_json::Value = serde_json::from_str(event).unwrap();
        assert_eq!(event["run_id"], run_id);
        assert_eq!(event["status"], "failed");
        assert_eq!(event["code"], 503);
        assert_eq!(event["error"], message);
        assert_eq!(termination, "data: [DONE]\n\n");

        let status = state
            .roundtrip(empty_request("GET", &format!("/v1/runs/{run_id}")), None)
            .await;
        let summary = json_body(&status);
        assert_eq!(summary["status"], "failed");
        assert_eq!(summary["last_event"], "run.failed");
        assert_eq!(summary["model"], "fixture-model");
        assert_eq!(summary["provider"], "fixture-provider");
        assert_eq!(summary["personality"], "fixture-personality");
        assert!(summary["completed_at"].is_i64());
        assert!(state.mailbox.read().await.pending.is_empty());
        assert!(state.run_cancels.read().await.pending.is_empty());
    }
}
