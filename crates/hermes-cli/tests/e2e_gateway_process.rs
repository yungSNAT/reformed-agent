//! E2E：短生命周期真实 `hermes gateway start` 子进程（无聊天适配器，仅 cron + webhook 侧车）。
//!
//! - 等待 `gateway.pid` 出现即视为就绪；
//! - `hermes gateway status` 应报告 running；
//! - 对子进程发 `SIGINT`，与交互式 Ctrl+C 一致，触发清理并删除 PID 文件。
//!
//! Windows 上信号语义与 `kill` 不同，测试仅在 Unix 注册。

#[cfg(unix)]
mod unix {
    use assert_cmd::cargo::cargo_bin;
    use assert_cmd::Command;
    use std::fs;
    use std::path::Path;
    use std::process::{Command as SysCommand, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    const READY_TIMEOUT: Duration = Duration::from_secs(60);
    const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

    fn wait_gateway_ready(pid_path: &Path, child: &mut std::process::Child) -> u32 {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!(
                    "timeout waiting for {}; child id {:?}",
                    pid_path.display(),
                    child.id()
                );
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!(
                    "gateway exited before ready (status={status}); expected {}",
                    pid_path.display()
                );
            }
            if let Ok(raw) = fs::read_to_string(pid_path) {
                if let Ok(pid) = raw.trim().parse::<u32>() {
                    return pid;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_child_exit(
        child: &mut std::process::Child,
        deadline: Instant,
    ) -> std::process::ExitStatus {
        loop {
            if let Ok(Some(s)) = child.try_wait() {
                return s;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("timeout waiting for gateway child to exit");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn e2e_gateway_subprocess_lifecycle_start_status_sigint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let pid_path = home.join("gateway.pid");

        let mut child = SysCommand::new(cargo_bin("hermes"))
            .env("HERMES_HOME", home)
            .args(["gateway", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn hermes gateway start");

        let file_pid = wait_gateway_ready(&pid_path, &mut child);
        assert!(file_pid > 0, "gateway.pid should contain a positive PID");

        let mut status = Command::cargo_bin("hermes").expect("hermes binary");
        status.env("HERMES_HOME", home).args(["gateway", "status"]);
        let out = status.assert().success().get_output().stdout.clone();
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("running"),
            "expected 'running' in gateway status, got: {text:?}"
        );

        let kill_st = SysCommand::new("kill")
            .args(["-INT", &file_pid.to_string()])
            .status()
            .expect("kill -INT");
        assert!(kill_st.success(), "kill -INT failed: {kill_st:?}");

        let status = wait_child_exit(&mut child, Instant::now() + EXIT_TIMEOUT);
        assert!(
            status.success(),
            "gateway should exit 0 after SIGINT shutdown, got {status:?}"
        );

        assert!(
            !pid_path.exists(),
            "gateway.pid should be removed after graceful shutdown"
        );
    }

    fn isolated_gateway_cli(home: &std::path::Path, args: &[&str]) -> assert_cmd::Command {
        let managed = home.join("managed");
        std::fs::create_dir_all(&managed).expect("create empty managed config directory");
        let mut cmd = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("hermes"));
        cmd.env_clear()
            .current_dir(home)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("HERMES_HOME", home)
            .env("HERMES_MANAGED_DIR", managed)
            .timeout(std::time::Duration::from_secs(15))
            .args(args);
        for key in ["LLVM_PROFILE_FILE", "SystemRoot"] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        cmd
    }

    #[test]
    fn gateway_cli_rejects_missing_requirements_before_starting_adapters() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(
            home.join("config.yaml"),
            "platforms:\n  discord:\n    enabled: true\n  ntfy:\n    enabled: true\n",
        )
        .unwrap();

        let mut command = isolated_gateway_cli(home, &["gateway", "run"]);
        let assertion = command.assert().failure();
        let output = assertion.get_output();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("Gateway requirement check failed"),
            "{error}"
        );
        assert!(error.contains("discord.enabled=true"), "{error}");
        assert!(
            error.contains("ntfy.enabled=true but topic is missing"),
            "{error}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("no gateway user allowlist configured"),
            "{stdout}"
        );
        assert!(!home.join("gateway.pid").exists());
        assert!(!home.join("gateway.lock").exists());
    }

    #[test]
    fn gateway_cli_status_cleans_invalid_pid_without_unlinking_owner_lock() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let pid_file = home.join("gateway.pid");
        let lock_file = home.join("gateway.lock");
        std::fs::write(&pid_file, "historical metadata").unwrap();
        std::fs::write(&lock_file, "owner marker").unwrap();

        let mut command = isolated_gateway_cli(home, &["gateway", "status"]);
        let assertion = command.assert().success();
        let stdout = String::from_utf8_lossy(&assertion.get_output().stdout);
        assert!(stdout.contains("invalid PID file"), "{stdout}");
        assert!(!pid_file.exists());
        assert_eq!(std::fs::read_to_string(&lock_file).unwrap(), "owner marker");
    }

    #[test]
    fn gateway_cli_setup_saves_only_selected_platform_choices() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config_path = home.join("config.yaml");

        let mut empty = isolated_gateway_cli(home, &["gateway", "setup"]);
        let assertion = empty.write_stdin("\n").assert().success();
        let stdout = String::from_utf8_lossy(&assertion.get_output().stdout);
        assert!(stdout.contains("No valid platforms selected"), "{stdout}");
        assert!(!config_path.exists());

        // Choose Webhook and API Server in the numbered fallback. Prompts are
        // presented in alphabetical order after the selection is confirmed.
        let mut configure = isolated_gateway_cli(home, &["gateway", "setup"]);
        configure
            .write_stdin("19\n20\n\n127.0.0.1\n18090\n\n\n19000\n/fixture-hook\n")
            .assert()
            .success();

        let raw = std::fs::read_to_string(config_path).unwrap();
        let config: hermes_config::GatewayConfig = serde_yaml::from_str(&raw).unwrap();
        let api = &config.platforms["api_server"];
        assert!(api.enabled);
        assert_eq!(api.extra["host"], "127.0.0.1");
        assert_eq!(api.extra["port"], 18090);
        let hook = &config.platforms["webhook"];
        assert!(hook.enabled);
        assert_eq!(hook.extra["port"], 19000);
        assert_eq!(hook.extra["path"], "/fixture-hook");
        assert_eq!(
            config
                .platforms
                .values()
                .filter(|platform| platform.enabled)
                .count(),
            2
        );
    }

    #[test]
    fn gateway_cli_setup_persists_platform_specific_fields() {
        use serde_json::json;

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let menu = isolated_gateway_cli(home, &["gateway", "setup"])
            .write_stdin("\n")
            .assert()
            .success();
        let stdout = String::from_utf8_lossy(&menu.get_output().stdout);
        let choices: std::collections::HashMap<_, _> = stdout
            .lines()
            .filter_map(|line| {
                let (number, label) = line.trim().strip_prefix("[ ]")?.split_once('.')?;
                let label = label.trim().strip_suffix("disabled")?.trim_end();
                Some((label, number.trim()))
            })
            .collect();
        let mut selections = [
            "BlueBubbles",
            "DingTalk",
            "Discord",
            "Email",
            "Feishu",
            "HomeAssistant",
            "Matrix",
            "Mattermost",
            "ntfy",
            "QQBot",
            "Signal",
            "Slack",
            "SMS",
            "WeCom",
            "WeCom Callback",
            "WhatsApp",
        ]
        .map(|label| {
            *choices
                .get(label)
                .unwrap_or_else(|| panic!("missing {label}"))
        })
        .join("\n");
        selections.push_str("\n\n");
        let answers = concat!(
            "http://127.0.0.1:8014\nfixture-bluebubbles\n",
            "fixture-dingtalk-id\nfixture-dingtalk-secret\n",
            " fixture-discord \nfixture-application\n first, ,second \nfixture-discord-room\n",
            "fixture@example.invalid\nfixture-email\n127.0.0.1\n127.0.0.1\n1993\n1587\n",
            "fixture-feishu-id\nfixture-feishu-secret\nfixture-verification\nfixture-encryption\n",
            "http://127.0.0.1:8123\nfixture-homeassistant\nfixture-hook\n",
            "http://127.0.0.1:8448\n@fixture:example.invalid\nfixture-matrix\n!fixture:example.invalid\n",
            "http://127.0.0.1:8065\nfixture-mattermost\nfixture-team\nfixture-mattermost-room\n",
            "fixture-subscribe\nhttp://127.0.0.1:8085\nfixture-publish\nfixture-ntfy\n",
            "fixture-qqbot-id\nfixture-qqbot-secret\nYeS\n",
            "+12025550101\nhttp://127.0.0.1:8080\n",
            "fixture-slack\nfixture-slack-app\nno\n",
            "fixture-twilio-id\nfixture-twilio-auth\n+12025550102\n",
            "fixture-wecom-id\n42\nfixture-wecom-secret\n",
            "fixture-callback-id\nfixture-callback-secret\n43\nfixture-callback-token\n",
            "fixture-encoding-key\n127.0.0.1\n18645\n/fixture-callback\n",
            "fixture-whatsapp\nfixture-phone-id\nfixture-verify\nfixture-whatsapp-room\n",
        );

        isolated_gateway_cli(home, &["gateway", "setup"])
            .write_stdin(format!("{selections}{answers}"))
            .assert()
            .success();

        let config_path = home.join("config.yaml");
        let raw = fs::read_to_string(&config_path).unwrap();
        let config: hermes_config::GatewayConfig = serde_yaml::from_str(&raw).unwrap();
        let expected = [
            (
                "bluebubbles",
                json!({"server_url": "http://127.0.0.1:8014", "password": "fixture-bluebubbles"}),
            ),
            (
                "dingtalk",
                json!({"client_id": "fixture-dingtalk-id", "client_secret": "fixture-dingtalk-secret"}),
            ),
            ("discord", json!({"application_id": "fixture-application"})),
            (
                "email",
                json!({"username": "fixture@example.invalid", "password": "fixture-email", "imap_host": "127.0.0.1", "smtp_host": "127.0.0.1", "imap_port": 1993, "smtp_port": 1587}),
            ),
            (
                "feishu",
                json!({"app_id": "fixture-feishu-id", "app_secret": "fixture-feishu-secret", "verification_token": "fixture-verification", "encrypt_key": "fixture-encryption"}),
            ),
            (
                "homeassistant",
                json!({"base_url": "http://127.0.0.1:8123", "webhook_id": "fixture-hook"}),
            ),
            (
                "matrix",
                json!({"homeserver_url": "http://127.0.0.1:8448", "user_id": "@fixture:example.invalid", "room_id": "!fixture:example.invalid"}),
            ),
            (
                "mattermost",
                json!({"server_url": "http://127.0.0.1:8065", "team_id": "fixture-team"}),
            ),
            (
                "ntfy",
                json!({"topic": "fixture-subscribe", "server": "http://127.0.0.1:8085", "publish_topic": "fixture-publish"}),
            ),
            (
                "qqbot",
                json!({"app_id": "fixture-qqbot-id", "client_secret": "fixture-qqbot-secret", "markdown_support": true}),
            ),
            (
                "signal",
                json!({"phone_number": "+12025550101", "api_url": "http://127.0.0.1:8080"}),
            ),
            (
                "slack",
                json!({"app_token": "fixture-slack-app", "socket_mode": false}),
            ),
            (
                "sms",
                json!({"account_sid": "fixture-twilio-id", "auth_token": "fixture-twilio-auth", "from_number": "+12025550102"}),
            ),
            (
                "wecom",
                json!({"corp_id": "fixture-wecom-id", "agent_id": "42", "secret": "fixture-wecom-secret"}),
            ),
            (
                "wecom_callback",
                json!({"corp_id": "fixture-callback-id", "corp_secret": "fixture-callback-secret", "agent_id": "43", "encoding_aes_key": "fixture-encoding-key", "host": "127.0.0.1", "port": 18645, "path": "/fixture-callback"}),
            ),
            (
                "whatsapp",
                json!({"phone_number_id": "fixture-phone-id", "verify_token": "fixture-verify"}),
            ),
        ];
        assert_eq!(config.platforms.len(), expected.len());
        for (name, extra) in expected {
            let platform = &config.platforms[name];
            assert!(platform.enabled, "{name}");
            assert_eq!(json!(platform.extra), extra, "{name}");
        }
        for (name, token) in [
            ("discord", "fixture-discord"),
            ("homeassistant", "fixture-homeassistant"),
            ("matrix", "fixture-matrix"),
            ("mattermost", "fixture-mattermost"),
            ("ntfy", "fixture-ntfy"),
            ("slack", "fixture-slack"),
            ("wecom_callback", "fixture-callback-token"),
            ("whatsapp", "fixture-whatsapp"),
        ] {
            assert_eq!(
                config.platforms[name].token.as_deref(),
                Some(token),
                "{name}"
            );
        }
        for (name, channel) in [
            ("discord", "fixture-discord-room"),
            ("mattermost", "fixture-mattermost-room"),
            ("whatsapp", "fixture-whatsapp-room"),
        ] {
            assert_eq!(
                config.platforms[name].home_channel.as_deref(),
                Some(channel),
                "{name}"
            );
        }
        assert_eq!(
            config.platforms["discord"].allowed_users,
            ["first", "second"]
        );
    }

    #[test]
    fn gateway_cli_setup_saves_telegram_polling_or_webhook_settings() {
        for (answers, polling, webhook) in [
            ("\nfixture-room\n", true, None),
            (
                "no\nhttp://127.0.0.1:18090/fixture-hook\nfixture-room\n",
                false,
                Some("http://127.0.0.1:18090/fixture-hook"),
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let home = directory.path();
            let assertion = isolated_gateway_cli(home, &["gateway", "setup"])
                .write_stdin(format!("3\n\nfixture-telegram-token\n{answers}"))
                .assert()
                .success();
            let output = assertion.get_output();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("Configuring telegram"), "{stdout}");
            assert!(stdout.contains("Gateway setup complete."), "{stdout}");
            assert!(!stdout.contains("fixture-telegram-token"));
            assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-telegram-token"));
            let config: hermes_config::GatewayConfig =
                serde_yaml::from_str(&fs::read_to_string(home.join("config.yaml")).unwrap())
                    .unwrap();
            let telegram = &config.platforms["telegram"];
            assert!(telegram.enabled);
            assert_eq!(telegram.token.as_deref(), Some("fixture-telegram-token"));
            assert_eq!(telegram.extra["polling"], polling);
            assert_eq!(telegram.webhook_url.as_deref(), webhook);
            assert_eq!(telegram.home_channel.as_deref(), Some("fixture-room"));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gateway_cli_setup_reconfigures_weixin_policies_after_qr_fallback() {
        use serde_json::json;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let origin = server.uri();
        Mock::given(method("GET"))
            .and(path("/fixture-start"))
            .and(query_param("bot_type", "3"))
            .respond_with(ResponseTemplate::new(503).set_body_string("fixture QR service down"))
            .expect(4)
            .mount(&server)
            .await;
        tokio::task::spawn_blocking(move || {
            let directory = tempfile::tempdir().unwrap();
            let home = directory.path();
            let config_path = home.join("config.yaml");
            fs::write(
                &config_path,
                format!(
                    "platforms:\n  weixin:\n    enabled: true\n    base_url: {origin}\n    qr_start_endpoint: /fixture-start\n"
                ),
            )
            .unwrap();
            for (answers, dm_policy, group_policy, users, groups) in [
                (
                    "3\nfixture-a, , fixture-b\n3\nfixture-group\nfixture-room\n",
                    "allowlist",
                    "allowlist",
                    json!(["fixture-a", "fixture-b"]),
                    json!(["fixture-group"]),
                ),
                ("2\n2\n\n", "open", "open", json!([]), json!([])),
                ("4\n\n\n", "disabled", "disabled", json!([]), json!([])),
                ("\n\n\n", "pairing", "disabled", json!([]), json!([])),
            ] {
                let assertion = isolated_gateway_cli(home, &["gateway", "setup"])
                    .env("WEIXIN_BASE_URL", &origin)
                    .env("WEIXIN_ACCOUNT_ID", "fixture-weixin")
                    .env("WEIXIN_TOKEN", "fixture-weixin-token")
                    .write_stdin(format!("\n{answers}"))
                    .assert()
                    .success();
                let output = assertion.get_output();
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(stdout.contains("fixture QR service down"), "{stdout}");
                assert!(stdout.contains("Gateway setup complete."), "{stdout}");
                assert!(!stdout.contains("fixture-weixin-token"));
                assert!(!String::from_utf8_lossy(&output.stderr).contains("fixture-weixin-token"));
                let config: hermes_config::GatewayConfig =
                    serde_yaml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
                let weixin = &config.platforms["weixin"];
                assert!(weixin.enabled);
                assert_eq!(weixin.token.as_deref(), Some("fixture-weixin-token"));
                assert_eq!(weixin.home_channel.as_deref(), Some("fixture-room"));
                assert_eq!(weixin.extra["account_id"], "fixture-weixin");
                assert_eq!(weixin.extra["base_url"], origin);
                assert_eq!(weixin.extra["dm_policy"], dm_policy);
                assert_eq!(weixin.extra["group_policy"], group_policy);
                assert_eq!(weixin.extra["allow_from"], users);
                assert_eq!(weixin.extra["group_allow_from"], groups);
            }
        })
        .await
        .unwrap();
        server.verify().await;
    }

    #[tokio::test]
    async fn whatsapp_setup_retains_other_platforms_when_api_is_unavailable() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let proxy = MockServer::start().await;
        Mock::given(method("CONNECT"))
            .respond_with(ResponseTemplate::new(503))
            .expect(2)
            .mount(&proxy)
            .await;
        let origin = proxy.uri();
        tokio::task::spawn_blocking(move || {
            let directory = tempfile::tempdir().unwrap();
            let home = directory.path();
            let config = home.join("config.yaml");
            let original = "platforms:\n  telegram:\n    enabled: false\n";
            fs::write(&config, original).unwrap();
            let cli = |action| {
                let mut command = isolated_gateway_cli(home, &["whatsapp", action]);
                command
                    .env("HTTP_PROXY", &origin)
                    .env("HTTPS_PROXY", &origin)
                    .env("ALL_PROXY", &origin);
                command
            };

            for (answers, missing) in [
                ("\n", "phone number ID"),
                ("fixture-phone\n\n", "business account ID"),
                ("fixture-phone\nfixture-business\n\n", "access token"),
            ] {
                let result = cli("setup").write_stdin(answers).assert().success();
                let stdout = String::from_utf8_lossy(&result.get_output().stdout);
                assert!(stdout.contains(&format!("Aborted: {missing} is required.")));
                assert_eq!(fs::read_to_string(&config).unwrap(), original);
            }

            let access = "synthetic-whatsapp-setup";
            let result = cli("setup")
                .write_stdin(format!(" fixture-phone \n fixture-business \n {access} \n"))
                .assert()
                .success();
            let stdout = String::from_utf8_lossy(&result.get_output().stdout);
            assert!(stdout.contains("Saving config anyway"));
            assert!(stdout.contains("WhatsApp configuration saved"));
            assert!(!stdout.contains(access));

            let yaml: serde_yaml::Value =
                serde_yaml::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
            assert_eq!(
                yaml["platforms"]["telegram"]["enabled"].as_bool(),
                Some(false)
            );
            let whatsapp = &yaml["platforms"]["whatsapp"];
            assert_eq!(whatsapp["enabled"].as_bool(), Some(true));
            assert_eq!(whatsapp["phone_number_id"].as_str(), Some("fixture-phone"));
            assert_eq!(
                whatsapp["business_account_id"].as_str(),
                Some("fixture-business")
            );
            assert_eq!(whatsapp["access_token"].as_str(), Some(access));

            let result = cli("status").assert().success();
            let stdout = String::from_utf8_lossy(&result.get_output().stdout);
            assert!(stdout.contains("Phone Number ID: fixture-phone"));
            assert!(stdout.contains("Access Token:   present"));
            assert!(stdout.contains("API Connectivity: unreachable"));
            assert!(!stdout.contains(access));
        })
        .await
        .unwrap();
        proxy.verify().await;
    }

    #[test]
    fn gateway_cli_setup_replaces_existing_wecom_callback_token() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let config = home.join("config.yaml");
        fs::write(
            &config,
            "platforms:\n  wecom_callback:\n    enabled: true\n    token: fixture-old\n",
        )
        .unwrap();
        // Confirm the existing selection and leave every other prompt blank.
        for stdin in ["\n\n\n\n fixture-new \n\n\n\n\n", "\n\n\n\n\n\n\n\n\n"] {
            let result = isolated_gateway_cli(home, &["gateway", "setup"])
                .write_stdin(stdin)
                .assert()
                .success();
            let stdout = String::from_utf8_lossy(&result.get_output().stdout);
            assert!(stdout.contains("Gateway setup complete."), "{stdout}");
            let parsed: hermes_config::GatewayConfig =
                serde_yaml::from_str(&fs::read_to_string(&config).unwrap())
                    .expect("callback configuration has a single token field");
            let callback = &parsed.platforms["wecom_callback"];
            assert!(callback.enabled);
            assert_eq!(callback.token.as_deref(), Some("fixture-new"));
        }
    }
}
