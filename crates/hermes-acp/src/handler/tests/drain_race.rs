/// A prompt that finds the turn busy is queued before the running turn can
/// take its last look at the queue. The hook runs the running turn's drain
/// step at the latest point the busy prompt passes before it answers; queued
/// there under a second lock, the prompt was stranded after the turn ended.
#[tokio::test]
async fn a_prompt_that_finds_the_turn_busy_is_queued_before_the_drain_can_end() {
    let handler = make_handler().with_prompt_executor(Arc::new(SteeringPromptExecutor::default()));
    let session_id = create_session(&handler).await;
    let crate::session::TurnStart::Started(running) =
        handler.session_manager.begin_turn(&session_id, None)
    else {
        panic!("the first turn starts");
    };
    let sessions = handler.session_manager.clone();
    let drained = Arc::new(std::sync::Mutex::new(None));
    let seen = drained.clone();
    let id = session_id.clone();
    *AFTER_BUSY.lock().unwrap() = Some((
        session_id.clone(),
        Box::new(move || {
            *seen.lock().unwrap() =
                Some(sessions.next_queued_or_finish(&id, running, SessionPhase::Idle));
        }),
    ));

    let resp = handler
        .handle_request(AcpRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "prompt".into(),
            params: Some(json!({"session_id": session_id.clone(), "text": "b"})),
        })
        .await;
    AFTER_BUSY.lock().unwrap().take();

    assert_eq!(resp.result.unwrap()["stopReason"], "end_turn");
    assert_eq!(
        drained.lock().unwrap().take(),
        Some(crate::session::QueuedOrFinished::Queued("b".into()))
    );
}
