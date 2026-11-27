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
        &[
            "add",
            "child work",
            "--depends",
            "K-0001",
            "--lane",
            "doing",
        ],
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
    assert_eq!(lanes(home), [KanbanLane::Done, KanbanLane::Done]);
}

/// Lanes of the current board's tasks, in order.
fn lanes(home: &Path) -> Vec<KanbanLane> {
    let store = board_store(home);
    let board = store
        .boards
        .iter()
        .find(|board| board.id == store.current_board_id)
        .unwrap();
    board.tasks.iter().map(|task| task.lane).collect()
}

async fn kanban_as(home: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = support::isolated_hermes(home);
    command
        .env("HERMES_KANBAN_CONTEXTLATTICE_SYNC", "0")
        .envs(env.iter().copied())
        .arg("kanban")
        .args(args);
    tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .unwrap_or_else(|_| panic!("kanban {args:?} timed out"))
        .unwrap()
}

/// A board with K-0001 and K-0002 in doing, each owned by a dispatched run
/// (bg_1, bg_2) whose job status file says `status`.
async fn two_running_workers(home: &Path, status: &str) {
    assert_kanban(home, &["init", "Runs"], "Board selected").await;
    assert_kanban(home, &["add", "first", "--lane", "doing"], "K-0001").await;
    assert_kanban(home, &["add", "second", "--lane", "doing"], "K-0002").await;
    let path = home.join("hermes/alpha/kanban/boards.json");
    let mut store = board_store(home);
    let current = store.current_board_id.clone();
    let board = store.boards.iter_mut().find(|b| b.id == current).unwrap();
    board.tasks[0].background_job_id = Some("bg_1".to_string());
    board.tasks[1].background_job_id = Some("bg_2".to_string());
    std::fs::write(&path, serde_json::to_vec_pretty(&store).unwrap()).unwrap();
    let jobs = home.join("hermes/background_jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    for run in ["bg_1", "bg_2"] {
        std::fs::write(
            jobs.join(format!("{run}.json")),
            format!(r#"{{"id": "{run}", "status": "{status}"}}"#),
        )
        .unwrap();
    }
}

fn refused(output: &Output, needle: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "succeeded: {stderr}");
    assert!(stderr.contains(needle), "{stderr}");
}

#[tokio::test]
async fn the_cli_refuses_a_live_task_and_holds_a_bound_child_to_its_own() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    two_running_workers(home, "running").await;
    let store = home.join("hermes/alpha/kanban/boards.json");
    let before = std::fs::read(&store).unwrap();
    // A worker's terminal child: the binding variables were stripped.
    for args in [
        &["done", "K-0002", "finished"][..],
        &["block", "K-0002", "stop"],
        &["move", "K-0002", "todo"],
        &["move", "K-0002", "done"],
    ] {
        refused(
            &kanban_as(home, &[], args).await,
            "a live worker is running it",
        );
    }
    // A child that set the variables itself is held to its own task.
    let as_bg_1 = [
        ("HERMES_KANBAN_BOARD", "runs"),
        ("HERMES_KANBAN_TASK", "K-0001"),
        ("HERMES_KANBAN_RUN_ID", "bg_1"),
    ];
    refused(
        &kanban_as(home, &as_bg_1, &["done", "K-0002"]).await,
        "bound to task K-0001",
    );
    refused(
        &kanban_as(home, &as_bg_1, &["done", "K-0002", "--force"]).await,
        "--force is for operators",
    );
    // Nor its own task whilst its run is live: the worker's tools finish it.
    refused(
        &kanban_as(home, &as_bg_1, &["done", "K-0001", "ok"]).await,
        "Let the worker finish it through its kanban tools",
    );
    assert_eq!(std::fs::read(&store).unwrap(), before);
    // Once the run has ended, its own task it may finish.
    std::fs::write(
        home.join("hermes/background_jobs/bg_1.json"),
        r#"{"id": "bg_1", "status": "completed"}"#,
    )
    .unwrap();
    let own = kanban_as(home, &as_bg_1, &["done", "K-0001", "ok"]).await;
    assert!(
        own.status.success(),
        "{}",
        String::from_utf8_lossy(&own.stderr)
    );
    assert_eq!(lanes(home), [KanbanLane::Done, KanbanLane::Doing]);
}

#[tokio::test]
async fn an_operator_needs_force_for_a_live_task() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    two_running_workers(home, "queued").await;
    refused(&kanban_as(home, &[], &["done", "K-0001"]).await, "--force");
    assert_kanban(home, &["done", "K-0001", "--force"], "Marked done: K-0001").await;
    assert_kanban(
        home,
        &["block", "K-0002", "need", "input", "--force"],
        "Blocked K-0002",
    )
    .await;
    let store = board_store(home);
    let board = store
        .boards
        .iter()
        .find(|board| board.id == store.current_board_id)
        .unwrap();
    assert_eq!(board.tasks[0].run_summary, None);
    assert_eq!(board.tasks[1].blocked_reason.as_deref(), Some("need input"));
}

#[tokio::test]
async fn a_task_whose_run_has_ended_behaves_as_before() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    two_running_workers(home, "completed").await;
    assert_kanban(home, &["done", "K-0001"], "Marked done: K-0001").await;
    assert_kanban(home, &["move", "K-0002", "todo"], "Moved K-0002 -> todo").await;
    assert_eq!(lanes(home), [KanbanLane::Done, KanbanLane::Todo]);
}

#[tokio::test]
async fn kanban_move_to_done_checks_parents_too() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    assert_kanban(home, &["init", "Gates"], "Board selected").await;
    assert_kanban(home, &["add", "parent"], "K-0001").await;
    assert_kanban(home, &["add", "child", "--depends", "K-0001"], "K-0002").await;
    refused(
        &kanban_as(home, &[], &["move", "K-0002", "done", "--force"]).await,
        "unfinished parent(s) K-0001 (todo)",
    );
    assert_kanban(home, &["move", "K-0002", "doing"], "Moved K-0002 -> doing").await;
    assert_eq!(lanes(home), [KanbanLane::Todo, KanbanLane::Doing]);
}

#[tokio::test]
async fn a_worker_terminal_with_only_the_marker_is_held_to_its_own_task() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    two_running_workers(home, "running").await;
    let store = home.join("hermes/alpha/kanban/boards.json");
    let before = std::fs::read(&store).unwrap();
    // What the terminal of the worker for K-0001 keeps after the strip.
    let marker = [("HERMES_KANBAN_WORKER_SCOPE", "runs/K-0001")];
    for args in [
        &["done", "K-0002", "x"][..],
        &["done", "K-0002", "x", "--force"],
        &["block", "K-0002", "stop", "--force"],
        &["move", "K-0002", "todo", "--force"],
    ] {
        let output = kanban_as(home, &marker, args).await;
        refused(&output, "may change only its own task");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("re-run with --force"), "{stderr}");
    }
    refused(
        &kanban_as(
            home,
            &[("HERMES_KANBAN_WORKER_SCOPE", "../K-0001")],
            &["done", "K-0002", "--force"],
        )
        .await,
        "is not <board>/<task>",
    );
    // Its own task only once the run has ended, and never with --force.
    refused(
        &kanban_as(home, &marker, &["done", "K-0001", "ok"]).await,
        "a live worker is running it",
    );
    refused(
        &kanban_as(home, &marker, &["done", "K-0001", "ok", "--force"]).await,
        "--force is for operators",
    );
    assert_eq!(std::fs::read(&store).unwrap(), before);
    std::fs::write(
        home.join("hermes/background_jobs/bg_1.json"),
        r#"{"id": "bg_1", "status": "completed"}"#,
    )
    .unwrap();
    // Its own task, and an operator with --force, still work.
    assert!(kanban_as(home, &marker, &["done", "K-0001", "ok"])
        .await
        .status
        .success());
    assert!(kanban_as(home, &[], &["done", "K-0002", "--force"])
        .await
        .status
        .success());
    assert_eq!(lanes(home), [KanbanLane::Done, KanbanLane::Done]);
}

/// A worker's terminal child can set the marker or the binding to another
/// worker's task. Neither may allow what having none refuses.
#[tokio::test]
async fn a_forged_marker_or_binding_never_beats_having_none() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    two_running_workers(home, "running").await;
    let store = home.join("hermes/alpha/kanban/boards.json");
    let before = std::fs::read(&store).unwrap();
    let marker = [("HERMES_KANBAN_WORKER_SCOPE", "runs/K-0002")];
    let binding = [
        ("HERMES_KANBAN_BOARD", "runs"),
        ("HERMES_KANBAN_TASK", "K-0002"),
        ("HERMES_KANBAN_RUN_ID", "bg_2"),
    ];
    let none: [(&str, &str); 0] = [];
    for env in [&none[..], &marker, &binding] {
        for args in [
            &["done", "K-0002", "x"][..],
            &["block", "K-0002", "stop"],
            &["move", "K-0002", "todo"],
        ] {
            refused(
                &kanban_as(home, env, args).await,
                "a live worker is running it",
            );
        }
    }
    for env in [&marker[..], &binding] {
        refused(
            &kanban_as(home, env, &["done", "K-0002", "x", "--force"]).await,
            "--force is for operators",
        );
    }
    assert_eq!(std::fs::read(&store).unwrap(), before);
}

/// As upstream fences a worker's descendants off every board write, a
/// worker's terminal may not dispatch, claim, add or otherwise write.
#[tokio::test]
async fn a_worker_terminal_cannot_write_the_board_beyond_its_own_lifecycle() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    two_running_workers(home, "running").await;
    let note = home.join("note.txt");
    std::fs::write(&note, "fixture").unwrap();
    let note = note.to_str().unwrap();
    let store = home.join("hermes/alpha/kanban/boards.json");
    let before = std::fs::read(&store).unwrap();
    let marker = [("HERMES_KANBAN_WORKER_SCOPE", "runs/K-0001")];
    let binding = [
        ("HERMES_KANBAN_BOARD", "runs"),
        ("HERMES_KANBAN_TASK", "K-0001"),
        ("HERMES_KANBAN_RUN_ID", "bg_1"),
    ];
    for env in [&marker[..], &binding] {
        for args in [
            &["dispatch", "K-0002"][..],
            &["claim", "K-0002", "me"],
            &["use", "Other"],
            &["init", "Other"],
            &["add", "third"],
            &["attach", "K-0002", note],
            &["detach", "K-0002", "note.txt"],
            &["archive-done"],
            &["sync"],
            &["specify", "--all"],
        ] {
            refused(
                &kanban_as(home, env, args).await,
                &format!("Refusing `kanban {}`", args[0]),
            );
        }
    }
    assert_eq!(std::fs::read(&store).unwrap(), before);
    // An operator still can, a board pinned with HERMES_KANBAN_BOARD alone
    // included.
    let pinned = kanban_as(home, &[("HERMES_KANBAN_BOARD", "runs")], &["add", "third"]).await;
    let stdout = String::from_utf8_lossy(&pinned.stdout);
    assert!(
        pinned.status.success() && stdout.contains("K-0003"),
        "{stdout}"
    );
}

#[tokio::test]
async fn a_worker_terminal_cannot_reach_its_task_id_on_another_board() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path();
    two_running_workers(home, "running").await;
    // A second board, selected by the child itself, with its own K-0001.
    assert_kanban(home, &["init", "Other"], "Board selected").await;
    assert_kanban(home, &["add", "elsewhere", "--lane", "doing"], "K-0001").await;
    let store = home.join("hermes/alpha/kanban/boards.json");
    let before = std::fs::read(&store).unwrap();
    let marker = [("HERMES_KANBAN_WORKER_SCOPE", "runs/K-0001")];
    for args in [
        &["done", "K-0001", "x"][..],
        &["done", "K-0001", "--force"],
        &["block", "K-0001", "stop"],
        &["move", "K-0001", "todo", "--force"],
    ] {
        refused(&kanban_as(home, &marker, args).await, "on board `runs`");
    }
    assert_eq!(std::fs::read(&store).unwrap(), before);
}
