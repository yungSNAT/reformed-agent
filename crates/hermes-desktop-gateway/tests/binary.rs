//! The binary as an operator starts it: loopback only, a strong token, and the readiness
//! lines Hermes Desktop's launcher reads from standard output.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_hermes-desktop-gateway");
const TOKEN_ENV: &str = "HERMES_DASHBOARD_SESSION_TOKEN";

/// Runs the binary, which must refuse and exit within 10 seconds (a binary that serves
/// instead fails the test rather than hanging it); returns its standard error.
fn refusal(args: &[&str], token: &OsStr) -> String {
    let mut child = Command::new(BIN)
        .args(args)
        .env(TOKEN_ENV, token)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(
        status.is_some_and(|s| !s.success()),
        "{args:?} was not refused: {stderr}"
    );
    stderr
}

#[test]
fn refuses_a_non_loopback_bind() {
    for host in ["0.0.0.0", "::", "192.0.2.1", "example.com"] {
        let stderr = refusal(
            &["--host", host, "--port", "0"],
            OsStr::new("0123456789abcdef0123456789abcdef"),
        );
        assert!(stderr.contains("loopback"), "{host}: {stderr}");
    }
}

#[test]
fn refuses_a_short_token() {
    let stderr = refusal(&["--port", "0"], OsStr::new("short"));
    assert!(stderr.contains("at least 32"), "{stderr}");

    // 16 characters, 48 bytes: the minimum counts characters.
    let stderr = refusal(&["--port", "0"], OsStr::new(&"\u{20ac}".repeat(16)));
    assert!(stderr.contains("at least 32"), "{stderr}");
}

/// A token that is set but not valid Unicode is refused, not silently replaced.
#[cfg(unix)]
#[test]
fn refuses_a_token_that_is_not_unicode() {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = vec![b'a'; 40];
    bytes[0] = 0xff;
    let stderr = refusal(&["--port", "0"], OsStr::from_bytes(&bytes));
    assert!(stderr.contains("not valid Unicode"), "{stderr}");
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
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
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
