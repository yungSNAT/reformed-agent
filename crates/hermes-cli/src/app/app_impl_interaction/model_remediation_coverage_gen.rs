use super::*;

#[test]
fn model_auto_remediation_environment_setting() {
    const SETTING: &str = "HERMES_MODEL_AUTO_REMEDIATE";
    const EXPECTED: &str = "HERMES_TEST_MODEL_REMEDIATION_EXPECTED";
    const PROBE_PASSED: &str = "model remediation environment probe passed";

    if let Ok(expected) = std::env::var(EXPECTED) {
        assert_eq!(
            App::model_auto_remediation_enabled(),
            expected.parse::<bool>().expect("expected probe result"),
            "{SETTING} = {:?}",
            std::env::var_os(SETTING),
        );
        println!("{PROBE_PASSED}");
        return;
    }

    // Run each case in isolation so parallel tests never see a changed setting.
    let executable = std::env::current_exe().expect("locate the test executable");
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let test_name = format!("{module}::model_auto_remediation_environment_setting");

    for (value, enabled) in [
        (None, true),
        (Some("0"), false),
        (Some(" FALSE \t"), false),
        (Some(" oFf "), false),
        (Some("\tNo "), false),
        (Some("1"), true),
        (Some(" TrUe "), true),
        (Some(""), true),
        (Some("unknown"), true),
    ] {
        let mut command = std::process::Command::new(&executable);
        command
            .args(["--exact", test_name.as_str(), "--nocapture"])
            .env(EXPECTED, enabled.to_string())
            .env_remove(SETTING);
        if let Some(value) = value {
            command.env(SETTING, value);
        }

        let output = command.output().expect("run the environment probe");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(PROBE_PASSED),
            "{SETTING} = {value:?}: {}\nstdout:\n{stdout}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn model_not_found_requires_model_context_and_a_supported_error_kind() {
    for error in [
        AgentError::LlmApi("MODEL NOT FOUND".into()),
        AgentError::Config("The requested MODEL does not exist".into()),
        AgentError::ToolExecution("404 NOT FOUND while loading model example".into()),
        AgentError::Gateway("Model missing from the OPENROUTER CATALOG".into()),
    ] {
        assert!(App::is_model_not_found_error(&error), "{error:?}");
    }

    for error in [
        AgentError::Gateway("HTTP 404 Not Found: route is missing".into()),
        AgentError::Config("OpenRouter catalog is unavailable".into()),
        AgentError::ToolExecution("Model request timed out".into()),
        AgentError::AuthFailed("Model not found".into()),
    ] {
        assert!(!App::is_model_not_found_error(&error), "{error:?}");
    }
}

#[test]
fn provider_auth_errors_are_distinguished_from_other_provider_failures() {
    for error in [
        AgentError::LlmApi("HTTP 401".into()),
        AgentError::Config("INVALID TOKEN".into()),
        AgentError::ToolExecution("TOKEN_EXPIRED".into()),
        AgentError::Gateway("HTTP 403 Forbidden".into()),
        AgentError::AuthFailed("SESSION EXPIRED".into()),
    ] {
        assert!(App::is_provider_auth_or_session_error(&error), "{error:?}");
    }

    for error in [
        AgentError::Config("Model not found".into()),
        AgentError::ToolExecution("Invalid tool schema".into()),
        AgentError::Gateway("HTTP 503: provider unavailable".into()),
    ] {
        assert!(!App::is_provider_auth_or_session_error(&error), "{error:?}",);
    }
}

#[test]
fn provider_tool_payload_errors_recognize_distinct_rejection_shapes() {
    for error in [
        AgentError::LlmApi("Provider returned error: request is not valid for a function".into()),
        AgentError::Config(
            "Provider returned error: expected a VALID PAYLOAD for a function".into(),
        ),
        AgentError::ToolExecution(
            "Provider returned error for function: CHECK THE MODEL NAME".into(),
        ),
        AgentError::Gateway("Provider returned error: INVALID SCHEMA".into()),
        AgentError::AuthFailed(
            "NO CHOICES IN RESPONSE: request is not valid for a function".into(),
        ),
        AgentError::LlmApi("Empty choices array: expected a valid payload for a function".into()),
        AgentError::Config("No choices in response for a function: provider returned error".into()),
        AgentError::ToolExecution("Empty choices array: invalid schema".into()),
        AgentError::Gateway("Tool schema rejected".into()),
        AgentError::LlmApi("Invalid input for a function".into()),
        AgentError::LlmApi("HTTP 422: expected a valid payload".into()),
    ] {
        assert!(App::is_provider_tool_payload_error(&error), "{error:?}");
    }

    for error in [
        AgentError::Config("Provider returned error: check the model name".into()),
        AgentError::ToolExecution("Provider returned error for a function call".into()),
        AgentError::Gateway("Empty choices array: invalid model".into()),
        AgentError::LlmApi("No choices in response for a function call".into()),
        AgentError::LlmApi("HTTP 400: expected a valid payload".into()),
        AgentError::LlmApi("HTTP 422: request rejected".into()),
        AgentError::AuthFailed("Invalid token".into()),
    ] {
        assert!(!App::is_provider_tool_payload_error(&error), "{error:?}");
    }
}
