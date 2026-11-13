//! Path management for the hermes home directory and its files.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// hermes_home
// ---------------------------------------------------------------------------

const DEFAULT_HOME_DIR: &str = ".hermes-agent-ultra";
/// Upstream Hermes Agent's home. Never chosen implicitly: its config, sessions
/// and keys belong to upstream, and `uninstall` removes whatever home is in use.
const UPSTREAM_HOME_DIR: &str = ".hermes";

/// Return the hermes home directory.
///
/// - If the `HERMES_HOME` environment variable is set, use that.
/// - Else if `HERMES_AGENT_ULTRA_HOME` is set, use that.
/// - Otherwise `~/.hermes-agent-ultra`, even when upstream's `~/.hermes`
///   exists; set `HERMES_HOME` to use that one.
pub fn hermes_home() -> PathBuf {
    if let Some(home) = env_var_path("HERMES_HOME") {
        return home;
    }
    if let Some(home) = env_var_path("HERMES_AGENT_ULTRA_HOME") {
        return home;
    }
    user_home_dir().join(DEFAULT_HOME_DIR)
}

/// A one-line notice for stderr when no home variable is set, this fork's own
/// home does not exist yet and upstream Hermes's `~/.hermes` does: it names the
/// home in use and how to point at the old one. `None` otherwise.
pub fn upstream_home_notice() -> Option<String> {
    if env_var_path("HERMES_HOME").is_some() || env_var_path("HERMES_AGENT_ULTRA_HOME").is_some() {
        return None;
    }
    let home_dir = user_home_dir();
    let own = home_dir.join(DEFAULT_HOME_DIR);
    let upstream = home_dir.join(UPSTREAM_HOME_DIR);
    (!own.exists() && upstream.is_dir()).then(|| {
        format!(
            "Using {} as this agent's home and leaving Hermes's {} untouched; set HERMES_HOME={} to use that one instead.",
            own.display(),
            upstream.display(),
            upstream.display()
        )
    })
}

fn env_var_path(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| expand_home_spelling(&s))
}

/// Expand `$VAR`, `${VAR}` and a leading `~` in a home path taken from the
/// environment.
///
/// fish does not expand `~` in `VAR=~/...`, and every shell passes a quoted
/// value through as written, so the literal spelling can reach the process and
/// would name a directory under the working directory. Unset variables and
/// `~user` are left as written.
fn expand_home_spelling(raw: &str) -> PathBuf {
    let expanded = expand_env_vars(raw);
    match expanded.strip_prefix('~') {
        Some("") => user_home_dir(),
        Some(rest) if rest.starts_with(std::path::is_separator) => {
            user_home_dir().join(rest.trim_start_matches(std::path::is_separator))
        }
        _ => PathBuf::from(expanded),
    }
}

fn expand_env_vars(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(dollar) = rest.find('$') {
        output.push_str(&rest[..dollar]);
        let after = &rest[dollar + 1..];
        // `reference` is the text after `$` that names the variable.
        let (name, reference) = match after.strip_prefix('{') {
            Some(braced) => match braced.find('}') {
                Some(end) => (&braced[..end], &after[..end + 2]),
                None => ("", ""),
            },
            None => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (&after[..end], &after[..end])
            }
        };
        match (!name.is_empty())
            .then(|| std::env::var(name).ok())
            .flatten()
        {
            Some(value) => output.push_str(&value),
            None => {
                output.push('$');
                output.push_str(reference);
            }
        }
        rest = &after[reference.len()..];
    }
    output.push_str(rest);
    output
}

/// Rewrite `HERMES_HOME` and `HERMES_AGENT_ULTRA_HOME` with `~` and `$VAR`
/// expanded. Call once at process entry.
///
/// Many callers read these variables directly; without this each would resolve
/// a literal `~` against the working directory whilst [`hermes_home`] expands it.
pub fn normalize_hermes_home_env() {
    for var in ["HERMES_HOME", "HERMES_AGENT_ULTRA_HOME"] {
        let Ok(raw) = std::env::var(var) else {
            continue;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let expanded = expand_home_spelling(trimmed);
        if expanded.as_os_str() != raw.as_str() {
            std::env::set_var(var, expanded);
        }
    }
}

/// The user's home directory: `HOME`, else `USERPROFILE`, ignoring empty
/// values; `.` when neither is set.
pub fn user_home_dir() -> PathBuf {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|home| !home.is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// Hermes state root directory (same rule as CLI `--config-dir` / gateway data).
///
/// If `config_dir_override` is set, that path is used; otherwise [`hermes_home`].
/// Use this for `cron/`, `webhooks.json`, and other machine-local state so CLI
/// and gateway stay aligned.
pub fn state_dir(config_dir_override: Option<&Path>) -> PathBuf {
    config_dir_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(hermes_home)
}

// ---------------------------------------------------------------------------
// Derived paths
// ---------------------------------------------------------------------------

/// `$hermes_home/config.yaml`
pub fn config_path() -> PathBuf {
    hermes_home().join("config.yaml")
}

/// `$hermes_home/cli-config.yaml`
pub fn cli_config_path() -> PathBuf {
    hermes_home().join("cli-config.yaml")
}

/// `$hermes_home/gateway.json`
pub fn gateway_json_path() -> PathBuf {
    hermes_home().join("gateway.json")
}

/// PID file written by `hermes gateway start` (same directory as `config.yaml`).
pub fn gateway_pid_path() -> PathBuf {
    hermes_home().join("gateway.pid")
}

/// Gateway PID file under an explicit Hermes home directory (e.g. `HERMES_HOME` or `-C`).
pub fn gateway_pid_path_in(home: impl AsRef<std::path::Path>) -> PathBuf {
    home.as_ref().join("gateway.pid")
}

/// `$hermes_home/MEMORY.md`
pub fn memory_path() -> PathBuf {
    hermes_home().join("MEMORY.md")
}

/// `$hermes_home/USER.md`
pub fn user_path() -> PathBuf {
    hermes_home().join("USER.md")
}

/// `$hermes_home/skills/`
pub fn skills_dir() -> PathBuf {
    hermes_home().join("skills")
}

/// `$hermes_home/sessions/`
pub fn sessions_dir() -> PathBuf {
    hermes_home().join("sessions")
}

/// `$hermes_home/cron/`
pub fn cron_dir() -> PathBuf {
    hermes_home().join("cron")
}

/// `$hermes_home/.env`
pub fn env_path() -> PathBuf {
    hermes_home().join(".env")
}

/// `$hermes_home/auth.json` — credential store written by `hermes auth login`.
///
/// Mirrors Python's `tools.managed_tool_gateway.auth_json_path()`. Used by
/// the managed-tool-gateway resolver to read provider OAuth tokens.
pub fn auth_json_path() -> PathBuf {
    hermes_home().join("auth.json")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_home_respects_env() {
        // Ensure helper always yields a concrete path.
        let home = hermes_home();
        assert!(!home.as_os_str().is_empty());
    }

    /// Combined test for all path helpers.
    ///
    /// Environment-variable mutations are not thread-safe, so we test
    /// both "derived paths" and "explicit home" in a single test to
    /// avoid races with parallel test threads.
    #[test]
    fn derived_paths_and_explicit_home() {
        let _guard = crate::managed_gateway::test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();
        let original_ultra = std::env::var("HERMES_AGENT_ULTRA_HOME").ok();

        // -- Part 1: derived paths are consistent --
        std::env::set_var("HERMES_HOME", "/tmp/hermes-path-test");
        let home = hermes_home();
        assert_eq!(home, PathBuf::from("/tmp/hermes-path-test"));
        assert_eq!(state_dir(None), home);
        assert_eq!(config_path(), home.join("config.yaml"));
        assert_eq!(cli_config_path(), home.join("cli-config.yaml"));
        assert_eq!(gateway_json_path(), home.join("gateway.json"));
        assert_eq!(gateway_pid_path(), home.join("gateway.pid"));
        assert_eq!(memory_path(), home.join("MEMORY.md"));
        assert_eq!(user_path(), home.join("USER.md"));
        assert_eq!(skills_dir(), home.join("skills"));
        assert_eq!(sessions_dir(), home.join("sessions"));
        assert_eq!(cron_dir(), home.join("cron"));
        assert_eq!(env_path(), home.join(".env"));

        // -- Part 2: explicit home override --
        std::env::set_var("HERMES_HOME", "/tmp/test-hermes");
        assert_eq!(hermes_home(), PathBuf::from("/tmp/test-hermes"));
        assert_eq!(config_path(), PathBuf::from("/tmp/test-hermes/config.yaml"));

        // -- Part 3: ultra env alias works when HERMES_HOME is absent --
        std::env::remove_var("HERMES_HOME");
        std::env::set_var("HERMES_AGENT_ULTRA_HOME", "/tmp/test-hermes-ultra");
        assert_eq!(hermes_home(), PathBuf::from("/tmp/test-hermes-ultra"));
        assert_eq!(
            config_path(),
            PathBuf::from("/tmp/test-hermes-ultra/config.yaml")
        );
        std::env::remove_var("HERMES_AGENT_ULTRA_HOME");

        // Restore
        match original {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
        match original_ultra {
            Some(v) => std::env::set_var("HERMES_AGENT_ULTRA_HOME", v),
            None => std::env::remove_var("HERMES_AGENT_ULTRA_HOME"),
        }
    }

    /// Runs `body` with `HOME` set to a fresh folder and neither home variable set.
    fn with_clean_home(body: impl FnOnce(&Path)) {
        let _guard = crate::managed_gateway::test_lock::lock();
        let saved: Vec<(&str, Option<std::ffi::OsString>)> =
            ["HOME", "HERMES_HOME", "HERMES_AGENT_ULTRA_HOME"]
                .into_iter()
                .map(|key| (key, std::env::var_os(key)))
                .collect();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HOME", tmp.path());
        std::env::remove_var("HERMES_HOME");
        std::env::remove_var("HERMES_AGENT_ULTRA_HOME");
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(tmp.path())));
        for (key, value) in saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn hermes_home_never_adopts_upstream_hermes_home() {
        with_clean_home(|user_home| {
            std::fs::create_dir_all(user_home.join(".hermes")).unwrap();
            assert_eq!(hermes_home(), user_home.join(".hermes-agent-ultra"));
        });
    }

    #[test]
    fn hermes_home_variable_still_wins_over_both_defaults() {
        with_clean_home(|user_home| {
            std::fs::create_dir_all(user_home.join(".hermes")).unwrap();
            std::fs::create_dir_all(user_home.join(".hermes-agent-ultra")).unwrap();
            let chosen = user_home.join("chosen");
            std::env::set_var("HERMES_HOME", &chosen);
            assert_eq!(hermes_home(), chosen);
            std::env::set_var("HERMES_HOME", user_home.join(".hermes"));
            assert_eq!(hermes_home(), user_home.join(".hermes"));
        });
    }

    #[test]
    fn upstream_home_notice_only_before_the_own_home_exists() {
        with_clean_home(|user_home| {
            assert_eq!(upstream_home_notice(), None, "no upstream home");

            std::fs::create_dir_all(user_home.join(".hermes")).unwrap();
            let notice = upstream_home_notice().expect("notice");
            assert!(!notice.contains('\n'));
            assert!(notice.contains(&user_home.join(".hermes-agent-ultra").display().to_string()));
            assert!(notice.contains(&format!("HERMES_HOME={}", user_home.join(".hermes").display())));

            std::env::set_var("HERMES_HOME", user_home.join("chosen"));
            assert_eq!(upstream_home_notice(), None, "HERMES_HOME set");
            std::env::remove_var("HERMES_HOME");

            std::fs::create_dir_all(user_home.join(".hermes-agent-ultra")).unwrap();
            assert_eq!(upstream_home_notice(), None, "own home exists");
        });
    }

    #[test]
    fn hermes_home_expands_tilde_and_variables() {
        const ROOT_VAR: &str = "HERMES_PATHS_TEST_ROOT";
        let _guard = crate::managed_gateway::test_lock::lock();
        let original = std::env::var_os("HERMES_HOME");
        let original_ultra = std::env::var_os("HERMES_AGENT_ULTRA_HOME");
        std::env::set_var(ROOT_VAR, "/tmp/hermes-paths-root");
        std::env::remove_var("HERMES_PATHS_TEST_UNSET");
        let user_home = user_home_dir();
        let root = PathBuf::from("/tmp/hermes-paths-root");

        let cases = [
            ("~", user_home.clone()),
            ("~/profiles/work", user_home.join("profiles/work")),
            ("$HERMES_PATHS_TEST_ROOT/home", root.join("home")),
            ("${HERMES_PATHS_TEST_ROOT}/home", root.join("home")),
            (" $HERMES_PATHS_TEST_ROOT ", root.clone()),
            // Left as written: unset variables, `~user`, a bare `$` and plain paths.
            (
                "$HERMES_PATHS_TEST_UNSET/home",
                PathBuf::from("$HERMES_PATHS_TEST_UNSET/home"),
            ),
            (
                "${HERMES_PATHS_TEST_UNSET}/home",
                PathBuf::from("${HERMES_PATHS_TEST_UNSET}/home"),
            ),
            ("~other/home", PathBuf::from("~other/home")),
            ("/tmp/cost$/${open", PathBuf::from("/tmp/cost$/${open")),
            ("relative/home", PathBuf::from("relative/home")),
        ];
        for (raw, expected) in &cases {
            std::env::set_var("HERMES_HOME", raw);
            assert_eq!(&hermes_home(), expected, "HERMES_HOME={raw:?}");
        }

        // The entry-point rewrite gives direct readers of either variable the same path.
        std::env::set_var("HERMES_HOME", "~/profiles/work");
        std::env::set_var("HERMES_AGENT_ULTRA_HOME", "~/profiles/ultra");
        normalize_hermes_home_env();
        assert_eq!(
            std::env::var_os("HERMES_HOME").map(PathBuf::from),
            Some(user_home.join("profiles/work"))
        );
        assert_eq!(
            std::env::var_os("HERMES_AGENT_ULTRA_HOME").map(PathBuf::from),
            Some(user_home.join("profiles/ultra"))
        );
        std::env::set_var("HERMES_HOME", "relative/home");
        normalize_hermes_home_env();
        assert_eq!(std::env::var("HERMES_HOME").unwrap(), "relative/home");

        std::env::remove_var(ROOT_VAR);
        match original {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
        match original_ultra {
            Some(v) => std::env::set_var("HERMES_AGENT_ULTRA_HOME", v),
            None => std::env::remove_var("HERMES_AGENT_ULTRA_HOME"),
        }
    }
}
