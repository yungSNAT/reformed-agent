    fn run_isolated_agent_test(test_name: &str) -> bool {
        const CHILD: &str = "REFORMED_AGENT_FIXTURE_CHILD";
        if std::env::var(CHILD).as_deref() == Ok(test_name) {
            return false;
        }

        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
        command
            .args(["--exact", test_name])
            .current_dir(home.path())
            .env_clear()
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("HERMES_HOME", home.path())
            .env("TEMP", home.path())
            .env("TMP", home.path())
            .env(CHILD, test_name);
        for name in ["LLVM_PROFILE_FILE", "SystemRoot"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let output = command.output().expect("run isolated agent fixture");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed;"),
            "{test_name}:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    #[test]
    fn hook_context_preserves_the_full_payload_when_spilling_or_falling_back() {
        if run_isolated_agent_test(
            "agent_loop::tests::hook_context_preserves_the_full_payload_when_spilling_or_falling_back",
        ) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let agent_for = |home: &Path| {
            AgentLoop::new(
                AgentConfig {
                    hermes_home: Some(home.display().to_string()),
                    ..AgentConfig::default()
                },
                Arc::new(ToolRegistry::new()),
                Arc::new(DummyProvider),
            )
        };
        let agent = agent_for(root.path());
        let large = format!("{}fixture-tail", "ø".repeat(13_000));
        let mut context = ContextManager::default_budget();
        agent.inject_hook_context(
            &[
                HookResult::InjectContext(" \n\t ".into()),
                HookResult::InjectContext("  small note  ".into()),
                HookResult::InjectContext(format!(" {large}\n")),
            ],
            &mut context,
        );
        let [inline, spilled] = context.get_messages() else {
            panic!("expected one inline and one spilled note");
        };
        assert_eq!(inline.role, MessageRole::System);
        assert_eq!(inline.content.as_deref(), Some("small note"));
        assert_eq!(spilled.role, MessageRole::System);
        let (file, preview) = spilled
            .content
            .as_deref()
            .unwrap()
            .strip_prefix("Hook context was oversized and spilled to disk.\nspill_path=")
            .unwrap()
            .split_once("\npreview:\n")
            .unwrap();
        let file = Path::new(file);
        assert_eq!(
            file.parent(),
            Some(root.path().join("hooks").join("spills").as_path())
        );
        assert_eq!(std::fs::read_to_string(file).unwrap(), large);
        assert!(preview.chars().count() <= 720);
        let (head, omitted) = preview.split_once("\n...[truncated ").unwrap();
        let omitted: usize = omitted.strip_suffix(" chars]...").unwrap().parse().unwrap();
        assert!(head.starts_with('ø'));
        assert!(large.starts_with(head));
        assert_eq!(head.chars().count() + omitted, large.chars().count());

        let blocked = root.path().join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("hooks"), b"fixture file").unwrap();
        let mut context = ContextManager::default_budget();
        agent_for(&blocked).inject_hook_context(
            &[HookResult::InjectContext(large.clone())],
            &mut context,
        );
        let [inline] = context.get_messages() else {
            panic!("expected the note to remain inline when spilling fails");
        };
        assert_eq!(inline.role, MessageRole::System);
        assert_eq!(inline.content.as_deref(), Some(large.as_str()));
    }

    #[test]
    fn compaction_checkpoint_obeys_isolation_and_reports_write_results() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        if run_isolated_agent_test(
            "agent_loop::tests::compaction_checkpoint_obeys_isolation_and_reports_write_results",
        ) {
            return;
        }

        let _timeout = EnvVarGuard::set("HERMES_CONTEXTLATTICE_TIMEOUT_SECS", "5");
        let home = tempfile::tempdir().unwrap();

        tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let server = MockServer::start().await;
                    let _url = EnvVarGuard::set("CONTEXTLATTICE_ORCHESTRATOR_URL", &server.uri());
                    for (mode, isolated, code, requests, expected_event) in [
                        ("advisory", true, 204, 0, None),
                        ("off", false, 204, 0, None),
                        (
                            "advisory",
                            false,
                            204,
                            1,
                            Some("ContextLattice compaction checkpoint written (75% -> 20%)."),
                        ),
                        ("advisory", false, 503, 1, None),
                        (
                            "enforce",
                            false,
                            503,
                            1,
                            Some("Compaction governance enforce-mode: checkpoint failed: HTTP 503 Service Unavailable: Fixture storage unavailable"),
                        ),
                    ] {
                        let _mode = EnvVarGuard::set("HERMES_CONTEXTLATTICE_COMPACTION_GOVERNANCE", mode);
                        Mock::given(method("POST"))
                            .and(path("/memory/write"))
                            .and(body_json(json!({
                                "projectName": "hermes-agent-ultra",
                                "fileName": "notes/hermes-agent-compaction.md",
                                "topicPath": "runbooks/alpha/compaction/fixture-session",
                                "content": format!("compaction_event mode={mode} session=fixture-session before_chars=150000 after_chars=40000 max_chars=200000 pressure_before_pct=75 pressure_after_pct=20")
                            })))
                            .respond_with(if code == 204 {
                                ResponseTemplate::new(code)
                            } else {
                                ResponseTemplate::new(code).set_body_string("Fixture storage unavailable")
                            })
                            .expect(requests as u64)
                            .mount(&server)
                            .await;
                        let events = Arc::new(Mutex::new(Vec::new()));
                        let captured = Arc::clone(&events);
                        let agent = AgentLoop::new(
                            AgentConfig {
                                isolated,
                                session_id: Some("fixture-session".to_string()),
                            hermes_home: Some(home.path().to_str().unwrap().to_string()),
                                ..AgentConfig::default()
                            },
                            Arc::new(ToolRegistry::new()),
                            Arc::new(DummyProvider),
                        )
                        .with_callbacks(AgentCallbacks {
                            status_callback: Some(Arc::new(move |kind, message| {
                                captured.lock().unwrap().push((kind.to_owned(), message.to_owned()));
                            })),
                            ..AgentCallbacks::default()
                        });
                        tokio::task::spawn_blocking(move || {
                            agent.emit_compaction_contextlattice_checkpoint(150_000, 40_000, 200_000);
                        })
                        .await
                        .unwrap();

                        server.verify().await;
                        assert_eq!(server.received_requests().await.unwrap().len(), requests);
                        let expected: Vec<_> = expected_event
                            .into_iter()
                            .map(|message| ("lifecycle".to_string(), message.to_string()))
                            .collect();
                        assert_eq!(*events.lock().unwrap(), expected, "mode={mode}, isolated={isolated}, status={code}");
                        server.reset().await;
                    }
                });
    }
