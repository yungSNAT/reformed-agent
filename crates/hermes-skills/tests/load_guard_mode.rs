//! `HERMES_SKILL_LOAD_GUARD_MODE` governs scanning of skills loaded from disk
//! and leaves install and creation scanning on `HERMES_SKILL_GUARD_MODE`.
//!
//! One test function, because it changes process environment variables.

use std::path::Path;

use hermes_core::types::Skill;
use hermes_skills::{FileSkillStore, SkillGuard, SkillStore};

fn write_skill(root: &Path, name: &str, body: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\n---\n{body}"),
    )
    .unwrap();
}

fn probe(body: &str) -> Skill {
    Skill {
        name: "probe".to_string(),
        content: body.to_string(),
        category: None,
        description: None,
    }
}

#[tokio::test]
async fn load_guard_mode_is_separate_from_install_mode() {
    const ORDINARY: &str =
        "# PR body\n\nSee [the guide](../guide.md) and share the context with the reviewer.\n";
    const DESTRUCTIVE: &str = "# Wipe\n\nrm -rf /\n";

    std::env::remove_var("HERMES_SKILL_GUARD_MODE");
    std::env::remove_var("HERMES_GUARD_MODE");
    std::env::remove_var("HERMES_SKILL_LOAD_GUARD_MODE");

    let root = tempfile::tempdir().unwrap();
    write_skill(root.path(), "pr-body", ORDINARY);
    write_skill(root.path(), "wipe", DESTRUCTIVE);
    let store = FileSkillStore::new(root.path().to_path_buf());

    // Unset: load scanning follows the global mode, strict by default.
    assert!(store.load("pr-body").await.is_err());

    std::env::set_var("HERMES_SKILL_LOAD_GUARD_MODE", "relaxed");
    assert!(store.load("pr-body").await.unwrap().is_some());
    assert!(store.load("wipe").await.is_err());
    // Install and creation scanning keep the strict global mode.
    assert!(SkillGuard::default()
        .scan_security_only(&probe(ORDINARY))
        .is_err());

    std::env::set_var("HERMES_SKILL_LOAD_GUARD_MODE", "off");
    assert!(store.load("wipe").await.unwrap().is_some());

    std::env::set_var("HERMES_SKILL_LOAD_GUARD_MODE", "strict");
    std::env::set_var("HERMES_SKILL_GUARD_MODE", "off");
    assert!(store.load("pr-body").await.is_err());
    assert!(SkillGuard::default()
        .scan_security_only(&probe(ORDINARY))
        .is_ok());

    std::env::remove_var("HERMES_SKILL_LOAD_GUARD_MODE");
    assert!(store.load("pr-body").await.unwrap().is_some());

    std::env::remove_var("HERMES_SKILL_GUARD_MODE");
}
