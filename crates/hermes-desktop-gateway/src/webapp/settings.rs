//! `webapp:` in the home's `config.yaml` (upstream gateway/webapp_listener.py `webapp_settings`).
//! A missing, unreadable or malformed block reads as off, so a typo never exposes the listener; a
//! block that asks for the Webapp but cannot be used carries the reason.

use std::net::{IpAddr, Ipv6Addr};
use std::path::Path;

use serde_yaml::Value;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 9120;

/// Why a bind that upstream serves behind its sign-in gate stays off here.
pub const NEEDS_SIGN_IN: &str = "needs the sign-in gate (dashboard.basic_auth), which this build \
     does not serve yet: use webapp.host 127.0.0.1, ::1 or localhost and no webapp.public_url";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub public_url: String,
    /// Why a block that asks for the Webapp keeps it off.
    pub reason: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            host: DEFAULT_HOST.into(),
            port: DEFAULT_PORT,
            public_url: String::new(),
            reason: None,
        }
    }
}

impl Settings {
    fn off(self, reason: String) -> Self {
        tracing::warn!("Webapp stays off: {reason}");
        Settings {
            enabled: false,
            reason: Some(reason),
            ..self
        }
    }
}

/// The `webapp:` block of `<home>/config.yaml`.
pub fn read(home: &Path) -> Settings {
    let path = home.join("config.yaml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(e) => {
            return Settings::default().off(format!("{} could not be read ({e})", path.display()))
        }
    };
    let document: Value = match serde_yaml::from_str(&text) {
        Ok(document) => document,
        Err(e) => {
            return Settings::default().off(format!("{} could not be read ({e})", path.display()))
        }
    };
    parse(document.get("webapp"), &path.display().to_string())
}

/// Parses a `webapp:` value from the config at `path` (named in reasons).
pub fn parse(block: Option<&Value>, path: &str) -> Settings {
    let block = match block {
        None | Some(Value::Null) => return Settings::default(),
        Some(Value::Mapping(block)) => block,
        Some(_) => return Settings::default().off(format!("webapp in {path} must be a mapping")),
    };
    let get = |key: &str| block.get(Value::String(key.into()));
    let host = match get("host") {
        None => Some(DEFAULT_HOST.to_string()),
        Some(Value::String(host)) => {
            let host = canonical_host(bare_host(host));
            (local_only(&host) || is_wildcard(&host)).then_some(host)
        }
        Some(_) => None,
    };
    let port = match get("port") {
        None => Some(DEFAULT_PORT),
        Some(Value::Number(n)) => n
            .as_u64()
            .filter(|p| (1..=65535).contains(p))
            .map(|p| p as u16),
        Some(_) => None,
    };
    let public_url = match get("public_url") {
        None | Some(Value::Null) => Some(String::new()),
        Some(Value::String(url)) => public_url(url),
        Some(_) => None,
    };
    let configured = Settings {
        host: host.clone().unwrap_or_else(|| DEFAULT_HOST.into()),
        port: port.unwrap_or(DEFAULT_PORT),
        public_url: public_url.clone().unwrap_or_default(),
        ..Settings::default()
    };
    match get("enabled") {
        None | Some(Value::Bool(false)) => return configured,
        Some(Value::Bool(true)) => {}
        Some(_) => {
            return configured.off(format!("webapp.enabled must be true or false in {path}"))
        }
    }
    let Some(host) = host else {
        return configured.off(format!(
            "webapp.host must be 127.0.0.1, ::1, localhost, 0.0.0.0 or :: in {path}"
        ));
    };
    if port.is_none() {
        return configured.off(format!(
            "webapp.port must be a port number (1-65535) in {path}"
        ));
    }
    let Some(public_url) = public_url else {
        return configured.off(format!(
            "webapp.public_url must be an http:// or https:// address with no credentials, query \
             or fragment in {path}"
        ));
    };
    if !local_only(&host) || !public_url.is_empty() {
        return configured.off(NEEDS_SIGN_IN.into());
    }
    Settings {
        enabled: true,
        ..configured
    }
}

/// `host` without surrounding space or the brackets a URL puts around an IPv6 address.
fn bare_host(host: &str) -> &str {
    let host = host.trim();
    match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(inner) if inner.contains(':') && !inner.contains('[') => inner,
        _ => host,
    }
}

/// An IP address in its short form, a name in lower case.
fn canonical_host(host: &str) -> String {
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => host.to_ascii_lowercase(),
    }
}

/// The binds upstream's sign-in gate leaves local-only.
fn local_only(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

fn is_wildcard(host: &str) -> bool {
    matches!(host, "0.0.0.0" | "::")
}

/// `webapp.public_url` without a trailing slash; None when malformed.
fn public_url(value: &str) -> Option<String> {
    if value.is_empty() {
        return Some(String::new());
    }
    if value.chars().any(|c| "\"'<> \t\r\n?#@\\".contains(c)) {
        return None;
    }
    let url = value.trim_end_matches('/');
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split('/').next().unwrap_or_default();
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => (host, Some(port)),
        _ => (authority, None),
    };
    let port_ok = port.is_none_or(|p| p.parse::<u16>().is_ok_and(|p| p != 0));
    (!host.is_empty() && port_ok).then(|| url.to_string())
}

/// The address the listener binds for a local-only `host`. `localhost` binds 127.0.0.1, so the
/// launch link names an address rather than a name a browser may resolve to ::1 first.
pub fn bind_ip(host: &str) -> IpAddr {
    if host == "::1" {
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    } else {
        IpAddr::from([127, 0, 0, 1])
    }
}

/// `http://<ip>:<port>` for a bound loopback address.
pub fn origin_of(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V6(ip) => format!("http://[{ip}]:{port}"),
        IpAddr::V4(ip) => format!("http://{ip}:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(yaml: &str) -> Settings {
        let document: Value = serde_yaml::from_str(yaml).unwrap();
        parse(document.get("webapp"), "config.yaml")
    }

    #[test]
    fn an_absent_block_or_enabled_false_is_off_without_a_reason() {
        assert_eq!(settings("model: x\n"), Settings::default());
        let off = settings("webapp:\n  enabled: false\n  port: 9300\n");
        assert!(!off.enabled && off.reason.is_none());
        assert_eq!(off.port, 9300);
    }

    #[test]
    fn a_local_only_host_turns_it_on() {
        for host in [
            "127.0.0.1",
            "::1",
            "[::1]",
            "localhost",
            "LocalHost",
            "0:0:0:0:0:0:0:1",
        ] {
            let on = settings(&format!(
                "webapp:\n  enabled: true\n  host: '{host}'\n  port: 9300\n"
            ));
            assert!(on.enabled, "{host}: {on:?}");
            assert_eq!(on.port, 9300);
        }
        assert!(settings("webapp:\n  enabled: true\n").enabled);
    }

    #[test]
    fn binds_upstream_gates_behind_sign_in_stay_off_here() {
        for block in [
            "webapp:\n  enabled: true\n  host: 0.0.0.0\n",
            "webapp:\n  enabled: true\n  host: '::'\n",
            "webapp:\n  enabled: true\n  public_url: https://hermes.example\n",
        ] {
            let off = settings(block);
            assert!(!off.enabled, "{block}");
            assert_eq!(off.reason.as_deref(), Some(NEEDS_SIGN_IN), "{block}");
        }
    }

    #[test]
    fn a_malformed_block_is_off_with_its_reason() {
        let cases = [
            ("webapp: yes\n", "must be a mapping"),
            (
                "webapp:\n  enabled: 'true'\n",
                "webapp.enabled must be true or false",
            ),
            (
                "webapp:\n  enabled: 1\n",
                "webapp.enabled must be true or false",
            ),
            (
                "webapp:\n  enabled: true\n  host: 10.0.0.5\n",
                "webapp.host must be",
            ),
            (
                "webapp:\n  enabled: true\n  host: 127.0.0.2\n",
                "webapp.host must be",
            ),
            (
                "webapp:\n  enabled: true\n  host: 5\n",
                "webapp.host must be",
            ),
            (
                "webapp:\n  enabled: true\n  port: 0\n",
                "webapp.port must be",
            ),
            (
                "webapp:\n  enabled: true\n  port: 70000\n",
                "webapp.port must be",
            ),
            (
                "webapp:\n  enabled: true\n  port: '9300'\n",
                "webapp.port must be",
            ),
            (
                "webapp:\n  enabled: true\n  public_url: 'ftp://x'\n",
                "webapp.public_url must be",
            ),
            (
                "webapp:\n  enabled: true\n  public_url: 'https://u:p@x'\n",
                "webapp.public_url must be",
            ),
            (
                "webapp:\n  enabled: true\n  public_url: 'https://x/?q'\n",
                "webapp.public_url must be",
            ),
        ];
        for (block, reason) in cases {
            let off = settings(block);
            assert!(!off.enabled, "{block}");
            let got = off.reason.unwrap_or_default();
            assert!(got.contains(reason), "{block}: {got}");
        }
    }

    #[test]
    fn an_unreadable_config_is_off_with_its_reason() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.yaml"), "webapp: [unclosed\n").unwrap();
        let off = read(home.path());
        assert!(!off.enabled);
        assert!(off.reason.unwrap().contains("could not be read"));
        let missing = tempfile::tempdir().unwrap();
        assert_eq!(read(missing.path()), Settings::default());
    }

    #[test]
    fn origins_bracket_ipv6() {
        assert_eq!(origin_of(bind_ip("::1"), 9), "http://[::1]:9");
        assert_eq!(origin_of(bind_ip("localhost"), 9), "http://127.0.0.1:9");
    }
}
