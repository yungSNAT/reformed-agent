use super::CopilotProvider;
use hermes_core::{LlmProvider, Message};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn provider(server: &MockServer) -> CopilotProvider {
    CopilotProvider::new(server.uri(), "synthetic-test-value").with_model("default-model")
}

async fn mock_chat(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "hello"}}]
        })))
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

#[tokio::test]
async fn copilot_clamps_reasoning_to_the_model_override_capabilities() {
    let server = MockServer::start().await;
    mock_chat(&server).await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "default-model", "capabilities": {"supports": {
                    "reasoning_effort": ["medium", "xhigh"]
                }}},
                {"id": "selected-model", "capabilities": {"supports": {
                    "reasoning_effort": ["medium", "high"]
                }}}
            ]
        })))
        .mount(&server)
        .await;

    one_shot(
        &provider(&server),
        Some("selected-model"),
        &json!({"reasoning_effort": "xhigh"}),
    )
    .await;

    let requests = server.received_requests().await.expect("requests");
    let chat = requests
        .iter()
        .find(|request| request.url.path() == "/chat/completions")
        .expect("chat request");
    let body: Value = chat.body_json().expect("chat body");
    assert_eq!(body["model"], "selected-model");
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(requests[0].url.path(), "/models");
    assert_eq!(requests.len(), 2);
}
