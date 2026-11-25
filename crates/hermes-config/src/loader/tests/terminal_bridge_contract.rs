use super::*;

fn enter_bridge_fixture(test: &str) -> bool {
    const CHILD: &str = "HERMES_CONFIG_BRIDGE_FIXTURE_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return true;
    }
    let working_dir = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .current_dir(working_dir.path())
        .env_clear()
        .env(CHILD, test)
        .args(["--exact", test]);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(&format!("test {test} ... ok")),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

#[test]
fn terminal_and_display_settings_round_trip() {
    if !enter_bridge_fixture(
        "loader::tests::terminal_bridge_contract::terminal_and_display_settings_round_trip",
    ) {
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
fn managed_settings_override_inherited_values_in_config_and_env() {
    if !enter_bridge_fixture(
        "loader::tests::terminal_bridge_contract::managed_settings_override_inherited_values_in_config_and_env",
    ) {
        return;
    }
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let home = tempfile::tempdir().unwrap();
    let managed = tempfile::tempdir().unwrap();
    std::fs::write(
        managed.path().join("config.yaml"),
        r#"
personality: managed persona
system_prompt: managed system prompt
kanban:
  dispatch_in_gateway: false
security:
  allow_private_urls: false
terminal:
  timeout: 37
  docker_forward_env: [FIXTURE_FIRST, FIXTURE_SECOND]
"#,
    )
    .unwrap();
    set_test_env("HERMES_HOME", home.path());
    set_test_env("HERMES_MANAGED_DIR", managed.path());
    for (key, value) in [
        ("HERMES_PERSONALITY", "inherited persona"),
        ("HERMES_SYSTEM_PROMPT", "inherited prompt"),
        ("HERMES_KANBAN_DISPATCH_IN_GATEWAY", "true"),
        ("HERMES_ALLOW_PRIVATE_URLS", "true"),
        ("TERMINAL_TIMEOUT", "71"),
        ("TERMINAL_DOCKER_FORWARD_ENV", "FIXTURE_INHERITED"),
        ("TERMINAL_DOCKER_IMAGE", "fixture-image"),
    ] {
        set_test_env(key, value);
    }

    let config = load_config(Some(home.path().to_str().unwrap())).unwrap();
    assert_eq!(config.personality.as_deref(), Some("managed persona"));
    assert_eq!(
        config.system_prompt.as_deref(),
        Some("managed system prompt")
    );
    assert!(!config.kanban.dispatch_in_gateway);
    assert!(!config.security.allow_private_urls);
    assert_eq!(config.terminal.timeout, 37);
    assert_eq!(
        config.terminal.docker_forward_env,
        ["FIXTURE_FIRST", "FIXTURE_SECOND"]
    );
    assert_eq!(
        config.terminal.docker_image.as_deref(),
        Some("fixture-image")
    );
    for (key, value) in [
        ("HERMES_PERSONALITY", "managed persona"),
        ("HERMES_SYSTEM_PROMPT", "managed system prompt"),
        ("HERMES_KANBAN_DISPATCH_IN_GATEWAY", "false"),
        ("HERMES_ALLOW_PRIVATE_URLS", "false"),
        ("TERMINAL_TIMEOUT", "37"),
        (
            "TERMINAL_DOCKER_FORWARD_ENV",
            "FIXTURE_FIRST,FIXTURE_SECOND",
        ),
        ("TERMINAL_DOCKER_IMAGE", "fixture-image"),
    ] {
        assert_eq!(std::env::var(key).as_deref(), Ok(value), "{key}");
    }
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
