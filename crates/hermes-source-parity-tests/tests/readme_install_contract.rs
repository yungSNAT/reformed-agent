//! README contracts for install and Rust-primary positioning.

use std::fs;
use std::path::PathBuf;

/// The README with line endings normalised, so a CRLF checkout still meets the
/// `\n` checks below.
fn readme() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
        .replace("\r\n", "\n")
}

#[test]
fn readme_installs_from_this_fork() {
    let text = readme();

    assert!(text.contains("cargo install --git https://github.com/yungSNAT/reformed-agent"));
    assert!(text.contains("git clone https://github.com/yungSNAT/reformed-agent.git"));
    assert!(text.contains("bash scripts/install.sh"));
    assert!(text.contains("--bin hermes-agent-ultra --bin hermes-ultra"));
    assert!(!text.contains("--bin hermes\n"));
    // Ultra's installer and source URL would install Ultra, not this fork.
    assert!(!text.contains("sheawinkler/hermes-agent-ultra/main/scripts/install.sh"));
    assert!(!text.contains("cargo install --git https://github.com/sheawinkler"));
}

#[test]
fn readme_documents_rust_primary_runtime() {
    let text = readme();

    for phrase in [
        "Rust-first autonomous agent runtime",
        "Fully Rust-native core runtime",
        "Rust-only implementation strategy",
    ] {
        assert!(text.contains(phrase), "README lost {phrase:?}");
    }
}

#[test]
fn readme_quick_start_uses_ultra_commands() {
    let text = readme();

    for command in [
        "hermes-ultra setup",
        "hermes-ultra",
        r#"hermes-ultra chat --query "summarize this repository""#,
        "hermes-ultra gateway run",
        "hermes-ultra doctor --deep --snapshot --bundle",
    ] {
        assert!(text.contains(command), "README lost {command:?}");
    }
    // `gateway` takes an action (`run`, `start`, ...) and rejects `--live`.
    assert!(!text.contains("gateway --live"));
}
