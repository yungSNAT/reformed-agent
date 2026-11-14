//! Media cache downloads with the default setting, private URLs refused.
//!
//! The shared `url_safety` check caches `HERMES_ALLOW_PRIVATE_URLS` for the
//! whole process, so this binary never sets it.

use std::time::Duration;

use hermes_gateway::media::{MediaCache, MediaCacheConfig};
use wiremock::MockServer;

#[tokio::test]
async fn private_urls_are_refused_before_connecting() {
    assert!(std::env::var_os("HERMES_ALLOW_PRIVATE_URLS").is_none());
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let cache = MediaCache::new(&MediaCacheConfig {
        cache_dir: dir.path().to_string_lossy().to_string(),
        max_size: 0,
        ttl_seconds: 0,
        max_file_size: 0,
    })
    .unwrap();

    let port = server.address().port();
    // 127.0.0.1 in its NAT64 form and directly.
    for url in [
        format!("http://[64:ff9b::7f00:1]:{port}/nat64.png"),
        format!("{}/direct.png", server.uri()),
    ] {
        let result =
            tokio::time::timeout(Duration::from_secs(5), cache.cache_image(&url, "x.png")).await;
        let err = match result {
            Ok(Err(err)) => err.to_string(),
            Ok(Ok(path)) => panic!("{url} was cached at {}", path.display()),
            Err(_) => panic!("{url} was not refused before connecting"),
        };
        assert!(err.contains("Blocked:"), "{url}: {err}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(!dir.path().join("images/x.png").exists());
}
