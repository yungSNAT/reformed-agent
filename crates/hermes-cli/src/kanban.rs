use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

mod attachments;
mod specify;
mod store;
mod worker;

pub use attachments::{
    add_attachment_to_task, attachments_root, remove_attachment_from_task, task_attachments_dir,
};
pub use specify::specify_kanban;
pub(crate) use store::lock_store;
pub use store::{kanban_store_path, load_store, save_store};
pub use worker::{
    build_worker_context, run_kanban_goal_loop, KanbanGoalBlockFn, KanbanGoalJudgeFn,
    KanbanGoalJudgment, KanbanGoalLoopCallbacks, KanbanGoalLoopOutcome, KanbanGoalLoopResult,
    KanbanGoalRunTurnFn, KanbanGoalStatusFn,
};

const KANBAN_STATE_DIR: &str = "alpha/kanban";
const KANBAN_SCHEMA_VERSION: u32 = 1;
const DEFAULT_BOARD_ID: &str = "main";
const CONTEXTLATTICE_DEFAULT_ORCHESTRATOR_URL: &str = "http://127.0.0.1:8075";
const CONTEXTLATTICE_KANBAN_FILE: &str = "notes/hermes-kanban.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KanbanLane {
    Triage,
    Todo,
    Doing,
    Blocked,
    Done,
}

impl KanbanLane {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "triage" => Some(Self::Triage),
            "todo" | "backlog" | "to-do" => Some(Self::Todo),
            "doing" | "in-progress" | "inprogress" | "running" => Some(Self::Doing),
            "blocked" | "stalled" => Some(Self::Blocked),
            "done" | "completed" => Some(Self::Done),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Triage => "triage",
            Self::Todo => "todo",
            Self::Doing => "doing",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }

    pub fn all() -> [Self; 5] {
        [
            Self::Triage,
            Self::Todo,
            Self::Doing,
            Self::Blocked,
            Self::Done,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KanbanTask {
    pub id: String,
    pub title: String,
    pub lane: KanbanLane,
    pub priority: u8,
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub description: Option<String>,
    pub depends_on: Vec<String>,
    pub blocked_reason: Option<String>,
    pub background_job_id: Option<String>,
    pub run_summary: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    #[serde(default)]
    pub attachments: Vec<KanbanAttachment>,
    #[serde(default)]
    pub goal_mode: bool,
    #[serde(default)]
    pub goal_max_turns: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KanbanAttachment {
    pub id: String,
    pub filename: String,
    pub stored_path: String,
    #[serde(default)]
    pub content_type: Option<String>,
    pub size: u64,
    #[serde(default)]
    pub uploaded_by: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanBoard {
    pub id: String,
    pub name: String,
    pub project_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub next_task_seq: u64,
    pub tasks: Vec<KanbanTask>,
    pub archived: Vec<KanbanTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanStore {
    pub schema_version: u32,
    pub current_board_id: String,
    pub boards: Vec<KanbanBoard>,
}

#[derive(Debug, Clone)]
pub struct KanbanActionInput {
    pub action: String,
    pub task_id: Option<String>,
    pub lane: Option<KanbanLane>,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct KanbanCheckpointResult {
    pub attempted: bool,
    pub succeeded: bool,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct NewKanbanTaskInput {
    pub title: String,
    pub lane: KanbanLane,
    pub priority: u8,
    pub assignee: Option<String>,
    pub tenant: Option<String>,
    pub description: Option<String>,
    pub depends_on: Vec<String>,
    pub goal_mode: bool,
    pub goal_max_turns: Option<u32>,
}

impl Default for KanbanStore {
    fn default() -> Self {
        let now = now_rfc3339();
        Self {
            schema_version: KANBAN_SCHEMA_VERSION,
            current_board_id: DEFAULT_BOARD_ID.to_string(),
            boards: vec![KanbanBoard {
                id: DEFAULT_BOARD_ID.to_string(),
                name: "Main".to_string(),
                project_path: None,
                created_at: now.clone(),
                updated_at: now,
                next_task_seq: 1,
                tasks: Vec::new(),
                archived: Vec::new(),
            }],
        }
    }
}

pub fn ensure_board<'a>(
    store: &'a mut KanbanStore,
    requested: Option<&str>,
) -> &'a mut KanbanBoard {
    if store.boards.is_empty() {
        *store = KanbanStore::default();
    }
    if let Some(raw) = requested {
        if let Some(idx) = store.boards.iter().position(|board| board.id == raw) {
            store.current_board_id = raw.to_string();
            return &mut store.boards[idx];
        }
        if let Some(idx) = store
            .boards
            .iter()
            .position(|board| board.name.eq_ignore_ascii_case(raw))
        {
            store.current_board_id = store.boards[idx].id.clone();
            return &mut store.boards[idx];
        }
    }
    if let Some(idx) = store
        .boards
        .iter()
        .position(|board| board.id == store.current_board_id)
    {
        return &mut store.boards[idx];
    }
    store.current_board_id = store.boards[0].id.clone();
    &mut store.boards[0]
}

pub fn create_or_select_board<'a>(
    store: &'a mut KanbanStore,
    name: &str,
    project_path: Option<String>,
) -> &'a mut KanbanBoard {
    if let Some(idx) = store
        .boards
        .iter()
        .position(|board| board.name.eq_ignore_ascii_case(name))
    {
        store.current_board_id = store.boards[idx].id.clone();
        if project_path.is_some() && store.boards[idx].project_path.is_none() {
            store.boards[idx].project_path = project_path;
            store.boards[idx].updated_at = now_rfc3339();
        }
        return &mut store.boards[idx];
    }
    let now = now_rfc3339();
    let board_id = slugify_board_id(name);
    let unique = unique_board_id(store, &board_id);
    store.boards.push(KanbanBoard {
        id: unique.clone(),
        name: name.trim().to_string(),
        project_path,
        created_at: now.clone(),
        updated_at: now,
        next_task_seq: 1,
        tasks: Vec::new(),
        archived: Vec::new(),
    });
    store.current_board_id = unique.clone();
    let idx = store
        .boards
        .iter()
        .position(|board| board.id == unique)
        .unwrap_or(0);
    &mut store.boards[idx]
}

pub fn add_task(board: &mut KanbanBoard, input: NewKanbanTaskInput) -> KanbanTask {
    let now = now_rfc3339();
    let id = format!("K-{:04}", board.next_task_seq.max(1));
    board.next_task_seq = board.next_task_seq.saturating_add(1);
    let task = KanbanTask {
        id,
        title: input.title.trim().to_string(),
        lane: input.lane,
        priority: input.priority.clamp(1, 5),
        assignee: normalize_optional(input.assignee),
        tenant: normalize_optional(input.tenant),
        description: normalize_optional(input.description),
        depends_on: dedupe_strings(input.depends_on),
        blocked_reason: None,
        background_job_id: None,
        run_summary: None,
        created_at: now.clone(),
        updated_at: now.clone(),
        started_at: (input.lane == KanbanLane::Doing).then_some(now.clone()),
        completed_at: (input.lane == KanbanLane::Done).then_some(now),
        attachments: Vec::new(),
        goal_mode: input.goal_mode,
        goal_max_turns: input.goal_max_turns,
    };
    board.updated_at = now_rfc3339();
    board.tasks.push(task.clone());
    task
}

pub fn find_task_mut<'a>(
    board: &'a mut KanbanBoard,
    id_or_title: &str,
) -> Option<&'a mut KanbanTask> {
    let needle = id_or_title.trim();
    if needle.is_empty() {
        return None;
    }
    if let Some(idx) = board
        .tasks
        .iter()
        .position(|task| task.id.eq_ignore_ascii_case(needle))
    {
        return board.tasks.get_mut(idx);
    }
    board
        .tasks
        .iter_mut()
        .find(|task| task.title.eq_ignore_ascii_case(needle))
}

pub fn move_task(task: &mut KanbanTask, lane: KanbanLane, note: Option<String>) {
    let now = now_rfc3339();
    task.lane = lane;
    task.updated_at = now.clone();
    if lane == KanbanLane::Doing && task.started_at.is_none() {
        task.started_at = Some(now.clone());
    }
    if lane == KanbanLane::Done {
        task.completed_at = Some(now.clone());
        if let Some(note) = normalize_optional(note) {
            task.run_summary = Some(note);
        }
    } else {
        task.completed_at = None;
    }
    if lane != KanbanLane::Blocked {
        task.blocked_reason = None;
    }
}

pub fn set_blocked(task: &mut KanbanTask, reason: Option<String>) {
    task.lane = KanbanLane::Blocked;
    task.blocked_reason = normalize_optional(reason);
    task.updated_at = now_rfc3339();
}

pub fn claim_task(task: &mut KanbanTask, assignee: Option<String>) {
    task.assignee = normalize_optional(assignee);
    if task.lane == KanbanLane::Todo {
        task.lane = KanbanLane::Doing;
        task.started_at = Some(now_rfc3339());
    }
    task.updated_at = now_rfc3339();
}

pub fn archive_done(board: &mut KanbanBoard) -> usize {
    let mut moved = Vec::new();
    board.tasks.retain(|task| {
        let done = task.lane == KanbanLane::Done;
        if done {
            moved.push(task.clone());
        }
        !done
    });
    let count = moved.len();
    board.archived.extend(moved);
    if count > 0 {
        board.updated_at = now_rfc3339();
    }
    count
}

pub fn lane_counts(board: &KanbanBoard) -> Vec<(KanbanLane, usize)> {
    let mut rows = Vec::new();
    for lane in KanbanLane::all() {
        rows.push((lane, board.tasks.iter().filter(|t| t.lane == lane).count()));
    }
    rows
}

pub fn maybe_checkpoint_to_contextlattice(
    board: &KanbanBoard,
    payload: KanbanActionInput,
) -> KanbanCheckpointResult {
    if !kanban_contextlattice_sync_enabled() {
        return KanbanCheckpointResult {
            attempted: false,
            succeeded: false,
            detail: "ContextLattice sync disabled via HERMES_KANBAN_CONTEXTLATTICE_SYNC."
                .to_string(),
        };
    }
    let mut content = format!(
        "kanban_action={} board_id={} board_name={} summary={}",
        payload.action,
        board.id,
        board.name,
        payload.summary.replace('\n', " ")
    );
    if let Some(task_id) = payload.task_id {
        content.push_str(&format!(" task_id={task_id}"));
    }
    if let Some(lane) = payload.lane {
        content.push_str(&format!(" lane={}", lane.as_str()));
    }

    let topic_path = format!("runbooks/kanban/{}", board.id);
    match write_contextlattice_checkpoint(&topic_path, CONTEXTLATTICE_KANBAN_FILE, &content) {
        Ok(()) => KanbanCheckpointResult {
            attempted: true,
            succeeded: true,
            detail: format!("ContextLattice checkpointed to topic `{topic_path}`."),
        },
        Err(err) => KanbanCheckpointResult {
            attempted: true,
            succeeded: false,
            detail: format!("ContextLattice checkpoint failed: {err}"),
        },
    }
}

fn unique_board_id(store: &KanbanStore, base: &str) -> String {
    let clean = if base.is_empty() {
        DEFAULT_BOARD_ID.to_string()
    } else {
        base.to_string()
    };
    if !store.boards.iter().any(|board| board.id == clean) {
        return clean;
    }
    for idx in 2..10_000 {
        let candidate = format!("{clean}-{idx}");
        if !store.boards.iter().any(|board| board.id == candidate) {
            return candidate;
        }
    }
    format!("{}-{}", clean, uuid::Uuid::new_v4().simple())
}

fn slugify_board_id(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        let normalized = v.trim().to_string();
        if normalized.is_empty() {
            continue;
        }
        if out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&normalized))
        {
            continue;
        }
        out.push(normalized);
    }
    out
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn now_rfc3339() -> String {
    hermes_core::time::now_rfc3339()
}

fn kanban_contextlattice_sync_enabled() -> bool {
    std::env::var("HERMES_KANBAN_CONTEXTLATTICE_SYNC")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(true)
}

fn contextlattice_orchestrator_url() -> String {
    std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .or_else(|_| std::env::var("MEMMCP_ORCHESTRATOR_URL"))
        .unwrap_or_else(|_| CONTEXTLATTICE_DEFAULT_ORCHESTRATOR_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn contextlattice_timeout() -> Duration {
    let seconds = std::env::var("HERMES_KANBAN_CONTEXTLATTICE_TIMEOUT_SECS")
        .or_else(|_| std::env::var("CONTEXTLATTICE_TIMEOUT_SECS"))
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .unwrap_or(10.0)
        .clamp(1.0, 60.0);
    Duration::from_secs_f64(seconds)
}

fn write_contextlattice_checkpoint(
    topic_path: &str,
    file_name: &str,
    content: &str,
) -> Result<(), String> {
    let url = format!("{}/memory/write", contextlattice_orchestrator_url());
    let payload = json!({
        "projectName": "hermes-agent-ultra",
        "fileName": file_name,
        "topicPath": topic_path,
        "content": content,
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(contextlattice_timeout())
        .build()
        .map_err(|err| err.to_string())?;
    let mut request = client.post(&url).json(&payload);
    if let Ok(api_key) = std::env::var("CONTEXTLATTICE_API_KEY") {
        if !api_key.trim().is_empty() {
            request = request.bearer_auth(api_key);
        }
    }
    let response = request.send().map_err(|err| err.to_string())?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().unwrap_or_default();
    let preview: String = body.chars().take(240).collect();
    Err(format!("HTTP {status}: {}", preview.trim()))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "kanban_coverage_gen.rs"]
mod coverage_gen_kanban;
