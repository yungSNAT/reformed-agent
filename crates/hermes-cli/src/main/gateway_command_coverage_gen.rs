use super::*;

#[test]
fn parse_csv_list_trims_and_drops_empty_entries() {
    assert_eq!(
        parse_csv_list(" alice, bob ,,carol , "),
        vec!["alice".to_string(), "bob".to_string(), "carol".to_string()]
    );
    assert!(parse_csv_list("").is_empty());
    assert!(parse_csv_list(" , ,, ").is_empty());
    assert_eq!(parse_csv_list("single"), vec!["single".to_string()]);
}

#[test]
fn enabled_flag_reports_platform_state() {
    assert_eq!(enabled_flag(None), "disabled");

    let disabled = PlatformConfig {
        enabled: false,
        ..Default::default()
    };
    assert_eq!(enabled_flag(Some(&disabled)), "disabled");

    let enabled = PlatformConfig {
        enabled: true,
        ..Default::default()
    };
    assert_eq!(enabled_flag(Some(&enabled)), "enabled");
}

#[test]
fn set_extra_string_if_nonempty_inserts_trimmed_values_only() {
    let mut platform = PlatformConfig::default();
    platform.extra.clear();

    set_extra_string_if_nonempty(&mut platform, "application_id", "  12345  ");
    assert_eq!(
        platform.extra.get("application_id"),
        Some(&serde_json::Value::String("12345".to_string()))
    );

    set_extra_string_if_nonempty(&mut platform, "blank", "   ");
    assert!(!platform.extra.contains_key("blank"));

    set_extra_string_if_nonempty(&mut platform, "empty", "");
    assert!(!platform.extra.contains_key("empty"));

    // Overwrites an existing value when a new non-empty value is supplied.
    set_extra_string_if_nonempty(&mut platform, "application_id", "999");
    assert_eq!(
        platform.extra.get("application_id"),
        Some(&serde_json::Value::String("999".to_string()))
    );
    assert_eq!(platform.extra.len(), 1);
}

#[tokio::test]
async fn configure_platform_basic_prompts_unknown_key_only_enables_platform() {
    let mut disk = hermes_config::GatewayConfig::default();
    disk.platforms.remove("custom_platform_xyz");

    configure_platform_basic_prompts(&mut disk, "custom_platform_xyz")
        .await
        .expect("unknown platform key needs no prompts");

    let platform = disk
        .platforms
        .get("custom_platform_xyz")
        .expect("platform entry created");
    assert!(platform.enabled);
    assert!(platform.token.is_none());
    assert!(platform.extra.is_empty());
}

#[tokio::test]
async fn configure_platform_basic_prompts_unknown_key_preserves_existing_fields() {
    let mut disk = hermes_config::GatewayConfig::default();
    let mut existing = PlatformConfig {
        enabled: false,
        token: Some("keep-me".to_string()),
        ..Default::default()
    };
    existing
        .extra
        .insert("note".to_string(), serde_json::json!("kept"));
    disk.platforms
        .insert("another_custom_platform".to_string(), existing);

    configure_platform_basic_prompts(&mut disk, "another_custom_platform")
        .await
        .expect("unknown platform key needs no prompts");

    let platform = disk
        .platforms
        .get("another_custom_platform")
        .expect("platform entry kept");
    assert!(platform.enabled);
    assert_eq!(platform.token.as_deref(), Some("keep-me"));
    assert_eq!(platform.extra.get("note"), Some(&serde_json::json!("kept")));
}

#[tokio::test]
async fn run_gateway_unknown_action_returns_ok_without_touching_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cli = <Cli as clap::Parser>::try_parse_from(["hermes-agent-ultra"])
        .expect("cli parses without arguments");
    let path_str = dir.path().to_str().expect("utf-8 temp path");
    cli.config_dir = Some(path_str.parse().expect("config dir parses"));

    let result = run_gateway(
        cli,
        Some("definitely-not-an-action".to_string()),
        None,
        false,
        false,
        false,
        None,
        false,
        false,
        false,
        false,
    )
    .await;
    assert!(
        result.is_ok(),
        "unknown action should not error: {result:?}"
    );

    // No pid file is created for an unknown action.
    let after: std::collections::HashSet<String> = std::fs::read_dir(dir.path())
        .expect("read tempdir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!after.iter().any(|k| k.ends_with(".pid")));
}
