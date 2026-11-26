//! One agent-loop turn end to end: the model reads a file, the agent's result
//! budget cuts what it is shown, and its next `write_file` of that file is
//! refused. With the whole file shown, the same write lands.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::stream::BoxStream;
use hermes_agent::agent_loop::ToolRegistry as AgentToolRegistry;
use hermes_agent::{AgentConfig, AgentLoop};
use hermes_core::{
    AgentError, BudgetConfig, FunctionCall, LlmProvider, LlmResponse, Message, StreamChunk,
    ToolCall, ToolHandler, ToolSchema,
};
use hermes_environments::local::LocalBackend;
use hermes_tools::registry::ToolRegistry;
use hermes_tools::tools::file::{ReadFileHandler, WriteFileHandler};
use serde_json::json;

/// Reads `path`, then writes it, then stops.
struct ReadThenWrite {
    path: PathBuf,
    turn: AtomicUsize,
}

fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> LlmResponse {
    LlmResponse {
        message: Message::assistant_with_tool_calls(
            None,
            vec![ToolCall {
                id: id.to_string(),
                function: FunctionCall {
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                },
                extra_content: None,
            }],
        ),
        usage: None,
        model: "mock:model".to_string(),
        finish_reason: Some("tool_calls".to_string()),
    }
}

#[async_trait::async_trait]
impl LlmProvider for ReadThenWrite {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        let path = self.path.to_str().unwrap();
        Ok(match self.turn.fetch_add(1, Ordering::SeqCst) {
            0 => tool_call("agent-read", "read_file", json!({ "path": path })),
            1 => tool_call(
                "agent-write",
                "write_file",
                json!({ "path": path, "content": "replacement\n" }),
            ),
            _ => LlmResponse {
                message: Message::assistant("done"),
                usage: None,
                model: "mock:model".to_string(),
                finish_reason: Some("stop".to_string()),
            },
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
        Box::pin(futures::stream::empty())
    }
}

/// The file tools as the agent sees them, through the tool registry.
fn agent_tools() -> AgentToolRegistry {
    let backend: Arc<dyn hermes_core::TerminalBackend> = Arc::new(LocalBackend::new(60, 1 << 20));
    let tools = ToolRegistry::new();
    let handlers: [Arc<dyn ToolHandler>; 2] = [
        Arc::new(ReadFileHandler::new(backend.clone())),
        Arc::new(WriteFileHandler::new(backend)),
    ];
    let mut agent = AgentToolRegistry::new();
    for handler in handlers {
        let schema = handler.schema();
        let name = schema.name.clone();
        tools.register(
            name.clone(),
            "file",
            schema.clone(),
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "",
            "",
            None,
        );
        let tools = tools.clone();
        let tool = name.clone();
        agent.register(
            name,
            schema,
            Arc::new(move |params| Ok(tools.dispatch(&tool, params))),
        );
    }
    agent
}

/// Runs one session against `path` with results cut to `max_result_size_chars`;
/// returns the `write_file` result as the model saw it.
async fn read_then_write(path: &Path, max_result_size_chars: usize) -> String {
    let provider = Arc::new(ReadThenWrite {
        path: path.to_path_buf(),
        turn: AtomicUsize::new(0),
    });
    let config = AgentConfig {
        max_turns: 5,
        session_id: Some(format!("e2e-{max_result_size_chars}")),
        budget: BudgetConfig {
            max_result_size_chars,
            ..BudgetConfig::default()
        },
        ..AgentConfig::default()
    };
    let result = AgentLoop::new(config, Arc::new(agent_tools()), provider)
        .run(vec![Message::user("rewrite the file")], None)
        .await
        .expect("agent loop should succeed");
    result
        .messages
        .iter()
        .find(|m| m.tool_call_id.as_deref() == Some("agent-write"))
        .and_then(|m| m.content.clone())
        .expect("write_file result")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_the_budget_cuts_does_not_bless_the_next_write() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("HERMES_HOME", home.path());
    let content = "a line long enough for the result budget to cut it short\n";

    let cut = home.path().join("cut.txt");
    std::fs::write(&cut, content).unwrap();
    let refused = read_then_write(&cut, 20).await;
    assert!(refused.contains("Refusing to overwrite"), "{refused}");
    assert_eq!(std::fs::read_to_string(&cut).unwrap(), content);

    let whole = home.path().join("whole.txt");
    std::fs::write(&whole, content).unwrap();
    let written = read_then_write(&whole, 100_000).await;
    assert!(!written.contains("Refusing"), "{written}");
    assert_eq!(std::fs::read_to_string(&whole).unwrap(), "replacement\n");
}
