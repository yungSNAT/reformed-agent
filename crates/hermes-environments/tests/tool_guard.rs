//! The profile's tool guard stops local commands and file writes before they
//! happen, refuses them when it fails, applies when added mid-run and stays in
//! force when the config loses it. One test, as it sets `HERMES_HOME` for this
//! process.

use std::path::Path;

use hermes_core::TerminalBackend;
use hermes_environments::LocalBackend;

const DENY: &str = r#"{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"guard says no"}}"#;

/// Every guarded route is refused for `why`, and nothing lands.
async fn assert_refused(backend: &LocalBackend, work: &Path, why: &str) {
    let workdir = work.to_str().unwrap();
    let written = work.join("written.txt");
    let results = [
        backend
            .execute_command("touch ran", None, Some(workdir), false, false)
            .await
            .map(|_| ()),
        backend
            .execute_command("touch ran", None, Some(workdir), true, false)
            .await
            .map(|_| ()),
        backend
            .execute_command_with_stdin("cat > ran", None, Some(workdir), false, false, Some("x"))
            .await
            .map(|_| ()),
        backend.write_file(written.to_str().unwrap(), "x").await,
    ];
    for result in results {
        let error = result.unwrap_err().to_string();
        assert!(error.contains(why), "{why}: {error}");
    }
    assert!(!work.join("ran").exists(), "a refused command ran");
    assert!(!written.exists(), "a refused write landed");
}

#[tokio::test]
async fn guard_decides_before_local_commands_and_writes() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::env::set_var("HERMES_HOME", home.path());
    let config = home.path().join("config.yaml");
    let backend = LocalBackend::new(30, 1 << 20);
    let workdir = work.path().to_str().unwrap();

    // Without a guard nothing asks, and that is not kept: a guard added later applies.
    std::fs::write(&config, "security: {}\n").unwrap();
    let output = backend
        .execute_command("touch unguarded", None, Some(workdir), false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(work.path().join("unguarded").exists());

    // The guard sources `answer.sh`, so each step changes the answer, not the config.
    let answer = home.path().join("answer.sh");
    std::fs::write(&answer, format!("printf '%s' '{DENY}'")).unwrap();
    std::fs::write(
        &config,
        format!(
            "security:\n  tool_guard:\n    command: sh\n    args: ['-c', 'cat >/dev/null; . \"$0\"', '{}']\n",
            answer.display()
        ),
    )
    .unwrap();
    assert_refused(&backend, work.path(), "guard says no").await;

    // A model that rewrites the config without the guard is still refused.
    std::fs::write(&config, "security: {}\n").unwrap();
    assert_refused(&backend, work.path(), "guard says no").await;

    std::fs::write(&answer, "exit 1").unwrap();
    assert_refused(&backend, work.path(), "the guard failed").await;

    std::fs::write(&answer, "").unwrap();
    let output = backend
        .execute_command("touch ran", None, Some(workdir), false, false)
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert!(work.path().join("ran").exists());
    let written = work.path().join("written.txt");
    backend
        .write_file(written.to_str().unwrap(), "x")
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "x");
}
