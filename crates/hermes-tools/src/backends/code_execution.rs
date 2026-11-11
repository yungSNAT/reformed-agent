//! Real code execution backend: run snippets via tokio::process::Command.

use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

use crate::tools::code_execution::CodeExecutionBackend;
use hermes_core::{subprocess::CommandNoWindowExt, ToolError};

/// Code execution backend using local non-Python interpreters.
pub struct LocalCodeExecutionBackend {
    default_timeout_secs: u64,
}

impl LocalCodeExecutionBackend {
    pub fn new(default_timeout_secs: u64) -> Self {
        Self {
            default_timeout_secs,
        }
    }
}

impl Default for LocalCodeExecutionBackend {
    fn default() -> Self {
        Self::new(30)
    }
}

fn hermes_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HERMES_HOME")
        .or_else(|| std::env::var_os("HERMES_AGENT_ULTRA_HOME"))
        .map(std::path::PathBuf::from)
}

fn real_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HERMES_REAL_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
        .or_else(|| std::env::var_os("USERPROFILE").map(std::path::PathBuf::from))
}

fn code_subprocess_home() -> Option<std::path::PathBuf> {
    match std::env::var("TERMINAL_HOME_MODE")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("profile") | Some("isolated") => {
            let home = hermes_home_dir()?.join("home");
            if std::fs::create_dir_all(&home).is_ok() {
                Some(home)
            } else {
                real_home_dir()
            }
        }
        _ => real_home_dir(),
    }
}

fn apply_code_subprocess_home(cmd: &mut TokioCommand) {
    if let Some(real_home) = real_home_dir() {
        cmd.env("HERMES_REAL_HOME", real_home);
    }
    if let Some(home) = code_subprocess_home() {
        cmd.env("HOME", home);
    }
}

#[async_trait]
impl CodeExecutionBackend for LocalCodeExecutionBackend {
    async fn execute(
        &self,
        code: &str,
        language: Option<&str>,
        timeout: Option<u64>,
    ) -> Result<String, ToolError> {
        let Some(lang) = language else {
            return Err(ToolError::InvalidParams(
                "Missing language parameter; Reformed Agent does not default to Python in the Rust-only runtime".into(),
            ));
        };
        let timeout_secs = timeout.unwrap_or(self.default_timeout_secs);

        let (interpreter, flag) = match lang {
            "python" | "python3" => {
                return Err(ToolError::InvalidParams(
                    "Python execution is disabled in Reformed Agent's Rust-only runtime".into(),
                ))
            }
            "javascript" | "js" | "node" => ("node", "-e"),
            "typescript" | "ts" => ("npx", ""),
            "bash" | "sh" => ("bash", "-c"),
            other => {
                return Err(ToolError::InvalidParams(format!(
                    "Unsupported language: {}",
                    other
                )))
            }
        };

        let mut cmd = TokioCommand::new(interpreter);
        if lang == "typescript" {
            // For TypeScript, write to temp file and run with ts-node
            let tmp = std::env::temp_dir().join(format!("hermes_exec_{}.ts", uuid::Uuid::new_v4()));
            tokio::fs::write(&tmp, code).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to write temp file: {}", e))
            })?;
            cmd = TokioCommand::new("npx");
            cmd.arg("ts-node").arg(tmp.to_str().unwrap_or(""));
        } else {
            cmd.arg(flag).arg(code);
        }

        // Model-written code gets the environment without provider keys and
        // other credentials, except those the user passed through, as the
        // terminal tool does.
        let passthrough = crate::tools::env_passthrough::get_all_passthrough();
        cmd.env_clear().envs(
            hermes_core::subprocess_env::hermes_subprocess_env_with_passthrough(&passthrough),
        );
        apply_code_subprocess_home(&mut cmd);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.suppress_windows_console();

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let child = cmd.spawn().map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to spawn {}: {}", interpreter, e))
            })?;
            let output = child.wait_with_output().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to wait for process: {}", e))
            })?;
            Ok::<_, ToolError>((
                output.status.code().unwrap_or(-1),
                output.stdout,
                output.stderr,
            ))
        })
        .await;

        match result {
            Ok(Ok((exit_code, stdout, stderr))) => {
                let stdout_str = String::from_utf8_lossy(&stdout).to_string();
                let stderr_str = String::from_utf8_lossy(&stderr).to_string();
                Ok(json!({
                    "exit_code": exit_code,
                    "stdout": stdout_str,
                    "stderr": stderr_str,
                    "language": lang,
                })
                .to_string())
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ToolError::Timeout(format!(
                "Code execution timed out after {}s",
                timeout_secs
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    #[tokio::test]
    async fn missing_language_does_not_default_to_python() {
        let backend = LocalCodeExecutionBackend::default();
        let err = backend
            .execute("print(1)", None, Some(1))
            .await
            .expect_err("missing language should fail");
        assert!(err
            .to_string()
            .contains("does not default to Python in the Rust-only runtime"));
    }

    #[tokio::test]
    async fn python_execution_is_disabled() {
        let backend = LocalCodeExecutionBackend::default();
        let err = backend
            .execute("print(1)", Some("python"), Some(1))
            .await
            .expect_err("python should fail");
        assert!(err.to_string().contains("Python execution is disabled"));
    }

    #[test]
    fn profile_home_mode_sets_home_for_shell_snippets() {
        let _global_env = test_lock::lock();
        let real = tempfile::tempdir().expect("real home");
        let hermes = tempfile::tempdir().expect("hermes home");
        let _home = EnvGuard::set("HOME", real.path().to_string_lossy().as_ref());
        let _real_home = EnvGuard::set("HERMES_REAL_HOME", real.path().to_string_lossy().as_ref());
        let _hermes_home = EnvGuard::set("HERMES_HOME", hermes.path().to_string_lossy().as_ref());
        let _mode = EnvGuard::set("TERMINAL_HOME_MODE", "profile");
        let backend = LocalCodeExecutionBackend::default();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        // An MSYS bash on Windows prints HOME in its POSIX form, so prove
        // which directory HOME names by writing a file through it.
        let raw = runtime.block_on(async {
            backend
                .execute(
                    "printf home-probe > \"$HOME/home-probe\" && printf '%s' \"$HERMES_REAL_HOME\"",
                    Some("bash"),
                    Some(5),
                )
                .await
        });
        let raw = raw.expect("bash should run");
        let payload: serde_json::Value = serde_json::from_str(&raw).expect("json payload");
        assert_eq!(
            payload["stdout"].as_str().unwrap(),
            real.path().display().to_string()
        );
        assert_eq!(
            std::fs::read_to_string(hermes.path().join("home").join("home-probe"))
                .expect("probe written through HOME"),
            "home-probe"
        );
    }

    /// Sets `OPENAI_API_KEY` to `canary` with no passthrough in force (an
    /// empty `HERMES_HOME`, so no configured list), runs `setup`, then returns
    /// what model code sees for it. Callers hold `test_lock`.
    fn openai_key_seen_by_model_code(canary: &str, setup: impl FnOnce()) -> String {
        let home = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", home.path().to_str().unwrap());
        crate::tools::env_passthrough::reset_config_passthrough_cache_for_tests();
        let _key = EnvGuard::set("OPENAI_API_KEY", canary);
        let _force = EnvGuard::remove("_HERMES_FORCE_OPENAI_API_KEY");
        crate::tools::env_passthrough::clear_env_passthrough();
        let _pass = EnvGuard::remove("HERMES_SUBPROCESS_ENV_PASSTHROUGH");
        setup();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let raw = runtime
            .block_on(LocalCodeExecutionBackend::default().execute(
                "printf '%s' \"${OPENAI_API_KEY:-absent}\"",
                Some("bash"),
                Some(5),
            ))
            .expect("bash should run");
        crate::tools::env_passthrough::clear_env_passthrough();
        let payload: serde_json::Value = serde_json::from_str(&raw).expect("json payload");
        payload["stdout"].as_str().unwrap().to_string()
    }

    #[test]
    fn model_code_does_not_see_provider_credentials() {
        let _global_env = test_lock::lock();
        assert_eq!(openai_key_seen_by_model_code("sk-canary", || {}), "absent");
    }

    #[test]
    fn model_code_sees_credentials_the_user_passed_through() {
        let _global_env = test_lock::lock();
        let seen = openai_key_seen_by_model_code("sk-canary", || {
            std::env::set_var("HERMES_SUBPROCESS_ENV_PASSTHROUGH", "OPENAI_API_KEY");
        });
        assert_eq!(seen, "sk-canary");
    }

    #[test]
    fn model_code_sees_credentials_on_the_passthrough_list() {
        let _global_env = test_lock::lock();
        let seen = openai_key_seen_by_model_code("sk-canary", || {
            crate::tools::env_passthrough::register_env_passthrough(["OPENAI_API_KEY"]);
            // Only the list, not the variable it is mirrored to.
            std::env::remove_var("HERMES_SUBPROCESS_ENV_PASSTHROUGH");
        });
        assert_eq!(seen, "sk-canary");
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
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

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
