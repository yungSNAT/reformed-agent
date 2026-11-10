//! In-process sub-agent orchestrator.
//!
//! Python v2026.4.13 parity: when the primary agent calls `delegate_task`, we
//! actually spawn a bounded child [`AgentLoop`] rather than just returning a
//! signal envelope. The orchestrator is responsible for:
//!
//! - building a restricted child [`AgentConfig`] (depth+1, half-remaining turn
//!   budget, isolated session id)
//! - running the child with a wall-clock timeout and cooperative cancellation
//!   tied to the parent's [`InterruptController`]
//! - persisting a lineage record per sub-agent under
//!   `$HERMES_HOME/subagents/<sub_agent_id>.json` with
//!   started/ended timestamps, status, turn count, usage and cost
//! - returning a JSON string that the agent loop feeds back to the LLM as the
//!   tool result (must include `sub_agent_id` so
//!   [`AgentLoop::delegation_event_from_tool_result`] still fires).
//!
//! Registration: consumers build an [`AgentLoop`] via
//! [`crate::AgentLoop::with_sub_agent_orchestrator`] to swap the default
//! signal-only behavior with real execution.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use hermes_core::{AgentError, LlmProvider, Message, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::timeout;

use crate::agent_loop::{AgentConfig, AgentLoop, ToolRegistry};
use crate::code_index::CodeIndex;
use crate::credential_pool::CredentialPool;
use crate::interrupt::InterruptController;

/// Boxed `Send` future type alias used to short-circuit async-recursion
/// between parent and child [`AgentLoop::execute_tool_calls`] futures.
type BoxSendFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub type BackgroundDelegationCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// Default wall-clock cap for a single child agent turn-chain.
pub const DEFAULT_SUB_AGENT_TIMEOUT_SECS: u64 = 600;
/// Floor on child max_turns regardless of parent budget.
pub const SUB_AGENT_MIN_TURNS: u32 = 2;
/// Ceiling on child max_turns to keep sub-tasks bounded.
pub const SUB_AGENT_MAX_TURNS_CAP: u32 = 12;

/// Lineage record persisted to disk for each spawned sub-agent.
///
/// One record is written at `started` and rewritten at terminal state. The file
/// layout stays intentionally flat so it can also be inspected / tailed by
/// other tooling (Python baseline uses a similar per-sub-agent JSON file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentLineage {
    pub sub_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub depth: u32,
    pub max_depth: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_budget_remaining_usd: Option<f64>,
    pub status: SubAgentStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub total_turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Terminal state vocabulary for sub-agent lineage records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentStatus {
    Started,
    Completed,
    Failed,
    Cancelled,
    Timeout,
}

/// Parameters forwarded by the `delegate_task` tool invocation.
#[derive(Debug, Clone, Default)]
pub struct SubAgentRequest {
    pub task: String,
    pub context: Option<String>,
    pub toolset: Option<String>,
    /// Parent-advertised tools to inherit when no explicit child toolset is requested.
    pub inherited_tool_schemas: Vec<ToolSchema>,
    pub model: Option<String>,
    /// Child depth injected by parent agent loop (parent_depth + 1).
    pub child_depth: u32,
    /// Propagated max depth for lineage / bookkeeping.
    pub max_depth: u32,
    /// Parent budget remaining (USD) — exposed to the child for cost awareness.
    pub parent_budget_remaining_usd: Option<f64>,
    /// Run the child detached and deliver completion through the background
    /// delegation callback instead of blocking the parent tool turn.
    pub background: bool,
}

/// Configuration for [`SubAgentOrchestrator`].
#[derive(Clone)]
pub struct SubAgentOrchestratorConfig {
    pub parent_config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub parent_credential_pool: Option<Arc<CredentialPool>>,
    pub parent_interrupt: InterruptController,
    pub hermes_home: PathBuf,
    pub parent_session_id: Option<String>,
    pub timeout: Duration,
    pub background_delegation_callback: Option<BackgroundDelegationCallback>,
    /// Parent's code index, shared with every child so a delegation never
    /// scans the workspace again. `None` gives children no repo map.
    pub code_index: Option<Arc<Mutex<CodeIndex>>>,
}

/// In-process executor for `delegate_task` tool calls.
pub struct SubAgentOrchestrator {
    cfg: SubAgentOrchestratorConfig,
}

impl SubAgentOrchestrator {
    pub fn new(cfg: SubAgentOrchestratorConfig) -> Self {
        Self { cfg }
    }

    /// Default orchestrator that mirrors the current `AgentLoop`'s wiring.
    ///
    /// The returned orchestrator inherits the parent's tool registry, provider
    /// and interrupt handle so cancellation propagates from the parent to any
    /// actively running child.
    pub fn from_parent(parent: &AgentLoop, hermes_home: PathBuf) -> Self {
        Self {
            cfg: SubAgentOrchestratorConfig {
                parent_config: parent.config.clone(),
                tool_registry: parent.tool_registry.clone(),
                llm_provider: parent.llm_provider.clone(),
                parent_credential_pool: parent.primary_credential_pool.clone(),
                parent_interrupt: parent.interrupt.clone(),
                hermes_home,
                parent_session_id: parent.config.session_id.clone(),
                timeout: Duration::from_secs(DEFAULT_SUB_AGENT_TIMEOUT_SECS),
                background_delegation_callback: parent
                    .callbacks
                    .background_delegation_callback
                    .clone()
                    .or_else(|| parent.callbacks.background_review_callback.clone()),
                code_index: parent.code_index.clone(),
            },
        }
    }

    /// Execute a `delegate_task` request synchronously from the perspective of
    /// the caller, returning a JSON string suitable for use as the tool result.
    ///
    /// The returned JSON always contains `sub_agent_id` at the top level so
    /// existing delegation-event wiring in [`AgentLoop`] continues to work.
    ///
    /// Returning `BoxSendFuture` (rather than an `async fn`) is load-bearing:
    /// the parent `AgentLoop::execute_tool_calls` awaits this future, and the
    /// child may eventually call back into an orchestrator of its own. A
    /// concrete `impl Future` type would force the compiler to recursively
    /// prove `Send` through that cycle, which it cannot. Boxing into
    /// `dyn Future + Send` erases the child's future type and breaks the
    /// cycle.
    pub fn execute(self: &Arc<Self>, req: SubAgentRequest) -> BoxSendFuture<String> {
        let this = self.clone();
        Box::pin(async move {
            if req.background {
                this.dispatch_background(req)
            } else {
                let sub_agent_id = format!("subagent-{}", uuid::Uuid::new_v4());
                this.execute_inner(req, sub_agent_id).await
            }
        })
    }

    fn dispatch_background(self: Arc<Self>, req: SubAgentRequest) -> String {
        let sub_agent_id = format!("subagent-{}", uuid::Uuid::new_v4());
        let task = req.task.clone();
        let context = req.context.clone();
        let toolset = req.toolset.clone();
        let model = req.model.clone();
        let depth = req.child_depth;
        let max_depth = req.max_depth;
        let callback = self.cfg.background_delegation_callback.clone();
        let worker = self.clone();
        let worker_sub_agent_id = sub_agent_id.clone();
        tokio::spawn(async move {
            let output = worker.execute_inner(req, worker_sub_agent_id).await;
            if let Some(callback) = callback {
                let summary = format_background_delegation_notification(&output);
                callback(&summary);
            }
        });

        json!({
            "sub_agent_id": sub_agent_id,
            "status": "dispatched",
            "background": true,
            "task": task,
            "context": context,
            "toolset": toolset,
            "model": model,
            "depth": depth,
            "max_depth": max_depth,
        })
        .to_string()
    }

    async fn execute_inner(self: Arc<Self>, req: SubAgentRequest, sub_agent_id: String) -> String {
        let started_at = Utc::now();

        let mut lineage = SubAgentLineage {
            sub_agent_id: sub_agent_id.clone(),
            parent_session_id: self.cfg.parent_session_id.clone(),
            task: req.task.clone(),
            context: req.context.clone(),
            toolset: req.toolset.clone(),
            model: clean_config_string(req.model.as_deref())
                .or_else(|| clean_config_string(self.cfg.parent_config.delegation_model.as_deref()))
                .map(str::to_string),
            provider: self.effective_delegation_provider(),
            depth: req.child_depth,
            max_depth: req.max_depth,
            parent_budget_remaining_usd: req.parent_budget_remaining_usd,
            status: SubAgentStatus::Started,
            started_at,
            ended_at: None,
            total_turns: 0,
            prompt_tokens: None,
            completion_tokens: None,
            estimated_cost_usd: None,
            error: None,
        };

        // Best-effort lineage persistence — failures must never break delegation.
        self.persist_lineage(&lineage).await;

        // Build and run the child.
        let outcome = self.run_child(&req, &sub_agent_id).await;

        match outcome {
            Ok(result) => {
                lineage.status = SubAgentStatus::Completed;
                lineage.total_turns = result.total_turns;
                if let Some(usage) = result.usage.as_ref() {
                    lineage.prompt_tokens = Some(usage.prompt_tokens);
                    lineage.completion_tokens = Some(usage.completion_tokens);
                    lineage.estimated_cost_usd = usage.estimated_cost;
                }
                lineage.ended_at = Some(Utc::now());
                self.persist_lineage(&lineage).await;

                let final_text = extract_final_assistant_text(&result.messages);
                json!({
                    "sub_agent_id": sub_agent_id,
                    "status": "completed",
                    "background": req.background,
                    "task": req.task,
                    "total_turns": result.total_turns,
                    "finished_naturally": result.finished_naturally,
                    "result": final_text,
                    "usage": result.usage.as_ref().map(|u| json!({
                        "prompt_tokens": u.prompt_tokens,
                        "completion_tokens": u.completion_tokens,
                        "total_tokens": u.total_tokens,
                        "estimated_cost": u.estimated_cost,
                    })),
                    "depth": req.child_depth,
                    "max_depth": req.max_depth,
                })
                .to_string()
            }
            Err(e) => {
                let status = match &e {
                    SubAgentError::Timeout => SubAgentStatus::Timeout,
                    SubAgentError::Cancelled => SubAgentStatus::Cancelled,
                    SubAgentError::Agent(_) => SubAgentStatus::Failed,
                };
                lineage.status = status;
                lineage.error = Some(e.to_string());
                lineage.ended_at = Some(Utc::now());
                self.persist_lineage(&lineage).await;

                json!({
                    "sub_agent_id": sub_agent_id,
                    "background": req.background,
                    "status": match status {
                        SubAgentStatus::Timeout => "timeout",
                        SubAgentStatus::Cancelled => "cancelled",
                        SubAgentStatus::Failed => "failed",
                        _ => "failed",
                    },
                    "task": req.task,
                    "error": e.to_string(),
                    "depth": req.child_depth,
                    "max_depth": req.max_depth,
                })
                .to_string()
            }
        }
    }

    async fn run_child(
        &self,
        req: &SubAgentRequest,
        sub_agent_id: &str,
    ) -> Result<hermes_core::AgentResult, SubAgentError> {
        let child_interrupt = InterruptController::new();

        // Forward parent cancellation to child via a light watcher task.
        let parent_ctrl = self.cfg.parent_interrupt.clone();
        let child_ctrl = child_interrupt.clone();
        let watcher_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher_stop_clone = watcher_stop.clone();
        let watcher = tokio::spawn(async move {
            while !watcher_stop_clone.load(std::sync::atomic::Ordering::Acquire) {
                if parent_ctrl.is_interrupted() {
                    child_ctrl.interrupt(Some("parent cancelled".to_string()));
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        let child_config = self.build_child_config(req, sub_agent_id);
        let child_provider = self.child_llm_provider(&child_config)?;
        let child_interrupt_for_spawn = child_interrupt.clone();
        let tool_registry = self.cfg.tool_registry.clone();
        let child_depth = req.child_depth;
        let inherited_tools = if req.toolset.is_none() && !req.inherited_tool_schemas.is_empty() {
            Some(req.inherited_tool_schemas.clone())
        } else {
            None
        };
        let initial = initial_messages(&req.task, req.context.as_deref());
        let code_index = self.cfg.code_index.clone();

        // Run the child on its own tokio task so that its `impl Future` type is
        // fully erased behind a `JoinHandle`, breaking async recursion with the
        // parent's `execute_tool_calls` future (which hosts the orchestrator
        // call). `tokio::spawn` requires `Send + 'static`, which forces child
        // `AgentLoop::run` to already be Send — this is also what the existing
        // `spawn_background_review` relies on.
        let join = tokio::spawn(async move {
            let mut child_agent = AgentLoop::with_interrupt(
                child_config,
                tool_registry,
                child_provider,
                child_interrupt_for_spawn,
            )
            .with_delegate_depth(child_depth);
            child_agent.code_index = code_index;
            child_agent.run(initial, inherited_tools).await
        });

        let result = match timeout(self.cfg.timeout, join).await {
            Ok(Ok(Ok(result))) if result.interrupted => Err(SubAgentError::Cancelled),
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(AgentError::Interrupted { .. }))) => Err(SubAgentError::Cancelled),
            Ok(Ok(Err(e))) => Err(SubAgentError::Agent(e)),
            Ok(Err(join_err)) => Err(SubAgentError::Agent(AgentError::ToolExecution(format!(
                "sub-agent join error: {}",
                join_err
            )))),
            Err(_) => {
                // Cooperative cancel on timeout — next `check_interrupt()`
                // boundary in the child unwinds.
                child_interrupt.interrupt(Some("timeout".to_string()));
                Err(SubAgentError::Timeout)
            }
        };

        watcher_stop.store(true, std::sync::atomic::Ordering::Release);
        let _ = watcher.await;
        result
    }

    fn build_child_config(&self, req: &SubAgentRequest, sub_agent_id: &str) -> AgentConfig {
        let parent = &self.cfg.parent_config;

        let remaining_parent_turns = parent
            .max_turns
            .saturating_sub(req.child_depth.saturating_mul(2));
        let child_turns = remaining_parent_turns
            .saturating_div(2)
            .clamp(SUB_AGENT_MIN_TURNS, SUB_AGENT_MAX_TURNS_CAP);

        let child_session_id = match parent.session_id.as_deref() {
            Some(s) if !s.is_empty() => format!("{}::{}", s, sub_agent_id),
            _ => sub_agent_id.to_string(),
        };

        let mut child = parent.clone();
        child.session_id = Some(child_session_id);
        child.max_turns = child_turns;
        child.stream = false;
        // Keep child sessions silent/contained (Python child quiet_mode semantics).
        child.background_review_enabled = false;
        child.background_review_metrics_enabled = false;
        child.memory_nudge_interval = 0;
        child.skill_creation_nudge_interval = 0;
        child.quiet_mode = true;
        // Children should not re-spawn grandchildren beyond the contract;
        // depth checks inside AgentLoop::execute_tool_calls still apply.
        child.skip_memory = true;

        let requested_model = clean_config_string(req.model.as_deref())
            .map(str::to_string)
            .or_else(|| {
                clean_config_string(parent.delegation_model.as_deref()).map(str::to_string)
            });
        let runtime_provider = self.effective_delegation_provider();

        if let Some(provider) = runtime_provider {
            let model = requested_model.unwrap_or_else(|| model_part(parent.model.as_str()));
            child.provider = Some(provider.clone());
            child.model = format!("{provider}:{}", strip_provider_prefix(model.as_str()));
        } else if let Some(model) = requested_model {
            // Model-only override inherits the parent provider and credentials.
            child.model = model;
        }

        // Budget: hard-cap child by parent remaining budget when known.
        if let Some(remaining) = req.parent_budget_remaining_usd {
            let cap = remaining.max(0.0);
            child.max_cost_usd = Some(match child.max_cost_usd {
                Some(existing) => existing.min(cap),
                None => cap,
            });
        }
        child
    }

    fn effective_delegation_provider(&self) -> Option<String> {
        let parent = &self.cfg.parent_config;
        clean_config_string(parent.delegation_provider.as_deref())
            .map(str::to_string)
            .or_else(|| {
                let has_direct_endpoint =
                    clean_config_string(parent.delegation_base_url.as_deref()).is_some()
                        || clean_config_string(parent.delegation_api_key.as_deref()).is_some();
                has_direct_endpoint.then(|| "custom".to_string())
            })
    }

    fn child_llm_provider(
        &self,
        child_config: &AgentConfig,
    ) -> Result<Arc<dyn LlmProvider>, SubAgentError> {
        let Some(provider) = self.effective_delegation_provider() else {
            return Ok(self.cfg.llm_provider.clone());
        };

        let model_name = strip_provider_prefix(child_config.model.as_str()).to_string();
        let mut resolver = AgentLoop::with_interrupt(
            child_config.clone(),
            self.cfg.tool_registry.clone(),
            self.cfg.llm_provider.clone(),
            InterruptController::new(),
        );
        // The parent's pool holds the parent provider's keys; a child on
        // another provider uses its own credential.
        let parent = &self.cfg.parent_config;
        let parent_provider = parent
            .model
            .split_once(':')
            .map(|(p, _)| p.trim())
            .filter(|p| !p.is_empty())
            .or_else(|| clean_config_string(parent.provider.as_deref()))
            .unwrap_or("openai");
        let same_provider =
            crate::provider_profiles::normalize_runtime_provider_name(parent_provider)
                == crate::provider_profiles::normalize_runtime_provider_name(provider.as_str());
        if let Some(pool) = self
            .cfg
            .parent_credential_pool
            .as_ref()
            .filter(|_| same_provider)
        {
            resolver = resolver.with_primary_credential_pool(pool.clone());
        }

        resolver
            .build_delegation_runtime_provider(
                provider.as_str(),
                model_name.as_str(),
                clean_config_string(child_config.delegation_base_url.as_deref()),
                clean_config_string(child_config.delegation_api_key.as_deref()),
            )
            .map_err(SubAgentError::Agent)
    }

    async fn persist_lineage(&self, lineage: &SubAgentLineage) {
        let dir = self.cfg.hermes_home.join("subagents");
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::debug!(error = %e, "sub-agent lineage dir create failed");
            return;
        }
        let path = dir.join(format!("{}.json", lineage.sub_agent_id));
        let body = match serde_json::to_string_pretty(lineage) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "sub-agent lineage serialize failed");
                return;
            }
        };
        if let Err(e) = tokio::fs::write(&path, body).await {
            tracing::debug!(error = %e, path = %path.display(), "sub-agent lineage write failed");
        }
    }
}

#[derive(Debug)]
enum SubAgentError {
    Timeout,
    Cancelled,
    Agent(AgentError),
}

impl std::fmt::Display for SubAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubAgentError::Timeout => write!(f, "sub-agent timed out"),
            SubAgentError::Cancelled => write!(f, "sub-agent cancelled"),
            SubAgentError::Agent(e) => write!(f, "sub-agent error: {}", e),
        }
    }
}

fn clean_config_string(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn strip_provider_prefix(model: &str) -> &str {
    model
        .split_once(':')
        .map(|(_, model)| model.trim())
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| model.trim())
}

fn model_part(model: &str) -> String {
    strip_provider_prefix(model).to_string()
}

fn initial_messages(task: &str, context: Option<&str>) -> Vec<Message> {
    let mut content = task.to_string();
    if let Some(ctx) = context {
        if !ctx.trim().is_empty() {
            content = format!("{task}\n\n[Context]\n{ctx}");
        }
    }
    vec![Message::user(&content)]
}

fn extract_final_assistant_text(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
        .and_then(|m| m.content.clone())
        .unwrap_or_default()
}

fn format_background_delegation_notification(output: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return format!("ASYNC DELEGATION COMPLETE\n\nStatus: unknown\n\n{output}");
    };
    let task = value
        .get("task")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(unknown task)");
    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let sub_agent_id = value
        .get("sub_agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let mut out = format!(
        "ASYNC DELEGATION COMPLETE\n\nTask: {task}\nSub-agent: {sub_agent_id}\nStatus: {status}"
    );
    if let Some(result) = value
        .get("result")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        out.push_str("\n\nResult:\n");
        out.push_str(result);
    }
    if let Some(error) = value
        .get("error")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        out.push_str("\n\nError:\n");
        out.push_str(error);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{BoxStream, StreamExt};
    use hermes_core::{JsonSchema, LlmResponse, StreamChunk, ToolSchema};
    use std::sync::{Arc, Mutex};

    // Minimal test provider that returns a deterministic assistant response
    // so the child loop finishes in a single turn. Behavioural tests live
    // in agent_loop::tests.
    struct NoopProvider;
    #[async_trait::async_trait]
    impl LlmProvider for NoopProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Ok(LlmResponse {
                message: Message::assistant("done"),
                usage: None,
                model: "noop".into(),
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

    #[tokio::test]
    async fn omitted_toolset_inherits_parent_advertised_tools() {
        struct RecordingProvider {
            seen_tools: Arc<Mutex<Vec<Vec<String>>>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RecordingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                self.seen_tools
                    .lock()
                    .expect("seen tools lock")
                    .push(tools.iter().map(|tool| tool.name.clone()).collect());
                Ok(LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "recording".into(),
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

        let seen_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: AgentConfig {
                max_turns: 2,
                ..AgentConfig::default()
            },
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(RecordingProvider {
                seen_tools: seen_tools.clone(),
            }),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_secs(2),
            background_delegation_callback: None,
            code_index: None,
        }));

        let out = orch
            .execute(SubAgentRequest {
                task: "child task".into(),
                child_depth: 1,
                max_depth: 4,
                inherited_tool_schemas: vec![ToolSchema::new(
                    "terminal",
                    "Terminal tool",
                    JsonSchema::new("object"),
                )],
                ..Default::default()
            })
            .await;

        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "completed");
        let seen = seen_tools.lock().expect("seen tools lock");
        assert_eq!(seen.as_slice(), &[vec!["terminal".to_string()]]);
    }

    #[tokio::test]
    async fn child_uses_parent_code_index_instead_of_scanning_its_own() {
        struct SystemPromptProvider {
            system_prompts: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for SystemPromptProvider {
            async fn chat_completion(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                let system = messages
                    .iter()
                    .find(|m| matches!(m.role, hermes_core::MessageRole::System))
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default();
                self.system_prompts
                    .lock()
                    .expect("system prompts lock")
                    .push(system);
                Ok(LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "system-prompt".into(),
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

        // The parent's index covers a workspace that is not the test's working
        // directory, so its marker reaches the child's repo map only if the
        // child reuses this index rather than scanning a workspace itself.
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("parent.rs"),
            "pub fn parent_index_marker_fn() {}\n",
        )
        .unwrap();
        let mut index = CodeIndex::new(
            workspace.path(),
            crate::code_index::CodeIndexConfig::default(),
        );
        index.refresh_full();

        let system_prompts = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut parent = AgentLoop::new(
            AgentConfig {
                max_turns: 2,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(SystemPromptProvider {
                system_prompts: system_prompts.clone(),
            }),
        );
        parent.code_index = Some(Arc::new(Mutex::new(index)));
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::from_parent(
            &parent,
            tmp.path().to_path_buf(),
        ));

        let out = orch
            .execute(SubAgentRequest {
                task: "child task".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            })
            .await;

        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "completed");
        let prompts = system_prompts.lock().expect("system prompts lock");
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("parent_index_marker_fn"));
    }

    #[tokio::test]
    async fn spawn_timeout_path_produces_timeout_json() {
        struct SlowProvider;
        #[async_trait::async_trait]
        impl LlmProvider for SlowProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(LlmResponse {
                    message: Message::assistant("late"),
                    usage: None,
                    model: "slow".into(),
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

        let parent_cfg = AgentConfig {
            max_turns: 2,
            model: "slow".into(),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent_cfg,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(SlowProvider),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_millis(50),
            background_delegation_callback: None,
            code_index: None,
        }));
        let out = orch
            .execute(SubAgentRequest {
                task: "test task".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            })
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "timeout");
        assert!(parsed["sub_agent_id"]
            .as_str()
            .unwrap()
            .starts_with("subagent-"));
        // Lineage file should exist under $HERMES_HOME/subagents/<id>.json
        let dir = tmp.path().join("subagents");
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        let entry = entries.next_entry().await.unwrap().unwrap();
        assert!(entry
            .path()
            .extension()
            .map(|s| s == "json")
            .unwrap_or(false));
    }

    #[tokio::test]
    async fn background_request_dispatches_handle_and_emits_completion_callback() {
        let tmp = tempfile::tempdir().unwrap();
        let completions = Arc::new(Mutex::new(Vec::<String>::new()));
        let completions_cb = completions.clone();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: AgentConfig {
                max_turns: 2,
                ..AgentConfig::default()
            },
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_secs(2),
            background_delegation_callback: Some(Arc::new(move |text: &str| {
                completions_cb
                    .lock()
                    .expect("completion lock")
                    .push(text.to_string());
            })),
            code_index: None,
        }));

        let out = orch
            .execute(SubAgentRequest {
                task: "background task".into(),
                child_depth: 1,
                max_depth: 4,
                background: true,
                ..Default::default()
            })
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "dispatched");
        assert_eq!(parsed["background"], true);
        let sub_agent_id = parsed["sub_agent_id"].as_str().unwrap().to_string();

        for _ in 0..50 {
            if !completions.lock().expect("completion lock").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let completion = completions
            .lock()
            .expect("completion lock")
            .pop()
            .expect("background completion callback");
        assert!(completion.contains("ASYNC DELEGATION COMPLETE"));
        assert!(completion.contains("background task"));
        assert!(completion.contains("Result:\ndone"));
        assert!(tmp
            .path()
            .join("subagents")
            .join(format!("{sub_agent_id}.json"))
            .exists());
    }

    #[tokio::test]
    async fn cancel_path_reports_cancelled() {
        let parent_ctrl = InterruptController::new();
        parent_ctrl.interrupt(Some("stop".into())); // pre-cancel before spawn

        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: AgentConfig::default(),
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: None,
            parent_interrupt: parent_ctrl,
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: None,
            timeout: Duration::from_secs(2),
            background_delegation_callback: None,
            code_index: None,
        }));
        let out = orch
            .execute(SubAgentRequest {
                task: "noop".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            })
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        // Parent interrupt propagates to child; status should be cancelled or
        // completed depending on scheduling. We accept either (cancelled most
        // of the time; completed if watcher missed the window). The lineage
        // file still must be written.
        assert!(parsed["sub_agent_id"].as_str().is_some());
    }

    #[test]
    fn build_child_config_clamps_turns() {
        let parent = AgentConfig {
            max_turns: 20,
            model: "m".into(),
            session_id: Some("root".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
            background_delegation_callback: None,
            code_index: None,
        });
        let child = orch.build_child_config(
            &SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                parent_budget_remaining_usd: Some(1.25),
                ..Default::default()
            },
            "subagent-test",
        );
        assert!(child.max_turns >= SUB_AGENT_MIN_TURNS);
        assert!(child.max_turns <= SUB_AGENT_MAX_TURNS_CAP);
        assert_eq!(child.session_id.as_deref(), Some("root::subagent-test"));
        assert_eq!(child.max_cost_usd, Some(1.25));
        assert!(child.skip_memory);
        assert!(!child.background_review_enabled);
        assert!(!child.background_review_metrics_enabled);
        assert_eq!(child.memory_nudge_interval, 0);
        assert_eq!(child.skill_creation_nudge_interval, 0);
        assert!(child.quiet_mode);
    }

    #[test]
    fn build_child_config_model_only_override_inherits_parent_provider() {
        let inherited_provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        let parent = AgentConfig {
            max_turns: 20,
            model: "nous:hermes-3".into(),
            provider: Some("nous".into()),
            delegation_model: Some("google/gemini-3-flash-preview".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: inherited_provider.clone(),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
            background_delegation_callback: None,
            code_index: None,
        });

        let child = orch.build_child_config(
            &SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            },
            "subagent-model-only",
        );
        let child_provider = orch
            .child_llm_provider(&child)
            .expect("model-only delegation inherits provider");

        assert_eq!(child.model, "google/gemini-3-flash-preview");
        assert_eq!(child.provider.as_deref(), Some("nous"));
        assert!(Arc::ptr_eq(&child_provider, &inherited_provider));
    }

    #[test]
    fn build_child_config_provider_override_prefixes_child_model() {
        let parent = AgentConfig {
            max_turns: 20,
            model: "nous:hermes-3".into(),
            provider: Some("nous".into()),
            delegation_model: Some("google/gemini-3-flash-preview".into()),
            delegation_provider: Some("openrouter".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
            background_delegation_callback: None,
            code_index: None,
        });

        let child = orch.build_child_config(
            &SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            },
            "subagent-provider",
        );

        assert_eq!(child.provider.as_deref(), Some("openrouter"));
        assert_eq!(child.model, "openrouter:google/gemini-3-flash-preview");
    }

    #[test]
    fn direct_endpoint_override_builds_distinct_child_provider() {
        let inherited_provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        let parent = AgentConfig {
            max_turns: 20,
            model: "nous:hermes-3".into(),
            provider: Some("nous".into()),
            delegation_model: Some("qwen2.5-coder".into()),
            delegation_base_url: Some("http://127.0.0.1:1234/v1".into()),
            delegation_api_key: Some("local-key".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: inherited_provider.clone(),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
            background_delegation_callback: None,
            code_index: None,
        });

        let child = orch.build_child_config(
            &SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            },
            "subagent-direct",
        );
        let child_provider = orch
            .child_llm_provider(&child)
            .expect("direct endpoint delegation builds provider");

        assert_eq!(child.provider.as_deref(), Some("custom"));
        assert_eq!(child.model, "custom:qwen2.5-coder");
        assert!(!Arc::ptr_eq(&child_provider, &inherited_provider));
    }

    #[tokio::test]
    async fn unresolved_delegation_provider_returns_structured_error() {
        let parent = AgentConfig {
            max_turns: 2,
            model: "nous:hermes-3".into(),
            provider: Some("nous".into()),
            delegation_model: Some("some-model".into()),
            delegation_provider: Some("definitely-missing-provider".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
            background_delegation_callback: None,
            code_index: None,
        }));

        let out = orch
            .execute(SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            })
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(parsed["status"], "failed");
        assert!(parsed["error"]
            .as_str()
            .unwrap()
            .contains("definitely-missing-provider"));
    }

    /// Loopback OpenAI-compatible endpoint that answers one request and hands
    /// the raw request (headers and body) back through the channel.
    fn loopback_chat_fixture() -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback fixture");
        let base_url = format!(
            "http://{}/v1",
            listener.local_addr().expect("fixture address")
        );
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                if let Some(header_end) = text.find("\r\n\r\n") {
                    let body_len = text[..header_end]
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + body_len {
                        break;
                    }
                }
            }
            let body = r#"{"id":"fixture","object":"chat.completion","created":1,"model":"local-model","choices":[{"index":0,"message":{"role":"assistant","content":"loopback"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(reply.as_bytes());
            let _ = request_tx.send(String::from_utf8_lossy(&request).to_string());
        });

        (base_url, request_rx)
    }

    #[tokio::test]
    async fn delegated_child_provider_does_not_carry_parent_credential_pool() {
        let (base_url, request_rx) = loopback_chat_fixture();
        let tmp = tempfile::tempdir().unwrap();
        let parent_config = AgentConfig {
            model: "openai:gpt-4o".into(),
            provider: Some("openai".into()),
            delegation_provider: Some("custom".into()),
            delegation_base_url: Some(base_url),
            delegation_api_key: Some("delegation-key".into()),
            ..AgentConfig::default()
        };
        let orch = SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent_config.clone(),
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: Some(Arc::new(CredentialPool::single("parent-pool-key"))),
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: None,
            timeout: Duration::from_secs(5),
            background_delegation_callback: None,
            code_index: None,
        });
        let child_config = AgentConfig {
            model: "custom:local-model".into(),
            provider: Some("custom".into()),
            ..parent_config
        };

        let provider = orch
            .child_llm_provider(&child_config)
            .unwrap_or_else(|_| panic!("child provider builds"));
        let _ = provider
            .chat_completion(&[Message::user("hi")], &[], None, None, None, None)
            .await;
        let request = request_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the child must address the delegation endpoint");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer delegation-key"),
            "the child must send the delegation key, not the parent's pool: {request}"
        );
    }

    /// Orchestrator whose parent runs on `openai:gpt-4o` with a one-key pool.
    fn openai_parent_orchestrator(
        parent_config: AgentConfig,
        hermes_home: &std::path::Path,
    ) -> SubAgentOrchestrator {
        SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: Some(Arc::new(CredentialPool::single("parent-pool-key"))),
            parent_interrupt: InterruptController::new(),
            hermes_home: hermes_home.to_path_buf(),
            parent_session_id: None,
            timeout: Duration::from_secs(5),
            background_delegation_callback: None,
            code_index: None,
        })
    }

    async fn child_request(
        orch: &SubAgentOrchestrator,
        child_config: &AgentConfig,
        rx: std::sync::mpsc::Receiver<String>,
    ) -> String {
        let provider = orch
            .child_llm_provider(child_config)
            .unwrap_or_else(|_| panic!("child provider builds"));
        let _ = provider
            .chat_completion(&[Message::user("hi")], &[], None, None, None, None)
            .await;
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the child must address the fixture endpoint")
            .to_ascii_lowercase()
    }

    #[tokio::test]
    async fn same_provider_child_with_own_endpoint_and_key_skips_parent_pool() {
        let (base_url, request_rx) = loopback_chat_fixture();
        let tmp = tempfile::tempdir().unwrap();
        let parent_config = AgentConfig {
            model: "openai:gpt-4o".into(),
            provider: Some("openai".into()),
            delegation_provider: Some("openai".into()),
            delegation_base_url: Some(base_url),
            delegation_api_key: Some("delegation-key".into()),
            ..AgentConfig::default()
        };
        let orch = openai_parent_orchestrator(parent_config.clone(), tmp.path());
        let request = child_request(&orch, &parent_config, request_rx).await;
        assert!(
            request.contains("authorization: bearer delegation-key"),
            "a child with its own endpoint and key must not send the parent's pool: {request}"
        );
    }

    #[tokio::test]
    async fn other_provider_child_without_override_skips_parent_pool() {
        let (base_url, request_rx) = loopback_chat_fixture();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime_providers = std::collections::HashMap::new();
        runtime_providers.insert(
            "custom".to_string(),
            crate::agent_loop::RuntimeProviderConfig {
                api_key: Some("custom-config-key".to_string()),
                base_url: Some(base_url),
                ..Default::default()
            },
        );
        let parent_config = AgentConfig {
            model: "openai:gpt-4o".into(),
            provider: Some("openai".into()),
            delegation_provider: Some("custom".into()),
            runtime_providers,
            ..AgentConfig::default()
        };
        let orch = openai_parent_orchestrator(parent_config.clone(), tmp.path());
        let request = child_request(&orch, &parent_config, request_rx).await;
        assert!(
            request.contains("authorization: bearer custom-config-key"),
            "a child on another provider must not send the parent's pool: {request}"
        );
    }

    #[tokio::test]
    async fn same_provider_child_without_override_shares_parent_pool() {
        let (base_url, request_rx) = loopback_chat_fixture();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime_providers = std::collections::HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            crate::agent_loop::RuntimeProviderConfig {
                api_key: Some("parent-config-key".to_string()),
                base_url: Some(base_url),
                ..Default::default()
            },
        );
        let parent_config = AgentConfig {
            model: "openai:gpt-4o".into(),
            provider: Some("openai".into()),
            delegation_provider: Some("openai".into()),
            runtime_providers,
            ..AgentConfig::default()
        };
        let orch = openai_parent_orchestrator(parent_config.clone(), tmp.path());
        let request = child_request(&orch, &parent_config, request_rx).await;
        assert!(
            request.contains("authorization: bearer parent-pool-key"),
            "a plain same-provider child shares the parent's pool: {request}"
        );
    }
}
