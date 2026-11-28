fn local_stream_adapter(server: String) -> NtfyAdapter {
    let mut config = test_config(server);
    config.token = Some("local-notification-fixture".into());
    config.stream_timeout_secs = 5;
    let mut adapter = NtfyAdapter::new(config).unwrap();
    Arc::get_mut(&mut adapter.inner).unwrap().client =
        reqwest::Client::builder().no_proxy().build().unwrap();
    adapter
}

#[tokio::test]
async fn notification_stream_preserves_utf8_at_an_http_chunk_boundary() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let adapter = local_stream_adapter(format!("http://{}", listener.local_addr().unwrap()));
    let (tx, mut rx) = mpsc::channel(4);
    adapter.set_inbound_sender(tx).await;
    adapter.inner.base.mark_running();

    let listener_response = async {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        let mut request = String::new();
        while !request.ends_with("\r\n\r\n") {
            assert!(socket.read_line(&mut request).await.unwrap() > 0);
        }
        assert!(request.starts_with("GET /hermes-in/json?poll=false HTTP/1.1\r\n"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer local-notification-fixture"));
        let event =
            r#"{"event":"message","id":"fixture-1","topic":"fixture-in","message":" café "}"#;
        let records = format!(" \r\n{{not-json}}\n{event}\n{event}\n");
        let split = records.find('é').unwrap() + 1;
        let bytes = records.as_bytes();
        let response = socket.get_mut();
        response.write_all(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n"
        ).await.unwrap();
        for chunk in [&bytes[..split], &bytes[split..]] {
            response
                .write_all(format!("{:X}\r\n", chunk.len()).as_bytes())
                .await
                .unwrap();
            response.write_all(chunk).await.unwrap();
            response.write_all(b"\r\n").await.unwrap();
        }
        response.write_all(b"0\r\n\r\n").await.unwrap();
    };
    let (_, consumed) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            listener_response,
            NtfyAdapter::consume_stream(adapter.inner.clone())
        )
    })
    .await
    .expect("synthetic notification stream timed out");
    consumed.unwrap();

    let delivered = rx.try_recv().unwrap();
    assert_eq!(delivered.text, "café");
    assert_eq!(delivered.chat_id, "fixture-in");
    assert_eq!(delivered.message_id.as_deref(), Some("fixture-1"));
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn notification_stream_reports_authentication_and_connection_failures() {
    for (status, expected) in [
        (401, "server rejected auth"),
        (404, "topic 'hermes-in' returned 404"),
        (503, "stream HTTP 503"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hermes-in/json"))
            .and(wiremock::matchers::query_param("poll", "false"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        let adapter = local_stream_adapter(server.uri());
        let error = NtfyAdapter::consume_stream(adapter.inner.clone())
            .await
            .unwrap_err();
        assert_eq!(matches!(error, GatewayError::Auth(_)), status == 401);
        assert_eq!(
            matches!(error, GatewayError::ConnectionFailed(_)),
            status != 401
        );
        assert!(error.to_string().contains(expected), "{error}");
        server.verify().await;
    }
}

#[tokio::test]
async fn notification_adapter_reconnects_after_stream_failure_and_stops() {
    let server = MockServer::start().await;
    for (priority, response) in [
        (1, ResponseTemplate::new(503)),
        (
            2,
            ResponseTemplate::new(200).set_body_raw(
                concat!(
                    "{\"event\":\"open\"}\n",
                    "{\"event\":\"message\",\"id\":\"empty\",\"message\":\"  \"}\n",
                    "{\"event\":\"message\",\"id\":\"reconnected\",\"message\":\"ready\"}\n"
                ),
                "application/x-ndjson",
            ),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path("/hermes-in/json"))
            .and(wiremock::matchers::query_param("poll", "false"))
            .respond_with(response)
            .with_priority(priority)
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
    }
    let reconnect_started = Arc::new(Notify::new());
    let on_reconnect = reconnect_started.clone();
    Mock::given(method("GET"))
        .and(path("/hermes-in/json"))
        .and(wiremock::matchers::query_param("poll", "false"))
        .respond_with(move |_: &wiremock::Request| {
            on_reconnect.notify_one();
            ResponseTemplate::new(200).set_delay(Duration::from_secs(5))
        })
        .with_priority(3)
        .expect(1)
        .mount(&server)
        .await;
    let adapter = local_stream_adapter(server.uri());
    let (tx, mut rx) = mpsc::channel(4);
    adapter.set_inbound_sender(tx).await;

    assert!(!adapter.is_running());
    assert_eq!(adapter.platform_name(), "ntfy");
    adapter.start().await.unwrap();
    assert!(adapter.is_running());
    let worker = adapter
        .run_task
        .read()
        .await
        .as_ref()
        .unwrap()
        .abort_handle();
    let (incoming, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(rx.recv(), reconnect_started.notified())
    })
    .await
    .expect("received notification and reopened the local stream");
    assert!(!worker.is_finished());
    adapter.stop().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !worker.is_finished() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("stop terminated the worker whilst its request was in flight");

    assert!(!adapter.is_running());
    assert!(adapter.run_task.read().await.is_none());
    let incoming = incoming.unwrap();
    assert_eq!(incoming.text, "ready");
    assert_eq!(incoming.chat_id, "hermes-in");
    assert_eq!(incoming.message_id.as_deref(), Some("reconnected"));
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    server.verify().await;
}

#[test]
fn notification_config_parses_legacy_markdown_values_and_default_timeout() {
    let defaults: NtfyConfig = serde_json::from_value(serde_json::json!({
        "topic": "fixture"
    }))
    .unwrap();
    assert_eq!(defaults.server, DEFAULT_SERVER);
    assert_eq!(defaults.stream_timeout_secs, STREAM_TIMEOUT_SECONDS);

    for (markdown, expected) in [
        (serde_json::json!(true), true),
        (serde_json::json!(" YeS "), true),
        (serde_json::json!("off"), false),
        (serde_json::json!(1), true),
        (serde_json::json!(0), false),
    ] {
        let platform: hermes_config::PlatformConfig = serde_json::from_value(serde_json::json!({
            "server": " http://127.0.0.1:1234/// ",
            "topic": " fixture-in ",
            "publish_topic": " fixture-out ",
            "token": " local-notification-fixture ",
            "markdown": markdown
        }))
        .unwrap();
        let adapter = NtfyAdapter::new(NtfyConfig::from_platform_config(&platform)).unwrap();
        let config = adapter.config();
        assert_eq!(config.server, "http://127.0.0.1:1234");
        assert_eq!(config.topic, "fixture-in");
        assert_eq!(config.publish_topic.as_deref(), Some("fixture-out"));
        assert_eq!(config.markdown, expected);
        assert_eq!(config.stream_timeout_secs, STREAM_TIMEOUT_SECONDS);
    }
}

#[tokio::test]
async fn notification_publish_preserves_captions_and_bounds_payloads_and_errors() {
    let server = MockServer::start().await;
    let response_body = "é".repeat(210);
    Mock::given(method("POST"))
        .and(path("/hermes-in"))
        .and(header("X-Tags", ECHO_TAG))
        .and(body_string("fixture caption\n[Attachment: report.txt]"))
        .respond_with(ResponseTemplate::new(502).set_body_string(response_body))
        .expect(1)
        .mount(&server)
        .await;
    let shortened = "é".repeat(4096);
    Mock::given(method("POST"))
        .and(path("/hermes-in"))
        .and(header("X-Tags", ECHO_TAG))
        .and(body_string(shortened.clone()))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let adapter = local_stream_adapter(server.uri());

    let failed = adapter
        .send_file("hermes-in", "report.txt", Some(" fixture caption "))
        .await
        .unwrap_err();
    assert!(matches!(
        failed,
        GatewayError::SendFailed(message)
            if message.starts_with("ntfy HTTP 502") && message.ends_with(&"é".repeat(200))
                && !message.ends_with(&"é".repeat(201))
    ));
    let longer = shortened + "trailing text";
    adapter
        .edit_message("hermes-in", "previous-fixture", &longer)
        .await
        .unwrap();
    server.verify().await;
}
