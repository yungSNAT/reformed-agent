//! The profile's tool guard decides before `patch` changes a file, and stays
//! in force when the config loses it mid-run. One test, as it sets
//! `HERMES_HOME` for this process.

use hermes_tools::backends::file::LocalPatchBackend;
use hermes_tools::tools::file::PatchBackend;

const DENY: &str = r#"{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"guard says no"}}"#;

#[tokio::test]
async fn guard_decides_before_a_patch_lands() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::env::set_var("HERMES_HOME", home.path());
    // The guard sources `answer.sh`, so each step changes the answer, not the config.
    let answer = home.path().join("answer.sh");
    let config = home.path().join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "security:\n  tool_guard:\n    command: sh\n    args: ['-c', 'cat >/dev/null; . \"$0\"', '{}']\n",
            answer.display()
        ),
    )
    .unwrap();
    let file = work.path().join("notes.txt");
    std::fs::write(&file, "old").unwrap();
    let path = file.to_str().unwrap();
    let backend = LocalPatchBackend::new();

    std::fs::write(&answer, format!("printf '%s' '{DENY}'")).unwrap();
    for config_text in [None, Some("security: {}\n")] {
        // The second pass is a model that rewrote the config without the guard.
        if let Some(text) = config_text {
            std::fs::write(&config, text).unwrap();
        }
        let error = backend
            .patch_file(path, "old", "new", false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("guard says no"), "{error}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
    }

    std::fs::write(&answer, "").unwrap();
    backend.patch_file(path, "old", "new", false).await.unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
}
