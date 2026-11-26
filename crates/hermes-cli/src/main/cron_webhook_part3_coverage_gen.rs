        impl WebhookArgs<'_> {
            async fn run(self, state: &Path) -> Result<(), AgentError> {
                run_webhook(
                    cli_for_state(state),
                    self.action.map(str::to_owned),
                    self.name.map(str::to_owned),
                    self.url.map(str::to_owned),
                    self.id.map(str::to_owned),
                    self.prompt.map(str::to_owned),
                    self.events.map(str::to_owned),
                    self.description.map(str::to_owned),
                    self.skills.map(str::to_owned),
                    self.deliver.map(str::to_owned),
                    self.chat_id.map(str::to_owned),
                    self.secret.map(str::to_owned),
                    self.deliver_only,
                    None,
                )
                .await
            }
        }

        #[tokio::test]
        async fn webhook_commands_manage_both_local_stores_without_sending_requests() {
            const SECRET: &str = "fixture-secret-not-a-credential";
            const FIRST_URL: &str = "https://first.example.invalid/callback";
            const SECOND_URL: &str = "http://second.example.invalid/callback";

            let Some(state) = isolated_state(&[], |stdout| {
                for expected in [
                    "(no webhooks in ",
                    "Webhook subscriptions (",
                    "events=(all)",
                    "events=push, release",
                    "Webhooks (",
                    FIRST_URL,
                    SECOND_URL,
                    "No matching webhook removed.",
                    "Removed subscription 'build-alerts'.",
                ] {
                    assert!(
                        stdout.contains(expected),
                        "missing {expected:?} in {stdout}"
                    );
                }
                assert!(stdout.contains(&format!("Secret: {}", mask_secret(SECRET))));
                assert!(!stdout.contains(SECRET));
            }) else {
                return;
            };
            let cli = cli_for_state(&state);
            let store_path = webhook_store_path(&cli);
            let subs_path = webhook_subscriptions_path(&cli);
            assert_eq!(store_path, hermes_state_root(&cli).join("webhooks.json"));
            assert_eq!(
                subs_path,
                hermes_state_root(&cli).join("webhook_subscriptions.json")
            );

            WebhookArgs::default().run(&state).await.unwrap();
            WebhookArgs {
                action: Some("subscribe"),
                name: Some("  BUILD Alerts  "),
                prompt: Some("summarize build"),
                events: Some(" push , , release, "),
                description: Some("Build notifications"),
                skills: Some(" build, , review "),
                deliver: Some("slack"),
                chat_id: Some("room-42"),
                secret: Some(SECRET),
                deliver_only: true,
                ..WebhookArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            WebhookArgs {
                action: Some("subscribe"),
                name: Some("Audit"),
                chat_id: Some(" "),
                ..WebhookArgs::default()
            }
            .run(&state)
            .await
            .unwrap();

            let subs = load_webhook_subscriptions(&subs_path).unwrap();
            let build = &subs["build-alerts"];
            assert_eq!(build.description, "Build notifications");
            assert_eq!(build.secret, SECRET);
            assert_eq!(build.events, ["push", "release"]);
            assert_eq!(build.skills, ["build", "review"]);
            assert_eq!(build.prompt, "summarize build");
            assert_eq!(build.deliver, "slack");
            assert!(build.deliver_only);
            assert_eq!(
                build.deliver_extra.as_ref().unwrap(),
                &serde_json::json!({"chat_id": "room-42"})
            );
            let audit = &subs["audit"];
            assert!(uuid::Uuid::parse_str(&audit.secret).is_ok());
            assert_eq!(audit.description, "Agent-created subscription: audit");
            assert_eq!(audit.deliver, "log");
            assert!(audit.events.is_empty());
            assert!(audit.deliver_extra.is_none());

            for url in [FIRST_URL, SECOND_URL] {
                WebhookArgs {
                    action: Some("add"),
                    url: Some(url),
                    ..WebhookArgs::default()
                }
                .run(&state)
                .await
                .unwrap();
            }
            let store = hermes_cli::webhook_delivery::load_webhook_store(&store_path).unwrap();
            assert_eq!(store.webhooks.len(), 2);
            assert_eq!(store.webhooks[0].url, FIRST_URL);
            assert_eq!(store.webhooks[1].url, SECOND_URL);

            WebhookArgs {
                action: Some("ls"),
                ..WebhookArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            let before_noop = fs::read(&store_path).unwrap();
            WebhookArgs {
                action: Some("rm"),
                id: Some("missing"),
                ..WebhookArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            assert_eq!(fs::read(&store_path).unwrap(), before_noop);

            // Removing a subscription takes precedence over removing an outbound URL.
            WebhookArgs {
                action: Some("remove"),
                name: Some("build-alerts"),
                url: Some(FIRST_URL),
                ..WebhookArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            let remaining = load_webhook_subscriptions(&subs_path).unwrap();
            assert_eq!(remaining.len(), 1);
            assert!(remaining.contains_key("audit"));
            assert_eq!(fs::read(&store_path).unwrap(), before_noop);

            WebhookArgs {
                action: Some("remove"),
                id: Some(&store.webhooks[0].id),
                ..WebhookArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            let remaining = hermes_cli::webhook_delivery::load_webhook_store(&store_path).unwrap();
            assert_eq!(remaining.webhooks.len(), 1);
            assert_eq!(remaining.webhooks[0].url, SECOND_URL);

            WebhookArgs {
                action: Some("rm"),
                url: Some(SECOND_URL),
                ..WebhookArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            assert!(
                hermes_cli::webhook_delivery::load_webhook_store(&store_path)
                    .unwrap()
                    .webhooks
                    .is_empty()
            );
        }

        #[tokio::test]
        async fn webhook_validation_does_not_write_to_either_store() {
            let Some(state) = isolated_state(&[], |_| {}) else {
                return;
            };

            for (args, error) in [
                (
                    WebhookArgs {
                        action: Some("subscribe"),
                        ..WebhookArgs::default()
                    },
                    "missing route name",
                ),
                (
                    WebhookArgs {
                        action: Some("subscribe"),
                        name: Some("  "),
                        ..WebhookArgs::default()
                    },
                    "route name cannot be empty",
                ),
                (
                    WebhookArgs {
                        action: Some("subscribe"),
                        name: Some("audit"),
                        deliver_only: true,
                        ..WebhookArgs::default()
                    },
                    "--deliver-only requires --deliver",
                ),
                (
                    WebhookArgs {
                        action: Some("add"),
                        url: Some(""),
                        ..WebhookArgs::default()
                    },
                    "webhook add: use --url",
                ),
                (
                    WebhookArgs {
                        action: Some("add"),
                        url: Some("ftp://example.invalid/hook"),
                        ..WebhookArgs::default()
                    },
                    "webhook URL must start with http:// or https://",
                ),
                (
                    WebhookArgs {
                        action: Some("remove"),
                        name: Some("missing"),
                        ..WebhookArgs::default()
                    },
                    "webhook remove: use <name>",
                ),
                (
                    WebhookArgs {
                        action: Some("test"),
                        ..WebhookArgs::default()
                    },
                    "webhook test: usage",
                ),
                (
                    WebhookArgs {
                        action: Some("test"),
                        name: Some("missing"),
                        ..WebhookArgs::default()
                    },
                    "No subscription named 'missing'",
                ),
                (
                    WebhookArgs {
                        action: Some("unknown"),
                        ..WebhookArgs::default()
                    },
                    "Unknown webhook action: unknown",
                ),
            ] {
                assert_config_error(args.run(&state).await, error);
            }

            let cli = cli_for_state(&state);
            assert!(!webhook_store_path(&cli).exists());
            assert!(!webhook_subscriptions_path(&cli).exists());
        }

        #[tokio::test]
        async fn webhook_subscription_can_explicitly_reveal_its_fixture_secret() {
            const SECRET: &str = "fixture-secret-reveal-123";
            let Some(state) = isolated_state(&[("HERMES_ALLOW_SECRET_STDOUT", "1")], |stdout| {
                assert!(stdout.contains(&format!("Secret: {SECRET}")));
                assert!(
                    stdout.contains("plaintext output enabled via HERMES_ALLOW_SECRET_STDOUT=1")
                );
            }) else {
                return;
            };

            WebhookArgs {
                action: Some("subscribe"),
                name: Some("sample"),
                secret: Some(SECRET),
                ..WebhookArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            let path = webhook_subscriptions_path(&cli_for_state(&state));
            assert_eq!(
                load_webhook_subscriptions(&path).unwrap()["sample"].secret,
                SECRET
            );
        }

        #[tokio::test]
        async fn llm_login_uses_the_provider_environment_key_without_prompting() {
            const KEY: &str = "test-openai-key-not-a-credential";
            let Some(state) = isolated_state(&[("OPENAI_API_KEY", KEY)], |_| {}) else {
                return;
            };
            let token = resolve_llm_login_token(&cli_for_state(&state), "openai")
                .await
                .unwrap();
            assert_eq!(token, KEY);
        }

        #[test]
        fn completion_writes_the_requested_shell_scripts_and_enhances_zsh() {
            if isolated_state(&[], |stdout| {
                for shell in [
                    CompletionShell::Bash,
                    CompletionShell::Fish,
                    CompletionShell::PowerShell,
                    CompletionShell::Elvish,
                ] {
                    let mut expected = Vec::new();
                    generate(
                        shell,
                        &mut Cli::command(),
                        "hermes-agent-ultra",
                        &mut expected,
                    );
                    let expected = String::from_utf8(expected).unwrap();
                    assert!(!expected.is_empty());
                    assert!(
                        stdout.contains(expected.as_str()),
                        "missing {shell:?} script"
                    );
                }
                assert_eq!(stdout.matches("#compdef hermes-agent-ultra").count(), 2);
                assert_eq!(
                    stdout
                        .matches("_hermes_agent_ultra_model_values() {")
                        .count(),
                    2
                );
                assert_eq!(
                    stdout
                        .matches("_hermes_agent_ultra_provider_values() {")
                        .count(),
                    2
                );
            })
            .is_none()
            {
                return;
            }

            for shell in [
                Some("bash"),
                Some("fish"),
                Some("powershell"),
                Some("elvish"),
                None,
                Some("other"),
            ] {
                run_completion(shell.map(str::to_owned)).unwrap();
            }
        }

        #[cfg(unix)]
        #[tokio::test]
        async fn uninstall_command_previews_then_removes_only_the_isolated_state() {
            let Some(state) = isolated_state(&[], |stdout| {
                assert!(stdout.contains("Uninstall would remove "));
                assert!(stdout.contains("Removed "));
                assert!(stdout.contains("Nothing to uninstall: "));
            }) else {
                return;
            };
            assert_eq!(hermes_config::hermes_home(), state);
            let sentinel = state.join("sentinel");
            fs::write(&sentinel, "keep on dry run").unwrap();

            run_uninstall(false).await.unwrap();
            assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep on dry run");
            run_uninstall(true).await.unwrap();
            assert!(!state.exists());
            run_uninstall(true).await.unwrap();
            assert!(!state.exists());
        }

        #[cfg(unix)]
        #[tokio::test]
        async fn lumio_reports_empty_status_and_accepts_idempotent_logout() {
            let Some(state) = isolated_state(&[], |stdout| {
                assert_eq!(stdout.matches("Lumio: not logged in").count(), 2);
                assert!(stdout.contains("Lumio token removed."));
                assert!(stdout.contains("Unknown lumio action: 'unexpected'"));
            }) else {
                return;
            };
            assert_eq!(hermes_config::hermes_home(), state);
            for action in ["status", "logout", "status", "unexpected"] {
                run_lumio(Some(action.to_owned()), None).await.unwrap();
            }
        }
