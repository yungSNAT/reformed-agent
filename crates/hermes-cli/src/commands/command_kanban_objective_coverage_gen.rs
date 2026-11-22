use super::*;

fn isolated_test_root(test_name: &str, vars: &[(&str, &str)]) -> Option<PathBuf> {
    const CHILD_ROOT: &str = "HERMES_KANBAN_OBJECTIVE_COVERAGE_CHILD";
    if let Some(path) = std::env::var_os(CHILD_ROOT) {
        return Some(PathBuf::from(path));
    }

    let root = tempfile::tempdir().expect("create isolated test directory");
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
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
        "Usage: kanban move <task-id|title> <triage|todo|doing|blocked|done> [summary]";
    let block_usage = "Usage: kanban block <task-id|title> <reason>";
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
        (&["done"], "Usage: kanban done <task-id|title> [summary]"),
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

#[test]
fn planner_modes_parse_aliases_and_emit_canonical_names() {
    if run_isolated("planner_modes_parse_aliases_and_emit_canonical_names") {
        return;
    }

    for (raw, mode, canonical) in [
        (" DISABLED ", PlanCapabilityMode::Off, "off"),
        ("0", PlanCapabilityMode::Off, "off"),
        (" WARN ", PlanCapabilityMode::Advisory, "advisory"),
        ("1", PlanCapabilityMode::Advisory, "advisory"),
        (" STRICT ", PlanCapabilityMode::Enforce, "enforce"),
    ] {
        assert_eq!(PlanCapabilityMode::parse(raw), Some(mode));
        assert_eq!(mode.as_str(), canonical);
    }
    assert_eq!(PlanCapabilityMode::parse("mandatory"), None);

    for (raw, profile, canonical) in [
        (" FAST ", TaskDepthProfile::Shallow, "shallow"),
        ("default", TaskDepthProfile::Balanced, "balanced"),
        (" THOROUGH ", TaskDepthProfile::Deep, "deep"),
        ("exhaustive", TaskDepthProfile::Max, "max"),
    ] {
        assert_eq!(TaskDepthProfile::parse(raw), Some(profile));
        assert_eq!(profile.as_str(), canonical);
    }
    assert_eq!(TaskDepthProfile::parse("medium"), None);
}

#[test]
fn planner_infers_requirements_independently() {
    if run_isolated("planner_infers_requirements_independently") {
        return;
    }

    for (task, expected) in [
        ("Summarize the meeting", [false, false, false, false]),
        ("PATCH the typo", [true, false, false, false]),
        ("Perform an AUDIT", [false, true, false, false]),
        ("Work end-to-end", [false, false, true, false]),
        ("Summarize the SCREENSHOT", [false, false, false, true]),
        (
            "AUDIT the ENTIRE REPO and check the DIAGRAM",
            [true, true, true, true],
        ),
    ] {
        let req = infer_plan_requirements(task);
        assert_eq!(
            [
                req.require_tools,
                req.require_reasoning,
                req.require_long_context,
                req.require_vision
            ],
            expected,
            "{task}"
        );
        assert_eq!(
            req.is_empty(),
            !expected.into_iter().any(|required| required)
        );
    }
}

#[test]
fn task_depth_profiles_apply_runtime_budgets() {
    if run_isolated("task_depth_profiles_apply_runtime_budgets") {
        return;
    }

    if isolated_test_root("task_depth_profiles_apply_runtime_budgets", &[]).is_none() {
        return;
    }
    const KEYS: [&str; 8] = [
        "HERMES_TASK_DEPTH_PROFILE",
        "HERMES_MAX_ITERATIONS",
        "HERMES_TOOL_CALL_MAX_CONCURRENCY",
        "HERMES_MAX_DELEGATE_DEPTH",
        "HERMES_PERF_GOV_WINDOW",
        "HERMES_PERF_GOV_LATENCY_WARN_MS",
        "HERMES_PERF_GOV_LATENCY_CRITICAL_MS",
        "HERMES_REPO_REVIEW_BUDGET_PROFILE",
    ];
    let _restore = RestoreEnvironment::capture(&KEYS);
    assert_eq!(current_task_depth_profile(), TaskDepthProfile::Balanced);

    for (profile, expected) in [
        (
            TaskDepthProfile::Shallow,
            [
                "shallow",
                "18",
                "10",
                "1",
                "6",
                "2800.00",
                "5200.00",
                "aggressive",
            ],
        ),
        (
            TaskDepthProfile::Deep,
            [
                "deep", "120", "6", "3", "10", "4800.00", "9000.00", "relaxed",
            ],
        ),
        (
            TaskDepthProfile::Max,
            ["max", "250", "5", "4", "12", "6500.00", "12000.00", "off"],
        ),
        (
            TaskDepthProfile::Balanced,
            [
                "balanced", "50", "12", "4", "8", "3500.00", "6500.00", "off",
            ],
        ),
    ] {
        apply_task_depth_profile(profile);
        for (key, value) in KEYS.into_iter().zip(expected) {
            assert_eq!(std::env::var(key).unwrap(), value, "{key}");
        }
        assert_eq!(current_task_depth_profile(), profile);
        assert_eq!(
            task_depth_runtime_summary(),
            format!(
                "task_depth profile={} max_iterations={} tool_concurrency={} max_delegate_depth={} repo_budget_profile={}",
                expected[0], expected[1], expected[2], expected[3], expected[7]
            )
        );
    }
}

#[test]
fn graph_candidate_walk_filters_subtrees_and_respects_limit() {
    if run_isolated("graph_candidate_walk_filters_subtrees_and_respects_limit") {
        return;
    }

    let root = tempfile::tempdir().expect("create graph project");
    let candidates = ["lib.RS", "main.py", "src/nested/view.tsx", "src/util.js"];
    for path in candidates.into_iter().chain([
        "README.md",
        "LICENSE",
        ".git/skip.rs",
        "target/skip.rs",
        "node_modules/skip.js",
        ".venv/skip.py",
        "venv/skip.py",
        "__pycache__/skip.py",
        ".mypy_cache/skip.py",
        ".pytest_cache/skip.py",
    ]) {
        let file = root.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "").unwrap();
    }
    let mut expected: Vec<_> = candidates
        .iter()
        .map(|path| root.path().join(path))
        .collect();
    expected.sort();
    let mut found = Vec::new();
    collect_graph_candidate_files(root.path(), 100, &mut found).unwrap();
    found.sort();
    assert_eq!(found, expected);

    found.clear();
    collect_graph_candidate_files(root.path(), 1, &mut found).unwrap();
    assert_eq!(found.len(), 1);
    assert!(expected.contains(&found[0]));

    let missing = root.path().join("does-not-exist");
    collect_graph_candidate_files(&missing, 1, &mut found).unwrap();
    assert_eq!(found.len(), 1);
    let error = collect_graph_candidate_files(&missing, 2, &mut found).unwrap_err();
    assert!(
        matches!(error, AgentError::Io(message) if message.starts_with(&format!("read_dir {}:", missing.display())))
    );
}

#[test]
fn graph_import_extraction_and_node_sanitization() {
    if run_isolated("graph_import_extraction_and_node_sanitization") {
        return;
    }

    assert_eq!(
        extract_semantic_refs_for_file(
            "rs",
            "  use crate::model; // note\nmod worker;\nuse ;\nmod ;\nfn main() {}",
        ),
        ["crate::model", "worker"]
    );
    assert_eq!(
        extract_semantic_refs_for_file(
            "py",
            "import os, , pathlib as paths\n  from .worker import run\nprint('done')",
        ),
        ["os", "pathlib", ".worker"]
    );
    for ext in ["ts", "tsx", "js", "jsx"] {
        assert_eq!(
            extract_semantic_refs_for_file(
                ext,
                "import { run } from './worker';\nexport { view } from \"./view\";\nconst path = require( 'node:path' );",
            ),
            ["./worker", "./view", "node:path"],
            "{ext}"
        );
    }
    assert!(extract_semantic_refs_for_file("txt", "use crate::model;").is_empty());
    assert_eq!(sanitize_graph_node("A-z_9./: \n?é"), "A-z_9./:____");
}

#[test]
fn graph_settings_prefer_current_names_and_fall_back_to_legacy_names() {
    if run_isolated("graph_settings_prefer_current_names_and_fall_back_to_legacy_names") {
        return;
    }

    if isolated_test_root(
        "graph_settings_prefer_current_names_and_fall_back_to_legacy_names",
        &[],
    )
    .is_none()
    {
        return;
    }
    const CURRENT_URL: &str = "CONTEXTLATTICE_ORCHESTRATOR_URL";
    const LEGACY_URL: &str = "MEMMCP_ORCHESTRATOR_URL";
    const CURRENT_KEY: &str = "CONTEXTLATTICE_API_KEY";
    const LEGACY_KEY: &str = "MEMMCP_API_KEY";
    let _restore = RestoreEnvironment::capture(&[CURRENT_URL, LEGACY_URL, CURRENT_KEY, LEGACY_KEY]);

    assert_eq!(contextlattice_base_url_for_graph(), "http://127.0.0.1:8075");
    assert_eq!(contextlattice_api_key_for_graph(), None);

    std::env::set_var(LEGACY_URL, "https://legacy.invalid");
    std::env::set_var(LEGACY_KEY, "legacy-key");
    std::env::set_var(CURRENT_URL, " \t");
    std::env::set_var(CURRENT_KEY, "");
    assert_eq!(
        contextlattice_base_url_for_graph(),
        "https://legacy.invalid"
    );
    assert_eq!(
        contextlattice_api_key_for_graph().as_deref(),
        Some("legacy-key")
    );

    std::env::set_var(CURRENT_URL, "https://current.invalid");
    std::env::set_var(CURRENT_KEY, "current-key");
    assert_eq!(
        contextlattice_base_url_for_graph(),
        "https://current.invalid"
    );
    assert_eq!(
        contextlattice_api_key_for_graph().as_deref(),
        Some("current-key")
    );

    std::env::remove_var(CURRENT_URL);
    std::env::remove_var(CURRENT_KEY);
    std::env::set_var(LEGACY_URL, " ");
    std::env::set_var(LEGACY_KEY, "\t");
    assert_eq!(contextlattice_base_url_for_graph(), "http://127.0.0.1:8075");
    assert_eq!(contextlattice_api_key_for_graph(), None);
}

#[tokio::test]
async fn graph_diagnostics_report_rejected_urls_without_network() {
    if run_isolated("graph_diagnostics_report_rejected_urls_without_network") {
        return;
    }

    // Reqwest rejects the file scheme before attempting a connection.
    if isolated_test_root(
        "graph_diagnostics_report_rejected_urls_without_network",
        &[
            (
                "CONTEXTLATTICE_ORCHESTRATOR_URL",
                "file:///coverage-diagnostics/",
            ),
            ("CONTEXTLATTICE_API_KEY", "coverage-key"),
        ],
    )
    .is_none()
    {
        return;
    }

    let lines = contextlattice_embedding_diagnostics_lines().await;
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(
        lines[0].starts_with("health_status: unreachable ("),
        "{lines:?}"
    );
    assert!(
        lines[1].starts_with("embedding_diagnostics: unavailable (unreachable: "),
        "{lines:?}"
    );
    assert_eq!(lines[2], "embedding_diagnostics: fallback=recall_telemetry");
    assert!(
        lines[3].starts_with("recall_telemetry: unreachable ("),
        "{lines:?}"
    );
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path.split_once("::").map_or(path.as_str(), |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([name, "--exact", "--test-threads=1", "--nocapture"])
        .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
        .output()
        .expect("run the isolated test");
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "isolated test {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    true
}
