use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::Stream;
use hermes_core::{LlmProvider, LlmResponse, StreamChunk, ToolSchema};

use super::*;
use crate::kanban::{add_task, create_or_select_board, KanbanStore, NewKanbanTaskInput};

fn add_triage(store: &mut KanbanStore, tenant: &str) -> KanbanTask {
    add_task(
        ensure_board(store, None),
        NewKanbanTaskInput {
            title: "Clarify the import".to_string(),
            lane: KanbanLane::Triage,
            priority: 3,
            assignee: None,
            tenant: Some(tenant.to_string()),
            description: Some("Validate the input file.".to_string()),
            depends_on: Vec::new(),
            goal_mode: false,
            goal_max_turns: None,
        },
    )
}

struct Specifier {
    reply: &'static str,
    on_call: Option<Box<dyn Fn() + Send + Sync>>,
}

fn test_client(
    reply: &'static str,
    on_call: Option<Box<dyn Fn() + Send + Sync>>,
) -> AuxiliaryClient {
    AuxiliaryClient::builder()
        .config(AuxiliaryConfig {
            consult_env: false,
            ..AuxiliaryConfig::default()
        })
        .add_candidate(ProviderCandidate::new(
            AuxiliarySource::Anthropic,
            "specifier",
            Arc::new(Specifier { reply, on_call }),
        ))
        .build()
}

#[async_trait]
impl LlmProvider for Specifier {
    async fn chat_completion(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        output_limit: Option<u32>,
        _temperature: Option<f64>,
        model: Option<&str>,
        extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        assert!(tools.is_empty());
        assert_eq!(output_limit, Some(1024));
        let input: Value = serde_json::from_str(messages[1].content.as_deref().unwrap()).unwrap();
        assert_eq!(
            input,
            json!({
                "title": "Clarify the import",
                "body": "Validate the input file.",
            })
        );
        assert_eq!(extra_body, Some(&json!({"test_option": true})));
        if let Some(on_call) = &self.on_call {
            on_call();
        }
        Ok(LlmResponse {
            message: Message::assistant(self.reply),
            finish_reason: Some("stop".into()),
            model: model.unwrap_or("specifier").to_string(),
            usage: None,
        })
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _output_limit: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamChunk, AgentError>> + Send + 'static>> {
        Box::pin(futures::stream::empty())
    }
}

#[tokio::test]
async fn kanban_specify_tenant_sweep_only_promotes_valid_replies() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("boards.json");
    let lock = KanbanStoreLock::open(path.clone()).unwrap();
    let mut store = lock.load().unwrap();
    let selected = add_triage(&mut store, "imports");
    let unrelated = add_triage(&mut store, "reports");
    lock.save(&store).unwrap();
    for (reply, expected_lane) in [
        (r#"{"title":"ignored","body":[]}"#, KanbanLane::Triage),
        (
            r#"{"title":"Validate CSV imports","body":"Reject invalid rows."}"#,
            KanbanLane::Todo,
        ),
    ] {
        let client = test_client(reply, None);
        let output = specify_board(
            path.clone(),
            Selection::parse(&["--all", "--tenant", "imports"]).unwrap(),
            &client,
            Some(json!({"test_option": true})),
        )
        .await
        .unwrap();
        assert!(output.starts_with(&selected.id));
        assert!(!output.contains(&unrelated.id));
        let store = KanbanStoreLock::open(path.clone()).unwrap().load().unwrap();
        let tasks = &store.boards[0].tasks;
        assert_eq!(tasks[0].lane, expected_lane);
        if expected_lane == KanbanLane::Todo {
            assert_eq!(tasks[0].title, "Validate CSV imports");
            assert_eq!(
                tasks[0].description.as_deref(),
                Some("Reject invalid rows.")
            );
        } else {
            assert_eq!(tasks[0], selected);
        }
        assert_eq!(tasks[1], unrelated);
    }
}

#[tokio::test]
async fn kanban_specify_does_not_send_a_later_task_removed_during_the_sweep() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("boards.json");
    let lock = KanbanStoreLock::open(path.clone()).unwrap();
    let mut store = lock.load().unwrap();
    add_triage(&mut store, "imports");
    add_triage(&mut store, "imports");
    lock.save(&store).unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_provider = Arc::clone(&calls);
    let path_for_provider = path.clone();
    let client = test_client(
        r#"{"body":"Verify each imported row."}"#,
        Some(Box::new(move || {
            assert_eq!(calls_for_provider.fetch_add(1, Ordering::Relaxed), 0);
            {
                let sidecar = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path_for_provider.with_extension("lock"))
                    .unwrap();
                sidecar.try_lock().expect("lock released before inference");
            }
            let lock = KanbanStoreLock::open(path_for_provider.clone()).unwrap();
            let mut store = lock.load().unwrap();
            move_task(&mut store.boards[0].tasks[1], KanbanLane::Blocked, None);
            lock.save(&store).unwrap();
        })),
    );
    let output = specify_board(
        path.clone(),
        Selection::All(None),
        &client,
        Some(json!({"test_option": true})),
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(output.contains("K-0002: task changed before specification"));
    let store = KanbanStoreLock::open(path).unwrap().load().unwrap();
    assert_eq!(store.boards[0].tasks[0].lane, KanbanLane::Todo);
    assert_eq!(store.boards[0].tasks[1].lane, KanbanLane::Blocked);
}

#[test]
fn kanban_specify_preserves_concurrent_edits_and_other_boards() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("boards.json");
    let lock = KanbanStoreLock::open(path.clone()).unwrap();
    let mut store = lock.load().unwrap();
    let original = add_triage(&mut store, "imports");
    lock.save(&store).unwrap();

    let (board_id, tasks) = snapshot_triage(&path, &Selection::Task(original.id.clone())).unwrap();
    let lock = KanbanStoreLock::open(path.clone()).unwrap();
    let mut store = lock.load().unwrap();
    store.boards[0].tasks[0].description = Some("Edited during specification.".into());
    create_or_select_board(&mut store, "Other", None);
    let other_task = add_triage(&mut store, "imports");
    assert_eq!(other_task.id, original.id);
    lock.save(&store).unwrap();

    assert!(!promote(
        &path,
        &board_id,
        &tasks[0],
        parse_reply(r#"{"body":"New specification."}"#).unwrap()
    )
    .unwrap());
    let store = KanbanStoreLock::open(path).unwrap().load().unwrap();
    let task = &store.boards[0].tasks[0];
    assert_eq!(task.lane, KanbanLane::Triage);
    assert_eq!(
        task.description.as_deref(),
        Some("Edited during specification.")
    );
    assert_eq!(store.boards[1].tasks[0], other_task);
}

#[test]
fn kanban_specify_validates_output_without_clearing_unspecified_fields() {
    let spec = parse_reply("```json\n{\"title\":\"Revised title\"}\n```").unwrap();
    assert_eq!(spec.title.as_deref(), Some("Revised title"));
    assert!(spec.body.is_none());
    assert!(parse_reply("Plain prose").is_none());
    assert!(parse_reply(r#"["a title","a body"]"#).is_none());
    assert!(parse_reply(r#"{"body":"Valid","other":"Extra"}"#).is_none());
    assert!(parse_reply(r#"{"title":"Clipboard \u001b]52","body":"text"}"#).is_none());
    assert!(parse_reply(r#"{"title":" ","body":null}"#).is_none());
    assert!(Selection::parse(&["--all", "--tenant", "--all"]).is_none());
}

#[test]
fn kanban_specify_limits_request_extensions_and_refuses_missing_endpoints() {
    assert!(specifier_client(&AuxiliaryTaskConfig::default()).is_err());
    assert!(specifier_client(&AuxiliaryTaskConfig {
        model: "fixture".into(),
        base_url: "http://example.test/v1".into(),
        ..Default::default()
    })
    .is_err());
    assert_eq!(
        safe_request_extensions(Some(&json!({
            "top_p": 0.6, "response_format": {"type": "text"}, "messages": ["unused"],
            "max_tokens": 80000,
        }))),
        Some(json!({"top_p": 0.6}))
    );
    assert!(safe_request_extensions(Some(&json!({"top_p": 2}))).is_none());
}

#[test]
fn kanban_specify_refuses_oversized_sweeps_before_inference() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("boards.json");
    let lock = KanbanStoreLock::open(path.clone()).unwrap();
    let mut store = lock.load().unwrap();
    for _ in 0..=MAX_BATCH {
        add_triage(&mut store, "imports");
    }
    lock.save(&store).unwrap();
    assert!(snapshot_triage(&path, &Selection::All(None)).is_err());
    assert_eq!(
        snapshot_triage(&path, &Selection::Task("K-0001".into()))
            .unwrap()
            .1
            .len(),
        1
    );
}
