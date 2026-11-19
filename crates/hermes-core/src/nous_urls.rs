//! Trust checks for Nous Portal and inference URLs that arrive from the network or a stored
//! credential file. The inference URL receives the user's inference bearer and the Portal URL
//! receives the refresh token, so a value from a token response or `auth.json` is used only when
//! it names a Nous host. Operator settings (environment variables, config and flags) are trusted
//! and do not pass through these checks.

use url::Url;

/// Hosts the Nous Portal forwards inference tokens to (upstream `_ALLOWED_NOUS_INFERENCE_HOSTS`).
pub const NOUS_INFERENCE_HOSTS: [&str; 2] = [
    "inference-api.nousresearch.com",
    // Free-tier host serving the `nous/welcome` model.
    "welcome-api.nousresearch.com",
];

/// Portal hosts a stored `portal_base_url` may name (upstream `_NOUS_PORTAL_ALLOWED_HOSTS`).
pub const NOUS_PORTAL_HOSTS: [&str; 3] = ["portal.nousresearch.com", "localhost", "127.0.0.1"];

const LOOPBACK_PORTAL_HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];

/// `url` without trailing slashes when it is HTTPS and names a Nous inference host or the host of
/// the operator's `NOUS_INFERENCE_BASE_URL`; otherwise `None`, with a warning.
pub fn trusted_nous_inference_url(url: &str) -> Option<String> {
    let operator_override = std::env::var("NOUS_INFERENCE_BASE_URL").ok();
    inference_url_allowed(url, operator_override.as_deref())
}

fn inference_url_allowed(url: &str, operator_override: Option<&str>) -> Option<String> {
    let cleaned = url.trim();
    let parsed = Url::parse(cleaned).ok()?;
    if parsed.scheme() != "https" {
        tracing::warn!(
            scheme = parsed.scheme(),
            "nous: refusing a non-https inference URL from the Portal or a stored credential"
        );
        return None;
    }
    let host = parsed.host_str();
    let override_host = operator_override
        .and_then(|value| Url::parse(value.trim()).ok())
        .and_then(|value| value.host_str().map(str::to_string));
    let allowed = host.is_some_and(|host| {
        NOUS_INFERENCE_HOSTS.contains(&host) || override_host.as_deref() == Some(host)
    });
    if !allowed {
        tracing::warn!(
            host = host.unwrap_or_default(),
            "nous: refusing an inference URL host outside the allowlist; using the default"
        );
        return None;
    }
    Some(cleaned.trim_end_matches('/').to_string())
}

/// `url` without trailing slashes when it names a Nous Portal host over HTTPS, or a loopback
/// Portal over HTTP; otherwise `None`, with a warning.
pub fn trusted_nous_portal_url(url: &str) -> Option<String> {
    let cleaned = url.trim();
    let parsed = Url::parse(cleaned).ok()?;
    let host = parsed.host_str().unwrap_or_default();
    let scheme_ok = parsed.scheme() == "https"
        || (parsed.scheme() == "http" && LOOPBACK_PORTAL_HOSTS.contains(&host));
    if !scheme_ok || !NOUS_PORTAL_HOSTS.contains(&host) {
        tracing::warn!(
            host,
            "nous: ignoring a stored Portal URL whose host or scheme is not allowed; using the default"
        );
        return None;
    }
    Some(cleaned.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_url_needs_https_and_a_nous_host() {
        let cases = [
            (
                "https://inference-api.nousresearch.com/v1/",
                Some("https://inference-api.nousresearch.com/v1"),
            ),
            (
                "https://welcome-api.nousresearch.com/v1",
                Some("https://welcome-api.nousresearch.com/v1"),
            ),
            (
                "https://INFERENCE-API.nousresearch.com:8443/v1",
                Some("https://INFERENCE-API.nousresearch.com:8443/v1"),
            ),
            ("http://inference-api.nousresearch.com/v1", None),
            ("https://attacker.example.com/v1", None),
            (
                "https://inference-api.nousresearch.com.attacker.example/v1",
                None,
            ),
            (
                "https://inference-api.nousresearch.com@attacker.example.com/v1",
                None,
            ),
            ("https://inference.nousresearch.com/v1", None),
            ("inference-api.nousresearch.com/v1", None),
            ("", None),
        ];
        for (url, expected) in cases {
            assert_eq!(
                inference_url_allowed(url, None).as_deref(),
                expected,
                "{url}"
            );
        }
    }

    #[test]
    fn inference_url_may_name_the_operator_override_host() {
        let staging = Some("https://inference.staging.example.com/v1");
        assert_eq!(
            inference_url_allowed("https://inference.staging.example.com/v2", staging).as_deref(),
            Some("https://inference.staging.example.com/v2")
        );
        assert_eq!(
            inference_url_allowed("http://inference.staging.example.com/v2", staging),
            None
        );
        assert_eq!(
            inference_url_allowed("https://attacker.example.com/v1", staging),
            None
        );
    }

    #[test]
    fn portal_url_needs_a_portal_host_and_https_off_loopback() {
        let cases = [
            (
                "https://portal.nousresearch.com/",
                Some("https://portal.nousresearch.com"),
            ),
            ("http://127.0.0.1:8080", Some("http://127.0.0.1:8080")),
            ("http://localhost:3000/", Some("http://localhost:3000")),
            ("http://portal.nousresearch.com", None),
            ("https://api.nousresearch.com", None),
            ("https://attacker.example.com", None),
            ("", None),
        ];
        for (url, expected) in cases {
            assert_eq!(trusted_nous_portal_url(url).as_deref(), expected, "{url}");
        }
    }
}
