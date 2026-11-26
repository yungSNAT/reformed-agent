use super::*;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use wiremock::matchers::{body_json, header};
use wiremock::Request;

const FILE_BYTES: &[u8] = &[0, 1, 0xff, b'\n'];

fn local_adapter(api: &MockServer) -> SlackAdapter {
    let mut adapter = SlackAdapter::new(slack_test_config()).unwrap();
    adapter.api_base = format!("{}/api", api.uri());
    adapter.client = Client::builder().no_proxy().build().unwrap();
    adapter
}

async fn expect_post(api: &MockServer, endpoint: &str, body: Value, response: Value) {
    Mock::given(method("POST"))
        .and(path(format!("/api{endpoint}")))
        .and(header("authorization", "Bearer xoxb-test"))
        .and(body_json(body))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(api)
        .await;
}

#[tokio::test]
async fn message_delivery_keeps_threads_and_unicode_boundaries() {
    let api = MockServer::start().await;
    let adapter = local_adapter(&api);
    assert_eq!(adapter.config().token, "xoxb-test");
    let head = "a".repeat(MAX_MESSAGE_LENGTH - 1);
    let full = format!("{head}é");
    expect_post(
        &api,
        "/chat.postMessage",
        json!({"channel":"C1","text":head,"thread_ts":"1.00"}),
        json!({"ok":true,"ts":"2.00"}),
    )
    .await;
    expect_post(
        &api,
        "/chat.postMessage",
        json!({"channel":"C1","text":"é","thread_ts":"1.00"}),
        json!({"ok":true,"ts":"3.00"}),
    )
    .await;
    assert_eq!(
        adapter
            .post_message("C1", &full, Some("1.00"))
            .await
            .unwrap(),
        "3.00"
    );
    expect_post(
        &api,
        "/chat.update",
        json!({"channel":"C1","ts":"3.00","text":head}),
        json!({"ok":true}),
    )
    .await;
    adapter.update_message("C1", "3.00", &full).await.unwrap();

    let blocks = BlockKitMessage::new().add_divider();
    expect_post(
        &api,
        "/chat.postMessage",
        json!({"channel":"C1","text":"heading","thread_ts":"1.00","blocks":[{"type":"divider"}]}),
        json!({"ok":true,"ts":"4.00"}),
    )
    .await;
    assert_eq!(
        adapter
            .post_block_kit("C1", &blocks, "heading", Some("1.00"))
            .await
            .unwrap(),
        "4.00"
    );
    expect_post(
        &api,
        "/chat.postMessage",
        json!({"channel":"C1","text":"missing ts","blocks":[]}),
        json!({"ok":true}),
    )
    .await;
    assert!(matches!(
        adapter.post_blocks("C1", &json!([]), "missing ts", None).await,
        Err(GatewayError::SendFailed(message)) if message == "No ts in response"
    ));
}

#[tokio::test]
async fn new_multi_part_message_reuses_its_first_chunk_as_the_thread_root() {
    let api = MockServer::start().await;
    let adapter = local_adapter(&api);
    let first = "a".repeat(MAX_MESSAGE_LENGTH);
    let second = "b".repeat(MAX_MESSAGE_LENGTH);
    for (request, ts) in [
        (json!({"channel":"C1","text":first}), "1.00"),
        (
            json!({"channel":"C1","text":second,"thread_ts":"1.00"}),
            "2.00",
        ),
        (
            json!({"channel":"C1","text":"end","thread_ts":"1.00"}),
            "3.00",
        ),
    ] {
        expect_post(
            &api,
            "/chat.postMessage",
            request,
            json!({"ok":true,"ts":ts}),
        )
        .await;
    }
    let message = format!("{first}{second}end");
    assert_eq!(
        adapter.post_message("C1", &message, None).await.unwrap(),
        "3.00"
    );
}

#[tokio::test]
async fn channel_and_view_commands_send_the_requested_objects() {
    let api = MockServer::start().await;
    let adapter = local_adapter(&api);
    expect_post(
        &api,
        "/views.publish",
        json!({"user_id":"U1","view":{"type":"home","blocks":[{"type":"divider"}]}}),
        json!({"ok":true}),
    )
    .await;
    let view = HomeView::new(vec![Block::divider()]);
    adapter.publish_home_tab("U1", &view).await.unwrap();

    let modal = ModalView::new("Review", Vec::new()).with_callback_id("dismiss");
    expect_post(
        &api,
        "/views.open",
        json!({"trigger_id":"T1","view":modal.to_json()}),
        json!({"ok":true}),
    )
    .await;
    adapter.open_modal("T1", &modal).await.unwrap();

    let reaction = json!({"channel":"C1","timestamp":"2.00","name":"wave"});
    expect_post(&api, "/reactions.add", reaction.clone(), json!({"ok":true})).await;
    adapter.add_reaction("C1", "2.00", "wave").await.unwrap();
    expect_post(
        &api,
        "/reactions.remove",
        reaction,
        json!({"ok":false,"error":"no_reaction"}),
    )
    .await;
    assert!(matches!(
        adapter.remove_reaction("C1", "2.00", "wave").await,
        Err(GatewayError::SendFailed(message)) if message == "Slack reactions.remove error: no_reaction"
    ));
    expect_post(
        &api,
        "/conversations.setTopic",
        json!({"channel":"C1","topic":"Release notes"}),
        json!({"ok":true}),
    )
    .await;
    adapter.set_topic("C1", "Release notes").await.unwrap();
}

#[tokio::test]
async fn user_and_permalink_requests_preserve_api_errors() {
    let api = MockServer::start().await;
    let adapter = local_adapter(&api);
    for (user, response) in [
        (
            "U1",
            json!({"ok":true,"user":{"id":"U1","is_bot":false,"profile":{"display_name":"Fixture"}}}),
        ),
        ("U2", json!({"ok":true})),
        ("U3", json!({"ok":false,"error":"user_not_found"})),
    ] {
        Mock::given(method("GET"))
            .and(path("/api/users.info"))
            .and(query_param("user", user))
            .and(header("authorization", "Bearer xoxb-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&api)
            .await;
    }
    let user = adapter.get_user_info("U1").await.unwrap();
    assert_eq!(user.id, "U1");
    assert_eq!(
        user.profile.unwrap().display_name.as_deref(),
        Some("Fixture")
    );
    assert!(matches!(
        adapter.get_user_info("U2").await,
        Err(GatewayError::SendFailed(message)) if message.contains("no user object")
    ));
    assert!(matches!(
        adapter.get_user_info("U3").await,
        Err(GatewayError::SendFailed(message)) if message.ends_with("user_not_found")
    ));

    for (ts, response) in [
        (
            "2.00",
            json!({"ok":true,"permalink":"https://fixture.invalid/archives/C1/p200"}),
        ),
        ("3.00", json!({"ok":true})),
        ("4.00", json!({"ok":false})),
    ] {
        Mock::given(method("GET"))
            .and(path("/api/chat.getPermalink"))
            .and(query_param("channel", "C1"))
            .and(query_param("message_ts", ts))
            .and(header("authorization", "Bearer xoxb-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&api)
            .await;
    }
    assert_eq!(
        adapter.get_permalink("C1", "2.00").await.unwrap(),
        "https://fixture.invalid/archives/C1/p200"
    );
    assert!(matches!(
        adapter.get_permalink("C1", "3.00").await,
        Err(GatewayError::SendFailed(message)) if message.contains("no permalink")
    ));
    assert!(matches!(
        adapter.get_permalink("C1", "4.00").await,
        Err(GatewayError::SendFailed(message)) if message == "Slack chat.getPermalink error: unknown"
    ));
}

#[tokio::test]
async fn socket_mode_uses_the_app_credential_and_validates_the_response() {
    for (response, expected) in [
        (
            json!({"ok":true,"url":"wss://fixture.invalid/socket"}),
            Ok("wss://fixture.invalid/socket"),
        ),
        (json!({"ok":true}), Err("No URL in Socket Mode response")),
        (
            json!({"ok":false,"error":"disabled"}),
            Err("Socket Mode connection failed: disabled"),
        ),
    ] {
        let api = MockServer::start().await;
        let mut adapter = local_adapter(&api);
        assert!(matches!(
            adapter.get_socket_mode_url().await,
            Err(GatewayError::Auth(_))
        ));
        adapter.config.app_token = Some("fixture-app".into());
        Mock::given(method("POST"))
            .and(path("/api/apps.connections.open"))
            .and(header("authorization", "Bearer fixture-app"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&api)
            .await;
        match expected {
            Ok(url) => assert_eq!(adapter.get_socket_mode_url().await.unwrap(), url),
            Err(reason) => assert!(matches!(
                adapter.get_socket_mode_url().await,
                Err(GatewayError::ConnectionFailed(message)) if message == reason
            )),
        }
    }
}

fn local_upload_adapter(api: &MockServer, upload: &MockServer) -> SlackAdapter {
    let mut adapter = local_adapter(api);
    adapter.file_upload_origin = reqwest::Url::parse(&upload.uri()).unwrap();
    adapter.file_upload_client =
        SlackAdapter::build_file_upload_client(Client::builder().no_proxy()).unwrap();
    adapter
}

async fn expect_upload_ready(api: &MockServer, response: Value) {
    Mock::given(method("POST"))
        .and(path("/api/files.getUploadURLExternal"))
        .and(header("authorization", "Bearer xoxb-test"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(|request: &Request| request.body == b"filename=fixture+file.txt&length=4")
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(api)
        .await;
}

#[tokio::test]
async fn external_file_upload_delivers_bytes_then_finalises_the_thread_reply() {
    let api = MockServer::start().await;
    let upload = MockServer::start().await;
    let adapter = local_upload_adapter(&api, &upload);
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fixture file.txt");
    tokio::fs::write(&file, FILE_BYTES).await.unwrap();
    expect_upload_ready(
        &api,
        json!({"ok":true,"file_id":"F1","upload_url":format!("{}/upload", upload.uri())}),
    )
    .await;
    let uploaded = Arc::new(AtomicBool::new(false));
    let recorded_upload = Arc::clone(&uploaded);
    Mock::given(method("POST"))
        .and(path("/upload"))
        .and(header("content-type", "application/octet-stream"))
        .and(|request: &Request| {
            request.body == FILE_BYTES && !request.headers.contains_key("authorization")
        })
        .respond_with(move |_: &Request| {
            recorded_upload.store(true, Ordering::Relaxed);
            ResponseTemplate::new(200)
        })
        .expect(1)
        .mount(&upload)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/files.completeUploadExternal"))
        .and(header("authorization", "Bearer xoxb-test"))
        .and(body_json(
            json!({"files":[{"id":"F1","title":"Summary"}],"channel_id":"C1","thread_ts":"2.00"}),
        ))
        .and(move |_: &Request| uploaded.load(Ordering::Relaxed))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
        .expect(1)
        .mount(&api)
        .await;
    adapter
        .upload_file("C1", file.to_str().unwrap(), Some("Summary"), Some("2.00"))
        .await
        .unwrap();
}

#[tokio::test]
async fn external_file_upload_stops_after_missing_metadata_or_a_redirect() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("fixture file.txt");
    let file_path = file.to_str().unwrap();
    for (mode, expected) in [
        ("missing file", "Failed to read file"),
        ("incomplete", "missing the file ID or URL"),
        ("foreign", "invalid file upload URL"),
        ("redirect", "HTTP 307"),
        ("api error", "missing_scope"),
    ] {
        let api = MockServer::start().await;
        let upload = MockServer::start().await;
        let adapter = local_upload_adapter(&api, &upload);
        let mut expected_file_requests = 0;
        if mode != "missing file" {
            tokio::fs::write(&file, FILE_BYTES).await.unwrap();
            let upload_url = match mode {
                "incomplete" => json!({"ok":true,"file_id":"F1"}),
                "foreign" => {
                    json!({"ok":true,"file_id":"F1","upload_url":format!("{}/upload", api.uri())})
                }
                "api error" => json!({"ok":false,"error":"missing_scope"}),
                "redirect" => {
                    expected_file_requests = 1;
                    Mock::given(method("POST"))
                        .and(path("/upload"))
                        .respond_with(
                            ResponseTemplate::new(307).insert_header("Location", "/redirected"),
                        )
                        .expect(1)
                        .mount(&upload)
                        .await;
                    json!({"ok":true,"file_id":"F1","upload_url":format!("{}/upload", upload.uri())})
                }
                _ => unreachable!(),
            };
            expect_upload_ready(&api, upload_url).await;
        }
        assert!(matches!(
            adapter.upload_file("C1", file_path, None, None).await,
            Err(GatewayError::SendFailed(message)) if message.contains(expected)
        ));
        assert_eq!(
            api.received_requests().await.unwrap().len(),
            usize::from(mode != "missing file")
        );
        assert_eq!(
            upload.received_requests().await.unwrap().len(),
            expected_file_requests
        );
    }
}
