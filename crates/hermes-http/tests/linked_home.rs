//! The HTTP server refuses to start when its implicit home is a link into
//! upstream Hermes's `~/.hermes`, before it writes anything there.
#![cfg(unix)]

#[path = "../../hermes-cli/tests/support/linked_home.rs"]
mod support;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use support::{linked_home, snapshot};

#[test]
fn server_refuses_to_start_with_its_home_linked_into_upstreams() {
    let (home, upstream, before) = linked_home();
    let stderr_path = home.path().join("stderr.log");

    let mut child = Command::new(env!("CARGO_BIN_EXE_hermes-http"))
        .env("HOME", home.path())
        .env_remove("HERMES_HOME")
        .env_remove("HERMES_AGENT_ULTRA_HOME")
        .env("HERMES_HTTP_ADDR", "127.0.0.1:0")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let stderr = std::fs::read_to_string(&stderr_path).unwrap();
    let status = status.unwrap_or_else(|| panic!("hermes-http kept running: {stderr}"));
    assert!(!status.success(), "exited {status}: {stderr}");
    assert!(stderr.contains("Refusing to start"), "{stderr}");
    assert_eq!(snapshot(&upstream), before);
}
