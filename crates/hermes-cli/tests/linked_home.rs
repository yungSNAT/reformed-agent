//! The CLI refuses to start when its implicit home is a link into upstream
//! Hermes's `~/.hermes`, before it writes anything there.
#![cfg(unix)]

#[path = "support/linked_home.rs"]
mod support;

use assert_cmd::Command;
use support::{linked_home, snapshot};

#[test]
fn cli_refuses_to_start_with_its_home_linked_into_upstreams() {
    let (home, upstream, before) = linked_home();

    let output = Command::cargo_bin("hermes-agent-ultra")
        .unwrap()
        .env("HOME", home.path())
        .env_remove("HERMES_HOME")
        .env_remove("HERMES_AGENT_ULTRA_HOME")
        .args(["config", "set", "model", "openai:gpt-4o-mini"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "exited {}: {stderr}",
        output.status
    );
    assert!(stderr.contains("Refusing to start"), "{stderr}");
    assert!(
        stderr.contains(&format!("HERMES_HOME={}", upstream.display())),
        "{stderr}"
    );
    assert_eq!(snapshot(&upstream), before);
}
