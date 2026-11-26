use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use super::*;
use crate::kanban::store::{load_store_from_path, save_store_to_path};
use crate::kanban::{add_task, create_or_select_board, KanbanStore, NewKanbanTaskInput};

/// A store with board `main` (K-0001 running under run-a, K-0002 in todo,
/// K-0003 running under run-z) and board `side` (its own K-0001).
struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    jobs: PathBuf,
}

impl Fixture {
    fn ctx(&self, binding: Option<KanbanWorkerBinding>) -> KanbanToolContext {
        KanbanToolContext {
            store_path: self.path.clone(),
            jobs_dir: self.jobs.clone(),
            binding,
            checkpoint: false,
        }
    }

    /// Write background job `run`'s status file.
    fn job_status(&self, run: &str, status: &str) {
        std::fs::write(
            self.jobs.join(format!("{run}.json")),
            json!({"id": run, "status": status}).to_string(),
        )
        .unwrap();
    }
}

fn new_task(title: &str, lane: KanbanLane) -> NewKanbanTaskInput {
    NewKanbanTaskInput {
        title: title.to_string(),
        lane,
        priority: 3,
        assignee: None,
        tenant: None,
        description: None,
        depends_on: Vec::new(),
        goal_mode: false,
        goal_max_turns: None,
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("boards.json");
    let mut store = KanbanStore::default();
    {
        let side = create_or_select_board(&mut store, "Side", None);
        add_task(side, new_task("side work", KanbanLane::Todo));
    }
    store.current_board_id = "main".to_string();
    let main = &mut store.boards[0];
    let mut build = new_task("build", KanbanLane::Doing);
    build.assignee = Some("ada".to_string());
    add_task(main, build);
    let mut other = new_task("other", KanbanLane::Todo);
    other.tenant = Some("acme".to_string());
    add_task(main, other);
    add_task(main, new_task("running elsewhere", KanbanLane::Doing));
    main.tasks[0].background_job_id = Some("run-a".to_string());
    main.tasks[2].background_job_id = Some("run-z".to_string());
    save_store_to_path(&store, &path).expect("save");
    let jobs = dir.path().join("background_jobs");
    std::fs::create_dir(&jobs).expect("jobs dir");
    Fixture {
        _dir: dir,
        path,
        jobs,
    }
}

fn worker(run: &str) -> Option<KanbanWorkerBinding> {
    Some(KanbanWorkerBinding {
        board: "main".to_string(),
        task: "K-0001".to_string(),
        run: run.to_string(),
    })
}

fn call(
    fx: &Fixture,
    binding: Option<KanbanWorkerBinding>,
    depth: Option<u32>,
    tool: Tool,
    args: Value,
) -> Result<Value, String> {
    run(tool, &fx.ctx(binding), depth, &args)
        .map(|reply| serde_json::from_str(&reply).expect("reply is JSON"))
        .map_err(|error| error.to_string())
}

fn as_worker(fx: &Fixture, tool: Tool, args: Value) -> Result<Value, String> {
    call(fx, worker("run-a"), Some(0), tool, args)
}

fn as_orchestrator(fx: &Fixture, tool: Tool, args: Value) -> Result<Value, String> {
    call(fx, None, Some(0), tool, args)
}

fn store(fx: &Fixture) -> KanbanStore {
    load_store_from_path(&fx.path).expect("load")
}

fn task(fx: &Fixture, board: &str, id: &str) -> KanbanTask {
    let store = store(fx);
    let board = store.boards.iter().find(|b| b.id == board).expect("board");
    board
        .tasks
        .iter()
        .find(|t| t.id == id)
        .cloned()
        .expect("task")
}

fn assert_refused(result: Result<Value, String>, needle: &str) {
    let error = result.expect_err("expected a refusal");
    assert!(error.contains(needle), "{error}");
    assert!(error.contains("Nothing changed."), "{error}");
}

// ---- binding ----

#[test]
fn binding_comes_only_from_the_dispatch_variables() {
    let none = KanbanWorkerBinding::from_lookup(|_| None);
    assert_eq!(none, None);
    let blank = KanbanWorkerBinding::from_lookup(|_| Some("  ".to_string()));
    assert_eq!(blank, None);
    let full = KanbanWorkerBinding::from_lookup(|key| {
        Some(
            match key {
                ENV_BOARD => " main ",
                ENV_TASK => "K-0001",
                ENV_RUN_ID => "run-a\n",
                _ => "",
            }
            .to_string(),
        )
    });
    assert_eq!(full, worker("run-a"));
}

#[test]
fn a_partial_binding_stays_a_worker() {
    let run_only =
        KanbanWorkerBinding::from_lookup(|key| (key == ENV_RUN_ID).then(|| "run-a".to_string()))
            .expect("any variable makes a worker");
    assert_eq!(run_only.task, "");
    let fx = fixture();
    assert_refused(
        call(&fx, Some(run_only.clone()), Some(0), Tool::Show, json!({})),
        "board",
    );
    assert_refused(
        call(&fx, Some(run_only), Some(0), Tool::List, json!({})),
        "for orchestrators",
    );
}

// ---- visibility ----

fn registry_names(binding: Option<KanbanWorkerBinding>) -> Vec<String> {
    let fx = fixture();
    let registry = ToolRegistry::new();
    register_kanban_tools_at(&registry, fx.ctx(binding), true);
    let mut names: Vec<String> = registry
        .get_definitions()
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    names.sort();
    names
}

#[test]
fn workers_never_see_the_orchestrator_tools() {
    let names = registry_names(worker("run-a"));
    assert_eq!(
        names,
        [
            "kanban_attachments",
            "kanban_block",
            "kanban_complete",
            "kanban_create",
            "kanban_link",
            "kanban_show"
        ]
    );
    assert_eq!(registry_names(None).len(), 8);
}

#[test]
fn a_worker_is_offered_its_tools_whatever_the_platform_list_says() {
    let mut schemas = vec![
        Tool::List.schema(),
        ToolSchema::new(
            "terminal",
            "t",
            serde_json::from_value(json!({"type": "object"})).unwrap(),
        ),
    ];
    offer_kanban_worker_tools(&mut schemas, None);
    assert_eq!(schemas.len(), 2);

    let binding = worker("run-a");
    offer_kanban_worker_tools(&mut schemas, binding.as_ref());
    offer_kanban_worker_tools(&mut schemas, binding.as_ref());
    let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "kanban_attachments",
            "kanban_block",
            "kanban_complete",
            "kanban_create",
            "kanban_link",
            "kanban_show",
            "terminal"
        ]
    );
}

/// The kanban tools a CLI session with `config` is offered.
fn offered_kanban_tools(
    fx: &Fixture,
    config: &hermes_config::GatewayConfig,
    binding: Option<KanbanWorkerBinding>,
) -> Vec<String> {
    let registry = Arc::new(ToolRegistry::new());
    // A built-in from the default `hermes-cli` toolset, so the default plan
    // resolves to named tools rather than falling back to every tool.
    registry.register(
        "terminal",
        "terminal",
        ToolSchema::new("terminal", "t", Tool::Show.schema().parameters),
        Arc::new(KanbanTool {
            tool: Tool::Show,
            ctx: Arc::new(fx.ctx(None)),
        }),
        Arc::new(|| true),
        Vec::new(),
        true,
        "t",
        "t",
        None,
    );
    let opted_in = kanban_toolset_named(config, "cli");
    register_kanban_tools_at(&registry, fx.ctx(binding.clone()), opted_in);
    let mut schemas = hermes_tool_planning::resolve_platform_tool_schemas(config, "cli", &registry);
    offer_kanban_worker_tools(&mut schemas, binding.as_ref());
    schemas
        .into_iter()
        .map(|schema| schema.name)
        .filter(|name| name.starts_with("kanban_"))
        .collect()
}

#[test]
fn an_orchestrator_gets_the_tools_only_by_naming_the_kanban_toolset() {
    let fx = fixture();
    let with = |list: &[&str]| {
        let mut config = hermes_config::GatewayConfig::default();
        config.platform_toolsets.insert(
            "cli".to_string(),
            list.iter().map(|s| s.to_string()).collect(),
        );
        config
    };
    let default = hermes_config::GatewayConfig::default();
    assert!(offered_kanban_tools(&fx, &default, None).is_empty());
    // Wildcards take every registered toolset, so they must not reach kanban.
    for list in [&["all"][..], &["*"], &["hermes-cli", "all"]] {
        let names = offered_kanban_tools(&fx, &with(list), None);
        assert!(names.is_empty(), "{list:?} offered {names:?}");
    }
    assert_eq!(offered_kanban_tools(&fx, &with(&["kanban"]), None).len(), 8);
    assert_eq!(
        offered_kanban_tools(&fx, &with(&["all", " Kanban "]), None).len(),
        8
    );
    // A worker gets its own tools on the default list and on a wildcard.
    for config in [default, with(&["all"])] {
        assert_eq!(offered_kanban_tools(&fx, &config, worker("run-a")).len(), 6);
    }
}

#[test]
fn a_terminal_descendant_of_a_worker_is_not_a_worker() {
    let fx = fixture();
    // What the terminal, execute_code and MCP servers pass a child.
    let child_env = hermes_core::subprocess_env::hermes_subprocess_env_from(
        [
            (ENV_BOARD, "main"),
            (ENV_TASK, "K-0001"),
            (ENV_RUN_ID, "run-a"),
            ("PATH", "/usr/bin"),
        ],
        false,
    );
    let binding = KanbanWorkerBinding::from_lookup(|key| child_env.get(key).cloned());
    assert_eq!(binding, None);
    let default = hermes_config::GatewayConfig::default();
    assert!(offered_kanban_tools(&fx, &default, binding.clone()).is_empty());
    // A descendant whose own config opts in is an orchestrator, which still
    // cannot finish the task while the worker's run is live.
    fx.job_status("run-a", "running");
    let before = std::fs::read(&fx.path).unwrap();
    for (tool, args) in [
        (Tool::Complete, json!({"task_id": "K-0001", "summary": "x"})),
        (Tool::Block, json!({"task_id": "K-0001", "reason": "x"})),
    ] {
        assert_refused(
            call(&fx, binding.clone(), Some(0), tool, args),
            "live run run-a",
        );
    }
    assert_eq!(std::fs::read(&fx.path).unwrap(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn cron_runs_never_get_kanban_tools() {
    let fx = fixture();
    let registry = ToolRegistry::new();
    register_kanban_tools_at(&registry, fx.ctx(worker("run-a")), false);
    let mut config = hermes_config::GatewayConfig::default();
    // Even a cron plan that names the toolset gets none of it.
    config
        .platform_toolsets
        .insert("cron".to_string(), vec![KANBAN_TOOLSET.to_string()]);
    let offered = hermes_tool_planning::resolve_platform_tool_schemas(
        &config,
        "cron",
        &Arc::new(registry.clone()),
    );
    assert!(offered.iter().any(|schema| schema.name == "kanban_show"));
    let bridged = crate::app::bridge_tool_registry_for_cron(
        &config,
        &registry,
        hermes_tools::ToolOverrides::new(),
    );
    for name in kanban_tool_names() {
        assert!(bridged.get(name).is_none(), "{name} reaches cron runs");
    }
}

// ---- delegation and call origin ----

#[test]
fn delegated_sub_agents_are_refused_in_every_mode() {
    let fx = fixture();
    let before = std::fs::read(&fx.path).unwrap();
    for binding in [worker("run-a"), None] {
        assert_refused(
            call(
                &fx,
                binding.clone(),
                Some(1),
                Tool::Show,
                json!({"task_id": "K-0001"}),
            ),
            "delegated sub-agents",
        );
        assert_refused(
            call(
                &fx,
                binding,
                Some(2),
                Tool::Complete,
                json!({"task_id": "K-0001", "summary": "x"}),
            ),
            "delegated sub-agents",
        );
    }
    assert_eq!(std::fs::read(&fx.path).unwrap(), before);
}

#[test]
fn a_worker_refuses_calls_from_outside_its_agent_loop() {
    let fx = fixture();
    assert_refused(
        call(
            &fx,
            worker("run-a"),
            None,
            Tool::Complete,
            json!({"summary": "x"}),
        ),
        "did not come from the dispatched worker",
    );
    assert_eq!(task(&fx, "main", "K-0001").lane, KanbanLane::Doing);
    // An orchestrator's tools also serve gateway and other unmarked callers.
    assert!(call(&fx, None, None, Tool::Show, json!({"task_id": "K-0002"})).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_depth_mark_reaches_a_handler_through_the_registry_bridge() {
    let fx = fixture();
    let registry = ToolRegistry::new();
    register_kanban_tools_at(&registry, fx.ctx(worker("run-a")), false);
    let show = |depth: Option<u32>| -> String {
        let dispatch = || registry.dispatch("kanban_show", json!({}));
        match depth {
            Some(depth) => hermes_core::traits::with_tool_call_delegate_depth(depth, dispatch),
            None => dispatch(),
        }
    };
    assert!(show(Some(0)).contains("\"K-0001\""), "{}", show(Some(0)));
    assert!(
        show(Some(1)).contains("delegated sub-agents"),
        "{}",
        show(Some(1))
    );
    assert!(show(None).contains("did not come from"), "{}", show(None));
}

// ---- arguments ----

#[test]
fn unknown_arguments_are_refused_with_the_valid_ones() {
    let fx = fixture();
    let result = as_worker(&fx, Tool::Complete, json!({"summary": "x", "progress": 50}));
    let error = result.expect_err("refused");
    assert!(error.contains("unknown parameter(s): progress"), "{error}");
    assert!(
        error.contains(
            "Valid parameters: artifacts, board, created_cards, metadata, result, summary, task_id"
        ),
        "{error}"
    );
    assert_eq!(task(&fx, "main", "K-0001").lane, KanbanLane::Doing);
}

#[test]
fn every_schema_names_exactly_the_arguments_its_handler_accepts() {
    for tool in TOOLS {
        let schema = serde_json::to_value(tool.schema().parameters).unwrap();
        assert_eq!(
            schema["additionalProperties"],
            json!(false),
            "{}",
            tool.name()
        );
        assert!(schema["properties"]["board"].is_object(), "{}", tool.name());
        for required in tool.required() {
            assert!(
                schema["properties"][*required].is_object(),
                "{}",
                tool.name()
            );
        }
    }
}

#[test]
fn task_ids_are_trimmed_matched_without_case_and_numbers_coerced() {
    let fx = fixture();
    let shown = as_orchestrator(&fx, Tool::Show, json!({"task_id": "  k-0002 "})).unwrap();
    assert_eq!(shown["task"]["id"], "K-0002");
    assert_refused(
        as_orchestrator(&fx, Tool::Show, json!({"task_id": 2})),
        "task 2 not found",
    );
    // A blank id means the worker's own task.
    let own = as_worker(&fx, Tool::Show, json!({"task_id": "   "})).unwrap();
    assert_eq!(own["task"]["id"], "K-0001");
}

// ---- show and list ----

#[test]
fn show_defaults_to_the_workers_own_task() {
    let fx = fixture();
    let shown = as_worker(&fx, Tool::Show, json!({})).unwrap();
    assert_eq!(shown["task"]["id"], "K-0001");
    assert_eq!(shown["task"]["board"], "main");
    assert!(shown["task"]["worker_context"]
        .as_str()
        .unwrap()
        .starts_with("Execute Kanban task K-0001: build"));
    // Any task on its own board is readable.
    let other = as_worker(&fx, Tool::Show, json!({"task_id": "K-0002"})).unwrap();
    assert_eq!(other["task"]["title"], "other");
}

#[test]
fn a_bare_show_outside_a_worker_points_at_kanban_list() {
    let fx = fixture();
    let reply = as_orchestrator(&fx, Tool::Show, json!({})).unwrap();
    assert_eq!(reply["ok"], true);
    assert!(reply["task"].is_null());
    assert!(reply["hint"].as_str().unwrap().contains("kanban_list"));
}

#[test]
fn a_bare_lifecycle_call_outside_a_worker_names_kanban_list() {
    let fx = fixture();
    assert_refused(
        as_orchestrator(&fx, Tool::Complete, json!({"summary": "x"})),
        "kanban_list",
    );
}

#[test]
fn list_filters_and_bounds_rows() {
    let fx = fixture();
    let all = as_orchestrator(&fx, Tool::List, json!({})).unwrap();
    assert_eq!(all["total"], 3);
    assert_eq!(all["truncated"], false);
    let doing = as_orchestrator(&fx, Tool::List, json!({"lane": "doing"})).unwrap();
    let ids: Vec<&str> = doing["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["K-0001", "K-0003"]);
    let ada = as_orchestrator(&fx, Tool::List, json!({"assignee": "ada"})).unwrap();
    assert_eq!(ada["total"], 1);
    let acme = as_orchestrator(&fx, Tool::List, json!({"tenant": "acme"})).unwrap();
    assert_eq!(acme["tasks"][0]["id"], "K-0002");
    let one = as_orchestrator(&fx, Tool::List, json!({"limit": 1})).unwrap();
    assert_eq!(one["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(one["truncated"], true);
    let side = as_orchestrator(&fx, Tool::List, json!({"board": "Side"})).unwrap();
    assert_eq!(side["board"], "side");
    assert_eq!(side["tasks"][0]["title"], "side work");
    assert_refused(
        as_orchestrator(&fx, Tool::List, json!({"limit": 0})),
        "limit",
    );
    assert_refused(
        as_orchestrator(&fx, Tool::List, json!({"lane": "review"})),
        "lane",
    );
}

#[test]
fn list_includes_archived_tasks_only_when_asked() {
    let fx = fixture();
    as_orchestrator(
        &fx,
        Tool::Complete,
        json!({"task_id": "K-0002", "summary": "done"}),
    )
    .unwrap();
    let mut store = store(&fx);
    crate::kanban::archive_done(&mut store.boards[0]);
    save_store_to_path(&store, &fx.path).unwrap();
    assert_eq!(
        as_orchestrator(&fx, Tool::List, json!({})).unwrap()["total"],
        2
    );
    let with = as_orchestrator(&fx, Tool::List, json!({"include_archived": "true"})).unwrap();
    assert_eq!(with["total"], 3);
    assert_eq!(with["tasks"][2]["archived"], true);
}

// ---- lifecycle ----

#[test]
fn a_worker_completes_its_own_running_task() {
    let fx = fixture();
    let reply = as_worker(&fx, Tool::Complete, json!({"summary": "shipped it"})).unwrap();
    assert_eq!(
        reply,
        json!({"ok": true, "task_id": "K-0001", "lane": "done"})
    );
    let done = task(&fx, "main", "K-0001");
    assert_eq!(done.lane, KanbanLane::Done);
    assert_eq!(done.run_summary.as_deref(), Some("shipped it"));
    assert_eq!(done.background_job_id.as_deref(), Some("run-a"));
    assert_refused(
        as_worker(&fx, Tool::Complete, json!({"summary": "again"})),
        "not doing",
    );
}

#[test]
fn complete_needs_a_summary_or_result() {
    let fx = fixture();
    assert_refused(
        as_worker(&fx, Tool::Complete, json!({})),
        "summary or result",
    );
    let reply = as_worker(&fx, Tool::Complete, json!({"result": "ok"})).unwrap();
    assert_eq!(reply["lane"], "done");
    assert_eq!(
        task(&fx, "main", "K-0001").run_summary.as_deref(),
        Some("ok")
    );
}

#[test]
fn a_worker_blocks_its_own_task_with_a_reason() {
    let fx = fixture();
    assert_refused(
        as_worker(&fx, Tool::Block, json!({"reason": "  "})),
        "reason is required",
    );
    let reply = as_worker(&fx, Tool::Block, json!({"reason": "needs a key"})).unwrap();
    assert_eq!(reply["lane"], "blocked");
    let blocked = task(&fx, "main", "K-0001");
    assert_eq!(blocked.blocked_reason.as_deref(), Some("needs a key"));
    assert_eq!(blocked.background_job_id, None);
}

#[test]
fn a_worker_cannot_finish_or_block_another_task() {
    let fx = fixture();
    let before = std::fs::read(&fx.path).unwrap();
    for (tool, args) in [
        (Tool::Complete, json!({"task_id": "K-0002", "summary": "x"})),
        (Tool::Block, json!({"task_id": "K-0003", "reason": "x"})),
    ] {
        assert_refused(as_worker(&fx, tool, args), "bound to task K-0001");
    }
    assert_eq!(std::fs::read(&fx.path).unwrap(), before);
}

#[test]
fn a_worker_without_a_run_id_cannot_finish_but_can_read() {
    let fx = fixture();
    let unbound = || {
        call(
            &fx,
            worker(""),
            Some(0),
            Tool::Complete,
            json!({"summary": "x"}),
        )
    };
    assert_refused(unbound(), "no HERMES_KANBAN_RUN_ID");
    assert_refused(
        call(
            &fx,
            worker(""),
            Some(0),
            Tool::Block,
            json!({"reason": "x"}),
        ),
        "no HERMES_KANBAN_RUN_ID",
    );
    assert!(call(&fx, worker(""), Some(0), Tool::Show, json!({})).is_ok());
    assert_eq!(task(&fx, "main", "K-0001").lane, KanbanLane::Doing);
}

#[test]
fn a_stale_worker_cannot_finish_a_re_dispatched_task() {
    let fx = fixture();
    let mut store = store(&fx);
    store.boards[0].tasks[0].background_job_id = Some("run-b".to_string());
    save_store_to_path(&store, &fx.path).unwrap();
    assert_refused(
        as_worker(&fx, Tool::Complete, json!({"summary": "late"})),
        "no longer owned by run run-a",
    );
    let fresh = call(
        &fx,
        worker("run-b"),
        Some(0),
        Tool::Complete,
        json!({"summary": "x"}),
    );
    assert_eq!(fresh.unwrap()["lane"], "done");
}

#[test]
fn a_manual_move_back_to_doing_does_not_return_the_task_to_its_old_run() {
    let fx = fixture();
    as_orchestrator(
        &fx,
        Tool::Block,
        json!({"task_id": "K-0001", "reason": "wait"}),
    )
    .unwrap();
    as_orchestrator(&fx, Tool::Unblock, json!({"task_id": "K-0001"})).unwrap();
    let mut store = store(&fx);
    crate::kanban::move_task(&mut store.boards[0].tasks[0], KanbanLane::Doing, None);
    save_store_to_path(&store, &fx.path).unwrap();
    assert_eq!(task(&fx, "main", "K-0001").lane, KanbanLane::Doing);
    assert_refused(
        as_worker(&fx, Tool::Complete, json!({"summary": "late"})),
        "no longer owned",
    );
}

#[test]
fn a_worker_cannot_finish_its_task_once_it_has_left_doing() {
    let fx = fixture();
    let mut store = store(&fx);
    // A task whose run id survived into another lane, as an older store may hold.
    store.boards[0].tasks[0].lane = KanbanLane::Todo;
    save_store_to_path(&store, &fx.path).unwrap();
    assert_refused(
        as_worker(&fx, Tool::Complete, json!({"summary": "x"})),
        "not doing",
    );
}

#[test]
fn an_orchestrator_may_finish_any_task_but_not_twice() {
    let fx = fixture();
    let reply = as_orchestrator(
        &fx,
        Tool::Complete,
        json!({"task_id": "K-0003", "summary": "x"}),
    );
    assert_eq!(reply.unwrap()["lane"], "done");
    assert_refused(
        as_orchestrator(
            &fx,
            Tool::Block,
            json!({"task_id": "K-0003", "reason": "x"}),
        ),
        "already done",
    );
}

#[test]
fn unblock_is_for_orchestrators_and_only_for_blocked_tasks() {
    let fx = fixture();
    assert_refused(
        as_orchestrator(&fx, Tool::Unblock, json!({"task_id": "K-0002"})),
        "not blocked",
    );
    as_orchestrator(
        &fx,
        Tool::Block,
        json!({"task_id": "K-0002", "reason": "x"}),
    )
    .unwrap();
    assert_refused(
        as_worker(&fx, Tool::Unblock, json!({"task_id": "K-0002"})),
        "for orchestrators",
    );
    let reply = as_orchestrator(&fx, Tool::Unblock, json!({"task_id": "K-0002"})).unwrap();
    assert_eq!(reply["lane"], "todo");
    assert_eq!(task(&fx, "main", "K-0002").blocked_reason, None);
}

// ---- boards ----

#[test]
fn a_worker_cannot_reach_another_board() {
    let fx = fixture();
    let before = std::fs::read(&fx.path).unwrap();
    assert_refused(
        as_worker(
            &fx,
            Tool::Show,
            json!({"task_id": "K-0001", "board": "side"}),
        ),
        "bound to board `main`; refusing board `side`",
    );
    assert_refused(
        as_worker(&fx, Tool::Create, json!({"title": "x", "board": "Side"})),
        "refusing board `Side`",
    );
    assert_refused(
        as_worker(&fx, Tool::Show, json!({"board": "nowhere"})),
        "refusing board `nowhere`",
    );
    assert_eq!(std::fs::read(&fx.path).unwrap(), before);
    // Its own board by name is its own board.
    assert!(as_worker(&fx, Tool::Show, json!({"board": "MAIN"})).is_ok());
}

#[test]
fn a_worker_whose_board_is_gone_is_refused() {
    let fx = fixture();
    let mut binding = worker("run-a").unwrap();
    binding.board = "deleted".to_string();
    assert_refused(
        call(&fx, Some(binding), Some(0), Tool::Show, json!({})),
        "board `deleted` is not in the kanban store",
    );
}

#[test]
fn an_unknown_board_is_an_error_and_tools_leave_the_current_board_alone() {
    let fx = fixture();
    assert_refused(
        as_orchestrator(
            &fx,
            Tool::Show,
            json!({"task_id": "K-0001", "board": "nowhere"}),
        ),
        "board `nowhere` not found",
    );
    as_orchestrator(
        &fx,
        Tool::Create,
        json!({"title": "on side", "board": "side"}),
    )
    .unwrap();
    assert_eq!(store(&fx).current_board_id, "main");
    assert_eq!(task(&fx, "side", "K-0002").title, "on side");
}

// ---- create and link ----

#[test]
fn create_adds_a_task_to_the_workers_board() {
    let fx = fixture();
    let reply = as_worker(
        &fx,
        Tool::Create,
        json!({"title": "follow up", "priority": 2, "depends_on": ["k-0001"], "description": "tests"}),
    )
    .unwrap();
    assert_eq!(
        reply,
        json!({"ok": true, "task_id": "K-0004", "lane": "todo", "board": "main"})
    );
    let created = task(&fx, "main", "K-0004");
    assert_eq!(created.priority, 2);
    assert_eq!(created.depends_on, ["K-0001"]);
    assert_eq!(created.description.as_deref(), Some("tests"));
    let triage = as_worker(
        &fx,
        Tool::Create,
        json!({"title": "idea", "lane": "triage"}),
    )
    .unwrap();
    assert_eq!(triage["lane"], "triage");
}

#[test]
fn create_refuses_bad_input_without_a_trace() {
    let fx = fixture();
    let before = std::fs::read(&fx.path).unwrap();
    for (args, needle) in [
        (json!({"title": " "}), "title is required"),
        (json!({"title": "x", "lane": "doing"}), "triage or todo"),
        (
            json!({"title": "x", "priority": 9}),
            "priority must be 1 to 5",
        ),
        (
            json!({"title": "x", "depends_on": ["K-0099"]}),
            "task K-0099 not found",
        ),
        (json!({"title": "x", "depends_on": 3}), "list of strings"),
        (json!({"title": "x", "goal_max_turns": 0}), "goal_max_turns"),
    ] {
        assert_refused(as_worker(&fx, Tool::Create, args), needle);
    }
    assert_eq!(std::fs::read(&fx.path).unwrap(), before);
}

#[test]
fn link_adds_a_dependency_once() {
    let fx = fixture();
    let link = || {
        as_orchestrator(
            &fx,
            Tool::Link,
            json!({"parent_id": "K-0001", "child_id": "K-0002"}),
        )
    };
    assert_eq!(link().unwrap()["child_id"], "K-0002");
    link().unwrap();
    assert_eq!(task(&fx, "main", "K-0002").depends_on, ["K-0001"]);
}

#[test]
fn link_refuses_self_links_and_cycles() {
    let fx = fixture();
    // No task is running, so only the cycle check can refuse.
    let mut store = store(&fx);
    for idx in [0, 2] {
        crate::kanban::move_task(&mut store.boards[0].tasks[idx], KanbanLane::Todo, None);
    }
    save_store_to_path(&store, &fx.path).unwrap();
    assert_refused(
        as_orchestrator(
            &fx,
            Tool::Link,
            json!({"parent_id": "K-0002", "child_id": "k-0002"}),
        ),
        "cannot depend on itself",
    );
    as_orchestrator(
        &fx,
        Tool::Link,
        json!({"parent_id": "K-0001", "child_id": "K-0002"}),
    )
    .unwrap();
    as_orchestrator(
        &fx,
        Tool::Link,
        json!({"parent_id": "K-0002", "child_id": "K-0003"}),
    )
    .unwrap();
    assert_refused(
        as_orchestrator(
            &fx,
            Tool::Link,
            json!({"parent_id": "K-0003", "child_id": "K-0001"}),
        ),
        "would form a cycle",
    );
    assert!(task(&fx, "main", "K-0001").depends_on.is_empty());
}

#[test]
fn a_worker_may_link_its_own_running_task_but_not_anothers() {
    let fx = fixture();
    assert_refused(
        as_worker(
            &fx,
            Tool::Link,
            json!({"parent_id": "K-0002", "child_id": "K-0003"}),
        ),
        "only its own run",
    );
    as_worker(
        &fx,
        Tool::Link,
        json!({"parent_id": "K-0002", "child_id": "K-0001"}),
    )
    .unwrap();
    assert_eq!(task(&fx, "main", "K-0001").depends_on, ["K-0002"]);
    as_worker(
        &fx,
        Tool::Link,
        json!({"parent_id": "K-0001", "child_id": "K-0003"}),
    )
    .expect_err("K-0003 still runs elsewhere");
}

#[test]
fn attachments_lists_the_tasks_files() {
    let fx = fixture();
    let reply = as_worker(&fx, Tool::Attachments, json!({})).unwrap();
    assert_eq!(
        reply,
        json!({"ok": true, "task_id": "K-0001", "attachments": []})
    );
    assert_refused(
        as_orchestrator(&fx, Tool::Attachments, json!({})),
        "kanban_list",
    );
}

// ---- review delta: dependencies, redaction, live runs, aliases, caps ----

/// K-0001 (doing, run-a) now waits for K-0002 (todo).
fn fixture_with_open_parent() -> Fixture {
    let fx = fixture();
    let mut store = store(&fx);
    store.boards[0].tasks[0].depends_on = vec!["K-0002".to_string()];
    save_store_to_path(&store, &fx.path).unwrap();
    fx
}

// Port of upstream test_kanban_complete_parents_tool.py::test_complete_names_unsatisfied_parent.
#[test]
fn complete_names_an_unsatisfied_parent() {
    let fx = fixture_with_open_parent();
    let before = std::fs::read(&fx.path).unwrap();
    let error = as_worker(
        &fx,
        Tool::Complete,
        json!({"summary": "genuinely done work"}),
    )
    .expect_err("refused");
    assert!(error.contains("K-0002"), "{error}");
    assert!(error.contains("parent"), "{error}");
    assert!(!error.contains("no longer owned"), "{error}");
    assert_eq!(std::fs::read(&fx.path).unwrap(), before);
    assert_eq!(task(&fx, "main", "K-0001").lane, KanbanLane::Doing);
    let shown = as_worker(&fx, Tool::Show, json!({})).unwrap();
    assert_eq!(
        shown["task"]["unsatisfied_parents"],
        json!([{"id": "K-0002", "status": "todo"}])
    );
    // An orchestrator is held to the same rule.
    assert_refused(
        as_orchestrator(
            &fx,
            Tool::Complete,
            json!({"task_id": "K-0001", "summary": "x"}),
        ),
        "unfinished parent(s) K-0002 (todo)",
    );
    // A done or archived parent no longer holds the task.
    as_orchestrator(
        &fx,
        Tool::Complete,
        json!({"task_id": "K-0002", "summary": "ok"}),
    )
    .unwrap();
    let mut store = store(&fx);
    crate::kanban::archive_done(&mut store.boards[0]);
    save_store_to_path(&store, &fx.path).unwrap();
    assert_eq!(
        as_worker(&fx, Tool::Complete, json!({"summary": "x"})).unwrap()["lane"],
        "done"
    );
}

#[test]
fn stored_text_is_redacted_before_it_is_saved() {
    let fx = fixture();
    // Built at run time so no credential-shaped literal sits in the source.
    let secret = format!("sk-{}", "a1".repeat(12));
    as_worker(
        &fx,
        Tool::Create,
        json!({"title": format!("rotate {secret}"), "description": format!("key {secret}")}),
    )
    .unwrap();
    as_orchestrator(
        &fx,
        Tool::Block,
        json!({"task_id": "K-0002", "reason": format!("needs {secret}")}),
    )
    .unwrap();
    as_worker(
        &fx,
        Tool::Complete,
        json!({"summary": format!("used {secret}"), "metadata": {"key": secret}}),
    )
    .unwrap();
    let saved = std::fs::read_to_string(&fx.path).unwrap();
    assert!(!saved.contains(&secret), "{saved}");
    // The text around the secret is kept.
    for kept in ["rotate ", "key ", "needs ", "used "] {
        assert!(saved.contains(kept), "{kept}: {saved}");
    }
}

#[test]
fn an_orchestrator_cannot_finish_or_block_a_live_run() {
    let fx = fixture();
    let before = std::fs::read(&fx.path).unwrap();
    for status in ["queued", "running"] {
        fx.job_status("run-z", status);
        assert_refused(
            as_orchestrator(
                &fx,
                Tool::Complete,
                json!({"task_id": "K-0003", "summary": "x"}),
            ),
            "live run run-z",
        );
    }
    // A status file that cannot be read counts as live.
    std::fs::write(fx.jobs.join("run-z.json"), "{not json").unwrap();
    assert_refused(
        as_orchestrator(
            &fx,
            Tool::Block,
            json!({"task_id": "K-0003", "reason": "x"}),
        ),
        "live run",
    );
    assert_eq!(std::fs::read(&fx.path).unwrap(), before);
    fx.job_status("run-z", "failed");
    assert_eq!(
        as_orchestrator(
            &fx,
            Tool::Complete,
            json!({"task_id": "K-0003", "summary": "x"})
        )
        .unwrap()["lane"],
        "done"
    );
}

#[test]
fn a_run_id_that_is_not_a_plain_job_id_counts_as_live() {
    let fx = fixture();
    assert!(run_is_live(&fx.jobs, "../boards"));
    assert!(run_is_live(&fx.jobs, "run z"));
    assert!(!run_is_live(&fx.jobs, "run-gone"));
}

#[test]
fn an_orchestrator_cannot_link_a_running_child() {
    let fx = fixture();
    assert_refused(
        as_orchestrator(
            &fx,
            Tool::Link,
            json!({"parent_id": "K-0002", "child_id": "K-0003"}),
        ),
        "only its own run",
    );
    assert!(task(&fx, "main", "K-0003").depends_on.is_empty());
}

#[test]
fn block_takes_only_todo_or_doing_tasks() {
    let fx = fixture();
    as_orchestrator(
        &fx,
        Tool::Block,
        json!({"task_id": "K-0002", "reason": "first"}),
    )
    .unwrap();
    assert_refused(
        as_orchestrator(
            &fx,
            Tool::Block,
            json!({"task_id": "K-0002", "reason": "second"}),
        ),
        "only a todo or doing task",
    );
    assert_eq!(
        task(&fx, "main", "K-0002").blocked_reason.as_deref(),
        Some("first")
    );
    as_orchestrator(
        &fx,
        Tool::Create,
        json!({"title": "idea", "lane": "triage"}),
    )
    .unwrap();
    assert_refused(
        as_orchestrator(
            &fx,
            Tool::Block,
            json!({"task_id": "K-0004", "reason": "x"}),
        ),
        "is in triage",
    );
}

#[test]
fn the_shipped_skills_argument_names_work() {
    let fx = fixture();
    let ready = as_orchestrator(&fx, Tool::List, json!({"status": "ready"})).unwrap();
    assert_eq!(ready["tasks"][0]["id"], "K-0002");
    let running = as_orchestrator(&fx, Tool::List, json!({"status": "running"})).unwrap();
    assert_eq!(running["total"], 2);
    assert_refused(
        as_orchestrator(&fx, Tool::List, json!({"status": "todo", "lane": "todo"})),
        "not both",
    );
    let created = as_worker(
        &fx,
        Tool::Create,
        json!({"title": "synthesise", "assignee": "analyst", "body": "read T1", "parents": ["K-0002"]}),
    )
    .unwrap();
    assert_eq!(created["task_id"], "K-0004");
    let card = task(&fx, "main", "K-0004");
    assert_eq!(card.description.as_deref(), Some("read T1"));
    assert_eq!(card.depends_on, ["K-0002"]);
    assert_refused(
        as_worker(
            &fx,
            Tool::Create,
            json!({"title": "x", "body": "a", "description": "b"}),
        ),
        "not both",
    );
    let reply = as_worker(
        &fx,
        Tool::Complete,
        json!({
            "summary": "shipped",
            "metadata": {"tests_run": 14},
            "artifacts": ["/tmp/q3.png"],
            "created_cards": ["k-0004"]
        }),
    )
    .unwrap();
    assert_eq!(reply["lane"], "done");
    assert_eq!(
        task(&fx, "main", "K-0001").run_summary.as_deref(),
        Some(
            "shipped\nArtifacts: /tmp/q3.png\nCreated cards: K-0004\nMetadata: {\"tests_run\":14}"
        )
    );
}

#[test]
fn complete_refuses_a_created_card_that_does_not_exist() {
    let fx = fixture();
    let before = std::fs::read(&fx.path).unwrap();
    assert_refused(
        as_worker(
            &fx,
            Tool::Complete,
            json!({"summary": "x", "created_cards": ["K-0099"]}),
        ),
        "created_cards names K-0099",
    );
    assert_refused(
        as_worker(
            &fx,
            Tool::Complete,
            json!({"summary": "x", "metadata": "loose"}),
        ),
        "metadata must be a JSON object",
    );
    assert_eq!(std::fs::read(&fx.path).unwrap(), before);
}

#[test]
fn oversized_input_is_refused_without_a_trace() {
    let fx = fixture();
    let before = std::fs::read(&fx.path).unwrap();
    let many: Vec<String> = (0..51).map(|_| "K-0002".to_string()).collect();
    for (tool, args, needle) in [
        (
            Tool::Create,
            json!({"title": "t".repeat(201)}),
            "title is 201 characters",
        ),
        (
            Tool::Create,
            json!({"title": "x", "description": "d".repeat(8 * 1024 + 1)}),
            "description is 8193 bytes",
        ),
        (
            Tool::Create,
            json!({"title": "x", "assignee": "a".repeat(101)}),
            "assignee is 101",
        ),
        (
            Tool::Create,
            json!({"title": "x", "depends_on": many}),
            "depends_on has 51 items",
        ),
        (
            Tool::Block,
            json!({"reason": "r".repeat(8 * 1024 + 1)}),
            "reason is 8193 bytes",
        ),
        (
            Tool::Complete,
            json!({"summary": "x", "metadata": {"m": "m".repeat(8 * 1024)}}),
            "metadata is",
        ),
    ] {
        assert_refused(as_worker(&fx, tool, args), needle);
    }
    assert_refused(
        as_orchestrator(&fx, Tool::List, json!({"limit": 201})),
        "limit must be 1 to 200, not 201",
    );
    assert_eq!(std::fs::read(&fx.path).unwrap(), before);
}

// ---- the human CLI's lifecycle checks ----

#[test]
fn the_cli_holds_a_bound_process_to_its_own_task_and_others_to_live_runs() {
    let fx = fixture();
    let store = store(&fx);
    let board = &store.boards[0];
    let (own, other) = (&board.tasks[0], &board.tasks[2]);
    let bound = worker("run-a");
    let check = |task: &KanbanTask, force: bool, binding: Option<&KanbanWorkerBinding>| {
        check_cli_lifecycle_with(board, task, "done", true, force, binding, &fx.jobs)
    };
    assert!(check(own, false, bound.as_ref()).is_ok());
    let foreign = check(other, false, bound.as_ref()).unwrap_err();
    assert!(foreign.contains("bound to task K-0001"), "{foreign}");
    let forced = check(own, true, bound.as_ref()).unwrap_err();
    assert!(forced.contains("--force is for operators"), "{forced}");
    // No binding: run-z has no status file, so it is not live.
    assert!(check(other, false, None).is_ok());
    fx.job_status("run-z", "running");
    let live = check(other, false, None).unwrap_err();
    assert!(
        live.contains("a live worker is running it (background job run-z)"),
        "{live}"
    );
    assert!(check(other, true, None).is_ok());
}

#[test]
fn the_cli_refuses_open_parents_even_with_force() {
    let fx = fixture_with_open_parent();
    let store = store(&fx);
    let board = &store.boards[0];
    let error =
        check_cli_lifecycle_with(board, &board.tasks[0], "done", true, true, None, &fx.jobs)
            .unwrap_err();
    assert!(
        error.contains("unfinished parent(s) K-0002 (todo)"),
        "{error}"
    );
    assert!(check_cli_lifecycle_with(
        board,
        &board.tasks[0],
        "block",
        false,
        false,
        None,
        &fx.jobs
    )
    .is_ok());
}

#[test]
fn a_completion_record_has_an_overall_cap() {
    let fx = fixture();
    let artifacts: Vec<String> = (0..3)
        .map(|i| format!("/{i}{}", "p".repeat(8000)))
        .collect();
    assert_refused(
        as_worker(
            &fx,
            Tool::Complete,
            json!({"summary": "x", "artifacts": artifacts}),
        ),
        "the completion record",
    );
}

#[test]
fn a_toolset_list_may_name_kanban_in_its_tools_form() {
    for name in ["kanban", "KANBAN", "kanban_tools", "kanban-tools"] {
        let mut config = hermes_config::GatewayConfig::default();
        config
            .platform_toolsets
            .insert("cli".to_string(), vec![name.to_string()]);
        assert!(kanban_toolset_named(&config, "cli"), "{name}");
    }
    let mut config = hermes_config::GatewayConfig::default();
    config
        .platform_toolsets
        .insert("cli".to_string(), vec!["kanbanish".to_string()]);
    assert!(!kanban_toolset_named(&config, "cli"));
}
