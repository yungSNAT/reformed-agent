use super::*;
use serde_json::json;

#[test]
fn converts_system_and_user_messages_and_merges_all_content_shapes() {
    let system_content = json!([{"type": "text", "text": "Final instructions"}]);
    let input = [
        json!({"role": "system", "content": "Superseded instructions"}),
        json!({"content": "  first message  "}),
        json!({"role": "user", "content": " \t "}),
        json!({"role": "system", "content": system_content}),
        json!({"role": "user", "content": [{"type": "unsupported"}]}),
        json!({
            "role": "custom",
            "content": [
                "raw text",
                {"type": "input_text", "text": "caption", "cache_control": {"type": "ephemeral"}},
                {"type": "image_url", "image_url": {"url": " data:AQID "}},
                {"type": "input_image", "image_url": "data:text/plain;base64,BAUG"},
                {"type": "image_url", "image_url": {"url": " https://example.invalid/image.png "}},
                {"type": "custom", "text": "fallback text", "cache_control": {"type": "ephemeral"}},
                {"type": "unsupported"}
            ]
        }),
        json!({"role": "user"}),
    ];

    let (system, messages) = convert_messages_to_anthropic(&input, None);

    assert_eq!(system, Some(system_content));
    assert_eq!(
        serde_json::to_value(messages).unwrap(),
        json!([{
            "role": "user",
            "content": [
                {"type": "text", "text": "  first message  \n(empty message)"},
                {"type": "text", "text": "(empty message)"},
                {"type": "text", "text": "raw text"},
                {"type": "text", "text": "caption", "cache_control": {"type": "ephemeral"}},
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "AQID"}},
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "BAUG"}},
                {"type": "image", "source": {"type": "url", "url": "https://example.invalid/image.png"}},
                {"type": "text", "text": "fallback text", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "(empty message)"}
            ]
        }])
    );
}

#[test]
fn converts_tool_calls_groups_results_and_removes_unpaired_blocks() {
    let input = [
        json!({
            "role": "assistant",
            "content": "Checking.",
            "tool_calls": [
                {"id": "call.1", "function": {"name": "lookup", "arguments": r#"{"limit":2}"#}},
                {"id": "call:2", "function": {"name": "fallback", "arguments": "not json"}},
                {}
            ]
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call.1",
            "content": "found",
            "cache_control": {"type": "ephemeral"}
        }),
        json!({"role": "tool", "tool_call_id": "call:2", "content": ""}),
        json!({"role": "tool", "content": {"count": 2}}),
        json!({"role": "tool", "tool_call_id": "stray", "content": "discard this"}),
        json!({"role": "assistant", "tool_calls": [{"id": "unanswered"}]}),
        json!({"role": "tool", "tool_call_id": "unmatched-1", "content": "discard this"}),
        json!({"role": "tool", "tool_call_id": "unmatched-2", "content": "discard this too"}),
    ];

    let (system, messages) = convert_messages_to_anthropic(&input, None);

    assert_eq!(system, None);
    assert_eq!(
        serde_json::to_value(messages).unwrap(),
        json!([
            {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Checking."},
                    {"type": "tool_use", "id": "call_1", "name": "lookup", "input": {"limit": 2}},
                    {"type": "tool_use", "id": "call_2", "name": "fallback", "input": {}},
                    {"type": "tool_use", "id": "tool_0", "name": "", "input": {}}
                ]
            },
            {
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "call_1",
                        "content": "found",
                        "cache_control": {"type": "ephemeral"}
                    },
                    {"type": "tool_result", "tool_use_id": "call_2", "content": "(no output)"},
                    {"type": "tool_result", "tool_use_id": "tool_0", "content": r#"{"count":2}"#}
                ]
            },
            {
                "role": "assistant",
                "content": [{"type": "text", "text": "(tool call removed)"}]
            },
            {
                "role": "user",
                "content": [{"type": "text", "text": "(tool result removed)"}]
            }
        ])
    );
}

#[test]
fn converts_multipart_assistant_content_and_preserves_empty_message_markers() {
    let input = [
        json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Answer"},
                {"type": "unsupported"},
                "Second part"
            ]
        }),
        json!({"role": "assistant", "content": ""}),
        json!({"role": "user", "content": [{"type": "text", "text": "Follow-up"}]}),
        json!({"role": "tool", "tool_call_id": "unknown", "content": "discard this"}),
    ];

    let (_, messages) = convert_messages_to_anthropic(&input, None);

    assert_eq!(
        serde_json::to_value(messages).unwrap(),
        json!([
            {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Answer"},
                    {"type": "text", "text": "Second part"},
                    {"type": "text", "text": "(empty)"}
                ]
            },
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "Follow-up"},
                    {"type": "text", "text": "(tool result removed)"}
                ]
            }
        ])
    );
}

#[test]
fn strips_stale_thinking_and_only_keeps_latest_thinking_on_native_endpoints() {
    let older = json!({
        "role": "assistant",
        "content": [
            {"type": "thinking", "thinking": "older reasoning", "signature": "older-signature"},
            {"type": "redacted_thinking", "data": "older-opaque-data"}
        ]
    });
    let user = json!({"role": "user", "content": "Continue"});
    let latest = json!({
        "role": "assistant",
        "content": [
            {"type": "thinking", "thinking": "latest reasoning", "signature": "latest-signature"},
            {"type": "redacted_thinking", "data": "latest-opaque-data"},
            {"type": "text", "text": "Visible answer"}
        ]
    });
    let trailing_user = json!({"role": "user", "content": "Thanks"});
    let history = json!([older, user, latest, trailing_user]);

    for (base_url, keep_latest) in [
        (None, true),
        (Some(""), true),
        (Some("https://api.anthropic.com"), true),
        (Some("https://proxy.example.invalid/v1"), false),
    ] {
        let mut messages: Vec<AnthropicMessage> = serde_json::from_value(history.clone()).unwrap();
        strip_stale_thinking_blocks(&mut messages, base_url);

        let expected_latest = if keep_latest {
            latest.clone()
        } else {
            json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "Visible answer"}]
            })
        };
        assert_eq!(
            serde_json::to_value(messages).unwrap(),
            json!([
                {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "(thinking elided)"}]
                },
                user,
                expected_latest,
                trailing_user
            ]),
            "base URL: {base_url:?}"
        );
    }
}

#[test]
fn normalizes_reasoning_and_optionally_strips_tool_prefix_without_visible_text() {
    let details = [
        json!({"type": "thinking", "thinking": "first thought", "signature": "sig"}),
        json!({"type": "redacted_thinking", "data": "opaque"}),
        json!({"type": "thinking", "thinking": 42}),
        json!({"type": "thinking", "thinking": "second thought"}),
    ];
    let blocks = [
        details[0].clone(),
        json!({"type": "unsupported", "text": "ignore this"}),
        details[1].clone(),
        details[2].clone(),
        details[3].clone(),
        json!({"type": "tool_use", "id": "one", "name": "mcp_lookup", "input": {"key": "value"}}),
        json!({"type": "tool_use", "id": "two", "name": "local_tool"}),
    ];

    for (strip_prefix, expected_name) in [(true, "lookup"), (false, "mcp_lookup")] {
        let (message, finish_reason) =
            normalize_anthropic_response(&blocks, "tool_use", strip_prefix);

        assert_eq!(finish_reason, "tool_calls");
        assert_eq!(message.content, None);
        assert_eq!(
            message.reasoning.as_deref(),
            Some("first thought\n\nsecond thought")
        );
        assert_eq!(message.reasoning_details.as_deref(), Some(&details[..]));
        assert_eq!(
            serde_json::to_value(message.tool_calls.unwrap()).unwrap(),
            json!([
                {"id": "one", "name": expected_name, "arguments": r#"{"key":"value"}"#},
                {"id": "two", "name": "local_tool", "arguments": "{}"}
            ])
        );
    }
}

#[test]
fn endpoint_checks_handle_absent_blank_and_malformed_urls() {
    assert!(!is_third_party_endpoint(Some(" /// ")));
    assert!(!is_azure_anthropic_endpoint(None));
    assert!(!is_azure_anthropic_endpoint(Some(" \t ")));
    assert!(!is_azure_anthropic_endpoint(Some("not a URL")));
    assert!(!requires_bearer_auth(None));
}

#[test]
fn native_oauth_fast_mode_includes_common_oauth_and_fast_mode_betas() {
    assert_eq!(
        fast_mode_request_beta_list(Some(" https://api.anthropic.com/ "), true),
        Some(vec![
            "interleaved-thinking-2025-05-14",
            "fine-grained-tool-streaming-2025-05-14",
            "claude-code-20250219",
            "oauth-2025-04-20",
            "fast-mode-2026-02-01",
        ])
    );
}

#[test]
fn reasoning_effort_maps_to_fixed_budgets_and_adaptive_names() {
    for (effort, budget, adaptive_name) in [
        (ReasoningEffort::XHigh, 32_000, "max"),
        (ReasoningEffort::High, 16_000, "high"),
        (ReasoningEffort::Medium, 8_000, "medium"),
        (ReasoningEffort::Low, 4_000, "low"),
        (ReasoningEffort::Minimal, 4_000, "low"),
    ] {
        assert_eq!(thinking_budget(effort), budget, "{effort:?}");
        assert_eq!(adaptive_effort_string(effort), adaptive_name, "{effort:?}");
    }
}
