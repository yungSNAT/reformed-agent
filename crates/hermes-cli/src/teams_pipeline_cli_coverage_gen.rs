use std::collections::HashMap;

use tempfile::TempDir;

use super::*;

fn store_options(dir: &TempDir) -> TeamsPipelineCliOptions {
    TeamsPipelineCliOptions {
        store_path: Some(
            dir.path()
                .join("teams_pipeline_store.json")
                .to_string_lossy()
                .into_owned(),
        ),
        ..TeamsPipelineCliOptions::default()
    }
}

fn contains_str(values: &Value, needle: &str) -> bool {
    values
        .as_array()
        .map(|items| items.iter().any(|v| v.as_str() == Some(needle)))
        .unwrap_or(false)
}

fn is_config_err(result: &Result<(), AgentError>, needle: &str) -> bool {
    matches!(result, Err(AgentError::Config(m)) if m.contains(needle))
}

#[tokio::test]
async fn unknown_action_is_rejected() {
    let options = TeamsPipelineCliOptions {
        action: Some("bogus".into()),
        ..TeamsPipelineCliOptions::default()
    };
    let result = handle_cli_teams_pipeline(options).await;
    assert!(is_config_err(
        &result,
        "Unknown teams-pipeline action: bogus"
    ));
}

#[tokio::test]
async fn list_on_empty_store_succeeds() {
    let dir = TempDir::new().unwrap();
    let options = store_options(&dir);
    assert!(handle_cli_teams_pipeline(options).await.is_ok());

    let mut options = store_options(&dir);
    options.action = Some("ls".into());
    options.status = Some("completed".into());
    options.limit = 3;
    assert!(handle_cli_teams_pipeline(options).await.is_ok());

    let mut options = store_options(&dir);
    options.action = Some("list".into());
    options.status = Some("   ".into());
    assert!(handle_cli_teams_pipeline(options).await.is_ok());
}

#[tokio::test]
async fn show_requires_id_and_known_job() {
    let dir = TempDir::new().unwrap();
    let mut options = store_options(&dir);
    options.action = Some("show".into());
    let result = handle_cli_teams_pipeline(options.clone()).await;
    assert!(is_config_err(
        &result,
        "teams-pipeline show <job-id>: missing id"
    ));

    options.id = Some("missing-job".into());
    let result = handle_cli_teams_pipeline(options).await;
    assert!(is_config_err(
        &result,
        "Unknown Teams pipeline job: missing-job"
    ));
}

#[tokio::test]
async fn run_requires_id() {
    let options = TeamsPipelineCliOptions {
        action: Some("replay".into()),
        id: Some("   ".into()),
        ..TeamsPipelineCliOptions::default()
    };
    let result = handle_cli_teams_pipeline(options).await;
    assert!(is_config_err(
        &result,
        "teams-pipeline run <job-id>: missing id"
    ));
}

#[tokio::test]
async fn fetch_requires_meeting_reference() {
    let options = TeamsPipelineCliOptions {
        action: Some("fetch".into()),
        meeting_id: Some("  ".into()),
        join_web_url: None,
        ..TeamsPipelineCliOptions::default()
    };
    let result = handle_cli_teams_pipeline(options).await;
    assert!(is_config_err(
        &result,
        "meeting_id or join_web_url is required"
    ));
}

#[tokio::test]
async fn subscribe_requires_resource_and_notification_url() {
    let options = TeamsPipelineCliOptions {
        action: Some("subscribe".into()),
        ..TeamsPipelineCliOptions::default()
    };
    let result = handle_cli_teams_pipeline(options).await;
    assert!(is_config_err(&result, "--resource is required"));

    let options = TeamsPipelineCliOptions {
        action: Some("subscribe".into()),
        resource: Some("/communications/onlineMeetings".into()),
        notification_url: Some("  ".into()),
        ..TeamsPipelineCliOptions::default()
    };
    let result = handle_cli_teams_pipeline(options).await;
    assert!(is_config_err(&result, "--notification-url is required"));
}

#[tokio::test]
async fn renew_subscription_requires_id_and_expiration() {
    let options = TeamsPipelineCliOptions {
        action: Some("renew-subscription".into()),
        ..TeamsPipelineCliOptions::default()
    };
    let result = handle_cli_teams_pipeline(options).await;
    assert!(is_config_err(
        &result,
        "teams-pipeline renew-subscription <subscription-id>: missing id"
    ));

    let options = TeamsPipelineCliOptions {
        action: Some("renew-subscription".into()),
        id: Some("sub-1".into()),
        expiration: Some("   ".into()),
        ..TeamsPipelineCliOptions::default()
    };
    let result = handle_cli_teams_pipeline(options).await;
    assert!(is_config_err(&result, "--expiration is required"));
}

#[tokio::test]
async fn delete_subscription_requires_id() {
    let options = TeamsPipelineCliOptions {
        action: Some("delete-subscription".into()),
        ..TeamsPipelineCliOptions::default()
    };
    let result = handle_cli_teams_pipeline(options).await;
    assert!(is_config_err(
        &result,
        "teams-pipeline delete-subscription <subscription-id>: missing id"
    ));
}

#[tokio::test]
async fn validate_action_with_temp_config_dir() {
    let dir = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();
    let mut options = store_options(&dir);
    options.action = Some("validate".into());
    options.config_dir = Some(config_dir.path().to_string_lossy().into_owned());
    assert!(handle_cli_teams_pipeline(options).await.is_ok());
}

#[test]
fn required_id_trims_and_rejects_blank() {
    let options = TeamsPipelineCliOptions {
        id: Some("  abc  ".into()),
        ..TeamsPipelineCliOptions::default()
    };
    assert_eq!(required_id(&options, "usage").unwrap(), "abc");

    let options = TeamsPipelineCliOptions::default();
    let err = required_id(&options, "my usage").unwrap_err();
    assert!(matches!(err, AgentError::Config(ref m) if m == "my usage: missing id"));
}

#[test]
fn open_store_uses_given_path() {
    let dir = TempDir::new().unwrap();
    let store = open_store(&store_options(&dir)).unwrap();
    assert!(store.list_jobs().unwrap().is_empty());
    assert!(store.get_job("nope").unwrap().is_none());
}

#[test]
fn runtime_config_without_teams_is_empty_object() {
    let config = GatewayConfig::default();
    assert_eq!(build_pipeline_runtime_config(&config), json!({}));
}

#[test]
fn runtime_config_with_disabled_teams_keeps_pipeline_only() {
    let mut config = GatewayConfig::default();
    let mut teams = PlatformConfig {
        enabled: false,
        ..PlatformConfig::default()
    };
    teams.extra.insert("delivery_mode".into(), json!("graph"));
    teams.extra.insert(
        "meeting_pipeline".into(),
        json!({"transcript_min_chars": 5}),
    );
    config.platforms.insert("teams".into(), teams);

    let runtime = build_pipeline_runtime_config(&config);
    assert_eq!(runtime, json!({"transcript_min_chars": 5}));
}

#[test]
fn runtime_config_enabled_without_delivery_keys_has_no_delivery() {
    let mut config = GatewayConfig::default();
    let teams = PlatformConfig {
        enabled: true,
        ..PlatformConfig::default()
    };
    config.platforms.insert("teams".into(), teams);
    let runtime = build_pipeline_runtime_config(&config);
    assert!(runtime.get("teams_delivery").is_none());
}

#[test]
fn runtime_config_incoming_webhook_mode() {
    let mut config = GatewayConfig::default();
    let mut teams = PlatformConfig {
        enabled: true,
        ..PlatformConfig::default()
    };
    teams
        .extra
        .insert("delivery_mode".into(), json!("incoming_webhook"));
    teams.extra.insert(
        "incoming_webhook_url".into(),
        json!("https://example.invalid/hook"),
    );
    config.platforms.insert("teams".into(), teams);

    let runtime = build_pipeline_runtime_config(&config);
    assert_eq!(
        runtime["teams_delivery"],
        json!({
            "enabled": true,
            "mode": "incoming_webhook",
            "incoming_webhook_url": "https://example.invalid/hook"
        })
    );
}

#[test]
fn teams_delivery_configured_checks_modes() {
    let extra: HashMap<String, Value> = HashMap::new();

    let mut delivery = Map::new();
    delivery.insert("mode".into(), json!("incoming_webhook"));
    assert!(!teams_delivery_is_configured(&extra, &delivery));
    delivery.insert("incoming_webhook_url".into(), json!("https://x.invalid"));
    assert!(teams_delivery_is_configured(&extra, &delivery));

    let mut delivery = Map::new();
    delivery.insert("mode".into(), json!("GRAPH"));
    delivery.insert("team_id".into(), json!("t"));
    assert!(!teams_delivery_is_configured(&extra, &delivery));
    delivery.insert("chat_id".into(), json!("c"));
    assert!(teams_delivery_is_configured(&extra, &delivery));

    let mut extra_graph: HashMap<String, Value> = HashMap::new();
    extra_graph.insert("delivery_mode".into(), json!("graph"));
    extra_graph.insert("team_id".into(), json!("t"));
    extra_graph.insert("channel_id".into(), json!("ch"));
    assert!(teams_delivery_is_configured(&extra_graph, &Map::new()));

    let mut delivery = Map::new();
    delivery.insert("mode".into(), json!("carrier_pigeon"));
    assert!(!teams_delivery_is_configured(&extra, &delivery));
}

#[test]
fn extra_string_trims_and_filters() {
    let mut platform = PlatformConfig::default();
    platform.extra.insert("a".into(), json!("  value  "));
    platform.extra.insert("b".into(), json!("   "));
    platform.extra.insert("c".into(), json!(42));
    assert_eq!(extra_string(&platform, "a").as_deref(), Some("value"));
    assert!(extra_string(&platform, "b").is_none());
    assert!(extra_string(&platform, "c").is_none());
    assert!(extra_string(&platform, "missing").is_none());
}

#[test]
fn validate_snapshot_default_config() {
    let dir = TempDir::new().unwrap();
    let store = open_store(&store_options(&dir)).unwrap();
    let config = GatewayConfig::default();
    let snapshot = validate_configuration_snapshot(&config, &store).unwrap();
    assert!(contains_str(
        &snapshot["issues"],
        "MSGRAPH_WEBHOOK_ENABLED is not enabled."
    ));
    assert!(contains_str(
        &snapshot["warnings"],
        "Teams outbound delivery is disabled."
    ));
    assert_eq!(snapshot["webhook_enabled"], json!(false));
    assert_eq!(snapshot["teams_enabled"], json!(false));
    assert_eq!(snapshot["teams_delivery_mode"], Value::Null);
    assert_eq!(snapshot["ok"], json!(false));
    assert!(snapshot["store_path"].is_string());
    assert!(snapshot.get("store_stats").is_some());
    assert!(snapshot["graph_config"]["tenant_id"].is_boolean());
}

#[test]
fn validate_snapshot_teams_without_mode_warns() {
    let dir = TempDir::new().unwrap();
    let store = open_store(&store_options(&dir)).unwrap();
    let mut config = GatewayConfig::default();
    config.platforms.insert(
        "msgraph_webhook".into(),
        PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        },
    );
    config.platforms.insert(
        "teams".into(),
        PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        },
    );
    let snapshot = validate_configuration_snapshot(&config, &store).unwrap();
    assert_eq!(snapshot["webhook_enabled"], json!(true));
    assert_eq!(snapshot["teams_enabled"], json!(true));
    assert!(!contains_str(
        &snapshot["issues"],
        "MSGRAPH_WEBHOOK_ENABLED is not enabled."
    ));
    assert!(contains_str(
        &snapshot["warnings"],
        "TEAMS_DELIVERY_MODE is not set."
    ));
}

#[test]
fn validate_snapshot_incoming_webhook_requires_url() {
    let dir = TempDir::new().unwrap();
    let store = open_store(&store_options(&dir)).unwrap();
    let mut config = GatewayConfig::default();
    let mut teams = PlatformConfig {
        enabled: true,
        ..PlatformConfig::default()
    };
    teams
        .extra
        .insert("delivery_mode".into(), json!("incoming_webhook"));
    config.platforms.insert("teams".into(), teams.clone());
    let snapshot = validate_configuration_snapshot(&config, &store).unwrap();
    assert_eq!(snapshot["teams_delivery_mode"], json!("incoming_webhook"));
    assert!(contains_str(
        &snapshot["issues"],
        "TEAMS_INCOMING_WEBHOOK_URL is required for incoming_webhook mode."
    ));

    teams
        .extra
        .insert("incoming_webhook_url".into(), json!("https://x.invalid"));
    config.platforms.insert("teams".into(), teams);
    let snapshot = validate_configuration_snapshot(&config, &store).unwrap();
    assert!(!contains_str(
        &snapshot["issues"],
        "TEAMS_INCOMING_WEBHOOK_URL is required for incoming_webhook mode."
    ));
}

#[test]
fn validate_snapshot_graph_mode_requirements() {
    let dir = TempDir::new().unwrap();
    let store = open_store(&store_options(&dir)).unwrap();
    let mut config = GatewayConfig::default();
    let mut teams = PlatformConfig {
        enabled: true,
        ..PlatformConfig::default()
    };
    teams.extra.insert("delivery_mode".into(), json!("graph"));
    config.platforms.insert("teams".into(), teams.clone());
    let snapshot = validate_configuration_snapshot(&config, &store).unwrap();
    assert!(contains_str(
        &snapshot["issues"],
        "TEAMS_TEAM_ID is required for graph delivery mode."
    ));
    assert!(contains_str(
        &snapshot["issues"],
        "TEAMS_CHANNEL_ID is required for graph delivery mode."
    ));

    teams.token = Some("token-value".into());
    teams.home_channel = Some("home-channel".into());
    teams.extra.insert("team_id".into(), json!("team-1"));
    config.platforms.insert("teams".into(), teams);
    let snapshot = validate_configuration_snapshot(&config, &store).unwrap();
    assert_eq!(snapshot["teams_delivery_mode"], json!("graph"));
    assert!(!contains_str(
        &snapshot["issues"],
        "TEAMS_TEAM_ID is required for graph delivery mode."
    ));
    assert!(!contains_str(
        &snapshot["issues"],
        "TEAMS_CHANNEL_ID is required for graph delivery mode."
    ));
    assert!(!contains_str(
        &snapshot["issues"],
        "TEAMS_GRAPH_ACCESS_TOKEN or complete MSGRAPH_* app credentials is required for graph delivery mode."
    ));
}

#[test]
fn compact_job_without_summary_is_unchanged() {
    let input = json!({"job_id": "j1", "status": "done"});
    assert_eq!(compact_job(input.clone()), input);

    let input = json!({"summary_payload": {"summary": "s"}});
    assert_eq!(compact_job(input.clone()), input);
}

#[test]
fn print_json_succeeds() {
    assert!(print_json(json!({"a": 1, "b": [1, 2, 3]})).is_ok());
}

#[test]
fn json_agent_error_wraps_message() {
    let err = serde_json::from_str::<Value>("{").unwrap_err();
    let mapped = json_agent_error(err);
    assert!(matches!(mapped, AgentError::Config(ref m) if m.starts_with("json: ")));
}

#[test]
fn agent_error_maps_all_variants() {
    assert!(matches!(
        agent_error(TeamsPipelineError::Config("c".into())),
        AgentError::Config(ref m) if m == "c"
    ));
    assert!(matches!(
        agent_error(TeamsPipelineError::Invalid("i".into())),
        AgentError::Config(ref m) if m == "i"
    ));
    assert!(matches!(
        agent_error(TeamsPipelineError::Store("s".into())),
        AgentError::Config(ref m) if m == "s"
    ));
    assert!(matches!(
        agent_error(TeamsPipelineError::Json("j".into())),
        AgentError::Config(ref m) if m == "j"
    ));
    assert!(matches!(
        agent_error(TeamsPipelineError::Io("io".into())),
        AgentError::Io(_)
    ));
    assert!(matches!(
        agent_error(TeamsPipelineError::Retryable("r".into())),
        AgentError::ToolExecution(ref m) if m == "r"
    ));
    assert!(matches!(
        agent_error(TeamsPipelineError::ArtifactNotFound("a".into())),
        AgentError::ToolExecution(ref m) if m == "a"
    ));
    assert!(matches!(
        agent_error(TeamsPipelineError::Sink("k".into())),
        AgentError::ToolExecution(ref m) if m == "k"
    ));
    assert!(matches!(
        agent_error(TeamsPipelineError::Graph {
            status: 503,
            message: "down".into()
        }),
        AgentError::ToolExecution(ref m) if m == "Microsoft Graph HTTP 503: down"
    ));
}
