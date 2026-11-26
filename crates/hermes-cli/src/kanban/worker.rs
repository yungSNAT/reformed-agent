use super::{KanbanLane, KanbanTask};

const DEFAULT_GOAL_MAX_TURNS: u32 = 20;

pub fn build_worker_context(task: &KanbanTask) -> String {
    let mut lines = vec![format!("Execute Kanban task {}: {}", task.id, task.title)];
    if let Some(desc) = task
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(format!("Details: {desc}"));
    }
    if !task.depends_on.is_empty() {
        lines.push(format!("Dependencies: {}", task.depends_on.join(", ")));
    }
    if !task.attachments.is_empty() {
        lines.push("Attachments:".to_string());
        lines.push(
            "Read attached files with file or terminal tools at these absolute paths:".to_string(),
        );
        for attachment in &task.attachments {
            let size_kb = attachment.size.div_ceil(1024);
            let content_type = attachment
                .content_type
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| format!(", {s}"))
                .unwrap_or_default();
            let size = if size_kb == 0 {
                String::new()
            } else {
                format!(", {size_kb} KB")
            };
            lines.push(format!(
                "- {}{}{} -> {}",
                attachment.filename, content_type, size, attachment.stored_path
            ));
        }
    }
    if task.goal_mode {
        let max_turns = task.goal_max_turns.unwrap_or(DEFAULT_GOAL_MAX_TURNS);
        lines.push(format!(
            "Goal mode: enabled (max {max_turns} turns). Continue in this same work session until the task is genuinely done, then mark it done with a summary; if blocked, mark it blocked with the reason. Do not silently stop while work remains."
        ));
    }
    lines.join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KanbanGoalLoopOutcome {
    CompletedByWorker,
    BlockedByWorker,
    BlockedBudget,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KanbanGoalLoopResult {
    pub outcome: KanbanGoalLoopOutcome,
    pub turns_used: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KanbanGoalJudgment {
    Done(String),
    Continue(String),
}

pub type KanbanGoalStatusFn<'a> = Box<dyn FnMut() -> KanbanLane + 'a>;
pub type KanbanGoalJudgeFn<'a> = Box<dyn FnMut(&str, &str) -> KanbanGoalJudgment + 'a>;
pub type KanbanGoalRunTurnFn<'a> = Box<dyn FnMut(&str) -> String + 'a>;
pub type KanbanGoalBlockFn<'a> = Box<dyn FnMut(&str) + 'a>;

pub struct KanbanGoalLoopCallbacks<'a> {
    pub task_status: KanbanGoalStatusFn<'a>,
    pub judge: KanbanGoalJudgeFn<'a>,
    pub run_turn: KanbanGoalRunTurnFn<'a>,
    pub block_task: KanbanGoalBlockFn<'a>,
}

pub fn run_kanban_goal_loop(
    task_id: &str,
    goal_text: &str,
    first_response: &str,
    max_turns: u32,
    callbacks: KanbanGoalLoopCallbacks<'_>,
) -> KanbanGoalLoopResult {
    let KanbanGoalLoopCallbacks {
        mut task_status,
        mut judge,
        mut run_turn,
        mut block_task,
    } = callbacks;
    let max_turns = max_turns.max(1);
    let mut turns_used = 1u32;
    let mut last_response = first_response.to_string();
    let mut nudged_to_finalize = false;

    loop {
        match task_status() {
            KanbanLane::Done => {
                return KanbanGoalLoopResult {
                    outcome: KanbanGoalLoopOutcome::CompletedByWorker,
                    turns_used,
                    reason: "worker completed the task".to_string(),
                };
            }
            KanbanLane::Blocked => {
                return KanbanGoalLoopResult {
                    outcome: KanbanGoalLoopOutcome::BlockedByWorker,
                    turns_used,
                    reason: "worker blocked the task".to_string(),
                };
            }
            KanbanLane::Triage => {
                return KanbanGoalLoopResult {
                    outcome: KanbanGoalLoopOutcome::Stopped,
                    turns_used,
                    reason: "task returned to triage".to_string(),
                };
            }
            KanbanLane::Todo | KanbanLane::Doing => {}
        }

        let (prompt, reason) = match judge(goal_text, &last_response) {
            KanbanGoalJudgment::Done(reason) => {
                if nudged_to_finalize {
                    let reason = format!(
                        "Goal-mode worker output looked complete but task {task_id} never moved to done after finalize nudge: {reason}"
                    );
                    block_task(&reason);
                    return KanbanGoalLoopResult {
                        outcome: KanbanGoalLoopOutcome::BlockedBudget,
                        turns_used,
                        reason,
                    };
                }
                nudged_to_finalize = true;
                (
                    format!(
                        "[The kanban task appears complete, but it is still open]\nReason: {}\n\nMark task {} done with a concise summary, or block it with the remaining blocker.",
                        truncate_for_prompt(&reason, 400),
                        task_id
                    ),
                    reason,
                )
            }
            KanbanGoalJudgment::Continue(reason) => (
                format!(
                    "[Continuing kanban goal-mode task {}]\nReason: {}\n\nTake the next concrete step toward completing the task. When finished, mark it done; if blocked, mark it blocked. Do not stop without changing task state.",
                    task_id,
                    truncate_for_prompt(&reason, 400)
                ),
                reason,
            ),
        };

        if turns_used >= max_turns {
            let reason = format!(
                "Goal-mode worker exhausted its turn budget ({turns_used}/{max_turns}) without completing task {task_id}. Last judge reason: {}",
                truncate_for_prompt(&reason, 300)
            );
            block_task(&reason);
            return KanbanGoalLoopResult {
                outcome: KanbanGoalLoopOutcome::BlockedBudget,
                turns_used,
                reason,
            };
        }

        last_response = run_turn(&prompt);
        turns_used = turns_used.saturating_add(1);

        if last_response.trim().is_empty() {
            return KanbanGoalLoopResult {
                outcome: KanbanGoalLoopOutcome::Stopped,
                turns_used,
                reason: "worker returned empty response".to_string(),
            };
        }
    }
}

fn truncate_for_prompt(raw: &str, max_chars: usize) -> String {
    let mut out = raw.chars().take(max_chars).collect::<String>();
    if raw.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

#[cfg(test)]
#[path = "worker_coverage_gen.rs"]
mod coverage_gen_kanban_worker;
