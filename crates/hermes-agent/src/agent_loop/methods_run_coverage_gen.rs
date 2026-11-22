use super::*;

fn context_expansion_agent() -> AgentLoop {
    // Context expansion does not send requests to the provider.
    let provider = crate::provider::OpenAiProvider::new("unused-test-key");
    AgentLoop::new(
        AgentConfig::default(),
        Default::default(),
        std::sync::Arc::new(provider),
    )
}

const ATTACHMENT_BODY: &str = "Hermes context expansion fixture: violet lantern.";

fn context_file_reference() -> (tempfile::TempDir, String) {
    // References are confined to the working directory. Put the fixture there
    // without changing the process-wide working directory.
    let directory = tempfile::Builder::new()
        .prefix("hermes-run-context-")
        .tempdir_in(".")
        .expect("create context fixture directory");
    std::fs::write(directory.path().join("note.txt"), ATTACHMENT_BODY)
        .expect("write context fixture");
    let name = directory
        .path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("fixture directory has a UTF-8 name");
    let reference = format!("@file:{name}/note.txt");
    (directory, reference)
}

#[tokio::test]
async fn context_expansion_leaves_non_user_messages_untouched() {
    let (_directory, reference) = context_file_reference();
    let agent = context_expansion_agent();
    let mut messages = [Message::system(&reference)];

    agent.expand_context_references(&mut messages).await;

    assert_eq!(messages[0].content.as_deref(), Some(reference.as_str()));
}

#[tokio::test]
async fn context_expansion_does_not_fall_back_to_history_when_latest_user_has_no_content() {
    let (_directory, reference) = context_file_reference();
    let agent = context_expansion_agent();
    let mut latest = Message::user("placeholder");
    latest.content = None;
    let mut messages = [
        Message::user(&reference),
        latest,
        Message::system("Trailing system note"),
    ];

    agent.expand_context_references(&mut messages).await;

    assert_eq!(messages[0].content.as_deref(), Some(reference.as_str()));
    assert_eq!(messages[1].content, None);
    assert_eq!(messages[2].content.as_deref(), Some("Trailing system note"));
}

#[tokio::test]
async fn context_expansion_skips_non_string_acp_text_and_expands_the_following_part() {
    let (_directory, reference) = context_file_reference();
    let agent = context_expansion_agent();
    let parts = vec![
        serde_json::json!({"type": "text", "text": 17}),
        serde_json::json!({"type": "image", "text": &reference}),
        serde_json::json!({"type": "text", "text": &reference}),
    ];
    let prompt = format!(
        "{}{}",
        crate::provider::ACP_MULTIMODAL_PREFIX,
        Value::Array(parts.clone())
    );
    let mut messages = [Message::user(&reference), Message::user(&prompt)];

    agent.expand_context_references(&mut messages).await;

    assert_eq!(messages[0].content.as_deref(), Some(reference.as_str()));
    let content = messages[1].content.as_deref().expect("ACP prompt retained");
    let expanded = crate::provider::parse_acp_multimodal_parts(content)
        .expect("expanded content is still an ACP prompt");
    assert_eq!(expanded.len(), parts.len());
    assert_eq!(&expanded[..2], &parts[..2]);
    assert_eq!(expanded[2]["type"], "text");
    let text = expanded[2]["text"].as_str().expect("expanded text part");
    assert!(
        text.contains(ATTACHMENT_BODY),
        "unexpected expansion: {text}"
    );
}
