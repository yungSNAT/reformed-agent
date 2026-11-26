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
