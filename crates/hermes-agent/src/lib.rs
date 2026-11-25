//! # hermes-agent
//!
//! Core agent loop engine — orchestrates LLM calls, tool execution, and
//! context management into a fully autonomous loop that runs until the
//! model finishes naturally or the turn budget is exhausted.

pub mod agent_loop;
pub mod api_bridge;
pub mod auxiliary_builder;
pub mod bedrock;
pub mod budget;
pub mod code_index;
pub mod coding_context;
pub mod compression;
pub mod context;
pub mod context_files;
pub mod context_references;
pub mod credential_pool;
pub mod embed;
pub mod fallback;
pub mod gemini_schema;
pub mod honcho_provider;
pub mod interrupt;
pub mod local_backends;
pub mod lsp_context;
pub mod memory_manager;
pub mod memory_plugins;
pub mod model_normalize;
pub mod oauth;
pub mod oneshot;
pub mod plugins;
pub mod provider;
pub mod provider_profiles;
pub mod providers_extra;
pub mod python_alignment;
pub mod rate_limit;
pub mod reasoning;
pub mod session_persistence;
pub mod skill_orchestrator;
pub mod smart_model_routing;
mod sse_buffer;
pub mod steer;
pub mod sub_agent_orchestrator;
pub mod subdirectory_hints;
pub mod tool_call_args;
pub mod vertex;

// Re-export primary agent types
pub use agent_loop::{
    AgentCallbacks, AgentConfig, AgentLoop, ApiMode, CheapModelRouteConfig, ErrorClass,
    RetryConfig, SmartModelRoutingConfig, TurnMetrics,
};

// Re-export context management
pub use compression::summarize_messages_with_llm;
pub use context::{
    builtin_personality_descriptions, builtin_personality_names, ensure_default_soul_md,
    is_legacy_template_soul, load_context_files, load_soul_md, load_soul_md_from,
    load_soul_md_from_home, switch_personality, ContextManager, SoulSeedOutcome,
    SystemPromptBuilder,
};

// Re-export budget enforcement
pub use budget::{check_aggregate_budget, enforce_budget, truncate_result};

// Re-export LLM providers
pub use api_bridge::CodexProvider;
pub use auxiliary_builder::{
    build_auxiliary_client_with_main_runtime, build_default_auxiliary_client, AuxiliaryMainRuntime,
    AuxiliaryWiringSummary,
};
pub use bedrock::BedrockProvider;
pub use hermes_intelligence::auxiliary::AuxiliaryConfig;
pub use oneshot::{
    render_template as render_oneshot_template, run_oneshot_with_client, strip_wrapping_code_fence,
    truncate_template_text, OneShotError, OneShotRequest,
};
pub use provider::{AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider};
pub use providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};
pub use vertex::VertexProvider;

// Re-export rate limiting, credential pool, and fallback chain
pub use credential_pool::CredentialPool;
pub use fallback::FallbackChain;
pub use oauth::{OAuthManager, OAuthToken, TokenFetcher};
pub use rate_limit::RateLimitTracker;

// Re-export reasoning parser
pub use reasoning::parse_reasoning;

// Re-export interrupt controller
pub use interrupt::InterruptController;
pub use steer::{format_steer_marker, STEER_CHANNEL_NOTE, STEER_MARKER_CLOSE, STEER_MARKER_OPEN};

// Re-export memory manager
pub use memory_manager::{
    build_memory_context_block, sanitize_context, MemoryManager, MemoryProviderPlugin,
    StreamingContextScrubber,
};

// Re-export plugin system
pub use plugins::{Plugin, PluginManager, PluginMeta};

// Re-export skill orchestrator
pub use skill_orchestrator::SkillOrchestrator;

// Re-export session persistence
pub use session_persistence::{leading_system_prompt_for_persist, SessionPersistence};

// Re-export context files
pub use code_index::{CodeIndex, CodeIndexConfig, IndexStats, ReferenceHit, SymbolInfo};
pub use context_files::{load_hermes_context_files, load_workspace_context, scan_context_content};
pub use context_references::{
    parse_context_references, preprocess_context_references_async, ContextReference,
    ContextReferenceResult,
};
pub use lsp_context::{build_lsp_context_note, LspContextConfig};

// Re-export subdirectory hints
pub use subdirectory_hints::{generate_project_hints, SubdirectoryHintTracker};

// Python `run_agent.py` alignment helpers (budget strip/inject, surrogate sanitize)
pub use python_alignment::{
    budget_pressure_text, inject_budget_pressure_into_last_tool_result,
    looks_like_codex_intermediate_ack, sanitize_surrogates, strip_budget_warnings_from_messages,
    strip_think_blocks_for_ack, CODEX_CONTINUE_USER_MESSAGE,
};

// Re-export sub-agent orchestrator
pub use sub_agent_orchestrator::{
    SubAgentLineage, SubAgentOrchestrator, SubAgentOrchestratorConfig, SubAgentRequest,
    SubAgentStatus,
};

// Re-export honcho provider
pub use honcho_provider::HonchoProvider;

// Re-export core types that consumers need
pub use hermes_core::{
    AgentError, AgentResult, BudgetConfig, LlmProvider, Message, StreamChunk, ToolCall, ToolError,
    ToolResult, ToolSchema, UsageStats,
};

/// The Hermes home the file tools' write rule protects and `hermes memory
/// setup` writes, so the plugins read their configs from the same place.
fn default_memory_home() -> String {
    hermes_config::hermes_home().to_string_lossy().into_owned()
}

/// Memory data and plugin configs the plugins keep in the memory home.
const MEMORY_HOME_FILES: &[&str] = &[
    "memory_store.db",
    "retaindb_queue.db",
    "honcho.json",
    "hindsight/config.json",
    "openviking.json",
    "mem0.json",
    "supermemory.json",
    "contextlattice.json",
    "byterover.json",
    "byterover",
];

/// With `HOME` unset the plugins used to read `.hermes-agent-ultra` in the
/// working folder. A notice when that folder holds their data or configs and
/// is not the memory home, so the data is moved rather than lost from view.
fn stranded_memory_notice(cwd: &std::path::Path, home: &std::path::Path) -> Option<String> {
    let local = cwd.join(".hermes-agent-ultra");
    let same = |a: &std::path::Path, b: &std::path::Path| {
        a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
    };
    if !local.is_dir() || same(&local, home) {
        return None;
    }
    MEMORY_HOME_FILES
        .iter()
        .any(|name| local.join(name).exists())
        .then(|| {
            format!(
                "Memory data or plugin configs in {} are no longer read; the memory home is {}. If they are yours, move them there, or set HERMES_HOME={} to keep using that folder; if this folder came with a repository you cloned, leave it.",
                local.display(),
                home.display(),
                local.display()
            )
        })
}

/// Logs the stranded memory notice the first time it applies; true when it did.
fn warn_stranded_memory_once(
    warned: &std::sync::atomic::AtomicBool,
    cwd: &std::path::Path,
    home: &std::path::Path,
) -> bool {
    let Some(notice) = stranded_memory_notice(cwd, home) else {
        return false;
    };
    if warned.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    tracing::warn!("{notice}");
    true
}

/// Attach discovered external memory providers to an `AgentLoop`.
///
/// This keeps ContextLattice-first and multi-provider memory behavior enabled
/// consistently across CLI, gateway, HTTP, and cron execution surfaces.
pub fn attach_discovered_memory(mut agent: AgentLoop) -> AgentLoop {
    if agent.config.skip_memory {
        return agent;
    }
    let session_id = agent
        .config
        .session_id
        .clone()
        .unwrap_or_else(|| "session-default".to_string());
    if agent.config.hermes_home.is_none() {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if let Ok(cwd) = std::env::current_dir() {
            warn_stranded_memory_once(&WARNED, &cwd, &hermes_config::hermes_home());
        }
    }
    let hermes_home = agent
        .config
        .hermes_home
        .clone()
        .unwrap_or_else(default_memory_home);
    if let Some(manager) = memory_plugins::build_initialized_memory_manager(
        &session_id,
        &hermes_home,
        agent.config.memory_nudge_interval,
    ) {
        agent = agent.with_memory(manager);
    }
    agent
}

#[cfg(test)]
mod stranded_memory_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn a_working_folder_holding_memory_data_is_named_once() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        let home = tmp.path().join("home/.hermes-agent-ultra");
        std::fs::create_dir_all(cwd.join(".hermes-agent-ultra")).unwrap();
        std::fs::write(cwd.join(".hermes-agent-ultra/memory_store.db"), "").unwrap();
        let notice = stranded_memory_notice(&cwd, &home).expect("notice");
        assert!(notice.contains(&cwd.join(".hermes-agent-ultra").display().to_string()));
        assert!(notice.contains(&home.display().to_string()));
        assert!(notice.contains("If they are yours, move them there"));
        assert!(notice.contains(&format!(
            "HERMES_HOME={}",
            cwd.join(".hermes-agent-ultra").display()
        )));
        assert!(notice.contains("came with a repository you cloned, leave it"));

        let warned = AtomicBool::new(false);
        assert!(warn_stranded_memory_once(&warned, &cwd, &home));
        assert!(!warn_stranded_memory_once(&warned, &cwd, &home));
    }

    #[test]
    fn byterover_settings_or_data_alone_are_named() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home/.hermes-agent-ultra");
        let settings = tmp.path().join("settings");
        std::fs::create_dir_all(settings.join(".hermes-agent-ultra")).unwrap();
        std::fs::write(settings.join(".hermes-agent-ultra/byterover.json"), "{}").unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(data.join(".hermes-agent-ultra/byterover")).unwrap();
        for cwd in [settings, data] {
            let local = cwd.join(".hermes-agent-ultra").display().to_string();
            let notice = stranded_memory_notice(&cwd, &home).expect("notice");
            assert!(notice.contains(&local), "{notice}");
        }
    }

    #[test]
    fn no_notice_without_stranded_data_or_when_the_folder_is_the_home() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        let local = cwd.join(".hermes-agent-ultra");
        let home = tmp.path().join("home/.hermes-agent-ultra");
        assert_eq!(stranded_memory_notice(&cwd, &home), None, "no folder");

        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("notes.txt"), "").unwrap();
        assert_eq!(
            stranded_memory_notice(&cwd, &home),
            None,
            "nothing stranded"
        );

        std::fs::write(local.join("mem0.json"), "{}").unwrap();
        assert!(
            stranded_memory_notice(&cwd, &home).is_some(),
            "a plugin config"
        );
        assert_eq!(
            stranded_memory_notice(&cwd, &local),
            None,
            "the home itself"
        );

        let warned = AtomicBool::new(false);
        assert!(!warn_stranded_memory_once(&warned, &cwd, &local));
        assert!(
            warn_stranded_memory_once(&warned, &cwd, &home),
            "flag kept for a real notice"
        );
    }
}
