async fn recovery_gateway(home: &Path, adapter: Arc<dyn PlatformAdapter>) -> Gateway {
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        dm_manager,
        GatewayConfig::default(),
    );
    gw.register_adapter("test", adapter).await;
    gw.enable_reply_recovery(home);
    gw
}

fn record_files(home: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(crate::reply_recovery::records_dir(home))
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn left_by_an_earlier_process(home: &Path, session: &str, reply: Option<&str>) {
    crate::reply_recovery::write(
        &crate::reply_recovery::records_dir(home),
        &crate::reply_recovery::PendingReply {
            session_key: session.to_string(),
            platform: "test".to_string(),
            chat_id: format!("chat-{session}"),
            thread_id: None,
            process: "4242-1".to_string(),
            reply: reply.map(str::to_string),
            attempts: 0,
        },
    );
}

fn incoming(text: &str) -> IncomingMessage {
    IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: text.into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    }
}

#[tokio::test]
async fn a_turn_is_recorded_while_it_runs_and_forgotten_once_answered_or_failed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sent = Arc::new(Mutex::new(Vec::new()));
    let gw = recovery_gateway(
        tmp.path(),
        Arc::new(TestAdapter {
            messages: sent.clone(),
        }),
    )
    .await;
    let home = tmp.path().to_path_buf();
    let during = Arc::new(Mutex::new(Vec::new()));
    let seen = during.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let (home, seen) = (home.clone(), seen.clone());
        Box::pin(async move {
            seen.lock().unwrap().push(record_files(&home).len());
            let fail = messages
                .iter()
                .any(|m| m.content.as_deref().is_some_and(|c| c.contains("fail")));
            if fail {
                Err(GatewayError::Platform("provider down".into()))
            } else {
                Ok("answer".to_string())
            }
        })
    }))
    .await;

    gw.route_message(&incoming("hello")).await.expect("route");
    assert_eq!(*during.lock().unwrap(), vec![1], "one record during the turn");
    assert!(record_files(tmp.path()).is_empty(), "none after the reply");
    assert_eq!(
        sent.lock().unwrap().as_slice(),
        &[("chat1".to_string(), "answer".to_string())]
    );

    let _ = gw.route_message(&incoming("please fail")).await;
    assert!(record_files(tmp.path()).is_empty(), "none after a failed turn");
    assert_eq!(gw.recover_pending_replies().await, 0, "its own turns are not recovered");
}

#[tokio::test]
async fn a_restart_sends_each_lost_reply_once_and_reports_each_cut_off_turn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    left_by_an_earlier_process(tmp.path(), "test:answered", Some("the lost answer"));
    left_by_an_earlier_process(tmp.path(), "test:running", None);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let gw = recovery_gateway(
        tmp.path(),
        Arc::new(TestAdapter {
            messages: sent.clone(),
        }),
    )
    .await;

    assert_eq!(gw.recover_pending_replies().await, 2);
    assert_eq!(gw.recover_pending_replies().await, 0, "sent once");
    assert!(record_files(tmp.path()).is_empty());
    let sent = sent.lock().unwrap().clone();
    assert_eq!(
        sent,
        vec![
            (
                "chat-test:answered".to_string(),
                format!(
                    "{}\n\nthe lost answer",
                    crate::reply_recovery::RECOVERED_REPLY_HEADING
                )
            ),
            (
                "chat-test:running".to_string(),
                crate::reply_recovery::INTERRUPTED_NOTICE.to_string()
            ),
        ]
    );
}

#[tokio::test]
async fn a_reply_that_cannot_be_sent_is_retried_on_later_starts_up_to_the_cap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    left_by_an_earlier_process(tmp.path(), "test:stuck", Some("stuck answer"));
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let gw = recovery_gateway(
        tmp.path(),
        Arc::new(FailingSendAdapter {
            messages: attempts.clone(),
            error: "platform offline",
        }),
    )
    .await;

    for start in 1..=crate::reply_recovery::MAX_SEND_ATTEMPTS {
        assert_eq!(gw.recover_pending_replies().await, 0);
        assert_eq!(attempts.lock().unwrap().len(), start as usize);
        assert_eq!(record_files(tmp.path()).len(), 1, "kept for a later start");
    }
    assert_eq!(gw.recover_pending_replies().await, 0);
    assert_eq!(
        attempts.lock().unwrap().len(),
        crate::reply_recovery::MAX_SEND_ATTEMPTS as usize,
        "no send past the cap"
    );
    assert!(record_files(tmp.path()).is_empty(), "dropped at the cap");
}

#[tokio::test]
async fn without_recovery_enabled_nothing_is_written() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let gw = Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        {
            let mut dm = DmManager::with_pair_behavior();
            dm.authorize_user("user1");
            dm
        },
        GatewayConfig::default(),
    );
    gw.register_adapter(
        "test",
        Arc::new(TestAdapter {
            messages: Arc::new(Mutex::new(Vec::new())),
        }),
    )
    .await;
    gw.set_message_handler(Arc::new(|_| Box::pin(async { Ok("ok".to_string()) })))
        .await;
    gw.route_message(&incoming("hello")).await.expect("route");
    assert!(record_files(tmp.path()).is_empty());
    assert_eq!(gw.recover_pending_replies().await, 0);
}
