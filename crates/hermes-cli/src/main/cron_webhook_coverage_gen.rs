// Keep the requested test filter working when cron_webhook.rs is included
// directly into main rather than compiled as a named module.
mod cron_webhook {
    mod coverage_gen {
        use super::super::super::*;
        use std::fs;

        const CHILD_STATE: &str = "HERMES_TEST_CRON_WEBHOOK_COVERAGE_STATE";

        // Give the child an isolated home and capture stdout without changing
        // the process environment shared by the other tests.
        fn isolated_state(
            env: &[(&str, &str)],
            check_stdout: impl FnOnce(&str),
        ) -> Option<PathBuf> {
            if let Some(path) = std::env::var_os(CHILD_STATE) {
                return Some(PathBuf::from(path));
            }

            let sandbox = tempfile::tempdir().unwrap();
            let home = sandbox.path().join("user");
            let state = sandbox.path().join("state");
            fs::create_dir(&home).unwrap();
            fs::create_dir(&state).unwrap();

            let thread = std::thread::current();
            let name = thread.name().expect("libtest test thread must be named");
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", name, "--nocapture"])
                .current_dir(sandbox.path())
                .env_clear()
                .env(CHILD_STATE, &state)
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .env("HERMES_HOME", &state)
                .env("XDG_CONFIG_HOME", home.join("config"))
                .env("XDG_DATA_HOME", home.join("data"))
                .env("XDG_STATE_HOME", home.join("state"))
                .env("TMPDIR", sandbox.path())
                .env("TEMP", sandbox.path())
                .env("TMP", sandbox.path())
                .env("HERMES_ALLOW_SECRET_STDOUT", "0")
                .envs(env.iter().copied());
            for key in ["LLVM_PROFILE_FILE", "SystemRoot"] {
                if let Some(value) = std::env::var_os(key) {
                    child.env(key, value);
                }
            }
            let output = child.output().unwrap();
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(
                output.status.success() && stdout.contains("running 1 test"),
                "isolated test {name} failed ({})\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            check_stdout(&stdout);
            None
        }

        fn cli_for_state(state: &Path) -> Cli {
            let mut cli = <Cli as clap::Parser>::try_parse_from(["hermes-agent-ultra"]).unwrap();
            cli.config_dir = Some(state.to_str().unwrap().into());
            assert!(hermes_state_root(&cli).starts_with(state));
            cli
        }

        fn assert_config_error<T: std::fmt::Debug>(result: Result<T, AgentError>, expected: &str) {
            let err = result.unwrap_err();
            assert!(
                matches!(&err, AgentError::Config(message) if message.contains(expected)),
                "expected configuration error containing {expected:?}, got {err:?}"
            );
        }

        fn assert_io_error<T: std::fmt::Debug>(result: Result<T, AgentError>, expected: &str) {
            let err = result.unwrap_err();
            assert!(
                matches!(&err, AgentError::Io(message) if message.contains(expected)),
                "expected I/O error containing {expected:?}, got {err:?}"
            );
        }

        #[test]
        fn delivery_aliases_and_platform_names_agree() {
            use hermes_cron::DeliverTarget as Target;

            for (raw, target, platform) in [
                ("origin", Target::Origin, None),
                ("local", Target::Local, None),
                ("telegram", Target::Telegram, Some("telegram")),
                ("discord", Target::Discord, Some("discord")),
                ("slack", Target::Slack, Some("slack")),
                ("email", Target::Email, Some("email")),
                ("whatsapp", Target::WhatsApp, Some("whatsapp")),
                ("signal", Target::Signal, Some("signal")),
                ("matrix", Target::Matrix, Some("matrix")),
                ("mattermost", Target::Mattermost, Some("mattermost")),
                ("dingtalk", Target::DingTalk, Some("dingtalk")),
                ("feishu", Target::Feishu, Some("feishu")),
                ("wecom", Target::WeCom, Some("wecom")),
                ("weixin", Target::Weixin, Some("weixin")),
                ("wechat", Target::Weixin, Some("weixin")),
                ("wx", Target::Weixin, Some("weixin")),
                ("bluebubbles", Target::BlueBubbles, Some("bluebubbles")),
                ("imessage", Target::BlueBubbles, Some("bluebubbles")),
                ("sms", Target::Sms, Some("sms")),
                (
                    "homeassistant",
                    Target::HomeAssistant,
                    Some("homeassistant"),
                ),
                ("ha", Target::HomeAssistant, Some("homeassistant")),
                ("ntfy", Target::Ntfy, Some("ntfy")),
            ] {
                let spec = format!(" {} :  destination:42  ", raw.to_ascii_uppercase());
                let cfg = parse_deliver_config(&spec, None).unwrap();
                assert_eq!(
                    std::mem::discriminant(&cfg.target),
                    std::mem::discriminant(&target),
                    "wrong target for {raw}"
                );
                assert_eq!(cfg.platform.as_deref(), Some("destination:42"));
                assert_eq!(cron_deliver_target_platform_name(&cfg.target), platform);
            }

            let cfg = parse_deliver_config("email:inline", Some(" explicit ")).unwrap();
            assert_eq!(cfg.platform.as_deref(), Some("explicit"));
            let cfg = parse_deliver_config("email:inline", Some(" ")).unwrap();
            assert!(cfg.platform.is_none());
            assert!(parse_deliver_config("not-a-platform:room", None).is_none());
        }

        #[test]
        fn home_channel_keys_and_telegram_thread_fallback() {
            for (platform, key) in [
                ("telegram", "TELEGRAM_HOME_CHANNEL"),
                ("discord", "DISCORD_HOME_CHANNEL"),
                ("slack", "SLACK_HOME_CHANNEL"),
                ("email", "EMAIL_HOME_CHANNEL"),
                ("whatsapp", "WHATSAPP_HOME_CHANNEL"),
                ("signal", "SIGNAL_HOME_CHANNEL"),
                ("mattermost", "MATTERMOST_HOME_CHANNEL"),
                ("dingtalk", "DINGTALK_HOME_CHANNEL"),
                ("feishu", "FEISHU_HOME_CHANNEL"),
                ("wecom", "WECOM_HOME_CHANNEL"),
                ("weixin", "WEIXIN_HOME_CHANNEL"),
                ("bluebubbles", "BLUEBUBBLES_HOME_CHANNEL"),
                ("sms", "SMS_HOME_CHANNEL"),
                ("homeassistant", "HOMEASSISTANT_HOME_CHANNEL"),
                ("ntfy", "NTFY_HOME_CHANNEL"),
            ] {
                assert_eq!(cron_home_channel_env_vars(platform), &[key]);
            }
            assert_eq!(
                cron_home_channel_env_vars("matrix"),
                &["MATRIX_HOME_ROOM", "MATRIX_HOME_CHANNEL"]
            );
            assert!(cron_home_channel_env_vars("unknown").is_empty());

            if isolated_state(
                &[
                    ("TELEGRAM_CRON_THREAD_ID", "73"),
                    ("MATRIX_HOME_ROOM", "!preferred:example.invalid"),
                    ("MATRIX_HOME_CHANNEL", "!fallback:example.invalid"),
                ],
                |_| {},
            )
            .is_none()
            {
                return;
            }

            assert_eq!(
                cron_home_channel_for_platform("matrix").as_deref(),
                Some("!preferred:example.invalid")
            );
            assert_eq!(
                split_telegram_cron_target(" -100 : 42 "),
                ("-100".to_owned(), Some("42".to_owned()))
            );
            assert_eq!(
                split_telegram_cron_target("  room:invalid  "),
                ("room:invalid".to_owned(), Some("73".to_owned()))
            );
            assert_eq!(
                split_telegram_cron_target(" :42 "),
                (":42".to_owned(), Some("73".to_owned()))
            );
        }

        #[derive(Default)]
        struct CronArgs<'a> {
            action: Option<&'a str>,
            job_id: Option<&'a str>,
            id: Option<&'a str>,
            schedule: Option<&'a str>,
            prompt: Option<&'a str>,
            name: Option<&'a str>,
            deliver: Option<&'a str>,
            chat_id: Option<&'a str>,
            repeat: Option<u32>,
            skills: &'a [&'a str],
            add_skills: &'a [&'a str],
            remove_skills: &'a [&'a str],
            clear_skills: bool,
            script: Option<&'a str>,
            no_agent: bool,
            agent: bool,
            timeout: Option<u64>,
            shell: Option<&'a str>,
            workdir: Option<&'a str>,
        }

        impl CronArgs<'_> {
            async fn run(self, state: &Path) -> Result<(), AgentError> {
                let owned = |values: &[&str]| values.iter().copied().map(str::to_owned).collect();
                run_cron(
                    cli_for_state(state),
                    self.action.map(str::to_owned),
                    self.job_id.map(str::to_owned),
                    self.id.map(str::to_owned),
                    self.schedule.map(str::to_owned),
                    self.prompt.map(str::to_owned),
                    self.name.map(str::to_owned),
                    self.deliver.map(str::to_owned),
                    self.chat_id.map(str::to_owned),
                    self.repeat,
                    owned(self.skills),
                    owned(self.add_skills),
                    owned(self.remove_skills),
                    self.clear_skills,
                    self.script.map(str::to_owned),
                    self.no_agent,
                    self.agent,
                    self.timeout,
                    self.shell.map(str::to_owned),
                    self.workdir.map(str::to_owned),
                    false,
                )
                .await
            }
        }

        fn cron_data_dir(state: &Path) -> PathBuf {
            hermes_state_root(&cli_for_state(state)).join("cron")
        }

        async fn persisted_jobs(state: &Path) -> Vec<hermes_cron::CronJob> {
            let scheduler = cron_scheduler_for_data_dir(cron_data_dir(state));
            scheduler.load_persisted_jobs().await.unwrap();
            scheduler.list_jobs().await
        }

        async fn sole_job(state: &Path) -> hermes_cron::CronJob {
            let mut jobs = persisted_jobs(state).await;
            assert_eq!(jobs.len(), 1);
            jobs.pop().unwrap()
        }

        #[tokio::test]
        async fn cron_job_lifecycle_persists_edits_and_status() {
            let Some(state) = isolated_state(&[], |stdout| {
                for expected in [
                    "(no cron jobs in ",
                    "Created cron job id=",
                    "Updated job ",
                    "Paused job ",
                    "Cron jobs (",
                    "No due jobs at ",
                    "jobs_total=1 active=0",
                    "\"prompt\": \"updated prompt\"",
                    "Resumed job ",
                    "Deleted job ",
                ] {
                    assert!(
                        stdout.contains(expected),
                        "missing {expected:?} in {stdout}"
                    );
                }
            }) else {
                return;
            };

            CronArgs::default().run(&state).await.unwrap();

            let script = state.join("fixture.sh");
            let replacement_script = state.join("replacement.sh");
            fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
            fs::write(&replacement_script, "#!/bin/sh\nexit 0\n").unwrap();
            let script = script.to_str().unwrap();
            let replacement_script = replacement_script.to_str().unwrap();
            let workdir = state.to_str().unwrap();

            CronArgs {
                action: Some("add"),
                name: Some("initial name"),
                deliver: Some("EMAIL: inbox"),
                repeat: Some(3),
                skills: &["original"],
                script: Some(script),
                no_agent: true,
                timeout: Some(11),
                shell: Some("  sh  "),
                workdir: Some(workdir),
                ..CronArgs::default()
            }
            .run(&state)
            .await
            .unwrap();

            let job = sole_job(&state).await;
            let id = job.id.as_str();
            assert_eq!(job.schedule, "0 * * * *");
            assert_eq!(job.prompt, "[script-only cron job]");
            assert_eq!(job.name.as_deref(), Some("initial name"));
            assert_eq!(
                job.deliver.as_ref().unwrap().platform.as_deref(),
                Some("inbox")
            );
            assert_eq!(job.repeat, Some(3));
            assert_eq!(job.skills.as_deref().unwrap(), ["original"]);
            assert_eq!(job.script.as_deref(), Some(script));
            assert!(job.no_agent);
            assert_eq!(job.script_timeout_seconds, Some(11));
            assert_eq!(job.script_shell.as_deref(), Some("sh"));
            assert_eq!(job.workdir.as_deref(), Some(workdir));

            assert_config_error(
                CronArgs {
                    action: Some("edit"),
                    id: Some(id),
                    script: Some(" "),
                    ..CronArgs::default()
                }
                .run(&state)
                .await,
                "no_agent mode requires a non-empty script",
            );
            assert_config_error(
                CronArgs {
                    action: Some("edit"),
                    id: Some(id),
                    deliver: Some("unknown-target"),
                    ..CronArgs::default()
                }
                .run(&state)
                .await,
                "Unknown deliver target 'unknown-target'",
            );

            let unchanged = sole_job(&state).await;
            assert_eq!(unchanged.script.as_deref(), Some(script));
            assert_eq!(
                unchanged.deliver.unwrap().platform.as_deref(),
                Some("inbox")
            );

            CronArgs {
                action: Some("edit"),
                job_id: Some(id),
                id: Some("not-the-job"),
                schedule: Some("*/15 * * * *"),
                prompt: Some("updated prompt"),
                name: Some("changed name"),
                deliver: Some("slack:inline"),
                chat_id: Some(" explicit "),
                repeat: Some(2),
                skills: &["discarded"],
                clear_skills: true,
                add_skills: &["keep", "drop", "keep"],
                remove_skills: &["drop"],
                script: Some(replacement_script),
                no_agent: true,
                agent: false,
                timeout: Some(22),
                shell: Some(" bash "),
                workdir: Some(workdir),
            }
            .run(&state)
            .await
            .unwrap();

            let edited = sole_job(&state).await;
            assert_eq!(edited.id, id);
            assert_eq!(edited.schedule, "*/15 * * * *");
            assert_eq!(edited.prompt, "updated prompt");
            assert_eq!(edited.name.as_deref(), Some("changed name"));
            let delivery = edited.deliver.unwrap();
            assert_eq!(
                cron_deliver_target_platform_name(&delivery.target),
                Some("slack")
            );
            assert_eq!(delivery.platform.as_deref(), Some("explicit"));
            assert_eq!(edited.repeat, Some(2));
            assert_eq!(edited.skills.as_deref().unwrap(), ["keep"]);
            assert_eq!(edited.script.as_deref(), Some(replacement_script));
            assert!(edited.no_agent);
            assert_eq!(edited.script_timeout_seconds, Some(22));
            assert_eq!(edited.script_shell.as_deref(), Some("bash"));
            assert_eq!(edited.workdir.as_deref(), Some(workdir));

            CronArgs {
                action: Some("edit"),
                id: Some(id),
                name: Some(" "),
                remove_skills: &["keep"],
                script: Some(" "),
                agent: true,
                timeout: Some(0),
                shell: Some(" "),
                workdir: Some(" "),
                ..CronArgs::default()
            }
            .run(&state)
            .await
            .unwrap();

            let cleared = sole_job(&state).await;
            assert!(cleared.name.is_none());
            assert!(cleared.skills.is_none());
            assert!(cleared.script.is_none());
            assert!(!cleared.no_agent);
            assert!(cleared.script_timeout_seconds.is_none());
            assert!(cleared.script_shell.is_none());
            assert!(cleared.workdir.is_none());

            for action in ["pause", "status", "tick", "list", "history"] {
                CronArgs {
                    action: Some(action),
                    id: Some(id),
                    ..CronArgs::default()
                }
                .run(&state)
                .await
                .unwrap();
            }
            assert!(matches!(
                sole_job(&state).await.status,
                hermes_cron::JobStatus::Paused
            ));

            CronArgs {
                action: Some("resume"),
                id: Some(id),
                ..CronArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            assert!(matches!(
                sole_job(&state).await.status,
                hermes_cron::JobStatus::Active
            ));

            CronArgs {
                action: Some("remove"),
                id: Some(id),
                ..CronArgs::default()
            }
            .run(&state)
            .await
            .unwrap();
            assert!(persisted_jobs(&state).await.is_empty());
            assert_config_error(
                CronArgs {
                    action: Some("history"),
                    id: Some(id),
                    ..CronArgs::default()
                }
                .run(&state)
                .await,
                "unknown job id:",
            );
        }

        #[tokio::test]
        async fn cron_create_agent_flag_overrides_script_only_mode() {
            let Some(state) = isolated_state(&[], |_| {}) else {
                return;
            };
            CronArgs {
                action: Some("create"),
                schedule: Some("0 12 * * *"),
                prompt: Some("report status"),
                name: Some(" "),
                script: Some(" "),
                no_agent: true,
                agent: true,
                timeout: Some(0),
                shell: Some(" "),
                ..CronArgs::default()
            }
            .run(&state)
            .await
            .unwrap();

            let job = sole_job(&state).await;
            assert_eq!(job.schedule, "0 12 * * *");
            assert_eq!(job.prompt, "report status");
            assert!(!job.no_agent);
            assert!(job.name.is_none());
            assert!(job.script.is_none());
            assert!(job.script_timeout_seconds.is_none());
            assert!(job.script_shell.is_none());
        }

        #[tokio::test]
        async fn cron_rejects_bad_arguments_and_a_blocked_data_directory() {
            let Some(state) = isolated_state(&[], |_| {}) else {
                return;
            };
            let data_dir = cron_data_dir(&state);
            fs::create_dir_all(data_dir.parent().unwrap()).unwrap();
            fs::write(&data_dir, "not a directory").unwrap();
            assert_io_error(CronArgs::default().run(&state).await, "cron dir ");
            assert_eq!(fs::read_to_string(&data_dir).unwrap(), "not a directory");
            fs::remove_file(&data_dir).unwrap();

            for (args, error) in [
                (
                    CronArgs {
                        action: Some("create"),
                        ..CronArgs::default()
                    },
                    "cron create: use --prompt",
                ),
                (
                    CronArgs {
                        action: Some("create"),
                        deliver: Some("unknown-target"),
                        prompt: Some("a prompt"),
                        ..CronArgs::default()
                    },
                    "Unknown deliver target 'unknown-target'",
                ),
                (
                    CronArgs {
                        action: Some("create"),
                        no_agent: true,
                        script: Some(" "),
                        ..CronArgs::default()
                    },
                    "--no-agent requires --script",
                ),
                (
                    CronArgs {
                        action: Some("edit"),
                        job_id: Some(" "),
                        ..CronArgs::default()
                    },
                    "cron edit: use <job-id> or --id",
                ),
                (
                    CronArgs {
                        action: Some("edit"),
                        id: Some("absent"),
                        ..CronArgs::default()
                    },
                    "unknown job id: absent",
                ),
                (
                    CronArgs {
                        action: Some("delete"),
                        ..CronArgs::default()
                    },
                    "delete: use <job-id> or --id",
                ),
                (
                    CronArgs {
                        action: Some("unrecognized"),
                        ..CronArgs::default()
                    },
                    "Unknown cron action: unrecognized",
                ),
            ] {
                assert_config_error(args.run(&state).await, error);
            }
            assert!(persisted_jobs(&state).await.is_empty());
        }

        #[test]
        fn subscription_store_defaults_round_trip_and_reports_io_errors() {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source.json");
            assert!(load_webhook_subscriptions(&source).unwrap().is_empty());

            fs::write(
                &source,
                r#"{"example":{"secret":"test-secret","created_at":"2026-01-02T03:04:05Z"}}"#,
            )
            .unwrap();
            let subs = load_webhook_subscriptions(&source).unwrap();
            let sub = &subs["example"];
            assert_eq!(sub.deliver, "log");
            assert!(sub.description.is_empty());
            assert!(sub.events.is_empty());
            assert!(sub.prompt.is_empty());
            assert!(sub.skills.is_empty());
            assert!(!sub.deliver_only);
            assert!(sub.deliver_extra.is_none());

            let destination = temp.path().join("nested").join("subscriptions.json");
            save_webhook_subscriptions(&destination, &subs).unwrap();
            let saved = load_webhook_subscriptions(&destination).unwrap();
            assert_eq!(saved.len(), 1);
            assert_eq!(saved["example"].secret, "test-secret");
            assert_eq!(saved["example"].created_at, "2026-01-02T03:04:05Z");
            let json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&destination).unwrap()).unwrap();
            assert_eq!(json["example"]["deliver"], "log");
            assert!(json["example"].get("deliver_extra").is_none());

            fs::write(&source, "{unfinished").unwrap();
            assert_config_error(load_webhook_subscriptions(&source), "parse ");
            assert_io_error(load_webhook_subscriptions(temp.path()), "read ");
            assert_io_error(
                save_webhook_subscriptions(&source.join("child"), &subs),
                "mkdir ",
            );
            assert_io_error(save_webhook_subscriptions(temp.path(), &subs), "write ");
        }

        #[derive(Default)]
        struct WebhookArgs<'a> {
            action: Option<&'a str>,
            name: Option<&'a str>,
            url: Option<&'a str>,
            id: Option<&'a str>,
            prompt: Option<&'a str>,
            events: Option<&'a str>,
            description: Option<&'a str>,
            skills: Option<&'a str>,
            deliver: Option<&'a str>,
            chat_id: Option<&'a str>,
            secret: Option<&'a str>,
            deliver_only: bool,
        }

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
    }
}
