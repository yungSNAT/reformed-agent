//! Backend manager – orchestrates the active terminal backend.

use std::sync::Arc;

use async_trait::async_trait;
use hermes_config::{TerminalBackendType, TerminalConfig};
use hermes_core::{AgentError, CommandOutput, TerminalBackend};

use crate::local::LocalBackend;

#[cfg(feature = "docker")]
use crate::docker::DockerBackend;

#[cfg(feature = "ssh")]
use crate::ssh::SshBackend;

#[cfg(feature = "daytona")]
use crate::daytona::DaytonaBackend;

#[cfg(feature = "modal")]
use crate::modal::ModalBackend;

#[cfg(feature = "singularity")]
use crate::singularity::SingularityBackend;

/// Stands in for a configured backend this build lacks. Every operation
/// fails: the user chose that backend for isolation, so running on the host
/// instead would remove it.
struct UnavailableBackend {
    reason: String,
}

impl UnavailableBackend {
    fn new(backend: TerminalBackendType) -> Self {
        let name = match backend {
            TerminalBackendType::Local => "local",
            TerminalBackendType::Docker => "docker",
            TerminalBackendType::Ssh => "ssh",
            TerminalBackendType::Daytona => "daytona",
            TerminalBackendType::Modal => "modal",
            TerminalBackendType::Singularity => "singularity",
        };
        Self {
            reason: format!(
                "terminal.backend is `{name}`, but this build does not include that backend \
                 (Cargo feature `{name}` of hermes-environments), so commands and file \
                 operations are refused rather than run on this machine. Set terminal.backend \
                 to `local` or use a build with the feature."
            ),
        }
    }

    fn refuse<T>(&self) -> Result<T, AgentError> {
        Err(AgentError::Config(self.reason.clone()))
    }
}

#[async_trait]
impl TerminalBackend for UnavailableBackend {
    async fn execute_command(
        &self,
        _command: &str,
        _timeout: Option<u64>,
        _workdir: Option<&str>,
        _background: bool,
        _pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        self.refuse()
    }

    async fn read_file(
        &self,
        _path: &str,
        _offset: Option<u64>,
        _limit: Option<u64>,
    ) -> Result<String, AgentError> {
        self.refuse()
    }

    async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
        self.refuse()
    }

    async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
        self.refuse()
    }
}

/// Manages the active terminal backend and provides a unified interface
/// for command execution and file operations regardless of backend type.
pub struct BackendManager {
    current_backend: Arc<dyn TerminalBackend>,
    active_backend_type: TerminalBackendType,
    config: TerminalConfig,
}

impl BackendManager {
    /// Create a new `BackendManager` with the given configuration.
    ///
    /// The initial backend is selected based on `config.backend`.
    /// If the selected backend type is not compiled in, every operation fails
    /// with a reason naming its Cargo feature; nothing runs on the host instead.
    pub fn new(config: TerminalConfig) -> Self {
        let (backend, active_backend_type) = Self::create_backend(&config);
        Self {
            current_backend: backend,
            active_backend_type,
            config,
        }
    }

    /// Create the appropriate backend based on configuration.
    fn create_backend(config: &TerminalConfig) -> (Arc<dyn TerminalBackend>, TerminalBackendType) {
        match config.backend {
            TerminalBackendType::Local => (
                Arc::new(LocalBackend::from_terminal_config(config)),
                TerminalBackendType::Local,
            ),
            #[cfg(feature = "docker")]
            TerminalBackendType::Docker => (
                Arc::new(DockerBackend::new(
                    config.docker_container_id.clone(),
                    config.docker_image.clone(),
                    config.docker_mount_cwd_to_workspace,
                    config.docker_run_as_host_user,
                    config.container_cpu,
                    config.container_memory,
                    config.container_disk,
                    config.container_persistent,
                    config.docker_env.clone(),
                    config.docker_forward_env.clone(),
                    config.docker_volumes.clone(),
                    config.timeout,
                    config.max_output_size,
                )),
                TerminalBackendType::Docker,
            ),
            #[cfg(feature = "ssh")]
            TerminalBackendType::Ssh => (
                Arc::new(SshBackend::new(
                    config
                        .ssh_host
                        .clone()
                        .unwrap_or_else(|| "localhost".to_string()),
                    config.ssh_port.unwrap_or(22),
                    config.ssh_user.clone(),
                    config.ssh_key_path.clone(),
                    config.timeout,
                    config.max_output_size,
                )),
                TerminalBackendType::Ssh,
            ),
            #[cfg(feature = "daytona")]
            TerminalBackendType::Daytona => (
                Arc::new(DaytonaBackend::new(
                    None,
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                )),
                TerminalBackendType::Daytona,
            ),
            #[cfg(feature = "modal")]
            TerminalBackendType::Modal => (
                Arc::new(ModalBackend::new(
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                )),
                TerminalBackendType::Modal,
            ),
            #[cfg(feature = "singularity")]
            TerminalBackendType::Singularity => (
                Arc::new(SingularityBackend::new(
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                )),
                TerminalBackendType::Singularity,
            ),
            #[allow(unreachable_patterns)]
            missing => {
                let backend = UnavailableBackend::new(missing);
                tracing::error!("{}", backend.reason);
                (Arc::new(backend), missing)
            }
        }
    }

    /// Clone the currently selected backend.
    pub fn terminal_backend(&self) -> Arc<dyn TerminalBackend> {
        self.current_backend.clone()
    }

    /// Execute a command through the active backend.
    pub async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        self.current_backend
            .execute_command(command, timeout, workdir, background, pty)
            .await
    }

    /// Read a file through the active backend.
    pub async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError> {
        self.current_backend.read_file(path, offset, limit).await
    }

    /// Write a file through the active backend.
    pub async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        self.current_backend.write_file(path, content).await
    }

    /// Check if a file exists through the active backend.
    pub async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        self.current_backend.file_exists(path).await
    }

    /// Switch to a different backend type at runtime.
    ///
    /// Creates a new backend of the specified type and replaces the active one.
    /// If the feature for the requested backend is not compiled in, returns an error.
    pub fn switch_backend(&mut self, backend_type: TerminalBackendType) -> Result<(), AgentError> {
        // Validate that the backend type is available
        match backend_type {
            TerminalBackendType::Local => {}
            #[cfg(feature = "docker")]
            TerminalBackendType::Docker => {}
            #[cfg(feature = "ssh")]
            TerminalBackendType::Ssh => {}
            #[cfg(feature = "daytona")]
            TerminalBackendType::Daytona => {}
            #[cfg(feature = "modal")]
            TerminalBackendType::Modal => {}
            #[cfg(feature = "singularity")]
            TerminalBackendType::Singularity => {}
            #[allow(unreachable_patterns)]
            _ => {
                return Err(AgentError::Config(format!(
                    "Backend type {:?} is not available (feature not enabled at compile time)",
                    backend_type
                )));
            }
        }

        self.config.backend = backend_type;
        let (backend, active_backend_type) = Self::create_backend(&self.config);
        self.current_backend = backend;
        self.active_backend_type = active_backend_type;
        Ok(())
    }

    /// Get a reference to the current configuration.
    pub fn config(&self) -> &TerminalConfig {
        &self.config
    }

    /// Get the current backend type.
    pub fn backend_type(&self) -> TerminalBackendType {
        self.active_backend_type
    }

    /// Get the configured backend type from the provided config.
    pub fn configured_backend_type(&self) -> TerminalBackendType {
        self.config.backend
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::TerminalConfig;

    #[test]
    fn local_backend_selected_from_default_config() {
        let manager = BackendManager::new(TerminalConfig::default());
        assert_eq!(manager.backend_type(), TerminalBackendType::Local);
        assert_eq!(
            manager.configured_backend_type(),
            TerminalBackendType::Local
        );
    }

    /// Backends this build lacks, with their Cargo feature names.
    fn missing_backends() -> Vec<(TerminalBackendType, &'static str)> {
        [
            (
                TerminalBackendType::Docker,
                "docker",
                cfg!(feature = "docker"),
            ),
            (TerminalBackendType::Ssh, "ssh", cfg!(feature = "ssh")),
            (
                TerminalBackendType::Daytona,
                "daytona",
                cfg!(feature = "daytona"),
            ),
            (TerminalBackendType::Modal, "modal", cfg!(feature = "modal")),
            (
                TerminalBackendType::Singularity,
                "singularity",
                cfg!(feature = "singularity"),
            ),
        ]
        .into_iter()
        .filter(|(_, _, built)| !built)
        .map(|(backend, feature, _)| (backend, feature))
        .collect()
    }

    /// A configured backend the build lacks must refuse every operation,
    /// never run it on the host: the user chose that backend for isolation.
    #[tokio::test]
    async fn a_configured_backend_this_build_lacks_refuses_to_run_on_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran-on-host");
        let file = dir.path().join("host-file.txt");
        std::fs::write(&file, "host secret").unwrap();
        let missing = missing_backends();
        assert!(
            !missing.is_empty(),
            "every backend is built in; nothing to check"
        );
        for (backend, feature) in missing {
            let manager = BackendManager::new(TerminalConfig {
                backend,
                ..TerminalConfig::default()
            });
            let command = format!("echo ran > \"{}\"", marker.display());
            let err = manager
                .execute_command(&command, Some(20), None, false, false)
                .await
                .expect_err("a missing backend must not run commands");
            let text = err.to_string();
            assert!(
                text.contains(&format!("`{feature}`")) && text.contains("terminal.backend"),
                "{text}"
            );
            assert!(!marker.exists(), "{feature}: the command ran on the host");

            let path = file.to_str().unwrap();
            assert!(manager.read_file(path, None, None).await.is_err());
            assert!(manager.file_exists(path).await.is_err());
            assert!(manager.write_file(path, "overwritten").await.is_err());
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "host secret");

            assert_eq!(manager.configured_backend_type(), backend);
            assert_eq!(manager.backend_type(), backend, "not reported as local");
        }
    }
}
