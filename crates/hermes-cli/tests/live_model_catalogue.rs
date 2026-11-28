use std::time::Duration;

use hermes_cli::model_switch::provider_model_ids_for_config;
use hermes_config::{GatewayConfig, LlmProviderConfig};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROVIDER: &str = "fixture-catalogue";
const FIXTURE_KEY: &str = "catalogue-fixture";
const CHILD: &str = "HERMES_TEST_LIVE_MODEL_CATALOGUE_CHILD";

#[tokio::test]
async fn configured_model_catalogue_uses_its_loopback_endpoint() {
    if std::env::var_os(CHILD).is_some() {
        configured_endpoint_returns_distinct_live_models_in_server_order().await;
        configured_models_are_used_when_discovery_fails().await;
        disabled_discovery_uses_configured_models_without_contacting_the_provider().await;
        return;
    }

    let sandbox = tempfile::tempdir().expect("catalogue fixture directory");
    let mut command = tokio::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .env_clear()
        .envs(
            ["LLVM_PROFILE_FILE", "SystemRoot"]
                .into_iter()
                .filter_map(|key| std::env::var_os(key).map(|value| (key, value))),
        )
        .env("HOME", sandbox.path())
        .env("USERPROFILE", sandbox.path())
        .env("HERMES_HOME", sandbox.path())
        .env("HERMES_MANAGED_DIR", sandbox.path())
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "127.0.0.1")
        .env(CHILD, "1")
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .expect("loopback catalogue checks timed out")
        .expect("run catalogue test in an isolated process");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn catalogue_config(base_url: String, key: Option<&str>, models: &[&str]) -> GatewayConfig {
    let mut config = GatewayConfig::default();
    config.llm_providers.insert(
        PROVIDER.to_string(),
        LlmProviderConfig {
            base_url: Some(base_url),
            api_key: key.map(str::to_string),
            models: models.iter().map(|model| model.to_string()).collect(),
            discover_models: true,
            ..Default::default()
        },
    );
    config
}

async fn configured_endpoint_returns_distinct_live_models_in_server_order() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(query_param("output_modalities", "all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "zulu-v1"},
                {"id": " MixCase "},
                {"id": "mixcase"},
                {"id": 42},
                {"id": "  "},
                {},
                {"id": "local-summary"}
            ]
        })))
        .expect(2)
        .mount(&server)
        .await;
    for (key, configured) in [
        (Some(FIXTURE_KEY), &["local-fallback"][..]),
        (None, &[][..]),
    ] {
        let config = catalogue_config(format!("{}/v1/", server.uri()), key, configured);
        assert_eq!(
            provider_model_ids_for_config(PROVIDER, &config).await,
            ["zulu-v1", "MixCase", "local-summary"]
        );
    }
    server.verify().await;
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 2);
    assert_eq!(
        received[0].headers["authorization"].to_str().unwrap(),
        format!("Bearer {FIXTURE_KEY}")
    );
    assert!(!received[1].headers.contains_key("authorization"));
}

async fn configured_models_are_used_when_discovery_fails() {
    let server = MockServer::start().await;
    let configured = [" Local-Fallback ", "local-fallback", "summary"];
    for (prefix, response) in [
        ("unavailable", ResponseTemplate::new(503)),
        (
            "malformed",
            ResponseTemplate::new(200).set_body_string("invalid JSON"),
        ),
        (
            "empty",
            ResponseTemplate::new(200).set_body_json(json!({"data": []})),
        ),
    ] {
        let endpoint = format!("/{prefix}/models");
        Mock::given(method("GET"))
            .and(path(endpoint.as_str()))
            .and(query_param("output_modalities", "all"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let config = catalogue_config(
            format!("{}/{prefix}", server.uri()),
            Some(FIXTURE_KEY),
            &configured,
        );
        assert_eq!(
            provider_model_ids_for_config(PROVIDER, &config).await,
            ["Local-Fallback", "summary"],
            "{prefix}"
        );
    }
    server.verify().await;

    let config = catalogue_config("invalid URL".to_string(), Some(FIXTURE_KEY), &configured);
    assert_eq!(
        provider_model_ids_for_config(PROVIDER, &config).await,
        ["Local-Fallback", "summary"]
    );
}

async fn disabled_discovery_uses_configured_models_without_contacting_the_provider() {
    let server = MockServer::start().await;
    let mut config = catalogue_config(server.uri(), Some(FIXTURE_KEY), &["summary"]);
    config
        .llm_providers
        .get_mut(PROVIDER)
        .unwrap()
        .discover_models = false;

    assert_eq!(
        provider_model_ids_for_config(PROVIDER, &config).await,
        ["summary"]
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
