//! The embedding facade: a host program builds an agent from a provider and its own tools, and
//! the agent sees only what the host gives it.

use std::sync::{Arc, Mutex};

use futures::stream::BoxStream;
use hermes_agent::embed::Agent;
use hermes_core::{
    AgentError, FunctionCall, JsonSchema, LlmProvider, LlmResponse, Message, StreamChunk, ToolCall,
    ToolSchema,
};
use serde_json::{json, Value};

/// Calls `add` once, then answers with the tool's result. Keeps every request's messages.
#[derive(Default)]
struct Scripted {
    requests: Mutex<Vec<Vec<Message>>>,
}

impl Scripted {
    fn system_prompts(&self) -> String {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .filter(|m| m.role == hermes_core::MessageRole::System)
            .filter_map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait::async_trait]
impl LlmProvider for Scripted {
    async fn chat_completion(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(messages.to_vec());
        let message = match messages.iter().rev().find(|m| m.tool_call_id.is_some()) {
            Some(result) => Message::assistant(format!(
                "the sum is {}",
                result.content.as_deref().unwrap_or_default()
            )),
            None => Message::assistant_with_tool_calls(
                None,
                vec![ToolCall {
                    id: "call-1".into(),
                    function: FunctionCall {
                        name: "add".into(),
                        arguments: r#"{"a":2,"b":3}"#.into(),
                    },
                    extra_content: None,
                }],
            ),
        };
        Ok(LlmResponse {
            message,
            usage: None,
            model: "scripted".into(),
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
    ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
        Box::pin(futures::stream::empty())
    }
}

fn add_schema() -> ToolSchema {
    let mut params = JsonSchema::new("object");
    params.properties = Some(
        [
            ("a".to_string(), json!({"type": "integer"})),
            ("b".to_string(), json!({"type": "integer"})),
        ]
        .into_iter()
        .collect(),
    );
    ToolSchema::new("add", "Add two integers", params)
}

fn add(args: Value) -> Result<String, hermes_core::ToolError> {
    Ok((args["a"].as_i64().unwrap_or(0) + args["b"].as_i64().unwrap_or(0)).to_string())
}

/// The tests below change the process's environment and working folder.
static ENV: Mutex<()> = Mutex::new(());

/// A Hermes home and a workspace holding a soul, memories, a skill and an AGENTS.md, each
/// carrying a marker the prompt must not show unless the host asks for that home.
struct UserState {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
}

fn user_state() -> UserState {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("SOUL.md"),
        "SOUL-MARKER: you are the user's agent",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("memories")).unwrap();
    std::fs::write(home.path().join("memories/MEMORY.md"), "MEMORY-MARKER").unwrap();
    std::fs::write(home.path().join("memories/USER.md"), "USER-MARKER").unwrap();
    let skill = home.path().join("skills/marker-skill");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: marker-skill\ndescription: SKILL-MARKER\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(workspace.path().join("AGENTS.md"), "AGENTS-MARKER").unwrap();
    UserState { home, workspace }
}

fn listing(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).unwrap().flatten() {
            let path = entry.path();
            out.push(path.strip_prefix(dir).unwrap().display().to_string());
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn an_embedded_agent_answers_through_the_hosts_tool() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let provider = Arc::new(Scripted::default());
    let agent = Agent::builder(provider.clone())
        .system_prompt("You add numbers.")
        .tool(add_schema(), add)
        .build()
        .unwrap();

    let reply = agent.ask("What is 2 + 3?").await.unwrap();

    assert_eq!(reply.text, "the sum is 5");
    assert_eq!(reply.result.total_turns, 2);
    assert!(provider.system_prompts().contains("You add numbers."));
}

#[tokio::test]
async fn an_embedded_agent_reads_and_writes_nothing_of_the_users() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let state = user_state();
    let home_before = listing(state.home.path());
    let workspace_before = listing(state.workspace.path());
    let old_cwd = std::env::current_dir().unwrap();
    // The ENV lock serialises every test in this binary that touches the environment.
    std::env::set_var("HERMES_HOME", state.home.path());
    std::env::set_var("TERMINAL_CWD", state.workspace.path());
    std::env::set_current_dir(state.workspace.path()).unwrap();

    let provider = Arc::new(Scripted::default());
    let agent = Agent::builder(provider.clone())
        .tool(add_schema(), add)
        .build()
        .unwrap();
    let reply = agent.ask("What is 2 + 3?").await;
    let scratch = agent.home().to_path_buf();
    drop(agent);

    std::env::set_current_dir(old_cwd).unwrap();
    std::env::remove_var("HERMES_HOME");
    std::env::remove_var("TERMINAL_CWD");
    assert_eq!(reply.unwrap().text, "the sum is 5");
    let prompt = provider.system_prompts();
    for marker in [
        "SOUL-MARKER",
        "MEMORY-MARKER",
        "USER-MARKER",
        "SKILL-MARKER",
        "AGENTS-MARKER",
    ] {
        assert!(!prompt.contains(marker), "{marker} reached the prompt");
    }
    assert_eq!(listing(state.home.path()), home_before);
    assert_eq!(listing(state.workspace.path()), workspace_before);
    assert!(!scratch.exists(), "the agent's own home outlived it");
}

#[tokio::test]
async fn a_host_that_names_a_home_gets_that_homes_soul() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let state = user_state();
    let provider = Arc::new(Scripted::default());
    let agent = Agent::builder(provider.clone())
        .home(state.home.path())
        .build()
        .unwrap();

    agent.ask("What is 2 + 3?").await.unwrap();
    drop(agent);

    assert!(provider.system_prompts().contains("SOUL-MARKER"));
    assert!(
        state.home.path().join("SOUL.md").exists(),
        "a named home is the host's to keep"
    );
}
