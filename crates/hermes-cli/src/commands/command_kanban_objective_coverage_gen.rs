use super::*;

fn isolated_test_root(test_name: &str, vars: &[(&str, &str)]) -> Option<PathBuf> {
    const CHILD_ROOT: &str = "HERMES_KANBAN_OBJECTIVE_COVERAGE_CHILD";
    if let Some(path) = std::env::var_os(CHILD_ROOT) {
        return Some(PathBuf::from(path));
    }

    let root = tempfile::tempdir().expect("create isolated test directory");
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable")); // nosemgrep: rust.lang.security.current-exe.current-exe
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if ["HERMES_", "CONTEXTLATTICE_", "MEMMCP_", "XDG_"]
            .iter()
            .any(|prefix| name.starts_with(*prefix))
        {
            command.env_remove(key);
        }
    }
    let output = command
        .arg("--exact")
        .arg(format!("{module}::{test_name}"))
        .arg("--test-threads=1")
        .current_dir(root.path())
        .env(CHILD_ROOT, root.path())
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("HERMES_HOME", root.path())
        .env("APPDATA", root.path())
        .env("LOCALAPPDATA", root.path())
        .env("XDG_CONFIG_HOME", root.path())
        .env("XDG_DATA_HOME", root.path())
        .env("XDG_STATE_HOME", root.path())
        .env("XDG_CACHE_HOME", root.path())
        .envs(vars.iter().copied())
        .output()
        .expect("run test in isolated process");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "isolated test {test_name} did not pass:\n{stdout}\n{stderr}"
    );
    None
}

struct RestoreEnvironment(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl RestoreEnvironment {
    fn capture(keys: &[&'static str]) -> Self {
        Self(
            keys.iter()
                .map(|&key| (key, std::env::var_os(key)))
                .collect(),
        )
    }
}

impl Drop for RestoreEnvironment {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

#[test]
fn kanban_add_parses_flags_and_defaults() {
    if run_isolated("kanban_add_parses_flags_and_defaults") {
        return;
    }

    let input = parse_kanban_add(&[
        "  Compare",
        "-p",
        "1",
        "reports  ",
        "--lane",
        "doing",
        "-a",
        "Morgan",
        "--tenant",
        "  reports-team  ",
        "--deps",
        " K-0001, ,K-0003, ",
        "--description",
        "Compare totals",
        "--goal-mode",
        "--goal-max-turns",
        "9",
    ])
    .expect("parse fully specified task");
    assert_eq!(input.title, "Compare reports");
    assert_eq!(input.lane.as_str(), "doing");
    assert_eq!(input.priority, 1);
    assert_eq!(input.assignee.as_deref(), Some("Morgan"));
    assert_eq!(input.tenant.as_deref(), Some("reports-team"));
    assert_eq!(input.depends_on, ["K-0001", "K-0003"]);
    assert_eq!(input.description.as_deref(), Some("Compare totals"));
    assert!(input.goal_mode);
    assert_eq!(input.goal_max_turns, Some(9));

    let input = parse_kanban_add(&["Draft", "notes"]).expect("parse bare title");
    assert_eq!(input.title, "Draft notes");
    assert_eq!(input.lane.as_str(), "todo");
    assert_eq!(input.priority, 3);
    assert!(input.assignee.is_none());
    assert!(input.tenant.is_none());
    assert!(input.depends_on.is_empty());
    assert!(input.description.is_none());
    assert!(!input.goal_mode);
    assert!(input.goal_max_turns.is_none());
}

#[test]
fn kanban_add_reports_invalid_and_missing_arguments() {
    if run_isolated("kanban_add_reports_invalid_and_missing_arguments") {
        return;
    }

    let cases: &[(&[&str], &str)] = &[
        (&[], "Usage: /kanban add <title> [--lane <triage|todo|doing|blocked|done>] [--priority <1..5>] [--assignee <name>] [--tenant <name>] [--depends K-0001,K-0002] [--desc <text>] [--goal] [--goal-max-turns N]"),
        (&["Notes", "--lane"], "Missing value for --lane"),
        (&["Notes", "--lane", "later"], "Invalid lane `later`. Use: triage|todo|doing|blocked|done."),
        (&["Notes", "-p"], "Missing value for --priority"),
        (&["Notes", "--priority", "urgent"], "Invalid priority `urgent`. Expected integer 1..5."),
        (&["Notes", "--priority", "6"], "Invalid priority `6`. Expected 1..5."),
        (&["Notes", "--tenant"], "Missing value for --tenant"),
        (&["Notes", "--tenant", "  "], "Missing value for --tenant"),
        (&["Notes", "--tenant", "--goal"], "Missing value for --tenant"),
        (&["Notes", "--goal-max-turns"], "Missing value for --goal-max-turns"),
        (&["Notes", "--goal-max-turns", "-1"], "Invalid goal max turns `-1`. Expected positive integer."),
        (&["Notes", "--goal-max-turns", "0"], "Invalid goal max turns `0`. Expected positive integer."),
        (&["  ", "--goal"], "Usage: /kanban add <title> [flags...]"),
    ];
    for &(args, expected) in cases {
        match parse_kanban_add(args) {
            Err(AgentError::Config(message)) => assert_eq!(message, expected, "{args:?}"),
            Err(error) => panic!("unexpected error for {args:?}: {error}"),
            Ok(_) => panic!("invalid arguments accepted: {args:?}"),
        }
    }
}

#[test]
fn kanban_status_sorts_active_tasks_and_limits_display() {
    if run_isolated("kanban_status_sorts_active_tasks_and_limits_display") {
        return;
    }

    if isolated_test_root("kanban_status_sorts_active_tasks_and_limits_display", &[]).is_none() {
        return;
    }
    let lock = lock_store().expect("lock isolated store");
    let mut store = lock.load().expect("load isolated store");
    let board = create_or_select_board(&mut store, "Coverage", None);

    let archived = add_task(board, parse_kanban_add(&["Old result"]).unwrap());
    find_task_mut(board, &archived.id).unwrap().lane = KanbanLane::Done;
    assert_eq!(archive_done(board), 1);

    for (id, priority, title) in [
        ("Z-doing", 1, "Second"),
        ("0-doing", 5, "Third"),
        ("A-doing", 1, "First"),
    ] {
        let added = add_task(
            board,
            parse_kanban_add(&[title, "--lane", "doing"]).unwrap(),
        );
        let task = find_task_mut(board, &added.id).unwrap();
        task.id = id.to_owned();
        task.priority = priority;
        task.assignee = None;
    }
    let added = add_task(board, parse_kanban_add(&["Reconcile"]).unwrap());
    let blocked = find_task_mut(board, &added.id).unwrap();
    blocked.id = "B-blocked".to_owned();
    blocked.lane = KanbanLane::Blocked;
    blocked.assignee = Some("Morgan".to_owned());
    blocked.blocked_reason = Some("awaiting totals".to_owned());
    blocked.background_job_id = Some("JOB-example".to_owned());
    for index in 0..18 {
        let title = format!("Filler {index:02}");
        let added = add_task(board, parse_kanban_add(&[&title]).unwrap());
        find_task_mut(board, &added.id).unwrap().id = format!("F-{index:02}");
    }

    board.name = " Coverage ".to_owned();
    board.id = " coverage ".to_owned();
    board.project_path = Some("/coverage/project".to_owned());

    let status = render_kanban_status(board);
    let (header, active) = status.split_once("\n\nActive tasks (top 20):\n").unwrap();
    assert!(header.starts_with("Kanban board: Coverage (coverage)\nProject: /coverage/project\n"));
    for expected in [
        "Tasks: 22 (archived done: 1)",
        "  blocked: 1",
        "    doing: 3",
        "     todo: 18",
        "     done: 0",
        "   triage: 0",
    ] {
        assert!(header.lines().any(|line| line == expected), "{status}");
    }
    let rows: Vec<_> = active.lines().collect();
    assert_eq!(rows.len(), 20);
    assert_eq!(
        &rows[..4],
        [
            "- B-blocked [blocked] p3 @Morgan Reconcile blocked=awaiting totals job=JOB-example",
            "- A-doing [doing] p1 @- First",
            "- Z-doing [doing] p1 @- Second",
            "- 0-doing [doing] p5 @- Third",
        ]
    );
    assert!(rows[19].ends_with("Filler 15"), "{status}");
    assert!(!status.contains("Old result"));
    assert!(!status.ends_with('\n'));
}

#[test]
fn kanban_commands_select_boards_and_list_saved_attachments() {
    if run_isolated("kanban_commands_select_boards_and_list_saved_attachments") {
        return;
    }

    let Some(root) = isolated_test_root(
        "kanban_commands_select_boards_and_list_saved_attachments",
        &[],
    ) else {
        return;
    };
    let lock = lock_store().expect("lock isolated store");
    let mut store = lock.load().expect("load isolated store");
    let project = root.canonicalize().unwrap();
    let project = project.to_str().unwrap();
    let empty_board =
        create_or_select_board(&mut store, "Coverage Empty", Some(project.to_owned()));
    empty_board.project_path = Some(project.to_owned());
    let empty_id = empty_board.id.clone();
    let board = create_or_select_board(&mut store, "Coverage Notes", None);
    board.project_path = None;
    let notes_id = board.id.clone();
    let task_id = add_task(board, parse_kanban_add(&["Review notes"]).unwrap())
        .id
        .clone();
    let clean_id = add_task(board, parse_kanban_add(&["Clean desk"]).unwrap())
        .id
        .clone();

    let note = root.join("summary.txt");
    std::fs::write(&note, b"note").unwrap();
    let attachment = add_attachment_to_task(board, &task_id, note.to_str().unwrap(), None)
        .expect("store a local attachment");
    lock.save(&store).expect("save board fixtures");

    let initial = run_kanban_command(&[]).unwrap();
    assert!(initial.starts_with(&format!("Kanban board: Coverage Notes ({notes_id})\n")));
    assert!(initial.contains("Review notes"));
    assert_eq!(
        run_kanban_command(&["attachments", "review NOTES"]).unwrap(),
        format!(
            "Attachments for {task_id}:\n- {} summary.txt size=4 path={}",
            attachment.id, attachment.stored_path
        )
    );
    assert_eq!(
        run_kanban_command(&["attachments", &clean_id.to_ascii_lowercase()]).unwrap(),
        format!("No attachments for {clean_id}.")
    );
    assert_eq!(
        run_kanban_command(&["remove-attachment", &task_id, "absent.txt"]).unwrap(),
        format!("Attachment not found: task={task_id} attachment=absent.txt")
    );

    assert_eq!(
        run_kanban_command(&["SELECT", &empty_id]).unwrap(),
        format!("Using board: Coverage Empty ({empty_id})")
    );
    let boards = run_kanban_command(&["list"]).unwrap();
    assert!(boards.starts_with("Kanban boards:\n"));
    let selected = format!("* Coverage Empty ({empty_id}) project={project}");
    let unselected = format!("  Coverage Notes ({notes_id}) project=-");
    assert!(boards.lines().any(|line| line == selected), "{boards}");
    assert!(boards.lines().any(|line| line == unselected), "{boards}");

    let empty = run_kanban_command(&["show"]).unwrap();
    assert!(empty.starts_with(&format!("Kanban board: Coverage Empty ({empty_id})\n")));
    assert!(empty.contains("Tasks: 0 (archived done: 0)"));
    assert!(empty.ends_with("No active tasks. Use `/kanban add <title>`."));
    let other = run_kanban_command(&["status", &notes_id]).unwrap();
    assert!(other.contains("Clean desk"));
    assert_eq!(run_kanban_command(&["status"]).unwrap(), empty);
}

#[test]
fn kanban_command_validation_leaves_the_board_unchanged() {
    if run_isolated("kanban_command_validation_leaves_the_board_unchanged") {
        return;
    }

    if isolated_test_root("kanban_command_validation_leaves_the_board_unchanged", &[]).is_none() {
        return;
    }
    let lock = lock_store().expect("lock isolated store");
    let mut store = lock.load().expect("load isolated store");
    let board = create_or_select_board(&mut store, "Coverage Validation", None);
    add_task(
        board,
        parse_kanban_add(&["Unsorted", "--lane", "triage"]).unwrap(),
    );
    lock.save(&store).expect("save triage fixture");

    let before = run_kanban_command(&[]).unwrap();
    let move_usage =
        "Usage: kanban move <task-id|title> <triage|todo|doing|blocked|done> [summary] [--force]";
    let block_usage = "Usage: kanban block <task-id|title> <reason> [--force]";
    let attach_usage = "Usage: kanban attach <task-id|title> <file-path>";
    let detach_usage = "Usage: kanban detach <task-id|title> <attachment-id|filename>";
    let cases: &[(&[&str], &str)] = &[
        (&["use"], "Usage: kanban use <board-id-or-name>"),
        (&["attach"], attach_usage),
        (&["attach", "Unsorted"], attach_usage),
        (&["detach"], detach_usage),
        (&["detach", "Unsorted"], detach_usage),
        (
            &["attachments"],
            "Usage: kanban attachments <task-id|title>",
        ),
        (&["attachments", "missing"], "Task not found: missing"),
        (&["move"], move_usage),
        (&["move", "Unsorted"], move_usage),
        (
            &["move", "Unsorted", "later"],
            "Invalid lane `later`. Use: triage|todo|doing|blocked|done.",
        ),
        (&["move", "missing", "done"], "Task not found: missing"),
        (&["claim"], "Usage: kanban claim <task-id|title> [assignee]"),
        (&["claim", "missing", "Morgan"], "Task not found: missing"),
        (&["block"], block_usage),
        (&["block", "Unsorted", "  "], block_usage),
        (
            &["block", "missing", "waiting", "for totals"],
            "Task not found: missing",
        ),
        (&["done"], "Usage: kanban done <task-id|title> [summary] [--force]"),
        (&["done", "missing", "resolved"], "Task not found: missing"),
        (
            &["dispatch"],
            "Usage: kanban dispatch <task-id|title> [background-task-override]",
        ),
        (&["dispatch", "missing"], "Task not found: missing"),
        (
            &["dispatch", "Unsorted"],
            "Specify or move the triage task before dispatching it.",
        ),
        (
            &["unrecognized"],
            "Unknown kanban action. Use `hermes kanban help`.",
        ),
    ];
    for &(args, expected) in cases {
        assert_eq!(run_kanban_command(args).unwrap(), expected, "{args:?}");
    }
    assert!(run_kanban_command(&["init"])
        .unwrap()
        .starts_with("Usage: kanban init <board-name>"));
    let help = run_kanban_command(&["help"]).unwrap();
    assert!(help.starts_with("Kanban commands:\n"));
    assert!(help.contains("hermes kanban attachments <task-id|title>"));
    assert!(help.ends_with("Interactive alias: /kanban <command>"));
    assert_eq!(run_kanban_command(&[]).unwrap(), before);
}

#[tokio::test(flavor = "current_thread")]
async fn kanban_dispatch_persists_the_bound_job_and_sync_respects_opt_out() {
    let Some(home) = isolated_test_root(
        "kanban_dispatch_persists_the_bound_job_and_sync_respects_opt_out",
        &[("HERMES_KANBAN_CONTEXTLATTICE_SYNC", "0")],
    ) else {
        return;
    };

    run_kanban_command(&["init", "Varrock archive"]).unwrap();
    // Check the durable queue on the current thread before its worker can start.
    for (title, override_text) in [
        ("Catalogue notes", ""),
        ("Review notes", "Summarise the supplied notes"),
    ] {
        run_kanban_command(&["add", title, "--lane", "todo", "--desc", "Local fixture"]).unwrap();
        let output = run_kanban_command(&["dispatch", title, override_text]).unwrap();
        let store = lock_store().unwrap().load().unwrap();
        let board = store
            .boards
            .iter()
            .find(|board| board.id == store.current_board_id)
            .unwrap();
        let task = board.tasks.iter().find(|task| task.title == title).unwrap();
        assert_eq!(task.lane, KanbanLane::Doing);
        let run = task.background_job_id.as_deref().unwrap();
        assert!(output.starts_with(&format!("Dispatched {} as background job {run}", task.id)));
        let path = home.join("background_jobs").join(format!("{run}.json"));
        let job: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(job["id"], run);
        assert_eq!(job["status"], "queued");
        assert_eq!(job["kanban"], serde_json::json!({ "board": board.id, "task": task.id, "run": run }));
        let message = job["task"].as_str().unwrap();
        if override_text.is_empty() {
            assert!(message.contains(title) && message.contains("Local fixture"));
        } else {
            assert_eq!(message, override_text);
        }
    }
    let before = run_kanban_command(&["status"]).unwrap();
    assert_eq!(
        run_kanban_command(&["sync"]).unwrap(),
        "ContextLattice sync disabled via HERMES_KANBAN_CONTEXTLATTICE_SYNC."
    );
    assert_eq!(run_kanban_command(&["status"]).unwrap(), before);
}

include!("command_kanban_objective_part2_coverage_gen.rs");
