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

#[tokio::test]
async fn api_run_stopped_while_queued_never_reaches_the_inbound_pipeline() {
    let (tx, mut rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let run_id = "run_fixture_queued";
    let inbound = ApiInboundRequest {
        request_id: run_id.into(),
        session_id: "fixture-session".into(),
        user_id: "fixture-user".into(),
        model: None,
        provider: None,
        personality: None,
        prompt: "Never dispatch this fixture".into(),
    };
    state.run_store.write().await.insert(RunRecord::new(
        inbound.request_id.clone(),
        inbound.session_id.clone(),
        inbound.user_id.clone(),
        None,
        None,
        None,
    ));

    let stop = state
        .roundtrip(empty_request("POST", &format!("/v1/runs/{run_id}/stop")), None)
        .await;
    assert_eq!(json_body(&stop)["status"], "stopping");

    let mailbox_key = inbound.session_id.clone();
    run_background_request(
        Arc::clone(&state.mailbox),
        Arc::clone(&state.run_cancels),
        Arc::clone(&state.run_store),
        Arc::clone(&state.inbound_tx),
        inbound,
        mailbox_key,
    )
    .await;

    assert!(matches!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    let record = state.run_store.read().await.get(run_id).unwrap();
    assert_eq!(record.status, "cancelled");
    assert!(record.completed_at.is_some());
    assert_eq!(record.events.last().unwrap()["error"], "Run stopped before dispatch");
    assert!(state.mailbox.read().await.pending.is_empty());
    assert!(state.run_cancels.read().await.pending.is_empty());
}

#[tokio::test]
async fn api_run_reports_a_closed_response_channel_and_clears_pending_state() {
    let (tx, mut rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);
    let start = state
        .roundtrip(
            json_request("POST", "/v1/runs", serde_json::json!({"input": "fixture prompt"})),
            None,
        )
        .await;
    assert!(start.starts_with("HTTP/1.1 202 Accepted"), "{start}");
    let started = json_body(&start);
    let run_id = started["run_id"].as_str().unwrap();
    let request = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.request_id, run_id);
    let mut mailbox = state.mailbox.write().await;
    assert_eq!(mailbox.pending.len(), 1);
    mailbox.pending.clear();
    drop(mailbox);

    let record = wait_for_run_status(&state.run_store, run_id, "failed").await;
    let failure = record.events.last().unwrap();
    assert_eq!(failure["code"], 502);
    assert_eq!(failure["error"], "Gateway closed response channel");
    assert!(state.mailbox.read().await.pending.is_empty());
    assert!(state.run_cancels.read().await.pending.is_empty());
}

#[tokio::test]
async fn api_run_routes_distinguish_missing_runs_and_invalid_input() {
    let (tx, mut rx) = mpsc::channel(1);
    let state = ApiTestState::new(tx);

    for (method, path) in [
        ("GET", "/v1/runs/missing"),
        ("GET", "/v1/runs/missing/events"),
        ("POST", "/v1/runs/missing/stop"),
        ("POST", "/v1/runs/missing/approval"),
    ] {
        let response = state.roundtrip(empty_request(method, path), None).await;
        assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");
        let error = &json_body(&response)["error"];
        assert_eq!(error["type"], "not_found", "{method} {path}");
        assert_eq!(error["message"], "Run not found", "{method} {path}");
    }

    for (body, message) in [
        (serde_json::json!({"input": 42}), "Invalid request:"),
        (
            serde_json::json!({"input": "   "}),
            "Request must include non-empty input",
        ),
    ] {
        let response = state
            .roundtrip(json_request("POST", "/v1/runs", body), None)
            .await;
        assert!(
            response.starts_with("HTTP/1.1 400 Bad Request"),
            "{response}"
        );
        let error = &json_body(&response)["error"];
        assert_eq!(error["type"], "invalid_request_error");
        assert!(error["message"].as_str().unwrap().starts_with(message));
    }
    assert!(state.run_store.read().await.records.is_empty());
    assert!(state.mailbox.read().await.pending.is_empty());
    assert_eq!(rx.try_recv().unwrap_err(), mpsc::error::TryRecvError::Empty);
}
