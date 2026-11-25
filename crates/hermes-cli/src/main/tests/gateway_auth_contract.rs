async fn gateway_fixture_auth(cli: &Cli, action: &str, provider: &str) -> Result<(), AgentError> {
    run_auth(
        cli.clone(),
        Some(action.into()),
        Some(provider.into()),
        None,
        None,
        None,
        None,
        false,
    )
    .await
}

#[cfg(feature = "gateway-weixin")]
#[tokio::test]
async fn weixin_poll_restores_the_saved_cursor_and_message_context() {
    if !crate::isolated_test_profile::enter(
        "tests::weixin_poll_restores_the_saved_cursor_and_message_context",
    ) {
        return;
    }
    use hermes_core::traits::PlatformAdapter;
    use hermes_gateway::platforms::weixin::{WeChatAdapter, WeixinConfig};
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let home = hermes_config::hermes_home();
    let accounts = home.join("weixin/accounts");
    std::fs::create_dir_all(&accounts).unwrap();
    let cursor_path = accounts.join("fixture-account.sync.json");
    std::fs::write(&cursor_path, r#"{"get_updates_buf":"fixture-start"}"#).unwrap();
    let deadline = std::time::Duration::from_secs(5);

    for (cursor, next, text, new_context) in [
        (
            "fixture-start",
            "fixture-next",
            "first fixture",
            Some("fixture-context"),
        ),
        ("fixture-next", "fixture-end", "second fixture", None),
    ] {
        let server = MockServer::start().await;
        let mut inbound = json!({
            "from_user_id":"fixture-peer",
            "message_id":next,
            "item_list":[{"type":1, "text_item":{"text":text}}]
        });
        if let Some(context) = new_context {
            inbound["context_token"] = json!(context);
        }
        Mock::given(method("POST"))
            .and(path("/ilink/bot/getupdates"))
            .and(body_partial_json(json!({"get_updates_buf":cursor})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ret":0, "get_updates_buf":next, "longpolling_timeout_ms":1200, "msgs":[inbound]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/ilink/bot/sendmessage"))
            .and(body_partial_json(json!({
                "msg":{"to_user_id":"fixture-peer", "context_token":"fixture-context",
                       "item_list":[{"type":1, "text_item":{"text":"fixture reply"}}]}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ret":0})))
            .expect(1)
            .mount(&server)
            .await;
        let adapter = WeChatAdapter::new(WeixinConfig {
            account_id: "fixture-account".into(),
            token: "fixture-value".into(),
            base_url: server.uri(),
            cdn_base_url: server.uri(),
            dm_policy: "allowlist".into(),
            group_policy: "disabled".into(),
            allow_from: vec!["fixture-peer".into()],
            group_allow_from: Vec::new(),
            proxy: Default::default(),
        })
        .unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        adapter.set_inbound_sender(sender).await;
        adapter.start().await.unwrap();
        assert!(adapter.is_running());
        let message = tokio::time::timeout(deadline, receiver.recv())
            .await
            .expect("fixture poll timed out")
            .unwrap();
        assert_eq!(message.user_id, "fixture-peer");
        assert_eq!(message.chat_id, "fixture-peer");
        assert_eq!(message.message_id.as_deref(), Some(next));
        assert_eq!(message.text, text);
        assert!(message.is_dm);
        tokio::time::timeout(
            deadline,
            adapter.send_ilink_text("fixture-peer", "fixture reply"),
        )
        .await
        .expect("fixture reply timed out")
        .unwrap();
        adapter.stop().await.unwrap();
        assert!(!adapter.is_running());
        server.verify().await;
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&cursor_path).unwrap()).unwrap();
        assert_eq!(stored, json!({"get_updates_buf":next}));
    }
}

#[tokio::test]
async fn gateway_auth_lifecycle_uses_the_isolated_profile() {
    let answers = concat!(
        "\n",
        "fixture-telegram-value\n",
        "fixture-weixin-account\n",
        "fixture-weixin-value\n",
        "100123456\n",
        "fixture-qq-value\n",
    );
    if !crate::isolated_test_profile::enter_with_input(
        "tests::gateway_auth_lifecycle_uses_the_isolated_profile",
        answers.as_bytes(),
    ) {
        return;
    }
    use clap::Parser;
    let cli = Cli::try_parse_from(["hermes-ultra"]).unwrap();
    let root = std::env::current_dir().unwrap();
    assert_eq!(
        hermes_state_root(&cli).canonicalize().unwrap(),
        root.canonicalize().unwrap()
    );
    let path = root.join("config.yaml");

    assert!(matches!(
        gateway_fixture_auth(&cli, "login", "telegram").await,
        Err(AgentError::Config(message)) if message.contains("Telegram bot token cannot be empty")
    ));
    assert!(!path.exists());
    gateway_fixture_auth(&cli, "login", "telegram")
        .await
        .unwrap();
    gateway_fixture_auth(&cli, "status", "telegram")
        .await
        .unwrap();
    let disk = load_user_config_file(&path).unwrap();
    let telegram = &disk.platforms["telegram"];
    assert!(telegram.enabled);
    assert_eq!(telegram.token.as_deref(), Some("fixture-telegram-value"));
    gateway_fixture_auth(&cli, "logout", "telegram")
        .await
        .unwrap();
    let disk = load_user_config_file(&path).unwrap();
    assert!(!disk.platforms["telegram"].enabled);
    assert!(disk.platforms["telegram"].token.is_none());

    gateway_fixture_auth(&cli, "login", "weixin").await.unwrap();
    gateway_fixture_auth(&cli, "status", "weixin")
        .await
        .unwrap();
    let disk = load_user_config_file(&path).unwrap();
    let weixin = &disk.platforms["weixin"];
    assert!(weixin.enabled);
    assert_eq!(weixin.token.as_deref(), Some("fixture-weixin-value"));
    assert_eq!(
        weixin.extra["account_id"].as_str(),
        Some("fixture-weixin-account")
    );
    let account = root.join("weixin/accounts/fixture-weixin-account.json");
    let account_payload = std::fs::read_to_string(&account).unwrap();
    let account_fields: serde_json::Value = serde_json::from_str(&account_payload).unwrap();
    assert_eq!(
        account_fields["token"].as_str(),
        Some("fixture-weixin-value")
    );

    gateway_fixture_auth(&cli, "logout", "weixin")
        .await
        .unwrap();
    let disk = load_user_config_file(&path).unwrap();
    assert!(!disk.platforms["weixin"].enabled);
    assert!(disk.platforms["weixin"].token.is_none());
    assert_eq!(std::fs::read_to_string(&account).unwrap(), account_payload);
    gateway_fixture_auth(&cli, "login", "weixin").await.unwrap();
    let disk = load_user_config_file(&path).unwrap();
    assert!(disk.platforms["weixin"].enabled);
    assert_eq!(disk.platforms["weixin"].token, weixin.token);

    gateway_fixture_auth(&cli, "login", "qqbot").await.unwrap();
    let mut disk = load_user_config_file(&path).unwrap();
    let qqbot = disk.platforms.get_mut("qqbot").unwrap();
    assert!(qqbot.enabled);
    assert_eq!(qqbot.extra["app_id"].as_str(), Some("100123456"));
    assert_eq!(
        qqbot.extra["client_secret"].as_str(),
        Some("fixture-qq-value")
    );
    assert_eq!(qqbot.extra["markdown_support"].as_bool(), Some(true));
    qqbot
        .extra
        .insert("markdown_support".into(), serde_json::json!(false));
    save_config_yaml(&path, &disk).unwrap();
    gateway_fixture_auth(&cli, "login", "qqbot").await.unwrap();
    let disk = load_user_config_file(&path).unwrap();
    assert_eq!(
        disk.platforms["qqbot"].extra["markdown_support"].as_bool(),
        Some(false)
    );
}

#[tokio::test]
async fn gateway_auth_prompts_save_only_the_requested_platform_settings() {
    let answers = concat!(
        " fixture-slack-value  \n",
        "fixture-slack-app\n",
        "n\n",
        "fixture-discord-value\n",
        "fixture-application\n",
        " 2101,2102, ,2103  \n",
        "4200\n",
        "http://127.0.0.1:8765\n",
        "@fixture:localhost\n",
        "fixture-matrix-value\n",
        "!fixture:localhost\n",
        "fixtures@example.invalid\n",
        "fixture-email-value\n",
        "imap.example.invalid\n",
        "smtp.example.invalid\n",
        "1993\n",
        "\n",
    );
    if !crate::isolated_test_profile::enter_with_input(
        "tests::gateway_auth_prompts_save_only_the_requested_platform_settings",
        answers.as_bytes(),
    ) {
        return;
    }
    use clap::Parser;
    let cli = Cli::try_parse_from(["hermes-ultra"]).unwrap();
    let root = std::env::current_dir().unwrap();
    assert_eq!(
        hermes_state_root(&cli).canonicalize().unwrap(),
        root.canonicalize().unwrap()
    );
    for provider in ["slack", "discord", "matrix", "email"] {
        gateway_fixture_auth(&cli, "login", provider).await.unwrap();
    }
    let path = root.join("config.yaml");
    let disk = load_user_config_file(&path).unwrap();
    assert_eq!(disk.platforms.len(), 4);
    assert!(disk.platforms.values().all(|platform| platform.enabled));
    let slack = &disk.platforms["slack"];
    assert_eq!(slack.token.as_deref(), Some("fixture-slack-value"));
    assert_eq!(slack.extra["app_token"].as_str(), Some("fixture-slack-app"));
    assert_eq!(slack.extra["socket_mode"].as_bool(), Some(false));
    let discord = &disk.platforms["discord"];
    assert_eq!(discord.token.as_deref(), Some("fixture-discord-value"));
    assert_eq!(
        discord.extra["application_id"].as_str(),
        Some("fixture-application")
    );
    assert_eq!(discord.allowed_users, ["2101", "2102", "2103"]);
    assert_eq!(discord.home_channel.as_deref(), Some("4200"));
    let matrix = &disk.platforms["matrix"];
    assert_eq!(matrix.token.as_deref(), Some("fixture-matrix-value"));
    assert_eq!(
        matrix.extra["homeserver_url"].as_str(),
        Some("http://127.0.0.1:8765")
    );
    assert_eq!(matrix.extra["user_id"].as_str(), Some("@fixture:localhost"));
    assert_eq!(matrix.extra["room_id"].as_str(), Some("!fixture:localhost"));
    let email = &disk.platforms["email"];
    assert_eq!(
        email.extra["username"].as_str(),
        Some("fixtures@example.invalid")
    );
    assert_eq!(
        email.extra["password"].as_str(),
        Some("fixture-email-value")
    );
    assert_eq!(
        email.extra["imap_host"].as_str(),
        Some("imap.example.invalid")
    );
    assert_eq!(
        email.extra["smtp_host"].as_str(),
        Some("smtp.example.invalid")
    );
    assert_eq!(email.extra["imap_port"].as_u64(), Some(1993));
    assert!(!email.extra.contains_key("smtp_port"));

    gateway_fixture_auth(&cli, "status", "discord")
        .await
        .unwrap();
    gateway_fixture_auth(&cli, "logout", "discord")
        .await
        .unwrap();
    let after = load_user_config_file(&path).unwrap();
    assert!(!after.platforms["discord"].enabled);
    assert!(after.platforms["discord"].token.is_none());
    assert_eq!(
        after.platforms["discord"].allowed_users,
        discord.allowed_users
    );
    assert_eq!(after.platforms["slack"].token, slack.token);
    assert_eq!(after.platforms["matrix"].token, matrix.token);
}
