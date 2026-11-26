//! The embedding facade: a host program builds an agent from a provider and its own tools, and
//! the agent sees only what the host gives it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::stream::BoxStream;
use hermes_agent::embed::{Agent, Ending};
use hermes_core::{
    AgentError, FunctionCall, JsonSchema, LlmProvider, LlmResponse, Message, StreamChunk, ToolCall,
    ToolSchema,
};
use serde_json::{json, Value};

/// Calls `add` once, then answers with the tool's result; or, given an answer, always gives it.
/// Keeps every request's messages.
#[derive(Default)]
struct Scripted {
    answer: Option<&'static str>,
    requests: Mutex<Vec<Vec<Message>>>,
}

impl Scripted {
    fn answering(answer: &'static str) -> Self {
        Self {
            answer: Some(answer),
            ..Self::default()
        }
    }

    fn system_prompts(&self) -> String {
        self.texts(|m| m.role == hermes_core::MessageRole::System)
    }

    /// Every message of every request, joined.
    fn everything_sent(&self) -> String {
        self.texts(|_| true)
    }

    fn texts(&self, keep: impl Fn(&Message) -> bool) -> String {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .filter(|m| keep(m))
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
        let tool_result = messages.iter().rev().find(|m| m.tool_call_id.is_some());
        let message = match (self.answer, tool_result) {
            (Some(answer), _) => Message::assistant(answer),
            (None, Some(result)) => Message::assistant(format!(
                "the sum is {}",
                result.content.as_deref().unwrap_or_default()
            )),
            (None, None) => Message::assistant_with_tool_calls(
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

/// Runs a future to completion. The tests run synchronously so that the environment lock below
/// is never held across an await.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// The tests below change the process's environment and working folder.
static ENV: Mutex<()> = Mutex::new(());

/// Holds the lock and puts the environment and working folder back when dropped, even when the
/// test panics.
struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    cwd: PathBuf,
    vars: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn new() -> Self {
        Self {
            _lock: ENV.lock().unwrap_or_else(|e| e.into_inner()),
            cwd: std::env::current_dir().unwrap(),
            vars: Vec::new(),
        }
    }

    fn set(&mut self, name: &'static str, value: impl AsRef<std::ffi::OsStr>) {
        if !self.vars.iter().any(|(n, _)| *n == name) {
            self.vars.push((name, std::env::var_os(name)));
        }
        std::env::set_var(name, value);
    }

    /// Point `HERMES_HOME` and the working folder at `state`.
    fn enter(&mut self, state: &UserState) {
        self.set("HERMES_HOME", state.home.path());
        self.set("TERMINAL_CWD", state.workspace.path());
        std::env::set_current_dir(state.workspace.path()).unwrap();
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.cwd);
        for (name, value) in self.vars.drain(..) {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

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

fn listing(dir: &Path) -> Vec<String> {
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

#[test]
fn an_embedded_agent_answers_through_the_hosts_tool() {
    let _env = EnvGuard::new();
    let provider = Arc::new(Scripted::default());
    let agent = Agent::builder(provider.clone())
        .model("scripted")
        .system_prompt("You add numbers.")
        .tool(add_schema(), add)
        .build()
        .unwrap();

    let reply = block_on(agent.ask("What is 2 + 3?")).unwrap();

    assert_eq!(reply.text, "the sum is 5");
    assert_eq!(reply.ending, Ending::Finished);
    assert_eq!(reply.result.total_turns, 2);
    assert!(provider.system_prompts().contains("You add numbers."));
}

#[test]
fn an_embedded_agent_reads_and_writes_nothing_of_the_users() {
    let mut env = EnvGuard::new();
    let state = user_state();
    let home_before = listing(state.home.path());
    let workspace_before = listing(state.workspace.path());
    env.enter(&state);

    let provider = Arc::new(Scripted::default());
    let agent = Agent::builder(provider.clone())
        .model("scripted")
        .tool(add_schema(), add)
        .build()
        .unwrap();
    let reply = block_on(agent.ask("What is 2 + 3?")).unwrap();
    let scratch = agent.home().to_path_buf();
    drop(agent);

    assert_eq!(reply.text, "the sum is 5");
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

#[test]
fn an_embedded_agent_expands_no_file_reference() {
    let mut env = EnvGuard::new();
    let state = user_state();
    env.enter(&state);

    let provider = Arc::new(Scripted::default());
    let agent = Agent::builder(provider.clone())
        .model("scripted")
        .tool(add_schema(), add)
        .build()
        .unwrap();
    block_on(agent.ask("Summarise @file:AGENTS.md, then add 2 and 3.")).unwrap();

    let sent = provider.everything_sent();
    assert!(
        sent.contains("@file:AGENTS.md"),
        "the prompt was sent as given"
    );
    assert!(
        !sent.contains("AGENTS-MARKER"),
        "a working-folder file reached the model"
    );
}

#[test]
fn an_embedded_agent_runs_no_claim_verifier_unless_the_host_turns_isolation_off() {
    let mut env = EnvGuard::new();
    let state = user_state();
    env.enter(&state);
    env.set("HERMES_CLAIM_VERIFIER_ENABLED", "1");
    let contract = "Finalizer evidence contract";
    let ask = |isolated: bool| {
        let provider = Arc::new(Scripted::answering("Fixed src/lib.rs."));
        let agent = Agent::builder(provider.clone())
            .model("scripted")
            .configure(|c| c.isolated = isolated)
            .build()
            .unwrap();
        block_on(agent.ask("Fix the bug in src/lib.rs.")).unwrap();
        provider.everything_sent()
    };

    assert!(!ask(true).contains(contract), "the claim verifier ran");
    assert!(ask(false).contains(contract), "the control saw no contract");
}

#[test]
fn an_embedded_agent_runs_no_repo_research_check_unless_the_host_turns_isolation_off() {
    let mut env = EnvGuard::new();
    let state = user_state();
    env.enter(&state);
    let contract = "Repo research planning contract failed";
    let ask = |isolated: bool| {
        let provider = Arc::new(Scripted::answering("src/lib.rs fails because of a typo."));
        let agent = Agent::builder(provider.clone())
            .model("scripted")
            .tool(add_schema(), add)
            .configure(|c| c.isolated = isolated)
            .build()
            .unwrap();
        block_on(agent.ask("Audit src/lib.rs and explain why it fails.")).unwrap();
        provider.everything_sent()
    };

    assert!(!ask(true).contains(contract), "the repo research check ran");
    assert!(ask(false).contains(contract), "the control saw no contract");
}

#[test]
fn a_host_that_names_a_home_gets_that_homes_soul() {
    let _env = EnvGuard::new();
    let state = user_state();
    let provider = Arc::new(Scripted::default());
    let agent = Agent::builder(provider.clone())
        .model("scripted")
        .home(state.home.path())
        .build()
        .unwrap();

    block_on(agent.ask("What is 2 + 3?")).unwrap();
    drop(agent);

    assert!(provider.system_prompts().contains("SOUL-MARKER"));
    assert!(
        state.home.path().join("SOUL.md").exists(),
        "a named home is the host's to keep"
    );
}

#[test]
fn a_home_set_through_configure_is_kept_and_two_homes_are_refused() {
    let _env = EnvGuard::new();
    let state = user_state();
    let other = tempfile::tempdir().unwrap();
    let home = state.home.path().display().to_string();
    let builder = || Agent::builder(Arc::new(Scripted::default())).model("scripted");

    let agent = builder()
        .configure(|c| c.hermes_home = Some(home.clone()))
        .build()
        .unwrap();
    assert_eq!(agent.home(), state.home.path());
    drop(agent);
    assert!(state.home.path().join("SOUL.md").exists());

    let two = builder()
        .home(other.path())
        .configure(|c| c.hermes_home = Some(home.clone()))
        .build();
    assert!(two.is_err(), "two different homes were accepted");
}

#[test]
fn building_needs_a_model_and_at_least_one_turn() {
    let provider = || Arc::new(Scripted::default());

    assert!(Agent::builder(provider()).build().is_err());
    assert!(Agent::builder(provider()).model(" ").build().is_err());
    assert!(Agent::builder(provider())
        .model("scripted")
        .max_turns(0)
        .build()
        .is_err());
    assert!(Agent::builder(provider())
        .model("scripted")
        .configure(|c| c.max_turns = 0)
        .build()
        .is_err());
}

#[test]
fn a_run_cut_short_by_its_turn_limit_says_so() {
    let mut env = EnvGuard::new();
    env.set("HERMES_MAX_TURNS_UNLIMITED", "1");
    let agent = Agent::builder(Arc::new(Scripted::default()))
        .model("scripted")
        .max_turns(1)
        .tool(add_schema(), add)
        .build()
        .unwrap();

    let reply = block_on(agent.ask("What is 2 + 3?")).unwrap();

    assert_eq!(reply.ending, Ending::MaxTurns);
    assert_eq!(
        reply.result.total_turns, 1,
        "the environment lifted the cap"
    );
}
