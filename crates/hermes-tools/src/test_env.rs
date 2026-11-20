//! Environment helper shared by this crate's tests.

/// Runs `check` with `vars` set (None removes one), under the shared
/// environment lock, then restores them.
pub(crate) fn with_env<T>(
    vars: &[(&str, Option<&std::path::Path>)],
    check: impl FnOnce() -> T,
) -> T {
    let _lock = hermes_config::managed_gateway::test_lock::lock();
    let saved: Vec<_> = vars
        .iter()
        .map(|(key, _)| (*key, std::env::var_os(key)))
        .collect();
    for (key, value) in vars {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check));
    for (key, value) in saved {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}
