//! Kanban tools: the structured surface a dispatched worker uses for its own
//! task, and an opted-in orchestrator session uses to route work.
//!
//! `kanban dispatch` starts a worker as a child process and passes it the
//! board, task and run (the background job id) in [`ENV_BOARD`], [`ENV_TASK`]
//! and [`ENV_RUN_ID`]. The child reads them once, when it registers these
//! tools, and every call is held to that binding: the worker reads its own
//! board, changes the lifecycle of its own task only while its run still owns
//! the task, and never sees the orchestrator tools. Delegated sub-agents share
//! the worker's process and registry, so the agent loop marks each call with
//! its delegate depth and the tools refuse any call made below the top level.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::{ToolError, ToolHandler, ToolSchema};
use hermes_tools::ToolRegistry;
use serde_json::{json, Map, Value};

use super::store::KanbanStoreLock;
use super::{
    add_task, build_worker_context, maybe_checkpoint_to_contextlattice, move_task, set_blocked,
    KanbanActionInput, KanbanBoard, KanbanLane, KanbanStore, KanbanTask, NewKanbanTaskInput,
};

/// Toolset the kanban tools register under.
pub const KANBAN_TOOLSET: &str = "kanban";
/// Board id `kanban dispatch` binds a worker to.
pub const ENV_BOARD: &str = "HERMES_KANBAN_BOARD";
/// Task id `kanban dispatch` binds a worker to.
pub const ENV_TASK: &str = "HERMES_KANBAN_TASK";
/// Background job id of the run `kanban dispatch` started.
pub const ENV_RUN_ID: &str = "HERMES_KANBAN_RUN_ID";
/// Every variable of a worker binding.
pub const KANBAN_WORKER_ENV: [&str; 3] = [ENV_BOARD, ENV_TASK, ENV_RUN_ID];

const LIST_DEFAULT_LIMIT: u64 = 50;
const LIST_MAX_LIMIT: u64 = 200;

/// The board, task and run a dispatched worker was started for. Any of the 3
/// variables set makes the process a worker; a missing value then refuses
/// the calls that need it rather than widening the worker to an orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KanbanWorkerBinding {
    pub board: String,
    pub task: String,
    pub run: String,
}

impl KanbanWorkerBinding {
    /// The binding in this process's environment, if it was dispatched.
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let read = |key| get(key).map(|v| v.trim().to_string()).unwrap_or_default();
        let binding = Self {
            board: read(ENV_BOARD),
            task: read(ENV_TASK),
            run: read(ENV_RUN_ID),
        };
        let any = !(binding.board.is_empty() && binding.task.is_empty() && binding.run.is_empty());
        any.then_some(binding)
    }

    /// The 3 variables `kanban dispatch` gives a worker's process.
    pub fn env(&self) -> [(&'static str, String); 3] {
        [
            (ENV_BOARD, self.board.clone()),
            (ENV_TASK, self.task.clone()),
            (ENV_RUN_ID, self.run.clone()),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Show,
    List,
    Complete,
    Block,
    Create,
    Link,
    Unblock,
    Attachments,
}

const TOOLS: [Tool; 8] = [
    Tool::Show,
    Tool::List,
    Tool::Complete,
    Tool::Block,
    Tool::Create,
    Tool::Link,
    Tool::Unblock,
    Tool::Attachments,
];

const DESC_TASK_ID: &str = "Task id (for example K-0003). A dispatched worker may omit it to mean its own task; any other caller must pass it (find ids with kanban_list).";
const DESC_BOARD: &str = "Board id or name. Omit it to use the current board; a dispatched worker may name only its own board.";

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Show => "kanban_show",
            Self::List => "kanban_list",
            Self::Complete => "kanban_complete",
            Self::Block => "kanban_block",
            Self::Create => "kanban_create",
            Self::Link => "kanban_link",
            Self::Unblock => "kanban_unblock",
            Self::Attachments => "kanban_attachments",
        }
    }

    /// Routing tools a dispatched worker never sees.
    fn orchestrator_only(self) -> bool {
        matches!(self, Self::List | Self::Unblock)
    }

    fn description(self) -> &'static str {
        match self {
            Self::Show => "Read a kanban task: title, details, lane, dependencies, attachments and a ready-made worker_context. A dispatched worker may omit task_id to read its own task.",
            Self::List => "List kanban tasks on a board so an orchestrator can find work to route, filtered by lane, assignee or tenant. Returns at most 50 rows by default, 200 at most. Not available to dispatched workers.",
            Self::Complete => "Mark a task done with a short summary of what was done. A dispatched worker may complete only its own running task.",
            Self::Block => "Move a task to blocked with the reason work stopped. A dispatched worker may block only its own running task.",
            Self::Create => "Create a follow-up task on the board, in triage or todo, and return its id.",
            Self::Link => "Make child_id depend on parent_id. Refuses a link that would form a cycle.",
            Self::Unblock => "Move a blocked task back to todo. Not available to dispatched workers.",
            Self::Attachments => "List a task's attachments with the absolute path of each file.",
        }
    }

    fn properties(self) -> Value {
        let task_id = json!({"type": "string", "description": DESC_TASK_ID});
        let mut props = match self {
            Self::Show | Self::Attachments => json!({"task_id": task_id}),
            Self::List => json!({
                "lane": {"type": "string", "enum": ["triage", "todo", "doing", "blocked", "done"], "description": "Only tasks in this lane."},
                "assignee": {"type": "string", "description": "Only tasks with this assignee."},
                "tenant": {"type": "string", "description": "Only tasks with this tenant."},
                "include_archived": {"type": "boolean", "description": "Include archived tasks. Defaults to false."},
                "limit": {"type": "integer", "description": "Most rows to return: default 50, at most 200."}
            }),
            Self::Complete => json!({
                "task_id": task_id,
                "summary": {"type": "string", "description": "What was done, in 1 to 3 sentences."},
                "result": {"type": "string", "description": "A short result line, used when summary is absent."}
            }),
            Self::Block => json!({
                "task_id": task_id,
                "reason": {"type": "string", "description": "What stopped the work, in a sentence or 2."}
            }),
            Self::Create => json!({
                "title": {"type": "string", "description": "Task title."},
                "description": {"type": "string", "description": "Details a worker needs."},
                "lane": {"type": "string", "enum": ["triage", "todo"], "description": "Starting lane. Defaults to todo."},
                "priority": {"type": "integer", "description": "1 (highest) to 5. Defaults to 3."},
                "assignee": {"type": "string", "description": "Assignee or profile name."},
                "tenant": {"type": "string", "description": "Tenant or project namespace."},
                "depends_on": {"type": "array", "items": {"type": "string"}, "description": "Ids of tasks on the same board this task waits for."},
                "goal_mode": {"type": "boolean", "description": "Run the task in goal mode when dispatched."},
                "goal_max_turns": {"type": "integer", "description": "Turn budget for goal mode."}
            }),
            Self::Link => json!({
                "parent_id": {"type": "string", "description": "Task that must finish first."},
                "child_id": {"type": "string", "description": "Task that waits for the parent."}
            }),
            Self::Unblock => json!({
                "task_id": {"type": "string", "description": "Id of the blocked task."}
            }),
        };
        props
            .as_object_mut()
            .expect("tool properties are an object")
            .insert(
                "board".to_string(),
                json!({"type": "string", "description": DESC_BOARD}),
            );
        props
    }

    fn required(self) -> &'static [&'static str] {
        match self {
            Self::Block => &["reason"],
            Self::Create => &["title"],
            Self::Link => &["parent_id", "child_id"],
            Self::Unblock => &["task_id"],
            _ => &[],
        }
    }

    fn schema(self) -> ToolSchema {
        let parameters = serde_json::from_value(json!({
            "type": "object",
            "properties": self.properties(),
            "required": self.required(),
            "additionalProperties": false,
        }))
        .expect("kanban tool schema is valid");
        ToolSchema::new(self.name(), self.description(), parameters)
    }
}

struct KanbanToolContext {
    store_path: PathBuf,
    binding: Option<KanbanWorkerBinding>,
    checkpoint: bool,
}

struct KanbanTool {
    tool: Tool,
    ctx: Arc<KanbanToolContext>,
}

#[async_trait]
impl ToolHandler for KanbanTool {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        // Read before leaving this task: the mark does not follow spawn_blocking.
        let depth = hermes_core::traits::tool_call_delegate_depth();
        let (tool, ctx) = (self.tool, Arc::clone(&self.ctx));
        tokio::task::spawn_blocking(move || run(tool, &ctx, depth, &params))
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("{} failed: {e}", tool.name())))?
    }

    fn schema(&self) -> ToolSchema {
        self.tool.schema()
    }
}

/// Register the kanban tools on `registry`, bound to `binding` when this
/// process is a dispatched worker.
pub fn register_kanban_tools(registry: &ToolRegistry, binding: Option<KanbanWorkerBinding>) {
    register_kanban_tools_at(registry, super::kanban_store_path(), binding, true);
}

fn register_kanban_tools_at(
    registry: &ToolRegistry,
    store_path: PathBuf,
    binding: Option<KanbanWorkerBinding>,
    checkpoint: bool,
) {
    let worker = binding.is_some();
    let ctx = Arc::new(KanbanToolContext {
        store_path,
        binding,
        checkpoint,
    });
    for tool in TOOLS {
        let visible = !(worker && tool.orchestrator_only());
        registry.register(
            tool.name(),
            KANBAN_TOOLSET,
            tool.schema(),
            Arc::new(KanbanTool {
                tool,
                ctx: Arc::clone(&ctx),
            }),
            Arc::new(move || visible),
            Vec::new(),
            true,
            tool.description(),
            "📋",
            None,
        );
    }
}

/// A dispatched worker is offered its tools whatever the platform's toolset
/// list says; the orchestrator tools stay out.
pub fn offer_kanban_worker_tools(
    schemas: &mut Vec<ToolSchema>,
    binding: Option<&KanbanWorkerBinding>,
) {
    if binding.is_none() {
        return;
    }
    for tool in TOOLS.into_iter().filter(|tool| !tool.orchestrator_only()) {
        if !schemas.iter().any(|schema| schema.name == tool.name()) {
            schemas.push(tool.schema());
        }
    }
    schemas.retain(|schema| {
        !TOOLS
            .iter()
            .any(|tool| tool.orchestrator_only() && tool.name() == schema.name)
    });
    schemas.sort_by(|a, b| a.name.cmp(&b.name));
}

/// Names of every kanban tool.
pub fn kanban_tool_names() -> impl Iterator<Item = &'static str> {
    TOOLS.into_iter().map(Tool::name)
}

fn refused(message: impl Into<String>) -> ToolError {
    ToolError::InvalidParams(format!("{} Nothing changed.", message.into()))
}

fn store_error(error: hermes_core::AgentError) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

fn run(
    tool: Tool,
    ctx: &KanbanToolContext,
    depth: Option<u32>,
    params: &Value,
) -> Result<String, ToolError> {
    let name = tool.name();
    match depth {
        Some(depth) if depth > 0 => {
            return Err(refused(format!(
                "{name} refused: delegated sub-agents cannot use kanban tools. Return your findings to the parent agent."
            )))
        }
        None if ctx.binding.is_some() => {
            return Err(refused(format!(
                "{name} refused: this call did not come from the dispatched worker's own agent."
            )))
        }
        _ => {}
    }
    let empty = Map::new();
    let args = match params {
        Value::Object(map) => map,
        Value::Null => &empty,
        _ => return Err(refused(format!("{name}: arguments must be a JSON object."))),
    };
    let schema = tool.properties();
    let allowed = schema.as_object().expect("tool properties are an object");
    let mut unknown: Vec<&str> = args
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains_key(*key))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        let mut valid: Vec<&str> = allowed.keys().map(String::as_str).collect();
        valid.sort_unstable();
        return Err(refused(format!(
            "{name}: unknown parameter(s): {}. Valid parameters: {}.",
            unknown.join(", "),
            valid.join(", ")
        )));
    }
    if tool.orchestrator_only() && ctx.binding.is_some() {
        return Err(refused(format!(
            "{name} is for orchestrators; a dispatched worker uses kanban_complete or kanban_block for its own task."
        )));
    }

    let lock = KanbanStoreLock::open(ctx.store_path.clone()).map_err(store_error)?;
    let mut store = lock.load().map_err(store_error)?;
    let board_idx = resolve_board(&store, ctx.binding.as_ref(), opt_str(args, "board")?)?;
    let board = &mut store.boards[board_idx];
    let outcome = match tool {
        Tool::Show => show(board, ctx, args)?,
        Tool::List => list(board, args)?,
        Tool::Complete => complete(board, ctx, args)?,
        Tool::Block => block(board, ctx, args)?,
        Tool::Create => create(board, args)?,
        Tool::Link => link(board, ctx, args)?,
        Tool::Unblock => unblock(board, args)?,
        Tool::Attachments => attachments(board, ctx, args)?,
    };
    let Outcome { reply, change } = outcome;
    if let Some((task_id, lane, summary)) = change {
        let snapshot = store.boards[board_idx].clone();
        lock.save(&store).map_err(store_error)?;
        if ctx.checkpoint {
            maybe_checkpoint_to_contextlattice(
                &snapshot,
                KanbanActionInput {
                    action: name.to_string(),
                    task_id: Some(task_id),
                    lane,
                    summary,
                },
            );
        }
    }
    Ok(reply.to_string())
}

struct Outcome {
    reply: Value,
    /// Task id, lane and summary of a change to save; `None` for a read.
    change: Option<(String, Option<KanbanLane>, String)>,
}

impl Outcome {
    fn read(reply: Value) -> Self {
        Self {
            reply,
            change: None,
        }
    }

    fn changed(reply: Value, task: &KanbanTask, summary: impl Into<String>) -> Self {
        Self {
            reply,
            change: Some((task.id.clone(), Some(task.lane), summary.into())),
        }
    }
}

/// The board a call acts on, never changing the store's current board.
fn resolve_board(
    store: &KanbanStore,
    binding: Option<&KanbanWorkerBinding>,
    requested: Option<String>,
) -> Result<usize, ToolError> {
    let find = |raw: &str| {
        store
            .boards
            .iter()
            .position(|board| board.id == raw)
            .or_else(|| {
                store
                    .boards
                    .iter()
                    .position(|board| board.name.eq_ignore_ascii_case(raw))
            })
    };
    match binding {
        Some(binding) => {
            let bound = store
                .boards
                .iter()
                .position(|board| !binding.board.is_empty() && board.id == binding.board)
                .ok_or_else(|| {
                    refused(format!(
                        "this worker's board `{}` is not in the kanban store.",
                        binding.board
                    ))
                })?;
            if let Some(raw) = requested {
                if find(&raw) != Some(bound) {
                    return Err(refused(format!(
                        "this worker is bound to board `{}`; refusing board `{raw}`.",
                        binding.board
                    )));
                }
            }
            Ok(bound)
        }
        None => match requested {
            Some(raw) => find(&raw).ok_or_else(|| refused(format!("board `{raw}` not found."))),
            None => Ok(store
                .boards
                .iter()
                .position(|board| board.id == store.current_board_id)
                .unwrap_or(0)),
        },
    }
}

/// A trimmed, non-blank string argument; numbers are taken as their text.
fn opt_str(args: &Map<String, Value>, key: &str) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => {
            let value = raw.trim();
            Ok((!value.is_empty()).then(|| value.to_string()))
        }
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(_) => Err(refused(format!("{key} must be a string."))),
    }
}

fn required_str(args: &Map<String, Value>, key: &str) -> Result<String, ToolError> {
    opt_str(args, key)?.ok_or_else(|| refused(format!("{key} is required.")))
}

fn opt_bool(args: &Map<String, Value>, key: &str) -> Result<Option<bool>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::String(raw)) => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Ok(Some(true)),
            "false" | "no" | "0" => Ok(Some(false)),
            _ => Err(refused(format!("{key} must be true or false."))),
        },
        Some(_) => Err(refused(format!("{key} must be true or false."))),
    }
}

fn opt_u64(args: &Map<String, Value>, key: &str) -> Result<Option<u64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .or_else(|| value.as_str().and_then(|raw| raw.trim().parse().ok()))
            .map(Some)
            .ok_or_else(|| refused(format!("{key} must be a whole number."))),
    }
}

fn task_index(board: &KanbanBoard, id: &str) -> Result<usize, ToolError> {
    board
        .tasks
        .iter()
        .position(|task| task.id.eq_ignore_ascii_case(id))
        .ok_or_else(|| refused(format!("task {id} not found on board `{}`.", board.id)))
}

/// The task a call names, or a worker's own task when it names none.
fn target_task(
    args: &Map<String, Value>,
    ctx: &KanbanToolContext,
    tool: Tool,
) -> Result<Option<String>, ToolError> {
    if let Some(id) = opt_str(args, "task_id")? {
        return Ok(Some(id));
    }
    match &ctx.binding {
        Some(binding) if !binding.task.is_empty() => Ok(Some(binding.task.clone())),
        Some(_) => Err(refused(format!(
            "{}: task_id is required; this worker has no {ENV_TASK}.",
            tool.name()
        ))),
        None => Ok(None),
    }
}

fn require_task(
    args: &Map<String, Value>,
    ctx: &KanbanToolContext,
    tool: Tool,
) -> Result<String, ToolError> {
    target_task(args, ctx, tool)?.ok_or_else(|| {
        refused(format!(
            "{}: task_id is required; this session is not a dispatched worker, so there is no default task. Find ids with kanban_list.",
            tool.name()
        ))
    })
}

/// A worker may finish or block only its own task, only while the task is
/// running and its stored run is this worker's run.
fn check_worker_owns(
    binding: &KanbanWorkerBinding,
    task: &KanbanTask,
    tool: Tool,
) -> Result<(), ToolError> {
    let name = tool.name();
    if !task.id.eq_ignore_ascii_case(&binding.task) {
        return Err(refused(format!(
            "{name} refused: this worker is bound to task {}, not {}. Use kanban_create for follow-up work.",
            binding.task, task.id
        )));
    }
    if binding.run.is_empty() {
        return Err(refused(format!(
            "{name} refused: this worker has no {ENV_RUN_ID}, so it cannot show that it owns the task's current run."
        )));
    }
    if task.background_job_id.as_deref() != Some(binding.run.as_str()) {
        return Err(refused(format!(
            "{name} refused: task {} is no longer owned by run {}.",
            task.id, binding.run
        )));
    }
    if task.lane != KanbanLane::Doing {
        return Err(refused(format!(
            "{name} refused: task {} is in {}, not doing.",
            task.id,
            task.lane.as_str()
        )));
    }
    Ok(())
}

fn task_json(board: &KanbanBoard, task: &KanbanTask) -> Value {
    let mut value = serde_json::to_value(task).unwrap_or(Value::Null);
    if let Some(map) = value.as_object_mut() {
        map.insert("board".to_string(), json!(board.id));
        map.insert(
            "worker_context".to_string(),
            json!(build_worker_context(task)),
        );
    }
    value
}

fn show(
    board: &KanbanBoard,
    ctx: &KanbanToolContext,
    args: &Map<String, Value>,
) -> Result<Outcome, ToolError> {
    let Some(id) = target_task(args, ctx, Tool::Show)? else {
        return Ok(Outcome::read(json!({
            "ok": true,
            "task": null,
            "hint": "No task_id given and this session is not a dispatched worker. Call kanban_list to find task ids, then pass task_id.",
        })));
    };
    let task = &board.tasks[task_index(board, &id)?];
    Ok(Outcome::read(
        json!({"ok": true, "task": task_json(board, task)}),
    ))
}

fn list(board: &KanbanBoard, args: &Map<String, Value>) -> Result<Outcome, ToolError> {
    let lane = match opt_str(args, "lane")? {
        Some(raw) => Some(
            KanbanLane::parse(&raw)
                .ok_or_else(|| refused(format!("lane `{raw}` is not a kanban lane.")))?,
        ),
        None => None,
    };
    let assignee = opt_str(args, "assignee")?;
    let tenant = opt_str(args, "tenant")?;
    let include_archived = opt_bool(args, "include_archived")?.unwrap_or(false);
    let limit = opt_u64(args, "limit")?.unwrap_or(LIST_DEFAULT_LIMIT);
    if limit == 0 {
        return Err(refused("limit must be at least 1."));
    }
    let limit = limit.min(LIST_MAX_LIMIT) as usize;
    let matches = |task: &&KanbanTask| {
        lane.is_none_or(|lane| task.lane == lane)
            && assignee
                .as_deref()
                .is_none_or(|want| task.assignee.as_deref() == Some(want))
            && tenant
                .as_deref()
                .is_none_or(|want| task.tenant.as_deref() == Some(want))
    };
    let archived: &[KanbanTask] = if include_archived {
        &board.archived
    } else {
        &[]
    };
    let rows: Vec<Value> = board
        .tasks
        .iter()
        .map(|task| (task, false))
        .chain(archived.iter().map(|task| (task, true)))
        .filter(|(task, _)| matches(task))
        .map(|(task, archived)| {
            json!({
                "id": task.id,
                "title": task.title,
                "lane": task.lane.as_str(),
                "priority": task.priority,
                "assignee": task.assignee,
                "tenant": task.tenant,
                "depends_on": task.depends_on,
                "background_job_id": task.background_job_id,
                "archived": archived,
            })
        })
        .collect();
    let total = rows.len();
    let rows: Vec<Value> = rows.into_iter().take(limit).collect();
    Ok(Outcome::read(json!({
        "ok": true,
        "board": board.id,
        "tasks": rows,
        "total": total,
        "truncated": total > limit,
    })))
}

fn lifecycle_target<'a>(
    board: &'a mut KanbanBoard,
    ctx: &KanbanToolContext,
    args: &Map<String, Value>,
    tool: Tool,
) -> Result<&'a mut KanbanTask, ToolError> {
    let id = require_task(args, ctx, tool)?;
    let idx = task_index(board, &id)?;
    let task = &mut board.tasks[idx];
    if let Some(binding) = &ctx.binding {
        check_worker_owns(binding, task, tool)?;
    }
    if task.lane == KanbanLane::Done {
        return Err(refused(format!(
            "{}: task {} is already done.",
            tool.name(),
            task.id
        )));
    }
    Ok(task)
}

fn complete(
    board: &mut KanbanBoard,
    ctx: &KanbanToolContext,
    args: &Map<String, Value>,
) -> Result<Outcome, ToolError> {
    let summary = opt_str(args, "summary")?.or(opt_str(args, "result")?);
    let Some(summary) = summary else {
        return Err(refused("kanban_complete needs summary or result."));
    };
    let task = lifecycle_target(board, ctx, args, Tool::Complete)?;
    move_task(task, KanbanLane::Done, Some(summary.clone()));
    let reply = json!({"ok": true, "task_id": task.id, "lane": task.lane.as_str()});
    Ok(Outcome::changed(reply, task, summary))
}

fn block(
    board: &mut KanbanBoard,
    ctx: &KanbanToolContext,
    args: &Map<String, Value>,
) -> Result<Outcome, ToolError> {
    let reason = required_str(args, "reason")?;
    let task = lifecycle_target(board, ctx, args, Tool::Block)?;
    set_blocked(task, Some(reason.clone()));
    let reply = json!({"ok": true, "task_id": task.id, "lane": task.lane.as_str()});
    Ok(Outcome::changed(reply, task, reason))
}

fn create(board: &mut KanbanBoard, args: &Map<String, Value>) -> Result<Outcome, ToolError> {
    let title = required_str(args, "title")?;
    let lane = match opt_str(args, "lane")? {
        None => KanbanLane::Todo,
        Some(raw) => match KanbanLane::parse(&raw) {
            Some(lane @ (KanbanLane::Triage | KanbanLane::Todo)) => lane,
            _ => {
                return Err(refused(format!(
                    "lane must be triage or todo, not `{raw}`."
                )))
            }
        },
    };
    let priority = match opt_u64(args, "priority")? {
        None => 3,
        Some(p @ 1..=5) => p as u8,
        Some(p) => return Err(refused(format!("priority must be 1 to 5, not {p}."))),
    };
    let depends_on = match args.get("depends_on") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(one)) => vec![one.trim().to_string()],
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(|s| s.trim().to_string())
                    .ok_or_else(|| refused("depends_on must be a list of task ids."))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(refused("depends_on must be a list of task ids.")),
    };
    let depends_on = depends_on
        .into_iter()
        .filter(|id| !id.is_empty())
        .map(|id| task_index(board, &id).map(|idx| board.tasks[idx].id.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    let goal_max_turns = match opt_u64(args, "goal_max_turns")? {
        None => None,
        Some(n @ 1..=10_000) => Some(n as u32),
        Some(n) => {
            return Err(refused(format!(
                "goal_max_turns must be 1 to 10000, not {n}."
            )))
        }
    };
    let task = add_task(
        board,
        NewKanbanTaskInput {
            title,
            lane,
            priority,
            assignee: opt_str(args, "assignee")?,
            tenant: opt_str(args, "tenant")?,
            description: opt_str(args, "description")?,
            depends_on,
            goal_mode: opt_bool(args, "goal_mode")?.unwrap_or(false),
            goal_max_turns,
        },
    );
    let reply =
        json!({"ok": true, "task_id": task.id, "lane": task.lane.as_str(), "board": board.id});
    let summary = task.title.clone();
    Ok(Outcome::changed(reply, &task, summary))
}

/// Whether `from` already waits, directly or through others, on `target`.
fn depends_transitively(board: &KanbanBoard, from: &str, target: &str) -> bool {
    let mut stack = vec![from.to_string()];
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = stack.pop() {
        if id.eq_ignore_ascii_case(target) {
            return true;
        }
        if !seen.insert(id.to_ascii_uppercase()) {
            continue;
        }
        if let Some(task) = board.tasks.iter().find(|t| t.id.eq_ignore_ascii_case(&id)) {
            stack.extend(task.depends_on.iter().cloned());
        }
    }
    false
}

fn link(
    board: &mut KanbanBoard,
    ctx: &KanbanToolContext,
    args: &Map<String, Value>,
) -> Result<Outcome, ToolError> {
    let parent_raw = required_str(args, "parent_id")?;
    let child_raw = required_str(args, "child_id")?;
    let parent = board.tasks[task_index(board, &parent_raw)?].id.clone();
    let child_idx = task_index(board, &child_raw)?;
    let child = board.tasks[child_idx].id.clone();
    if parent == child {
        return Err(refused("a task cannot depend on itself."));
    }
    if let Some(binding) = &ctx.binding {
        let task = &board.tasks[child_idx];
        let own_run = !binding.run.is_empty()
            && task.background_job_id.as_deref() == Some(binding.run.as_str());
        if task.lane == KanbanLane::Doing && !own_run {
            return Err(refused(format!(
                "kanban_link refused: task {child} is running under another run."
            )));
        }
    }
    if depends_transitively(board, &parent, &child) {
        return Err(refused(format!(
            "linking {child} after {parent} would form a cycle."
        )));
    }
    let task = &mut board.tasks[child_idx];
    let reply = json!({"ok": true, "parent_id": parent, "child_id": child});
    if task
        .depends_on
        .iter()
        .any(|id| id.eq_ignore_ascii_case(&parent))
    {
        return Ok(Outcome::read(reply));
    }
    task.depends_on.push(parent.clone());
    task.updated_at = hermes_core::time::now_rfc3339();
    Ok(Outcome::changed(
        reply,
        task,
        format!("depends_on {parent}"),
    ))
}

fn unblock(board: &mut KanbanBoard, args: &Map<String, Value>) -> Result<Outcome, ToolError> {
    let id = required_str(args, "task_id")?;
    let idx = task_index(board, &id)?;
    let task = &mut board.tasks[idx];
    if task.lane != KanbanLane::Blocked {
        return Err(refused(format!(
            "task {} is in {}, not blocked.",
            task.id,
            task.lane.as_str()
        )));
    }
    move_task(task, KanbanLane::Todo, None);
    let reply = json!({"ok": true, "task_id": task.id, "lane": task.lane.as_str()});
    Ok(Outcome::changed(reply, task, "unblocked"))
}

fn attachments(
    board: &KanbanBoard,
    ctx: &KanbanToolContext,
    args: &Map<String, Value>,
) -> Result<Outcome, ToolError> {
    let id = require_task(args, ctx, Tool::Attachments)?;
    let task = &board.tasks[task_index(board, &id)?];
    Ok(Outcome::read(json!({
        "ok": true,
        "task_id": task.id,
        "attachments": task.attachments,
    })))
}

#[cfg(test)]
mod tests;
