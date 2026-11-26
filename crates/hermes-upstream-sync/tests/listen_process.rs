//! The listener as a process: start-up refusals and what reaches its log.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_hermes-upstream-sync");
const SECRET: &str = "process-test-secret-4f1c";

fn listen(dir: &std::path::Path, port: u16) -> Command {
    let mut command = Command::new(BIN);
    command
        .args(["listen", "--port", &port.to_string(), "--sqlite-path"])
        .arg(dir.join("q.db"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[test]
fn refuses_to_start_when_the_secret_is_absent_or_empty() {
    let dir = tempfile::tempdir().unwrap();
    let absent = listen(dir.path(), 1)
        .env_remove("GITHUB_WEBHOOK_SECRET")
        .output()
        .unwrap();
    let empty = listen(dir.path(), 1)
        .env("GITHUB_WEBHOOK_SECRET", "")
        .output()
        .unwrap();
    let wildcard = listen(dir.path(), 1)
        .env("GITHUB_WEBHOOK_SECRET", "")
        .args(["--allow-unsigned", "--host", "0.0.0.0"])
        .output()
        .unwrap();
    for output in [&absent, &empty, &wildcard] {
        assert_eq!(output.status.code(), Some(2));
    }
    assert!(String::from_utf8_lossy(&absent.stderr).contains("GITHUB_WEBHOOK_SECRET is not set"));
    assert!(String::from_utf8_lossy(&wildcard.stderr).contains("not loopback"));
    assert!(
        !dir.path().join("q.db").exists(),
        "refused before opening the queue"
    );
}

#[cfg(unix)]
#[test]
fn refuses_to_start_when_the_secret_is_set_but_not_unicode() {
    // Read as unset, it would let --allow-unsigned run unsigned whilst a
    // secret is configured.
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let mut child = listen(dir.path(), 0)
        .env(
            "GITHUB_WEBHOOK_SECRET",
            std::ffi::OsStr::from_bytes(b"secret-\xff"),
        )
        .arg("--allow-unsigned")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(code, Some(2), "the listener started");
}

fn post(port: u16, headers: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "POST /github/upstream-sync HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn no_secret_signature_or_body_reaches_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = listen(dir.path(), port)
        .env("GITHUB_WEBHOOK_SECRET", SECRET)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "listener did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let signature = format!("sha256={}", "ab".repeat(32));
    let body = r#"{"marker": "body-marker-77"}"#;
    let reply = post(
        port,
        &format!(
            "X-GitHub-Event: push\r\nX-GitHub-Delivery: d\r\nX-Hub-Signature-256: {signature}\r\n"
        ),
        body,
    );
    assert!(reply.starts_with("HTTP/1.1 401"), "{reply}");
    for leaked in [SECRET, &signature, "body-marker-77"] {
        assert!(!reply.contains(leaked), "reply carries {leaked}");
    }
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(b"POST /x?token=query-marker-91 HTTP/1.1\r\nHost: t\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 404"), "{reply}");
    child.kill().unwrap();
    let mut log = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut log)
        .unwrap();
    child.wait().unwrap();
    assert!(
        log.contains("\" 401 -") && log.contains("\"POST /x HTTP/1.1\" 404 -"),
        "{log}"
    );
    for leaked in [SECRET, &signature, "body-marker-77", "query-marker-91"] {
        assert!(!log.contains(leaked), "log carries {leaked}: {log}");
    }
}
