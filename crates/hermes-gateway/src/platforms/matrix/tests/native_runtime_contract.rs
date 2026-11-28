use super::*;
use serde_json::json;
use std::time::Duration;
use wiremock::matchers::{body_json, body_partial_json, header, path_regex};

fn adapter_for_matrix_fixture(server: &MockServer) -> MatrixAdapter {
    let mut adapter = MatrixAdapter::new(MatrixConfig {
        homeserver_url: server.uri(),
        user_id: "@fixture:test".into(),
        access_token: "matrix-fixture".into(),
        room_id: None,
        proxy: AdapterProxyConfig::default(),
    })
    .unwrap();
    adapter.client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    adapter.e2ee.client = adapter.client.clone();
    adapter.decrypt_ffi = None;
    adapter.native_decrypt = Some(MatrixNativeDecryptConfig {
        device_id_override: None,
    });
    adapter
}

#[tokio::test]
async fn matrix_messaging_sends_the_expected_relations_and_typing_state() {
    let server = MockServer::start().await;
    let adapter = adapter_for_matrix_fixture(&server);
    Mock::given(path_regex(r"^/_matrix/client/v3/rooms/"))
        .and(header("Authorization", "Bearer matrix-fixture"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"event_id": "$fixture"})))
        .expect(10)
        .mount(&server)
        .await;
    let room = "!room:test";
    let event = "$original";
    for (text, mode) in [
        ("Fixture", None),
        ("<b>Fixture</b>", Some(ParseMode::Html)),
        ("**Fixture**", Some(ParseMode::Markdown)),
    ] {
        assert_eq!(
            adapter.send_text(room, text, mode).await.unwrap(),
            "$fixture"
        );
    }
    adapter
        .edit_text(room, event, "Updated fixture")
        .await
        .unwrap();
    assert_eq!(
        adapter.send_reaction(room, event, "👍").await.unwrap(),
        "$fixture"
    );
    for reason in [None, Some("Stale fixture")] {
        assert_eq!(
            adapter.redact_event(room, event, reason).await.unwrap(),
            "$fixture"
        );
    }
    adapter.send_read_receipt(room, event).await.unwrap();
    adapter.send_typing(room, true, None).await.unwrap();
    adapter.send_typing(room, false, None).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 10);
    let mut transactions = std::collections::HashSet::new();
    for request in &requests[..7] {
        let transaction = request.url.path_segments().unwrap().next_back().unwrap();
        assert!(!transaction.is_empty() && transactions.insert(transaction));
    }
    for (request, (verb, expected_path, body)) in requests.iter().zip([
        (
            "PUT",
            "send/m.room.message/",
            json!({"msgtype":"m.text","body":"Fixture"}),
        ),
        (
            "PUT",
            "send/m.room.message/",
            json!({
                "msgtype":"m.text", "body":"<b>Fixture</b>", "format":"org.matrix.custom.html",
                "formatted_body":"<b>Fixture</b>"
            }),
        ),
        (
            "PUT",
            "send/m.room.message/",
            json!({
                "msgtype":"m.text", "body":"**Fixture**", "format":"org.matrix.custom.html",
                "formatted_body":"<strong>Fixture</strong>"
            }),
        ),
        (
            "PUT",
            "send/m.room.message/",
            json!({
                "msgtype":"m.text", "body":"* Updated fixture",
                "m.new_content":{"msgtype":"m.text","body":"Updated fixture"},
                "m.relates_to":{"rel_type":"m.replace","event_id":event}
            }),
        ),
        (
            "PUT",
            "send/m.reaction/",
            json!({
                "m.relates_to":{"rel_type":"m.annotation","event_id":event,"key":"👍"}
            }),
        ),
        ("PUT", "redact/$original/", json!({})),
        (
            "PUT",
            "redact/$original/",
            json!({"reason":"Stale fixture"}),
        ),
        ("POST", "receipt/m.read/$original", json!({})),
        (
            "PUT",
            "typing/@fixture:test",
            json!({"typing":true,"timeout":30_000}),
        ),
        ("PUT", "typing/@fixture:test", json!({"typing":false})),
    ]) {
        assert_eq!(request.method.as_str(), verb);
        let decoded = urlencoding::decode(request.url.path()).unwrap();
        let expected = format!("/_matrix/client/v3/rooms/{room}/{expected_path}");
        if expected.ends_with('/') {
            assert_eq!(
                decoded.rsplit_once('/').unwrap().0,
                expected.trim_end_matches('/')
            );
        } else {
            assert_eq!(decoded, expected);
        }
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
            body
        );
    }
}

#[tokio::test]
async fn matrix_messaging_keeps_homeserver_errors_in_send_failures() {
    let server = MockServer::start().await;
    let adapter = adapter_for_matrix_fixture(&server);
    Mock::given(path_regex(r"^/_matrix/client/v3/rooms/"))
        .respond_with(ResponseTemplate::new(403).set_body_string("fixture denied"))
        .expect(6)
        .mount(&server)
        .await;
    let room = "!room:test";
    let event = "$original";
    for (result, detail) in [
        (
            adapter.send_text(room, "Fixture", None).await.map(drop),
            "Matrix API error",
        ),
        (
            adapter.edit_text(room, event, "Updated").await,
            "Matrix edit API error",
        ),
        (
            adapter.send_reaction(room, event, "👍").await.map(drop),
            "Matrix reaction error",
        ),
        (
            adapter.redact_event(room, event, None).await.map(drop),
            "Matrix redact error",
        ),
        (
            adapter.send_read_receipt(room, event).await,
            "Matrix read receipt error",
        ),
        (
            adapter.send_typing(room, true, Some(5_000)).await,
            "Matrix typing error",
        ),
    ] {
        assert!(matches!(
            result,
            Err(GatewayError::SendFailed(error))
                if error.contains(detail) && error.contains("fixture denied")
        ));
    }
}

#[tokio::test]
async fn matrix_typing_reports_a_transport_failure() {
    let server = MockServer::start().await;
    let mut adapter = adapter_for_matrix_fixture(&server);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    adapter.config.homeserver_url = format!("http://{}", listener.local_addr().unwrap());
    let ((), result) = tokio::time::timeout(Duration::from_secs(7), async {
        tokio::join!(
            async move { drop(listener.accept().await.unwrap()) },
            adapter.send_typing("!room:test", true, None)
        )
    })
    .await
    .expect("typing request and disconnect timed out");
    assert!(matches!(
        result,
        Err(GatewayError::SendFailed(message))
            if message.starts_with("Matrix typing indicator failed:")
    ));
}

#[tokio::test]
async fn native_http_distinguishes_request_errors_from_truncated_responses() {
    use ruma::api::client::account::whoami::v3::Request;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let server = MockServer::start().await;
    let mut adapter = adapter_for_matrix_fixture(&server);
    adapter.config.homeserver_url = "http://[".into();
    assert!(matches!(
        adapter.build_ruma_request(Request::new(), "whoami"),
        Err(GatewayError::ConnectionFailed(detail)) if detail.contains("whoami build request failed")
    ));
    let relative = http::Request::get("/fixture").body(Vec::new()).unwrap();
    assert!(matches!(
        adapter.execute_ruma_request(relative, "whoami").await,
        Err(GatewayError::ConnectionFailed(detail)) if detail.contains("whoami invalid request URI")
    ));

    for (truncated, expected) in [
        (false, "whoami request failed"),
        (true, "whoami read response failed"),
    ] {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        adapter.config.homeserver_url = format!("http://{}", listener.local_addr().unwrap());
        let request = adapter
            .build_ruma_request(Request::new(), "whoami")
            .unwrap();
        let serve = async {
            let mut socket = BufReader::new(listener.accept().await.unwrap().0);
            let mut request_line = String::new();
            socket.read_line(&mut request_line).await.unwrap();
            assert_eq!(
                request_line,
                "GET /_matrix/client/v3/account/whoami HTTP/1.1\r\n"
            );
            let mut authorization_seen = false;
            loop {
                let mut header = String::new();
                assert_ne!(socket.read_line(&mut header).await.unwrap(), 0);
                if header == "\r\n" {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("authorization") {
                    assert!(!authorization_seen);
                    assert_eq!(value.trim(), "Bearer matrix-fixture");
                    authorization_seen = true;
                }
            }
            assert!(authorization_seen);
            if truncated {
                socket
                    .get_mut()
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\nshort",
                    )
                    .await
                    .unwrap();
            }
            socket.get_mut().shutdown().await.unwrap();
        };
        let ((), result) = tokio::time::timeout(Duration::from_secs(7), async {
            tokio::join!(serve, adapter.execute_ruma_request(request, "whoami"))
        })
        .await
        .expect("native Matrix request and fixture response timed out");
        assert!(matches!(
            result,
            Err(GatewayError::ConnectionFailed(detail)) if detail.contains(expected)
        ));
    }
}

#[tokio::test]
async fn native_runtime_reuses_whoami_device_and_reconciles_generated_keys() {
    let server = MockServer::start().await;
    let adapter = adapter_for_matrix_fixture(&server);
    Mock::given(method("GET"))
        .and(path("/_matrix/client/v3/account/whoami"))
        .and(header("Authorization", "Bearer matrix-fixture"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user_id": "@alias:test",
            "device_id": "FIXTUREDEVICE",
        })))
        .expect(1)
        .mount(&server)
        .await;
    let uploaded_device = Arc::new(std::sync::Mutex::new(None));
    let device_for_upload = Arc::clone(&uploaded_device);
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/keys/upload"))
        .and(header("Authorization", "Bearer matrix-fixture"))
        .and(body_partial_json(json!({
            "device_keys": {"user_id": "@fixture:test", "device_id": "FIXTUREDEVICE"},
        })))
        .respond_with(move |request: &wiremock::Request| {
            let payload: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            *device_for_upload.lock().unwrap() = Some(payload["device_keys"].clone());
            let count = payload["one_time_keys"].as_object().unwrap().len();
            ResponseTemplate::new(200).set_body_json(json!({
                "one_time_key_counts": {"signed_curve25519": count},
            }))
        })
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/keys/query"))
        .and(header("Authorization", "Bearer matrix-fixture"))
        .and(body_partial_json(
            json!({"device_keys": {"@fixture:test": []}}),
        ))
        .respond_with(move |_: &wiremock::Request| {
            let device = uploaded_device.lock().unwrap().clone().unwrap();
            ResponseTemplate::new(200).set_body_json(json!({
                "device_keys": {"@fixture:test": {"FIXTUREDEVICE": device}},
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let runtime = adapter.ensure_native_runtime().await.unwrap().unwrap();
    assert_eq!(runtime.machine.user_id().as_str(), "@fixture:test");
    assert_eq!(runtime.machine.device_id().as_str(), "FIXTUREDEVICE");
    let reused = adapter.ensure_native_runtime().await.unwrap().unwrap();
    assert!(Arc::ptr_eq(&runtime, &reused));
    for _ in 0..2 {
        tokio::time::timeout(
            Duration::from_secs(10),
            adapter.process_native_outgoing_requests(&runtime),
        )
        .await
        .expect("generated Matrix requests must drain")
        .unwrap();
    }
}

#[tokio::test]
async fn native_outgoing_dispatch_sends_keys_and_room_requests() {
    use matrix_sdk_crypto::types::requests::RoomMessageRequest;
    use ruma::events::room::message::RoomMessageEventContent;

    let server = MockServer::start().await;
    let mut adapter = adapter_for_matrix_fixture(&server);
    adapter.native_decrypt = Some(MatrixNativeDecryptConfig {
        device_id_override: Some("FIXTUREDEVICE".into()),
    });
    let runtime = adapter.ensure_native_runtime().await.unwrap().unwrap();
    let peer_keys = BTreeMap::from([(
        ruma::user_id!("@peer:test").to_owned(),
        BTreeMap::from([(
            ruma::device_id!("PEERDEVICE").to_owned(),
            OneTimeKeyAlgorithm::SignedCurve25519,
        )]),
    )]);
    let signature_payload = json!({
        "@fixture:test": {
            "FIXTUREDEVICE": {
                "user_id": "@fixture:test",
                "device_id": "FIXTUREDEVICE",
                "algorithms": ["m.olm.v1.curve25519-aes-sha2"],
                "keys": {"ed25519:FIXTUREDEVICE": "fixturePublicKey"},
                "signatures": {"@fixture:test": {"ed25519:FIXTUREDEVICE": "fixtureSignature"}}
            }
        }
    });
    let transaction = ruma::TransactionId::new();
    let content = RoomMessageEventContent::text_plain("Working notes");
    let requests = [
        (
            AnyOutgoingRequest::from(claim_keys::v3::Request::new(peer_keys)),
            Mock::given(method("POST"))
                .and(path("/_matrix/client/v3/keys/claim"))
                .and(body_partial_json(json!({
                    "one_time_keys": {"@peer:test": {"PEERDEVICE": "signed_curve25519"}},
                }))),
            json!({"one_time_keys": {}}),
        ),
        (
            AnyOutgoingRequest::from(upload_signatures::v3::Request::new(
                serde_json::from_value(signature_payload.clone()).unwrap(),
            )),
            Mock::given(method("POST"))
                .and(path("/_matrix/client/v3/keys/signatures/upload"))
                .and(body_json(signature_payload)),
            json!({"failures": {}}),
        ),
        (
            AnyOutgoingRequest::from(RoomMessageRequest {
                room_id: ruma::room_id!("!room:test").to_owned(),
                txn_id: transaction.clone(),
                content: Box::new(content.into()),
            }),
            Mock::given(method("PUT"))
                .and(path_regex(format!(
                    r"^/_matrix/client/v3/rooms/(?:!|%21)room(?::|%3[Aa])test/send/m\.room\.message/{transaction}$"
                )))
                .and(body_json(json!({"msgtype": "m.text", "body": "Working notes"}))),
            json!({"event_id": "$fixture"}),
        ),
    ];
    for (request, mock, response) in requests {
        mock.and(header("Authorization", "Bearer matrix-fixture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
        adapter
            .send_native_outgoing_request(&runtime, &transaction, &request)
            .await
            .unwrap();
    }
    server.verify().await;
}

#[tokio::test]
async fn native_to_device_delivers_an_event_and_reports_homeserver_failures() {
    let server = MockServer::start().await;
    let mut adapter = adapter_for_matrix_fixture(&server);
    adapter.native_decrypt = Some(MatrixNativeDecryptConfig {
        device_id_override: Some("FIXTUREDEVICE".into()),
    });
    let runtime = adapter.ensure_native_runtime().await.unwrap().unwrap();

    for (transaction, status, response) in [
        ("fixture-ready", 200, json!({})),
        (
            "fixture-retry",
            503,
            json!({"errcode": "M_UNKNOWN", "error": "fixture homeserver unavailable"}),
        ),
    ] {
        let messages = json!({"@fixture:test": {"FIXTUREDEVICE": {}}});
        let request: ToDeviceRequest = serde_json::from_value(json!({
            "event_type": "m.dummy",
            "txn_id": transaction,
            "messages": messages,
        }))
        .unwrap();
        Mock::given(method("PUT"))
            .and(path(format!(
                "/_matrix/client/v3/sendToDevice/m.dummy/{transaction}"
            )))
            .and(header("Authorization", "Bearer matrix-fixture"))
            .and(body_partial_json(json!({"messages": messages})))
            .respond_with(ResponseTemplate::new(status).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = adapter
            .send_native_to_device(&runtime, &request.txn_id, &request)
            .await;
        if status == 200 {
            outcome.unwrap();
        } else {
            assert!(matches!(
                outcome,
                Err(GatewayError::ConnectionFailed(message))
                    if message.contains("sendToDevice response parse failed")
                        && message.contains("fixture homeserver unavailable")
            ));
        }
    }
}

#[tokio::test]
async fn matrix_whoami_reports_http_invalid_json_and_missing_device() {
    for (response, detail) in [
        (
            ResponseTemplate::new(403).set_body_string("fixture denied"),
            "fixture denied",
        ),
        (
            ResponseTemplate::new(200).set_body_string("not JSON"),
            "whoami parse",
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({
                "user_id": "@fixture:test", "device_id": " ",
            })),
            "did not return device_id",
        ),
    ] {
        let server = MockServer::start().await;
        let adapter = adapter_for_matrix_fixture(&server);
        Mock::given(method("GET"))
            .and(path("/_matrix/client/v3/account/whoami"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        assert!(matches!(
            adapter.ensure_native_runtime().await,
            Err(GatewayError::ConnectionFailed(error)) if error.contains(detail)
        ));
        assert!(adapter.native_runtime.lock().await.is_none());
    }
}

#[tokio::test]
async fn matrix_sync_validates_each_crypto_field_and_uses_device_override_offline() {
    let server = MockServer::start().await;
    let mut adapter = adapter_for_matrix_fixture(&server);
    adapter.native_decrypt = None;
    assert!(adapter
        .try_native_decrypt_event("!fixture:test", &json!({}))
        .await
        .unwrap()
        .is_none());
    adapter.native_decrypt = Some(MatrixNativeDecryptConfig {
        device_id_override: Some("OVERRIDEDEVICE".into()),
    });
    let runtime = adapter.ensure_native_runtime().await.unwrap().unwrap();
    assert_eq!(runtime.machine.device_id().as_str(), "OVERRIDEDEVICE");
    assert!(matches!(
        adapter.try_native_decrypt_event("invalid-room", &json!({})).await,
        Err(GatewayError::ConnectionFailed(error)) if error.contains("Invalid Matrix room_id")
    ));

    adapter
        .process_native_sync_changes(
            &runtime,
            &json!({
                "next_batch": "batch-fixture",
                "to_device": {"events": []},
                "device_lists": {"changed": ["@alias:test"], "left": []},
                "device_one_time_keys_count": {"signed_curve25519": 50},
                "device_unused_fallback_key_types": ["signed_curve25519"],
            }),
        )
        .await
        .unwrap();
    for (body, field) in [
        (json!({"to_device": {"events": true}}), "to-device"),
        (json!({"device_lists": false}), "device_lists"),
        (
            json!({"device_one_time_keys_count": {"signed_curve25519": "invalid"}}),
            "one-time key count",
        ),
        (
            json!({"device_unused_fallback_key_types": true}),
            "fallback key",
        ),
    ] {
        let expected = format!("{field} parse failed");
        assert!(matches!(
            adapter.process_native_sync_changes(&runtime, &body).await,
            Err(GatewayError::ConnectionFailed(error)) if error.contains(&expected)
        ));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn matrix_encryption_state_caches_only_confirmed_rooms() {
    let server = MockServer::start().await;
    let adapter = adapter_for_matrix_fixture(&server);
    for (room, response, requests) in [
        (
            "encrypted",
            ResponseTemplate::new(200).set_body_json(json!({"algorithm": "m.megolm.v1.aes-sha2"})),
            1,
        ),
        ("missing", ResponseTemplate::new(404), 2),
        (
            "unconfigured",
            ResponseTemplate::new(200).set_body_json(json!({"algorithm": ""})),
            2,
        ),
        (
            "unavailable",
            ResponseTemplate::new(503).set_body_string("fixture unavailable"),
            2,
        ),
        (
            "invalid",
            ResponseTemplate::new(200).set_body_string("invalid JSON"),
            2,
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/_matrix/client/v3/rooms/{room}/state/m.room.encryption/"
            )))
            .and(header("Authorization", "Bearer matrix-fixture"))
            .respond_with(response)
            .expect(requests)
            .mount(&server)
            .await;
    }

    for _ in 0..2 {
        assert!(adapter.e2ee.is_encrypted_room("encrypted").await.unwrap());
        assert!(!adapter.e2ee.is_encrypted_room("missing").await.unwrap());
        assert!(!adapter
            .e2ee
            .is_encrypted_room("unconfigured")
            .await
            .unwrap());
        assert!(matches!(
            adapter.e2ee.is_encrypted_room("unavailable").await,
            Err(GatewayError::ConnectionFailed(message)) if message.contains("fixture unavailable")
        ));
        assert!(matches!(
            adapter.e2ee.is_encrypted_room("invalid").await,
            Err(GatewayError::ConnectionFailed(message)) if message.contains("encryption state parse failed")
        ));
    }
    assert!(adapter.e2ee.is_room_marked_encrypted("encrypted"));
    for room in ["missing", "unconfigured", "unavailable", "invalid"] {
        assert!(!adapter.e2ee.is_room_marked_encrypted(room), "{room}");
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 9);
}

#[tokio::test]
async fn matrix_room_key_claims_skip_rooms_without_joined_peer_devices() {
    let server = MockServer::start().await;
    let adapter = adapter_for_matrix_fixture(&server);
    let own_member = json!({"state_key": "@fixture:test", "content": {"membership": "join"}});
    let left_member = json!({"state_key": "@left:test", "content": {"membership": "leave"}});
    let peer_member = json!({"state_key": "@peer:test", "content": {"membership": "join"}});
    for (room, members) in [
        ("no-peers", vec![own_member.clone(), left_member]),
        ("no-devices", vec![own_member, peer_member]),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/_matrix/client/v3/rooms/{room}/members")))
            .and(header("Authorization", "Bearer matrix-fixture"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"chunk": members})))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/keys/query"))
        .and(header("Authorization", "Bearer matrix-fixture"))
        .and(body_json(json!({"device_keys": {"@peer:test": []}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"device_keys": {}})))
        .expect(2)
        .mount(&server)
        .await;

    assert_eq!(adapter.e2ee.share_room_keys("no-peers").await.unwrap(), 0);
    assert_eq!(adapter.e2ee.share_room_keys("no-devices").await.unwrap(), 0);
    assert!(matches!(
        adapter.e2ee.verify_device_keys("@peer:test").await,
        Err(GatewayError::Platform(message)) if message == "No device keys published for user @peer:test"
    ));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    assert!(!requests
        .iter()
        .any(|request| request.url.path().ends_with("/keys/claim")));
}
