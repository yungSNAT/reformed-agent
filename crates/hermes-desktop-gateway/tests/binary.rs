//! The binary as an operator starts it: loopback only, a strong token, and the readiness
//! lines Hermes Desktop's launcher reads from standard output.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_hermes-desktop-gateway");
const TOKEN_ENV: &str = "HERMES_DASHBOARD_SESSION_TOKEN";

#[test]
fn refuses_a_non_loopback_bind() {
    for host in ["0.0.0.0", "::", "192.0.2.1", "example.com"] {
        let out = Command::new(BIN)
            .args(["--host", host, "--port", "0"])
            .env(TOKEN_ENV, "0123456789abcdef0123456789abcdef")
            .output()
            .unwrap();
        assert!(!out.status.success(), "{host} was accepted");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("loopback"),
            "{host}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn refuses_a_short_token() {
    let out = Command::new(BIN)
        .args(["--port", "0"])
        .env(TOKEN_ENV, "short")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("at least 32"));
}

/// Starts the binary, returns the port from its first readiness line, then stops it.
fn ready_port(token: Option<&str>) -> (u16, String) {
    let mut cmd = Command::new(BIN);
    cmd.args(["--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match token {
        Some(t) => cmd.env(TOKEN_ENV, t),
        None => cmd.env_remove(TOKEN_ENV),
    };
    let mut child = cmd.spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = lines.next().unwrap().unwrap();
    let second = lines.next().unwrap().unwrap();
    child.kill().unwrap();
    let stderr = {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut s).unwrap();
        s
    };
    child.wait().unwrap();
    let port = first
        .strip_prefix("HERMES_BACKEND_READY port=")
        .unwrap_or_else(|| panic!("first line: {first}"))
        .parse()
        .unwrap();
    assert_eq!(second, format!("HERMES_DASHBOARD_READY port={port}"));
    (port, stderr)
}

#[test]
fn announces_the_bound_port_and_keeps_a_given_token_off_the_output() {
    let token = "0123456789abcdef0123456789abcdef-given";
    let (port, stderr) = ready_port(Some(token));
    assert_ne!(port, 0);
    assert!(!stderr.contains(token));
}

#[test]
fn without_a_token_it_makes_one_and_prints_it_once() {
    let (_, stderr) = ready_port(None);
    let printed: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.trim().strip_prefix("token: "))
        .collect();
    assert_eq!(printed.len(), 1, "{stderr}");
    let token = printed[0];
    assert!(token.len() >= 32 && token.chars().all(|c| c.is_ascii_hexdigit()));
}
