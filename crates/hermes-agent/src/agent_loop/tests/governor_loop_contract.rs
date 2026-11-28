use std::sync::Mutex;

use super::*;
use hermes_core::{FunctionCall, FunctionCallDelta, JsonSchema, StreamDelta, ToolCallDelta};

const TOOL: &str = "fixture_failure";
const HOUSEKEEPING_TOOL: &str = "skill_manage";
const SUMMARY: &str = "Fixture failed twice; inspect the test result.";
const RECOVERED: &str = "The backup model finished the fixture.";
const BACKUP_MODEL: &str = "fixture-backup";

#[derive(Debug, PartialEq, Eq)]
enum GuardEvent {
    Mute(bool),
    ToolFailure,
    Summary,
}

#[derive(Default)]
struct GuardProvider {
    complete_turns: std::sync::atomic::AtomicUsize,
    stream_turns: std::sync::atomic::AtomicUsize,
    summaries: std::sync::atomic::AtomicUsize,
    housekeeping: bool,
    events: Arc<Mutex<Vec<GuardEvent>>>,
}

impl GuardProvider {
    fn call(&self, index: usize) -> ToolCall {
        ToolCall {
            id: format!("fixture-{index}"),
            function: FunctionCall {
                name: if self.housekeeping {
                    HOUSEKEEPING_TOOL
                } else {
                    TOOL
                }
                .into(),
                arguments: "{}".into(),
            },
            extra_content: None,
        }
    }
}

#[tokio::test]
async fn interrupt_preserves_only_answered_tool_calls() {
    for (streaming, interrupt_before_tool) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(GuardProvider::default());
        let interrupt = InterruptController::new();
        let stop = interrupt.clone();
        let tool_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded_runs = tool_runs.clone();
        let schema = ToolSchema::new(TOOL, "Fixture tool", JsonSchema::new("object"));
        let mut tools = ToolRegistry::new();
        tools.register(
            TOOL,
            schema.clone(),
            Arc::new(move |_| {
                recorded_runs.fetch_add(1, Ordering::Relaxed);
                Ok("fixture completed".into())
            }),
        );

        let mut callbacks = AgentCallbacks::default();
        if interrupt_before_tool {
            callbacks.on_tool_start = Some(Box::new(move |name, args| {
                assert_eq!(name, TOOL);
                assert_eq!(args, &serde_json::json!({}));
                stop.interrupt(None);
            }));
        } else {
            callbacks.on_tool_complete = Some(Box::new(move |name, result| {
                assert_eq!(name, TOOL);
                assert_eq!(result, "fixture completed");
                stop.interrupt(None);
            }));
        }
        let agent = AgentLoop::with_interrupt(
            AgentConfig {
                model: "openai:fixture-model".into(),
                hermes_home: Some(directory.path().display().to_string()),
                max_turns: 3,
                skip_memory: true,
                skip_context_files: true,
                code_index_enabled: false,
                ..Default::default()
            },
            Arc::new(tools),
            provider.clone(),
            interrupt,
        )
        .with_callbacks(callbacks);

        let messages = vec![Message::user("Try the fixture tool.")];
        let schemas = Some(vec![schema]);
        let result = if streaming {
            agent
                .run_stream(messages, schemas, Some(Box::new(|_| {})))
                .await
        } else {
            agent.run(messages, schemas).await
        }
        .unwrap();
        assert!(result.interrupted);
        assert!(!result.finished_naturally);
        assert_eq!(
            provider.stream_turns.load(Ordering::Relaxed),
            usize::from(streaming)
        );
        assert_eq!(
            provider.complete_turns.load(Ordering::Relaxed),
            usize::from(!streaming)
        );
        assert_eq!(
            tool_runs.load(Ordering::Relaxed),
            usize::from(!interrupt_before_tool)
        );

        let calls: Vec<_> = result
            .messages
            .iter()
            .flat_map(|message| message.tool_calls.iter().flatten())
            .map(|call| call.id.as_str())
            .collect();
        let answers: Vec<_> = result
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Tool)
            .map(|message| message.tool_call_id.as_deref())
            .collect();
        let expected: &[&str] = if interrupt_before_tool {
            &[]
        } else {
            &["fixture-0"]
        };
        assert_eq!(calls, expected);
        let expected_answers: Vec<_> = expected.iter().copied().map(Some).collect();
        assert_eq!(answers, expected_answers);
    }
}

#[async_trait::async_trait]
impl LlmProvider for GuardProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        let (message, reason) = if tools.is_empty() {
            assert!(messages.iter().any(|message| {
                message.role == MessageRole::System
                    && message.content.as_deref().is_some_and(|text| {
                        text.contains("Tool-loop guard triggered after 2 consecutive error turn(s)")
                    })
            }));
            self.events.lock().unwrap().push(GuardEvent::Summary);
            self.summaries.fetch_add(1, Ordering::Relaxed);
            (Message::assistant(SUMMARY), "stop")
        } else {
            let index = self.complete_turns.fetch_add(1, Ordering::Relaxed);
            (
                Message::assistant_with_tool_calls(None, vec![self.call(index)]),
                "tool_calls",
            )
        };
        Ok(LlmResponse {
            message,
            usage: None,
            model: "fixture-model".into(),
            finish_reason: Some(reason.into()),
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
        let index = self.stream_turns.fetch_add(1, Ordering::Relaxed);
        let call = self.call(index);
        Box::pin(futures::stream::iter([Ok(StreamChunk {
            delta: Some(StreamDelta {
                tool_calls: Some(vec![ToolCallDelta {
                    index: 0,
                    id: Some(call.id),
                    function: Some(FunctionCallDelta {
                        name: Some(call.function.name),
                        arguments: Some(call.function.arguments),
                    }),
                }]),
                content: self
                    .housekeeping
                    .then(|| "Checking the fixture skill.".into()),
                extra: None,
            }),
            finish_reason: Some("tool_calls".into()),
            usage: None,
        })]))
    }
}

async fn enter_guard_test(test: &str, max_consecutive_errors: &str) -> bool {
    const CHILD: &str = "REFORMED_TEST_TOOL_GUARD";
    if std::env::var(CHILD).is_ok_and(|child| child == test) {
        return true;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env(CHILD, test)
        .env("HOME", directory.path())
        .env("USERPROFILE", directory.path())
        .env("HERMES_HOME", directory.path())
        .env(
            "HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS",
            max_consecutive_errors,
        )
        .env("HERMES_PERF_GOV_LATENCY_WARN_MS", "1000000")
        .env("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", "1000000")
        .current_dir(directory.path())
        .kill_on_drop(true)
        .args(["--exact", test]);
    for key in ["LLVM_PROFILE_FILE", "SystemRoot"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let output = tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .expect("tool guard child timed out")
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(&format!("test {test} ... ok")),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

#[tokio::test]
async fn tool_loop_guard_reports_repeated_failures_in_complete_and_streamed_turns() {
    if !enter_guard_test(
        "agent_loop::guard_contract_tests::tool_loop_guard_reports_repeated_failures_in_complete_and_streamed_turns",
        "2",
    )
    .await
    {
        return;
    }

    for (streaming, housekeeping) in [(false, false), (true, false), (true, true)] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(GuardProvider {
            housekeeping,
            ..GuardProvider::default()
        });
        let tool = if housekeeping {
            HOUSEKEEPING_TOOL
        } else {
            TOOL
        };
        let schema = ToolSchema::new(
            tool,
            "Fail the synthetic tool call",
            JsonSchema::new("object"),
        );
        let mut tools = ToolRegistry::new();
        let events = provider.events.clone();
        tools.register(
            tool,
            schema.clone(),
            Arc::new(move |_| {
                events.lock().unwrap().push(GuardEvent::ToolFailure);
                Err(ToolError::ExecutionFailed("fixture tool failure".into()))
            }),
        );
        let agent = AgentLoop::new(
            AgentConfig {
                model: "openai:fixture-model".into(),
                hermes_home: Some(directory.path().display().to_string()),
                max_turns: 6,
                skip_memory: true,
                skip_context_files: true,
                code_index_enabled: false,
                ..Default::default()
            },
            Arc::new(tools),
            provider.clone(),
        );
        let messages = vec![Message::user("Try the fixture tool.")];
        let schemas = Some(vec![schema]);
        let result = if streaming {
            let events = provider.events.clone();
            agent
                .run_stream(
                    messages,
                    schemas,
                    Some(Box::new(move |chunk| {
                        if let Some(control) = chunk
                            .delta
                            .and_then(|delta| delta.extra)
                            .filter(|extra| extra["control"] == "mute_post_response")
                        {
                            events
                                .lock()
                                .unwrap()
                                .push(GuardEvent::Mute(control["enabled"].as_bool().unwrap()));
                        }
                    })),
                )
                .await
        } else {
            agent.run(messages, schemas).await
        }
        .unwrap();
        assert!(!result.finished_naturally);
        assert!(!result.interrupted);
        assert_eq!(result.total_turns, 2);
        assert_eq!(result.tool_errors.len(), 2);
        assert!(result.tool_errors.iter().all(|error| {
            error.tool_name == tool && error.error.contains("fixture tool failure")
        }));
        assert_eq!(
            result.messages.last().unwrap().content.as_deref(),
            Some(SUMMARY)
        );
        assert_eq!(
            provider.complete_turns.load(Ordering::Relaxed),
            if streaming { 0 } else { 2 },
        );
        assert_eq!(
            provider.stream_turns.load(Ordering::Relaxed),
            if streaming { 2 } else { 0 },
        );
        assert_eq!(provider.summaries.load(Ordering::Relaxed), 1);
        use GuardEvent::{Mute, Summary, ToolFailure};
        let expected: &[GuardEvent] = if housekeeping {
            &[Mute(true), ToolFailure, ToolFailure, Summary, Mute(false)]
        } else {
            &[ToolFailure, ToolFailure, Summary]
        };
        assert_eq!(provider.events.lock().unwrap().as_slice(), expected);
    }
}

#[tokio::test]
async fn reliability_guard_routes_both_loops_to_backup_after_tool_failures() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    if !enter_guard_test(
        "agent_loop::guard_contract_tests::reliability_guard_routes_both_loops_to_backup_after_tool_failures",
        "4",
    )
    .await
    {
        return;
    }

    for streaming in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(GuardProvider::default());
        let server = MockServer::start().await;
        let reply = if streaming {
            let chunk = serde_json::json!({
                "choices": [{"index": 0, "delta": {"content": RECOVERED}, "finish_reason": "stop"}]
            });
            ResponseTemplate::new(200).set_body_raw(
                format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                "text/event-stream",
            )
        } else {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": BACKUP_MODEL,
                "choices": [{
                    "index": 0, "message": {"role": "assistant", "content": RECOVERED},
                    "finish_reason": "stop"
                }]
            }))
        };
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(reply)
            .expect(1)
            .mount(&server)
            .await;
        let schema = ToolSchema::new(TOOL, "Fail the fixture tool", JsonSchema::new("object"));
        let mut tools = ToolRegistry::new();
        tools.register(
            TOOL,
            schema.clone(),
            Arc::new(|_| Err(ToolError::ExecutionFailed("fixture tool failure".into()))),
        );
        let mut config = AgentConfig {
            model: "openai:fixture-model".into(),
            hermes_home: Some(directory.path().display().to_string()),
            max_turns: 6,
            skip_memory: true,
            skip_context_files: true,
            code_index_enabled: false,
            runtime_providers: std::collections::HashMap::from([(
                "openai".into(),
                RuntimeProviderConfig {
                    base_url: Some(format!("{}/v1", server.uri())),
                    request_timeout_seconds: Some(5.0),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        config.retry.fallback_model = Some(format!("openai:{BACKUP_MODEL}"));
        let notices = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&notices);
        let agent = AgentLoop::new(config, Arc::new(tools), provider.clone()).with_callbacks(
            AgentCallbacks {
                status_callback: Some(Arc::new(move |kind, message| {
                    if kind == "lifecycle" && message.starts_with("Reliability guard switching") {
                        recorded.lock().unwrap().push(message.to_owned());
                    }
                })),
                ..Default::default()
            },
        );
        let input = vec![Message::user("Try the fixture tool.")];
        let result = if streaming {
            agent
                .run_stream(input, Some(vec![schema]), Some(Box::new(|_| {})))
                .await
        } else {
            agent.run(input, Some(vec![schema])).await
        }
        .unwrap();
        assert!(result.finished_naturally && !result.interrupted);
        assert_eq!(result.total_turns, 4);
        assert_eq!(result.tool_errors.len(), 3);
        assert!(result.tool_errors.iter().all(|error| {
            error.tool_name == TOOL && error.error.contains("fixture tool failure")
        }));
        assert_eq!(
            provider.complete_turns.load(Ordering::Relaxed),
            if streaming { 0 } else { 3 }
        );
        assert_eq!(
            provider.stream_turns.load(Ordering::Relaxed),
            if streaming { 3 } else { 0 }
        );
        assert_eq!(provider.summaries.load(Ordering::Relaxed), 0);
        let requests = server.received_requests().await.unwrap();
        let [request] = requests.as_slice() else {
            panic!(
                "expected a single backup request, received {}",
                requests.len()
            );
        };
        let body: Value = request.body_json().unwrap();
        assert_eq!(body["model"], BACKUP_MODEL);
        assert_eq!(body["stream"].as_bool().unwrap_or(false), streaming);
        assert_eq!(
            notices.lock().unwrap().as_slice(),
            [format!(
                "Reliability guard switching route to `openai:{BACKUP_MODEL}` after degradation."
            )]
        );
        let last = result.messages.last().unwrap();
        assert_eq!(last.role, MessageRole::Assistant);
        assert_eq!(last.content.as_deref(), Some(RECOVERED));
    }
}
