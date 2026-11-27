async fn recovery_gateway(home: &Path, adapter: Arc<dyn PlatformAdapter>) -> Gateway {
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        dm_manager,
        GatewayConfig::default(),
    );
    gw.register_adapter("test", adapter).await;
    gw.enable_reply_recovery(home, |_| false);
    gw
}

fn json_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default()
}

/// Records of turns in flight, and records taken over at start.
fn record_files(home: &Path) -> Vec<PathBuf> {
    let dir = crate::reply_recovery::records_dir(home);
    let mut files = json_in(&dir);
    files.extend(json_in(&crate::reply_recovery::recovering_dir(&dir)));
    files
}

fn left_by_an_earlier_process(home: &Path, session: &str, reply: Option<&str>) {
    crate::reply_recovery::write(
        &crate::reply_recovery::records_dir(home),
        &crate::reply_recovery::PendingReply {
            session_key: session.to_string(),
            platform: "test".to_string(),
            chat_id: format!("chat-{session}"),
            thread_id: None,
            // Started since this boot, so only liveness decides.
            process: format!(
                "4242-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ),
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
async fn a_turn_during_recovery_keeps_its_own_record_and_the_lost_reply_is_sent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    // Learn the session key a turn from chat1 records.
    let first = recovery_gateway(tmp.path(), adapter.clone()).await;
    let home = tmp.path().to_path_buf();
    let key = Arc::new(Mutex::new(String::new()));
    let seen = key.clone();
    first
        .set_message_handler(Arc::new(move |_| {
            let (home, seen) = (home.clone(), seen.clone());
            Box::pin(async move {
                let file = record_files(&home).remove(0);
                let record: crate::reply_recovery::PendingReply =
                    serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
                *seen.lock().unwrap() = record.session_key;
                Ok("first".to_string())
            })
        }))
        .await;
    first.route_message(&incoming("hello")).await.expect("route");
    let key = key.lock().unwrap().clone();
    assert!(!key.is_empty());

    // An earlier process answered that session and stopped before sending.
    left_by_an_earlier_process(tmp.path(), &key, Some("the lost answer"));
    let gw = recovery_gateway(tmp.path(), adapter).await;
    let home = tmp.path().to_path_buf();
    let during = Arc::new(Mutex::new(0));
    let count = during.clone();
    gw.set_message_handler(Arc::new(move |_| {
        let (home, count) = (home.clone(), count.clone());
        Box::pin(async move {
            *count.lock().unwrap() = record_files(&home).len();
            Ok("new answer".to_string())
        })
    }))
    .await;
    // The turn routes before recovery runs.
    gw.route_message(&incoming("hello?")).await.expect("route");
    assert_eq!(*during.lock().unwrap(), 2, "its own record beside the taken-over one");
    assert_eq!(gw.recover_pending_replies().await, 1);
    assert!(record_files(tmp.path()).is_empty());
    let sent = sent.lock().unwrap().clone();
    assert!(
        sent.iter().any(|(_, text)| text.ends_with("the lost answer")),
        "{sent:?}"
    );
    assert!(sent.iter().any(|(_, text)| text == "new answer"), "{sent:?}");
}

#[tokio::test]
async fn a_retry_pass_takes_over_a_record_whose_writer_has_since_stopped() {
    static ALIVE_CHECKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn alive(_pid: u32) -> bool {
        // Check 0 is enable_reply_recovery, check 1 is pass 0 (both true while recovering/ is empty).
        // Writer stops before pass 1.
        ALIVE_CHECKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2
    }
    ALIVE_CHECKS.store(0, std::sync::atomic::Ordering::SeqCst);
    let tmp = tempfile::tempdir().expect("tempdir");
    left_by_an_earlier_process(tmp.path(), "test:late", Some("late answer"));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        dm_manager,
        GatewayConfig::default(),
    );
    gw.register_adapter(
        "test",
        Arc::new(TestAdapter {
            messages: sent.clone(),
        }),
    )
    .await;
    gw.enable_reply_recovery(tmp.path(), alive);
    let recovered = gw
        .recover_pending_replies_with_retries(std::time::Duration::from_millis(2))
        .await;
    assert_eq!(recovered, 1);
    assert!(record_files(tmp.path()).is_empty());
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_turn_with_no_handler_leaves_no_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let gw = recovery_gateway(
        tmp.path(),
        Arc::new(TestAdapter {
            messages: Arc::new(Mutex::new(Vec::new())),
        }),
    )
    .await;
    assert!(gw.route_message(&incoming("hello")).await.is_err());
    assert!(record_files(tmp.path()).is_empty());
}

#[tokio::test]
async fn a_reply_that_cannot_be_sent_is_retried_in_the_same_process_up_to_the_cap() {
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
    let sent = gw
        .recover_pending_replies_with_retries(std::time::Duration::from_millis(5))
        .await;
    assert_eq!(sent, 0);
    assert_eq!(
        attempts.lock().unwrap().len(),
        crate::reply_recovery::MAX_SEND_ATTEMPTS as usize
    );
    assert!(record_files(tmp.path()).is_empty(), "dropped at the cap");
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

struct FailThenOkAdapter {
    succeed_first: Mutex<usize>,
    fail_remaining: Mutex<usize>,
    messages: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl PlatformAdapter for FailThenOkAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.messages
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string()));
        {
            let mut skip = self.succeed_first.lock().unwrap();
            if *skip > 0 {
                *skip -= 1;
                return Ok(());
            }
        }
        let mut remaining = self.fail_remaining.lock().unwrap();
        if *remaining > 0 {
            *remaining -= 1;
            return Err(GatewayError::Platform("platform offline".into()));
        }
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_image_url(
        &self,
        _chat_id: &str,
        _image_url: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "test"
    }
}

async fn learn_session_key(adapter: Arc<dyn PlatformAdapter>) -> String {
    let probe_home = tempfile::tempdir().expect("tempdir");
    let first = recovery_gateway(probe_home.path(), adapter).await;
    let home = probe_home.path().to_path_buf();
    let key = Arc::new(Mutex::new(String::new()));
    let seen = key.clone();
    first
        .set_message_handler(Arc::new(move |_| {
            let (home, seen) = (home.clone(), seen.clone());
            Box::pin(async move {
                let file = record_files(&home).remove(0);
                let record: crate::reply_recovery::PendingReply =
                    serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
                *seen.lock().unwrap() = record.session_key;
                Ok("probe".to_string())
            })
        }))
        .await;
    first.route_message(&incoming("probe")).await.expect("probe");
    let session_key = key.lock().unwrap().clone();
    assert!(!session_key.is_empty());
    session_key
}

/// Simulate a gateway process restart in tests: records written by `this_process()`
/// now belong to an earlier stopped process so the next gateway instance takes them over.
fn simulate_restart(home: &Path) -> usize {
    let mut count = 0;
    for path in json_in(&crate::reply_recovery::records_dir(home)) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Ok(mut record) = serde_json::from_slice::<crate::reply_recovery::PendingReply>(&bytes) {
            record.process = "4242-1".to_string();
            let body = serde_json::to_vec_pretty(&record).expect("serialize");
            std::fs::write(&path, body).expect("rewrite");
            count += 1;
        }
    }
    count
}

#[tokio::test]
async fn a_turn_preserves_an_unmoved_record_when_recovering_dir_is_blocked() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter: Arc<dyn PlatformAdapter> = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_key = learn_session_key(adapter.clone()).await;
    sent.lock().unwrap().clear();

    let tmp = tempfile::tempdir().expect("tempdir");
    left_by_an_earlier_process(tmp.path(), &session_key, Some("old lost answer"));
    let dir = crate::reply_recovery::records_dir(tmp.path());
    let recovering = crate::reply_recovery::recovering_dir(&dir);
    std::fs::write(&recovering, b"blocking recovering dir").expect("block");

    let gw = recovery_gateway(tmp.path(), adapter.clone()).await;
    gw.set_message_handler(Arc::new(|_| Box::pin(async { Ok("new answer".to_string()) })))
        .await;
    gw.route_message(&incoming("hello again")).await.expect("route");

    std::fs::remove_file(&recovering).expect("unblock");
    let gw_next = recovery_gateway(tmp.path(), adapter).await;
    assert_eq!(gw_next.recover_pending_replies().await, 1);
    let sent = sent.lock().unwrap().clone();
    assert!(
        sent.iter().any(|(_, text)| text.ends_with("old lost answer")),
        "blocked old record must survive a subsequent turn in the same session: {sent:?}"
    );
    assert!(sent.iter().any(|(_, text)| text == "new answer"), "{sent:?}");
}

#[tokio::test]
async fn a_subsequent_turn_keeps_an_earlier_failed_live_send_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailThenOkAdapter {
        succeed_first: Mutex::new(0),
        fail_remaining: Mutex::new(1),
        messages: sent.clone(),
    });
    let gw = recovery_gateway(tmp.path(), adapter).await;
    gw.set_message_handler(Arc::new(|messages| {
        let last = messages
            .last()
            .and_then(|m| m.content.as_deref())
            .unwrap_or("")
            .to_string();
        Box::pin(async move { Ok(format!("reply:{last}")) })
    }))
    .await;

    assert!(gw.route_message(&incoming("first")).await.is_err());
    gw.route_message(&incoming("second")).await.expect("second succeeds");
    assert_eq!(simulate_restart(tmp.path()), 1, "failed live send record survived turn 2");

    let recovered_sent = Arc::new(Mutex::new(Vec::new()));
    let gw_next = recovery_gateway(
        tmp.path(),
        Arc::new(TestAdapter {
            messages: recovered_sent.clone(),
        }),
    )
    .await;
    assert_eq!(gw_next.recover_pending_replies().await, 1);
    let recovered = recovered_sent.lock().unwrap().clone();
    assert_eq!(
        recovered,
        vec![(
            "chat1".to_string(),
            format!(
                "{}\n\nreply:first",
                crate::reply_recovery::RECOVERED_REPLY_HEADING
            )
        )]
    );
}

#[tokio::test]
async fn an_original_live_send_is_counted_toward_the_three_attempt_cap() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailingSendAdapter {
        messages: attempts.clone(),
        error: "platform offline",
    });
    let gw1 = recovery_gateway(tmp.path(), adapter.clone()).await;
    gw1.set_message_handler(Arc::new(|_| Box::pin(async { Ok("live reply".to_string()) })))
        .await;
    assert!(gw1.route_message(&incoming("hello")).await.is_err());
    let live_calls = attempts
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, text)| text.ends_with("live reply"))
        .count();
    assert_eq!(live_calls, 1, "original send attempted once");
    assert_eq!(simulate_restart(tmp.path()), 1, "live failed record kept on disk");

    let gw2 = recovery_gateway(tmp.path(), adapter).await;
    let recovered = gw2
        .recover_pending_replies_with_retries(std::time::Duration::from_millis(2))
        .await;
    assert_eq!(recovered, 0);
    let total_reply_calls = attempts
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, text)| text.ends_with("live reply"))
        .count();
    assert_eq!(
        total_reply_calls,
        crate::reply_recovery::MAX_SEND_ATTEMPTS as usize,
        "original send plus recovery retries must not exceed MAX_SEND_ATTEMPTS"
    );
    assert!(record_files(tmp.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_gateway_recoveries_at_two_authorize_at_most_three_total_calls() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = crate::reply_recovery::records_dir(tmp.path());
    crate::reply_recovery::write(
        &dir,
        &crate::reply_recovery::PendingReply {
            session_key: "test:race".to_string(),
            platform: "test".to_string(),
            chat_id: "chat-race".to_string(),
            thread_id: None,
            process: "4242-1".to_string(),
            reply: Some("race reply".to_string()),
            attempts: crate::reply_recovery::MAX_SEND_ATTEMPTS - 1,
        },
    )
    .expect("write");

    struct SlowFailAdapter {
        calls: Arc<Mutex<Vec<(String, String)>>>,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        hold_first: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl PlatformAdapter for SlowFailAdapter {
        async fn start(&self) -> Result<(), GatewayError> {
            Ok(())
        }
        async fn stop(&self) -> Result<(), GatewayError> {
            Ok(())
        }
        async fn send_message(
            &self,
            chat_id: &str,
            text: &str,
            _parse_mode: Option<ParseMode>,
        ) -> Result<(), GatewayError> {
            self.calls
                .lock()
                .unwrap()
                .push((chat_id.to_string(), text.to_string()));
            if self
                .hold_first
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Err(GatewayError::Platform("offline".into()))
        }
        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &str,
            _text: &str,
        ) -> Result<(), GatewayError> {
            Ok(())
        }
        async fn send_file(
            &self,
            _chat_id: &str,
            _file_path: &str,
            _caption: Option<&str>,
        ) -> Result<(), GatewayError> {
            Ok(())
        }
        async fn send_image_url(
            &self,
            _chat_id: &str,
            _image_url: &str,
            _caption: Option<&str>,
        ) -> Result<(), GatewayError> {
            Ok(())
        }
        fn is_running(&self) -> bool {
            true
        }
        fn platform_name(&self) -> &str {
            "test"
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(SlowFailAdapter {
        calls: calls.clone(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        hold_first: std::sync::atomic::AtomicBool::new(true),
    });
    let gw1 = recovery_gateway(tmp.path(), adapter.clone()).await;
    let gw2 = recovery_gateway(tmp.path(), adapter.clone()).await;

    let first_fut = gw1.recover_pending_replies();
    tokio::pin!(first_fut);
    let (r1, r2) = tokio::join!(first_fut, async {
        adapter.entered.notified().await;
        let second = gw2.recover_pending_replies().await;
        adapter.release.notify_one();
        second
    });
    assert_eq!(r1 + r2, 0);
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "at most one third-attempt call across concurrent recoveries"
    );
    assert!(record_files(tmp.path()).is_empty());
}

#[tokio::test]
async fn three_recorded_attempts_without_sends_exhaust_record() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = crate::reply_recovery::records_dir(tmp.path());
    crate::reply_recovery::write(
        &dir,
        &crate::reply_recovery::PendingReply {
            session_key: "test:crash-before-send".to_string(),
            platform: "test".to_string(),
            chat_id: "chat-crash".to_string(),
            thread_id: None,
            process: "4242-1".to_string(),
            reply: Some("never sent".to_string()),
            attempts: 0,
        },
    )
    .expect("write");
    assert_eq!(crate::reply_recovery::take_over(&dir, |_| false), 1);
    let (path, mut rec) = crate::reply_recovery::recovering(&dir).remove(0);
    for _ in 0..crate::reply_recovery::MAX_SEND_ATTEMPTS {
        assert_eq!(
            crate::reply_recovery::count_attempt(&dir, &path, &mut rec),
            crate::reply_recovery::AttemptOutcome::Reserved
        );
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let gw = recovery_gateway(
        tmp.path(),
        Arc::new(TestAdapter {
            messages: calls.clone(),
        }),
    )
    .await;
    assert_eq!(gw.recover_pending_replies().await, 0);
    assert!(calls.lock().unwrap().is_empty(), "exhausted without 3 adapter sends");
    assert!(record_files(tmp.path()).is_empty());
}

#[tokio::test]
async fn a_writer_stopping_on_the_old_final_pass_is_still_retried_until_delivery() {
    static ALIVE_CHECKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn alive(_pid: u32) -> bool {
        let check = ALIVE_CHECKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Check 0 is enable_reply_recovery; checks 1..=3 are passes 0..=2.
        // Writer stops right as pass 3 (the former MAX_SEND_ATTEMPTS final pass) begins.
        check <= crate::reply_recovery::MAX_SEND_ATTEMPTS as usize
    }
    ALIVE_CHECKS.store(0, std::sync::atomic::Ordering::SeqCst);

    let tmp = tempfile::tempdir().expect("tempdir");
    left_by_an_earlier_process(tmp.path(), "test:late-final-pass", Some("late pass answer"));
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailThenOkAdapter {
        succeed_first: Mutex::new(0),
        fail_remaining: Mutex::new((crate::reply_recovery::MAX_SEND_ATTEMPTS - 1) as usize),
        messages: attempts.clone(),
    });
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        dm_manager,
        GatewayConfig::default(),
    );
    gw.register_adapter("test", adapter).await;
    gw.enable_reply_recovery(tmp.path(), alive);

    let recovered = gw
        .recover_pending_replies_with_retries(std::time::Duration::from_millis(2))
        .await;
    assert_eq!(recovered, 1, "must retry past the old pass cap until delivered");
    assert_eq!(
        attempts.lock().unwrap().len(),
        crate::reply_recovery::MAX_SEND_ATTEMPTS as usize
    );
    assert!(record_files(tmp.path()).is_empty());
}

#[tokio::test]
async fn streaming_turns_record_recover_and_count_live_sends() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: attempts.clone(),
    });
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let cfg = GatewayConfig {
        streaming_enabled: true,
        ..Default::default()
    };
    let gw = Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        dm_manager,
        cfg,
    );
    gw.register_adapter("test", adapter).await;
    gw.enable_reply_recovery(tmp.path(), |_| false);

    let home = tmp.path().to_path_buf();
    let seen = Arc::new(Mutex::new(0usize));
    let seen_clone = seen.clone();
    gw.set_streaming_handler(Arc::new(move |_, _| {
        let (home, seen_clone) = (home.clone(), seen_clone.clone());
        Box::pin(async move {
            *seen_clone.lock().unwrap() = record_files(&home).len();
            Ok("streamed reply".to_string())
        })
    }))
    .await;

    gw.route_message(&incoming("stream please"))
        .await
        .expect("stream route");
    assert_eq!(*seen.lock().unwrap(), 1, "streaming turn must record while active");
    assert!(record_files(tmp.path()).is_empty(), "cleared after stream send");
    assert!(
        attempts
            .lock()
            .unwrap()
            .iter()
            .any(|(_, text)| text == "streamed reply")
    );

    // Failed live send on streaming path must also count toward MAX_SEND_ATTEMPTS.
    let fail_home = tempfile::tempdir().expect("tempdir");
    let fail_calls = Arc::new(Mutex::new(Vec::new()));
    let fail_adapter = Arc::new(FailThenOkAdapter {
        succeed_first: Mutex::new(1),
        fail_remaining: Mutex::new(10),
        messages: fail_calls.clone(),
    });
    let mut dm_fail = DmManager::with_pair_behavior();
    dm_fail.authorize_user("user1");
    let gw_fail = Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        dm_fail,
        GatewayConfig {
            streaming_enabled: true,
            ..Default::default()
        },
    );
    gw_fail.register_adapter("test", fail_adapter.clone()).await;
    gw_fail.enable_reply_recovery(fail_home.path(), |_| false);
    gw_fail
        .set_streaming_handler(Arc::new(|_, _| {
            Box::pin(async move { Ok("failed stream reply".to_string()) })
        }))
        .await;
    assert!(gw_fail.route_message(&incoming("stream fail")).await.is_err());
    assert_eq!(simulate_restart(fail_home.path()), 1, "failed streaming send kept on disk");

    let gw_recover = recovery_gateway(fail_home.path(), fail_adapter).await;
    assert_eq!(
        gw_recover
            .recover_pending_replies_with_retries(std::time::Duration::from_millis(2))
            .await,
        0
    );
    let stream_reply_sends = fail_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, text)| text.ends_with("failed stream reply"))
        .count();
    assert_eq!(
        stream_reply_sends,
        crate::reply_recovery::MAX_SEND_ATTEMPTS as usize
    );
    assert!(record_files(fail_home.path()).is_empty());
}

#[tokio::test]
async fn retry_loop_terminates_when_stopped_record_cannot_be_taken_over() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = crate::reply_recovery::records_dir(tmp.path());
    let path = crate::reply_recovery::write(
        &dir,
        &crate::reply_recovery::PendingReply {
            session_key: "test:unmovable".to_string(),
            platform: "test".to_string(),
            chat_id: "chat-unmovable".to_string(),
            thread_id: None,
            process: "4242-1".to_string(),
            reply: Some("blocked takeover".to_string()),
            attempts: 0,
        },
    )
    .expect("write");
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap();
    let blocker = crate::reply_recovery::recovering_dir(&dir).join(format!("{stem}-42421.json"));
    std::fs::create_dir_all(blocker.join("keep-non-empty")).expect("blocker dir");

    let sent = Arc::new(Mutex::new(Vec::new()));
    let gw = recovery_gateway(
        tmp.path(),
        Arc::new(TestAdapter {
            messages: sent.clone(),
        }),
    )
    .await;

    let recovered = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        gw.recover_pending_replies_with_retries(std::time::Duration::from_millis(5)),
    )
    .await
    .expect("retry loop must terminate rather than spin forever when take_over fails");
    assert_eq!(recovered, 0);
    assert!(sent.lock().unwrap().is_empty());
    assert!(path.exists(), "unmovable record must remain preserved");
}

#[cfg(unix)]
#[tokio::test]
async fn retry_loop_terminates_when_recovering_dir_is_symlinked() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = crate::reply_recovery::records_dir(tmp.path());
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(
        outside.join("leak.json"),
        serde_json::to_vec(&crate::reply_recovery::PendingReply {
            session_key: "test:symlink-dir".to_string(),
            platform: "test".to_string(),
            chat_id: "chat-sym".to_string(),
            thread_id: None,
            process: "4242-1".to_string(),
            reply: Some("should not leak".to_string()),
            attempts: 0,
        })
        .unwrap(),
    )
    .expect("write outside");
    std::fs::create_dir_all(&dir).expect("records dir");
    std::os::unix::fs::symlink(&outside, crate::reply_recovery::recovering_dir(&dir))
        .expect("symlink");

    let sent = Arc::new(Mutex::new(Vec::new()));
    let gw = recovery_gateway(
        tmp.path(),
        Arc::new(TestAdapter {
            messages: sent.clone(),
        }),
    )
    .await;
    let recovered = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        gw.recover_pending_replies_with_retries(std::time::Duration::from_millis(5)),
    )
    .await
    .expect("symlinked recovering/ must not cause infinite loop");
    assert_eq!(recovered, 0);
    assert!(sent.lock().unwrap().is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn retry_loop_terminates_when_recovered_record_is_readonly() {
    let tmp = tempfile::tempdir().expect("tempdir");
    left_by_an_earlier_process(tmp.path(), "test:readonly", Some("cannot persist count"));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let gw = recovery_gateway(
        tmp.path(),
        Arc::new(TestAdapter {
            messages: sent.clone(),
        }),
    )
    .await;
    let (path, _) = crate::reply_recovery::recovering(&crate::reply_recovery::records_dir(tmp.path()))
        .remove(0);
    let mut perms = std::fs::metadata(&path).expect("meta").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&path, perms).expect("readonly");

    let recovered = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        gw.recover_pending_replies_with_retries(std::time::Duration::from_millis(5)),
    )
    .await
    .expect("readonly recovered record must terminate retry loop instead of hanging");
    assert_eq!(recovered, 0);
    assert!(sent.lock().unwrap().is_empty());
    assert!(path.exists(), "record stays intact when WriteFailed");
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut perms = std::fs::metadata(&path).expect("meta").permissions();
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(&path, perms);
    }
}

#[tokio::test]
async fn enabled_turns_refuse_to_run_or_send_when_reply_begin_fails() {
    for streaming in [false, true] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = crate::reply_recovery::records_dir(tmp.path());
        std::fs::create_dir_all(dir.parent().unwrap()).expect("gateway dir");
        std::fs::write(&dir, b"block pending-replies dir").expect("block dir");

        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(
            Arc::new(SessionManager::new(SessionConfig::default())),
            dm_manager,
            GatewayConfig {
                streaming_enabled: streaming,
                ..Default::default()
            },
        );
        gw.register_adapter(
            "test",
            Arc::new(TestAdapter {
                messages: sent.clone(),
            }),
        )
        .await;
        gw.enable_reply_recovery(tmp.path(), |_| false);

        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if streaming {
            let ran_c = ran.clone();
            gw.set_streaming_handler(Arc::new(move |_, _| {
                ran_c.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok("unrecorded stream reply".to_string()) })
            }))
            .await;
        } else {
            let ran_c = ran.clone();
            gw.set_message_handler(Arc::new(move |_| {
                ran_c.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok("unrecorded reply".to_string()) })
            }))
            .await;
        }

        let res = gw.route_message(&incoming("hello")).await;
        assert!(res.is_err(), "streaming={streaming} must fail closed when reply_begin fails");
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "streaming={streaming} must not run handler without durable record"
        );
        let messages = sent.lock().unwrap().clone();
        assert!(
            !messages
                .iter()
                .any(|(_, t)| t == "..." || t.contains("unrecorded")),
            "streaming={streaming} must not emit anchor or unrecorded reply: {messages:?}"
        );
    }
}

#[tokio::test]
async fn enabled_turns_refuse_to_send_and_keep_record_when_begin_send_fails() {
    for streaming in [false, true] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(
            Arc::new(SessionManager::new(SessionConfig::default())),
            dm_manager,
            GatewayConfig {
                streaming_enabled: streaming,
                ..Default::default()
            },
        );
        gw.register_adapter(
            "test",
            Arc::new(TestAdapter {
                messages: sent.clone(),
            }),
        )
        .await;
        gw.enable_reply_recovery(tmp.path(), |_| false);

        let dir = crate::reply_recovery::records_dir(tmp.path());
        let dir_hook = dir.clone();
        let block_lock = move || {
            let lock = dir_hook.join(".lock");
            std::fs::remove_file(&lock).expect("remove lock file");
            std::fs::create_dir(&lock).expect("replace lock with dir");
        };
        if streaming {
            gw.set_streaming_handler(Arc::new(move |_, _| {
                block_lock();
                Box::pin(async { Ok("unreserved stream reply".to_string()) })
            }))
            .await;
        } else {
            gw.set_message_handler(Arc::new(move |_| {
                block_lock();
                Box::pin(async { Ok("unreserved reply".to_string()) })
            }))
            .await;
        }

        let res = gw.route_message(&incoming("hello")).await;
        assert!(res.is_err(), "streaming={streaming} must fail when begin_send fails");
        let messages = sent.lock().unwrap().clone();
        assert!(
            !messages.iter().any(|(_, t)| t.contains("unreserved")),
            "streaming={streaming} must never send an unreserved reply: {messages:?}"
        );

        let files = json_in(&dir);
        assert_eq!(
            files.len(),
            1,
            "streaming={streaming} must preserve pending record on begin_send failure"
        );
        let kept: crate::reply_recovery::PendingReply =
            serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
        assert_eq!(kept.reply, None);
        assert_eq!(kept.attempts, 0);

        std::fs::remove_dir(dir.join(".lock")).expect("unblock lock");
        assert_eq!(simulate_restart(tmp.path()), 1);
        let recovered_sent = Arc::new(Mutex::new(Vec::new()));
        let gw_next = recovery_gateway(
            tmp.path(),
            Arc::new(TestAdapter {
                messages: recovered_sent.clone(),
            }),
        )
        .await;
        assert_eq!(gw_next.recover_pending_replies().await, 1);
        assert_eq!(
            recovered_sent.lock().unwrap().as_slice(),
            &[(
                "chat1".to_string(),
                crate::reply_recovery::INTERRUPTED_NOTICE.to_string()
            )]
        );
    }
}
