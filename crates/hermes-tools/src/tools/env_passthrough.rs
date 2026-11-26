use std::collections::BTreeSet;
use std::sync::{LazyLock, Mutex};

use hermes_core::subprocess_env::normalize_env_passthrough_name as normalize_name;
pub use hermes_core::subprocess_env::SUBPROCESS_ENV_PASSTHROUGH_VAR;

static SESSION_PASSTHROUGH: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));
static CONFIG_PASSTHROUGH: LazyLock<Mutex<Option<BTreeSet<String>>>> =
    LazyLock::new(|| Mutex::new(None));

fn load_config_passthrough_uncached() -> BTreeSet<String> {
    let home = hermes_config::hermes_home();
    // A relative home (no HOME or USERPROFILE) sits in the working folder,
    // which a cloned repository controls; its config must not choose which
    // host variables reach the terminal.
    if home.is_relative() {
        return BTreeSet::new();
    }
    // With the managed scope laid over it, as `load_config` reads it, so a list
    // the administrator pins applies.
    let Some(config) = hermes_config::load_effective_config_yaml_value(&home.join("config.yaml"))
    else {
        return BTreeSet::new();
    };
    config
        .pointer("/terminal/env_passthrough")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter_map(normalize_name)
        .collect()
}

fn config_passthrough() -> BTreeSet<String> {
    let mut guard = CONFIG_PASSTHROUGH.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = guard.as_ref() {
        return cached.clone();
    }
    let loaded = load_config_passthrough_uncached();
    *guard = Some(loaded.clone());
    loaded
}

fn sync_process_env_from_set(values: &BTreeSet<String>) {
    if values.is_empty() {
        std::env::remove_var(SUBPROCESS_ENV_PASSTHROUGH_VAR);
    } else {
        std::env::set_var(
            SUBPROCESS_ENV_PASSTHROUGH_VAR,
            values.iter().cloned().collect::<Vec<_>>().join(" "),
        );
    }
}

pub fn register_env_passthrough<I, S>(var_names: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    {
        let mut allowed = SESSION_PASSTHROUGH
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for name in var_names
            .into_iter()
            .filter_map(|v| normalize_name(v.as_ref()))
        {
            allowed.insert(name);
        }
    }
    sync_process_env_from_set(&get_all_passthrough());
}

pub fn clear_env_passthrough() {
    SESSION_PASSTHROUGH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    sync_process_env_from_set(&get_all_passthrough());
}

pub fn is_env_passthrough(var_name: &str) -> bool {
    let Some(name) = normalize_name(var_name) else {
        return false;
    };
    if SESSION_PASSTHROUGH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&name)
    {
        return true;
    }
    config_passthrough().contains(&name)
}

pub fn get_all_passthrough() -> BTreeSet<String> {
    let mut all = config_passthrough();
    all.extend(
        SESSION_PASSTHROUGH
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned(),
    );
    all
}

#[cfg(test)]
pub fn reset_config_passthrough_cache_for_tests() {
    *CONFIG_PASSTHROUGH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    sync_process_env_from_set(&get_all_passthrough());
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn reset() {
        SESSION_PASSTHROUGH
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *CONFIG_PASSTHROUGH.lock().unwrap_or_else(|e| e.into_inner()) = None;
        std::env::remove_var(SUBPROCESS_ENV_PASSTHROUGH_VAR);
    }

    #[test]
    fn registry_trims_skips_empty_and_clears_session_names() {
        let _global_env = test_lock::lock();
        reset();

        assert!(!is_env_passthrough("TENOR_API_KEY"));
        register_env_passthrough(["  TENOR_API_KEY  ", "", "  ", "BAR_SECRET"]);
        assert!(is_env_passthrough("TENOR_API_KEY"));
        assert!(is_env_passthrough("BAR_SECRET"));
        assert!(!is_env_passthrough(""));
        assert_eq!(
            std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR).unwrap(),
            "BAR_SECRET TENOR_API_KEY"
        );

        clear_env_passthrough();
        assert!(!is_env_passthrough("TENOR_API_KEY"));
        assert!(std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR).is_err());
    }

    #[test]
    fn config_passthrough_is_loaded_from_hermes_home() {
        let _global_env = test_lock::lock();
        reset();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("config.yaml"),
            "terminal:\n  env_passthrough:\n    - MY_CUSTOM_KEY\n    - '  ANOTHER_TOKEN  '\n",
        )
        .expect("write config");
        let _home = EnvVarGuard::set("HERMES_HOME", tmp.path());
        reset_config_passthrough_cache_for_tests();

        assert!(is_env_passthrough("MY_CUSTOM_KEY"));
        assert!(is_env_passthrough("ANOTHER_TOKEN"));
        assert!(!is_env_passthrough("UNRELATED_VAR"));

        register_env_passthrough(["SKILL_KEY"]);
        let all = get_all_passthrough();
        assert!(all.contains("MY_CUSTOM_KEY"));
        assert!(all.contains("SKILL_KEY"));
        assert_eq!(
            std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR).unwrap(),
            "ANOTHER_TOKEN MY_CUSTOM_KEY SKILL_KEY"
        );

        reset();
    }

    #[test]
    fn config_passthrough_reads_the_default_home_not_upstreams() {
        let _global_env = test_lock::lock();
        reset();
        let tmp = tempfile::tempdir().expect("tempdir");
        for (dir, key) in [
            (".hermes-agent-ultra", "OWN_HOME_KEY"),
            (".hermes", "UPSTREAM_HOME_KEY"),
        ] {
            std::fs::create_dir_all(tmp.path().join(dir)).expect("home dir");
            std::fs::write(
                tmp.path().join(dir).join("config.yaml"),
                format!(
                    "terminal:
  env_passthrough:
    - {key}
"
                ),
            )
            .expect("write config");
        }
        let _hermes_home = EnvVarGuard::remove("HERMES_HOME");
        let _ultra_home = EnvVarGuard::remove("HERMES_AGENT_ULTRA_HOME");
        let _home = EnvVarGuard::set("HOME", tmp.path());
        reset_config_passthrough_cache_for_tests();

        assert!(is_env_passthrough("OWN_HOME_KEY"));
        assert!(!is_env_passthrough("UPSTREAM_HOME_KEY"));

        reset();
    }

    #[test]
    fn config_passthrough_honours_the_managed_scope() {
        let _global_env = test_lock::lock();
        reset();
        let home = tempfile::tempdir().expect("home");
        let managed = tempfile::tempdir().expect("managed");
        std::fs::write(
            managed.path().join("config.yaml"),
            "terminal:\n  env_passthrough:\n    - MANAGED_ONLY_KEY\n",
        )
        .expect("write managed config");
        let _home = EnvVarGuard::set("HERMES_HOME", home.path());
        let _managed = EnvVarGuard::set("HERMES_MANAGED_DIR", managed.path());

        // A list only the administrator sets still applies.
        reset_config_passthrough_cache_for_tests();
        assert!(is_env_passthrough("MANAGED_ONLY_KEY"));

        // The administrator's list replaces the profile's, as `load_config` merges it.
        std::fs::write(
            home.path().join("config.yaml"),
            "terminal:\n  env_passthrough:\n    - PROFILE_KEY\n",
        )
        .expect("write config");
        reset_config_passthrough_cache_for_tests();
        assert!(is_env_passthrough("MANAGED_ONLY_KEY"));
        assert!(!is_env_passthrough("PROFILE_KEY"));

        reset();
    }
}
