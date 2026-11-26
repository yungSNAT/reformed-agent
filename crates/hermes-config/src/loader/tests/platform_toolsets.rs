/// The 5 default entries every release that wrote the defaults wrote.
const WRITTEN_DEFAULTS_YAML: &str = "platform_toolsets:\n  cli: [hermes-cli]\n  telegram: [hermes-telegram]\n  discord: [hermes-discord]\n  whatsapp: [hermes-whatsapp]\n  slack: [hermes-slack]\n";

fn load_platform_toolsets(
    home: &std::path::Path,
    managed: Option<&std::path::Path>,
) -> std::collections::HashMap<String, Vec<String>> {
    set_test_env("HERMES_HOME", home);
    if let Some(managed) = managed {
        set_test_env("HERMES_MANAGED_DIR", managed);
    }
    let cfg = load_config(Some(home.to_string_lossy().as_ref())).unwrap();
    clear_managed_scope_test_env();
    cfg.platform_toolsets
}

#[test]
fn a_managed_pin_of_the_defaults_outranks_the_user_list() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    clear_managed_scope_test_env();
    let home = tempfile::tempdir().unwrap();
    let managed = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        "platform_toolsets:\n  cli: [web, browser, terminal]\n",
    )
    .unwrap();
    // The pin equals the defaults an earlier release wrote, and still binds.
    std::fs::write(managed.path().join("config.yaml"), WRITTEN_DEFAULTS_YAML).unwrap();

    let toolsets = load_platform_toolsets(home.path(), Some(managed.path()));
    assert_eq!(toolsets.get("cli"), Some(&vec!["hermes-cli".to_string()]));
    assert_eq!(toolsets.len(), 5, "{toolsets:?}");
}

#[test]
fn each_user_file_drops_the_defaults_an_earlier_release_wrote() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let changed_telegram = WRITTEN_DEFAULTS_YAML.replace("[hermes-telegram]", "[web]");
    let as_json = serde_json::to_string(
        &serde_yaml::from_str::<serde_json::Value>(&changed_telegram).unwrap(),
    )
    .unwrap();
    for (file, contents) in [
        ("config.yaml", changed_telegram.as_str()),
        ("cli-config.yaml", changed_telegram.as_str()),
        ("gateway.json", as_json.as_str()),
    ] {
        clear_managed_scope_test_env();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(file), contents).unwrap();

        let toolsets = load_platform_toolsets(home.path(), None);
        assert_eq!(
            toolsets,
            [("telegram".to_string(), vec!["web".to_string()])].into(),
            "{file}"
        );
    }
}
