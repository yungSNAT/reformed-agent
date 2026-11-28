use std::sync::Arc;

use super::options::configured_key;
use super::*;
use crate::chat::Conn;

#[test]
fn a_switch_value_names_a_model_and_maybe_its_provider() {
    assert_eq!(parse_switch("gpt-5"), Some(("gpt-5".into(), None)));
    assert_eq!(
        parse_switch("example/a --provider nous --session"),
        Some(("example/a".into(), Some("nous".into())))
    );
    for refused in [
        "gpt-5 --global",
        "gpt-5 --once",
        "--provider",
        "gpt-5 --provider",
        "",
    ] {
        assert_eq!(parse_switch(refused), None, "{refused}");
    }
}

#[test]
fn a_key_changed_after_start_up_stays_out_of_the_process_environment() {
    let keys = Keys::default();
    let var = "DESKTOP_MODEL_KEYS_UNIT_TEST_KEY";
    assert_eq!(keys.value(var), None);
    assert!(!keys.changed(var));
    assert!(keys.set(var, Some("k".into())));
    assert_eq!(keys.value(var).as_deref(), Some("k"));
    assert!(std::env::var_os(var).is_none());
    assert!(!keys.set(var, Some("k".into())), "the same value again");
    assert!(keys.set(var, None));
    assert_eq!(keys.value(var), None);
    assert!(keys.changed(var));
}

/// A gateway whose one provider, `envkey`, has `api_key` (and `key_env` when given) in
/// `config.yaml`.
fn chat_with_key(api_key: &str, key_env: Option<&str>) -> Chat {
    let provider: LlmProviderConfig =
        serde_json::from_value(json!({"api_key": api_key, "api_key_env": key_env})).unwrap();
    let mut config = GatewayConfig::default();
    config.llm_providers.insert("envkey".into(), provider);
    Chat::new(config)
}

fn turn_key(chat: &Chat) -> Result<Option<String>, String> {
    let config = chat.turn_config("envkey:m")?;
    Ok(config.llm_providers["envkey"].api_key.clone())
}

#[test]
fn a_config_key_from_the_environment_follows_later_key_changes() {
    // Start-up expanded `${PATH}`: the key is the variable's start-up value.
    let start_up = std::env::var("PATH").unwrap();
    let chat = chat_with_key(&start_up, Some("PATH"));
    assert_eq!(turn_key(&chat).unwrap().as_deref(), Some(start_up.as_str()));
    let (_, authenticated) =
        configured_key("envkey", &chat.config.llm_providers["envkey"], &chat.keys);
    assert!(authenticated);
    chat.keys.set("PATH", Some("saved-key".into()));
    assert_eq!(turn_key(&chat).unwrap().as_deref(), Some("saved-key"));
    chat.keys.set("PATH", None);
    assert!(turn_key(&chat).unwrap_err().contains("disconnected"));
    let (_, authenticated) =
        configured_key("envkey", &chat.config.llm_providers["envkey"], &chat.keys);
    assert!(!authenticated, "a removed key is not configured");

    // A reference start-up could not expand stays `${VAR}`; a key saved since is used.
    let var = "DESKTOP_MODEL_UNIT_TEST_REFERENCE_KEY";
    let chat = chat_with_key(&format!("${{{var}}}"), None);
    chat.keys.set(var, Some("saved-key".into()));
    assert_eq!(turn_key(&chat).unwrap().as_deref(), Some("saved-key"));

    // A key written out in `config.yaml` comes first.
    let chat = chat_with_key("written-key", Some("PATH"));
    chat.keys.set("PATH", Some("saved-key".into()));
    assert_eq!(turn_key(&chat).unwrap().as_deref(), Some("written-key"));
}

#[test]
fn a_saved_key_goes_to_the_key_env_the_provider_is_configured_with() {
    let dir = tempfile::tempdir().unwrap();
    let var = "DESKTOP_MODEL_UNIT_TEST_CONFIGURED_KEY";
    let provider: LlmProviderConfig =
        serde_json::from_value(json!({ "api_key_env": var })).unwrap();
    let mut config = GatewayConfig {
        home_dir: Some(dir.path().to_string_lossy().into_owned()),
        ..GatewayConfig::default()
    };
    config.llm_providers.insert("xiaomi".into(), provider);
    let chat = Chat::new(config);
    let env = || std::fs::read_to_string(dir.path().join(".env")).unwrap();

    chat.save_key("xiaomi", "saved-key").unwrap();
    assert_eq!(env(), format!("{var}=saved-key\n"));
    let config = chat.turn_config("xiaomi:m").unwrap();
    assert_eq!(
        config.llm_providers["xiaomi"].api_key.as_deref(),
        Some("saved-key")
    );
    drop(config);
    chat.disconnect_provider("xiaomi").unwrap();
    assert!(!env().contains(var), "{}", env());
    assert_eq!(chat.keys.value(var), None);
}

#[test]
fn a_saved_key_goes_to_the_variable_an_unexpanded_config_key_names() {
    let dir = tempfile::tempdir().unwrap();
    let var = "DESKTOP_MODEL_UNIT_TEST_UNEXPANDED_KEY";
    let provider: LlmProviderConfig =
        serde_json::from_value(json!({ "api_key": format!("${{{var}}}") })).unwrap();
    let mut config = GatewayConfig {
        home_dir: Some(dir.path().to_string_lossy().into_owned()),
        ..GatewayConfig::default()
    };
    config.llm_providers.insert("xiaomi".into(), provider);
    let chat = Chat::new(config);
    let path = dir.path().join(".env");
    std::fs::write(&path, "OTHER_KEY=keep-me\n").unwrap();
    let env = || std::fs::read_to_string(&path).unwrap();
    chat.save_key("xiaomi", "saved-key").unwrap();
    assert_eq!(env(), format!("OTHER_KEY=keep-me\n{var}=saved-key\n"));
    let config = chat.turn_config("xiaomi:m").unwrap();
    assert_eq!(
        config.llm_providers["xiaomi"].api_key.as_deref(),
        Some("saved-key")
    );
    drop(config);
    chat.disconnect_provider("xiaomi").unwrap();
    assert_eq!(env(), "OTHER_KEY=keep-me\n");
    assert_eq!(chat.keys.value(var), None);
    assert!(
        !chat.keys.changed("OTHER_KEY"),
        "only the saved key was held"
    );
}

/// A gateway whose `xiaomi` provider has `fields` in `config.yaml`, with a `.env` holding
/// one other key.
pub(super) fn xiaomi_chat(dir: &std::path::Path, fields: Value) -> Chat {
    let provider: LlmProviderConfig = serde_json::from_value(fields).unwrap();
    let mut config = GatewayConfig {
        home_dir: Some(dir.to_string_lossy().into_owned()),
        ..GatewayConfig::default()
    };
    config.llm_providers.insert("xiaomi".into(), provider);
    std::fs::write(dir.join(".env"), "OTHER_KEY=keep-me\n").unwrap();
    Chat::new(config)
}

#[test]
fn a_config_key_variable_that_is_not_a_name_is_refused_and_nothing_written() {
    for fields in [
        json!({"api_key": "${X\nOPENAI_API_KEY}"}),
        json!({"api_key": "${ DESKTOP_MODEL_UNIT_TEST_SPACED }"}),
        json!({"api_key": "${A}${B}"}),
        json!({"api_key_env": "X\nOPENAI_API_KEY"}),
        json!({"api_key": "${}"}),
        json!({"api_key_env": "PATH"}),
        json!({"api_key_env": "LD_PRELOAD"}),
        json!({"api_key": "${HERMES_HOME}"}),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let chat = xiaomi_chat(dir.path(), fields.clone());
        let (code, _) = chat.save_key("xiaomi", "k").unwrap_err();
        assert_eq!(code, NO_KEY_VARIABLE, "{fields}");
        let env = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert_eq!(env, "OTHER_KEY=keep-me\n", "{fields}");
        for var in [
            "OPENAI_API_KEY",
            "XIAOMI_API_KEY",
            "PATH",
            "LD_PRELOAD",
            "HERMES_HOME",
        ] {
            assert!(!chat.keys.changed(var), "{fields} {var}");
        }
    }
}

#[test]
fn a_key_written_out_in_config_yaml_is_not_shadowed_by_a_saved_one() {
    let dir = tempfile::tempdir().unwrap();
    let chat = xiaomi_chat(dir.path(), json!({"api_key": "written-key"}));
    let (code, message) = chat.save_key("xiaomi", "saved-key").unwrap_err();
    assert_eq!(code, CONFIGURED_IN_FILE);
    assert!(message.contains("config.yaml"), "{message}");
    let env = std::fs::read_to_string(dir.path().join(".env")).unwrap();
    assert_eq!(env, "OTHER_KEY=keep-me\n");
    let config = chat.turn_config("xiaomi:m").unwrap();
    assert_eq!(
        config.llm_providers["xiaomi"].api_key.as_deref(),
        Some("written-key")
    );
}

#[test]
fn a_key_config_yaml_takes_from_another_variable_is_not_shadowed() {
    let dir = tempfile::tempdir().unwrap();
    let chat = xiaomi_chat(dir.path(), json!({}));
    let mut config = chat.config.clone();
    // `mimo` saves to its own `key_env`, but its key is `PATH`'s start-up value, which a
    // turn's build sends.
    let start_up = std::env::var("PATH").unwrap();
    let fields = json!({"api_key_env": "DESKTOP_MODEL_UNIT_TEST_WRITE_KEY", "api_key": start_up});
    config
        .llm_providers
        .insert("mimo".into(), serde_json::from_value(fields).unwrap());
    let chat = Chat::new(config);
    let (code, _) = chat.save_key("mimo", "saved-key").unwrap_err();
    assert_eq!(code, CONFIGURED_IN_FILE);
    let env = std::fs::read_to_string(dir.path().join(".env")).unwrap();
    assert_eq!(env, "OTHER_KEY=keep-me\n");
    assert!(!chat.keys.changed("DESKTOP_MODEL_UNIT_TEST_WRITE_KEY"));
}

#[tokio::test]
async fn the_key_methods_serve_only_clients_on_this_machine() {
    let dir = tempfile::tempdir().unwrap();
    let config = GatewayConfig {
        home_dir: Some(dir.path().to_string_lossy().into_owned()),
        ..GatewayConfig::default()
    };
    let chat = Arc::new(Chat::new(config));
    let call = |origin: Origin, method: &str, params: Value| {
        let handler = METHODS.iter().find(|(m, _)| *m == method).unwrap().1;
        let (outbox, _events) = tokio::sync::mpsc::unbounded_channel();
        let mut conn = Conn::new(1, outbox, false);
        conn.origin = origin;
        let params = params.as_object().unwrap().clone();
        handler(Request {
            chat: Arc::clone(&chat),
            conn,
            params,
        })
    };
    let requests = [
        ("model.options", json!({})),
        (
            "model.save_key",
            json!({"slug": "xiaomi", "api_key": "remote-key"}),
        ),
        ("model.disconnect", json!({"slug": "xiaomi"})),
        ("reload.env", json!({})),
    ];
    for origin in [Origin::Remote, Origin::Webapp] {
        for (method, params) in &requests {
            let reply = call(origin, method, params.clone()).await;
            assert_eq!(reply.unwrap_err().0, LOCAL_ONLY, "{method} from {origin:?}");
        }
    }
    assert!(!dir.path().join(".env").exists());
    assert!(!chat.keys.changed("XIAOMI_API_KEY"));
    // The same request from this machine is served.
    assert!(call(Origin::Local, "model.options", json!({}))
        .await
        .is_ok());
}

#[test]
fn a_env_file_that_cannot_be_written_fails_the_save_and_the_disconnect() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".env")).unwrap();
    let config = GatewayConfig {
        home_dir: Some(dir.path().to_string_lossy().into_owned()),
        ..GatewayConfig::default()
    };
    let chat = Chat::new(config);
    let failed = (
        SAVE_FAILED,
        "could not write this profile's .env".to_string(),
    );
    assert_eq!(chat.save_key("xiaomi", "saved-key").unwrap_err(), failed);
    assert!(
        !chat.keys.changed("XIAOMI_API_KEY"),
        "a failed save is not used"
    );
    let failed = (DISCONNECT_FAILED, failed.1);
    assert_eq!(chat.disconnect_provider("xiaomi").unwrap_err(), failed);
}

#[test]
fn a_key_saved_for_a_provider_config_yaml_does_not_list_passes_the_set_up_check() {
    assert!(
        std::env::var_os("XIAOMI_API_KEY").is_none(),
        "this test needs XIAOMI_API_KEY unset"
    );
    let dir = tempfile::tempdir().unwrap();
    let config = GatewayConfig {
        home_dir: Some(dir.path().to_string_lossy().into_owned()),
        model: Some("xiaomi:mimo-test".into()),
        ..GatewayConfig::default()
    };
    let chat = Chat::new(config);
    let before = chat.runtime_check();
    assert_eq!(before["ok"], false, "{before}");

    let saved = chat.save_key("xiaomi", "saved-key").unwrap();
    assert_eq!(saved["provider"]["slug"], "xiaomi", "{saved}");
    assert_eq!(saved["provider"]["authenticated"], true, "{saved}");
    let check = chat.runtime_check();
    assert_eq!(check["ok"], true, "{check}");

    // Disconnected, the provider is as it was before the save: start-up never had its key.
    chat.disconnect_provider("xiaomi").unwrap();
    assert_eq!(chat.runtime_check(), before);
}

#[test]
fn a_saved_key_answers_the_picker_row_of_the_slug_sent() {
    let dir = tempfile::tempdir().unwrap();
    let var = "DESKTOP_MODEL_UNIT_TEST_ALIAS_KEY";
    let provider: LlmProviderConfig =
        serde_json::from_value(json!({ "api_key_env": var })).unwrap();
    let mut config = GatewayConfig {
        home_dir: Some(dir.path().to_string_lossy().into_owned()),
        ..GatewayConfig::default()
    };
    config.llm_providers.insert("mimo".into(), provider);
    let chat = Chat::new(config);
    // The picker lists the alias entry as `mimo`: the reply is that row.
    let saved = chat.save_key("MiMo", "saved-key").unwrap();
    assert_eq!(saved["provider"]["slug"], "mimo", "{saved}");
    assert_eq!(saved["provider"]["source"], "configured", "{saved}");
    assert_eq!(saved["provider"]["key_env"], var, "{saved}");
    assert_eq!(saved["provider"]["authenticated"], true, "{saved}");
    assert_eq!(chat.keys.value(var).as_deref(), Some("saved-key"));

    // With no entry, the picker lists the key as `xiaomi`; the reply still names `mimo`.
    let dir = tempfile::tempdir().unwrap();
    let config = GatewayConfig {
        home_dir: Some(dir.path().to_string_lossy().into_owned()),
        ..GatewayConfig::default()
    };
    let saved = Chat::new(config).save_key("mimo", "saved-key").unwrap();
    assert_eq!(
        saved["provider"],
        json!({"slug": "mimo", "name": "mimo", "total_models": 0, "models": [],
               "is_current": false, "source": "env", "authenticated": true})
    );
}

#[test]
fn the_set_up_check_reports_a_disconnected_start_up_key() {
    // Start-up read `PATH` from the environment; the gateway has since removed it.
    let provider: LlmProviderConfig =
        serde_json::from_value(json!({ "api_key_env": "PATH" })).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = GatewayConfig {
        home_dir: Some(dir.path().to_string_lossy().into_owned()),
        model: Some("envkey:m".into()),
        ..GatewayConfig::default()
    };
    config.llm_providers.insert("envkey".into(), provider);
    let chat = Chat::new(config);
    chat.keys.set("PATH", None);
    let check = chat.runtime_check();
    assert_eq!(check["ok"], false, "{check}");
    let error = check["error"].as_str().unwrap();
    assert!(error.contains("it was disconnected"), "{check}");
}

#[test]
fn a_providers_own_model_lists_first_among_its_models() {
    let dir = tempfile::tempdir().unwrap();
    let chat = xiaomi_chat(
        dir.path(),
        json!({ "model": "mimo-own", "models": ["mimo-a", "mimo-b"] }),
    );
    let options = chat.model_options(false);
    let row = options["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["slug"] == "xiaomi")
        .unwrap()
        .clone();
    assert_eq!(
        row["models"],
        json!(["mimo-own", "mimo-a", "mimo-b"]),
        "{row}"
    );
    let listed = xiaomi_chat(
        dir.path(),
        json!({ "model": "mimo-b", "models": ["mimo-a", "mimo-b"] }),
    )
    .model_options(false);
    assert_eq!(
        listed["providers"][0]["models"],
        json!(["mimo-a", "mimo-b"])
    );
}
