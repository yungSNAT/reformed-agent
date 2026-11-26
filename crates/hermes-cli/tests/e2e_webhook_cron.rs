//! E2E: cron 完成事件经 `webhooks.json` POST 到外部 HTTP（WireMock 收 webhook）。

use hermes_cli::webhook_delivery::{
    deliver_cron_completion_to_webhooks, save_webhook_store, webhook_http_client, WebhookRecord,
    WebhookStore,
};
use hermes_core::{AgentResult, Message};
use hermes_cron::{CronCompletionEvent, CronJob};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[path = "support/isolated_hermes.rs"]
mod isolated;

#[tokio::test]
async fn webhook_cli_test_sends_its_default_and_custom_payloads_to_loopback() {
    use hmac::Mac;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/webhooks/fixture"))
        .respond_with(ResponseTemplate::new(202).set_body_string("fixture accepted"))
        .expect(2)
        .mount(&server)
        .await;
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.yaml"),
        format!(
            "platforms:\n  webhook:\n    enabled: true\n    host: 0.0.0.0\n    port: {}\n",
            server.address().port()
        ),
    )
    .unwrap();

    let signing_value = "synthetic-subscription-value";
    let custom_payload = r#"{"event_type":"test","message":"Estuary survey complete"}"#;
    for args in [
        &["subscribe", "fixture", "--secret", signing_value][..],
        &["test", "fixture"],
        &["test", "fixture", "--payload", custom_payload],
    ] {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            isolated::isolated_hermes(sandbox.path())
                .arg("webhook")
                .args(args)
                .output(),
        )
        .await
        .expect("webhook CLI timed out")
        .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success(), "{args:?}: {stdout}");
        if args[0] == "test" {
            assert!(stdout.contains(&format!(
                "Test POST {}/webhooks/fixture -> 202 Accepted",
                server.uri()
            )));
            assert!(stdout.contains("fixture accepted"));
        }
        assert!(!stdout.contains(signing_value));
    }

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].body_json::<serde_json::Value>().unwrap(),
        serde_json::json!({
            "test": true, "event_type": "test", "message": "Hello from hermes webhook test"
        })
    );
    assert_eq!(requests[1].body, custom_payload.as_bytes());
    for request in requests {
        assert_eq!(request.headers["content-type"], "application/json");
        assert_eq!(request.headers["x-github-event"], "test");
        let encoded = request.headers["x-hub-signature-256"]
            .to_str()
            .unwrap()
            .strip_prefix("sha256=")
            .unwrap();
        let mut signature =
            hmac::Hmac::<sha2::Sha256>::new_from_slice(signing_value.as_bytes()).unwrap();
        signature.update(&request.body);
        signature
            .verify_slice(&hex::decode(encoded).unwrap())
            .unwrap();
    }
}

#[tokio::test]
async fn cron_completion_delivers_json_to_registered_webhook_url() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/notify"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let webhooks_path = dir.path().join("webhooks.json");
    let store = WebhookStore {
        webhooks: vec![WebhookRecord {
            id: "w1".to_string(),
            url: format!("{}/notify", mock.uri()),
            created_at: "2020-01-01T00:00:00Z".to_string(),
        }],
    };
    save_webhook_store(&webhooks_path, &store).expect("write webhooks.json");

    let job = CronJob::new("0 * * * *", "ping");
    let result = AgentResult {
        messages: vec![Message::assistant("done")],
        finished_naturally: true,
        total_turns: 1,
        tool_errors: vec![],
        usage: None,
        ..Default::default()
    };
    let event = CronCompletionEvent::new(&job, "schedule", Ok(&result));

    let client = webhook_http_client().expect("http client");
    deliver_cron_completion_to_webhooks(&webhooks_path, &event, &client)
        .await
        .expect("delivery");

    mock.verify().await;

    let reqs = mock.received_requests().await.expect("requests");
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value =
        serde_json::from_slice(reqs[0].body.as_slice()).expect("json body");
    assert_eq!(body["event"], "cron_job_finished");
    assert_eq!(body["job_id"], job.id);
    assert_eq!(body["trigger"], "schedule");
    assert_eq!(body["ok"], true);
    assert_eq!(body["assistant_snippet"], "done");
}
