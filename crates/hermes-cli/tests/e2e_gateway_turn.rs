#![cfg(all(unix, feature = "gateway-api-server"))]

#[path = "support/isolated_hermes.rs"]
mod support;

use serde_json::{json, Value};
use std::fs;
use std::net::TcpListener;
use std::process::Stdio;
use std::time::Duration;
use tokio::time::{sleep, timeout};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const REPLY: &str = "1234567890123456789é follows the SSE boundary.";

fn provider_response(streaming: bool, call_tool: bool) -> ResponseTemplate {
    let (mut reply, finish_reason) = if call_tool {
        (
            json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "fixture-list",
                    "type": "function",
                    "function": {"name": "todo", "arguments": "{\"action\":\"list\"}"}
                }]
            }),
            "tool_calls",
        )
    } else {
        (json!({"role": "assistant", "content": REPLY}), "stop")
    };
    if !streaming {
        return ResponseTemplate::new(200).set_body_json(json!({
            "id": "fixture-chat",
            "object": "chat.completion",
            "created": 0,
            "model": "deterministic",
            "choices": [{
                "index": 0,
                "message": reply,
                "finish_reason": finish_reason
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
    }
    if call_tool {
        reply["tool_calls"][0]["index"] = json!(0);
    }
    let start = json!({
        "id": "fixture-stream",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "deterministic",
        "choices": [{"index": 0, "delta": reply}]
    });
    let stop = json!({
        "id": "fixture-stream",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "deterministic",
        "choices": [{"index": 0, "delta": {}, "finish_reason": finish_reason}]
    });
    ResponseTemplate::new(200).set_body_raw(
        format!("data: {start}\n\ndata: {stop}\n\ndata: [DONE]\n\n"),
        "text/event-stream",
    )
}

fn requests_todo_schema(request: &Request) -> bool {
    let Ok(body) = request.body_json::<Value>() else {
        return false;
    };
    body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .is_some_and(|last| last["role"] == "user")
        && body["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["function"]["name"] == "todo"))
}

fn returns_empty_todo_list(request: &Request) -> bool {
    let Ok(body) = request.body_json::<Value>() else {
        return false;
    };
    let Some(last) = body["messages"].as_array().and_then(|items| items.last()) else {
        return false;
    };
    last["role"] == "tool"
        && last["tool_call_id"] == "fixture-list"
        && last["content"]
            .as_str()
            .and_then(|content| serde_json::from_str::<Value>(content).ok())
            .is_some_and(|result| result == json!({"todos": [], "total": 0}))
}

#[tokio::test]
async fn live_gateway_routes_complete_and_streamed_turns_to_the_configured_provider() {
    exercise_gateway_turns(false).await;
}

#[tokio::test]
async fn live_gateway_returns_tool_results_to_the_complete_and_streamed_provider() {
    exercise_gateway_turns(true).await;
}

async fn exercise_gateway_turns(with_tool: bool) {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    for streaming in [false, true] {
        let provider = MockServer::start().await;
        if with_tool {
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .and(body_partial_json(json!({"model": "deterministic"})))
                .and(requests_todo_schema)
                .respond_with(provider_response(streaming, true))
                .expect(1)
                .mount(&provider)
                .await;
        }
        let reply_mock = Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({"model": "deterministic"})));
        let reply_mock = if with_tool {
            reply_mock.and(returns_empty_todo_list)
        } else {
            reply_mock
        };
        reply_mock
            .respond_with(provider_response(streaming, false))
            .expect(1)
            .mount(&provider)
            .await;

        let sandbox = tempfile::tempdir().unwrap();
        let home = sandbox.path().join("hermes");
        fs::create_dir(&home).unwrap();
        let log = home.join("gateway-test.log");
        let toolsets = if with_tool {
            "platform_toolsets:\n  api_server: [todo]\n"
        } else {
            ""
        };
        let mut attempt = 0;
        let (mut child, base, token) = loop {
            attempt += 1;
            let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let address = reservation.local_addr().unwrap();
            let base = format!("http://{address}");
            let token = format!("local-gateway-fixture-{}", address.port());
            fs::write(
                home.join("config.yaml"),
                format!(
                    r#"model: local-test:deterministic
tools: []
{toolsets}llm_providers:
  local-test:
    base_url: {}
    api_key: local-provider-fixture
    discover_models: false
    models:
      deterministic: {{}}
streaming:
  enabled: {streaming}
platforms:
  api_server:
    enabled: true
    token: {token}
    host: 127.0.0.1
    port: {}
    allowed_users: [fixture-client]
"#,
                    provider.uri(),
                    address.port(),
                ),
            )
            .unwrap();
            let output = fs::File::create(&log).unwrap();
            let mut command = support::isolated_hermes(sandbox.path());
            command
                .args(["gateway", "run"])
                .env("RUST_LOG", "error")
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("HTTPS_PROXY", "http://127.0.0.1:1")
                .env("NO_PROXY", "127.0.0.1,localhost")
                .stdin(Stdio::null())
                .stdout(output.try_clone().unwrap())
                .stderr(output);
            drop(reservation);
            let mut child = command.spawn().unwrap();

            let ready = timeout(Duration::from_secs(20), async {
                loop {
                    if child.try_wait().unwrap().is_some()
                        || fs::read_to_string(&log)
                            .unwrap_or_default()
                            .contains("API server failed to bind:")
                    {
                        return false;
                    }
                    if client
                        .get(format!("{base}/health"))
                        .bearer_auth(&token)
                        .timeout(Duration::from_millis(250))
                        .send()
                        .await
                        .is_ok_and(|response| response.status().is_success())
                    {
                        return true;
                    }
                    sleep(Duration::from_millis(25)).await;
                }
            })
            .await;
            if matches!(ready, Ok(true)) {
                break (child, base, token);
            }
            let transcript = fs::read_to_string(&log).unwrap_or_default();
            if attempt < 3
                && transcript.lines().any(|line| {
                    line.contains("API server failed to bind:")
                        && line.contains("Address already in use")
                })
            {
                child
                    .kill()
                    .await
                    .expect("stop gateway after port collision");
                continue;
            }
            panic!("gateway did not become ready: {transcript}");
        };

        let response = client
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth(&token)
            .json(&json!({
                "user": "fixture-client",
                "session_id": "fixture-session",
                "messages": [{"role": "user", "content": "Return the fixture reply."}],
                "stream": streaming
            }))
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "gateway completion request failed (stream={streaming}): {error}\n{}",
                    fs::read_to_string(&log).unwrap_or_default()
                );
            });
        let status = response.status();
        let connection = response
            .headers()
            .get(reqwest::header::CONNECTION)
            .and_then(|header| header.to_str().ok())
            .map(str::to_owned);
        let payload = response.text().await.unwrap();
        assert!(
            status.is_success(),
            "{status}: {payload}\n{}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        assert_eq!(connection.as_deref(), Some("close"));
        if streaming {
            assert!(payload.ends_with("data: [DONE]\n\n"), "{payload}");
            let reply: String = payload
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .filter(|line| *line != "[DONE]")
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter_map(|chunk| {
                    chunk["choices"][0]["delta"]["content"]
                        .as_str()
                        .map(str::to_owned)
                })
                .collect();
            assert_eq!(reply, REPLY);
        } else {
            let result: Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(result["choices"][0]["message"]["content"], REPLY);
        }

        let pid = i32::try_from(child.id().unwrap()).unwrap();
        // The PID belongs to the direct child whose handle we still hold.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
        assert!(timeout(Duration::from_secs(15), child.wait())
            .await
            .expect("gateway shutdown timeout")
            .unwrap()
            .success());
        assert!(!home.join("gateway.pid").exists());
        provider.verify().await;
        let received = provider.received_requests().await.unwrap();
        assert_eq!(received.len(), if with_tool { 2 } else { 1 });
        for request in &received {
            let sent: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(sent["stream"].as_bool().unwrap_or(false), streaming);
        }
    }
}
