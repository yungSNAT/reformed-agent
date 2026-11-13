use std::fmt::Display;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::str::FromStr;
use std::sync::Mutex;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use url::Url;

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

const METADATA_HOSTNAMES: [&str; 3] = [
    "metadata.google.internal",
    "metadata.goog",
    "metadata.internal",
];
const QQ_MULTIMEDIA_HOST: &str = "multimedia.nt.qq.com.cn";

static ALLOW_PRIVATE_URLS_CACHE: Mutex<Option<bool>> = Mutex::new(None);

pub struct UrlSafetyHandler;

fn parse_bool_like(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
fn config_allow_private_urls() -> bool {
    false
}

#[cfg(not(test))]
fn config_allow_private_urls() -> bool {
    match hermes_config::load_config(None) {
        Ok(cfg) => {
            if cfg.security.allow_private_urls {
                return true;
            }
            cfg.tools_config
                .per_tool
                .get("browser")
                .and_then(|v| v.get("allow_private_urls"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        }
        Err(_) => false,
    }
}

fn global_allow_private_urls() -> bool {
    let mut guard = ALLOW_PRIVATE_URLS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(v) = *guard {
        return v;
    }

    let resolved = std::env::var("HERMES_ALLOW_PRIVATE_URLS")
        .ok()
        .and_then(|v| parse_bool_like(&v))
        .unwrap_or_else(config_allow_private_urls);
    *guard = Some(resolved);
    resolved
}

#[cfg(test)]
fn reset_allow_private_cache_for_tests() {
    let mut guard = ALLOW_PRIVATE_URLS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
}

fn is_benchmark_ipv4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 198 && (18..=19).contains(&octets[1])
}

fn is_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 10
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
        || octets[0] == 127
        || (octets[0] == 169 && octets[1] == 254)
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || is_benchmark_ipv4(ip)
        || ip.is_multicast()
        || ip.is_broadcast()
}

fn mapped_ipv4(ip: &Ipv6Addr) -> Option<Ipv4Addr> {
    ip.to_ipv4_mapped()
}

/// The IPv4 address an IPv6 address delivers to: IPv4-mapped and
/// IPv4-compatible (`::ffff:a.b.c.d`, `::a.b.c.d`), NAT64's well-known prefix
/// (`64:ff9b::/96`, RFC 6052) or 6to4 (`2002::/16`, RFC 3056).
fn embedded_ipv4(ip: &Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = ip.segments();
    match segments {
        [0x2002, high, low, ..] => Some(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low))),
        [0x64, 0xff9b, 0, 0, 0, 0, ..] => Some(Ipv4Addr::from(u128::from(*ip) as u32)),
        _ => ip.to_ipv4(),
    }
}

/// NAT64 local-use prefix `64:ff9b:1::/48` (RFC 8215): the operator's
/// translator picks where the IPv4 address sits, so it cannot be decoded.
fn is_nat64_local_use(ip: &Ipv6Addr) -> bool {
    ip.segments()[..3] == [0x64, 0xff9b, 1]
}

fn is_blocked_ipv6(ip: &Ipv6Addr) -> bool {
    if let Some(v4) = embedded_ipv4(ip) {
        return is_blocked_ipv4(&v4);
    }

    let segments = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xfe00) == 0xfc00
        || is_nat64_local_use(ip)
        // Teredo (RFC 4380) tunnels to IPv4 hosts and is never a fetch target.
        || segments[..2] == [0x2001, 0]
}

fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

fn is_always_blocked_ipv4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    (octets[0] == 169 && octets[1] == 254) || octets == [100, 100, 100, 200]
}

fn is_always_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_always_blocked_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = embedded_ipv4(v6) {
                return is_always_blocked_ipv4(&v4);
            }
            is_nat64_local_use(v6) || *v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)
        }
    }
}

fn is_metadata_hostname(host: &str) -> bool {
    METADATA_HOSTNAMES.contains(&host)
}

fn qq_multimedia_benchmark_exception(host: &str, scheme: &str, ip: &IpAddr) -> bool {
    if host != QQ_MULTIMEDIA_HOST || scheme != "https" {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => is_benchmark_ipv4(v4),
        IpAddr::V6(v6) => mapped_ipv4(v6).as_ref().is_some_and(is_benchmark_ipv4),
    }
}

fn resolve_host_ips(host: &str, port: u16) -> Result<Vec<IpAddr>, std::io::Error> {
    let ips: Vec<IpAddr> = (host, port)
        .to_socket_addrs()?
        .map(|addr| addr.ip())
        .collect();
    if ips.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "host resolved to no addresses",
        ));
    }
    Ok(ips)
}

fn ip_allowed(host: &str, scheme: &str, ip: &IpAddr, allow_private_urls: bool) -> bool {
    if is_always_blocked_ip(ip) {
        return false;
    }
    allow_private_urls || !is_blocked_ip(ip) || qq_multimedia_benchmark_exception(host, scheme, ip)
}

/// Whitespace every URL parser strips from both ends. WHATWG also strips the
/// other C0 controls, but `urlsplit`-style parsers keep a trailing one in the
/// host, so those count as ambiguous instead.
const URL_EDGE_WHITESPACE: &[char] = &[' ', '\t', '\n', '\r', '\x0b', '\x0c'];

/// Why another client could read a different host or port from an http(s)
/// `url` than WHATWG parsers (the `url` crate, browsers, Node) do, else `None`.
///
/// `urlsplit`-style parsers do not end the authority at `\`, do not
/// percent-decode the host and do not accept `http:\\host` or `http:///host`,
/// so `http://127.0.0.1:8080\@1.1.1.1` is 1.1.1.1 to one and 127.0.0.1:8080 to
/// the other. Such URLs are refused rather than guessed at. `\` and `@` after
/// the authority are accepted: every parser has fixed the host by then.
/// `floor` keeps only what can move the host itself (controls, no `//`, `\` in
/// the authority): userinfo and an unparseable port (templated `{port}` or
/// `%d` literals) leave the host as written.
fn authority_ambiguity(url: &str, floor: bool) -> Option<&'static str> {
    // Split the way `urlsplit` does: it drops leading C0 controls and space,
    // and tab, CR and LF anywhere, before reading the scheme and authority. It
    // keeps a trailing space, so `http://example.com ` has whitespace in its
    // host, which WHATWG parsers drop.
    let cleaned: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let (scheme, rest) = cleaned.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let trimmed = url.trim_matches(URL_EDGE_WHITESPACE);
    if trimmed.chars().any(|c| c < ' ' || c == '\x7f') {
        return Some("control character in URL");
    }
    let netloc = rest
        .strip_prefix("//")
        .map(|after| &after[..after.find(['/', '?', '#']).unwrap_or(after.len())])
        .unwrap_or("");
    if netloc.is_empty() {
        return Some("authority not introduced by //");
    }
    if netloc.contains('\\') {
        return Some("backslash in authority");
    }
    if floor {
        return None;
    }
    if netloc.contains('@') {
        return Some("userinfo in authority");
    }
    if netloc.contains('%') {
        return Some("percent-encoding in host");
    }
    if netloc.chars().any(char::is_whitespace) {
        return Some("whitespace in host");
    }
    let port = match netloc.strip_prefix('[') {
        Some(bracketed) => bracketed
            .split_once(']')
            .map_or("", |(_, after)| after)
            .split_once(':')
            .map_or("", |(_, port)| port),
        None => netloc.split_once(':').map_or("", |(_, port)| port),
    };
    if !port.is_empty()
        && !(port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok())
    {
        return Some("invalid port");
    }
    None
}

/// `Url::host_str` keeps the brackets of an IPv6 literal (`[::1]`).
fn ip_literal(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
}

fn is_safe_url_with_resolver<F, E>(url: &str, mut resolver: F) -> bool
where
    F: FnMut(&str, u16) -> Result<Vec<IpAddr>, E>,
    E: Display,
{
    if let Some(reason) = authority_ambiguity(url, false) {
        // The reason only: the URL may carry userinfo credentials.
        tracing::warn!("Blocked request: ambiguous URL authority: {reason}");
        return false;
    }
    let parsed = match Url::parse(url) {
        Ok(parsed) => parsed,
        Err(_) => return false,
    };
    let scheme = parsed.scheme();
    if !matches!(scheme, "http" | "https") {
        return false;
    }

    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    if is_metadata_hostname(&host) {
        return false;
    }

    let allow_private_urls = global_allow_private_urls();
    if let Ok(ip) = IpAddr::from_str(ip_literal(&host)) {
        return ip_allowed(&host, scheme, &ip, allow_private_urls);
    }

    let port = parsed.port_or_known_default().unwrap_or(80);
    let ips = match resolver(&host, port) {
        Ok(ips) if !ips.is_empty() => ips,
        // Proxy-side DNS cannot establish that the destination is public.
        // Refuse an unverified hostname even when a proxy is configured.
        Ok(_) | Err(_) => return false,
    };
    ips.iter()
        .all(|ip| ip_allowed(&host, scheme, ip, allow_private_urls))
}

pub fn is_safe_url(url: &str) -> bool {
    is_safe_url_with_resolver(url, resolve_host_ips)
}

fn is_always_blocked_url_with_resolver<F, E>(url: &str, mut resolver: F) -> bool
where
    F: FnMut(&str, u16) -> Result<Vec<IpAddr>, E>,
    E: Display,
{
    if let Some(reason) = authority_ambiguity(url, true) {
        tracing::warn!("Blocked request: ambiguous URL authority (always-blocked floor): {reason}");
        return true;
    }
    let parsed = match Url::parse(url) {
        Ok(parsed) => parsed,
        Err(_) => return false,
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    if is_metadata_hostname(&host) {
        return true;
    }
    if let Ok(ip) = IpAddr::from_str(ip_literal(&host)) {
        return is_always_blocked_ip(&ip);
    }

    let port = parsed.port_or_known_default().unwrap_or(80);
    resolver(&host, port)
        .map(|ips| ips.iter().any(is_always_blocked_ip))
        .unwrap_or(false)
}

pub fn is_always_blocked_url(url: &str) -> bool {
    is_always_blocked_url_with_resolver(url, resolve_host_ips)
}

/// Refuses a URL the agent must not fetch: a private, internal or metadata
/// address, as `is_safe_url` judges it, or a host on
/// `security.website_blocklist`. Fails closed when either check cannot run.
///
/// shortcut: the address is resolved here and again by the HTTP client, so a
/// DNS answer that changes in between (rebinding) is not caught; pinning the
/// resolved address on the request would close that.
pub async fn ensure_url_fetch_allowed(url: &str) -> Result<(), ToolError> {
    let owned = url.to_string();
    tokio::task::spawn_blocking(move || check_url_fetch_allowed(&owned))
        .await
        .unwrap_or_else(|e| {
            Err(ToolError::ExecutionFailed(format!(
                "Blocked: URL check did not complete: {e}"
            )))
        })
}

fn check_url_fetch_allowed(url: &str) -> Result<(), ToolError> {
    if !is_safe_url(url) {
        return Err(ToolError::ExecutionFailed(format!(
            "Blocked: '{url}' is invalid, could not be resolved, or targets a private or internal address"
        )));
    }
    match crate::website_policy::check_website_access(url, None) {
        Ok(None) => Ok(()),
        Ok(Some(block)) => Err(ToolError::ExecutionFailed(format!(
            "Blocked: '{}' matches website blocklist rule '{}'",
            block.url, block.rule
        ))),
        Err(e) => Err(ToolError::ExecutionFailed(format!(
            "Blocked: website blocklist could not be read: {e}"
        ))),
    }
}

#[async_trait]
impl ToolHandler for UrlSafetyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'url'".into()));
        }

        let safe = is_safe_url(url);
        let reason = if safe {
            "ok"
        } else if is_always_blocked_url(url) {
            "always_blocked"
        } else {
            "blocked_or_invalid"
        };
        Ok(json!({"url":url,"safe":safe,"reason":reason}).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("url".into(), json!({"type":"string"}));
        tool_schema(
            "url_safety",
            "Check whether a URL is safe to access.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            reset_allow_private_cache_for_tests();
            Self { key, old }
        }

        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            std::env::remove_var(key);
            reset_allow_private_cache_for_tests();
            Self { key, old }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
            reset_allow_private_cache_for_tests();
        }
    }

    fn without_proxy() -> Vec<EnvVarGuard> {
        [
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "ALL_PROXY",
            "all_proxy",
        ]
        .iter()
        .map(|key| EnvVarGuard::remove(key))
        .collect()
    }

    #[test]
    fn dns_failure_is_blocked_even_behind_a_configured_proxy() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::set("HERMES_ALLOW_PRIVATE_URLS", "false");
        let _no_proxy = without_proxy();
        assert!(!is_safe_url_with_resolver(
            "https://docs.example.com/",
            resolver_err
        ));
        let _proxy = EnvVarGuard::set("HTTPS_PROXY", "http://proxy.internal:3128");
        assert!(!is_safe_url_with_resolver(
            "https://docs.example.com/",
            resolver_err
        ));
        assert!(!is_safe_url_with_resolver(
            "https://docs.example.com/",
            with_ips(&[])
        ));
        assert!(is_safe_url_with_resolver(
            "https://docs.example.com/",
            with_ip("93.184.216.34")
        ));
        assert!(!is_safe_url_with_resolver(
            "http://metadata.google.internal/computeMetadata/v1/",
            resolver_err
        ));
    }

    #[test]
    fn ipv6_literals_are_judged_by_address() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::set("HERMES_ALLOW_PRIVATE_URLS", "false");
        let _no_proxy = without_proxy();
        assert!(is_safe_url_with_resolver(
            "https://[2606:4700:4700::1111]/",
            resolver_err
        ));
        assert!(!is_safe_url_with_resolver(
            "http://[::1]:8080/",
            resolver_err
        ));
        assert!(is_always_blocked_url_with_resolver(
            "http://[fd00:ec2::254]/latest/",
            resolver_err
        ));
    }

    #[test]
    fn ipv6_forms_that_carry_an_ipv4_address_are_judged_by_it() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::set("HERMES_ALLOW_PRIVATE_URLS", "false");
        let _no_proxy = without_proxy();
        for address in [
            // NAT64 well-known prefix (RFC 6052): 127.0.0.1 and 10.0.0.1.
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
            // The NAT64 local-use prefix (RFC 8215) can translate to any address.
            "64:ff9b:1::a00:1",
            // 6to4 (RFC 3056): 192.168.1.1.
            "2002:c0a8:101::1",
            // IPv4-compatible (deprecated): 127.0.0.1.
            "::7f00:1",
            // Teredo (RFC 4380) tunnels to IPv4 hosts and is never a fetch target.
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
        ] {
            let literal = format!("http://[{address}]/");
            assert!(
                !is_safe_url_with_resolver(&literal, resolver_err),
                "{literal} should be refused"
            );
            assert!(
                !is_safe_url_with_resolver("http://rebind.example/", with_ip(address)),
                "an AAAA answer of {address} should be refused"
            );
        }
        for literal in [
            "http://[64:ff9b::a9fe:a9fe]/latest/meta-data/",
            "http://[2002:a9fe:a9fe::1]/latest/meta-data/",
            // A local-use NAT64 translator may place 169.254.169.254 in the low 32 bits.
            "http://[64:ff9b:1::a9fe:a9fe]/latest/meta-data/",
        ] {
            assert!(
                is_always_blocked_url_with_resolver(literal, resolver_err),
                "{literal} reaches the metadata address"
            );
        }
        // Public IPv4 addresses behind the same prefixes stay reachable.
        for literal in ["http://[64:ff9b::808:808]/", "http://[2002:808:808::1]/"] {
            assert!(
                is_safe_url_with_resolver(literal, resolver_err),
                "{literal} should be allowed"
            );
        }
    }

    fn ips(values: &[&str]) -> Vec<IpAddr> {
        values
            .iter()
            .map(|value| value.parse().expect("valid ip"))
            .collect()
    }

    fn with_ips(
        values: &'static [&'static str],
    ) -> impl FnMut(&str, u16) -> Result<Vec<IpAddr>, String> {
        move |_, _| Ok(ips(values))
    }

    fn with_ip(value: &'static str) -> impl FnMut(&str, u16) -> Result<Vec<IpAddr>, String> {
        move |_, _| Ok(ips(&[value]))
    }

    fn resolver_err(_: &str, _: u16) -> Result<Vec<IpAddr>, String> {
        Err("dns failure".to_string())
    }

    #[test]
    fn safe_url_allows_public_http_and_https_only() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::remove("HERMES_ALLOW_PRIVATE_URLS");
        assert!(is_safe_url_with_resolver(
            "https://example.com/image.png",
            with_ips(&["93.184.216.34"])
        ));
        assert!(is_safe_url_with_resolver(
            "http://example.com/image.png",
            with_ips(&["93.184.216.34"])
        ));
        assert!(!is_safe_url_with_resolver(
            "ftp://example.com/file.txt",
            with_ips(&["93.184.216.34"])
        ));
        assert!(!is_safe_url_with_resolver(
            "example.com/path",
            with_ips(&["93.184.216.34"])
        ));
        assert!(!is_safe_url_with_resolver("http://", with_ips(&[])));
        assert!(!is_safe_url_with_resolver("", with_ips(&[])));
    }

    #[test]
    fn safe_url_blocks_private_reserved_and_metadata_ips() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::remove("HERMES_ALLOW_PRIVATE_URLS");
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "100.127.255.254",
            "198.18.0.23",
            "224.0.0.251",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fd12::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:100.100.100.200",
        ] {
            let parsed: IpAddr = ip.parse().expect("valid ip");
            assert!(is_blocked_ip(&parsed), "{ip} should be blocked");
            assert!(!is_safe_url_with_resolver(
                "https://resolved.example/file",
                with_ip(ip)
            ));
        }
    }

    #[test]
    fn safe_url_allows_public_and_non_cgnat_100_addresses() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::remove("HERMES_ALLOW_PRIVATE_URLS");
        for ip in [
            "8.8.8.8",
            "93.184.216.34",
            "1.1.1.1",
            "100.0.0.1",
            "2606:4700::1",
            "2001:4860:4860::8888",
            "::ffff:8.8.8.8",
        ] {
            let parsed: IpAddr = ip.parse().expect("valid ip");
            assert!(!is_blocked_ip(&parsed), "{ip} should be allowed");
            assert!(is_safe_url_with_resolver(
                "https://resolved.example/file",
                with_ip(ip)
            ));
        }
    }

    #[test]
    fn qq_multimedia_exact_https_exception_allows_benchmark_ip() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::remove("HERMES_ALLOW_PRIVATE_URLS");
        let _no_proxy = without_proxy();
        assert!(is_safe_url_with_resolver(
            "https://multimedia.nt.qq.com.cn/download?id=123",
            with_ips(&["198.18.0.23"])
        ));
        assert!(!is_safe_url_with_resolver(
            "https://sub.multimedia.nt.qq.com.cn/download?id=123",
            with_ips(&["198.18.0.23"])
        ));
        assert!(!is_safe_url_with_resolver(
            "http://multimedia.nt.qq.com.cn/download?id=123",
            with_ips(&["198.18.0.23"])
        ));
        assert!(!is_safe_url_with_resolver(
            "https://multimedia.nt.qq.com.cn/download?id=123",
            resolver_err
        ));
    }

    #[test]
    fn safe_url_blocks_metadata_hostnames_without_dns() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::set("HERMES_ALLOW_PRIVATE_URLS", "true");
        assert!(!is_safe_url_with_resolver(
            "http://metadata.google.internal/computeMetadata/v1/",
            resolver_err
        ));
        assert!(!is_safe_url_with_resolver(
            "http://metadata.goog/computeMetadata/v1/",
            resolver_err
        ));
    }

    #[test]
    fn allow_private_toggle_permits_private_but_not_metadata() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::set("HERMES_ALLOW_PRIVATE_URLS", "true");
        let _no_proxy = without_proxy();
        assert!(is_safe_url_with_resolver(
            "http://router.local",
            with_ips(&["192.168.1.1"])
        ));
        assert!(is_safe_url_with_resolver(
            "http://tailscale-peer.example",
            with_ips(&["100.100.100.100"])
        ));
        assert!(is_safe_url_with_resolver(
            "http://localhost:8080/api",
            with_ips(&["127.0.0.1"])
        ));
        assert!(!is_safe_url_with_resolver(
            "http://169.254.169.254/latest/meta-data/",
            with_ips(&["169.254.169.254"])
        ));
        assert!(!is_safe_url_with_resolver(
            "http://[fd00:ec2::254]/latest/",
            with_ips(&["fd00:ec2::254"])
        ));
        assert!(!is_safe_url_with_resolver(
            "https://nonexistent.example.com",
            resolver_err
        ));
    }

    #[test]
    fn env_toggle_false_values_keep_private_blocked_and_cache_result() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::set("HERMES_ALLOW_PRIVATE_URLS", "false");
        assert!(!is_safe_url_with_resolver(
            "http://router.local",
            with_ips(&["192.168.1.1"])
        ));
        std::env::set_var("HERMES_ALLOW_PRIVATE_URLS", "true");
        assert!(!is_safe_url_with_resolver(
            "http://router.local",
            with_ips(&["192.168.1.1"])
        ));
    }

    #[test]
    fn always_blocked_url_floor_is_metadata_only() {
        assert!(is_always_blocked_url_with_resolver(
            "http://169.254.169.254/latest/meta-data/",
            with_ips(&["8.8.8.8"])
        ));
        assert!(is_always_blocked_url_with_resolver(
            "http://metadata.google.internal/",
            resolver_err
        ));
        assert!(is_always_blocked_url_with_resolver(
            "http://attacker.example/",
            with_ips(&["169.254.42.1"])
        ));
        assert!(!is_always_blocked_url_with_resolver(
            "http://127.0.0.1:8080/",
            with_ips(&["127.0.0.1"])
        ));
        assert!(!is_always_blocked_url_with_resolver(
            "http://100.64.0.1/",
            with_ips(&["100.64.0.1"])
        ));
        assert!(!is_always_blocked_url_with_resolver(
            "http://nonexistent.example.com/",
            resolver_err
        ));
        assert!(!is_always_blocked_url_with_resolver("", resolver_err));
    }

    /// URLs whose authority `urlsplit`-style and WHATWG clients read
    /// differently (upstream hermes-agent#145578). Every name resolves to a
    /// public address in these tests, so only the parse can block them.
    /// Entries shaped like `host:port<separator>@host` are split at the `@`
    /// so secret scanners do not read them as URL credentials.
    const AMBIGUOUS_AUTHORITY: &[&str] = &[
        concat!("http://127.0.0.1:8080\\", "@1.1.1.1"),
        concat!("http://127.0.0.1:8080\\\\", "@1.1.1.1"),
        "http://127.0.0.1\\@1.1.1.1/",
        "http://1.1.1.1\\.127.0.0.1/",
        "http://\\127.0.0.1/",
        "http://%31%32%37.0.0.1/",
        concat!("http://127.0.0.1:8080\t", "@1.1.1.1"),
        concat!("http://127.0.0.1:8080\n", "@1.1.1.1"),
        concat!("http://127.0.0.1:8080\r", "@1.1.1.1"),
        concat!("http://127.0.0.1:8080\x00", "@1.1.1.1"),
        "http://exa\tmple.com/",
        "https://example.com/a\nb",
        "http://exa mple.com/",
        "http://1.1.1.1:99999/",
        "http://1.1.1.1:80x/",
        // The `url` crate reads 1.1.1.1 here, `urlsplit` the address after it.
        "http://1.1.1.1\\@169.254.169.254/",
        "http://1.1.1.1\\@127.0.0.1/",
        "http:/1.1.1.1/",
        // `urlsplit` keeps a trailing space in the host; WHATWG drops it.
        "http://example.com ",
        "http://1.1.1.1:80 ",
        // Userinfo.
        concat!("http://127.0.0.1:8080%5C", "@1.1.1.1"),
        "http://user:pass@1.1.1.1/",
        "http://1.1.1.1@1.1.1.1/",
        "https://@example.com/",
    ];

    #[test]
    fn ambiguous_authority_is_refused_and_ordinary_urls_still_pass() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::remove("HERMES_ALLOW_PRIVATE_URLS");
        for url in AMBIGUOUS_AUTHORITY {
            assert!(
                !is_safe_url_with_resolver(url, with_ip("1.1.1.1")),
                "{url:?} should be refused"
            );
        }
        for url in [
            "https://example.com/image.png",
            "https://example.com/a\\b?q=c\\d",
            "https://example.com/%5C@x",
            "https://example.com/p@q?x=@y#frag@z",
            "https://example.com/r?next=http://127.0.0.1\\@x",
            "https://example.com/path%20with%20spaces?q=a%40b",
            "https://example.com:8443/x",
            "http://[2606:4700:4700::1111]:8080/",
            "HTTPS://EXAMPLE.COM/",
            "  https://example.com/  ",
            "https://example.com/ ",
            "https://example.com/\n",
            "\thttps://example.com/\r\n",
        ] {
            assert!(
                is_safe_url_with_resolver(url, with_ip("1.1.1.1")),
                "{url:?} should be allowed"
            );
        }
    }

    #[test]
    fn always_blocked_floor_refuses_authority_that_can_hide_metadata_host() {
        for url in [
            "http://169.254.169.254\\@1.1.1.1/latest/meta-data/",
            "http:\\\\169.254.169.254\\latest",
            "http:///169.254.169.254/latest",
            "http://169.254.169.254\t@1.1.1.1/",
            // The `url` crate reads 1.1.1.1 here, so only the authority
            // check refuses them.
            "http://1.1.1.1\\@169.254.169.254/",
            "http://1.1.1.1\\@127.0.0.1/",
            "http:/1.1.1.1/",
        ] {
            assert!(
                is_always_blocked_url_with_resolver(url, with_ip("1.1.1.1")),
                "{url:?} should hit the floor"
            );
        }
        // Trailing C0 controls and DEL, which some clients drop from the host.
        for control in ['\x00', '\x01', '\x08', '\x1b', '\x1f', '\x7f'] {
            let url = format!("http://169.254.169.254{control}");
            assert!(
                is_always_blocked_url_with_resolver(&url, resolver_err),
                "{url:?} should hit the floor"
            );
        }
        // Userinfo alone, and templated ports or hosts, leave the host as
        // written, so the floor leaves them to `is_safe_url`.
        for url in [
            "https://user:pass@example.com/",
            "https://example.com/a\\b",
            "about:blank",
            "data:text/plain,line1\nline2",
            "http://localhost:{port}/",
            "http://localhost:%d/",
            "http://localhost:${port}/api",
            "http://{host}:8080/",
            "http://%s/",
            "http://user:pass@localhost:{port}/",
            "  https://example.com/  ",
            "\thttps://example.com/\r\n",
        ] {
            assert!(
                !is_always_blocked_url_with_resolver(url, with_ip("1.1.1.1")),
                "{url:?} should not hit the floor"
            );
        }
    }

    #[test]
    fn checks_judge_the_host_whatwg_clients_dial() {
        // The `url` crate is a WHATWG parser, so it already percent-decodes the
        // host and maps IDNA dots; these upstream #145578 cases held before it.
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _env = EnvVarGuard::remove("HERMES_ALLOW_PRIVATE_URLS");
        for url in [
            "http://%31%36%39.254.169.254/",
            "HTTP://%31%36%39.254.169.254:80/latest",
            "http://metadata.google.%69nternal/computeMetadata/v1/",
            "http://169.254.169.254\u{3002}/",
            "http://169.254.169.254\u{ff0e}/latest/meta-data/",
            "http://169.254.169.254\u{ff61}/",
            "http://169\u{3002}254\u{3002}169\u{3002}254/",
            "http://\u{ff11}\u{ff16}\u{ff19}.254.169.254\u{3002}/",
        ] {
            assert!(
                is_always_blocked_url_with_resolver(url, resolver_err),
                "{url:?} should hit the floor"
            );
            assert!(
                !is_safe_url_with_resolver(url, with_ip("1.1.1.1")),
                "{url:?} should be refused"
            );
        }
        let ideographic_dots = "https://example\u{3002}com\u{3002}/";
        assert!(!is_always_blocked_url_with_resolver(
            ideographic_dots,
            with_ip("1.1.1.1")
        ));
        assert!(is_safe_url_with_resolver(
            ideographic_dots,
            with_ip("1.1.1.1")
        ));
        for url in [
            "http://%6c%6f%63%61%6c%68%6f%73%74/",
            "http://exa%20mple.com/",
        ] {
            assert!(!is_always_blocked_url_with_resolver(
                url,
                with_ip("127.0.0.1")
            ));
        }
    }

    #[tokio::test]
    async fn url_safety_handler_reports_safe_and_blocked_urls() {
        let handler = UrlSafetyHandler;
        let blocked = handler
            .execute(json!({"url":"http://169.254.169.254/latest/meta-data/"}))
            .await
            .expect("handler result");
        let blocked: Value = serde_json::from_str(&blocked).expect("json");
        assert_eq!(blocked["safe"], false);
        assert_eq!(blocked["reason"], "always_blocked");

        let missing = handler.execute(json!({})).await;
        assert!(matches!(missing, Err(ToolError::InvalidParams(_))));
    }
}
