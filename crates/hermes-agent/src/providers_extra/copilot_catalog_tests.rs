use super::{CopilotCatalog, CopilotProvider};
use futures::StreamExt;
use hermes_core::{LlmProvider, Message};
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider(server: &MockServer) -> CopilotProvider {
    let mut provider =
        CopilotProvider::new(server.uri(), "synthetic-test-value").with_model("default-model");
    provider.catalog = CopilotCatalog::new(format!("{}/models", server.uri()).parse().unwrap());
    assert!(provider.catalog.is_some());
    provider
}

async fn mock_chat(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "hello"}}]
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "hello"}]}],
            "status": "completed"
        })))
        .mount(server)
        .await;
}

/// The effort a request asks for, on either route.
fn effort(body: &Value) -> Option<&str> {
    body.get("reasoning_effort")
        .or_else(|| body.pointer("/reasoning/effort"))
        .and_then(Value::as_str)
}

async fn mock_models(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(response)
        .mount(server)
        .await;
}

async fn one_shot(provider: &CopilotProvider, model: Option<&str>, extra_body: &Value) {
    provider
        .chat_completion(
            &[Message::user("hello")],
            &[],
            None,
            None,
            model,
            Some(extra_body),
        )
        .await
        .expect("successful chat response");
}

async fn chat_requests(server: &MockServer, catalog_count: usize) -> Vec<Value> {
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.url.path() == "/models")
            .count(),
        catalog_count,
    );
    requests
        .iter()
        .filter(|request| matches!(request.url.path(), "/chat/completions" | "/responses"))
        .map(|request| request.body_json().expect("chat body"))
        .collect()
}

fn model(id: &str, capabilities: Value) -> Value {
    json!({"id": id, "capabilities": capabilities})
}

#[tokio::test]
async fn copilot_clamps_reasoning_to_the_model_override_capabilities() {
    let server = MockServer::start().await;
    mock_chat(&server).await;
    mock_models(&server, ResponseTemplate::new(200).set_body_json(json!({
        "data": [
            model("default-model", json!({"supports": {"reasoning_effort": ["medium", "xhigh"]}})),
            model("selected-model", json!({"supports": {"reasoning_effort": ["medium", "high"]}}))
        ]
    })))
    .await;

    let provider = provider(&server);
    let effort = json!({"reasoning_effort": "xhigh", "temperature": 0.2});
    one_shot(&provider, Some(" selected-model "), &effort).await;
    one_shot(
        &provider,
        Some("selected-model"),
        &json!({"reasoning_effort": "xhigh", "model": "default-model"}),
    )
    .await;

    let chats = chat_requests(&server, 1).await;
    assert_eq!(chats.len(), 2);
    assert_eq!(chats[0]["model"], "selected-model");
    assert_eq!(chats[0]["reasoning_effort"], "high");
    assert_eq!(chats[0]["temperature"], 0.2);
    assert_eq!(chats[1]["model"], "default-model");
    assert_eq!(chats[1]["reasoning_effort"], "xhigh");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].url.path(), "/models");
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer synthetic-test-value"
    );
    assert_eq!(requests[0].headers["x-github-api-version"], "2026-08-01");
}

#[tokio::test]
async fn copilot_uses_only_usable_catalog_capabilities() {
    let server = MockServer::start().await;
    mock_chat(&server).await;
    let models = vec![
        model("absent-effort", json!({"supports": {}})),
        model(
            "empty-effort",
            json!({"supports": {"reasoning_effort": []}}),
        ),
        model(
            "bad-format",
            json!({"supports": {"reasoning_effort": ["high", false]}}),
        ),
        model(
            "duplicate",
            json!({"supports": {"reasoning_effort": ["high"]}}),
        ),
        model("duplicate", json!({"supports": {"reasoning_effort": []}})),
        model("gpt-5-legacy", json!(["reasoning"])),
        model("gpt-5-no-tag", json!(["tool_calls"])),
        model(
            "ordered-effort",
            json!({"supports": {"reasoning_effort": ["HIGH", "low", "high"]}}),
        ),
    ];
    mock_models(&server, ResponseTemplate::new(200).set_body_json(models)).await;
    let provider = provider(&server);
    let cases = [
        ("absent-effort", "xhigh", None),
        ("empty-effort", "xhigh", None),
        ("bad-format", "xhigh", Some("xhigh")),
        ("missing-row", "xhigh", Some("xhigh")),
        ("duplicate", "xhigh", Some("xhigh")),
        ("gpt-5-legacy", "xhigh", Some("high")),
        ("gpt-5-no-tag", "xhigh", None),
        ("ordered-effort", "other", Some("high")),
    ];
    for (model, effort, _) in cases {
        one_shot(&provider, Some(model), &json!({"reasoning_effort": effort})).await;
    }
    let chats = chat_requests(&server, 1).await;
    assert_eq!(chats.len(), cases.len());
    for (chat, (model, _, expected)) in chats.iter().zip(cases) {
        assert_eq!(chat["model"], model);
        assert_eq!(effort(chat), expected, "{model}");
    }
}

#[tokio::test]
async fn copilot_sends_gpt_5_and_later_to_the_responses_api() {
    let server = MockServer::start().await;
    mock_chat(&server).await;
    mock_models(
        &server,
        ResponseTemplate::new(200).set_body_json(vec![model(
            "gpt-5.1",
            json!({"supports": {"reasoning_effort": ["low", "medium", "high"]}}),
        )]),
    )
    .await;
    let provider = provider(&server);
    let models = [
        "gpt-5.1",
        "gpt-4o",
        "gpt-5-mini",
        "claude-sonnet-4.5",
        "openai/GPT-6",
        "gpt-50",
    ];
    for model in models {
        one_shot(
            &provider,
            Some(model),
            &json!({"reasoning_effort": "xhigh"}),
        )
        .await;
    }

    let requests = server.received_requests().await.unwrap();
    let routes: Vec<&str> = requests
        .iter()
        .map(|request| request.url.path())
        .filter(|path| *path != "/models")
        .collect();
    assert_eq!(
        routes,
        [
            "/responses",
            "/chat/completions",
            "/chat/completions",
            "/chat/completions",
            "/responses",
            "/responses",
        ]
    );
    let first: Value = requests
        .iter()
        .find(|request| request.url.path() == "/responses")
        .unwrap()
        .body_json()
        .unwrap();
    assert_eq!(first["model"], "gpt-5.1");
    assert_eq!(
        first["reasoning"],
        json!({"effort": "high", "summary": "auto"})
    );
    assert!(first.get("reasoning_effort").is_none(), "{first}");
    assert_eq!(first["input"][0]["content"][0]["text"], "hello");
}

#[tokio::test]
async fn copilot_responses_route_honours_disabled_reasoning_and_streams() {
    let server = MockServer::start().await;
    mock_chat(&server).await;
    let provider = CopilotProvider::new(server.uri(), "synthetic-test-value").with_model("gpt-5.2");
    one_shot(
        &provider,
        None,
        &json!({"reasoning_effort": "high", "reasoning": {"enabled": false}}),
    )
    .await;
    let stream =
        provider.chat_completion_stream(&[Message::user("hello")], &[], None, None, None, None);
    let _ = stream.collect::<Vec<_>>().await;

    let bodies = chat_requests(&server, 0).await;
    let paths: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.url.path().to_string())
        .collect();
    assert_eq!(paths, ["/responses", "/responses"]);
    assert!(bodies[0].get("reasoning").is_none(), "{}", bodies[0]);
    assert!(bodies[0].get("reasoning_effort").is_none(), "{}", bodies[0]);
    assert_eq!(bodies[1]["stream"], true);
}

#[tokio::test]
async fn copilot_stream_and_plain_chat_share_catalog_cache() {
    let server = MockServer::start().await;
    mock_chat(&server).await;
    mock_models(
        &server,
        ResponseTemplate::new(200).set_body_json(vec![model(
            "default-model",
            json!({"supports": {"reasoning_effort": ["low", "medium", "high"]}}),
        )]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({"stream": true})))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: [DONE]\n\n",
            "text/event-stream",
        ))
        .with_priority(1)
        .mount(&server)
        .await;
    let provider = provider(&server);
    let effort = json!({"reasoning_effort": "xhigh"});
    let stream = provider.chat_completion_stream(
        &[Message::user("hello")],
        &[],
        None,
        None,
        None,
        Some(&effort),
    );
    let chunks = stream.collect::<Vec<_>>().await;
    assert!(!chunks.is_empty());
    assert!(chunks.iter().all(Result::is_ok));

    one_shot(&provider, None, &effort).await;
    let requests = chat_requests(&server, 1).await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["stream"], true);
    assert_eq!(requests[0]["reasoning_effort"], "high");
    assert_eq!(requests[1]["reasoning_effort"], "high");
}

#[tokio::test]
async fn copilot_preserves_inference_after_a_refused_catalog_redirect() {
    let server = MockServer::start().await;
    let destination = MockServer::start().await;
    mock_chat(&server).await;
    mock_models(
        &server,
        ResponseTemplate::new(302).insert_header("Location", destination.uri()),
    )
    .await;
    let provider = provider(&server);
    let effort = json!({"reasoning_effort": "xhigh"});
    one_shot(&provider, None, &effort).await;
    one_shot(&provider, None, &effort).await;
    assert!(destination.received_requests().await.unwrap().is_empty());
    let chats = chat_requests(&server, 1).await;
    assert_eq!(chats.len(), 2);
    assert!(chats.iter().all(|chat| chat["reasoning_effort"] == "xhigh"));
}

#[tokio::test]
async fn copilot_skips_catalog_for_a_custom_base_and_disabled_reasoning() {
    let server = MockServer::start().await;
    mock_chat(&server).await;
    let custom = CopilotProvider::new(server.uri(), "synthetic-test-value").with_model_catalog();
    assert!(custom.catalog.is_none());
    one_shot(&custom, None, &json!({"reasoning_effort": "xhigh"})).await;
    let provider = provider(&server);
    one_shot(&provider, None, &json!({"reasoning_effort": "none"})).await;
    one_shot(
        &provider,
        None,
        &json!({"reasoning_effort": "high", "reasoning": {"enabled": false}}),
    )
    .await;
    assert_eq!(chat_requests(&server, 0).await.len(), 3);
}

#[tokio::test]
async fn copilot_leaves_dynamic_alias_resolution_to_the_chat_adapter() {
    let server = MockServer::start().await;
    mock_chat(&server).await;
    let provider = provider(&server);
    one_shot(
        &provider,
        None,
        &json!({"model": "openai:dynamic", "reasoning_effort": "xhigh"}),
    )
    .await;
    let chats = chat_requests(&server, 0).await;
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0]["model"], "default-model");
    assert_eq!(chats[0]["reasoning_effort"], "xhigh");
}

#[tokio::test]
async fn copilot_preserves_effort_when_catalog_is_oversized() {
    let server = MockServer::start().await;
    mock_chat(&server).await;
    mock_models(
        &server,
        ResponseTemplate::new(200).set_body_raw(vec![b' '; 1024 * 1024 + 1], "application/json"),
    )
    .await;
    one_shot(
        &provider(&server),
        None,
        &json!({"reasoning_effort": "xhigh"}),
    )
    .await;
    let chats = chat_requests(&server, 1).await;
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0]["reasoning_effort"], "xhigh");
}

#[test]
fn copilot_catalog_requires_a_recognised_https_origin() {
    for base in [
        "http://api.githubcopilot.com",
        "https://api.githubcopilot.com.example.test",
        "https://api.githubcopilot.com@elsewhere.test",
        "https://operator@api.githubcopilot.com",
        "https://api.githubcopilot.com:444",
        "https://api.githubcopilot.com/custom",
        "https://api.githubcopilot.com?profile=test",
        "https://api.githubcopilot.com#ignored",
    ] {
        assert!(CopilotCatalog::for_base_url(base).is_none(), "{base}");
    }
    for base in [
        "https://api.githubcopilot.com",
        "https://api.enterprise.githubcopilot.com/v1/",
    ] {
        assert!(CopilotCatalog::for_base_url(base).is_some(), "{base}");
    }
}
