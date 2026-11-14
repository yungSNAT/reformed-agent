//! The benchmark runner refuses to start when its implicit home is a link
//! into upstream Hermes's `~/.hermes`, before it writes anything there.
#![cfg(unix)]

#[path = "../../hermes-cli/tests/support/linked_home.rs"]
mod support;

use std::path::Path;
use std::process::Command;

use support::{linked_home, snapshot};

#[test]
fn bench_refuses_to_start_with_its_home_linked_into_upstreams() {
    let (home, upstream, before) = linked_home();
    let benchmark = Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/configured-smoke.toml");
    let output_path = home.path().join("run.json");

    let output = Command::new(env!("CARGO_BIN_EXE_hermes-bench"))
        .env("HOME", home.path())
        .env_remove("HERMES_HOME")
        .env_remove("HERMES_AGENT_ULTRA_HOME")
        .arg("--benchmark")
        .arg(&benchmark)
        .args(["--rollout", "smoke", "--output"])
        .arg(&output_path)
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
    assert!(!output_path.exists(), "wrote a run record: {stderr}");
    assert_eq!(snapshot(&upstream), before);
}
