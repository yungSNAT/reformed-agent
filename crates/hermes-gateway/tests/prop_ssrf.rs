//! Bounded invariant coverage: SSRF protection blocks private IPs
//! **Validates: Requirement 22.2**
//!
//! Private/reserved IPv4 addresses in URLs must be blocked, while selected
//! public IPv4 HTTP/HTTPS URLs must be allowed.

use hermes_gateway::is_safe_url;

fn private_ipv4_cases() -> &'static [&'static str] {
    &[
        "10.0.0.1",
        "10.255.255.254",
        "172.16.0.1",
        "172.31.255.254",
        "192.168.0.1",
        "192.168.255.254",
        "127.0.0.1",
        "127.255.255.254",
        "169.254.0.1",
        "169.254.255.254",
    ]
}

fn public_ipv4_cases() -> &'static [&'static str] {
    &[
        "1.1.1.1",
        "8.8.8.8",
        "11.0.0.1",
        "99.255.255.254",
        "128.0.0.1",
        "168.255.255.254",
        "170.0.0.1",
        "171.255.255.254",
        "173.0.0.1",
        "191.255.255.254",
        "193.0.0.1",
        "197.255.255.254",
        "200.0.0.1",
        "223.255.255.254",
    ]
}

#[test]
fn private_ip_blocked() {
    for ip in private_ipv4_cases() {
        let url = format!("http://{ip}/api");
        assert!(
            !is_safe_url(&url),
            "Private IP {} should be blocked but is_safe_url returned true",
            ip
        );
    }
}

#[test]
fn public_ip_allowed() {
    for ip in public_ipv4_cases() {
        let url = format!("https://{ip}/api");
        assert!(
            is_safe_url(&url),
            "Public IP {} should be allowed but is_safe_url returned false",
            ip
        );
    }
}

/// `Url::host_str` keeps an IPv6 literal's brackets; the check must judge the
/// address inside them, not resolve the bracketed text as a host name.
#[test]
fn bracketed_ipv6_literals_are_judged_by_address() {
    assert!(is_safe_url("https://[2606:4700:4700::1111]/api"));
    assert!(is_safe_url("https://[2001:4860:4860::8888]:8443/api"));
    for url in ["http://[::1]/api", "http://[fd00::1]/api", "http://[fe80::1]/api"] {
        assert!(!is_safe_url(url), "{url} should be blocked");
    }
}
