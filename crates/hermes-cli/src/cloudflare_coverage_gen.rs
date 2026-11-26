use super::*;

fn block<F: std::future::Future>(fut: F) -> F::Output {
    futures::executor::block_on(fut)
}

#[test]
fn cloudflare_help_lists_usage_and_actions() {
    let help = cloudflare_help();
    assert!(help.starts_with("Usage:"));
    assert!(help.contains("hermes cloudflare parse-temporary-deploy-output < wrangler.log"));
    assert!(help.contains("hermes cloudflare --selftest"));
    assert!(help.contains("Actions:"));
    assert!(
        help.contains("parse-temporary-deploy          Alias for parse-temporary-deploy-output")
    );
    assert!(
        help.contains("parse                           Alias for parse-temporary-deploy-output")
    );
}

#[test]
fn run_selftest_succeeds_on_builtin_sample() {
    assert!(run_selftest().is_ok());
}

#[test]
fn handle_selftest_flag_runs_selftest() {
    assert!(block(handle_cli_cloudflare(None, true)).is_ok());
}

#[test]
fn handle_selftest_flag_takes_precedence_over_unknown_action() {
    let result = block(handle_cli_cloudflare(
        Some("definitely-unknown".to_string()),
        true,
    ));
    assert!(result.is_ok());
}

#[test]
fn handle_selftest_action_runs_selftest() {
    let result = block(handle_cli_cloudflare(Some("selftest".to_string()), false));
    assert!(result.is_ok());
}

#[test]
fn handle_no_action_defaults_to_help() {
    assert!(block(handle_cli_cloudflare(None, false)).is_ok());
}

#[test]
fn handle_help_aliases_succeed() {
    for action in ["help", "-h", "--help"] {
        let result = block(handle_cli_cloudflare(Some(action.to_string()), false));
        assert!(result.is_ok(), "action {action} should succeed");
    }
}

#[test]
fn handle_unknown_action_returns_config_error_with_help() {
    let result = block(handle_cli_cloudflare(
        Some("bogus-action".to_string()),
        false,
    ));
    match result {
        Err(hermes_core::AgentError::Config(msg)) => {
            assert!(msg.starts_with("Unknown cloudflare action 'bogus-action'.\n"));
            assert!(msg.contains(cloudflare_help()));
        }
        Err(_) => panic!("expected AgentError::Config variant"),
        Ok(()) => panic!("expected error for unknown action"),
    }
}
