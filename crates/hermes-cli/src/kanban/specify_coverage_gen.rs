// Unit tests for the Kanban specify module. Coverage targets: argument
// parsing, config validation, specifier client construction, reply size
// limits, worker error mapping and the empty-board path.

use super::*;

fn settings(base_url: &str, api_key: &str) -> AuxiliaryTaskConfig {
    AuxiliaryTaskConfig {
        model: "test-model".to_string(),
        base_url: base_url.to_string(),
        api_key: api_key.to_string(),
        ..AuxiliaryTaskConfig::default()
    }
}

fn client_config_error(settings: &AuxiliaryTaskConfig) -> String {
    match specifier_client(settings) {
        Err(AgentError::Config(message)) => message,
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("expected specifier_client to fail"),
    }
}

#[test]
fn selection_parse_handles_all_and_task_ids() {
    assert!(matches!(
        Selection::parse(&["--all"]),
        Some(Selection::All(None))
    ));
    assert!(matches!(
        Selection::parse(&["abc"]),
        Some(Selection::Task(ref id)) if id == "abc"
    ));
    assert!(matches!(
        Selection::parse(&[" task-7 "]),
        Some(Selection::Task(ref id)) if id == "task-7"
    ));
    assert!(Selection::parse(&["-x"]).is_none());
    assert!(Selection::parse(&["   "]).is_none());
    assert!(Selection::parse(&[]).is_none());
}

#[test]
fn specifier_client_rejects_urls_with_credentials_query_or_fragment() {
    for url in [
        "https://user@example.com/v1",
        "https://:secret@example.com/v1",
        "https://example.com/v1?a=1",
        "https://example.com/v1#frag",
        "http://example.com/v1",
    ] {
        let message = client_config_error(&settings(url, "sk-test"));
        assert!(
            message.contains("HTTPS endpoint"),
            "url {url} produced {message}"
        );
    }
}

#[test]
fn specifier_client_accepts_https_and_loopback_endpoints() {
    assert!(specifier_client(&settings("https://api.example.com/v1", "sk-test")).is_ok());
    assert!(specifier_client(&settings("http://127.0.0.1:9/v1", "sk-test")).is_ok());
    assert!(specifier_client(&settings("http://localhost:9/v1", "sk-test")).is_ok());
}

#[test]
fn specifier_client_without_configured_key_still_builds() {
    // An empty configured key falls back to the environment or a placeholder.
    assert!(specifier_client(&settings("https://api.example.com/v1", "")).is_ok());
}

#[test]
fn specifier_client_rejects_invalid_api_keys() {
    let long_key = "a".repeat(5000);
    let message = client_config_error(&settings("https://api.example.com/v1", &long_key));
    assert_eq!(message, "Invalid Kanban specifier API key.");

    let message = client_config_error(&settings("https://api.example.com/v1", "bad\u{7}key"));
    assert_eq!(message, "Invalid Kanban specifier API key.");
}

#[test]
fn parse_reply_rejects_oversized_text() {
    let mut text = String::from("{\"title\":\"");
    text.push_str(&"a".repeat(MAX_RESPONSE_BYTES + 10));
    text.push_str("\"}");
    assert!(parse_reply(&text).is_none());
}

#[tokio::test]
async fn store_worker_error_maps_join_error_to_io() {
    let handle = tokio::spawn(std::future::pending::<()>());
    handle.abort();
    let join_error = match handle.await {
        Err(error) => error,
        Ok(()) => panic!("aborted task should not complete"),
    };
    match store_worker_error(join_error) {
        AgentError::Io(message) => assert_eq!(message, "Kanban store worker failed"),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn specify_kanban_returns_usage_for_bad_arguments() {
    let config = GatewayConfig::default();
    match specify_kanban(&[], &config).await {
        Ok(text) => assert_eq!(text, USAGE),
        Err(error) => panic!("unexpected error: {error:?}"),
    }
    match specify_kanban(&["--bogus"], &config).await {
        Ok(text) => assert_eq!(text, USAGE),
        Err(error) => panic!("unexpected error: {error:?}"),
    }
}

#[tokio::test]
async fn specify_kanban_requires_specifier_config() {
    let config = GatewayConfig::default();
    match specify_kanban(&["--all"], &config).await {
        Err(AgentError::Config(message)) => {
            assert!(message.contains("auxiliary.triage_specifier"))
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn specify_kanban_rejects_insecure_endpoint() {
    let mut config = GatewayConfig::default();
    config.auxiliary.insert(
        TASK.to_string(),
        settings("http://example.com/v1", "sk-test"),
    );
    match specify_kanban(&["task-1"], &config).await {
        Err(AgentError::Config(message)) => assert!(message.contains("HTTPS endpoint")),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn specify_board_reports_no_matching_tasks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("kanban.json");
    let client = match specifier_client(&settings("https://api.example.com/v1", "sk-test")) {
        Ok(client) => client,
        Err(error) => panic!("client should build: {error:?}"),
    };
    let result = specify_board(
        path,
        Selection::Task("does-not-exist".to_string()),
        &client,
        None,
    )
    .await;
    match result {
        Ok(text) => assert_eq!(text, "No matching triage tasks on this board."),
        Err(error) => panic!("unexpected error: {error:?}"),
    }
}
