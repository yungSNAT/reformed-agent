#![cfg(feature = "gateway-webhook")]

use super::*;
use serde_json::json;

fn webhook_platform_cfg() -> PlatformConfig {
    PlatformConfig {
        enabled: true,
        ..PlatformConfig::default()
    }
}

#[test]
fn build_webhook_config_uses_defaults_when_extra_is_empty() {
    let cfg = webhook_platform_cfg();
    let wh = build_webhook_config(&cfg, "s3cret".to_string());
    assert_eq!(wh.host, "0.0.0.0");
    assert_eq!(wh.port, 9000);
    assert_eq!(wh.path, "/webhook");
    assert_eq!(wh.secret, "s3cret");
    assert_eq!(wh.rate_limit, 30);
    assert_eq!(wh.max_body_bytes, 1_048_576);
}

#[test]
fn build_webhook_config_reads_values_from_extra() {
    let mut cfg = webhook_platform_cfg();
    cfg.extra.insert("host".to_string(), json!("127.0.0.1"));
    cfg.extra.insert("port".to_string(), json!(9100));
    cfg.extra.insert("path".to_string(), json!("/hooks/in"));
    cfg.extra.insert("rate_limit".to_string(), json!(5));
    cfg.extra.insert("max_body_bytes".to_string(), json!(2048));
    let wh = build_webhook_config(&cfg, "other-secret".to_string());
    assert_eq!(wh.host, "127.0.0.1");
    assert_eq!(wh.port, 9100);
    assert_eq!(wh.path, "/hooks/in");
    assert_eq!(wh.secret, "other-secret");
    assert_eq!(wh.rate_limit, 5);
    assert_eq!(wh.max_body_bytes, 2048);
}

#[test]
fn build_webhook_config_falls_back_on_invalid_numeric_extras_and_routes() {
    let mut cfg = webhook_platform_cfg();
    // Too large for u32 -> falls back to default rate limit.
    cfg.extra.insert("rate_limit".to_string(), json!(u64::MAX));
    // Negative values are not u64 -> falls back to default body size.
    cfg.extra.insert("max_body_bytes".to_string(), json!(-1));
    // Routes that cannot be deserialized fall back to defaults.
    cfg.extra.insert("routes".to_string(), json!(42));
    let wh = build_webhook_config(&cfg, "x".to_string());
    assert_eq!(wh.rate_limit, 30);
    assert_eq!(wh.max_body_bytes, 1_048_576);
    assert_eq!(wh.host, "0.0.0.0");
    assert_eq!(wh.path, "/webhook");
    assert_eq!(wh.secret, "x");
}
