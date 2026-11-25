use super::*;

struct FailingReader;

impl std::io::Read for FailingReader {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("boom"))
    }
}

#[test]
fn read_chat_query_maps_stdin_read_failure_to_io_error() {
    let err = read_chat_query(Some("-".to_string()), FailingReader)
        .expect_err("a failing reader must produce an error");
    match err {
        hermes_core::AgentError::Io(msg) => {
            assert!(msg.contains("Failed to read the query from standard input"));
            assert!(msg.contains("boom"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn read_chat_query_passes_through_non_dash_query() {
    let out = read_chat_query(Some("hello".to_string()), FailingReader).unwrap();
    assert_eq!(out.as_deref(), Some("hello"));
}

#[test]
fn skills_extra_has_flag_matches_whole_words_only() {
    assert!(skills_extra_has_flag(Some("--remove --yes"), "--remove"));
    assert!(skills_extra_has_flag(Some("  -y  "), "-y"));
    assert!(!skills_extra_has_flag(Some("--removed"), "--remove"));
    assert!(!skills_extra_has_flag(None, "--sync"));
    assert!(!skills_extra_has_flag(Some(""), "--sync"));
}

#[test]
fn env_path_or_default_uses_default_when_unset() {
    let key = "HERMES_COVERAGE_GEN_SURELY_UNSET_KEY_8f3a2c";
    let default = std::path::PathBuf::from("/tmp/default-path-for-test");
    assert_eq!(env_path_or_default(key, default.clone()), default);
}

#[test]
fn env_path_or_default_uses_env_value_when_set() {
    if let Ok(value) = std::env::var("PATH") {
        let trimmed = value.trim().to_string();
        let default = std::path::PathBuf::from("/nonexistent-default");
        let got = env_path_or_default("PATH", default.clone());
        if trimmed.is_empty() {
            assert_eq!(got, default);
        } else {
            assert_eq!(got, std::path::PathBuf::from(trimmed));
        }
    }
}

#[test]
fn repo_root_for_skill_sync_returns_non_empty_path() {
    let root = repo_root_for_skill_sync();
    assert!(!root.as_os_str().is_empty());
}

#[tokio::test]
async fn remediation_target_is_none_for_dynamic_selector() {
    assert!(query_mode_remediation_target("anthropic:dynamic")
        .await
        .is_none());
    assert!(query_mode_remediation_target("dynamic").await.is_none());
}

#[tokio::test]
async fn remediation_target_is_none_for_empty_model() {
    assert!(query_mode_remediation_target("").await.is_none());
}

fn action_blocked(action: &str, name: Option<&str>) -> bool {
    !skills_tier_bypass_enabled()
        && skills_action_blocked_by_tier(skills_execution_tier(), action, name)
}

#[tokio::test]
async fn handle_cli_skills_unknown_action_is_ok_unless_tier_blocks() {
    let action = "definitely-not-an-action";
    let blocked = action_blocked(action, None);
    let result = handle_cli_skills(Some(action.to_string()), None, None, false, false, false).await;
    assert_eq!(result.is_err(), blocked);
}

#[tokio::test]
async fn handle_cli_skills_search_without_query_prints_usage() {
    let blocked = action_blocked("search", None);
    let result =
        handle_cli_skills(Some("search".to_string()), None, None, false, false, false).await;
    assert_eq!(result.is_err(), blocked);
}

#[tokio::test]
async fn handle_cli_skills_reset_without_name_errors() {
    let blocked = action_blocked("reset", None);
    let err = handle_cli_skills(Some("reset".to_string()), None, None, false, false, false)
        .await
        .expect_err("reset without a name must fail");
    let msg = err.to_string();
    if blocked {
        assert!(msg.contains("blocked by skills tier"));
    } else {
        assert!(msg.contains("Missing skill name"));
    }
}

#[tokio::test]
async fn handle_cli_skills_uninstall_without_name_errors() {
    let blocked = action_blocked("uninstall", None);
    let err = handle_cli_skills(
        Some("uninstall".to_string()),
        None,
        None,
        false,
        false,
        false,
    )
    .await
    .expect_err("uninstall without a name must fail");
    let msg = err.to_string();
    if blocked {
        assert!(msg.contains("blocked by skills tier"));
    } else {
        assert!(msg.contains("Missing skill name"));
    }
}

#[tokio::test]
async fn handle_cli_skills_subscribe_without_source_errors() {
    let blocked = action_blocked("subscribe", None);
    let err = handle_cli_skills(
        Some("subscribe".to_string()),
        None,
        None,
        false,
        false,
        false,
    )
    .await
    .expect_err("subscribe without a source must fail");
    let msg = err.to_string();
    if blocked {
        assert!(msg.contains("blocked by skills tier"));
    } else {
        assert!(msg.contains("Missing source"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn skills_snapshot_round_trips_a_local_tree_and_reports_archive_errors() {
    if !crate::isolated_test_profile::enter(
        "commands::coverage_gen_commands_command_cli_chat_skills::skills_snapshot_round_trips_a_local_tree_and_reports_archive_errors",
    ) {
        return;
    }
    let snapshot = |action: Option<&str>, path: Option<&str>| {
        handle_cli_skills(
            Some("snapshot".into()),
            action.map(str::to_string),
            path.map(str::to_string),
            false,
            false,
            false,
        )
    };
    let home = hermes_config::hermes_home();
    let skills = home.join("skills");
    snapshot(None, None).await.unwrap();
    assert!(!skills.exists());

    let archive = home.join("fixture-skills.tar.gz");
    let archive_path = archive.to_str().unwrap();
    let nested = skills.join("fixture");
    std::fs::create_dir_all(&nested).unwrap();
    let content = b"# Fixture skill\n";
    std::fs::write(nested.join("SKILL.md"), content).unwrap();
    let missing_parent = home.join("missing-parent/fixture.tar.gz");
    let error = snapshot(Some("export"), missing_parent.to_str())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Failed to create archive"));
    snapshot(Some("export"), Some(archive_path)).await.unwrap();
    assert!(archive.is_file());
    std::fs::rename(&skills, home.join("before-import")).unwrap();

    let blocked = snapshot(Some("import"), Some(archive_path))
        .await
        .unwrap_err();
    assert!(blocked.to_string().contains("blocked by skills tier"));
    assert!(!skills.exists());
    std::env::set_var("HERMES_SKILLS_EXECUTION_TIER", "open");
    let missing = snapshot(Some("import"), None).await.unwrap_err();
    assert!(missing.to_string().contains("Missing snapshot path"));
    let absent_path = home.join("absent.tar.gz");
    let absent = snapshot(Some("import"), absent_path.to_str())
        .await
        .unwrap_err();
    assert!(absent.to_string().contains("Failed to open archive"));
    snapshot(Some("import"), Some(archive_path)).await.unwrap();
    assert_eq!(std::fs::read(nested.join("SKILL.md")).unwrap(), content);
    assert_eq!(
        std::fs::read(home.join("before-import/fixture/SKILL.md")).unwrap(),
        content
    );

    let invalid_path = home.join("invalid.tar.gz");
    std::fs::write(&invalid_path, "fixture is not an archive").unwrap();
    let invalid = snapshot(Some("import"), invalid_path.to_str())
        .await
        .unwrap_err();
    assert!(invalid.to_string().contains("Failed to extract archive"));
    assert_eq!(std::fs::read(nested.join("SKILL.md")).unwrap(), content);
}
