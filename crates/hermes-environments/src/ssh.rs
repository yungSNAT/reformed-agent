//! SSH terminal backend – executes commands on a remote host via SSH.

use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

use hermes_core::{subprocess::CommandNoWindowExt, AgentError, CommandOutput, TerminalBackend};

/// A [`TerminalBackend`] that runs commands on a remote machine via SSH.
pub struct SshBackend {
    /// Remote hostname or IP address.
    host: String,
    /// SSH port (default: 22).
    port: u16,
    /// Username for SSH authentication.
    username: Option<String>,
    /// Path to the private key file.
    key_path: Option<String>,
    /// Default timeout in seconds.
    default_timeout: u64,
    /// Maximum output size in bytes.
    max_output_size: usize,
}

impl SshBackend {
    /// Create a new SSH backend.
    pub fn new(
        host: String,
        port: u16,
        username: Option<String>,
        key_path: Option<String>,
        default_timeout: u64,
        max_output_size: usize,
    ) -> Self {
        Self {
            host,
            port,
            username,
            key_path,
            default_timeout,
            max_output_size,
        }
    }

    /// Build the base SSH command arguments (host, port, identity, etc.).
    fn build_ssh_args(&self) -> Vec<String> {
        let mut args = vec![
            "-o".to_string(),
            "StrictHostKeyChecking=no".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(), // Non-interactive
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
        ];

        if self.port != 22 {
            args.push("-p".to_string());
            args.push(self.port.to_string());
        }

        if let Some(ref key) = self.key_path {
            args.push("-i".to_string());
            args.push(key.clone());
        }

        // Build the destination string
        let destination = if let Some(ref user) = self.username {
            format!("{}@{}", user, self.host)
        } else {
            self.host.clone()
        };
        args.push(destination);

        args
    }

    fn truncate_output(&self, s: String) -> String {
        if s.len() > self.max_output_size {
            s[..s.floor_char_boundary(self.max_output_size)].to_string()
        } else {
            s
        }
    }

    fn ssh_command(&self, args: &[String]) -> TokioCommand {
        let mut command = TokioCommand::new("ssh");
        command.args(args).stdin(Stdio::null());
        command.suppress_windows_console();
        command
    }
}

#[async_trait]
impl TerminalBackend for SshBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let timeout_secs = timeout.unwrap_or(self.default_timeout);

        let mut ssh_args = self.build_ssh_args();

        // Build the remote command
        let mut remote_command = String::new();
        if let Some(dir) = workdir {
            remote_command.push_str(&format!("cd {} && ", shlex_quote_local(dir)));
        }

        if background {
            // Run in background on the remote machine using nohup
            remote_command.push_str(&format!("nohup {} &>/dev/null &", command));
        } else {
            remote_command.push_str(command);
        }

        ssh_args.push(remote_command);

        if pty {
            // Insert -t flag for PTY allocation
            ssh_args.insert(0, "-t".to_string());
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let output = self
                .ssh_command(&ssh_args)
                .output()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to execute SSH command: {}", e)))?;

            let stdout = self.truncate_output(String::from_utf8_lossy(&output.stdout).to_string());
            let stderr = self.truncate_output(String::from_utf8_lossy(&output.stderr).to_string());

            Ok(CommandOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout,
                stderr,
            })
        })
        .await;

        match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AgentError::Timeout(format!(
                "SSH command timed out after {} seconds",
                timeout_secs
            ))),
        }
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError> {
        // Build a remote command to read the file with offset/limit
        let remote_cmd = if offset.is_some() || limit.is_some() {
            let start = offset.unwrap_or(0) + 1; // sed is 1-indexed
            if let Some(lim) = limit {
                format!(
                    "sed -n '{},{}p' {}",
                    start,
                    start + lim - 1,
                    shlex_quote_local(path)
                )
            } else {
                format!("sed -n '{},\\$p' {}", start, shlex_quote_local(path))
            }
        } else {
            format!("cat {}", shlex_quote_local(path))
        };

        let mut ssh_args = self.build_ssh_args();
        ssh_args.push(remote_cmd);

        let timeout_secs = self.default_timeout;
        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let output = self
                .ssh_command(&ssh_args)
                .output()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to read file via SSH: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(AgentError::Io(format!(
                    "Failed to read file '{}': {}",
                    path, stderr
                )));
            }

            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        })
        .await;

        match result {
            Ok(Ok(content)) => Ok(content),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AgentError::Timeout(format!(
                "SSH read_file timed out after {} seconds",
                timeout_secs
            ))),
        }
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        // Ensure parent directory exists
        let parent_dir = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        if !parent_dir.is_empty() {
            let mkdir_cmd = format!("mkdir -p {}", shlex_quote_local(&parent_dir));
            let mut ssh_args = self.build_ssh_args();
            ssh_args.push(mkdir_cmd);

            let mkdir_output = self.ssh_command(&ssh_args).output().await.map_err(|e| {
                AgentError::Io(format!("Failed to create parent dir via SSH: {}", e))
            })?;

            if !mkdir_output.status.success() {
                let stderr = String::from_utf8_lossy(&mkdir_output.stderr);
                return Err(AgentError::Io(format!(
                    "Failed to create parent directory '{}': {}",
                    parent_dir, stderr
                )));
            }
        }

        // Write content using heredoc over SSH
        let escaped_content = content.replace('\'', "'\\''");
        let write_cmd = format!(
            "cat > {} << 'HERMES_EOF'\n{}\nHERMES_EOF",
            shlex_quote_local(path),
            escaped_content
        );

        let mut ssh_args = self.build_ssh_args();
        ssh_args.push(write_cmd);

        let output = self
            .ssh_command(&ssh_args)
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to write file via SSH: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AgentError::Io(format!(
                "Failed to write file '{}': {}",
                path, stderr
            )));
        }

        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        let test_cmd = format!("test -e {}", shlex_quote_local(path));
        let mut ssh_args = self.build_ssh_args();
        ssh_args.push(test_cmd);

        let output = self
            .ssh_command(&ssh_args)
            .output()
            .await
            .map_err(|e| AgentError::Io(format!("Failed to check file via SSH: {}", e)))?;

        Ok(output.status.success())
    }
}

/// Simple shell quoting for local arguments (paths, etc.).
fn shlex_quote_local(s: &str) -> String {
    if s.is_empty() {
        "''".to_string()
    } else if !s
        .chars()
        .any(|c| c.is_whitespace() || c == '\'' || c == '"' || c == '\\' || c == '$' || c == '`')
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_args_construction() {
        let backend = SshBackend::new(
            "example.com".to_string(),
            22,
            Some("user".to_string()),
            Some("/home/user/.ssh/id_rsa".to_string()),
            120,
            1_048_576,
        );

        let args = backend.build_ssh_args();
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"/home/user/.ssh/id_rsa".to_string()));
        assert!(args.contains(&"user@example.com".to_string()));
    }

    #[test]
    fn test_ssh_args_custom_port() {
        let backend = SshBackend::new("example.com".to_string(), 2222, None, None, 120, 1_048_576);

        let args = backend.build_ssh_args();
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"2222".to_string()));
        assert!(args.contains(&"example.com".to_string()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ssh_backend_reports_command_and_file_results() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        const CHILD: &str = "HERMES_SSH_BACKEND_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::tempdir().unwrap();
            let fixture = root.path().canonicalize().unwrap();
            let ssh = fixture.join("ssh");
            std::fs::write(
                &ssh,
                r#"#!/bin/sh
pty=$1
for argument do command=$argument; done
case "$command" in
    "cd '/fixture work' && nohup fixture-command &>/dev/null &")
        [ "$pty" = '-t' ] || exit 80
        printf 'café and more'
        printf 'warning' >&2
        exit 3
        ;;
    "sed -n '2,3p' docs/notes.txt") printf 'second\nthird\n' ;;
    "cat missing.txt") printf 'fixture path missing' >&2; exit 1 ;;
    "mkdir -p docs") : > "$HOME/fixture-directory" ;;
    "cat > docs/notes.txt << 'HERMES_EOF'
Night heron
HERMES_EOF")
        [ -f "$HOME/fixture-directory" ] || exit 82
        : > "$HOME/fixture-file"
        ;;
    "test -e docs/notes.txt") [ -f "$HOME/fixture-file" ] ;;
    "test -e missing.txt") : > "$HOME/fixture-missing-check"; exit 1 ;;
    *) printf 'Unexpected arguments: %s\n' "$*" >&2; exit 81 ;;
esac
"#,
            )
            .unwrap();
            std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
            // Re-run this libtest fixture with a temporary home and a cleared environment.
            let mut child = TokioCommand::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
            child
                .env_clear()
                .env("HOME", &fixture)
                .env("PATH", &fixture)
                .env(CHILD, "1")
                .current_dir(&fixture)
                .kill_on_drop(true)
                .args([
                    "--exact",
                    "ssh::tests::ssh_backend_reports_command_and_file_results",
                    "--color",
                    "never",
                ]);
            if let Some(path) = std::env::var_os("LLVM_PROFILE_FILE") {
                child.env("LLVM_PROFILE_FILE", path);
            }
            let output = tokio::time::timeout(Duration::from_secs(12), child.output())
                .await
                .expect("isolated SSH fixture deadline")
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed; 0 failed"),
                "isolated SSH fixture exited {}\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let directory = std::env::current_dir().unwrap();
        assert_eq!(
            std::env::var_os("PATH").as_deref(),
            Some(directory.as_os_str())
        );
        assert!(directory.join("ssh").is_file());
        let backend = SshBackend::new("fixture.invalid".to_owned(), 22, None, None, 2, 4);
        let output = backend
            .execute_command("fixture-command", None, Some("/fixture work"), true, true)
            .await
            .unwrap();
        assert_eq!(
            (
                output.exit_code,
                output.stdout.as_str(),
                output.stderr.as_str()
            ),
            (3, "caf", "warn")
        );
        assert_eq!(
            backend
                .read_file("docs/notes.txt", Some(1), Some(2))
                .await
                .unwrap(),
            "second\nthird\n"
        );
        let failure = backend
            .read_file("missing.txt", None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(failure, AgentError::Io(message) if message == "Failed to read file 'missing.txt': fixture path missing")
        );
        backend
            .write_file("docs/notes.txt", "Night heron")
            .await
            .unwrap();
        assert!(backend.file_exists("docs/notes.txt").await.unwrap());
        assert!(!backend.file_exists("missing.txt").await.unwrap());
        assert!(directory.join("fixture-missing-check").is_file());
    }

    #[test]
    fn test_shlex_quote_local() {
        assert_eq!(shlex_quote_local("simple"), "simple");
        assert_eq!(shlex_quote_local(""), "''");
        assert_eq!(
            shlex_quote_local("/path/with spaces"),
            "'/path/with spaces'"
        );
    }
}
