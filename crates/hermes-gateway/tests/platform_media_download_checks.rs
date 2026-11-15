//! Platform adapters' media downloads (`download_media_url`): the URL and
//! every redirect hop pass the shared `url_safety` check before the adapter
//! connects to them.
//!
//! The check caches `HERMES_ALLOW_PRIVATE_URLS` for the whole process, so
//! these tests live in their own binary, where it is always on: the mock
//! server listens on 127.0.0.1, and the metadata addresses stay refused.

use std::sync::Once;
use std::time::Duration;

use hermes_gateway::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use hermes_gateway::platforms::helpers::download_media_url_with_limit;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nimage";

fn adapter() -> BasePlatformAdapter {
    static ALLOW_PRIVATE: Once = Once::new();
    ALLOW_PRIVATE.call_once(|| std::env::set_var("HERMES_ALLOW_PRIVATE_URLS", "true"));
    BasePlatformAdapter::new("token")
}

async fn download(
    adapter: &BasePlatformAdapter,
    url: &str,
    max_bytes: u64,
) -> Result<(Vec<u8>, Option<String>), String> {
    download_media_url_with_limit(adapter, url, max_bytes).await
}

async fn redirect_to(server: &MockServer, from: &str, location: &str) {
    Mock::given(method("GET"))
        .and(path(from))
        .respond_with(ResponseTemplate::new(302).insert_header("location", location))
        .mount(server)
        .await;
}

async fn serve_png(server: &MockServer, at: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(PNG.to_vec()),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn redirects_to_metadata_addresses_are_refused_before_connecting() {
    let server = MockServer::start().await;
    let targets = [
        "http://169.254.169.254/latest/meta-data/",
        "http://metadata.google.internal/computeMetadata/v1/",
        // NAT64 and 6to4 forms of 169.254.169.254.
        "http://[64:ff9b::a9fe:a9fe]/latest/meta-data/",
        "http://[2002:a9fe:a9fe::1]/latest/meta-data/",
    ];
    for (i, target) in targets.iter().enumerate() {
        redirect_to(&server, &format!("/hop{i}"), target).await;
    }
    // A redirect through an allowed hop is checked again at the next one.
    redirect_to(&server, "/twice", &format!("{}/hop0", server.uri())).await;

    let adapter = adapter();
    let starts = (0..targets.len())
        .map(|i| format!("/hop{i}"))
        .chain(["/twice".to_string()]);
    for start in starts {
        let url = format!("{}{start}", server.uri());
        let result =
            tokio::time::timeout(Duration::from_secs(5), download(&adapter, &url, 0)).await;
        let err = match result {
            Ok(Err(err)) => err,
            Ok(Ok(_)) => panic!("{url} was downloaded"),
            Err(_) => panic!("{url} was not refused before connecting"),
        };
        assert!(err.contains("Blocked: redirect to"), "{url}: {err}");
    }
}

#[tokio::test]
async fn metadata_addresses_are_refused_as_the_first_url() {
    let adapter = adapter();
    for url in [
        "http://169.254.169.254/latest/meta-data/",
        "http://[64:ff9b::a9fe:a9fe]/latest/meta-data/",
    ] {
        let result = tokio::time::timeout(Duration::from_secs(5), download(&adapter, url, 0)).await;
        let err = match result {
            Ok(Err(err)) => err,
            Ok(Ok(_)) => panic!("{url} was downloaded"),
            Err(_) => panic!("{url} was not refused before connecting"),
        };
        assert!(err.contains("Blocked:"), "{url}: {err}");
    }
}

#[tokio::test]
async fn redirects_to_allowed_addresses_are_followed() {
    let server = MockServer::start().await;
    redirect_to(&server, "/start", "/real.png").await;
    serve_png(&server, "/real.png").await;

    let (bytes, content_type) = download(&adapter(), &format!("{}/start", server.uri()), 1024)
        .await
        .expect("download media");
    assert_eq!(bytes, PNG);
    assert_eq!(content_type.as_deref(), Some("image/png"));
}

#[tokio::test]
async fn oversized_bodies_are_refused() {
    let server = MockServer::start().await;
    serve_png(&server, "/big.png").await;

    let err = download(&adapter(), &format!("{}/big.png", server.uri()), 4)
        .await
        .expect_err("oversized body should fail");
    assert!(
        err.contains("Content-Length") || err.contains("downloaded body"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn redirects_through_the_adapters_proxy_are_checked() {
    // The mock server is the adapter's HTTP proxy, so requests for the public
    // address 8.8.8.8 reach it in absolute form and never leave the machine.
    let proxy = MockServer::start().await;
    redirect_to(&proxy, "/start", "http://169.254.169.254/latest/meta-data/").await;
    redirect_to(
        &proxy,
        "/named",
        "http://metadata.google.internal/computeMetadata/v1/",
    )
    .await;
    serve_png(&proxy, "/real.png").await;
    let adapter = adapter().with_proxy(AdapterProxyConfig {
        http_proxy: Some(proxy.uri()),
        socks_proxy: None,
    });

    let (bytes, _) = download(&adapter, "http://8.8.8.8/real.png", 1024)
        .await
        .expect("download through the proxy");
    assert_eq!(bytes, PNG);
    for start in ["/start", "/named"] {
        let url = format!("http://8.8.8.8{start}");
        let result =
            tokio::time::timeout(Duration::from_secs(5), download(&adapter, &url, 0)).await;
        let err = match result {
            Ok(Err(err)) => err,
            Ok(Ok(_)) => panic!("{url} was downloaded"),
            Err(_) => panic!("{url} was not refused before connecting"),
        };
        assert!(err.contains("Blocked: redirect to"), "{url}: {err}");
    }

    // Every request went through the proxy, and none to a refused hop.
    let requests = proxy.received_requests().await.expect("request recording");
    let urls: Vec<String> = requests.iter().map(|r| r.url.to_string()).collect();
    assert_eq!(
        urls,
        [
            "http://8.8.8.8/real.png",
            "http://8.8.8.8/start",
            "http://8.8.8.8/named"
        ],
    );
}
