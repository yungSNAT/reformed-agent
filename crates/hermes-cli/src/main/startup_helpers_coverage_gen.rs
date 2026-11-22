use super::*;

const PROBE_MODE: &str = "HERMES_STARTUP_HELPERS_COVERAGE_PROBE";
const PROBE_PASSED: &str = "startup helpers coverage probe passed";

fn probe_command(test_name: &str) -> std::process::Command {
    let (_, module) = module_path!()
        .split_once("::")
        .expect("the coverage tests are in a child module");
    let mut command =
        std::process::Command::new(std::env::current_exe().expect("locate the test executable"));
    command
        .arg("--exact")
        .arg(format!("{module}::{test_name}"))
        .arg("--nocapture")
        .stdin(std::process::Stdio::null());
    command
}

fn assert_probe_passed(mut command: std::process::Command) {
    let output = command.output().expect("run the test probe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains(PROBE_PASSED),
        "probe did not pass ({}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
}

#[test]
fn auth_messages_are_lowercased_for_config_tool_and_gateway_errors() {
    let message = "INVALID_GRANT: Refresh Token Expired";
    for error in [
        AgentError::Config(message.to_owned()),
        AgentError::ToolExecution(message.to_owned()),
        AgentError::Gateway(message.to_owned()),
    ] {
        assert_eq!(
            auth_error_message(&error).as_deref(),
            Some("invalid_grant: refresh token expired")
        );
        assert!(oneshot_auth_requires_fresh_login(&error));
    }
}

#[test]
fn io_error_is_not_treated_as_a_fresh_login_request() {
    let error = AgentError::from(std::io::Error::other(
        "invalid_grant: refresh token expired",
    ));

    assert_eq!(auth_error_message(&error), None);
    assert!(!oneshot_auth_requires_fresh_login(&error));
}

#[test]
fn environment_flags_and_login_prompt_in_a_noninteractive_process() {
    const FLAG_PREFIX: &str = "HERMES_STARTUP_HELPERS_COVERAGE_FLAG";
    const MISSING_FLAG: &str = "HERMES_STARTUP_HELPERS_COVERAGE_MISSING_FLAG";
    let cases = [
        ("1", true),
        (" \tTrUe\n", true),
        ("YES", true),
        (" on ", true),
        ("AuTo", true),
        ("", false),
        ("0", false),
        ("false", false),
        ("automatic", false),
    ];

    if std::env::var(PROBE_MODE).as_deref() == Ok("noninteractive") {
        assert!(!oneshot_login_prompt_available());
        assert!(!env_truthy(MISSING_FLAG));
        for (index, (value, expected)) in cases.into_iter().enumerate() {
            assert_eq!(
                env_truthy(&format!("{FLAG_PREFIX}_{index}")),
                expected,
                "unexpected classification for {value:?}"
            );
        }
        println!("{PROBE_PASSED}");
        return;
    }

    let mut command =
        probe_command("environment_flags_and_login_prompt_in_a_noninteractive_process");
    command
        .env(PROBE_MODE, "noninteractive")
        .env_remove(MISSING_FLAG);
    for (index, (value, _)) in cases.into_iter().enumerate() {
        command.env(format!("{FLAG_PREFIX}_{index}"), value);
    }
    assert_probe_passed(command);
}

#[cfg(unix)]
#[tokio::test]
async fn fresh_openai_login_rejects_nous() {
    if std::env::var(PROBE_MODE).as_deref() == Ok("unsupported-login") {
        let cli = <Cli as clap::Parser>::try_parse_from(["hermes-agent-ultra"])
            .expect("parse the default CLI options");
        let result = run_fresh_openai_oneshot_login(&cli, "nous").await;
        let Err(AgentError::Config(message)) = result else {
            panic!("expected a configuration error for Nous, got {result:?}");
        };
        assert_eq!(
            message,
            "fresh OpenAI one-shot login does not support provider 'nous'"
        );
        println!("{PROBE_PASSED}");
        return;
    }

    let home = tempfile::tempdir().expect("create an isolated home directory");
    let mut command = probe_command("fresh_openai_login_rejects_nous");
    command
        .env_clear()
        .current_dir(home.path())
        .env(PROBE_MODE, "unsupported-login")
        .env("HOME", home.path())
        .env("HERMES_HOME", home.path().join(".hermes"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CACHE_HOME", home.path().join("cache"));
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env(
            "LLVM_PROFILE_FILE",
            std::path::absolute(profile).expect("resolve the coverage profile path"),
        );
    }
    assert_probe_passed(command);
}
