use super::*;
use std::collections::BTreeMap;
use std::time::Duration;
use wiremock::matchers::{basic_auth, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ACCOUNT: &str = "fixture-account";
const KEY: &str = "fixture-key";
const FROM: &str = "+10000000000";
const TO: &str = "+10000000001";
const MESSAGES_PATH: &str = "/2010-04-01/Accounts/fixture-account/Messages.json";

fn fixture_adapter(server: &MockServer) -> SmsAdapter {
    let mut adapter = SmsAdapter::new(SmsConfig {
        provider: "twilio".into(),
        account_sid: ACCOUNT.into(),
        auth_token: KEY.into(),
        from_number: FROM.into(),
        proxy: AdapterProxyConfig::default(),
    })
    .unwrap();
    assert_eq!(
        adapter.messages_url,
        format!("https://api.twilio.com{MESSAGES_PATH}")
    );
    adapter.messages_url = format!("{}{MESSAGES_PATH}", server.uri());
    adapter.client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    adapter
}

#[tokio::test]
async fn sms_and_mms_submit_the_expected_fields_and_local_attachment_label() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(MESSAGES_PATH))
        .and(basic_auth(ACCOUNT, KEY))
        .respond_with(ResponseTemplate::new(201))
        .expect(3)
        .mount(&server)
        .await;
    let adapter = fixture_adapter(&server);
    let media = "https://example.invalid/fixture.png";
    adapter
        .send_message(TO, "Sample & notes", None)
        .await
        .unwrap();
    adapter
        .send_file(TO, media, Some("Image caption"))
        .await
        .unwrap();
    adapter
        .send_file(TO, "fixtures/notes.txt", Some("Local caption"))
        .await
        .unwrap();

    server.verify().await;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    for (request, (body, media)) in requests.iter().zip([
        ("Sample & notes", None),
        ("Image caption", Some(media)),
        ("[Attachment: notes.txt] Local caption", None),
    ]) {
        let fields: BTreeMap<_, _> = url::form_urlencoded::parse(&request.body).collect();
        assert_eq!(fields.get("To").map(|value| value.as_ref()), Some(TO));
        assert_eq!(fields.get("From").map(|value| value.as_ref()), Some(FROM));
        assert_eq!(fields.get("Body").map(|value| value.as_ref()), Some(body));
        assert_eq!(fields.get("MediaUrl").map(|value| value.as_ref()), media);
        assert_eq!(fields.len(), 3 + usize::from(media.is_some()));
    }
}

#[tokio::test]
async fn sms_and_mms_report_api_rejections_and_request_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(MESSAGES_PATH))
        .and(basic_auth(ACCOUNT, KEY))
        .respond_with(ResponseTemplate::new(400).set_body_string("Fixture rejection"))
        .expect(2)
        .mount(&server)
        .await;
    let mut adapter = fixture_adapter(&server);
    let media = "https://example.invalid/fixture.png";
    let error = adapter.send_message(TO, "Sample", None).await.unwrap_err();
    assert!(
        matches!(error, GatewayError::SendFailed(text) if text == "Twilio API error: Fixture rejection")
    );
    let error = adapter.send_file(TO, media, None).await.unwrap_err();
    assert!(
        matches!(error, GatewayError::SendFailed(text) if text == "Twilio MMS error: Fixture rejection")
    );
    server.verify().await;

    adapter.messages_url = "http://[".into();
    let error = adapter.send_message(TO, "Sample", None).await.unwrap_err();
    assert!(
        matches!(error, GatewayError::SendFailed(text) if text.starts_with("Twilio send failed:"))
    );
    let error = adapter.send_file(TO, media, None).await.unwrap_err();
    assert!(
        matches!(error, GatewayError::SendFailed(text) if text.starts_with("Twilio MMS send failed:"))
    );
}
