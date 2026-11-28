#[test]
fn coding_context_prunes_non_coding_skill_categories_in_prompt_only() {
    use hermes_core::JsonSchema;

    let _lock = env_test_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("write manifest");
    let home = tmp.path().join("home");
    let social = home.join("skills").join("social-media").join("tweet-stuff");
    let github = home.join("skills").join("github").join("pr-review");
    std::fs::create_dir_all(&social).expect("social skill dir");
    std::fs::create_dir_all(&github).expect("github skill dir");
    std::fs::write(
        social.join("SKILL.md"),
        "---\nname: tweet-stuff\ndescription: Draft social posts\n---\nBody",
    )
    .expect("write social skill");
    std::fs::write(
        github.join("SKILL.md"),
        "---\nname: pr-review\ndescription: Review pull requests\n---\nBody",
    )
    .expect("write github skill");
    let _cwd = EnvVarGuard::set("TERMINAL_CWD", tmp.path().to_str().unwrap());

    let config = AgentConfig {
        platform: Some("cli".to_string()),
        coding_context: "auto".to_string(),
        hermes_home: Some(home.to_string_lossy().to_string()),
        skip_memory: true,
        skip_context_files: true,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider),
    );
    let tools = vec![
        ToolSchema::new("terminal", "Terminal", JsonSchema::new("object")),
        ToolSchema::new("skills_list", "List skills", JsonSchema::new("object")),
        ToolSchema::new("skill_view", "View skill", JsonSchema::new("object")),
    ];
    let prompt = agent.build_system_prompt("", &tools, "anthropic/claude-sonnet-4");
    assert!(prompt.contains("pr-review"));
    assert!(!prompt.contains("tweet-stuff"));
    assert!(prompt.contains("skills_prompt_pruned"));
    assert!(
        prompt.contains("full catalog remains available through skills_list and skill_view")
    );
    assert!(prompt.contains("mode='replace'"));
}

#[tokio::test]
async fn mid_turn_steer_is_appended_to_last_tool_result() {
    use futures::stream::BoxStream;
    use hermes_core::{FunctionCall, JsonSchema};

    #[derive(Default)]
    struct RecordingProvider {
        calls: std::sync::Mutex<u32>,
        second_call_messages: std::sync::Mutex<Vec<Message>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RecordingProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let mut calls = self.calls.lock().expect("calls lock");
            *calls += 1;
            if *calls == 1 {
                return Ok(hermes_core::LlmResponse {
                    message: Message::assistant_with_tool_calls(
                        None,
                        vec![ToolCall {
                            id: "call_1".to_string(),
                            function: FunctionCall {
                                name: "steer_test".to_string(),
                                arguments: "{}".to_string(),
                            },
                            extra_content: None,
                        }],
                    ),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("tool_calls".into()),
                });
            }

            *self.second_call_messages.lock().expect("messages lock") = messages.to_vec();
            Ok(hermes_core::LlmResponse {
                message: Message::assistant("done"),
                usage: None,
                model: "dummy".into(),
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
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    let interrupt = InterruptController::new();
    let interrupt_for_tool = interrupt.clone();
    let mut registry = ToolRegistry::new();
    registry.register(
        "steer_test",
        ToolSchema::new("steer_test", "Steer test", JsonSchema::new("object")),
        Arc::new(move |_args| {
            interrupt_for_tool.steer("prefer the simpler fix");
            Ok("tool output".to_string())
        }),
    );
    let provider = Arc::new(RecordingProvider::default());
    let agent = AgentLoop::with_interrupt(
        AgentConfig::default(),
        Arc::new(registry),
        provider.clone(),
        interrupt,
    );

    let result = agent
        .run(vec![Message::user("use the tool")], None)
        .await
        .expect("agent run");

    assert!(!result.interrupted);
    let second_call_messages = provider
        .second_call_messages
        .lock()
        .expect("messages lock")
        .clone();
    let tool_message = second_call_messages
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .expect("tool result in second call");
    assert_eq!(tool_message.name.as_deref(), Some("steer_test"));
    let content = tool_message.content.as_deref().expect("tool content");
    assert!(content.contains("tool output"));
    assert!(content.contains(crate::steer::STEER_MARKER_OPEN));
    assert!(content.contains("prefer the simpler fix"));
    assert!(content.contains(crate::steer::STEER_MARKER_CLOSE));
    assert!(!content.contains("User guidance:"));

    let persisted_tool_message = result
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .expect("persisted tool result");
    assert_eq!(persisted_tool_message.name.as_deref(), Some("steer_test"));
}

#[tokio::test]
async fn a_steer_sent_during_a_model_call_reaches_the_next_one() {
    use futures::stream::BoxStream;
    use hermes_core::{FunctionCall, JsonSchema};

    struct SteeringProvider {
        interrupt: InterruptController,
        calls: std::sync::Mutex<Vec<Vec<Message>>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for SteeringProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let mut calls = self.calls.lock().expect("calls lock");
            calls.push(messages.to_vec());
            if calls.len() > 1 {
                return Ok(hermes_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                });
            }
            // Sent whilst the model works, before the turn has a tool result.
            self.interrupt.steer("prefer the simpler fix");
            Ok(hermes_core::LlmResponse {
                message: Message::assistant_with_tool_calls(
                    None,
                    vec![ToolCall {
                        id: "call_1".to_string(),
                        function: FunctionCall {
                            name: "plain_tool".to_string(),
                            arguments: "{}".to_string(),
                        },
                        extra_content: None,
                    }],
                ),
                usage: None,
                model: "dummy".into(),
                finish_reason: Some("tool_calls".into()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    let interrupt = InterruptController::new();
    let mut registry = ToolRegistry::new();
    registry.register(
        "plain_tool",
        ToolSchema::new("plain_tool", "Plain tool", JsonSchema::new("object")),
        Arc::new(|_args| Ok("tool output".to_string())),
    );
    let provider = Arc::new(SteeringProvider {
        interrupt: interrupt.clone(),
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let agent = AgentLoop::with_interrupt(
        AgentConfig::default(),
        Arc::new(registry),
        provider.clone(),
        interrupt.clone(),
    );

    let result = agent
        .run(vec![Message::user("use the tool")], None)
        .await
        .expect("agent run");

    assert!(!result.interrupted);
    let calls = provider.calls.lock().expect("calls lock").clone();
    assert_eq!(calls.len(), 2);
    let tool_message = calls[1]
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .expect("tool result in second call");
    let content = tool_message.content.as_deref().expect("tool content");
    assert!(content.starts_with("tool output"));
    assert!(content.ends_with(&crate::steer::format_steer_marker("prefer the simpler fix")));
    assert_eq!(interrupt.take_steer(), None);
}

#[tokio::test]
async fn a_steer_during_a_streamed_answer_leaves_the_answer_whole() {
    use futures::stream::BoxStream;
    use hermes_core::StreamDelta;

    struct SteeredStream {
        interrupt: InterruptController,
    }

    #[async_trait::async_trait]
    impl LlmProvider for SteeredStream {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            Err(AgentError::LlmApi("this run streams".to_string()))
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            let text = |content: &str| {
                Ok(StreamChunk {
                    delta: Some(StreamDelta {
                        content: Some(content.to_string()),
                        tool_calls: None,
                        extra: None,
                    }),
                    finish_reason: None,
                    usage: None,
                })
            };
            let chunks = vec![
                text("first "),
                text("second"),
                Ok(StreamChunk {
                    delta: None,
                    finish_reason: Some("stop".into()),
                    usage: None,
                }),
            ];
            let interrupt = self.interrupt.clone();
            futures::stream::iter(chunks)
                .enumerate()
                .map(move |(index, chunk)| {
                    // Sent once the first words are out.
                    if index == 1 {
                        interrupt.steer("also mention the tests");
                    }
                    chunk
                })
                .boxed()
        }
    }

    let interrupt = InterruptController::new();
    let config = AgentConfig {
        skip_memory: true,
        skip_context_files: true,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::with_interrupt(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(SteeredStream {
            interrupt: interrupt.clone(),
        }),
        interrupt.clone(),
    );

    let result = agent
        .run_stream(vec![Message::user("answer")], None, Some(Box::new(|_| {})))
        .await
        .expect("agent run");

    assert!(!result.interrupted);
    assert!(result
        .messages
        .iter()
        .any(|message| message.content.as_deref() == Some("first second")));
    // No tool result came to carry it, so it is still pending when the run
    // ends; the controller's owner decides what happens to it.
    assert_eq!(
        interrupt.take_steer().as_deref(),
        Some("also mention the tests")
    );
}

#[tokio::test]
async fn a_steer_during_an_answer_the_loop_rejects_reaches_the_retry() {
    use futures::stream::BoxStream;
    use hermes_core::{FunctionCall, JsonSchema};

    struct RejectedAnswerProvider {
        interrupt: InterruptController,
        calls: std::sync::Mutex<Vec<Vec<Message>>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RejectedAnswerProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let mut calls = self.calls.lock().expect("calls lock");
            calls.push(messages.to_vec());
            let message = match calls.len() {
                1 => Message::assistant_with_tool_calls(
                    None,
                    vec![ToolCall {
                        id: "call_1".to_string(),
                        function: FunctionCall {
                            name: "plain_tool".to_string(),
                            arguments: "{}".to_string(),
                        },
                        extra_content: None,
                    }],
                ),
                2 => {
                    // Sent whilst the model writes an answer that names none of
                    // the user's anchors, so the loop asks for it again.
                    self.interrupt.steer("prefer the simpler fix");
                    Message::assistant("done")
                }
                _ => Message::assistant("Re-issued: src/main.rs is fixed"),
            };
            Ok(hermes_core::LlmResponse {
                message,
                usage: None,
                model: "dummy".into(),
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
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    let interrupt = InterruptController::new();
    let mut registry = ToolRegistry::new();
    registry.register(
        "plain_tool",
        ToolSchema::new("plain_tool", "Plain tool", JsonSchema::new("object")),
        Arc::new(|_args| Ok("tool output".to_string())),
    );
    let provider = Arc::new(RejectedAnswerProvider {
        interrupt: interrupt.clone(),
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let config = AgentConfig {
        skip_memory: true,
        skip_context_files: true,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::with_interrupt(
        config,
        Arc::new(registry),
        provider.clone(),
        interrupt.clone(),
    );

    let result = agent
        .run(vec![Message::user("fix src/main.rs")], None)
        .await
        .expect("agent run");

    assert!(!result.interrupted);
    let calls = provider.calls.lock().expect("calls lock").clone();
    assert!(calls.len() >= 3);
    let retry = &calls[2];
    let tool_at = retry
        .iter()
        .position(|message| message.role == MessageRole::Tool)
        .expect("tool result in the retry");
    // The loop's own request to re-issue the answer follows the tool result.
    assert!(retry[tool_at..]
        .iter()
        .any(|message| message.role == MessageRole::User));
    let content = retry[tool_at].content.as_deref().expect("tool content");
    assert!(content.ends_with(&crate::steer::format_steer_marker("prefer the simpler fix")));
    assert_eq!(interrupt.take_steer(), None);
}

#[tokio::test]
async fn a_steer_during_a_streamed_tool_call_reaches_the_next_stream() {
    use futures::stream::BoxStream;
    use hermes_core::{FunctionCallDelta, JsonSchema, StreamDelta, ToolCallDelta};

    struct SteeredToolStream {
        interrupt: InterruptController,
        calls: std::sync::Mutex<Vec<Vec<Message>>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for SteeredToolStream {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            Err(AgentError::LlmApi("this run streams".to_string()))
        }

        fn chat_completion_stream(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            let mut calls = self.calls.lock().expect("calls lock");
            calls.push(messages.to_vec());
            let chunk = |delta: StreamDelta, finish: Option<&str>| {
                Ok(StreamChunk {
                    delta: Some(delta),
                    finish_reason: finish.map(str::to_string),
                    usage: None,
                })
            };
            if calls.len() > 1 {
                let text = StreamDelta {
                    content: Some("done".to_string()),
                    tool_calls: None,
                    extra: None,
                };
                return futures::stream::iter(vec![chunk(text, Some("stop"))]).boxed();
            }
            let call = StreamDelta {
                content: None,
                tool_calls: Some(vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".to_string()),
                    function: Some(FunctionCallDelta {
                        name: Some("plain_tool".to_string()),
                        arguments: Some("{}".to_string()),
                    }),
                }]),
                extra: None,
            };
            let end = StreamDelta {
                content: None,
                tool_calls: None,
                extra: None,
            };
            let chunks = vec![chunk(call, None), chunk(end, Some("tool_calls"))];
            let interrupt = self.interrupt.clone();
            futures::stream::iter(chunks)
                .enumerate()
                .map(move |(index, chunk)| {
                    // Sent whilst the tool call streams in.
                    if index == 1 {
                        interrupt.steer("prefer the simpler fix");
                    }
                    chunk
                })
                .boxed()
        }
    }

    let interrupt = InterruptController::new();
    let mut registry = ToolRegistry::new();
    registry.register(
        "plain_tool",
        ToolSchema::new("plain_tool", "Plain tool", JsonSchema::new("object")),
        Arc::new(|_args| Ok("tool output".to_string())),
    );
    let provider = Arc::new(SteeredToolStream {
        interrupt: interrupt.clone(),
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let config = AgentConfig {
        skip_memory: true,
        skip_context_files: true,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::with_interrupt(
        config,
        Arc::new(registry),
        provider.clone(),
        interrupt.clone(),
    );

    let result = agent
        .run_stream(
            vec![Message::user("use the tool")],
            None,
            Some(Box::new(|_| {})),
        )
        .await
        .expect("agent run");

    assert!(!result.interrupted);
    let calls = provider.calls.lock().expect("calls lock").clone();
    assert_eq!(calls.len(), 2);
    let tool_message = calls[1]
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .expect("tool result in the second stream");
    let content = tool_message.content.as_deref().expect("tool content");
    assert!(content.starts_with("tool output"));
    assert!(content.ends_with(&crate::steer::format_steer_marker("prefer the simpler fix")));
    assert_eq!(interrupt.take_steer(), None);
}

fn rollback_registry() -> ToolRegistry {
    use hermes_core::JsonSchema;
    let mut registry = ToolRegistry::new();
    registry.register(
        "plain_tool",
        ToolSchema::new("plain_tool", "Plain tool", JsonSchema::new("object")),
        Arc::new(|_args| Ok("tool output".to_string())),
    );
    registry.register(
        "failing_tool",
        ToolSchema::new("failing_tool", "Failing tool", JsonSchema::new("object")),
        Arc::new(|_args| Err(hermes_core::ToolError::ExecutionFailed("boom".to_string()))),
    );
    registry
}

/// Whether the steer survived an auto-rollback: delivered again after it, or
/// still pending.
fn steer_outlived_the_rollback(
    calls: &[Vec<Message>],
    result: &AgentResult,
    interrupt: &InterruptController,
) -> bool {
    let marker = crate::steer::format_steer_marker("prefer the simpler fix");
    let has_marker = |msgs: &[Message]| {
        msgs.iter()
            .any(|m| m.content.as_deref().is_some_and(|c| c.contains(&marker)))
    };
    assert!(calls.len() >= 3, "expected a call after the rollback");
    assert!(has_marker(&calls[1]), "the steer reached call 2");
    assert!(
        calls[2].iter().any(|m| m.role == MessageRole::System
            && m.content.as_deref().is_some_and(|c| c.contains("Auto-rollback"))),
        "the rollback ran before call 3"
    );
    calls[2..].iter().any(|c| has_marker(c))
        || has_marker(&result.messages)
        || interrupt.take_steer().is_some()
}

// A steer delivered into a tool result, then an auto-rollback (defaults:
// checkpoint every 3 turns, rollback at 3 failed calls in one batch) restores
// a snapshot taken before delivery. From the fresh-context review.
#[tokio::test]
async fn a_steer_delivered_before_a_rollback_is_not_lost() {
    use futures::stream::BoxStream;
    use hermes_core::FunctionCall;

    struct RollbackProvider {
        interrupt: InterruptController,
        calls: std::sync::Mutex<Vec<Vec<Message>>>,
    }

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
            extra_content: None,
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for RollbackProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let mut calls = self.calls.lock().expect("calls lock");
            calls.push(messages.to_vec());
            let message = match calls.len() {
                1 => {
                    self.interrupt.steer("prefer the simpler fix");
                    Message::assistant_with_tool_calls(None, vec![call("c1", "plain_tool", "{}")])
                }
                2 => Message::assistant_with_tool_calls(
                    None,
                    vec![
                        call("c2", "failing_tool", "{\"n\":1}"),
                        call("c3", "failing_tool", "{\"n\":2}"),
                        call("c4", "failing_tool", "{\"n\":3}"),
                    ],
                ),
                _ => Message::assistant("Done: used the tool; src/main.rs is fixed"),
            };
            Ok(hermes_core::LlmResponse {
                message,
                usage: None,
                model: "dummy".into(),
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
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    let interrupt = InterruptController::new();
    let provider = Arc::new(RollbackProvider {
        interrupt: interrupt.clone(),
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let config = AgentConfig {
        skip_memory: true,
        skip_context_files: true,
        ..AgentConfig::default()
    };
    assert_eq!(config.checkpoint_interval_turns, 3);
    assert_eq!(config.rollback_on_tool_error_threshold, 3);
    let agent = AgentLoop::with_interrupt(
        config,
        Arc::new(rollback_registry()),
        provider.clone(),
        interrupt.clone(),
    );

    let result = agent
        .run(vec![Message::user("use plain_tool to fix src/main.rs")], None)
        .await
        .expect("agent run");

    let calls = provider.calls.lock().expect("calls lock").clone();
    assert!(
        steer_outlived_the_rollback(&calls, &result, &interrupt),
        "the steer vanished: the rollback restored a snapshot taken before it was delivered"
    );
}

#[tokio::test]
async fn a_streamed_steer_delivered_before_a_rollback_is_not_lost() {
    use futures::stream::BoxStream;
    use hermes_core::{FunctionCallDelta, StreamDelta, ToolCallDelta};

    struct RollbackStream {
        interrupt: InterruptController,
        calls: std::sync::Mutex<Vec<Vec<Message>>>,
    }

    fn call(index: u32, name: &str, args: &str) -> ToolCallDelta {
        ToolCallDelta {
            index,
            id: Some(format!("c{index}")),
            function: Some(FunctionCallDelta {
                name: Some(name.to_string()),
                arguments: Some(args.to_string()),
            }),
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for RollbackStream {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            Err(AgentError::LlmApi("this run streams".to_string()))
        }

        fn chat_completion_stream(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            let mut calls = self.calls.lock().expect("calls lock");
            calls.push(messages.to_vec());
            let (delta, finish) = match calls.len() {
                1 => {
                    self.interrupt.steer("prefer the simpler fix");
                    (
                        StreamDelta {
                            content: None,
                            tool_calls: Some(vec![call(0, "plain_tool", "{}")]),
                            extra: None,
                        },
                        "tool_calls",
                    )
                }
                2 => (
                    StreamDelta {
                        content: None,
                        tool_calls: Some(vec![
                            call(1, "failing_tool", "{\"n\":1}"),
                            call(2, "failing_tool", "{\"n\":2}"),
                            call(3, "failing_tool", "{\"n\":3}"),
                        ]),
                        extra: None,
                    },
                    "tool_calls",
                ),
                _ => (
                    StreamDelta {
                        content: Some("Done: used the tool; src/main.rs is fixed".to_string()),
                        tool_calls: None,
                        extra: None,
                    },
                    "stop",
                ),
            };
            futures::stream::iter(vec![Ok(StreamChunk {
                delta: Some(delta),
                finish_reason: Some(finish.to_string()),
                usage: None,
            })])
            .boxed()
        }
    }

    let interrupt = InterruptController::new();
    let provider = Arc::new(RollbackStream {
        interrupt: interrupt.clone(),
        calls: std::sync::Mutex::new(Vec::new()),
    });
    let config = AgentConfig {
        skip_memory: true,
        skip_context_files: true,
        ..AgentConfig::default()
    };
    assert_eq!(config.checkpoint_interval_turns, 3);
    assert_eq!(config.rollback_on_tool_error_threshold, 3);
    let agent = AgentLoop::with_interrupt(
        config,
        Arc::new(rollback_registry()),
        provider.clone(),
        interrupt.clone(),
    );

    let result = agent
        .run_stream(
            vec![Message::user("use plain_tool to fix src/main.rs")],
            None,
            Some(Box::new(|_| {})),
        )
        .await
        .expect("agent run");

    let calls = provider.calls.lock().expect("calls lock").clone();
    assert!(
        steer_outlived_the_rollback(&calls, &result, &interrupt),
        "the steer vanished: the rollback restored a snapshot taken before it was delivered"
    );
}

#[test]
fn test_smart_model_routing_cheap_route_for_simple_turn() {
    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "openai".to_string(),
        RuntimeProviderConfig {
            api_key: Some("sk-test-key".to_string()),
            api_key_env: None,
            base_url: None,
            request_timeout_seconds: None,
            api_mode: None,
            command: None,
            args: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
        },
    );

    let mut config = AgentConfig {
        model: "openai:gpt-4o".to_string(),
        runtime_providers,
        smart_model_routing: SmartModelRoutingConfig {
            enabled: true,
            max_simple_chars: 160,
            max_simple_words: 28,
            cheap_model: Some(CheapModelRouteConfig {
                provider: Some("openai".to_string()),
                model: Some("gpt-4o-mini".to_string()),
                base_url: None,
                api_key_env: None,
            }),
        },
        ..AgentConfig::default()
    };
    let _home = isolate_route_learning_home(&mut config);
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider),
    );
    let messages = vec![Message::user("帮我总结一下今天要做什么")];
    let selected = agent.resolve_smart_runtime_route(&messages);
    assert_eq!(
        selected.as_ref().map(|r| r.model.as_str()),
        Some("gpt-4o-mini")
    );
}

#[test]
fn test_smart_model_routing_online_learning_prefers_primary_when_cheap_unstable() {
    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "openai".to_string(),
        RuntimeProviderConfig {
            api_key: Some("sk-test-key".to_string()),
            api_key_env: None,
            base_url: None,
            request_timeout_seconds: None,
            api_mode: None,
            command: None,
            args: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
        },
    );
    let mut config = AgentConfig {
        model: "openai:gpt-4o".to_string(),
        runtime_providers,
        smart_model_routing: SmartModelRoutingConfig {
            enabled: true,
            max_simple_chars: 160,
            max_simple_words: 28,
            cheap_model: Some(CheapModelRouteConfig {
                provider: Some("openai".to_string()),
                model: Some("gpt-4o-mini".to_string()),
                base_url: None,
                api_key_env: None,
            }),
        },
        ..AgentConfig::default()
    };
    let _home = isolate_route_learning_home(&mut config);
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider),
    );
    if let Ok(mut m) = agent.route_learning.lock() {
        m.insert(
            "openai:gpt-4o".to_string(),
            RouteLearningStats {
                samples: 12,
                success_rate: 0.98,
                avg_latency_ms: 900.0,
                consecutive_failures: 0,
                updated_at_unix_ms: now_unix_ms(),
            },
        );
        m.insert(
            "openai:gpt-4o-mini".to_string(),
            RouteLearningStats {
                samples: 12,
                success_rate: 0.35,
                avg_latency_ms: 3800.0,
                consecutive_failures: 3,
                updated_at_unix_ms: now_unix_ms(),
            },
        );
    }
    let selected =
        agent.resolve_smart_runtime_route(&[Message::user("summarize today's work")]);
    assert!(
        selected.is_none(),
        "online learning should keep primary when cheap route is unstable"
    );
}

#[test]
fn test_route_learning_updates_after_outcomes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = AgentConfig {
        hermes_home: Some(tmp.path().to_string_lossy().to_string()),
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider));
    agent.update_route_learning(None, Some("openai:gpt-4o"), 2100, false);
    agent.update_route_learning(None, Some("openai:gpt-4o"), 900, true);
    let snapshot = agent.route_learning_snapshot(None, Some("openai:gpt-4o"));
    assert_eq!(snapshot["enabled"], true);
    assert_eq!(snapshot["key"], "openai:gpt-4o");
    assert_eq!(snapshot["stats"]["samples"], 2);
    assert!(snapshot["stats"]["success_rate"].as_f64().unwrap_or(0.0) > 0.0);
    assert!(snapshot["stats"]["avg_latency_ms"].as_f64().unwrap_or(0.0) > 0.0);
}

#[test]
fn test_route_learning_persists_across_agent_restarts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = AgentConfig {
        hermes_home: Some(tmp.path().to_string_lossy().to_string()),
        ..AgentConfig::default()
    };

    let agent = AgentLoop::new(
        cfg.clone(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider),
    );
    let persisted_path = route_learning_state_path(&cfg);
    std::fs::create_dir_all(&persisted_path).expect("block writing the state file");
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        agent.update_route_learning(None, Some("openai:gpt-4o"), 1200, true);
    });
    assert!(persisted_path.is_dir());
    let in_memory = agent.route_learning_snapshot(None, Some("openai:gpt-4o"));
    assert_eq!(in_memory["stats"]["samples"], 1);

    std::fs::remove_dir(&persisted_path).expect("unblock writing the state file");
    agent.update_route_learning(None, Some("openai:gpt-4o"), 1400, true);
    assert!(
        persisted_path.exists(),
        "route-learning state file must exist"
    );

    let reloaded = AgentLoop::new(
        cfg.clone(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider),
    );
    let snapshot = reloaded.route_learning_snapshot(None, Some("openai:gpt-4o"));
    assert_eq!(snapshot["key"], "openai:gpt-4o");
    assert_eq!(snapshot["stats"]["samples"], 2);
}

#[test]
fn test_route_learning_malformed_file_is_safe_fallback() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = AgentConfig {
        hermes_home: Some(tmp.path().to_string_lossy().to_string()),
        ..AgentConfig::default()
    };
    let state_path = route_learning_state_path(&cfg);
    std::fs::create_dir_all(
        state_path
            .parent()
            .expect("route-learning path should have a parent"),
    )
    .expect("create route-learning dir");
    std::fs::write(&state_path, "{ this-is-invalid-json")
        .expect("write malformed route-learning file");

    let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider));
    let snapshot = agent.route_learning_snapshot(None, Some("openai:gpt-4o"));
    assert!(
        snapshot["stats"].is_null(),
        "malformed state must fall back cleanly"
    );
}

#[cfg(unix)]
#[test]
fn test_route_learning_writes_through_a_symlinked_state_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = AgentConfig {
        hermes_home: Some(tmp.path().join("home").to_string_lossy().to_string()),
        ..AgentConfig::default()
    };
    let link = route_learning_state_path(&cfg);
    let real = tmp.path().join("dotfiles").join("route-learning.json");
    std::fs::create_dir_all(link.parent().expect("parent")).expect("logs dir");
    std::fs::create_dir_all(real.parent().expect("parent")).expect("dotfiles dir");
    std::os::unix::fs::symlink(&real, &link).expect("link");

    let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider));
    agent.update_route_learning(None, Some("openai:gpt-4o"), 1200, true);

    assert!(link.is_symlink(), "the state link was replaced");
    let saved = std::fs::read_to_string(&real).expect("target written");
    assert!(saved.contains("openai:gpt-4o"));
}
