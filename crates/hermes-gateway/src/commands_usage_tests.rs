use super::*;

#[test]
fn model_refresh_keeps_an_explicit_provider_and_reports_incomplete_options() {
    let command = handle_command("/model survey-model --provider=fixture --refresh");
    let GatewayCommandResult::SwitchModel { request } = command else {
        panic!("expected a model switch, got {command:?}");
    };
    assert_eq!(request.model, "fixture:survey-model");
    assert_eq!(request.provider.as_deref(), Some("fixture"));
    assert!(request.force_refresh);
    assert_eq!(request.scope, ModelSwitchScope::Default);

    for (input, hint) in [
        ("/model --refresh", "refresh requires a model value"),
        ("/model --global", "Usage: /model <name>"),
        ("/model survey-model --provider", "Usage: /model <name>"),
        ("/model survey-model --provider=", "Usage: /model <name>"),
    ] {
        assert!(
            matches!(handle_command(input), GatewayCommandResult::Reply(text) if text.contains(hint)),
            "{input:?} should ask for the missing option value"
        );
    }
}

#[test]
fn informational_gateway_commands_return_guidance_without_dispatching_work() {
    for (input, hint) in [
        ("/personality", "/personality <name>"),
        ("/sethome", "/sethome <path>"),
        ("/agents", "/background list"),
        ("/tasks", "/background list"),
        ("/voice", "platform-specific"),
        ("/resume", "configured session store"),
        ("/provider", "Current provider"),
        ("/profile", "Current profile"),
        ("/tools detailed report", "Usage: /tools"),
        ("/sessions find", "Usage: /sessions search"),
    ] {
        assert!(
            matches!(handle_command(input), GatewayCommandResult::Reply(text) if text.contains(hint)),
            "{input:?} should return guidance"
        );
    }
}

#[test]
fn gateway_command_recognition_skips_empty_input_and_uses_aliases() {
    assert_eq!(canonical_command_name(" /@fixturebot "), None);
    assert!(!is_registered_command_name("/"));
    assert!(is_registered_command_name("/MCP-RELOAD@fixturebot"));
    assert!(matches!(
        handle_command("/MCP-RELOAD@fixturebot"),
        GatewayCommandResult::ReloadMcp
    ));
    assert!(matches!(
        handle_command("park survey"),
        GatewayCommandResult::Unknown(text) if text == "Not a command: park survey"
    ));
}
