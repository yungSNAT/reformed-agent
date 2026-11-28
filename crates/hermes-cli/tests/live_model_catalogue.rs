#[path = "support/isolated_hermes.rs"]
mod support;

use std::time::Duration;

use hermes_cli::model_switch::curated_provider_slugs;
use serde_json::{json, Map};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use support::isolated_hermes;

#[tokio::test]
async fn model_completions_use_configured_loopback_catalogues() {
    const FIXTURE_KEY: &str = "catalogue-fixture";
    const PREFIX: &str = "fixture-catalogue-";
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    std::fs::create_dir(&home).unwrap();
    let server = MockServer::start().await;
    let live = ResponseTemplate::new(200).set_body_json(json!({
        "data": [
            {"id": "zulu-v1"},
            {"id": " MixCase "},
            {"id": "mixcase"},
            {"id": 42},
            {"id": "  "},
            {},
            {"id": "local-summary"}
        ]
    }));
    let mut providers: Map<String, serde_json::Value> = curated_provider_slugs()
        .into_iter()
        .map(|provider| {
            (
                provider.to_string(),
                json!({"discover_models": false, "models": ["fixture-default"]}),
            )
        })
        .collect();
    let fallback = &[" Local-Fallback ", "local-fallback", "summary"][..];
    let fallback_models = &["Local-Fallback", "summary"][..];
    let live_models = &["zulu-v1", "MixCase", "local-summary"][..];
    let key = Some(FIXTURE_KEY);
    let mut expected = Vec::new();
    for (case, key, configured, discover, response, models) in [
        ("disabled", key, fallback, false, None, fallback_models),
        (
            "empty",
            key,
            fallback,
            true,
            Some(ResponseTemplate::new(200).set_body_json(json!({"data": []}))),
            fallback_models,
        ),
        ("invalid-url", key, fallback, true, None, fallback_models),
        (
            "keyed",
            key,
            fallback,
            true,
            Some(live.clone()),
            live_models,
        ),
        ("keyless", None, &[], true, Some(live), live_models),
        (
            "malformed",
            key,
            fallback,
            true,
            Some(ResponseTemplate::new(200).set_body_string("invalid JSON")),
            fallback_models,
        ),
        (
            "unavailable",
            key,
            fallback,
            true,
            Some(ResponseTemplate::new(503)),
            fallback_models,
        ),
    ] {
        let provider = format!("{PREFIX}{case}");
        expected.extend(models.iter().map(|model| format!("{provider}:{model}")));
        let base_url = if case == "invalid-url" {
            "http://[".to_string()
        } else {
            format!("{}/{case}/", server.uri())
        };
        providers.insert(
            provider,
            json!({
                "base_url": base_url,
                "api_key": key,
                "models": configured,
                "discover_models": discover,
            }),
        );
        if let Some(response) = response {
            let mut request = Mock::given(method("GET"))
                .and(path(format!("/{case}/models")))
                .and(query_param("output_modalities", "all"));
            if let Some(key) = key {
                request = request.and(header("authorization", format!("Bearer {key}")));
            }
            request
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
        }
    }
    std::fs::write(
        home.join("config.yaml"),
        json!({"llm_providers": providers}).to_string(),
    )
    .unwrap();

    let mut hermes = isolated_hermes(sandbox.path());
    hermes
        .args(["model", "--completion-values"])
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "127.0.0.1");
    let output = tokio::time::timeout(Duration::from_secs(30), hermes.output())
        .await
        .expect("loopback catalogue checks timed out")
        .expect("run model completions");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let completions: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with(PREFIX))
        .collect();
    assert_eq!(completions, expected);

    server.verify().await;
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 5);
    let keyless = received
        .iter()
        .find(|request| request.url.path() == "/keyless/models")
        .unwrap();
    assert!(!keyless.headers.contains_key("authorization"));
}
