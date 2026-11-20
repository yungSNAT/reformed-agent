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
async fn completed_old_turn_leaves_replacement_transcript_intact() {
    use hermes_tools::gateway_approval::{self, GatewayApprovalBinding, Resolution};

    for streaming_enabled in [false, true] {
        let manager = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm = DmManager::with_pair_behavior();
        dm.authorize_user("user1");
        let gateway = Gateway::new(
            manager.clone(),
            dm,
            GatewayConfig {
                streaming_enabled,
                ..GatewayConfig::default()
            },
        );
        gateway
            .register_adapter(
                "test",
                Arc::new(TestAdapter {
                    messages: Arc::default(),
                }),
            )
            .await;
        let pause = Arc::new(PauseHook::default());
        let handler: MessageHandlerWithContext = Arc::new({
            let pause = pause.clone();
            move |_, context| {
                let pause = pause.clone();
                Box::pin(async move {
                    let permissions = GatewayApprovalBinding {
                        session_key: context.session_key,
                        user_id: context.user_id,
                        generation: context.session_generation.unwrap(),
                        notify: gateway_approval::notify_with(|_| async {
                            panic!("A retired turn tried to send a new approval prompt")
                        }),
                    };
                    pause.entered.notify_one();
                    pause.resume.notified().await;
                    let error = gateway_approval::authorise(
                        &permissions,
                        "echo fixture",
                        &serde_json::json!({"command": "echo fixture"}),
                    )
                    .await
                    .unwrap_err();
                    assert!(error.contains("conversation has ended"), "{error}");
                    assert_eq!(
                        gateway_approval::approve(&permissions.session_key, &permissions.user_id),
                        Resolution::NothingPending
                    );
                    Ok("old answer".into())
                })
            }
        });
        gateway
            .set_message_handler_with_context(handler.clone())
            .await;
        gateway
            .set_streaming_handler_with_context(Arc::new(move |messages, context, _| {
                handler(messages, context)
            }))
            .await;

        let incoming = test_incoming("old question");
        let key =
            manager.compose_session_key(&incoming.platform, &incoming.chat_id, &incoming.user_id);
        let mut old = std::pin::pin!(gateway.route_message(&incoming));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                () = pause.entered.notified() => {}
                result = &mut old => panic!("turn completed before its model response: {result:?}"),
            }
            gateway
                .route_message(&test_incoming("/reset"))
                .await
                .unwrap();
            manager
                .add_message(&key, Message::user("replacement question"))
                .await;
            pause.resume.notify_one();
            old.await.unwrap();
        })
        .await
        .expect("turn and reset did not finish");
        let messages = manager.get_messages(&key).await;
        assert_eq!(messages.len(), 1, "streaming_enabled={streaming_enabled}");
        assert_eq!(
            messages[0].content,
            Message::user("replacement question").content
        );
    }
}

#[tokio::test]
async fn committed_reset_delivers_all_hooks_after_request_cancellation() {
    use hermes_tools::gateway_approval::{self, GatewayApprovalBinding, Resolution};

    let manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("user1");
    let gateway = Gateway::new(manager.clone(), dm, GatewayConfig::default());
    let pause = Arc::new(PauseHook::default());
    let mut hooks = HookRegistry::new();
    let completed = Arc::new(PauseHook::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    hooks.register_in_process("session:end", pause.clone());
    for event in ["on_session_finalize", "session:reset", "on_session_reset"] {
        hooks.register_in_process(event, Arc::new(RecordingHook { seen: seen.clone() }));
    }
    hooks.register_in_process("on_session_reset", completed.clone());
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
        generation: Default::default(),
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
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        completed.entered.notified(),
    )
    .await
    .expect("committed reset lost lifecycle callbacks after cancellation");
    completed.resume.notify_one();
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .map(|(event, _)| event.as_str())
            .collect::<Vec<_>>(),
        ["on_session_finalize", "session:reset", "on_session_reset"],
    );
    assert_eq!(seen[0].1["session_id"], before.id);
}

#[tokio::test]
async fn delayed_yolo_cannot_restore_permissions_after_reset_or_switch() {
    use hermes_tools::approval;

    for (chat_id, transition) in [
        ("delayed-yolo-reset", "/reset"),
        ("delayed-yolo-switch", "/sessions test:permission-target"),
    ] {
        let manager = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm = DmManager::with_pair_behavior();
        dm.authorize_user("user1");
        let gateway = Gateway::new(manager.clone(), dm, GatewayConfig::default());
        let sent = Arc::new(Mutex::new(Vec::new()));
        gateway
            .register_adapter(
                "test",
                Arc::new(TestAdapter {
                    messages: sent.clone(),
                }),
            )
            .await;
        let pause = Arc::new(PauseHook::default());
        let mut hooks = HookRegistry::new();
        hooks.register_in_process("command:yolo", pause.clone());
        gateway.set_hook_registry(Arc::new(hooks)).await;
        let mut command = test_incoming("/yolo");
        command.chat_id = chat_id.into();
        let key = manager.compose_session_key(&command.platform, chat_id, &command.user_id);
        manager
            .get_or_create_session(&command.platform, "permission-target", &command.user_id)
            .await;
        manager
            .add_message("test:permission-target", Message::user("switched history"))
            .await;
        approval::approve_session(&key, "previous permission");

        let mut pending = std::pin::pin!(gateway.route_message(&command));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                () = pause.entered.notified() => {}
                result = &mut pending => panic!("YOLO completed before its hook: {result:?}"),
            }
        })
        .await
        .expect("YOLO never reached its hook");
        let before = manager.get_session(&key).await.unwrap();
        let mut replacement = command.clone();
        replacement.text = transition.into();
        gateway.route_message(&replacement).await.unwrap();
        let after = manager.get_session(&key).await.unwrap();
        if transition == "/reset" {
            assert_ne!(after.id, before.id);
        } else {
            assert_eq!(after.id, before.id);
            assert_eq!(after.messages.len(), 1);
        }
        pause.resume.notify_one();
        pending.await.unwrap();
        let enabled = approval::is_session_yolo_enabled(&key);
        let approved = approval::is_approved(&key, "previous permission");
        approval::clear_session(&key);
        assert!(!enabled, "delayed YOLO enabled bypass after {transition}");
        assert!(!approved, "{transition} retained previous permissions");
        assert!(sent
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .1
            .contains("conversation changed"));
    }
}

#[tokio::test]
async fn switching_conversations_preserves_the_destination_title() {
    let manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("user1");
    let gateway = Gateway::new(manager.clone(), dm, GatewayConfig::default());
    let sent = Arc::new(Mutex::new(Vec::new()));
    gateway
        .register_adapter(
            "test",
            Arc::new(TestAdapter {
                messages: sent.clone(),
            }),
        )
        .await;
    let pause = Arc::new(PauseHook::default());
    let title_events = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process("command:title", pause.clone());
    hooks.register_in_process(
        "session:title",
        Arc::new(RecordingHook {
            seen: title_events.clone(),
        }),
    );
    gateway.set_hook_registry(Arc::new(hooks)).await;

    let mut command = test_incoming("/title Previous conversation");
    command.chat_id = "source-title".into();
    let current_key =
        manager.compose_session_key(&command.platform, &command.chat_id, &command.user_id);
    manager
        .get_or_create_session(&command.platform, "destination-title", &command.user_id)
        .await;
    manager
        .add_message(
            "test:destination-title",
            Message::user("destination history"),
        )
        .await;
    manager
        .set_title("test:destination-title", "Destination")
        .await;

    let mut pending = std::pin::pin!(gateway.route_message(&command));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            () = pause.entered.notified() => {}
            result = &mut pending => panic!("Title completed before its hook: {result:?}"),
        }
    })
    .await
    .expect("Title never reached its hook");
    let replacement = IncomingMessage {
        text: "/sessions test:destination-title".into(),
        ..command.clone()
    };
    gateway.route_message(&replacement).await.unwrap();
    pause.resume.notify_one();
    pending.await.unwrap();

    assert_eq!(
        manager.get_title(&current_key).await.as_deref(),
        Some("Destination")
    );
    assert!(title_events.lock().unwrap().is_empty());
    assert!(sent
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .1
        .contains("conversation changed"));
}

#[tokio::test]
async fn switching_conversations_retires_the_previous_browser_on_commit() {
    use hermes_tools::backends::browser::unavailable_browser_backend;
    use hermes_tools::session_browsers::{BrowserReleaseQueue, SessionBrowsers};

    let browsers = SessionBrowsers::new(|| unavailable_browser_backend("fixture browser"));
    let queue = BrowserReleaseQueue::start(browsers.clone());
    let retirer = queue.retirer();
    let manager = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm = DmManager::with_pair_behavior();
    dm.authorize_user("user1");
    let gateway = Gateway::new(manager.clone(), dm, GatewayConfig::default())
        .with_browser_retirement(retirer.clone());
    let sent = Arc::new(Mutex::new(Vec::new()));
    gateway
        .register_adapter(
            "test",
            Arc::new(TestAdapter {
                messages: sent.clone(),
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
    let mut incoming = test_incoming("Open original conversation");
    incoming.chat_id = "browser-switch-source".into();
    let key = manager.compose_session_key(&incoming.platform, &incoming.chat_id, &incoming.user_id);
    gateway.route_message(&incoming).await.unwrap();
    manager.set_title(&key, "Original").await;
    let original = turns.try_recv().unwrap();
    let snapshot_error = |lease: BrowserLease| async move {
        lease.tool_overrides()["browser_snapshot"]
            .execute(serde_json::json!({}))
            .await
            .unwrap_err()
            .to_string()
    };
    assert!(snapshot_error(original.clone())
        .await
        .contains("fixture browser"));

    let target = "browser-switch-destination";
    let target_key = manager.compose_session_key(&incoming.platform, target, &incoming.user_id);
    manager
        .get_or_create_session(&incoming.platform, target, &incoming.user_id)
        .await;
    manager
        .add_message(&target_key, Message::user("Destination history"))
        .await;
    manager.set_title(&target_key, "Destination").await;
    let switching = IncomingMessage {
        text: format!("/sessions {target_key}"),
        ..incoming.clone()
    };
    let sent_before_switch = sent.lock().unwrap().len();
    let mut slots = Vec::new();
    while let Some(slot) = retirer.try_reserve().unwrap() {
        slots.push(slot);
    }
    assert!(!slots.is_empty());
    {
        let mut pending = std::pin::pin!(gateway.route_message(&switching));
        assert!(futures::poll!(pending.as_mut()).is_pending());
    }
    assert_eq!(sent.lock().unwrap().len(), sent_before_switch);
    assert_eq!(manager.get_title(&key).await.as_deref(), Some("Original"));
    assert!(snapshot_error(original.clone())
        .await
        .contains("fixture browser"));

    drop(slots);
    gateway.route_message(&switching).await.unwrap();
    assert_eq!(
        manager.get_title(&key).await.as_deref(),
        Some("Destination")
    );
    let error = snapshot_error(original).await;
    assert!(error.contains("browser was closed"), "{error}");
    gateway.route_message(&incoming).await.unwrap();
    let current = turns.try_recv().unwrap();
    let error = snapshot_error(current).await;
    assert!(error.contains("fixture browser"), "{error}");
    browsers.close_all().await;
    queue.drain().await.unwrap();
}

#[tokio::test]
async fn delayed_approval_commands_cannot_resolve_the_replacement_conversation() {
    use hermes_tools::gateway_approval::{self, GatewayApprovalBinding, Resolution};

    for (chat_id, action) in [("delayed-approve", "/approve"), ("delayed-deny", "/deny")] {
        let manager = Arc::new(SessionManager::new(SessionConfig::default()));
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
        let pause = Arc::new(PauseHook::default());
        let mut hooks = HookRegistry::new();
        hooks.register_in_process(format!("command:{}", &action[1..]), pause.clone());
        gateway.set_hook_registry(Arc::new(hooks)).await;
        let mut command = test_incoming(action);
        command.chat_id = chat_id.into();
        let key = manager.compose_session_key(&command.platform, chat_id, &command.user_id);

        let mut pending = std::pin::pin!(gateway.route_message(&command));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                () = pause.entered.notified() => {}
                result = &mut pending => panic!("{action} completed before its hook: {result:?}"),
            }
        })
        .await
        .expect("approval command never reached its hook");
        let mut reset = command.clone();
        reset.text = "/reset".into();
        gateway.route_message(&reset).await.unwrap();
        let binding = GatewayApprovalBinding {
            session_key: key.clone(),
            user_id: command.user_id.clone(),
            generation: Default::default(),
            notify: gateway_approval::notify_with(|_| async { true }),
        };
        let fixture_command = "echo fixture";
        let error = gateway_approval::authorise(
            &binding,
            fixture_command,
            &serde_json::json!({"command": fixture_command}),
        )
        .await
        .unwrap_err();
        assert!(error.contains("shown it"), "{error}");

        pause.resume.notify_one();
        let result = pending.await;
        let resolution = gateway_approval::deny(&key, &command.user_id);
        gateway_approval::clear(&key);
        assert_eq!(
            resolution,
            Resolution::Resolved,
            "delayed {action} consumed the replacement approval"
        );
        result.unwrap();
    }
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
    assert_expiry_batch_retires_before_hooks(false).await;
    assert_expiry_batch_retires_before_hooks(true).await;
}

async fn assert_expiry_batch_retires_before_hooks(with_browser: bool) {
    use hermes_tools::backends::browser::unavailable_browser_backend;
    use hermes_tools::gateway_approval::{self, GatewayApprovalBinding, Resolution};
    use hermes_tools::session_browsers::{BrowserReleaseQueue, SessionBrowsers};

    let manager = Arc::new(SessionManager::new(SessionConfig {
        reset_policy: hermes_config::SessionResetPolicy::Idle { timeout_minutes: 0 },
        ..SessionConfig::default()
    }));
    let gateway = Gateway::new(
        manager.clone(),
        DmManager::with_pair_behavior(),
        GatewayConfig::default(),
    );
    let browsers = SessionBrowsers::new(|| unavailable_browser_backend("fixture browser"));
    let queue = with_browser.then(|| BrowserReleaseQueue::start(browsers.clone()));
    let gateway = if let Some(queue) = &queue {
        gateway.with_browser_retirement(queue.retirer())
    } else {
        gateway
    };
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
            generation: Default::default(),
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
        let remaining_sessions = manager.session_count().await;
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
        assert_eq!(remaining_sessions, 0);
    })
    .await
    .expect("expiry hooks failed to finish");
    if let Some(queue) = queue {
        queue.drain().await.unwrap();
    }
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
    let hooks_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    for event in ["session:end", "on_session_finalize"] {
        hooks.register_in_process(
            event,
            Arc::new(RecordingHook {
                seen: hooks_seen.clone(),
            }),
        );
    }
    gateway.set_hook_registry(Arc::new(hooks)).await;
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
    let unchanged = manager.get_session(&key).await.unwrap();
    assert_eq!(unchanged.id, before.id);
    assert_eq!(unchanged.last_active_at, before.last_active_at);

    let mut racing = std::pin::pin!(gateway.route_message(&reset));
    assert!(futures::poll!(racing.as_mut()).is_pending());
    assert!(
        manager
            .replace_messages_and_title(&key, before.messages.clone(), Some("replacement".into()))
            .await
    );
    let switched = manager.get_session(&key).await.unwrap();
    drop(slots.pop());
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), racing)
        .await
        .expect("reset kept waiting after release capacity was restored")
        .unwrap_err();
    assert!(error.to_string().contains("Session changed before reset"));

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
    assert_eq!(after.title.as_deref(), Some("replacement"));
    assert_eq!(after.last_active_at, switched.last_active_at);
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
    let events = hooks_seen.lock().unwrap();
    assert_eq!(
        events.len(),
        2,
        "an uncommitted reset announced finalisation: {events:?}"
    );
    assert!(events.iter().all(|(name, context)| {
        let key_field = if name == "session:end" {
            "session_id"
        } else {
            "session_key"
        };
        context[key_field] == "test:other"
    }));
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
    let gateway = Gateway::new(manager.clone(), dm, GatewayConfig::default())
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
    assert!(turns.try_recv().is_err(), "a stale turn reached the model");
    let shared = manager.get_messages("test:shared").await;
    assert_eq!(shared.len(), 2);
    assert_eq!(shared[0].content, Message::user("second reset").content);
    let snapshot_error = |browser: BrowserLease| async move {
        browser.tool_overrides()["browser_snapshot"]
            .execute(serde_json::json!({}))
            .await
            .unwrap_err()
            .to_string()
    };
    let error = snapshot_error(oldest).await;
    assert!(error.contains("browser was closed"), "{error}");
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
        async fn handle(&self, _event: &HookEvent) -> Result<(), String> {
            Err("finalisation did not supply a session snapshot".into())
        }

        async fn handle_finalized_session(
            &self,
            event: &HookEvent,
            snapshot: &Session,
        ) -> Result<(), String> {
            let key = event.context["session_key"].as_str().ok_or("missing key")?;
            let current = self
                .manager
                .get_session(key)
                .await
                .ok_or("missing session")?;
            assert_ne!(
                current.id, snapshot.id,
                "hook called before reset committed"
            );
            assert!(current.messages.is_empty());
            self.snapshots.lock().unwrap().push(snapshot.clone());
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
    assert_eq!(finalize_payload.as_object().unwrap().len(), 6);
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
