use super::*;

use std::ffi::OsString;

struct EnvRestore(Vec<(&'static str, Option<OsString>)>);

impl EnvRestore {
    fn capture(keys: &[&'static str]) -> Self {
        EnvRestore(
            keys.iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect(),
        )
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn config_with_provider_model(provider: &str, model: &str) -> GatewayConfig {
    let mut config = GatewayConfig::default();
    config
        .llm_providers
        .entry(provider.to_string())
        .or_default()
        .model = Some(model.to_string());
    config
}

#[test]
fn resolve_startup_model_empty_defaults_to_gpt() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::resolve_startup_model_empty_defaults_to_gpt");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let config = GatewayConfig::default();
    assert_eq!(resolve_startup_model(&config, ""), "gpt-5.5");
    assert_eq!(resolve_startup_model(&config, "   "), "gpt-5.5");
}

#[test]
fn resolve_startup_model_uses_qualified_provider_runtime_model() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::resolve_startup_model_uses_qualified_provider_runtime_model");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let config = config_with_provider_model("nous", "custom:model-x");
    assert_eq!(resolve_startup_model(&config, "nous"), "custom:model-x");
    assert_eq!(resolve_startup_model(&config, "NOUS"), "custom:model-x");
}

#[test]
fn resolve_startup_model_prefixes_bare_provider_runtime_model() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::resolve_startup_model_prefixes_bare_provider_runtime_model");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let config = config_with_provider_model("nous", "hermes-4");
    assert_eq!(resolve_startup_model(&config, "nous"), "nous:hermes-4");
}

#[test]
fn resolve_startup_model_ignores_runtime_model_equal_to_provider() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::resolve_startup_model_ignores_runtime_model_equal_to_provider");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let config = config_with_provider_model("nous", "NOUS");
    assert_eq!(resolve_startup_model(&config, "nous"), "nous");
    let empty = config_with_provider_model("nous", "   ");
    assert_eq!(resolve_startup_model(&empty, "nous"), "nous");
}

#[test]
fn sync_runtime_model_env_blank_model_is_noop() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::sync_runtime_model_env_blank_model_is_noop");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let before_model = std::env::var_os("HERMES_MODEL");
    let before_provider = std::env::var_os("HERMES_INFERENCE_PROVIDER");
    let config = GatewayConfig::default();
    sync_runtime_model_env(&config, "   ");
    assert_eq!(std::env::var_os("HERMES_MODEL"), before_model);
    assert_eq!(
        std::env::var_os("HERMES_INFERENCE_PROVIDER"),
        before_provider
    );
}

#[test]
fn parse_bool_env_recognises_values() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::parse_bool_env_recognises_values");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    for value in ["1", "true", "YES", " on "] {
        assert_eq!(parse_bool_env(value), Some(true), "value {value:?}");
    }
    for value in ["0", "False", "no", " OFF "] {
        assert_eq!(parse_bool_env(value), Some(false), "value {value:?}");
    }
    assert_eq!(parse_bool_env("maybe"), None);
    assert_eq!(parse_bool_env(""), None);
}

#[test]
fn default_pet_settings_reads_environment() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::default_pet_settings_reads_environment");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _restore = EnvRestore::capture(&[
        "HERMES_PET",
        "HERMES_PET_SPECIES",
        "HERMES_PET_MOOD",
        "HERMES_PET_DOCK",
        "HERMES_PET_TICK_MS",
    ]);

    std::env::set_var("HERMES_PET", "yes");
    std::env::set_var("HERMES_PET_SPECIES", "cat");
    std::env::set_var("HERMES_PET_MOOD", "happy");
    std::env::set_var("HERMES_PET_DOCK", " LEFT ");
    std::env::set_var("HERMES_PET_TICK_MS", " 250 ");

    let settings = default_pet_settings();
    let expected = PetSettings {
        enabled: true,
        species: "cat".to_string(),
        mood: "happy".to_string(),
        dock: PetDock::Left,
        tick_ms: 250,
    }
    .normalized();
    assert_eq!(settings.enabled, expected.enabled);
    assert_eq!(settings.species, expected.species);
    assert_eq!(settings.mood, expected.mood);
    assert_eq!(settings.tick_ms, expected.tick_ms);
    assert!(matches!(settings.dock, PetDock::Left));

    std::env::set_var("HERMES_PET", "maybe");
    std::env::remove_var("HERMES_PET_SPECIES");
    std::env::remove_var("HERMES_PET_MOOD");
    std::env::set_var("HERMES_PET_DOCK", "right");
    std::env::set_var("HERMES_PET_TICK_MS", "abc");

    let settings = default_pet_settings();
    let expected = PetSettings {
        dock: PetDock::Right,
        ..PetSettings::default()
    }
    .normalized();
    assert_eq!(settings.enabled, expected.enabled);
    assert_eq!(settings.species, expected.species);
    assert_eq!(settings.mood, expected.mood);
    assert_eq!(settings.tick_ms, expected.tick_ms);
    assert!(matches!(settings.dock, PetDock::Right));
}

#[test]
fn persist_pet_settings_reports_io_failures() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::persist_pet_settings_reports_io_failures");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let _restore = EnvRestore::capture(&["HERMES_HOME"]);

    // Directory creation failure: home lives beneath a regular file.
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let bad_home = file.path().join("sub");
    std::env::set_var("HERMES_HOME", &bad_home);
    if pet_settings_path().starts_with(&bad_home) {
        let err = persist_pet_settings(&PetSettings::default()).expect_err("must fail");
        match err {
            AgentError::Io(msg) => {
                assert!(
                    msg.contains("Failed to create pet settings directory"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    // Write failure: pet.json is a directory.
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("pet.json")).expect("create pet.json dir");
    std::env::set_var("HERMES_HOME", dir.path());
    if pet_settings_path().starts_with(dir.path()) {
        let err = persist_pet_settings(&PetSettings::default()).expect_err("must fail");
        match err {
            AgentError::Io(msg) => {
                assert!(
                    msg.contains("Failed to persist pet settings"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
