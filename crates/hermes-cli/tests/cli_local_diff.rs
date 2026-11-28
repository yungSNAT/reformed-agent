#[path = "support/isolated_hermes.rs"]
mod support;

use std::path::Path;
use std::time::Duration;

async fn run_diff(sandbox: &Path, workspace: &Path, query: &str) -> String {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        support::isolated_hermes(sandbox)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("TERMINAL_CWD", workspace)
            .args(["--oneshot", query])
            .output(),
    )
    .await
    .expect("local diff timed out")
    .expect("local diff command");
    assert!(
        output.status.success(),
        "{query}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn oneshot_diff_reads_the_terminal_workspace_and_reports_dispatch_errors() {
    let sandbox = tempfile::tempdir().unwrap();
    let workspace = sandbox.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("field notes.txt"), "Park survey\n").unwrap();
    std::fs::write(workspace.join("other.txt"), "Separate notes\n").unwrap();

    let mut git = tokio::process::Command::new("git");
    git.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", sandbox.path())
        .env("USERPROFILE", sandbox.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", sandbox.path().join("gitconfig"))
        .current_dir(&workspace)
        .kill_on_drop(true);
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        git.env("SystemRoot", system_root);
    }
    let init = tokio::time::timeout(
        Duration::from_secs(30),
        git.args(["init", "-q", "--template="]).output(),
    )
    .await
    .expect("fixture git init timed out")
    .expect("initialise fixture git repository");
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );

    let patch = run_diff(sandbox.path(), &workspace, "/diff").await;
    assert!(patch.contains("Untracked:"), "{patch}");
    assert!(patch.contains("  + field notes.txt"), "{patch}");
    assert!(patch.contains("  + other.txt"), "{patch}");
    assert!(
        patch.contains("\n+Park survey") && patch.contains("\n+Separate notes"),
        "{patch}"
    );

    let names = run_diff(sandbox.path(), &workspace, "/diff --stat").await;
    assert!(
        names.contains("Untracked:") && names.contains("  + field notes.txt"),
        "{names}"
    );
    assert!(!names.contains("diff --git"), "{names}");
    assert!(!names.contains("Park survey"), "{names}");
    assert!(!names.contains("Separate notes"), "{names}");

    let usage = run_diff(sandbox.path(), &workspace, "/diff session").await;
    assert!(
        usage.contains("Usage: /diff [staged|all] [--stat] [path...]"),
        "{usage}"
    );

    let error = run_diff(sandbox.path(), &workspace.join(".git"), "/diff").await;
    assert!(error.contains("Not a git repository."), "{error}");
}
