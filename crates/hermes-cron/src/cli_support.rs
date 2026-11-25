//! Helpers for CLI / tooling: build a [`CronScheduler`] backed by disk + a minimal LLM.
//!
//! This fallback scheduler is suitable for CRUD/status operations and tests.
//! Real execution paths should inject a live [`hermes_core::LlmProvider`].

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use hermes_agent::agent_loop::ToolRegistry;
use hermes_core::{
    AgentError, LlmProvider, LlmResponse, Message, StreamChunk, StreamDelta, ToolSchema,
};

use crate::persistence::FileJobPersistence;
use crate::runner::CronRunner;
use crate::scheduler::CronScheduler;

/// Minimal LLM used when the CLI constructs a scheduler without a real provider.
#[derive(Debug)]
pub struct MinimalCronLlm;

#[async_trait]
impl LlmProvider for MinimalCronLlm {
    async fn chat_completion(
        &self,
        _messages: &[hermes_core::Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        Ok(LlmResponse {
            message: Message::assistant(
                "CLI cron manual run (fallback LLM path — configure gateway for real execution).",
            ),
            usage: None,
            model: model.unwrap_or("fallback").to_string(),
            finish_reason: Some("stop".into()),
        })
    }

    fn chat_completion_stream(
        &self,
        _messages: &[hermes_core::Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        stream::iter(vec![
            Ok(StreamChunk {
                delta: Some(StreamDelta {
                    content: Some("ok".into()),
                    tool_calls: None,
                    extra: None,
                }),
                finish_reason: None,
                usage: None,
            }),
            Ok(StreamChunk {
                delta: None,
                finish_reason: Some("stop".into()),
                usage: None,
            }),
        ])
        .boxed()
    }
}

/// Build a scheduler that persists jobs under `data_dir` (e.g. `$HERMES_HOME/cron`).
pub fn cron_scheduler_for_data_dir(data_dir: PathBuf) -> CronScheduler {
    let persistence = Arc::new(FileJobPersistence::with_dir(data_dir));
    let llm: Arc<dyn LlmProvider> = Arc::new(MinimalCronLlm);
    let runner = Arc::new(CronRunner::new(llm, Arc::new(ToolRegistry::new())));
    CronScheduler::new(persistence, runner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fallback_scheduler_starts_and_stops_idempotently_without_jobs() {
        let state = tempfile::tempdir().unwrap();
        let scheduler = cron_scheduler_for_data_dir(state.path().to_path_buf());
        scheduler.load_persisted_jobs().await.unwrap();
        assert!(scheduler.list_jobs().await.is_empty());
        assert!(!scheduler.is_running().await);

        scheduler.start().await;
        scheduler.start().await;
        assert!(scheduler.is_running().await);
        tokio::task::yield_now().await;
        assert_eq!(scheduler.tick_due_jobs().await, 0);

        scheduler.stop().await;
        scheduler.stop().await;
        assert!(!scheduler.is_running().await);
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn fallback_cron_llm_returns_model_labels_and_a_terminal_stream_chunk() {
        let llm = MinimalCronLlm;
        for (requested, expected) in [(None, "fallback"), (Some("fixture-model"), "fixture-model")]
        {
            let result = llm
                .chat_completion(&[], &[], None, None, requested, None)
                .await
                .unwrap();
            assert_eq!(result.model, expected);
            assert_eq!(result.finish_reason.as_deref(), Some("stop"));
            assert!(result
                .message
                .content
                .as_deref()
                .unwrap()
                .contains("fallback LLM path"));
        }

        let chunks = llm
            .chat_completion_stream(&[], &[], None, None, None, None)
            .collect::<Vec<_>>()
            .await;
        assert_eq!(chunks.len(), 2);
        let first = chunks[0].as_ref().unwrap();
        let terminal = chunks[1].as_ref().unwrap();
        assert_eq!(first.delta.as_ref().unwrap().content.as_deref(), Some("ok"));
        assert!(first.finish_reason.is_none());
        assert!(terminal.delta.is_none());
        assert_eq!(terminal.finish_reason.as_deref(), Some("stop"));
    }
}
