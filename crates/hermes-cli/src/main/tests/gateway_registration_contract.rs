#[cfg(unix)]
#[tokio::test]
async fn gateway_registration_skips_incomplete_configs_and_keeps_configured_adapters() {
    if !crate::isolated_test_profile::enter(
        "tests::gateway_registration_skips_incomplete_configs_and_keeps_configured_adapters",
    ) {
        return;
    }
    let gateway = make_gateway();
    let mut sidecars = Vec::new();
    let mut config = hermes_config::GatewayConfig {
        platforms: [
            "telegram",
            "weixin",
            "discord",
            "slack",
            "matrix",
            "mattermost",
            "signal",
            "whatsapp",
            "dingtalk",
            "feishu",
            "wecom",
            "wecom_callback",
            "qq",
            "bluebubbles",
            "email",
            "sms",
            "homeassistant",
            "webhook",
        ]
        .into_iter()
        .map(|name| (name.into(), make_platform(true, None)))
        .collect(),
        ..Default::default()
    };
    register_gateway_adapters(&config, gateway.clone(), &mut sidecars)
        .await
        .expect("skip incomplete adapters");
    assert!(gateway.adapter_names().await.is_empty());
    assert!(sidecars.is_empty());

    let value = "plain-local-fixture";
    let loopback = "http://127.0.0.1:1";
    let configured = [
        (
            "discord",
            "discord",
            cfg!(feature = "gateway-discord"),
            serde_json::json!({
                "token": value,
                "extra": {
                    "api_base_url": loopback,
                },
            }),
        ),
        (
            "slack",
            "slack",
            cfg!(feature = "gateway-slack"),
            serde_json::json!({
                "token": value,
            }),
        ),
        (
            "matrix",
            "matrix",
            cfg!(feature = "gateway-matrix"),
            serde_json::json!({
                "token": value,
                "extra": {
                    "homeserver_url": loopback,
                    "user_id": "@fixture:localhost",
                },
            }),
        ),
        (
            "mattermost",
            "mattermost",
            cfg!(feature = "gateway-mattermost"),
            serde_json::json!({
                "token": value,
                "extra": {
                    "server_url": loopback,
                },
            }),
        ),
        (
            "signal",
            "signal",
            cfg!(feature = "gateway-signal"),
            serde_json::json!({
                "extra": {
                    "phone_number": "+15550100100",
                    "api_url": loopback,
                },
            }),
        ),
        (
            "email",
            "email",
            cfg!(feature = "gateway-email"),
            serde_json::json!({
                "allowed_users": ["sender@example.invalid"],
                "extra": {
                    "imap_host": "localhost",
                    "smtp_host": "localhost",
                    "username": "fixture@example.invalid",
                    "password": value,
                },
            }),
        ),
        (
            "qq",
            "qqbot",
            cfg!(feature = "gateway-qqbot"),
            serde_json::json!({
                "extra": {
                    "app_id": "fixture-app",
                    "client_secret": value,
                },
            }),
        ),
        (
            "feishu",
            "feishu",
            cfg!(feature = "gateway-feishu"),
            serde_json::json!({
                "extra": {
                    "app_id": "fixture-app",
                    "app_secret": value,
                },
            }),
        ),
        (
            "wecom",
            "wecom",
            cfg!(feature = "gateway-wecom"),
            serde_json::json!({
                "extra": {
                    "corp_id": "fixture-company",
                    "agent_id": "1",
                    "secret": value,
                },
            }),
        ),
        (
            "webhook",
            "webhook",
            cfg!(feature = "gateway-webhook"),
            serde_json::json!({
                "token": value,
                "extra": {
                    "host": "127.0.0.1",
                    "port": 0,
                    "path": "/fixture",
                },
            }),
        ),
        (
            "api-server",
            "api_server",
            cfg!(feature = "gateway-api-server"),
            serde_json::json!({
                "extra": {
                    "host": "127.0.0.1",
                    "port": 0,
                },
            }),
        ),
    ];
    let mut expected = Vec::new();
    for (key, registered_name, compiled, extra) in configured {
        let row = config
            .platforms
            .entry(key.into())
            .or_insert_with(|| make_platform(true, None));
        row.merge_with_json(&extra);
        if compiled {
            expected.push(registered_name.to_string());
        }
    }
    // Registration must leave every adapter unstarted.
    register_gateway_adapters(&config, gateway.clone(), &mut sidecars)
        .await
        .expect("register fixture adapters");
    let mut registered = gateway.adapter_names().await;
    registered.sort();
    expected.sort();
    assert_eq!(registered, expected);

    for name in &registered {
        let adapter = gateway.get_adapter(name).await.expect("registered adapter");
        assert!(!adapter.is_running(), "{name} started during registration");
    }
    assert_eq!(
        sidecars.len(),
        usize::from(cfg!(feature = "gateway-webhook"))
            + usize::from(cfg!(feature = "gateway-api-server"))
    );
    for sidecar in sidecars {
        sidecar.abort();
        assert!(sidecar.await.unwrap_err().is_cancelled());
    }
}
