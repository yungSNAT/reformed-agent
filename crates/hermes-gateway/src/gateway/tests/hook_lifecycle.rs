use super::*;

#[derive(Default)]
struct PauseHook {
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl HookHandler for PauseHook {
    async fn handle(&self, _event: &HookEvent) -> Result<(), String> {
        self.entered.notify_one();
        self.resume.notified().await;
        Ok(())
    }

    fn name(&self) -> &str {
        "pause-hook"
    }
}

#[tokio::test]
async fn cancelled_reset_clears_previous_approval_before_reset_hooks_finish() {
    use hermes_tools::gateway_approval::{self, GatewayApprovalBinding, Resolution};

    let manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("user1");
    let gateway = Gateway::new(manager.clone(), dm, GatewayConfig::default());
    let pause = Arc::new(PauseHook::default());
    let mut hooks = HookRegistry::new();
    hooks.register_in_process("on_session_reset", pause.clone());
    gateway.set_hook_registry(Arc::new(hooks)).await;
    let mut reset = test_incoming("/reset");
    reset.chat_id = "paused-reset-approval".into();
    let key = manager.compose_session_key(&reset.platform, &reset.chat_id, &reset.user_id);
    let before = manager
        .get_or_create_session(&reset.platform, &reset.chat_id, &reset.user_id)
        .await;
    let binding = GatewayApprovalBinding {
        session_key: key.clone(),
        user_id: reset.user_id.clone(),
        notify: gateway_approval::notify_with(|_| async { true }),
    };
    let command = "echo fixture";
    let error =
        gateway_approval::authorise(&binding, command, &serde_json::json!({"command": command}))
            .await
            .unwrap_err();
    assert!(error.contains("shown it"), "{error}");

    {
        let mut request = std::pin::pin!(gateway.apply_command_result(
            &reset,
            &key,
            GatewayCommandResult::ResetSession("fixture response".into()),
            None,
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                () = pause.entered.notified() => {}
                result = &mut request => panic!("reset completed before its hook: {result:?}"),
            }
        })
        .await
        .expect("reset never reached its post-transition hook");
        assert_ne!(manager.get_session(&key).await.unwrap().id, before.id);
    }
    pause.resume.notify_one();
    assert_eq!(
        gateway_approval::approve(&key, &reset.user_id),
        Resolution::NothingPending,
        "a reset session retained its previous gateway approval"
    );
}

#[tokio::test]
async fn automatic_reset_clears_previous_approval_bypass() {
    use hermes_tools::approval;

    let manager = Arc::new(SessionManager::new(SessionConfig {
        reset_policy: hermes_config::SessionResetPolicy::Idle { timeout_minutes: 0 },
        ..SessionConfig::default()
    }));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("user1");
    let gateway = Gateway::new(manager.clone(), dm, GatewayConfig::default());
    gateway
        .register_adapter(
            "test",
            Arc::new(TestAdapter {
                messages: Arc::default(),
            }),
        )
        .await;

    let mut message = test_incoming("/yolo");
    message.chat_id = "automatic-reset-permissions".into();
    let key = manager.compose_session_key(&message.platform, &message.chat_id, &message.user_id);
    gateway.route_message(&message).await.unwrap();
    assert!(approval::is_session_yolo_enabled(&key));
    approval::approve_session(&key, "fixture");

    message.text = "/help".into();
    gateway.route_message(&message).await.unwrap();
    let enabled = approval::is_session_yolo_enabled(&key);
    let approved = approval::is_approved(&key, "fixture");
    approval::clear_session(&key);
    assert!(
        !enabled && !approved,
        "automatic reset kept previous permissions"
    );
}

#[tokio::test]
async fn expiry_clears_all_pending_approvals_before_finalizing_any_session() {
    use hermes_tools::gateway_approval::{self, GatewayApprovalBinding, Resolution};

    let manager = Arc::new(SessionManager::new(SessionConfig {
        reset_policy: hermes_config::SessionResetPolicy::Idle { timeout_minutes: 0 },
        ..SessionConfig::default()
    }));
    let gateway = Gateway::new(
        manager.clone(),
        DmManager::with_pair_behavior(),
        GatewayConfig::default(),
    );
    let pause = Arc::new(PauseHook::default());
    let mut hooks = HookRegistry::new();
    hooks.register_in_process("on_session_finalize", pause.clone());
    gateway.set_hook_registry(Arc::new(hooks)).await;

    let mut keys = Vec::new();
    for chat in ["expire-first-permissions", "expire-second-permissions"] {
        manager.get_or_create_session("test", chat, "user1").await;
        let key = manager.compose_session_key("test", chat, "user1");
        let binding = GatewayApprovalBinding {
            session_key: key.clone(),
            user_id: "user1".to_string(),
            notify: gateway_approval::notify_with(|_| async { true }),
        };
        gateway_approval::authorise(
            &binding,
            "echo fixture",
            &serde_json::json!({"command": "echo fixture"}),
        )
        .await
        .expect_err("fixture command should require approval");
        keys.push(key);
    }

    let mut expiry = std::pin::pin!(gateway.expire_idle_sessions_once("idle_expiry"));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            () = pause.entered.notified() => {}
            count = &mut expiry => panic!("expiry completed before its hook: {count}"),
        }
        let remaining = keys
            .iter()
            .any(|key| gateway_approval::approve(key, "user1") != Resolution::NothingPending);
        pause.resume.notify_one();
        tokio::select! {
            () = pause.entered.notified() => {}
            count = &mut expiry => panic!("expiry skipped its second hook: {count}"),
        }
        pause.resume.notify_one();
        assert_eq!(expiry.await, 2);
        for key in &keys {
            gateway_approval::clear(key);
        }
        assert!(
            !remaining,
            "one expired session kept its previous pending approval"
        );
    })
    .await
    .expect("expiry hooks failed to finish");
}

#[tokio::test]
async fn cancelled_or_unavailable_browser_retirement_preserves_the_gateway_session() {
    use hermes_tools::backends::browser::unavailable_browser_backend;
    use hermes_tools::session_browsers::{BrowserReleaseQueue, SessionBrowsers};

    let browsers = SessionBrowsers::new(|| unavailable_browser_backend("fixture browser"));
    let queue = BrowserReleaseQueue::start(browsers);
    let retirer = queue.retirer();
    let manager = Arc::new(SessionManager::new(SessionConfig {
        reset_policy: hermes_config::SessionResetPolicy::Idle { timeout_minutes: 0 },
        ..SessionConfig::default()
    }));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("user1");
    let gateway = Gateway::new(manager.clone(), dm, GatewayConfig::default())
        .with_browser_retirement(retirer.clone());
    let messages = Arc::new(Mutex::new(Vec::new()));
    gateway
        .register_adapter(
            "test",
            Arc::new(TestAdapter {
                messages: messages.clone(),
            }),
        )
        .await;
    gateway
        .set_message_handler(Arc::new(|_| {
            Box::pin(async { Ok("fixture response".into()) })
        }))
        .await;

    let first = test_incoming("open");
    gateway.route_message(&first).await.unwrap();
    let key = manager.compose_session_key(&first.platform, &first.chat_id, &first.user_id);
    let before = manager.get_session(&key).await.unwrap();
    let browser = retirer.lease(&key);
    let snapshot = browser.tool_overrides()["browser_snapshot"].clone();
    let fixture_error = snapshot.execute(serde_json::json!({})).await.unwrap_err();
    assert!(fixture_error.to_string().contains("fixture browser"));
    let mut slots = Vec::new();
    while let Some(slot) = retirer.try_reserve().unwrap() {
        slots.push(slot);
    }
    assert!(!slots.is_empty());

    let reset = test_incoming("/reset");
    {
        let mut pending = std::pin::pin!(gateway.route_message(&reset));
        assert!(futures::poll!(pending.as_mut()).is_pending());
        let mut other = test_incoming("/help");
        other.chat_id = "other".into();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            gateway.route_message(&other),
        )
        .await
        .expect("another chat waited for the full release queue")
        .unwrap();
        let other_key =
            manager.compose_session_key(&other.platform, &other.chat_id, &other.user_id);
        let other_browser = retirer.lease(&other_key);
        other.text = "/reset".into();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            gateway.route_message(&other),
        )
        .await
        .expect("a reset without a browser backend waited for release capacity")
        .unwrap();
        let stale = other_browser.tool_overrides()["browser_snapshot"]
            .execute(serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(stale.to_string().contains("browser was closed"));
    }
    let sent_before_failure = messages.lock().unwrap().len();
    drop(slots);
    queue.drain().await.unwrap();

    let error = gateway.route_message(&reset).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("Browser release worker has stopped"));
    assert_eq!(messages.lock().unwrap().len(), sent_before_failure);
    let after = manager.get_session(&key).await.unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.messages.len(), before.messages.len());
    assert_eq!(after.last_active_at, before.last_active_at);
    let error = snapshot.execute(serde_json::json!({})).await.unwrap_err();
    assert!(error.to_string().contains("fixture browser"));

    let mut dormant = test_incoming("/new");
    dormant.chat_id = "other".into();
    let dormant_key =
        manager.compose_session_key(&dormant.platform, &dormant.chat_id, &dormant.user_id);
    let dormant_before = manager.get_session(&dormant_key).await.unwrap();
    let dormant_error = gateway.route_message(&dormant).await.unwrap_err();
    assert!(dormant_error
        .to_string()
        .contains("Browser release worker has stopped"));
    let dormant_after = manager.get_session(&dormant_key).await.unwrap();
    assert_eq!(dormant_after.id, dormant_before.id);
    assert_eq!(dormant_after.last_active_at, dormant_before.last_active_at);

    let mut unknown = test_incoming("/clear");
    unknown.chat_id = "unknown".into();
    let unknown_key =
        manager.compose_session_key(&unknown.platform, &unknown.chat_id, &unknown.user_id);
    let unknown_error = gateway.route_message(&unknown).await.unwrap_err();
    assert!(unknown_error
        .to_string()
        .contains("Browser release worker has stopped"));
    assert!(manager.get_session(&unknown_key).await.is_none());
    assert_eq!(messages.lock().unwrap().len(), sent_before_failure);
}

#[derive(Default)]
struct PauseFirstPolicyFinalize {
    used: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl HookHandler for PauseFirstPolicyFinalize {
    async fn handle(&self, event: &HookEvent) -> Result<(), String> {
        if event.context["session_key"] == "test:shared"
            && event.context["reason"] == "auto_reset"
            && !self.used.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            self.entered.notify_one();
            self.resume.notified().await;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "pause-first-policy-finalize"
    }
}

#[tokio::test]
async fn delayed_policy_finalization_keeps_the_admitted_browser_generation() {
    use hermes_tools::backends::browser::unavailable_browser_backend;
    use hermes_tools::session_browsers::{BrowserReleaseQueue, SessionBrowsers};

    let browsers = SessionBrowsers::new(|| unavailable_browser_backend("fixture browser"));
    let queue = BrowserReleaseQueue::start(browsers.clone());
    let manager = Arc::new(SessionManager::new(SessionConfig {
        reset_policy: hermes_config::SessionResetPolicy::Idle { timeout_minutes: 0 },
        ..SessionConfig::default()
    }));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("user1");
    let gateway = Gateway::new(manager, dm, GatewayConfig::default())
        .with_browser_retirement(queue.retirer());
    let pause = Arc::new(PauseFirstPolicyFinalize::default());
    let mut hooks = HookRegistry::new();
    hooks.register_in_process("on_session_finalize", pause.clone());
    gateway.set_hook_registry(Arc::new(hooks)).await;
    gateway
        .register_adapter(
            "test",
            Arc::new(TestAdapter {
                messages: Arc::default(),
            }),
        )
        .await;
    let (send, mut turns) = tokio::sync::mpsc::unbounded_channel();
    gateway
        .set_message_handler_with_context(Arc::new(move |_, context| {
            send.send(context.browser.unwrap()).unwrap();
            Box::pin(async { Ok("fixture response".into()) })
        }))
        .await;
    let message = |chat: &str, text: &str| IncomingMessage {
        platform: "test".into(),
        chat_id: chat.into(),
        user_id: "user1".into(),
        text: text.into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    gateway
        .route_message(&message("shared", "open"))
        .await
        .unwrap();
    let oldest = turns.try_recv().unwrap();
    let first_reset = message("shared", "first reset");
    let (first_result, (current, unrelated)) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(gateway.route_message(&first_reset), async {
                pause.entered.notified().await;
                gateway
                    .route_message(&message("shared", "second reset"))
                    .await
                    .unwrap();
                let current = turns.try_recv().unwrap();
                gateway
                    .route_message(&message("other", "open"))
                    .await
                    .unwrap();
                let unrelated = turns.try_recv().unwrap();
                pause.resume.notify_one();
                (current, unrelated)
            })
        })
        .await
        .expect("another route waited for the paused finalization hook");
    first_result.unwrap();
    let delayed = turns.try_recv().unwrap();
    let snapshot_error = |browser: BrowserLease| async move {
        browser.tool_overrides()["browser_snapshot"]
            .execute(serde_json::json!({}))
            .await
            .unwrap_err()
            .to_string()
    };
    for browser in [oldest, delayed] {
        let error = snapshot_error(browser).await;
        assert!(error.contains("browser was closed"), "{error}");
    }
    for browser in [current, unrelated] {
        let error = snapshot_error(browser).await;
        assert!(error.contains("fixture browser"), "{error}");
    }
    browsers.close_all().await;
    queue.drain().await.unwrap();
}

#[tokio::test]
async fn gateway_emits_agent_start_and_end_hooks() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "agent:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async move { Ok("main-response".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec!["agent:start".to_string(), "agent:end".to_string()]
    );
    let end_payload = events
        .iter()
        .find(|(name, _)| name == "agent:end")
        .map(|(_, ctx)| ctx.clone())
        .expect("agent:end payload should exist");
    assert_eq!(end_payload["success"], serde_json::json!(true));
}

#[tokio::test]
async fn gateway_busy_queue_mode_drains_fifo_followups() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("queue".to_string());
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let calls_for_handler = calls.clone();
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let calls = calls_for_handler.clone();
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|m| {
                    (m.role == MessageRole::User)
                        .then(|| m.content.clone())
                        .flatten()
                })
                .unwrap_or_default();
            calls.lock().unwrap().push(latest.clone());
            if latest == "first" {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    gw.route_message(&test_incoming("second"))
        .await
        .expect("second route should queue");
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_string(), "second".to_string()]
    );
    let texts: Vec<String> = sent
        .lock()
        .unwrap()
        .iter()
        .map(|(_, text)| text.clone())
        .collect();
    assert!(texts
        .iter()
        .any(|text| text.contains("Queued for the next turn")));
    assert!(texts.iter().any(|text| text == "reply:first"));
    assert!(texts.iter().any(|text| text == "reply:second"));
}

#[tokio::test]
async fn gateway_busy_queue_ack_can_be_suppressed() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("queue".to_string());
    cfg.display.busy_ack_enabled = Some(false);
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|m| {
                    (m.role == MessageRole::User)
                        .then(|| m.content.clone())
                        .flatten()
                })
                .unwrap_or_default();
            if latest == "first" {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    gw.route_message(&test_incoming("second"))
        .await
        .expect("second route should queue silently");
    assert!(
        sent.lock()
            .unwrap()
            .iter()
            .all(|(_, text)| !text.contains("Queued for the next turn")),
        "automatic busy ack should be suppressed"
    );
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
}

#[tokio::test]
async fn gateway_queue_command_bypasses_busy_guard_and_drains() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("interrupt".to_string());
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let calls_for_handler = calls.clone();
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let calls = calls_for_handler.clone();
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|m| {
                    (m.role == MessageRole::User)
                        .then(|| m.content.clone())
                        .flatten()
                })
                .unwrap_or_default();
            calls.lock().unwrap().push(latest.clone());
            if latest == "first" {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    gw.route_message(&test_incoming("/queue second"))
        .await
        .expect("/queue should bypass and enqueue");
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_string(), "second".to_string()]
    );
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("Queued follow-up for the active session")));
}

#[tokio::test]
async fn gateway_steer_command_uses_attached_busy_control() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.register_adapter("test", adapter).await;

    let control = Arc::new(BusyControlProbe::default());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let control_for_handler = control.clone();
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let control = control_for_handler.clone();
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            if let Some(registration) = ctx.busy_control {
                assert!(registration.attach(control).await);
            }
            if let Some(tx) = entered.lock().unwrap().take() {
                let _ = tx.send(());
            }
            if let Some(rx) = release.lock().await.take() {
                let _ = rx.await;
            }
            Ok("reply:first".to_string())
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should attach control");
    gw.route_message(&test_incoming("/steer check tests"))
        .await
        .expect("/steer should use attached control");
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");

    assert_eq!(
        control.steers.lock().unwrap().as_slice(),
        ["check tests".to_string()]
    );
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("Steered the running task")));
}

#[tokio::test]
async fn gateway_hook_event_order_captures_start_status_step_end() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "agent:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;

    let gw_for_handler = gw.clone();
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let gw = gw_for_handler.clone();
        Box::pin(async move {
            gw.emit_hook_event(
                "agent:status",
                serde_json::json!({
                    "platform": ctx.platform,
                    "user_id": ctx.user_id,
                    "session_id": ctx.session_key,
                    "event_type": "lifecycle",
                    "message": "Context pressure 85%"
                }),
            )
            .await;
            gw.emit_hook_event(
                "agent:step",
                serde_json::json!({
                    "platform": ctx.platform,
                    "user_id": ctx.user_id,
                    "session_id": ctx.session_key,
                    "iteration": 1,
                    "tool_names": ["memory"],
                    "tools": [{"name":"memory","result":"ok"}]
                }),
            )
            .await;
            Ok("done".to_string())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec![
            "agent:start".to_string(),
            "agent:status".to_string(),
            "agent:step".to_string(),
            "agent:end".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_emits_session_start_and_command_hook_events() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "session:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );
    hooks.register_in_process(
        "command:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert!(names.contains(&"session:start".to_string()));
    assert!(names.contains(&"command:status".to_string()));
}

#[tokio::test]
async fn gateway_emits_session_end_and_reset_for_reset_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "session:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async move { Ok("assistant".to_string()) })
    }))
    .await;
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");

    let normal = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&normal).await.is_ok());

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/reset".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec![
            "session:start".to_string(),
            "session:end".to_string(),
            "session:reset".to_string()
        ]
    );
    let end_payload = events
        .iter()
        .find(|(name, _)| name == "session:end")
        .map(|(_, ctx)| ctx.clone())
        .expect("session:end payload should exist");
    let reset_payload = events
        .iter()
        .find(|(name, _)| name == "session:reset")
        .map(|(_, ctx)| ctx.clone())
        .expect("session:reset payload should exist");
    assert_eq!(end_payload["session_id"], serde_json::json!(session_key));
    assert_eq!(reset_payload["session_id"], serde_json::json!(session_key));
}

#[tokio::test]
async fn gateway_emits_plugin_session_finalize_and_reset_for_reset_command() {
    check_explicit_reset_finalization(hermes_config::SessionResetPolicy::None).await;
}

#[tokio::test]
async fn explicit_reset_does_not_also_run_the_automatic_reset_policy() {
    check_explicit_reset_finalization(hermes_config::SessionResetPolicy::Idle {
        timeout_minutes: 0,
    })
    .await;
}

async fn check_explicit_reset_finalization(reset_policy: hermes_config::SessionResetPolicy) {
    struct ReadFinalizedSession {
        manager: Arc<SessionManager>,
        snapshots: Mutex<Vec<Session>>,
    }

    #[async_trait::async_trait]
    impl HookHandler for ReadFinalizedSession {
        async fn handle(&self, event: &HookEvent) -> Result<(), String> {
            let key = event.context["session_key"].as_str().ok_or("missing key")?;
            let snapshot = self
                .manager
                .get_session(key)
                .await
                .ok_or("missing session")?;
            self.snapshots.lock().unwrap().push(snapshot);
            Ok(())
        }

        fn name(&self) -> &str {
            "read-finalized-session"
        }
    }

    let session_mgr = Arc::new(SessionManager::new(SessionConfig {
        reset_policy,
        ..SessionConfig::default()
    }));
    let reader = Arc::new(ReadFinalizedSession {
        manager: session_mgr.clone(),
        snapshots: Mutex::default(),
    });
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "on_session_finalize",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );
    hooks.register_in_process("on_session_finalize", reader.clone());
    hooks.register_in_process(
        "on_session_reset",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async move { Ok("assistant".to_string()) })
    }))
    .await;
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat-plugin-reset", "user1");

    let normal = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-plugin-reset".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&normal).await.is_ok());
    let old_session = gw
        .session_manager
        .get_session(&session_key)
        .await
        .expect("session should exist");
    assert!(!old_session.messages.is_empty());

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-plugin-reset".into(),
        user_id: "user1".into(),
        text: "/reset".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let finalized = reader.snapshots.lock().unwrap();
    assert_eq!(finalized.len(), 1, "explicit reset finalised twice");
    assert_eq!(
        finalized[0].messages.len(),
        old_session.messages.len(),
        "finalisation lost the previous conversation"
    );
    assert_eq!(finalized[0].id, old_session.id);
    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec![
            "on_session_finalize".to_string(),
            "on_session_reset".to_string()
        ]
    );
    let finalize_payload = &events[0].1;
    let reset_payload = &events[1].1;
    assert_eq!(
        finalize_payload["session_id"],
        serde_json::json!(old_session.id)
    );
    assert_eq!(
        finalize_payload["session_key"],
        serde_json::json!(session_key)
    );
    assert_eq!(finalize_payload["reason"], serde_json::json!("reset"));
    assert_eq!(reset_payload["session_key"], serde_json::json!(session_key));
    assert_eq!(reset_payload["reason"], serde_json::json!("reset"));
    assert_ne!(reset_payload["session_id"], finalize_payload["session_id"]);
}

#[tokio::test]
async fn gateway_stop_all_finalizes_active_sessions() {
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "on_session_finalize",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let first = session_mgr
        .get_or_create_session("test", "stop-chat-a", "user1")
        .await;
    let second = session_mgr
        .get_or_create_session("test", "stop-chat-b", "user2")
        .await;
    let gw = Gateway::new(
        session_mgr,
        DmManager::with_pair_behavior(),
        GatewayConfig::default(),
    );
    gw.set_hook_registry(Arc::new(hooks)).await;

    gw.stop_all().await.expect("stop should succeed");

    let events = hook_seen.lock().unwrap();
    let session_ids: HashSet<String> = events
        .iter()
        .filter(|(name, _)| name == "on_session_finalize")
        .filter_map(|(_, ctx)| ctx["session_id"].as_str().map(ToOwned::to_owned))
        .collect();
    assert_eq!(
        session_ids,
        HashSet::from([first.id.clone(), second.id.clone()])
    );
    assert!(events
        .iter()
        .all(|(_, ctx)| ctx["reason"] == serde_json::json!("shutdown")));
}

#[tokio::test]
async fn gateway_idle_expiry_finalizes_removed_sessions() {
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "on_session_finalize",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_config = SessionConfig {
        reset_policy: hermes_config::session::SessionResetPolicy::Idle { timeout_minutes: 0 },
        ..SessionConfig::default()
    };
    let session_mgr = Arc::new(SessionManager::new(session_config));
    let expired = session_mgr
        .get_or_create_session("test", "idle-chat", "user1")
        .await;
    let session_key = session_mgr.compose_session_key("test", "idle-chat", "user1");
    let gw = Gateway::new(
        session_mgr.clone(),
        DmManager::with_pair_behavior(),
        GatewayConfig::default(),
    );
    gw.set_hook_registry(Arc::new(hooks)).await;

    let expired_count = gw.expire_idle_sessions_once("idle_expiry").await;

    assert_eq!(expired_count, 1);
    assert!(session_mgr.get_session(&session_key).await.is_none());
    let events = hook_seen.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "on_session_finalize");
    assert_eq!(events[0].1["session_id"], serde_json::json!(expired.id));
    assert_eq!(events[0].1["session_key"], serde_json::json!(session_key));
    assert_eq!(events[0].1["reason"], serde_json::json!("idle_expiry"));
}

#[tokio::test]
async fn gateway_hook_error_does_not_break_reset_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let mut hooks = HookRegistry::new();
    hooks.register_in_process("session:*", Arc::new(FailingHook));

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-hook-error".into(),
        user_id: "user1".into(),
        text: "/new".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let replies = sent.lock().unwrap();
    assert!(replies
        .iter()
        .any(|(_, text)| { text.contains("New conversation") || text.contains("Session reset") }));
}
