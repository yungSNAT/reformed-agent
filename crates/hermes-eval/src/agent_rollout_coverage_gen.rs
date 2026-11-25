use super::*;
use hermes_core::{AgentResult, MessageRole};

fn serialized_default<T: Default + serde::Serialize>(_field: fn(&AgentResult) -> &T) -> Value {
    serde_json::to_value(T::default()).expect("serialize fixture field")
}

fn result_with_messages(messages: Vec<Message>) -> AgentResult {
    let mut result: AgentResult = serde_json::from_value(json!({
        "messages": [],
        "finished_naturally": true,
        "total_turns": 3,
        "tool_errors": [],
        "usage": serialized_default(|result| &result.usage),
        "final_response": "fixture response",
    }))
    .expect("deserialize agent result fixture");
    result.messages = messages;
    result
}

fn assistant_message(text: &str) -> Message {
    let mut message = Message::user(text.to_owned());
    message.role = MessageRole::Assistant;
    message
}

#[test]
fn compact_summary_truncates_the_latest_assistant_text_by_character() {
    let preview = format!("{}é", "🦀".repeat(1999));
    let answer = format!("{preview}outside the preview");
    let result = result_with_messages(vec![
        Message::user("initial question".to_owned()),
        assistant_message("earlier answer"),
        assistant_message(&answer),
        Message::user("later user message".to_owned()),
    ]);

    let summary = AgentLoopRollout::summarize_result(&result, "unicode-task", false)
        .expect("compact summary");

    assert_eq!(
        summary,
        json!({
            "task_id": "unicode-task",
            "finished_naturally": true,
            "total_turns": 3,
            "message_count": 4,
            "tool_error_count": 0,
            "usage": result.usage,
            "last_assistant_preview": preview,
        })
    );
}

#[test]
fn compact_summary_uses_empty_preview_when_latest_assistant_text_is_absent() {
    let mut contentless_assistant = assistant_message("discarded text");
    contentless_assistant.content = None;

    for messages in [
        vec![Message::user("no assistant reply".to_owned())],
        vec![
            assistant_message("must not fall back to this reply"),
            contentless_assistant,
        ],
    ] {
        let mut result = result_with_messages(messages);
        result.finished_naturally = false;

        let summary = AgentLoopRollout::summarize_result(&result, "no-text-task", false)
            .expect("compact summary without assistant text");

        assert_eq!(summary["last_assistant_preview"], json!(""));
        assert_eq!(summary["finished_naturally"], json!(false));
    }
}

#[test]
fn full_summary_preserves_the_serialized_agent_result() {
    let result = result_with_messages(vec![
        Message::user("question".to_owned()),
        assistant_message(&"answer ".repeat(400)),
    ]);
    let expected = serde_json::to_value(&result).expect("serialize full agent result");

    let summary =
        AgentLoopRollout::summarize_result(&result, "full-state-task", true).expect("full summary");

    assert_eq!(summary, expected);
}
