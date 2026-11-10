//! A cron script asks the profile's tool guard before it runs, as a model can
//! schedule one through the cronjob tool. One test, as it sets `HERMES_HOME`
//! for this process.

use std::sync::Arc;

use futures::stream::{BoxStream, StreamExt};
use hermes_agent::agent_loop::ToolRegistry;
use hermes_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema};
use hermes_cron::job::CronJob;
use hermes_cron::runner::CronRunner;

const DENY: &str = r#"{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"guard says no"}}"#;

/// Script-only jobs never call the model.
struct NoModel;

#[async_trait::async_trait]
impl LlmProvider for NoModel {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        Err(AgentError::LlmApi(
            "script-only job called the model".into(),
        ))
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
async fn guard_decides_before_a_cron_script_runs() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::env::set_var("HERMES_HOME", home.path());
    // The guard sources `answer.sh`, so each step changes the answer, not the config.
    let answer = home.path().join("answer.sh");
    let config = home.path().join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "security:\n  tool_guard:\n    command: sh\n    args: ['-c', 'cat >/dev/null; . \"$0\"', '{}']\n",
            answer.display()
        ),
    )
    .unwrap();
    let runner = CronRunner::new(Arc::new(NoModel), Arc::new(ToolRegistry::new()));
    let mut job = CronJob::new("* * * * *", "unused");
    job.no_agent = true;
    job.script = Some("touch ran".into());
    job.script_shell = Some("sh".into());
    job.workdir = Some(work.path().to_str().unwrap().into());
    let ran = work.path().join("ran");

    std::fs::write(&answer, format!("printf '%s' '{DENY}'")).unwrap();
    for config_text in [None, Some("security: {}\n")] {
        // The second pass is a model that rewrote the config without the guard.
        if let Some(text) = config_text {
            std::fs::write(&config, text).unwrap();
        }
        let error = runner.run_job(&job).await.unwrap_err().to_string();
        assert!(error.contains("guard says no"), "{error}");
        assert!(!ran.exists(), "a refused cron script ran");
    }

    std::fs::write(&answer, "").unwrap();
    runner.run_job(&job).await.unwrap();
    assert!(ran.exists());
}
