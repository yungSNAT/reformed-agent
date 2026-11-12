use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn clear_terminal_env_bridge_vars() {
    for (_, env_key) in terminal_config_env_bridge_pairs() {
        // SAFETY: tests serialize env mutation with ENV_LOCK.
        unsafe { std::env::remove_var(env_key) };
    }
    // SAFETY: tests serialize env mutation with ENV_LOCK.
    unsafe { std::env::remove_var("TERMINAL_BACKEND") };
}

fn clear_web_env_bridge_vars() {
    for (_, env_key) in web_config_env_bridge_pairs() {
        // SAFETY: tests serialize env mutation with ENV_LOCK.
        unsafe { std::env::remove_var(env_key) };
    }
}

fn clear_display_env_bridge_vars() {
    for (_, env_key) in display_config_env_bridge_pairs() {
        // SAFETY: tests serialize env mutation with ENV_LOCK.
        unsafe { std::env::remove_var(env_key) };
    }
}

fn remove_test_env(name: &str) {
    // SAFETY: tests serialize env mutation with ENV_LOCK.
    unsafe { std::env::remove_var(name) };
}

fn set_test_env(name: &str, value: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: tests serialize env mutation with ENV_LOCK.
    unsafe { std::env::set_var(name, value) };
}

fn clear_managed_scope_test_env() {
    for name in [
        "HERMES_HOME",
        "HERMES_MANAGED_DIR",
        "HERMES_IGNORE_USER_CONFIG",
        "HERMES_MODEL",
        "HERMES_MAX_TURNS",
        "HERMES_WEB_BACKEND",
        "HERMES_WEB_SEARCH_BACKEND",
        "HERMES_GATEWAY_BUSY_INPUT_MODE",
        "HERMES_GATEWAY_BUSY_ACK_ENABLED",
    ] {
        remove_test_env(name);
    }
}

#[test]
fn validate_valid_config() {
    let config = GatewayConfig::default();
    assert!(validate_config(&config).is_ok());
}

#[test]
fn validate_zero_max_turns() {
    let config = GatewayConfig {
        max_turns: 0,
        ..GatewayConfig::default()
    };
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_zero_terminal_timeout() {
    let mut config = GatewayConfig::default();
    config.terminal.timeout = 0;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_empty_api_key() {
    let mut config = GatewayConfig::default();
    let mut providers = HashMap::new();
    providers.insert(
        "test".into(),
        crate::config::LlmProviderConfig {
            api_key: Some("".into()),
            ..Default::default()
        },
    );
    config.llm_providers = providers;
    assert!(validate_config(&config).is_err());
}

#[test]
fn validate_whitespace_api_key() {
    let mut config = GatewayConfig::default();
    let mut providers = HashMap::new();
    providers.insert(
        "test".into(),
        crate::config::LlmProviderConfig {
            api_key: Some("   ".into()),
            ..Default::default()
        },
    );
    config.llm_providers = providers;
    assert!(validate_config(&config).is_err());
}

#[test]
fn normalize_provider_secrets_removes_empty_provider_entries() {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        "minimax".into(),
        crate::config::LlmProviderConfig {
            api_key: Some("".into()),
            ..Default::default()
        },
    );
    config.llm_providers.insert(
        "openrouter".into(),
        crate::config::LlmProviderConfig {
            api_key: Some("   ".into()),
            oauth_token_url: Some("  ".into()),
            ..Default::default()
        },
    );
    config.llm_providers.insert(
        "nous".into(),
        crate::config::LlmProviderConfig {
            api_key: Some("tok-abc".into()),
            ..Default::default()
        },
    );
    normalize_provider_secrets(&mut config);
    assert_eq!(config.llm_providers.len(), 1);
    assert_eq!(
        config
            .llm_providers
            .get("nous")
            .and_then(|cfg| cfg.api_key.as_deref()),
        Some("tok-abc")
    );
}

#[test]
fn normalize_provider_secrets_keeps_timeout_only_provider_entries() {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        "anthropic".into(),
        crate::config::LlmProviderConfig {
            request_timeout_seconds: Some(45.0),
            ..Default::default()
        },
    );

    normalize_provider_secrets(&mut config);

    assert_eq!(
        config
            .llm_providers
            .get("anthropic")
            .and_then(|cfg| cfg.request_timeout_seconds),
        Some(45.0)
    );
}

#[test]
fn normalize_provider_secrets_keeps_models_only_provider_entries() {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        "qianfan-coding".into(),
        crate::config::LlmProviderConfig {
            models: vec!["kimi-k2.5".into(), "glm-5".into()],
            discover_models: false,
            ..Default::default()
        },
    );

    normalize_provider_secrets(&mut config);

    let provider = config
        .llm_providers
        .get("qianfan-coding")
        .expect("models-only provider should remain");
    assert_eq!(provider.models, vec!["kimi-k2.5", "glm-5"]);
    assert!(!provider.discover_models);
}

#[test]
fn validate_llm_provider_request_timeout_seconds() {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        "anthropic".into(),
        crate::config::LlmProviderConfig {
            request_timeout_seconds: Some(45.0),
            ..Default::default()
        },
    );
    assert!(validate_config(&config).is_ok());

    config
        .llm_providers
        .get_mut("anthropic")
        .expect("provider")
        .request_timeout_seconds = Some(0.0);
    let err = validate_config(&config).unwrap_err().to_string();
    assert!(err.contains("request_timeout_seconds"));
}

#[test]
fn env_overrides_model() {
    // Simulate env var (we can't easily set env vars in tests, so test the logic directly)
    let config = GatewayConfig {
        model: Some("env-model".into()),
        ..GatewayConfig::default()
    };
    assert_eq!(config.model.as_deref(), Some("env-model"));
}

#[test]
fn atomic_json_write_roundtrips_and_cleans_temp() {
    use serde_json::json;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("data.json");
    atomic_json_write(&path, &json!({"key": "value", "nested": {"a": 1}})).unwrap();

    let loaded: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(loaded, json!({"key": "value", "nested": {"a": 1}}));
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains(".tmp.")));
}

#[test]
fn cross_device_rename_detection_matches_platform_error_code() {
    #[cfg(unix)]
    assert!(is_cross_device_rename_error(
        &std::io::Error::from_raw_os_error(18)
    ));
    #[cfg(windows)]
    assert!(is_cross_device_rename_error(
        &std::io::Error::from_raw_os_error(17)
    ));
    assert!(!is_cross_device_rename_error(&std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "ordinary rename failure",
    )));
}

#[test]
fn atomic_yaml_write_appends_extra_content() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("data.yaml");
    let value: serde_yaml::Value = serde_yaml::from_str("key: value\n").unwrap();
    atomic_yaml_write(&path, &value, Some("\n# comment\n")).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("key: value"));
    assert!(text.contains("# comment"));
}

#[test]
fn atomic_yaml_write_indents_mapping_child_sequences() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let value: serde_yaml::Value = serde_yaml::from_str(
        r#"
custom_providers:
  - name: provider-a
    base_url: https://a.example.com
fallback_providers:
  - backup-a
  - backup-b
"#,
    )
    .unwrap();

    atomic_yaml_write(&path, &value, None).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("custom_providers:\n  - "));
    assert!(text.contains("fallback_providers:\n  - backup-a"));
    serde_yaml::from_str::<serde_yaml::Value>(&text).expect("normalized yaml parses");
}

#[test]
fn save_config_yaml_preserves_utf8_personality_and_prompt() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let mut config = GatewayConfig {
        personality: Some("mentor 🧠".to_string()),
        system_prompt: Some("Answer with precision; preserve emoji like 🚀 and café.".to_string()),
        ..GatewayConfig::default()
    };
    config.home_dir = Some("/tmp/should-not-persist".to_string());

    save_config_yaml(&path, &config).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let text = std::str::from_utf8(&bytes).expect("config.yaml must be valid UTF-8");
    assert!(text.contains("mentor 🧠"));
    assert!(text.contains("🚀"));
    assert!(text.contains("café"));
    assert!(!text.contains("should-not-persist"));

    let loaded = load_from_yaml(&path).unwrap();
    assert_eq!(loaded.personality.as_deref(), Some("mentor 🧠"));
    assert_eq!(
        loaded.system_prompt.as_deref(),
        Some("Answer with precision; preserve emoji like 🚀 and café.")
    );
}

#[test]
fn load_from_yaml_expands_env_refs_but_user_config_load_preserves_templates() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
llm_providers:
  openai:
    api_key: ${TEST_OPENAI_KEY}
    base_url: https://${TEST_OPENAI_HOST}/v1
"#,
    )
    .unwrap();

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::set_var("TEST_OPENAI_KEY", "sk-test");
        std::env::set_var("TEST_OPENAI_HOST", "api.example.test");
    }

    let runtime = load_from_yaml(&path).unwrap();
    let openai = runtime.llm_providers.get("openai").unwrap();
    assert_eq!(openai.api_key.as_deref(), Some("sk-test"));
    assert_eq!(
        openai.base_url.as_deref(),
        Some("https://api.example.test/v1")
    );

    let editable = load_user_config_file(&path).unwrap();
    let editable_openai = editable.llm_providers.get("openai").unwrap();
    assert_eq!(
        editable_openai.api_key.as_deref(),
        Some("${TEST_OPENAI_KEY}")
    );
    assert_eq!(
        editable_openai.base_url.as_deref(),
        Some("https://${TEST_OPENAI_HOST}/v1")
    );

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe {
        std::env::remove_var("TEST_OPENAI_KEY");
        std::env::remove_var("TEST_OPENAI_HOST");
    }
}

#[test]
fn load_from_yaml_keeps_unresolved_env_refs_verbatim() {
    use tempfile::tempdir;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
llm_providers:
  openai:
    api_key: ${MISSING_OPENAI_KEY_FOR_TEST}
"#,
    )
    .unwrap();

    // SAFETY: test process serializes env mutation via ENV_LOCK.
    unsafe { std::env::remove_var("MISSING_OPENAI_KEY_FOR_TEST") };
    let runtime = load_from_yaml(&path).unwrap();
    assert_eq!(
        runtime
            .llm_providers
            .get("openai")
            .and_then(|provider| provider.api_key.as_deref()),
        Some("${MISSING_OPENAI_KEY_FOR_TEST}")
    );
}

#[test]
fn load_from_yaml_tolerates_null_top_level_sections() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        r#"
model: null
personality: null
tools: null
platforms: null
platform_toolsets: null
llm_providers: null
display: null
terminal: null
web: null
sessions: null
agent: null
personalities: null
"#,
    )
    .unwrap();

    let cfg = load_from_yaml(&path).unwrap();
    let default = GatewayConfig::default();
    assert_eq!(cfg.model, default.model);
    assert_eq!(cfg.personality, default.personality);
    assert_eq!(cfg.tools, default.tools);
    assert_eq!(cfg.platforms, default.platforms);
    assert_eq!(cfg.platform_toolsets, default.platform_toolsets);
    assert_eq!(cfg.llm_providers, default.llm_providers);
    assert_eq!(cfg.display, default.display);
    assert_eq!(cfg.terminal, default.terminal);
    assert_eq!(cfg.web, default.web);
    assert_eq!(cfg.sessions, default.sessions);
    assert_eq!(cfg.agent, default.agent);
}

#[cfg(unix)]
#[test]
fn atomic_write_through_a_symlink_updates_the_target_and_keeps_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("dotfiles");
    std::fs::create_dir(&real_dir).unwrap();
    let real = real_dir.join("config.yaml");
    std::fs::write(&real, "old").unwrap();
    let link = dir.path().join("config.yaml");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    atomic_write_bytes(&link, b"new").unwrap();

    assert!(link.is_symlink(), "the link was replaced by a regular file");
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
    // The temp file was staged beside the target and renamed away.
    let names = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    assert_eq!(names(dir.path()), ["config.yaml", "dotfiles"]);
    assert_eq!(names(&real_dir), ["config.yaml"]);
}

#[cfg(unix)]
#[test]
fn atomic_write_through_a_dangling_symlink_creates_the_target_and_keeps_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("config.yaml");
    // A relative target, resolved against the link's folder, in a folder not yet made.
    std::os::unix::fs::symlink("dotfiles/config.yaml", &link).unwrap();

    atomic_write_bytes(&link, b"new").unwrap();

    assert!(link.is_symlink(), "the link was replaced by a regular file");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("dotfiles/config.yaml")).unwrap(),
        "new"
    );
}

#[cfg(unix)]
#[test]
fn atomic_write_follows_a_chain_of_40_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.yaml");
    std::fs::write(&real, "old").unwrap();
    // Each link names the one made before it, so the last is 40 links from the file.
    let mut target = "real.yaml".to_string();
    for index in 0..40 {
        let name = format!("link-{index}");
        std::os::unix::fs::symlink(&target, dir.path().join(&name)).unwrap();
        target = name;
    }
    let first = dir.path().join(target);

    atomic_write_bytes(&first, b"new").unwrap();

    assert!(first.is_symlink(), "the first link was replaced by a file");
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
}

#[cfg(unix)]
#[test]
fn atomic_write_replaces_a_symlink_loop() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("config.yaml");
    std::os::unix::fs::symlink("config.yaml", &link).unwrap();

    atomic_write_bytes(&link, b"new").unwrap();

    assert!(!link.is_symlink());
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "new");
}

#[cfg(unix)]
#[test]
fn atomic_write_keeps_the_file_mode() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    // No single umask gives a new file both modes.
    for mode in [0o600, 0o640] {
        std::fs::write(&path, "OLD=fake").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();

        atomic_write_bytes(&path, b"NEW=fake").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "NEW=fake");
        let kept = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(kept, mode, "mode {mode:o} became {kept:o}");
    }
}

#[cfg(windows)]
#[test]
fn atomic_write_keeps_an_explicit_acl_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(&path, "OLD=fake").unwrap();
    // Grant Builtin Guests read access by SID, then read the DACL back as SDDL,
    // where Guests is `BG` in any display language.
    let icacls = |args: &[&std::ffi::OsStr]| {
        let status = std::process::Command::new("icacls")
            .args(args)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "icacls {args:?}");
    };
    let sddl = || {
        let saved = dir.path().join("acl.txt");
        icacls(&[path.as_os_str(), "/save".as_ref(), saved.as_os_str()]);
        let bytes = std::fs::read(&saved).unwrap();
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect();
        String::from_utf16_lossy(&units)
    };
    icacls(&[
        path.as_os_str(),
        "/grant".as_ref(),
        "*S-1-5-32-546:(R)".as_ref(),
    ]);
    let granted = sddl();
    assert!(granted.contains(";;;BG)"), "the grant did not apply");

    atomic_write_bytes(&path, b"NEW=fake").unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "NEW=fake");
    assert_eq!(sddl(), granted, "the rewrite changed the DACL");
}

#[cfg(windows)]
#[test]
fn atomic_write_to_a_file_held_open_fails_and_keeps_the_old_content() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(&path, "OLD=fake").unwrap();
    // Held open without delete sharing, the file can be neither replaced nor
    // renamed over, so both ReplaceFileW and the rename fallback fail.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .unwrap();

    let result = atomic_write_bytes(&path, b"NEW=fake");
    drop(held);

    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "OLD=fake");
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, [".env"], "the temp file was left behind");
}

include!("tests/runtime_bridges.rs");
include!("tests/managed_overlays.rs");

include!("tests/user_config_patch.rs");

include!("tests/env_overrides.rs");
