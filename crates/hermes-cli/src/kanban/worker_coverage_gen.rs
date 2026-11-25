use super::*;
use std::cell::RefCell;

#[test]
fn goal_loop_returns_blocked_by_worker_when_lane_blocked() {
    let judged = RefCell::new(0u32);
    let callbacks = KanbanGoalLoopCallbacks {
        task_status: Box::new(|| KanbanLane::Blocked),
        judge: Box::new(|_g: &str, _r: &str| {
            *judged.borrow_mut() += 1;
            KanbanGoalJudgment::Continue("x".to_string())
        }),
        run_turn: Box::new(|_p: &str| "resp".to_string()),
        block_task: Box::new(|_r: &str| {}),
    };
    let result = run_kanban_goal_loop("t1", "goal", "first", 5, callbacks);
    assert_eq!(result.outcome, KanbanGoalLoopOutcome::BlockedByWorker);
    assert_eq!(result.turns_used, 1);
    assert_eq!(result.reason, "worker blocked the task");
    assert_eq!(*judged.borrow(), 0);
}

#[test]
fn goal_loop_returns_stopped_when_lane_triage() {
    let callbacks = KanbanGoalLoopCallbacks {
        task_status: Box::new(|| KanbanLane::Triage),
        judge: Box::new(|_g: &str, _r: &str| KanbanGoalJudgment::Continue("x".to_string())),
        run_turn: Box::new(|_p: &str| "resp".to_string()),
        block_task: Box::new(|_r: &str| {}),
    };
    let result = run_kanban_goal_loop("t2", "goal", "first", 5, callbacks);
    assert_eq!(result.outcome, KanbanGoalLoopOutcome::Stopped);
    assert_eq!(result.turns_used, 1);
    assert_eq!(result.reason, "task returned to triage");
}

#[test]
fn goal_loop_blocks_after_finalize_nudge_ignored() {
    let long_reason = "r".repeat(450);
    let prompts = RefCell::new(Vec::<String>::new());
    let blocked = RefCell::new(Vec::<String>::new());
    let callbacks = KanbanGoalLoopCallbacks {
        task_status: Box::new(|| KanbanLane::Doing),
        judge: Box::new(|_g: &str, _r: &str| KanbanGoalJudgment::Done(long_reason.clone())),
        run_turn: Box::new(|p: &str| {
            prompts.borrow_mut().push(p.to_string());
            "still working".to_string()
        }),
        block_task: Box::new(|r: &str| blocked.borrow_mut().push(r.to_string())),
    };
    let result = run_kanban_goal_loop("task-9", "goal", "first", 10, callbacks);
    assert_eq!(result.outcome, KanbanGoalLoopOutcome::BlockedBudget);
    assert_eq!(result.turns_used, 2);
    assert!(result
        .reason
        .contains("task task-9 never moved to done after finalize nudge"));
    assert!(result.reason.ends_with(&long_reason));

    let prompts = prompts.borrow();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].starts_with("[The kanban task appears complete, but it is still open]"));
    assert!(prompts[0].contains(&format!("Reason: {}...", "r".repeat(400))));
    assert!(prompts[0].contains("Mark task task-9 done"));

    let blocked = blocked.borrow();
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0], result.reason);
}

#[test]
fn goal_loop_stops_on_empty_worker_response() {
    let blocked = RefCell::new(0u32);
    let callbacks = KanbanGoalLoopCallbacks {
        task_status: Box::new(|| KanbanLane::Todo),
        judge: Box::new(|_g: &str, _r: &str| {
            KanbanGoalJudgment::Continue("keep going".to_string())
        }),
        run_turn: Box::new(|_p: &str| "   \n ".to_string()),
        block_task: Box::new(|_r: &str| *blocked.borrow_mut() += 1),
    };
    let result = run_kanban_goal_loop("t3", "goal", "first", 5, callbacks);
    assert_eq!(result.outcome, KanbanGoalLoopOutcome::Stopped);
    assert_eq!(result.turns_used, 2);
    assert_eq!(result.reason, "worker returned empty response");
    assert_eq!(*blocked.borrow(), 0);
}

#[test]
fn truncate_for_prompt_appends_ellipsis_only_when_truncated() {
    assert_eq!(truncate_for_prompt("abcdef", 3), "abc...");
    assert_eq!(truncate_for_prompt("abc", 3), "abc");
    assert_eq!(truncate_for_prompt("héllo wörld", 5), "héllo...");
}
