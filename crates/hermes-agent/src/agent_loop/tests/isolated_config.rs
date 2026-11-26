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
