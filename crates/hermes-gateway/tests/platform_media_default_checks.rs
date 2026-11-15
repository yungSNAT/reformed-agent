//! A platform adapter sending an image URL, with the default setting: a
//! private URL is refused before the adapter connects, and the image goes
//! as text instead.
//!
//! The shared `url_safety` check caches `HERMES_ALLOW_PRIVATE_URLS` for the
//! whole process, so this binary never sets it.
#![cfg(feature = "signal")]

use std::time::Duration;

use hermes_gateway::adapter::AdapterProxyConfig;
use hermes_gateway::platforms::signal::{SignalAdapter, SignalConfig};
use hermes_gateway::PlatformAdapter;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn private_image_urls_are_not_fetched() {
    assert!(std::env::var_os("HERMES_ALLOW_PRIVATE_URLS").is_none());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/send"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"timestamp": 1})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/image.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNG\r\n\x1a\nimage".to_vec()))
        .mount(&server)
        .await;
    let adapter = SignalAdapter::new(SignalConfig {
        phone_number: "+15551234567".to_string(),
        api_url: server.uri(),
        proxy: AdapterProxyConfig::default(),
    })
    .expect("adapter");

    let port = server.address().port();
    // 127.0.0.1 directly and in its NAT64 form.
    for url in [
        format!("{}/image.png", server.uri()),
        format!("http://[64:ff9b::7f00:1]:{port}/image.png"),
    ] {
        tokio::time::timeout(
            Duration::from_secs(5),
            adapter.send_image_url("+15559876543", &url, None),
        )
        .await
        .unwrap_or_else(|_| panic!("{url} was not refused before connecting"))
        .unwrap_or_else(|err| panic!("{url} did not fall back to text: {err}"));

        let requests = server.received_requests().await.unwrap();
        let fetched = requests
            .iter()
            .filter(|request| request.url.path() == "/image.png")
            .count();
        assert_eq!(fetched, 0, "{url} was fetched");
        assert!(
            requests
                .iter()
                .any(|request| String::from_utf8_lossy(&request.body).contains(&url)),
            "{url} was not sent as text"
        );
    }
}
