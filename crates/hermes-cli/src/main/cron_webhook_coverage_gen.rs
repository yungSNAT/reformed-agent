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
            let mut child = std::process::Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
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

        include!("cron_webhook_part2_coverage_gen.rs");
        include!("cron_webhook_part3_coverage_gen.rs");
    }
}
