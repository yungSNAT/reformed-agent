use super::*;

#[cfg(unix)]
#[test]
fn discover_setup_env_sources_orders_the_isolated_legacy_profiles() {
    if !crate::isolated_test_profile::enter(
        "coverage_gen_main_setup_flow::discover_setup_env_sources_orders_the_isolated_legacy_profiles",
    ) {
        return;
    }

    let root = std::env::current_dir().unwrap();
    let working = root.join("working");
    std::fs::create_dir(&working).unwrap();
    std::env::set_current_dir(working).unwrap();
    let first = root.join("Documents/Projects/hermes-agent/.env");
    let second = root.join("Projects/hermes-agent/.env");
    for (path, contents) in [
        (&first, "SURVEY_ONE=first\n"),
        (&second, "SURVEY_TWO=second\n"),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    assert_eq!(discover_setup_env_sources(), [first, second]);
}

#[test]
fn read_env_key_skips_other_keys_and_returns_none_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(&path, "OTHER=1\nFOO=\"bar\"\n").unwrap();
    assert_eq!(read_env_key(&path, "FOO").as_deref(), Some("bar"));
    assert_eq!(read_env_key(&path, "MISSING"), None);
}

#[test]
fn setup_default_model_pick_index_empty_list_is_zero() {
    assert_eq!(
        setup_default_model_pick_index("openai", "openai:gpt-4o", &[]),
        0
    );
}

#[test]
fn setup_default_model_pick_index_exact_match() {
    let models = vec!["x".to_string(), "openai:gpt-4o".to_string()];
    assert_eq!(
        setup_default_model_pick_index("openai", "openai:gpt-4o", &models),
        1
    );
}

#[test]
fn setup_default_model_pick_index_provider_prefixed_match() {
    let models = vec!["openai:other".to_string(), "openai:gpt-4o".to_string()];
    assert_eq!(
        setup_default_model_pick_index("openai", "gpt-4o", &models),
        1
    );
}

#[test]
fn setup_default_model_pick_index_nous_kimi_fallback() {
    let models = vec!["a".to_string(), "moonshotai/kimi-k2.6".to_string()];
    assert_eq!(
        setup_default_model_pick_index("nous", "nous:foo", &models),
        1
    );
    assert_eq!(
        setup_default_model_pick_index("openai", "openai:foo", &models),
        0
    );
}

#[test]
fn setup_provider_display_zai_and_unknown() {
    assert_eq!(setup_provider_display("zai"), "Z.AI / GLM");
    assert_eq!(setup_provider_display("does-not-exist"), "Provider");
}

#[test]
fn setup_provider_env_keys_zai_and_unknown() {
    assert_eq!(
        setup_provider_env_keys("zai"),
        &["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"]
    );
    assert!(setup_provider_env_keys("does-not-exist").is_empty());
}

#[test]
fn merge_missing_env_keys_imports_only_new_non_empty_keys() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.env");
    let dst = dir.path().join("dst.env");
    std::fs::write(&src, "A=1\nB=2\nC=\n").unwrap();
    std::fs::write(&dst, "A=old").unwrap();
    let count = merge_missing_env_keys(&src, &dst, "label").unwrap();
    assert_eq!(count, 1);
    let out = std::fs::read_to_string(&dst).unwrap();
    assert_eq!(
        out,
        "A=old\n# Imported by `hermes-ultra setup` from label\nB=2\n"
    );
}

#[test]
fn merge_missing_env_keys_returns_zero_when_nothing_new() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.env");
    let dst = dir.path().join("dst.env");
    std::fs::write(&src, "A=1\n").unwrap();
    std::fs::write(&dst, "A=x\n").unwrap();
    assert_eq!(merge_missing_env_keys(&src, &dst, "label").unwrap(), 0);
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "A=x\n");
}

#[test]
fn upsert_env_key_replaces_and_preserves_other_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(&path, "OTHER=1\nKEY=old\n").unwrap();
    upsert_env_key(&path, "KEY", "new").unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "OTHER=1\nKEY=new\n"
    );
}

#[cfg(unix)]
#[test]
fn maybe_import_legacy_env_respects_consent_and_retains_existing_keys() {
    if !crate::isolated_test_profile::enter(
        "coverage_gen_main_setup_flow::maybe_import_legacy_env_respects_consent_and_retains_existing_keys",
    ) {
        return;
    }

    let root = std::env::current_dir().unwrap();
    let working = root.join("working");
    std::fs::create_dir(&working).unwrap();
    std::env::set_current_dir(working).unwrap();
    let legacy = root.join("Documents/Projects/hermes-agent/.env");
    let target = root.join("wizard.env");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(
        &legacy,
        "SURVEY_KEEP=legacy\nSURVEY_NEW=added\nSURVEY_SKIP=''\n",
    )
    .unwrap();
    std::fs::write(&target, "SURVEY_KEEP=existing").unwrap();

    let mut decline = std::io::Cursor::new(b"No\n");
    maybe_import_legacy_env(&mut decline, &target).unwrap();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "SURVEY_KEEP=existing"
    );

    let expected =
        "SURVEY_KEEP=existing\n# Imported by `hermes-ultra setup` from .env\nSURVEY_NEW=added\n";
    for _ in 0..2 {
        let mut accept = std::io::Cursor::new(b"\n");
        maybe_import_legacy_env(&mut accept, &target).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), expected);
    }
}
