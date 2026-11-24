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
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPLY: &str = "1234567890123456789é follows the SSE boundary.";

fn provider_response(streaming: bool) -> ResponseTemplate {
    if !streaming {
        return ResponseTemplate::new(200).set_body_json(json!({
            "id": "fixture-chat",
            "object": "chat.completion",
            "created": 0,
            "model": "deterministic",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": REPLY},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
    }
    let start = json!({
        "id": "fixture-stream",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "deterministic",
        "choices": [{"index": 0, "delta": {"role": "assistant", "content": REPLY}}]
    });
    let stop = json!({
        "id": "fixture-stream",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "deterministic",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
    });
    ResponseTemplate::new(200).set_body_raw(
        format!("data: {start}\n\ndata: {stop}\n\ndata: [DONE]\n\n"),
        "text/event-stream",
    )
}

#[tokio::test]
async fn live_gateway_routes_complete_and_streamed_turns_to_the_configured_provider() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    for streaming in [false, true] {
        let provider = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(json!({"model": "deterministic"})))
            .respond_with(provider_response(streaming))
            .expect(1)
            .mount(&provider)
            .await;

        let sandbox = tempfile::tempdir().unwrap();
        let home = sandbox.path().join("hermes");
        fs::create_dir(&home).unwrap();
        let log = home.join("gateway-test.log");
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
llm_providers:
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
            .unwrap();
        let status = response.status();
        let payload = response.text().await.unwrap();
        assert!(
            status.is_success(),
            "{status}: {payload}\n{}",
            fs::read_to_string(&log).unwrap_or_default()
        );
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
        let [request] = received.as_slice() else {
            panic!("expected one provider request");
        };
        let sent: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(sent["stream"].as_bool().unwrap_or(false), streaming);
    }
}
