//! Media cache downloads: the URL and every redirect hop pass the shared
//! `url_safety` check before the cache connects to them.
//!
//! The check caches `HERMES_ALLOW_PRIVATE_URLS` for the whole process, so
//! these tests live in their own binary, where it is always on: the mock
//! server listens on 127.0.0.1, and the metadata addresses stay refused.

use std::sync::Once;
use std::time::Duration;

use hermes_gateway::media::{MediaCache, MediaCacheConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nimage";

fn cache(dir: &tempfile::TempDir, max_file_size: u64) -> MediaCache {
    static ALLOW_PRIVATE: Once = Once::new();
    ALLOW_PRIVATE.call_once(|| std::env::set_var("HERMES_ALLOW_PRIVATE_URLS", "true"));
    MediaCache::new(&MediaCacheConfig {
        cache_dir: dir.path().to_string_lossy().to_string(),
        max_size: 0,
        ttl_seconds: 0,
        max_file_size,
    })
    .unwrap()
}

async fn redirect_to(server: &MockServer, from: &str, location: &str) {
    Mock::given(method("GET"))
        .and(path(from))
        .respond_with(ResponseTemplate::new(302).insert_header("location", location))
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

    let dir = tempfile::tempdir().unwrap();
    let cache = cache(&dir, 0);
    let starts = (0..targets.len())
        .map(|i| format!("/hop{i}"))
        .chain(["/twice".to_string()]);
    for start in starts {
        let url = format!("{}{start}", server.uri());
        let result =
            tokio::time::timeout(Duration::from_secs(5), cache.cache_image(&url, "meta.png")).await;
        let err = match result {
            Ok(Err(err)) => err.to_string(),
            Ok(Ok(path)) => panic!("{url} was cached at {}", path.display()),
            Err(_) => panic!("{url} was followed to an unreachable host"),
        };
        assert!(err.contains("Blocked: redirect to"), "{url}: {err}");
    }
    assert!(!dir.path().join("images/meta.png").exists());
}

#[tokio::test]
async fn redirects_to_allowed_addresses_are_followed() {
    let server = MockServer::start().await;
    redirect_to(&server, "/start", "/real.png").await;
    Mock::given(method("GET"))
        .and(path("/real.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(PNG),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let cached = cache(&dir, 0)
        .cache_image(&format!("{}/start", server.uri()), "real.png")
        .await
        .unwrap();
    assert_eq!(std::fs::read(cached).unwrap(), PNG);
}

#[tokio::test]
async fn cache_file_rejects_oversized_content_length_before_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/big.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"oversized".as_slice()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let err = cache(&dir, 4)
        .cache_document(&format!("{}/big.pdf", server.uri()), "big.pdf")
        .await
        .unwrap_err();

    assert!(err.to_string().contains("Content-Length"));
    assert!(!dir.path().join("documents").join("big.pdf").exists());
}
