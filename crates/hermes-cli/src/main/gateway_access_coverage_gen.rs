use super::*;

fn enabled_platform() -> PlatformConfig {
    PlatformConfig {
        enabled: true,
        ..Default::default()
    }
}

#[test]
fn normalize_tool_progress_mode_maps_aliases() {
    assert_eq!(
        normalize_gateway_tool_progress_mode(" OFF "),
        Some("off".to_string())
    );
    assert_eq!(
        normalize_gateway_tool_progress_mode("none"),
        Some("off".to_string())
    );
    assert_eq!(
        normalize_gateway_tool_progress_mode("0"),
        Some("off".to_string())
    );
    assert_eq!(
        normalize_gateway_tool_progress_mode("new"),
        Some("new".to_string())
    );
    assert_eq!(
        normalize_gateway_tool_progress_mode("true"),
        Some("all".to_string())
    );
    assert_eq!(
        normalize_gateway_tool_progress_mode("1"),
        Some("all".to_string())
    );
    assert_eq!(
        normalize_gateway_tool_progress_mode("Verbose"),
        Some("verbose".to_string())
    );
    assert_eq!(normalize_gateway_tool_progress_mode("bogus"), None);
}

#[test]
fn resolve_tool_progress_mode_prefers_override_then_platform_default() {
    let config = hermes_config::GatewayConfig::default();
    assert_eq!(
        resolve_gateway_tool_progress_mode(&config, "telegram", Some("verbose")),
        "verbose"
    );
    assert_eq!(
        resolve_gateway_tool_progress_mode(&config, "discord", Some("none")),
        "off"
    );
    assert_eq!(
        resolve_gateway_tool_progress_mode(&config, "Telegram", Some("garbage")),
        "off"
    );
    assert_eq!(
        resolve_gateway_tool_progress_mode(&config, "slack", None),
        "off"
    );
    assert_eq!(
        resolve_gateway_tool_progress_mode(&config, "discord", None),
        "all"
    );
}

#[test]
fn should_emit_tool_progress_respects_mode() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::HashSet::<String>::new(),
    ));
    assert!(!should_emit_gateway_tool_progress("off", "terminal", &seen));
    assert!(should_emit_gateway_tool_progress("new", "terminal", &seen));
    assert!(!should_emit_gateway_tool_progress("new", "terminal", &seen));
    assert!(should_emit_gateway_tool_progress("new", "browser", &seen));
    assert!(should_emit_gateway_tool_progress("all", "terminal", &seen));
    assert!(should_emit_gateway_tool_progress(
        "verbose", "terminal", &seen
    ));
    assert!(!should_emit_gateway_tool_progress(
        "weird", "terminal", &seen
    ));
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[test]
fn tool_progress_parse_mode_only_markdown_on_telegram_code_blocks() {
    assert!(matches!(
        gateway_tool_progress_parse_mode("Telegram", "```\ncode\n```"),
        Some(ParseMode::Markdown)
    ));
    assert!(matches!(
        gateway_tool_progress_parse_mode("telegram", "intro\n```rust\nx\n```"),
        Some(ParseMode::Markdown)
    ));
    assert!(gateway_tool_progress_parse_mode("discord", "```\ncode\n```").is_none());
    assert!(gateway_tool_progress_parse_mode("telegram", "plain text").is_none());
}

#[test]
fn extract_last_assistant_reply_defaults_when_empty() {
    assert_eq!(extract_last_assistant_reply(&[]), "(no assistant reply)");
}

#[test]
fn truncate_hook_tool_result_trims_and_truncates() {
    assert_eq!(truncate_hook_tool_result("  short  "), "short");
    let long = "x".repeat(300);
    let out = truncate_hook_tool_result(&long);
    assert_eq!(out.chars().count(), 243);
    assert!(out.ends_with("..."));
    let exact = "y".repeat(240);
    assert_eq!(truncate_hook_tool_result(&exact), exact);
}

#[cfg(feature = "gateway-telegram")]
#[test]
fn telegram_allowed_users_include_wildcard_when_allow_all() {
    let mut p = PlatformConfig {
        enabled: true,
        allowed_users: vec!["alice".to_string()],
        ..Default::default()
    };
    p.extra
        .insert("allow_all_users".to_string(), serde_json::json!("yes"));
    let users = telegram_allowed_users_for_adapter(&p);
    assert!(users.contains(&"alice".to_string()));
    assert!(users.contains(&"*".to_string()));
}

#[cfg(feature = "gateway-telegram")]
#[test]
fn telegram_command_menu_string_vec_handles_string_and_other() {
    let mut p = enabled_platform();
    p.extra.insert(
        "command_menu".to_string(),
        serde_json::json!({"priority": "help, ,status ,  new"}),
    );
    assert_eq!(
        telegram_command_menu_string_vec(&p, "priority"),
        vec!["help".to_string(), "status".to_string(), "new".to_string()]
    );

    let mut q = enabled_platform();
    q.extra
        .insert("command_menu_priority".to_string(), serde_json::json!(42));
    assert!(telegram_command_menu_string_vec(&q, "priority").is_empty());
}

#[test]
fn extra_string_set_handles_numbers_and_ignored_items() {
    let mut p = enabled_platform();
    p.extra.insert(
        "list".to_string(),
        serde_json::json!(["a", 7, true, null, "  "]),
    );
    p.extra.insert("num".to_string(), serde_json::json!(12345));
    let list = extra_string_set(&p, "list");
    assert_eq!(list.len(), 2);
    assert!(list.contains("a"));
    assert!(list.contains("7"));
    let num = extra_string_set(&p, "num");
    assert_eq!(num.len(), 1);
    assert!(num.contains("12345"));
}

#[cfg(feature = "gateway-telegram")]
#[test]
fn extra_string_vec_handles_number_and_other() {
    let mut p = enabled_platform();
    p.extra.insert("num".to_string(), serde_json::json!(99));
    p.extra.insert("flag".to_string(), serde_json::json!(true));
    assert_eq!(extra_string_vec(&p, "num"), vec!["99".to_string()]);
    assert!(extra_string_vec(&p, "flag").is_empty());
}

#[cfg(feature = "gateway-telegram")]
#[test]
fn reply_to_mode_string_handles_bools_and_invalid() {
    let mut p = enabled_platform();
    p.extra
        .insert("reply_to_mode".to_string(), serde_json::json!(false));
    assert_eq!(reply_to_mode_string(&p), Some("off".to_string()));

    let mut q = enabled_platform();
    q.extra
        .insert("reply_to_mode".to_string(), serde_json::json!(true));
    assert_eq!(reply_to_mode_string(&q), Some("all".to_string()));

    let mut r = enabled_platform();
    r.extra
        .insert("reply_to_mode".to_string(), serde_json::json!(3));
    assert_eq!(reply_to_mode_string(&r), None);
}

#[test]
fn extra_bool_loose_parses_strings() {
    let mut p = enabled_platform();
    p.extra.insert("a".to_string(), serde_json::json!(" Yes "));
    p.extra
        .insert("b".to_string(), serde_json::json!("disabled"));
    p.extra.insert("c".to_string(), serde_json::json!("maybe"));
    p.extra.insert("d".to_string(), serde_json::json!(1));
    assert_eq!(extra_bool_loose(&p, "a"), Some(true));
    assert_eq!(extra_bool_loose(&p, "b"), Some(false));
    assert_eq!(extra_bool_loose(&p, "c"), None);
    assert_eq!(extra_bool_loose(&p, "d"), None);
    assert_eq!(extra_bool_loose(&p, "missing"), None);
}

#[cfg(feature = "gateway-email")]
#[test]
fn env_email_identity_vec_missing_var_is_empty() {
    assert!(env_email_identity_vec("HERMES_COVGEN_EMAIL_IDENTITY_NOT_SET_7F3A").is_empty());
}

#[test]
fn dm_policy_unknown_is_none() {
    assert!(dm_policy_unauthorized_behavior("something-else").is_none());
    assert!(matches!(
        dm_policy_unauthorized_behavior("pairing"),
        Some(UnauthorizedDmBehavior::Pair)
    ));
}

#[test]
fn explicit_unauthorized_dm_behavior_from_extra() {
    let mut lookup = |_: &str| -> Option<String> { None };

    let mut pair = enabled_platform();
    pair.extra.insert(
        "unauthorized_dm_behavior".to_string(),
        serde_json::json!("Pair"),
    );
    assert!(matches!(
        explicit_platform_unauthorized_dm_behavior("telegram", &pair, &mut lookup),
        Some(UnauthorizedDmBehavior::Pair)
    ));

    let mut ignore = enabled_platform();
    ignore.extra.insert(
        "unauthorized_dm_behavior".to_string(),
        serde_json::json!("drop"),
    );
    assert!(matches!(
        explicit_platform_unauthorized_dm_behavior("telegram", &ignore, &mut lookup),
        Some(UnauthorizedDmBehavior::Ignore)
    ));

    let mut bogus = enabled_platform();
    bogus.extra.insert(
        "unauthorized_dm_behavior".to_string(),
        serde_json::json!("whatever"),
    );
    assert!(explicit_platform_unauthorized_dm_behavior("telegram", &bogus, &mut lookup).is_none());

    let mut policy = enabled_platform();
    policy
        .extra
        .insert("dm_policy".to_string(), serde_json::json!("pairing"));
    assert!(matches!(
        explicit_platform_unauthorized_dm_behavior("telegram", &policy, &mut lookup),
        Some(UnauthorizedDmBehavior::Pair)
    ));
}

#[test]
fn split_group_values_skips_empty_and_routes_chats() {
    let (users, chats) = split_group_authorization_values(
        "telegram",
        vec![
            "  ".to_string(),
            "123".to_string(),
            "-100200".to_string(),
            String::new(),
        ],
    );
    assert_eq!(users.len(), 1);
    assert!(users.contains("123"));
    assert_eq!(chats.len(), 1);
    assert!(chats.contains("-100200"));

    let (users, chats) = split_group_authorization_values("qq", vec!["555".to_string()]);
    assert!(users.is_empty());
    assert!(chats.contains("555"));
}

#[test]
fn startup_warning_suppressed_by_config_allow_all() {
    let mut config = hermes_config::GatewayConfig::default();
    let mut p = enabled_platform();
    p.extra
        .insert("allow_all_users".to_string(), serde_json::json!(true));
    config.platforms.insert("discord".to_string(), p);
    assert!(!gateway_allowlist_startup_would_warn(&config));
}

#[test]
fn parse_group_access_mode_explicit_policies() {
    let mut disabled = enabled_platform();
    disabled
        .extra
        .insert("group_policy".to_string(), serde_json::json!("Deny"));
    assert!(matches!(
        parse_group_access_mode(&disabled, true),
        GroupAccessMode::Disabled
    ));

    let mut allow = enabled_platform();
    allow
        .extra
        .insert("group_access".to_string(), serde_json::json!("whitelist"));
    assert!(matches!(
        parse_group_access_mode(&allow, false),
        GroupAccessMode::Allowlist
    ));

    let mut open = enabled_platform();
    open.extra
        .insert("group_policy".to_string(), serde_json::json!("open"));
    assert!(matches!(
        parse_group_access_mode(&open, true),
        GroupAccessMode::Open
    ));

    let mut unknown = enabled_platform();
    unknown
        .extra
        .insert("group_policy".to_string(), serde_json::json!("strange"));
    assert!(matches!(
        parse_group_access_mode(&unknown, false),
        GroupAccessMode::Open
    ));
    assert!(matches!(
        parse_group_access_mode(&unknown, true),
        GroupAccessMode::Allowlist
    ));
}

#[test]
fn access_policies_collect_users_admins_and_wildcards() {
    let mut config = hermes_config::GatewayConfig::default();
    let mut discord = PlatformConfig {
        enabled: true,
        allowed_users: vec![" alice ".to_string(), "   ".to_string()],
        admin_users: vec!["bob".to_string(), String::new()],
        ..Default::default()
    };
    discord
        .extra
        .insert("allow_all_users".to_string(), serde_json::json!("on"));
    config.platforms.insert("discord".to_string(), discord);
    config
        .platforms
        .insert("slack".to_string(), enabled_platform());

    let policies = build_gateway_platform_access_policies_with_lookup(&config, |key| {
        if key == "SLACK_ALLOW_ALL_USERS" {
            Some("1".to_string())
        } else {
            None
        }
    });

    let d = policies.get("discord").expect("discord policy");
    assert!(d.allowed_users.contains("alice"));
    assert!(d.allowed_users.contains("*"));
    assert!(!d.allowed_users.contains(""));
    assert!(d.admin_users.contains("bob"));
    assert_eq!(d.admin_users.len(), 1);

    let s = policies.get("slack").expect("slack policy");
    assert!(s.allowed_users.contains("*"));
    assert!(s.admin_users.is_empty());
}

#[test]
fn requirement_issues_report_missing_credentials() {
    let mut config = hermes_config::GatewayConfig::default();
    for name in ["weixin", "discord", "slack", "ntfy", "wecom_callback"] {
        config
            .platforms
            .insert(name.to_string(), enabled_platform());
    }
    let issues = gateway_requirement_issues(&config);
    assert!(issues.iter().any(|i| i.starts_with("weixin.enabled=true")));
    assert!(issues.iter().any(|i| i.starts_with("discord.enabled=true")));
    assert!(issues.iter().any(|i| i.starts_with("slack.enabled=true")));
    assert!(issues
        .iter()
        .any(|i| i.starts_with("wecom_callback.enabled=true")));
    let ntfy_reported = issues
        .iter()
        .any(|i| i == "ntfy.enabled=true but topic is missing");
    assert_eq!(ntfy_reported, std::env::var("NTFY_TOPIC").is_err());

    let mut ok = hermes_config::GatewayConfig::default();
    let mut slack = PlatformConfig {
        enabled: true,
        token: Some("xoxb-test".to_string()), // nosemgrep: codacy.yaml.security.hard-coded-tokens
        ..Default::default()
    };
    slack
        .extra
        .insert("unused".to_string(), serde_json::json!(1));
    ok.platforms.insert("slack".to_string(), slack);
    assert!(gateway_requirement_issues(&ok).is_empty());
}
