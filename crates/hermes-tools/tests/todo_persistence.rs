use std::sync::Arc;

use hermes_core::{ToolError, ToolHandler};
use hermes_tools::backends::todo::FileTodoBackend;
use hermes_tools::tools::todo::TodoHandler;
use serde_json::{json, Value};

async fn todo_result(handler: &TodoHandler, params: Value) -> Value {
    let response = handler
        .execute(params)
        .await
        .expect("todo request succeeds");
    serde_json::from_str(&response).expect("todo returns JSON")
}

#[tokio::test]
async fn todo_tool_persists_edits_and_filters_after_reopen() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("state").join("todos.json");
    let handler = TodoHandler::new(Arc::new(FileTodoBackend::new(path.clone())));
    let original = todo_result(&handler, json!({"action": "create", "title": "First task"})).await;
    assert_eq!(original["status"], "ok");
    let id = original["id"].as_str().expect("created item ID");

    let reopened = TodoHandler::new(Arc::new(FileTodoBackend::new(path.clone())));
    let pending = todo_result(&reopened, json!({"action": "list", "status": "pending"})).await;
    assert_eq!(pending["total"], 1);
    assert_eq!(pending["todos"][0]["id"], id);
    assert_eq!(pending["todos"][0]["priority"], "medium");
    assert!(pending["todos"][0]["description"].is_null());
    let created_at = pending["todos"][0]["created_at"]
        .as_str()
        .filter(|value| !value.is_empty())
        .expect("creation timestamp");

    let result = todo_result(
        &reopened,
        json!({
            "action": "update",
            "id": id,
            "title": "Review notes",
            "description": "Read the fixture",
            "status": "completed",
            "priority": "high"
        }),
    )
    .await;
    assert_eq!(result, json!({"status":"ok","id":id,"action":"updated"}));
    let second = todo_result(
        &reopened,
        json!({"action": "create", "title": "Second task", "priority": "low"}),
    )
    .await;

    let persisted = TodoHandler::new(Arc::new(FileTodoBackend::new(path)));
    let completed = todo_result(&persisted, json!({"action": "list", "status": "completed"})).await;
    assert_eq!(completed["total"], 1);
    let item = &completed["todos"][0];
    assert_eq!(item["id"], id);
    assert_eq!(item["title"], "Review notes");
    assert_eq!(item["description"], "Read the fixture");
    assert_eq!(item["status"], "completed");
    assert_eq!(item["priority"], "high");
    assert_eq!(item["created_at"], created_at);

    let all = todo_result(&persisted, json!({"action": "list"})).await;
    assert_eq!(all["total"], 2);
    let second_id = second["id"].as_str().expect("second item ID");
    assert_ne!(second_id, id);
    let second_item = all["todos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == second_id)
        .expect("second item persisted");
    assert_eq!(second_item["status"], "pending");
    assert_eq!(second_item["priority"], "low");
}

#[tokio::test]
async fn todo_tool_reports_bad_requests_and_file_errors() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("todos.json");
    std::fs::create_dir(&path).unwrap();
    let sentinel = path.join("keep.txt");
    std::fs::write(&sentinel, b"preserved").unwrap();
    let handler = TodoHandler::new(Arc::new(FileTodoBackend::new(path.clone())));

    for (params, expected) in [
        (json!({}), "Missing 'action' parameter"),
        (json!({"action": "create"}), "Missing 'title' parameter"),
        (json!({"action": "update"}), "Missing 'id' parameter"),
        (
            json!({"action": "archive"}),
            "Unknown action: 'archive'. Use 'create', 'update', or 'list'.",
        ),
    ] {
        assert!(matches!(
            handler.execute(params).await.unwrap_err(),
            ToolError::InvalidParams(message) if message == expected
        ));
    }
    assert!(matches!(
        handler
            .execute(json!({"action": "update", "id": "missing"}))
            .await
            .unwrap_err(),
        ToolError::NotFound(message) if message == "Todo 'missing' not found"
    ));
    assert!(matches!(
        handler
            .execute(json!({"action": "create", "title": "Cannot replace a directory"}))
            .await
            .unwrap_err(),
        ToolError::ExecutionFailed(message) if message.starts_with("Failed to write todos: ")
    ));
    assert!(path.is_dir());
    assert_eq!(std::fs::read(sentinel).unwrap(), b"preserved");
}
