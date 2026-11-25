use hermes_tools::tool_policy::{ToolPolicyEngine, ToolPolicyMode};
use serde_json::{json, Value};

// This integration test owns its process environment.
#[test]
fn environment_overrides_file_policy_and_file_overrides_preset() {
    let sandbox = tempfile::tempdir().expect("temporary policy directory");
    let file = sandbox.path().join("policy.json");
    std::fs::write(
        &file,
        json!({
            "mode": "audit",
            "allowlist": [" Terminal ", " fixture-file-allowed ", " fixture-file-denied "],
            "denylist": [" Fixture-File-Denied "],
            "deny_param_patterns": ["fixture-file-pattern"],
            "max_param_bytes": 64
        })
        .to_string(),
    )
    .expect("write file policy");
    std::env::set_var("HERMES_TOOL_POLICY_PRESET", "strict");
    for key in [
        "HERMES_TOOL_POLICY_FILE",
        "HERMES_TOOL_POLICY_MODE",
        "HERMES_TOOL_POLICY_ALLOWLIST",
        "HERMES_TOOL_POLICY_DENYLIST",
        "HERMES_TOOL_POLICY_DENY_PARAM_PATTERNS",
        "HERMES_TOOL_POLICY_MAX_PARAM_BYTES",
        "HERMES_EXECUTION_SANDBOX_PROFILE",
    ] {
        std::env::remove_var(key);
    }

    let remote_command = json!({"cmd": "ssh fixture.invalid"});
    let preset = ToolPolicyEngine::from_env().evaluate("terminal", &remote_command);
    assert_eq!(preset.mode, ToolPolicyMode::Enforce);
    assert!(!preset.allow);
    assert_eq!(preset.code.as_deref(), Some("sandbox_profile_violation"));

    std::env::set_var("HERMES_TOOL_POLICY_FILE", &file);
    let file_policy = ToolPolicyEngine::from_env();
    let file_allowed = file_policy.evaluate("fixture-file-allowed", &Value::Null);
    assert!(file_allowed.allow && !file_allowed.would_block);
    let file_command = file_policy.evaluate("terminal", &remote_command);
    assert!(file_command.allow && !file_command.would_block);
    for (name, params, code) in [
        ("fixture-file-denied", Value::Null, "tool_denylisted"),
        ("fixture-other", Value::Null, "tool_not_allowlisted"),
        (
            "terminal",
            json!({"cmd": "echo fixture-file-pattern"}),
            "params_pattern_denied",
        ),
        ("terminal", json!("x".repeat(64)), "params_too_large"),
    ] {
        let decision = file_policy.evaluate(name, &params);
        assert_eq!(decision.mode, ToolPolicyMode::Audit);
        assert!(decision.allow && decision.audited_only && decision.would_block);
        assert_eq!(decision.code.as_deref(), Some(code));
    }

    std::env::set_var("HERMES_TOOL_POLICY_MODE", " SiMuLaTe ");
    std::env::set_var(
        "HERMES_TOOL_POLICY_ALLOWLIST",
        " , TERMINAL, fixture-file-denied, fixture-env-denied ",
    );
    std::env::set_var("HERMES_TOOL_POLICY_DENYLIST", ", Fixture-Env-Denied ");
    std::env::set_var(
        "HERMES_TOOL_POLICY_DENY_PARAM_PATTERNS",
        "fixture-env-pattern",
    );
    std::env::set_var("HERMES_TOOL_POLICY_MAX_PARAM_BYTES", "128");
    std::env::set_var("HERMES_EXECUTION_SANDBOX_PROFILE", "strict");
    let overrides = ToolPolicyEngine::from_env();
    for (name, params, code, would_block) in [
        (
            "fixture-file-denied",
            Value::Null,
            "simulation_allow",
            false,
        ),
        ("fixture-env-denied", Value::Null, "tool_denylisted", true),
        (
            "fixture-file-allowed",
            Value::Null,
            "tool_not_allowlisted",
            true,
        ),
        (
            "terminal",
            json!({"cmd": "echo fixture-file-pattern"}),
            "simulation_allow",
            false,
        ),
        (
            "terminal",
            json!({"cmd": "echo fixture-env-pattern"}),
            "params_pattern_denied",
            true,
        ),
        ("terminal", json!("x".repeat(64)), "simulation_allow", false),
        ("terminal", json!("x".repeat(128)), "params_too_large", true),
        (
            "terminal",
            remote_command,
            "sandbox_profile_violation",
            true,
        ),
    ] {
        let decision = overrides.evaluate(name, &params);
        assert_eq!(decision.mode.as_str(), "simulate");
        assert!(decision.allow && decision.simulated);
        assert_eq!(decision.audited_only, would_block, "{name} {params}");
        assert_eq!(decision.would_block, would_block, "{name} {params}");
        assert_eq!(decision.code.as_deref(), Some(code), "{name} {params}");
    }
}
