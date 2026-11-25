use super::*;
use serde_json::json;
use std::time::Duration;
use wiremock::matchers::{body_partial_json, header};

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
    adapter.decrypt_ffi = None;
    adapter.native_decrypt = Some(MatrixNativeDecryptConfig {
        device_id_override: None,
    });
    adapter
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
