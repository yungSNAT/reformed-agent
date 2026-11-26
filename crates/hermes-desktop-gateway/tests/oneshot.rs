//! `llm.oneshot` as the desktop's review pane and new-project dialog call it: one model call
//! outside any conversation. The model is a scripted OpenAI-compatible endpoint that keeps
//! each request body.

#[path = "support/oneshot.rs"]
mod support;

use serde_json::json;
use support::{call, connect, start};

/// The review pane's request: the `commit_message` template with the diff, recent subjects and
/// the message to avoid, at the temperature it asks for; the fence the model added is removed.
#[tokio::test]
async fn the_commit_message_template_goes_to_the_model() {
    let (addr, requests, _home) = start(true).await;
    let mut socket = connect(addr).await;
    let params = json!({
        "template": "commit_message",
        "temperature": 0.8,
        "variables": {"diff": "+fn served() {}", "recent_commits": "fix: an earlier one",
                      "avoid": "feat: the last proposal"},
    });
    let reply = call(&mut socket, 2, params).await;
    assert_eq!(
        reply["result"],
        json!({"text": "feat(gateway): serve one-shot requests"}),
        "{reply}"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let body = &requests[0];
    assert_eq!(body["temperature"], 0.8);
    assert_eq!(body["max_tokens"], 1024);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "{body}");
    assert_eq!(messages[0]["role"], "system");
    let system = messages[0]["content"].as_str().unwrap();
    assert!(
        system.starts_with("You write git commit messages."),
        "{system}"
    );
    let user = messages[1]["content"].as_str().unwrap();
    for part in [
        "+fn served() {}",
        "fix: an earlier one",
        "feat: the last proposal",
    ] {
        assert!(user.contains(part), "{part} missing from {user}");
    }
}

/// Raw instructions and input, as the new-project dialog sends them; an unknown `session_id`
/// is not an error, as upstream.
#[tokio::test]
async fn instructions_and_input_go_as_they_are() {
    let (addr, requests, _home) = start(true).await;
    let mut socket = connect(addr).await;
    let params = json!({"instructions": "Write an idea.", "input": "Project name: x",
                        "max_tokens": 50, "session_id": "not-a-session"});
    let reply = call(&mut socket, 2, params).await;
    assert!(reply["result"]["text"].is_string(), "{reply}");
    let body = requests.lock().unwrap()[0].clone();
    assert_eq!(body["max_tokens"], 50);
    assert_eq!(body["temperature"], 0.3);
    let sent: Vec<(&str, &str)> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["role"].as_str().unwrap(), m["content"].as_str().unwrap()))
        .collect();
    assert_eq!(
        sent,
        [("system", "Write an idea."), ("user", "Project name: x")]
    );
}

/// Upstream's refusals, none of which reaches the model.
#[tokio::test]
async fn llm_oneshot_refuses_bad_requests() {
    let (addr, requests, _home) = start(true).await;
    let mut socket = connect(addr).await;
    let cases = [
        (json!({}), 4030),
        (json!({"instructions": "  ", "input": ""}), 4030),
        (json!({"template": "  "}), 4030),
        (json!({"template": "limerick", "input": "x"}), 4031),
    ];
    for (n, (params, code)) in cases.iter().enumerate() {
        let reply = call(&mut socket, 10 + n as u64, params.clone()).await;
        assert_eq!(reply["error"]["code"], *code, "{params}: {reply}");
    }
    assert!(requests.lock().unwrap().is_empty());

    let (addr, _, _home) = start(false).await;
    let mut socket = connect(addr).await;
    let reply = call(&mut socket, 2, json!({"input": "hello"})).await;
    assert_eq!(reply["error"]["code"], 5030, "{reply}");
}

/// Out-of-range numbers are brought within bounds before the model sees them; text over
/// 64 KiB, and a template, instructions or input that is not a string, is refused unsent.
#[tokio::test]
async fn llm_oneshot_keeps_requests_within_bounds() {
    let (addr, requests, _home) = start(true).await;
    let mut socket = connect(addr).await;
    let params = json!({"input": "x", "max_tokens": 1_000_000, "temperature": -5});
    let reply = call(&mut socket, 2, params).await;
    assert!(reply["result"]["text"].is_string(), "{reply}");
    let params = json!({"input": "x", "temperature": 9});
    let reply = call(&mut socket, 3, params).await;
    assert!(reply["result"]["text"].is_string(), "{reply}");
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests[0]["max_tokens"], 4096);
        assert_eq!(requests[0]["temperature"], 0.0);
        assert_eq!(requests[1]["temperature"], 2.0);
    }

    let long = "x".repeat((64 << 10) + 1);
    let cases = [
        json!({"input": long}),
        json!({"instructions": long, "input": "x"}),
        json!({"template": 5, "input": "x"}),
        json!({"instructions": ["a"], "input": "x"}),
        json!({"input": {"text": "x"}}),
    ];
    for (n, params) in cases.iter().enumerate() {
        let reply = call(&mut socket, 10 + n as u64, params.clone()).await;
        assert_eq!(reply["error"]["code"], 4000, "{reply}");
    }
    assert_eq!(requests.lock().unwrap().len(), 2);
}
