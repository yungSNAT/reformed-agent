use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use super::store::{
    kanban_store_path_in, load_store_from_path, save_store_to_path, KanbanStoreLock,
};
use super::*;
use crate::test_env_lock;

fn env_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    test_env_lock::lock()
}

fn with_temp_home<T>(f: impl FnOnce(&Path) -> T) -> T {
    let _guard = env_test_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev = std::env::var_os("HERMES_HOME");
    std::env::set_var("HERMES_HOME", tmp.path());
    let result = f(tmp.path());
    match prev {
        Some(value) => std::env::set_var("HERMES_HOME", value),
        None => std::env::remove_var("HERMES_HOME"),
    }
    result
}

#[test]
fn load_store_bootstraps_default_board() {
    with_temp_home(|home| {
        let path = kanban_store_path_in(home);
        let store = load_store_from_path(&path).expect("load");
        assert_eq!(store.current_board_id, "main");
        assert_eq!(store.boards.len(), 1);
        assert_eq!(store.boards[0].name, "Main");
    });
}

#[test]
fn board_update_holds_the_lock_until_the_new_snapshot_is_visible() {
    let home = tempfile::tempdir().expect("tempdir");
    let path = kanban_store_path_in(home.path());
    let first = KanbanStoreLock::open(path.clone()).expect("first lock");
    let mut snapshot = first.load().expect("load board");
    create_or_select_board(&mut snapshot, "First", None);

    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))
        .expect("open lock from another handle");
    assert!(matches!(
        contender.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    first.save(&snapshot).expect("commit first update");
    contender.try_lock().expect("commit releases lock");
    contender.unlock().expect("release contention check");

    let second = KanbanStoreLock::open(path.clone()).expect("second lock");
    let mut snapshot = second.load().expect("load latest board");
    create_or_select_board(&mut snapshot, "Second", None);
    second.save(&snapshot).expect("commit second update");

    let final_lock = KanbanStoreLock::open(path).expect("final lock");
    let final_store = final_lock.load().expect("reload boards");
    let names: Vec<_> = final_store
        .boards
        .iter()
        .map(|board| board.name.as_str())
        .collect();
    assert_eq!(names, ["Main", "First", "Second"]);
}

#[test]
fn load_store_refuses_malformed_existing_store_and_preserves_backup() {
    with_temp_home(|home| {
        let path = kanban_store_path_in(home);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let original = b"{ not valid json";
        std::fs::write(&path, original).expect("write corrupt store");

        let err = load_store_from_path(&path).expect_err("corrupt store should fail");
        let msg = err.to_string();
        assert!(msg.contains("Refusing to open corrupt kanban store"));
        assert!(msg.contains("parse failed"));
        assert_eq!(std::fs::read(&path).expect("read original"), original);

        let backups = corrupt_store_backups(path.parent().expect("parent"));
        assert_eq!(backups.len(), 1, "unexpected backups: {backups:?}");
        assert_eq!(std::fs::read(&backups[0]).expect("read backup"), original);
    });
}

#[test]
fn load_store_refuses_semantically_empty_existing_store() {
    with_temp_home(|home| {
        let path = kanban_store_path_in(home);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let original = br#"{"schema_version":1,"current_board_id":"main","boards":[]}"#;
        std::fs::write(&path, original).expect("write empty store");

        let err = load_store_from_path(&path).expect_err("empty existing store should fail closed");
        let msg = err.to_string();
        assert!(msg.contains("Refusing to open corrupt kanban store"));
        assert!(msg.contains("store has no boards"));
        assert_eq!(std::fs::read(&path).expect("read original"), original);

        let backups = corrupt_store_backups(path.parent().expect("parent"));
        assert_eq!(backups.len(), 1, "unexpected backups: {backups:?}");
        assert_eq!(std::fs::read(&backups[0]).expect("read backup"), original);
    });
}

#[test]
fn add_move_and_archive_flow_works() {
    with_temp_home(|home| {
        let path = kanban_store_path_in(home);
        let mut store = load_store_from_path(&path).expect("load");
        let board = ensure_board(&mut store, None);
        let task = add_task(
            board,
            NewKanbanTaskInput {
                title: "Ship kanban".to_string(),
                lane: KanbanLane::Todo,
                priority: 2,
                assignee: None,
                tenant: None,
                description: None,
                depends_on: vec![],
                goal_mode: false,
                goal_max_turns: None,
            },
        );
        assert_eq!(task.id, "K-0001");
        let task = find_task_mut(board, "K-0001").expect("task");
        claim_task(task, Some("worker-a".to_string()));
        assert_eq!(task.lane, KanbanLane::Doing);
        move_task(task, KanbanLane::Done, Some("verified".to_string()));
        assert_eq!(task.lane, KanbanLane::Done);
        assert!(task.completed_at.is_some());
        let moved = archive_done(board);
        assert_eq!(moved, 1);
        assert_eq!(board.tasks.len(), 0);
        assert_eq!(board.archived.len(), 1);
        save_store_to_path(&store, &path).expect("save");
    });
}

#[test]
fn kanban_claim_completion_archive_is_idempotent_under_reclaim_like_replay() {
    with_temp_home(|home| {
        let path = kanban_store_path_in(home);
        let mut store = load_store_from_path(&path).expect("load");
        let board = ensure_board(&mut store, None);
        let task = add_task(
            board,
            NewKanbanTaskInput {
                title: "Race-safe task".to_string(),
                lane: KanbanLane::Todo,
                priority: 1,
                assignee: None,
                tenant: None,
                description: None,
                depends_on: vec![],
                goal_mode: false,
                goal_max_turns: None,
            },
        );
        assert_eq!(task.id, "K-0001");

        let task = find_task_mut(board, "K-0001").expect("task");
        claim_task(task, Some("worker-a".to_string()));
        claim_task(task, Some("worker-a".to_string()));
        move_task(task, KanbanLane::Done, Some("finished once".to_string()));

        assert_eq!(archive_done(board), 1);
        assert_eq!(archive_done(board), 0);
        assert!(find_task_mut(board, "K-0001").is_none());
        assert_eq!(board.archived.len(), 1);
        assert_eq!(board.archived[0].id, "K-0001");
        assert_eq!(
            board.archived[0].run_summary.as_deref(),
            Some("finished once")
        );
        save_store_to_path(&store, &path).expect("save");
    });
}

#[test]
fn contextlattice_checkpoint_disabled_path() {
    with_temp_home(|_home| {
        std::env::set_var("HERMES_KANBAN_CONTEXTLATTICE_SYNC", "0");
        let board = KanbanBoard {
            id: "main".to_string(),
            name: "Main".to_string(),
            project_path: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            next_task_seq: 1,
            tasks: vec![],
            archived: vec![],
        };
        let res = maybe_checkpoint_to_contextlattice(
            &board,
            KanbanActionInput {
                action: "add".to_string(),
                task_id: Some("K-0001".to_string()),
                lane: Some(KanbanLane::Todo),
                summary: "test".to_string(),
            },
        );
        assert!(!res.attempted);
        std::env::remove_var("HERMES_KANBAN_CONTEXTLATTICE_SYNC");
    });
}

#[test]
fn attachments_copy_with_safe_unique_names_and_surface_in_worker_context() {
    with_temp_home(|home| {
        let path = kanban_store_path_in(home);
        let mut store = load_store_from_path(&path).expect("load");
        let board = ensure_board(&mut store, None);
        let task = add_task(
            board,
            NewKanbanTaskInput {
                title: "Read source".to_string(),
                lane: KanbanLane::Todo,
                priority: 3,
                assignee: Some("worker".to_string()),
                tenant: None,
                description: Some("summarize attached file".to_string()),
                depends_on: vec![],
                goal_mode: false,
                goal_max_turns: None,
            },
        );
        let source_dir = tempfile::tempdir().expect("source tempdir");
        let source = source_dir.path().join("notes.txt");
        std::fs::write(&source, "hello attachment").expect("write source");

        let first = add_attachment_to_task(board, &task.id, &source, Some("tester".to_string()))
            .expect("attach first");
        let second = add_attachment_to_task(board, &task.id, &source, None).expect("attach second");

        assert_eq!(first.id, "A-0001");
        assert_eq!(second.id, "A-0002");
        assert_eq!(first.filename, "notes.txt");
        assert_eq!(second.filename, "notes-1.txt");
        assert!(Path::new(&first.stored_path).is_file());
        assert!(Path::new(&second.stored_path).is_file());

        let task = find_task_mut(board, &task.id).expect("task");
        let context = build_worker_context(task);
        assert!(context.contains("Attachments:"));
        assert!(context.contains(&first.stored_path));
        assert!(context.contains("notes-1.txt"));
    });
}

#[test]
fn remove_attachment_deletes_row_and_blob() {
    with_temp_home(|home| {
        let path = kanban_store_path_in(home);
        let mut store = load_store_from_path(&path).expect("load");
        let board = ensure_board(&mut store, None);
        let task = add_task(
            board,
            NewKanbanTaskInput {
                title: "Task".to_string(),
                lane: KanbanLane::Todo,
                priority: 3,
                assignee: None,
                tenant: None,
                description: None,
                depends_on: vec![],
                goal_mode: false,
                goal_max_turns: None,
            },
        );
        let source_dir = tempfile::tempdir().expect("source tempdir");
        let source = source_dir.path().join("data.pdf");
        std::fs::write(&source, b"%PDF-1.4").expect("write source");
        let attachment = add_attachment_to_task(board, &task.id, &source, None).expect("attach");
        let stored = PathBuf::from(&attachment.stored_path);

        let removed = remove_attachment_from_task(board, &task.id, &attachment.id).expect("remove");
        assert_eq!(removed.expect("removed").id, attachment.id);
        assert!(!stored.exists());
        assert!(find_task_mut(board, &task.id)
            .expect("task")
            .attachments
            .is_empty());
    });
}

#[test]
fn goal_mode_context_and_loop_cover_continue_complete_and_budget() {
    let task = KanbanTask {
        id: "K-0007".to_string(),
        title: "Ship feature".to_string(),
        lane: KanbanLane::Doing,
        priority: 1,
        assignee: Some("worker".to_string()),
        tenant: None,
        description: Some("must pass tests".to_string()),
        depends_on: vec!["K-0001".to_string()],
        blocked_reason: None,
        background_job_id: None,
        run_summary: None,
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
        started_at: Some(now_rfc3339()),
        completed_at: None,
        attachments: Vec::new(),
        goal_mode: true,
        goal_max_turns: Some(3),
    };
    let context = build_worker_context(&task);
    assert!(context.contains("Goal mode: enabled (max 3 turns)"));
    assert!(context.contains("Dependencies: K-0001"));

    let mut statuses = [KanbanLane::Doing, KanbanLane::Done].into_iter();
    let mut prompts = Vec::new();
    let result = run_kanban_goal_loop(
        &task.id,
        "Ship feature",
        "started",
        5,
        KanbanGoalLoopCallbacks {
            task_status: Box::new(|| statuses.next().unwrap_or(KanbanLane::Done)),
            judge: Box::new(|_, _| KanbanGoalJudgment::Continue("needs more work".to_string())),
            run_turn: Box::new(|prompt: &str| {
                prompts.push(prompt.to_string());
                "continued".to_string()
            }),
            block_task: Box::new(|_| panic!("should not block")),
        },
    );
    assert_eq!(result.outcome, KanbanGoalLoopOutcome::CompletedByWorker);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("Continuing kanban goal-mode task"));

    let mut blocked_reason = String::new();
    let budget = run_kanban_goal_loop(
        &task.id,
        "Ship feature",
        "started",
        2,
        KanbanGoalLoopCallbacks {
            task_status: Box::new(|| KanbanLane::Doing),
            judge: Box::new(|_, _| KanbanGoalJudgment::Continue("not done".to_string())),
            run_turn: Box::new(|_| "still going".to_string()),
            block_task: Box::new(|reason: &str| blocked_reason = reason.to_string()),
        },
    );
    assert_eq!(budget.outcome, KanbanGoalLoopOutcome::BlockedBudget);
    assert!(blocked_reason.contains("turn budget"));
}

fn corrupt_store_backups(parent: &Path) -> Vec<PathBuf> {
    let mut backups = std::fs::read_dir(parent)
        .expect("read parent")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("boards.json.corrupt."))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    backups.sort();
    backups
}
