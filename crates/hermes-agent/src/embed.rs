//! Embed the agent loop in another program.
//!
//! [`Agent::builder`] takes any [`LlmProvider`], a model name and the host's own tools, and
//! the agent sees only what the host gives it. By default it runs in a fresh, empty home of
//! its own, removed when the agent is dropped, so it reads no `SOUL.md`, memories or skills of
//! the person running the host; and it is [isolated](AgentConfig::isolated): it reads nothing
//! from the working folder (no `AGENTS.md`, `@file:` or other `@` references, code index or
//! LSP notes), runs no claim verifier or background review, and posts no ContextLattice
//! checkpoint. [`AgentBuilder::home`] points it at a Hermes home instead.
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

/// The model calls one [`Agent::ask`] may make unless [`AgentBuilder::max_turns`] says
/// otherwise.
pub const DEFAULT_MAX_TURNS: u32 = 30;

/// Builds an [`Agent`]. Start with [`Agent::builder`].
pub struct AgentBuilder {
    provider: Arc<dyn LlmProvider>,
    config: AgentConfig,
    model: Option<String>,
    tools: ToolRegistry,
    home: Option<PathBuf>,
}

impl AgentBuilder {
    /// The model name the provider is asked for. Required.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Instructions added to the agent's system prompt.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.config.system_prompt = Some(prompt.into());
        self
    }

    /// The most model calls one [`Agent::ask`] may make: at least 1; default
    /// [`DEFAULT_MAX_TURNS`].
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
    /// files stay after the agent is dropped. The agent stays isolated from the working
    /// folder.
    pub fn home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// **Unstable.** Change any other [`AgentConfig`] setting. It runs after the builder's
    /// own choices, so it can turn [`AgentConfig::isolated`] off or switch the working-folder
    /// features back on; that is the caller's decision. A `hermes_home` set here is used as
    /// the agent's home, as [`AgentBuilder::home`] would be; naming two different homes is an
    /// error. The model always comes from [`AgentBuilder::model`], and a `max_turns` of 0 set
    /// here is refused as well.
    pub fn configure(mut self, change: impl FnOnce(&mut AgentConfig)) -> Self {
        change(&mut self.config);
        self
    }

    /// Build the agent, creating its own empty home unless a home was named. Fails without a
    /// model, with `max_turns` 0, with two different homes, or when the home cannot be
    /// created.
    pub fn build(self) -> Result<Agent, AgentError> {
        let Self {
            provider,
            mut config,
            model,
            tools,
            home,
        } = self;
        config.model = model
            .filter(|m| !m.trim().is_empty())
            .ok_or_else(|| AgentError::Config("an embedded agent needs a model".into()))?;
        if config.max_turns == 0 {
            return Err(AgentError::Config("max_turns must be at least 1".into()));
        }
        let home = match (home, config.hermes_home.take()) {
            (Some(a), Some(b)) if a != Path::new(&b) => {
                return Err(AgentError::Config(format!(
                    "two homes named: {} and {b}",
                    a.display()
                )))
            }
            (Some(path), _) => Home::Named(path),
            (None, Some(path)) => Home::Named(path.into()),
            (None, None) => Home::Scratch(scratch_home()?),
        };
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
#[non_exhaustive]
pub struct Reply {
    /// The model's last answer; empty when it gave none.
    pub text: String,
    /// How the run ended.
    pub ending: Ending,
    /// The whole run: every message, turn count, tool errors and usage.
    pub result: AgentResult,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ending {
    /// The model answered without asking for another tool call.
    Finished,
    /// It used every turn [`AgentBuilder::max_turns`] allowed.
    MaxTurns,
    /// Its spend reached `max_cost_usd`.
    CostCap,
    /// It was interrupted.
    Interrupted,
    /// It stopped for another reason before a final answer.
    Stopped,
}

impl Ending {
    fn of(result: &AgentResult, config: &AgentConfig) -> Self {
        if result.interrupted {
            Ending::Interrupted
        } else if result.finished_naturally {
            Ending::Finished
        } else if config
            .max_cost_usd
            .zip(result.session_cost_usd)
            .is_some_and(|(cap, spent)| spent >= cap)
        {
            Ending::CostCap
        } else if result.total_turns >= config.max_turns {
            Ending::MaxTurns
        } else {
            Ending::Stopped
        }
    }
}

impl Agent {
    /// Start building an agent that talks to `provider`.
    pub fn builder(provider: Arc<dyn LlmProvider>) -> AgentBuilder {
        AgentBuilder {
            provider,
            config: AgentConfig {
                max_turns: DEFAULT_MAX_TURNS,
                isolated: true,
                skip_context_files: true,
                code_index_enabled: false,
                lsp_context_enabled: false,
                background_review_enabled: false,
                quiet_mode: true,
                ..AgentConfig::default()
            },
            model: None,
            tools: ToolRegistry::new(),
            home: None,
        }
    }

    /// Ask one question in a new conversation and run until the model answers.
    pub async fn ask(&self, prompt: impl Into<String>) -> Result<Reply, AgentError> {
        let result = self.run(vec![Message::user(prompt)]).await?;
        let ending = Ending::of(&result, &self.inner.config);
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
        Ok(Reply {
            text,
            ending,
            result,
        })
    }

    /// Continue a conversation the host keeps: pass its messages, get the run back.
    pub async fn run(&self, messages: Vec<Message>) -> Result<AgentResult, AgentError> {
        self.inner.run(messages, self.tools.clone()).await
    }

    /// The home the agent runs in.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// **Unstable.** The underlying loop, for callbacks, interrupts and the rest of its API;
    /// it may change without notice.
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
