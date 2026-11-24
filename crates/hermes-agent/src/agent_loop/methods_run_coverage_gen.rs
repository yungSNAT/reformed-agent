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

#[derive(Default)]
struct TurnBudgetProvider {
    streams: std::sync::atomic::AtomicUsize,
    summaries: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl LlmProvider for TurnBudgetProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.summaries.fetch_add(1, Ordering::Relaxed);
        assert!(tools.is_empty(), "the final summary must not ask for tools");
        assert!(messages.iter().any(|message| {
            message.role == MessageRole::System
                && message
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("Maximum conversation turns reached"))
        }));
        assert!(messages.iter().any(|message| {
            message.role == MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("fixture-call")
                && message.content.as_deref() == Some("one nest")
        }));
        Ok(LlmResponse {
            message: Message::assistant("Recorded one nest."),
            usage: None,
            model: "fixture-model".into(),
            finish_reason: Some("stop".into()),
        })
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        let count = self.streams.fetch_add(1, Ordering::Relaxed);
        assert_eq!(count, 0, "the stream must stop at the turn cap");
        futures::stream::iter([
            Ok(StreamChunk {
                delta: Some(hermes_core::StreamDelta {
                    content: None,
                    tool_calls: Some(vec![hermes_core::ToolCallDelta {
                        index: 0,
                        id: Some("fixture-call".into()),
                        function: Some(hermes_core::FunctionCallDelta {
                            name: Some("record_nest".into()),
                            arguments: Some("{}".into()),
                        }),
                    }]),
                    extra: None,
                }),
                finish_reason: None,
                usage: None,
            }),
            Ok(StreamChunk {
                delta: None,
                finish_reason: Some("tool_calls".into()),
                usage: None,
            }),
        ])
        .boxed()
    }
}

fn turn_budget_agent(
    home: &Path,
    provider: Arc<TurnBudgetProvider>,
) -> AgentLoop {
    let mut registry = ToolRegistry::new();
    registry.register(
        "record_nest",
        ToolSchema::new("record_nest", "Record the fixture", hermes_core::JsonSchema::new("object")),
        Arc::new(|_| Ok("one nest".to_string())),
    );
    let config = AgentConfig {
        model: "openai:fixture-model".into(),
        max_turns: 1,
        budget_pressure_enabled: false,
        hermes_home: Some(home.to_string_lossy().into_owned()),
        skip_memory: true,
        skip_context_files: true,
        code_index_enabled: false,
        ..AgentConfig::default()
    };
    AgentLoop::new(config, Arc::new(registry), provider)
}

#[tokio::test]
async fn streaming_turn_cap_preserves_tool_result_and_requests_summary() {
    let home = tempfile::tempdir().unwrap();
    let provider = Arc::new(TurnBudgetProvider::default());
    let agent = turn_budget_agent(home.path(), provider.clone());
    let result = agent
        .run_stream(
            vec![Message::user("Record the nest.")],
            None,
            Some(Box::new(|_| {})),
        )
        .await
        .unwrap();

    assert_eq!(result.total_turns, 1);
    assert!(!result.finished_naturally);
    assert!(!result.interrupted);
    assert!(result.messages.iter().any(|message| {
        message.role == MessageRole::Tool
            && message.tool_call_id.as_deref() == Some("fixture-call")
            && message.content.as_deref() == Some("one nest")
    }));
    assert_eq!(
        result.messages.last().and_then(|message| message.content.as_deref()),
        Some("Recorded one nest.")
    );
    assert_eq!(provider.streams.load(Ordering::Relaxed), 1);
    assert_eq!(provider.summaries.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn interrupt_before_streaming_preserves_user_message_without_calling_provider() {
    let home = tempfile::tempdir().unwrap();
    let provider = Arc::new(TurnBudgetProvider::default());
    let agent = turn_budget_agent(home.path(), provider.clone());
    agent.interrupt.interrupt(None);
    let result = agent
        .run_stream(
            vec![Message::user("Record the nest.")],
            None,
            Some(Box::new(|_| panic!("interrupted before any stream chunks"))),
        )
        .await
        .unwrap();

    assert!(result.interrupted);
    assert!(!result.finished_naturally);
    assert_eq!(result.total_turns, 0);
    assert!(result.messages.iter().any(|message| {
        message.role == MessageRole::User && message.content.as_deref() == Some("Record the nest.")
    }));
    assert_eq!(provider.streams.load(Ordering::Relaxed), 0);
    assert_eq!(provider.summaries.load(Ordering::Relaxed), 0);
}
