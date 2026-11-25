//! Embed the agent loop in another program.
//!
//! [`Agent::builder`] takes any [`LlmProvider`] and the host's own tools, and the agent sees
//! only what the host gives it: by default it runs in a fresh, empty home of its own, removed
//! when the agent is dropped, so it reads no `SOUL.md`, memories or skills of the person
//! running the host, and it does not index or read the working folder (no `AGENTS.md`, code
//! index or LSP notes). [`AgentBuilder::home`] points it at a Hermes home instead, and
//! [`AgentBuilder::configure`] reaches every other [`AgentConfig`] field.
//!
//! ```no_run
//! use std::sync::Arc;
//! use hermes_agent::embed::Agent;
//! use hermes_agent::OpenAiProvider;
//! use hermes_core::{JsonSchema, ToolSchema};
//!
//! # async fn demo() -> Result<(), hermes_core::AgentError> {
//! let provider = Arc::new(OpenAiProvider::new(std::env::var("OPENAI_API_KEY").unwrap_or_default()));
//! let agent = Agent::builder(provider)
//!     .model("gpt-4o-mini")
//!     .system_prompt("You answer in one sentence.")
//!     .tool(
//!         ToolSchema::new("clock", "The time in UTC", JsonSchema::new("object")),
//!         |_args| Ok("12:00".to_string()),
//!     )
//!     .build()?;
//! let reply = agent.ask("What time is it?").await?;
//! println!("{}", reply.text);
//! # Ok(())
//! # }
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hermes_core::{
    AgentError, AgentResult, LlmProvider, Message, MessageRole, ToolError, ToolSchema,
};
use serde_json::Value;

use crate::agent_loop::ToolRegistry;
use crate::{AgentConfig, AgentLoop};

/// Builds an [`Agent`]. Start with [`Agent::builder`].
pub struct AgentBuilder {
    provider: Arc<dyn LlmProvider>,
    config: AgentConfig,
    tools: ToolRegistry,
    home: Option<PathBuf>,
}

impl AgentBuilder {
    /// The model name the provider is asked for.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.config.model = model.into();
        self
    }

    /// Instructions added to the agent's system prompt.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.config.system_prompt = Some(prompt.into());
        self
    }

    /// The most model calls one [`Agent::ask`] may make.
    pub fn max_turns(mut self, turns: u32) -> Self {
        self.config.max_turns = turns;
        self
    }

    /// A tool the model may call. The handler gets the call's arguments and returns the text
    /// the model reads; an error is reported to the model as the tool's result.
    pub fn tool(
        mut self,
        schema: ToolSchema,
        handler: impl Fn(Value) -> Result<String, ToolError> + Send + Sync + 'static,
    ) -> Self {
        self.tools
            .register(schema.name.clone(), schema, Arc::new(handler));
        self
    }

    /// Run in this Hermes home: its `SOUL.md`, memories and skills shape the prompt, and its
    /// files stay after the agent is dropped.
    pub fn home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// Change any other setting. Runs after the builder's own choices.
    pub fn configure(mut self, change: impl FnOnce(&mut AgentConfig)) -> Self {
        change(&mut self.config);
        self
    }

    /// Build the agent, creating its own empty home unless [`AgentBuilder::home`] named one.
    pub fn build(self) -> Result<Agent, AgentError> {
        let Self {
            provider,
            mut config,
            tools,
            home,
        } = self;
        let home = match home {
            Some(path) => Home::Named(path),
            None => Home::Scratch(scratch_home()?),
        };
        if matches!(home, Home::Scratch(_)) {
            config.skip_memory = true;
        }
        config.hermes_home = Some(home.path().display().to_string());
        let schemas = tools.schemas();
        Ok(Agent {
            inner: AgentLoop::new(config, Arc::new(tools), provider),
            tools: (!schemas.is_empty()).then_some(schemas),
            home,
        })
    }
}

/// An agent loop with its provider, tools and home. See the [module docs](self).
pub struct Agent {
    inner: AgentLoop,
    tools: Option<Vec<ToolSchema>>,
    home: Home,
}

/// What one [`Agent::ask`] produced.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The model's last answer.
    pub text: String,
    /// The whole run: every message, turn count, tool errors and usage.
    pub result: AgentResult,
}

impl Agent {
    /// Start building an agent that talks to `provider`.
    pub fn builder(provider: Arc<dyn LlmProvider>) -> AgentBuilder {
        AgentBuilder {
            provider,
            config: AgentConfig {
                skip_context_files: true,
                code_index_enabled: false,
                lsp_context_enabled: false,
                background_review_enabled: false,
                quiet_mode: true,
                ..AgentConfig::default()
            },
            tools: ToolRegistry::new(),
            home: None,
        }
    }

    /// Ask one question in a new conversation and run until the model answers.
    pub async fn ask(&self, prompt: impl Into<String>) -> Result<Reply, AgentError> {
        let result = self.run(vec![Message::user(prompt)]).await?;
        let text = result
            .messages
            .iter()
            .rev()
            .find(|m| {
                m.role == MessageRole::Assistant
                    && m.tool_calls.as_ref().is_none_or(|calls| calls.is_empty())
            })
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        Ok(Reply { text, result })
    }

    /// Continue a conversation the host keeps: pass its messages, get the run back.
    pub async fn run(&self, messages: Vec<Message>) -> Result<AgentResult, AgentError> {
        self.inner.run(messages, self.tools.clone()).await
    }

    /// The home the agent runs in.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// The underlying loop, for callbacks, interrupts and the rest of its API.
    pub fn agent_loop(&self) -> &AgentLoop {
        &self.inner
    }
}

enum Home {
    Named(PathBuf),
    Scratch(PathBuf),
}

impl Home {
    fn path(&self) -> &Path {
        match self {
            Home::Named(path) | Home::Scratch(path) => path,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Home::Scratch(path) = self {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// A new folder under the temp folder. `create_dir` refuses a name that already exists, so a
/// folder or link someone else put there is never used.
fn scratch_home() -> Result<PathBuf, AgentError> {
    let path = std::env::temp_dir().join(format!("hermes-embed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path)
        .map_err(|e| AgentError::Config(format!("cannot create {}: {e}", path.display())))?;
    Ok(path)
}
