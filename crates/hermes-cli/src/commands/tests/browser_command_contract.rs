use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn browser_command_reports_the_configured_loopback_endpoint_and_probe_failures() {
    if !crate::isolated_test_profile::enter(
        "commands::tests::browser_command_contract::browser_command_reports_the_configured_loopback_endpoint_and_probe_failures",
    ) {
        return;
    }
    let home = hermes_config::hermes_home();
    let mut app = build_test_app_with_stream(&home).await;
    let browser = MockServer::start().await;
    let target = format!("ws://{}/devtools/browser/fixture", browser.address());
    Mock::given(method("GET"))
        .and(path("/json/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "Browser": "Fixture browser/1.0",
            "webSocketDebuggerUrl": target,
        })))
        .expect(2)
        .mount(&browser)
        .await;

    let connected = command_output(&mut app, "/browser", &["connect", &browser.uri()]).await;
    assert!(
        connected.contains("Browser: Fixture browser/1.0"),
        "{connected}"
    );
    assert!(
        connected.contains(&format!("WebSocket target: {target}")),
        "{connected}"
    );
    assert!(connected.contains("Saved CHROME_CDP_URL"), "{connected}");
    assert_eq!(std::env::var("CHROME_CDP_URL").unwrap(), browser.uri());
    let status = command_output(&mut app, "/browser", &[]).await;
    assert!(status.contains("Browser: Fixture browser/1.0"), "{status}");

    let unavailable = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/json/version"))
        .respond_with(ResponseTemplate::new(503))
        .expect(2)
        .mount(&unavailable)
        .await;
    let failed = command_output(&mut app, "/browser", &["connect", &unavailable.uri()]).await;
    assert!(failed.contains("but probe failed"), "{failed}");
    assert!(failed.contains("503"), "{failed}");
    let status = command_output(&mut app, "/browser", &["show"]).await;
    assert!(
        status.contains(&format!("configured endpoint: {}", unavailable.uri())),
        "{status}"
    );
    assert!(status.contains("Probe error"), "{status}");

    let invalid = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/json/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not JSON"))
        .expect(1)
        .mount(&invalid)
        .await;
    let failed = command_output(&mut app, "/browser", &["connect", &invalid.uri()]).await;
    assert!(failed.contains("Browser probe parse failed"), "{failed}");

    let disconnected = command_output(&mut app, "/browser", &["disconnect"]).await;
    assert!(
        disconnected.contains("Browser CDP override removed"),
        "{disconnected}"
    );
    assert!(std::env::var_os("CHROME_CDP_URL").is_none());
    let help = command_output(&mut app, "/browser", &["unknown"]).await;
    assert!(help.contains("Usage: /browser"), "{help}");
}
