//! Message sync helpers. Expected values follow the Python plugin this port replaced
//! (OpenViking `e855796c`, `examples/hermes-plugin/__init__.py`: `_preview`,
//! `_tool_call_input`, `_tool_result_status`, `_extract_current_turn_messages`).

use super::*;

#[test]
fn preview_escapes_newlines_and_cuts_long_text_at_160() {
    assert_eq!(preview_sync_value("a\nb"), "a\\nb");
    let exact = "x".repeat(160);
    assert_eq!(preview_sync_value(&exact), exact);
    assert_eq!(
        preview_sync_value("y".repeat(200)),
        format!("{}...", "y".repeat(160))
    );
    // The escape counts towards the limit: 159 characters plus "\\n" is 161.
    let escaped = format!("{}\n", "z".repeat(159));
    assert_eq!(
        preview_sync_value(escaped),
        format!("{}\\...", "z".repeat(159))
    );
}

#[test]
fn tool_call_input_parses_arguments_into_an_object() {
    let cases = [
        (
            json!({"function": {"arguments": {"a": 1}}}),
            json!({"a": 1}),
        ),
        (
            json!({"function": {"arguments": "{\"cmd\":\"ls\"}"}}),
            json!({"cmd": "ls"}),
        ),
        (
            json!({"function": {"arguments": "[1,2]"}}),
            json!({"value": [1, 2]}),
        ),
        (
            json!({"function": {"arguments": "not json"}}),
            json!({"value": "not json"}),
        ),
        (json!({"function": {"arguments": "   "}}), json!({})),
        (json!({"args": "{\"k\":\"v\"}"}), json!({"k": "v"})),
        (json!({"args": 7}), json!({"value": 7})),
        (json!({"id": "call_1"}), json!({})),
    ];
    for (tool_call, expected) in cases {
        assert_eq!(tool_call_input(&tool_call), expected, "{tool_call}");
    }
}

#[test]
fn tool_result_status_reads_aliases_then_the_json_result() {
    let cases = [
        (json!({"status": "FAILED", "content": "ok"}), "error"),
        (
            json!({"tool_status": "Success", "content": "{\"exit_code\": 1}"}),
            "completed",
        ),
        (json!({"content": "{\"success\": false}"}), "error"),
        (json!({"content": "{\"exit_code\": 2}"}), "error"),
        (json!({"content": "{\"exit_code\": 0}"}), "completed"),
        (json!({"content": "{\"error\": \"boom\"}"}), "error"),
        (
            json!({"status": "pending", "content": "plain output"}),
            "completed",
        ),
        (json!({"content": ""}), "completed"),
    ];
    for (message, expected) in cases {
        assert_eq!(tool_result_status(&message), expected, "{message}");
    }
}

#[test]
fn current_turn_falls_back_to_the_nearest_user_and_needs_one() {
    let messages = vec![
        json!({"role": "user", "content": "old question"}),
        json!({"role": "assistant", "content": "old answer"}),
        json!({"role": "user", "content": "new question"}),
        json!({"role": "assistant", "content": "new answer"}),
    ];

    assert_eq!(
        extract_current_turn_messages(&messages, "no such text", "new answer"),
        messages[2..].to_vec(),
        "an unmatched user text takes the nearest user before the end"
    );
    assert_eq!(
        extract_current_turn_messages(&messages, "old question", "old answer"),
        messages[..2].to_vec(),
        "a matched assistant ends the turn early"
    );
    assert!(
        extract_current_turn_messages(&[json!({"role": "assistant", "content": "hi"})], "", "")
            .is_empty(),
        "no user message gives no turn"
    );
    assert!(extract_current_turn_messages(&[], "q", "a").is_empty());
}
