#[tokio::test]
async fn gateway_api_inbound_routes_requests_with_session_overrides_after_handler_failure() {
    if !crate::isolated_test_profile::enter(
        "tests::gateway_api_inbound_routes_requests_with_session_overrides_after_handler_failure",
    ) {
        return;
    }

    let mut dm = DmManager::with_ignore_behavior();
    dm.authorize_user_for_platform("api_server", "field-user");
    let gateway = Arc::new(Gateway::new(
        Arc::new(SessionManager::new(SessionConfig::default())),
        dm,
        hermes_gateway::gateway::GatewayConfig::default(),
    ));
    let adapter = Arc::new(ApiServerAdapter::new(build_api_server_config(
        &PlatformConfig::default(),
    )));
    gateway.register_adapter("api_server", adapter).await;
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observations = observed.clone();
    gateway
        .set_message_handler_with_context(Arc::new(move |messages, context| {
            let observations = observations.clone();
            Box::pin(async move {
                let prompt = messages.last().unwrap().content.clone().unwrap();
                let fail = prompt == "First survey";
                observations.lock().unwrap().push((
                    context.platform,
                    context.chat_id,
                    context.user_id,
                    context.model,
                    context.provider,
                    context.personality,
                    prompt,
                ));
                if fail {
                    return Err(hermes_gateway::GatewayError::Platform(
                        "fixture failure".into(),
                    ));
                }
                Ok("Survey received".into())
            })
        }))
        .await;

    let (send, recv) = mpsc::channel(2);
    for (session, prompt, model) in [
        ("field-session", "First survey", Some("survey-model")),
        ("other-session", "Second survey", None),
    ] {
        send.send(ApiInboundRequest {
            request_id: format!("{session}-request"),
            session_id: session.into(),
            user_id: "field-user".into(),
            model: model.map(String::from),
            provider: model.map(|_| "survey-provider".into()),
            personality: model.map(|_| "survey-personality".into()),
            prompt: prompt.into(),
        })
        .await
        .unwrap();
    }
    drop(send);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_api_server_inbound_loop(gateway, recv),
    )
    .await
    .expect("the API loop drains its closed channel");

    assert_eq!(
        *observed.lock().unwrap(),
        [
            (
                String::from("api_server"),
                String::from("field-session"),
                String::from("field-user"),
                Some(String::from("survey-model")),
                Some(String::from("survey-provider")),
                Some(String::from("survey-personality")),
                String::from("First survey"),
            ),
            (
                String::from("api_server"),
                String::from("other-session"),
                String::from("field-user"),
                None,
                None,
                None,
                String::from("Second survey"),
            ),
        ]
    );
}
