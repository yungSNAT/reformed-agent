use super::*;

#[test]
fn terminal_and_display_settings_round_trip() {
    const CHILD: &str = "HERMES_CONFIG_BRIDGE_FIXTURE_CHILD";
    const TEST: &str =
        "loader::tests::terminal_bridge_contract::terminal_and_display_settings_round_trip";
    if std::env::var(CHILD).as_deref() != Ok("1") {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.env_clear().env(CHILD, "1").args(["--exact", TEST]);
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(&format!("test {TEST} ... ok")),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut terminal = TerminalConfig {
        backend: TerminalBackendType::Modal,
        timeout: 41,
        max_output_size: 8_192,
        workdir: Some("fixture-workdir".into()),
        docker_container_id: Some("fixture-container".into()),
        docker_image: Some("fixture-image".into()),
        docker_mount_cwd_to_workspace: true,
        docker_run_as_host_user: true,
        container_cpu: Some(4),
        container_memory: Some(2_048),
        container_disk: Some(10_240),
        container_persistent: true,
        docker_forward_env: vec!["FIXTURE_A".into(), "FIXTURE_B".into()],
        docker_volumes: vec!["volume:/workspace".into(), "cache:/cache:ro".into()],
        vercel_runtime: Some("fixture-runtime".into()),
        modal_mode: Some("managed".into()),
        shell_init_files: vec!["fixture-a.sh".into(), "fixture-b.sh".into()],
        home_mode: TerminalHomeMode::Profile,
        ssh_host: Some("fixture.invalid".into()),
        ssh_port: Some(2_222),
        ssh_user: Some("fixture-user".into()),
        ssh_key_path: Some("fixture-key-path".into()),
        ..TerminalConfig::default()
    };
    let display = DisplayConfig {
        busy_input_mode: Some("steer".into()),
        busy_ack_enabled: Some(false),
        memory_notifications: Some(true),
        friendly_tool_labels: false,
        ..DisplayConfig::default()
    };

    set_test_env("TERMINAL_BACKEND", "daytona");
    set_test_env("TERMINAL_DOCKER_IMAGE", "  env-image  ");
    bridge_terminal_config_to_env(&terminal);
    bridge_display_config_to_env(&display);
    assert_eq!(env_var_nonempty("TERMINAL_ENV").as_deref(), Some("modal"));
    assert_eq!(
        env_var_nonempty("TERMINAL_DOCKER_IMAGE").as_deref(),
        Some("env-image")
    );
    let mut config = GatewayConfig::default();
    apply_env_overrides(&mut config);
    terminal.docker_image = Some("env-image".into());
    assert_eq!(config.terminal, terminal);
    assert_eq!(config.display, display);

    remove_test_env("TERMINAL_ENV");
    set_test_env("TERMINAL_HOME_MODE", "invalid-mode");
    set_test_env("TERMINAL_TIMEOUT", "not-a-number");
    set_test_env("TERMINAL_MAX_OUTPUT_SIZE", "-1");
    set_test_env("TERMINAL_CONTAINER_CPU", "4294967296");
    set_test_env("TERMINAL_CONTAINER_MEMORY", "not-a-number");
    set_test_env("TERMINAL_CONTAINER_DISK", "-1");
    set_test_env("TERMINAL_SSH_PORT", "65536");
    set_test_env("HERMES_GATEWAY_BUSY_ACK_ENABLED", "maybe");
    set_test_env("TERMINAL_DOCKER_FORWARD_ENV", r#"[" FIXTURE_JSON ", ""]"#);
    set_test_env("TERMINAL_SHELL_INIT_FILES", "a.sh::b.sh");
    assert!(check_terminal_backend_env().is_ok());
    apply_env_overrides(&mut config);
    terminal.backend = TerminalBackendType::Daytona;
    terminal.docker_forward_env = vec!["FIXTURE_JSON".into()];
    terminal.shell_init_files = vec!["a.sh".into(), "b.sh".into()];
    assert_eq!(config.terminal, terminal);
    assert_eq!(config.display, display);

    remove_test_env("TERMINAL_ENV");
    terminal.backend = TerminalBackendType::Singularity;
    bridge_terminal_config_to_env(&terminal);
    assert_eq!(
        env_var_nonempty("TERMINAL_ENV").as_deref(),
        Some("singularity")
    );
    apply_env_overrides(&mut config);
    assert_eq!(config.terminal, terminal);
}

#[test]
fn config_validation_identifies_the_invalid_provider_or_delegation_setting() {
    use crate::config::LlmProviderConfig;

    for (provider, expected) in [
        (
            LlmProviderConfig {
                request_timeout_seconds: Some(f64::INFINITY),
                ..LlmProviderConfig::default()
            },
            "request_timeout_seconds must be a positive finite number",
        ),
        (
            LlmProviderConfig {
                max_tokens: Some(0),
                ..LlmProviderConfig::default()
            },
            "max_tokens must be a positive integer",
        ),
        (
            LlmProviderConfig {
                models: vec!["fixture-model".into(), "  ".into()],
                ..LlmProviderConfig::default()
            },
            "models must not contain empty model ids",
        ),
    ] {
        let mut config = GatewayConfig::default();
        config.llm_providers.insert("fixture".into(), provider);
        assert!(matches!(
            validate_config(&config),
            Err(ConfigError::ValidationError(detail))
                if detail == format!("llm_providers.fixture.{expected}")
        ));
    }
    let mut config = GatewayConfig::default();
    config.delegation.api_key = Some(" ".into());
    assert!(matches!(
        validate_config(&config),
        Err(ConfigError::ValidationError(detail)) if detail == "delegation.api_key must not be empty"
    ));
}
