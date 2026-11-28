use std::collections::BTreeSet;

use super::tests::xiaomi_chat;
use super::*;

#[test]
fn reload_env_drops_a_saved_key_removed_from_a_readable_file() {
    let dir = tempfile::tempdir().unwrap();
    let var = "DESKTOP_MODEL_UNIT_TEST_RELOADED_KEY";
    let chat = xiaomi_chat(dir.path(), json!({ "api_key_env": var }));
    let path = dir.path().join(".env");
    chat.save_key("xiaomi", "saved-key").unwrap();
    std::fs::write(&path, "OTHER_KEY=keep-me\n").unwrap();
    assert_eq!(chat.reload_env(), 1);
    assert_eq!(chat.keys.value(var), None);
    assert!(!chat.keys.changed("OTHER_KEY"));
    // A file that cannot be read removes nothing.
    chat.save_key("xiaomi", "saved-key").unwrap();
    std::fs::write(&path, b"\xff\n").unwrap();
    assert_eq!(chat.reload_env(), 0);
    assert_eq!(chat.keys.value(var).as_deref(), Some("saved-key"));
}

#[test]
fn reload_env_drops_a_key_start_up_read_from_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let var = "DESKTOP_MODEL_UNIT_TEST_START_UP_KEY";
    let config = xiaomi_chat(dir.path(), json!({ "api_key_env": var })).config;
    let path = dir.path().join(".env");
    std::fs::write(&path, format!("OTHER_KEY=keep-me\n{var}=start-up-key\n")).unwrap();
    let chat = Chat::new(config);
    std::fs::write(&path, "OTHER_KEY=keep-me\n").unwrap();
    // The process environment never held `var`, so the count cannot show the removal.
    chat.reload_env();
    assert!(chat.keys.changed(var));
    assert_eq!(chat.keys.value(var), None);
    assert!(!chat.keys.changed("OTHER_KEY"));
}

#[cfg(windows)]
#[test]
fn reload_env_drops_a_start_up_key_whose_line_differs_in_case() {
    let dir = tempfile::tempdir().unwrap();
    let var = "DESKTOP_MODEL_UNIT_TEST_CASE_KEY";
    let config = xiaomi_chat(dir.path(), json!({ "api_key_env": var })).config;
    let path = dir.path().join(".env");
    let line = var.to_ascii_lowercase();
    std::fs::write(&path, format!("OTHER_KEY=keep-me\n{line}=start-up-key\n")).unwrap();
    let chat = Chat::new(config);
    std::fs::write(&path, "OTHER_KEY=keep-me\n").unwrap();
    chat.reload_env();
    assert!(chat.keys.changed(var));
    assert_eq!(chat.keys.value(var), None);
}

#[test]
fn a_turn_takes_the_key_of_the_entry_named_as_its_provider() {
    let (lower, upper) = (
        "DESKTOP_MODEL_UNIT_TEST_TURN_LOWER_KEY",
        "DESKTOP_MODEL_UNIT_TEST_TURN_UPPER_KEY",
    );
    // `llm_providers` is a `HashMap`: each new map iterates in its own order.
    for _ in 0..32 {
        let mut config = GatewayConfig::default();
        for (name, var) in [("mimo", lower), ("MIMO", upper)] {
            let provider: LlmProviderConfig =
                serde_json::from_value(json!({ "api_key_env": var })).unwrap();
            config.llm_providers.insert(name.into(), provider);
        }
        let chat = Chat::new(config);
        chat.keys.set(lower, Some("lower-key".into()));
        chat.keys.set(upper, Some("upper-key".into()));
        for (provider, key) in [("mimo", "lower-key"), ("MIMO", "upper-key")] {
            let config = chat.turn_config(&format!("{provider}:m")).unwrap();
            assert_eq!(
                config.llm_providers[provider].api_key.as_deref(),
                Some(key),
                "{provider}"
            );
        }
        // A third spelling takes the entry first by name.
        let config = chat.turn_config("Mimo:m").unwrap();
        assert_eq!(
            config.llm_providers["MIMO"].api_key.as_deref(),
            Some("upper-key")
        );
        assert_eq!(config.llm_providers["mimo"].api_key, None);
    }
}

#[test]
fn a_home_value_whose_line_is_gone_takes_what_a_fresh_start_would_read() {
    let project = |line: &str| HashMap::from([("PATH".to_string(), line.to_string())]);
    // The shell set `PATH` at start-up; neither `.env` did.
    let start_up = std::env::var("PATH").ok();
    let keys = Keys::default();
    keys.set("PATH", Some("saved-key".into()));
    assert!(keys.drop_home_value("PATH", Some(&project("project-key"))));
    assert_eq!(keys.value("PATH"), start_up);
    assert!(!keys.changed("PATH"));
    // A disconnect stays.
    keys.set("PATH", None);
    assert!(!keys.drop_home_value("PATH", Some(&project("project-key"))));
    assert_eq!(keys.value("PATH"), None);

    // Start-up took it from the project `.env`: its value there now, if any.
    let keys = Keys {
        started_in_project_file: BTreeSet::from(["PATH".to_string()]),
        ..Keys::default()
    };
    keys.set("PATH", Some("saved-key".into()));
    assert!(keys.drop_home_value("PATH", Some(&project("changed-key"))));
    assert_eq!(keys.value("PATH").as_deref(), Some("changed-key"));
    // A value a `.env` would not read back as itself, or a project file that cannot be
    // read: start-up's project value.
    let start_up = std::env::var("PATH").ok();
    assert!(keys.drop_home_value("PATH", Some(&project("${OTHER}"))));
    assert_eq!(keys.value("PATH"), start_up);
    keys.set("PATH", Some("saved-key".into()));
    assert!(keys.drop_home_value("PATH", None));
    assert_eq!(keys.value("PATH"), start_up);
    // Gone from the project file too: removed.
    assert!(keys.drop_home_value("PATH", Some(&HashMap::new())));
    assert_eq!(keys.value("PATH"), None);

    // Start-up took it from the home `.env`, which shadowed the project's: the project's.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "PATH=from-home\n").unwrap();
    let keys = Keys::at_start_up(dir.path());
    assert!(keys.drop_home_value("PATH", Some(&project("project-key"))));
    assert_eq!(keys.value("PATH").as_deref(), Some("project-key"));
    // With no project file readable or no usable value there: removed, even where start-up
    // also read the project file.
    let keys = Keys::at_start_up(dir.path());
    assert!(keys.drop_home_value("PATH", None));
    assert_eq!(keys.value("PATH"), None);
    let keys = Keys {
        started_in_project_file: BTreeSet::from(["PATH".to_string()]),
        ..Keys::at_start_up(dir.path())
    };
    assert!(keys.drop_home_value("PATH", None));
    assert_eq!(keys.value("PATH"), None);
    let keys = Keys::at_start_up(dir.path());
    assert!(keys.drop_home_value("PATH", Some(&project("${OTHER}"))));
    assert_eq!(keys.value("PATH"), None);
}

#[test]
fn a_env_file_reads_as_start_up_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(env_file_values(dir.path()), Some(HashMap::new()));
    std::fs::write(
        dir.path().join(".env"),
        "# note\nA_KEY=\"quoted\"\nB_KEY= plain \nA_KEY=later\n",
    )
    .unwrap();
    let values = env_file_values(dir.path()).unwrap();
    // The first of 2 lines, as start-up's fill-only project read keeps it.
    assert_eq!(values["A_KEY"], "quoted");
    assert_eq!(values["B_KEY"], "plain");
    assert_eq!(values.len(), 2);
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".env")).unwrap();
    assert_eq!(env_file_values(dir.path()), None);
}

#[test]
fn entries_whose_names_differ_only_in_case_each_save_to_their_own_variable() {
    let (lower, upper) = (
        "DESKTOP_MODEL_UNIT_TEST_LOWER_KEY",
        "DESKTOP_MODEL_UNIT_TEST_UPPER_KEY",
    );
    // `llm_providers` is a `HashMap`: each load iterates in its own order.
    for _ in 0..8 {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.yaml"),
            format!(
                "llm_providers:\n  mimo:\n    api_key_env: {lower}\n  MIMO:\n    \
                 api_key_env: {upper}\n"
            ),
        )
        .unwrap();
        // `load_config` also loads a project `.env`; there is none in a test's folder.
        assert!(!Path::new(".env").exists());
        let config = hermes_config::load_config(dir.path().to_str()).unwrap();
        // `load_config` keeps both entries, so the picker lists both.
        assert_eq!(config.llm_providers.len(), 2);
        let chat = Chat::new(config);
        for (slug, var) in [("mimo", lower), ("MIMO", upper)] {
            assert_eq!(chat.slug_key_vars(slug).unwrap()[0], var, "{slug}");
            let saved = chat.save_key(slug, "saved-key").unwrap();
            assert_eq!(saved["provider"]["slug"], slug, "{saved}");
            assert_eq!(saved["provider"]["key_env"], var, "{saved}");
        }
        // A third spelling takes the entry first by name, and answers its row.
        assert_eq!(chat.slug_key_vars("Mimo").unwrap()[0], upper);
        let saved = chat.save_key("Mimo", "saved-key").unwrap();
        assert_eq!(saved["provider"]["key_env"], upper, "{saved}");
    }
}
