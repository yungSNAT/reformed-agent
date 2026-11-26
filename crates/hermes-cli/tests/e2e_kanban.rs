#[path = "support/isolated_hermes.rs"]
mod support;

use hermes_cli::kanban::{KanbanLane, KanbanStore};
use std::path::Path;
use std::process::Output;
use std::time::Duration;

async fn kanban(home: &Path, args: &[&str]) -> Output {
    tokio::time::timeout(
        Duration::from_secs(10),
        support::isolated_hermes(home)
            .env("HERMES_KANBAN_CONTEXTLATTICE_SYNC", "0")
            .arg("kanban")
            .args(args)
            .output(),
    )
    .await
    .unwrap_or_else(|_| panic!("kanban {args:?} timed out"))
    .unwrap()
}

async fn assert_kanban(home: &Path, args: &[&str], expected: &str) {
    let output = kanban(home, args).await;
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(expected), "{args:?}: {stdout}");
}

fn board_store(home: &Path) -> KanbanStore {
    let path = home.join("hermes/alpha/kanban/boards.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn kanban_lifecycle_persists_board_attachment_and_archive() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    let project = home.join("project");
    std::fs::create_dir(&project).unwrap();
    let fixture = project.join("summary.txt");
    let contents = "Synthetic notes for café";
    std::fs::write(&fixture, contents).unwrap();

    assert_kanban(
        home,
        &["init", "Local Review", project.to_str().unwrap()],
        "Board selected: Local Review (local-review)",
    )
    .await;
    assert_kanban(
        home,
        &["add", "Review notes", "--lane", "triage", "-p", "2"],
        "Added task K-0001 [triage] p2: Review notes",
    )
    .await;
    assert_kanban(home, &["move", "K-0001", "todo"], "Moved K-0001 -> todo").await;
    assert_kanban(
        home,
        &["claim", "Review notes", "fixture-reviewer"],
        "Claimed K-0001",
    )
    .await;
    assert_kanban(
        home,
        &["block", "K-0001", "awaiting", "fixture"],
        "Blocked K-0001",
    )
    .await;
    assert_kanban(
        home,
        &["status"],
        "[blocked] p2 @fixture-reviewer Review notes blocked=awaiting fixture",
    )
    .await;
    assert_kanban(
        home,
        &["attach", "Review notes", fixture.to_str().unwrap()],
        "Attached summary.txt to K-0001",
    )
    .await;
    assert_kanban(home, &["attachments", "K-0001"], "summary.txt").await;
    let store = board_store(home);
    let board = store
        .boards
        .iter()
        .find(|b| b.id == "local-review")
        .unwrap();
    let task = &board.tasks[0];
    assert_eq!(task.lane, KanbanLane::Blocked);
    assert!(task.started_at.is_some());
    assert!(task.background_job_id.is_none());
    let [attachment] = task.attachments.as_slice() else {
        panic!("expected one persisted attachment");
    };
    let stored_path = Path::new(&attachment.stored_path);
    let expected_path = home
        .canonicalize()
        .unwrap()
        .join("hermes/alpha/kanban/attachments/local-review/K-0001/summary.txt");
    assert_eq!(stored_path, expected_path);
    assert_eq!(stored_path.canonicalize().unwrap(), expected_path);
    assert_eq!(std::fs::read_to_string(stored_path).unwrap(), contents);

    assert_kanban(
        home,
        &["detach", "K-0001", "summary.txt"],
        &format!("Removed attachment {} (summary.txt)", attachment.id),
    )
    .await;
    assert!(!stored_path.exists());
    assert_eq!(std::fs::read_to_string(&fixture).unwrap(), contents);
    assert_kanban(
        home,
        &["detach", "K-0001", "summary.txt"],
        "Attachment not found",
    )
    .await;
    assert_kanban(
        home,
        &["done", "K-0001", "fixture checked"],
        "Marked done: K-0001",
    )
    .await;
    assert_kanban(home, &["archive"], "Archived 1 done task(s).").await;
    assert_kanban(home, &["use", "local-review"], "Using board: Local Review").await;
    assert_kanban(home, &["list"], "* Local Review (local-review)").await;
    assert_kanban(home, &["show"], "Tasks: 0 (archived done: 1)").await;

    let store = board_store(home);
    assert_eq!(store.current_board_id, "local-review");
    let archived = &store
        .boards
        .iter()
        .find(|b| b.id == "local-review")
        .unwrap()
        .archived;
    let [task] = archived.as_slice() else {
        panic!("expected one archived task");
    };
    assert_eq!(task.id, "K-0001");
    assert_eq!(task.lane, KanbanLane::Done);
    assert_eq!(task.run_summary.as_deref(), Some("fixture checked"));
    assert!(task.completed_at.is_some());
    assert!(task.blocked_reason.is_none());
    assert!(task.attachments.is_empty());
}

#[tokio::test]
async fn invalid_kanban_updates_leave_the_saved_board_unchanged() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    assert_kanban(home, &["init", "Input Review"], "Board selected").await;
    let store = home.join("hermes/alpha/kanban/boards.json");
    let before = std::fs::read(&store).unwrap();
    for (args, error) in [
        (
            &["add", "Fixture", "--priority"][..],
            "Missing value for --priority",
        ),
        (&["add", "Fixture", "--priority", "6"], "Invalid priority"),
        (&["add", "Fixture", "--lane", "unknown"], "Invalid lane"),
        (
            &["add", "Fixture", "--tenant", "--goal"],
            "Missing value for --tenant",
        ),
        (
            &["add", "Fixture", "--goal-max-turns", "0"],
            "Expected positive integer",
        ),
    ] {
        let output = kanban(home, args).await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        assert!(stderr.contains(error), "{args:?}: {stderr}");
    }
    assert_kanban(home, &["move", "K-0001", "unknown"], "Invalid lane").await;
    assert_kanban(home, &["claim", "K-0001"], "Task not found: K-0001").await;
    assert_kanban(home, &["block", "K-0001"], "Usage: kanban block").await;
    assert_eq!(std::fs::read(store).unwrap(), before);
}

// Port of upstream test_kanban_complete_parents_tool.py::test_cli_complete_names_unsatisfied_parent.
#[tokio::test]
async fn kanban_done_names_an_unfinished_parent_and_changes_nothing() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    assert_kanban(home, &["init", "Gates"], "Board selected").await;
    assert_kanban(home, &["add", "parent gate"], "Added task K-0001").await;
    assert_kanban(
        home,
        &["add", "child work", "--depends", "K-0001", "--lane", "doing"],
        "Added task K-0002",
    )
    .await;
    let store = home.join("hermes/alpha/kanban/boards.json");
    let before = std::fs::read(&store).unwrap();
    let output = kanban(home, &["done", "K-0002", "finished"]).await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "done succeeded: {stderr}");
    assert!(
        stderr.contains("unfinished parent(s) K-0001 (todo)"),
        "{stderr}"
    );
    assert_eq!(std::fs::read(&store).unwrap(), before);
    assert_kanban(home, &["done", "K-0001"], "K-0001").await;
    assert_kanban(home, &["done", "K-0002"], "K-0002").await;
    let lanes: Vec<KanbanLane> = board_store(home).boards[0]
        .tasks
        .iter()
        .map(|task| task.lane)
        .collect();
    assert_eq!(lanes, [KanbanLane::Done, KanbanLane::Done]);
}
