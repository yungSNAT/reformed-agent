use super::*;
use crate::agent_loop::{AgentCallbacks, AgentConfig, AgentLoop, ToolRegistry};
use crate::sub_agent_orchestrator::{SubAgentOrchestrator, SubAgentRequest};
use futures::stream::{BoxStream, StreamExt};
use hermes_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Recording {
    seen: Mutex<Vec<Vec<Message>>>,
}

#[async_trait::async_trait]
impl LlmProvider for Recording {
    async fn chat_completion(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        self.seen.lock().unwrap().push(messages.to_vec());
        Ok(LlmResponse {
            message: Message::assistant("child answer"),
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

fn record(id: &str, session: &str, status: SubAgentStatus) -> SubAgentLineage {
    serde_json::from_value(serde_json::json!({
        "sub_agent_id": id, "parent_session_id": session, "task": format!("task {id}"),
        "depth": 1, "max_depth": 4, "status": status,
        "started_at": "2026-11-01T00:00:00Z", "delivered": false,
        "result": format!("answer {id}"),
    }))
    .unwrap()
}

fn read_opt(home: &Path, id: &str) -> Option<SubAgentLineage> {
    let body = std::fs::read(lineage_dir(home).join(format!("{id}.json"))).ok()?;
    serde_json::from_slice(&body).ok()
}

fn read(home: &Path, id: &str) -> SubAgentLineage {
    read_opt(home, id).expect("lineage record")
}

fn parent(session: &str, home: &Path, provider: Arc<Recording>) -> AgentLoop {
    let config = AgentConfig {
        session_id: Some(session.into()),
        skip_memory: true,
        skip_context_files: true,
        hermes_home: Some(home.to_string_lossy().into_owned()),
        max_turns: 2,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), provider);
    let orch = Arc::new(SubAgentOrchestrator::from_parent(
        &agent,
        home.to_path_buf(),
    ));
    agent.with_sub_agent_orchestrator(orch)
}

fn child_id(out: &str) -> String {
    serde_json::from_str::<serde_json::Value>(out).unwrap()["sub_agent_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn lists_only_this_sessions_undelivered_results() {
    let home = tempfile::tempdir().unwrap();
    write_lineage(home.path(), &record("a", "s1", SubAgentStatus::Completed));
    write_lineage(home.path(), &record("b", "s2", SubAgentStatus::Completed));
    let mut legacy = record("c", "s1", SubAgentStatus::Completed);
    legacy.delivered = None;
    write_lineage(home.path(), &legacy);
    let mut failed = record("d", "s1", SubAgentStatus::Failed);
    failed.result = None;
    failed.error = Some("provider down".into());
    write_lineage(home.path(), &failed);

    let note = recover(home.path(), "s1", Duration::from_secs(600)).unwrap();

    let expected = [
        RECOVERED_RESULTS_HEADING,
        "- [completed] task a (a): answer a",
        "- [failed] task d (d): error: provider down",
    ]
    .join("\n");
    assert_eq!(note, expected);
}

#[test]
fn a_started_record_past_its_limit_is_reported_interrupted() {
    let home = tempfile::tempdir().unwrap();
    write_lineage(home.path(), &record("old", "s", SubAgentStatus::Started));
    let mut live = record("live", "s", SubAgentStatus::Started);
    live.started_at = Timestamp::now();
    write_lineage(home.path(), &live);

    let note = recover(home.path(), "s", Duration::from_secs(600)).unwrap();

    assert!(
        note.contains("- [interrupted] task old (old): answer old"),
        "{note}"
    );
    assert!(!note.contains("live"), "{note}");
    let old = read(home.path(), "old");
    assert_eq!(old.status, SubAgentStatus::Interrupted);
    assert!(old.ended_at.is_some());
    assert_eq!(read(home.path(), "live").status, SubAgentStatus::Started);
}

#[test]
fn delivered_records_are_not_recovered_again() {
    let home = tempfile::tempdir().unwrap();
    write_lineage(home.path(), &record("a", "s", SubAgentStatus::Completed));
    let mut live = record("live", "s", SubAgentStatus::Started);
    live.started_at = Timestamp::now();
    write_lineage(home.path(), &live);

    mark_delivered(home.path(), "s");

    assert_eq!(read(home.path(), "a").delivered, Some(true));
    assert_eq!(read(home.path(), "live").delivered, Some(false));
    assert_eq!(recover(home.path(), "s", Duration::from_secs(600)), None);
}

#[tokio::test]
async fn a_child_keeps_its_answer_until_a_parent_turn_finishes() {
    let home = tempfile::tempdir().unwrap();
    let agent = parent("s", home.path(), Arc::new(Recording::default()));
    let orch = Arc::new(SubAgentOrchestrator::from_parent(
        &agent,
        home.path().to_path_buf(),
    ));

    let out = orch
        .execute(SubAgentRequest {
            task: "look".into(),
            child_depth: 1,
            max_depth: 4,
            ..Default::default()
        })
        .await;

    let kept = read(home.path(), &child_id(&out));
    assert_eq!(kept.result.as_deref(), Some("child answer"));
    assert_eq!(kept.delivered, Some(false));
}

#[tokio::test]
async fn the_next_run_receives_results_an_unfinished_turn_lost() {
    let home = tempfile::tempdir().unwrap();
    write_lineage(home.path(), &record("lost", "s", SubAgentStatus::Completed));
    let provider = Arc::new(Recording::default());
    let agent = parent("s", home.path(), provider.clone());

    agent
        .run(vec![Message::user("continue")], None)
        .await
        .unwrap();
    let first_run_requests = provider.seen.lock().unwrap().len();
    agent.run(vec![Message::user("again")], None).await.unwrap();

    let seen = provider.seen.lock().unwrap();
    let has_note = |messages: &[Message]| {
        messages.iter().any(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.starts_with(RECOVERED_RESULTS_HEADING))
        })
    };
    assert!(
        has_note(&seen[0]),
        "the first run gets the lost result: {:?}",
        seen[0]
    );
    assert!(seen[0].iter().any(|m| m
        .content
        .as_deref()
        .is_some_and(|c| c.contains("task lost (lost): answer lost"))));
    assert!(
        !has_note(&seen[first_run_requests]),
        "a finished run delivered it"
    );
    assert_eq!(read(home.path(), "lost").delivered, Some(true));
}

#[tokio::test]
async fn a_background_result_its_callback_received_is_delivered() {
    let home = tempfile::tempdir().unwrap();
    let heard = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = heard.clone();
    let mut agent = parent("s", home.path(), Arc::new(Recording::default()));
    agent.callbacks = Arc::new(AgentCallbacks {
        background_delegation_callback: Some(Arc::new(move |s: &str| {
            sink.lock().unwrap().push(s.to_string())
        })),
        ..Default::default()
    });
    let orch = Arc::new(SubAgentOrchestrator::from_parent(
        &agent,
        home.path().to_path_buf(),
    ));

    let out = orch
        .execute(SubAgentRequest {
            task: "later".into(),
            child_depth: 1,
            max_depth: 4,
            background: true,
            ..Default::default()
        })
        .await;
    let id = child_id(&out);
    for _ in 0..400 {
        if read_opt(home.path(), &id).is_some_and(|r| r.delivered == Some(true)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert_eq!(heard.lock().unwrap().len(), 1);
    assert_eq!(read(home.path(), &id).delivered, Some(true));
}
