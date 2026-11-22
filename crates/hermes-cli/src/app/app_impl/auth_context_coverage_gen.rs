use super::*;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var(key, value);
        }
        EnvGuard { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        #[allow(unused_unsafe)]
        unsafe {
            std::env::remove_var(key);
        }
        EnvGuard { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        #[allow(unused_unsafe)]
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn force_preflight_explicit_env_overrides_provider() {
    if run_isolated("force_preflight_explicit_env_overrides_provider") {
        return;
    }

    let _l = lock();
    {
        let _g = EnvGuard::set("HERMES_FORCE_RUNTIME_AUTH_REFRESH", "false");
        assert!(!App::should_force_preflight_auth_refresh("nous"));
        assert!(!App::should_force_preflight_auth_refresh("qwen-oauth"));
    }
    {
        let _g = EnvGuard::set("HERMES_FORCE_RUNTIME_AUTH_REFRESH", "true");
        assert!(App::should_force_preflight_auth_refresh("openai"));
        assert!(App::should_force_preflight_auth_refresh("anthropic"));
    }
}

#[test]
fn force_preflight_defaults_by_provider_when_unset() {
    if run_isolated("force_preflight_defaults_by_provider_when_unset") {
        return;
    }

    let _l = lock();
    let _g = EnvGuard::unset("HERMES_FORCE_RUNTIME_AUTH_REFRESH");
    for p in [
        "nous",
        "qwen-oauth",
        "google-gemini-cli",
        "gemini-cli",
        "gemini-oauth",
    ] {
        assert!(App::should_force_preflight_auth_refresh(p), "{p}");
    }
    assert!(!App::should_force_preflight_auth_refresh("openai"));
    assert!(!App::should_force_preflight_auth_refresh(""));
}

#[test]
fn quorum_toolless_provider_fallback_env_toggle() {
    if run_isolated("quorum_toolless_provider_fallback_env_toggle") {
        return;
    }

    let _l = lock();
    {
        let _g = EnvGuard::set("HERMES_QUORUM_TOOLLESS_PROVIDER_FALLBACK", "false");
        assert!(!App::quorum_toolless_provider_fallback_enabled());
    }
    {
        let _g = EnvGuard::set("HERMES_QUORUM_TOOLLESS_PROVIDER_FALLBACK", "true");
        assert!(App::quorum_toolless_provider_fallback_enabled());
    }
    {
        let _g = EnvGuard::unset("HERMES_QUORUM_TOOLLESS_PROVIDER_FALLBACK");
        assert!(App::quorum_toolless_provider_fallback_enabled());
    }
}

#[test]
fn quorum_synthesis_tools_env_toggle() {
    if run_isolated("quorum_synthesis_tools_env_toggle") {
        return;
    }

    let _l = lock();
    {
        let _g = EnvGuard::set("HERMES_QUORUM_SYNTHESIS_TOOLS", "false");
        assert!(!App::quorum_synthesis_tools_enabled());
    }
    {
        let _g = EnvGuard::set("HERMES_QUORUM_SYNTHESIS_TOOLS", "true");
        assert!(App::quorum_synthesis_tools_enabled());
    }
    {
        let _g = EnvGuard::unset("HERMES_QUORUM_SYNTHESIS_TOOLS");
        assert!(App::quorum_synthesis_tools_enabled());
    }
}

#[test]
fn contextlattice_ui_status_env_toggle() {
    if run_isolated("contextlattice_ui_status_env_toggle") {
        return;
    }

    let _l = lock();
    for off in ["0", "false", "OFF", " no "] {
        let _g = EnvGuard::set("HERMES_CONTEXTLATTICE_UI_STATUS", off);
        assert!(!App::contextlattice_ui_status_enabled(), "{off}");
    }
    {
        let _g = EnvGuard::set("HERMES_CONTEXTLATTICE_UI_STATUS", "1");
        assert!(App::contextlattice_ui_status_enabled());
    }
    {
        let _g = EnvGuard::unset("HERMES_CONTEXTLATTICE_UI_STATUS");
        assert!(App::contextlattice_ui_status_enabled());
    }
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path.split_once("::").map_or(path.as_str(), |(_, rest)| rest);
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
    true
}
