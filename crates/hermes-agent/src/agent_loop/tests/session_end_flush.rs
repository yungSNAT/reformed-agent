struct SessionTrackingHook {
    session: Mutex<String>,
    captured: Arc<Mutex<Vec<String>>>,
}

impl crate::memory_manager::MemoryProviderPlugin for SessionTrackingHook {
    fn name(&self) -> &str {
        "test-session-tracking"
    }

    fn on_session_switch(&self, session: &str, _parent: &str, _reset: bool) {
        *self.session.lock().unwrap() = session.into();
    }

    fn prepare_session_end(
        &self,
        _messages: &[Value],
    ) -> Option<futures::future::BoxFuture<'static, ()>> {
        let session = self.session.lock().unwrap().clone();
        let captured = self.captured.clone();
        Some(Box::pin(
            async move { captured.lock().unwrap().push(session) },
        ))
    }
}

fn agent_with_session_end_hook(
    hook: Arc<dyn crate::memory_manager::MemoryProviderPlugin>,
) -> (AgentLoop, Arc<Mutex<MemoryManager>>) {
    let mut manager = MemoryManager::new();
    manager.add_provider(hook);
    let manager = Arc::new(Mutex::new(manager));
    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(RecordingProvider::default()),
    )
    .with_memory(manager.clone());
    (agent, manager)
}

#[tokio::test]
async fn session_end_flush_captures_session_before_a_switch() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let (agent, manager) = agent_with_session_end_hook(Arc::new(SessionTrackingHook {
        session: Mutex::new("ending-session".into()),
        captured: captured.clone(),
    }));
    let result = AgentLoop::complete_memory_flush(async {
        agent.memory_on_session_end(&[]);
        manager
            .lock()
            .unwrap()
            .on_session_switch("new-session", "ending-session", false);
        Err(AgentError::MaxTurnsExceeded)
    })
    .await;
    assert!(matches!(result, Err(AgentError::MaxTurnsExceeded)));
    assert_eq!(*captured.lock().unwrap(), ["ending-session"]);
}

#[tokio::test]
async fn session_end_flush_survives_caller_cancellation() {
    use tokio::sync::Notify;
    use tokio::time::{timeout, Duration};

    #[derive(Default)]
    struct FlushSignals {
        started: Notify,
        release: Notify,
        completed: Notify,
    }

    struct DelayedHook(Arc<FlushSignals>);
    impl crate::memory_manager::MemoryProviderPlugin for DelayedHook {
        fn name(&self) -> &str {
            "test-delayed-flush"
        }

        fn prepare_session_end(
            &self,
            _messages: &[Value],
        ) -> Option<futures::future::BoxFuture<'static, ()>> {
            let signals = self.0.clone();
            Some(Box::pin(async move {
                signals.started.notify_one();
                signals.release.notified().await;
                signals.completed.notify_one();
            }))
        }
    }

    let signals = Arc::new(FlushSignals::default());
    let (agent, _manager) = agent_with_session_end_hook(Arc::new(DelayedHook(signals.clone())));
    let run = tokio::spawn(async move {
        AgentLoop::complete_memory_flush(async {
            agent.memory_on_session_end(&[]);
            Err(AgentError::MaxTurnsExceeded)
        })
        .await
    });
    timeout(Duration::from_secs(2), signals.started.notified())
        .await
        .expect("flush started");
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    signals.release.notify_one();
    timeout(Duration::from_secs(2), signals.completed.notified())
        .await
        .expect("the session-end job completed after its caller was cancelled");
}

#[tokio::test]
async fn session_end_flush_does_not_send_a_stale_legacy_transcript() {
    #[derive(Default)]
    struct LegacyHook(Mutex<Vec<Value>>);

    impl crate::memory_manager::MemoryProviderPlugin for LegacyHook {
        fn name(&self) -> &str {
            "test-legacy-flush"
        }

        fn on_session_end(&self, messages: &[Value]) {
            self.0.lock().unwrap().extend_from_slice(messages);
        }
    }

    let hook = Arc::new(LegacyHook::default());
    let (_agent, manager) = agent_with_session_end_hook(hook.clone());
    let stale = MemoryManager::session_end_tasks(&manager, &[Message::user("stale transcript")]);
    manager
        .lock()
        .unwrap()
        .on_session_switch("new-session", "ending-session", false);
    for task in stale {
        task.await;
    }
    assert!(hook.0.lock().unwrap().is_empty());

    let current =
        MemoryManager::session_end_tasks(&manager, &[Message::user("current transcript")]);
    for task in current {
        task.await;
    }
    let observed = hook.0.lock().unwrap();
    assert_eq!(observed[0]["content"], "current transcript");
}

#[tokio::test]
async fn session_end_flush_waits_for_every_queued_job() {
    use tokio::sync::Notify;
    use tokio::time::{timeout, Duration};

    #[derive(Default)]
    struct FlushSignals {
        first_finished: Notify,
        second_started: Notify,
        release: Notify,
    }

    struct PairedHook(Arc<FlushSignals>);
    impl crate::memory_manager::MemoryProviderPlugin for PairedHook {
        fn name(&self) -> &str {
            "test-paired-flush"
        }

        fn prepare_session_end(
            &self,
            messages: &[Value],
        ) -> Option<futures::future::BoxFuture<'static, ()>> {
            let signals = self.0.clone();
            let first = messages.is_empty();
            Some(Box::pin(async move {
                if first {
                    signals.first_finished.notify_one();
                } else {
                    signals.second_started.notify_one();
                    signals.release.notified().await;
                }
            }))
        }
    }

    let signals = Arc::new(FlushSignals::default());
    let (agent, _manager) = agent_with_session_end_hook(Arc::new(PairedHook(signals.clone())));
    let mut run = std::pin::pin!(AgentLoop::complete_memory_flush(async {
        agent.memory_on_session_end(&[]);
        agent.memory_on_session_end(&[Message::user("second")]);
        Err(AgentError::MaxTurnsExceeded)
    }));
    let ready = timeout(Duration::from_secs(2), async {
        signals.first_finished.notified().await;
        signals.second_started.notified().await;
    });
    tokio::select! {
        biased;
        _ = &mut run => panic!("completion returned before the second flush"),
        result = ready => result.expect("both jobs started"),
    }
    assert!(futures::poll!(&mut run).is_pending());
    signals.release.notify_one();
    assert!(matches!(run.await, Err(AgentError::MaxTurnsExceeded)));
}
