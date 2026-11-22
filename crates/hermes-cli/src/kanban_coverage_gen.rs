use std::{env, process::Command};

use super::*;

const OLD_TIMESTAMP: &str = "2000-01-01T00:00:00Z";
const CONTEXTLATTICE_CASE: &str = "HERMES_KANBAN_COVERAGE_CONTEXTLATTICE_CASE";
const CONTEXTLATTICE_CHECKED: &str = "isolated ContextLattice assertions passed";

fn task_input(title: &str, lane: KanbanLane) -> NewKanbanTaskInput {
    NewKanbanTaskInput {
        title: title.to_owned(),
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

#[test]
fn lanes_have_stable_order_labels_and_aliases() {
    use KanbanLane::{Blocked, Doing, Done, Todo, Triage};

    assert_eq!(KanbanLane::all(), [Triage, Todo, Doing, Blocked, Done]);
    assert_eq!(
        KanbanLane::all().map(KanbanLane::as_str),
        ["triage", "todo", "doing", "blocked", "done"]
    );

    for (raw, expected) in [
        (" blocked ", Blocked),
        (" STALLED\t", Blocked),
        (" Done ", Done),
        ("\tCOMPLETED ", Done),
    ] {
        assert_eq!(KanbanLane::parse(raw), Some(expected), "{raw:?}");
    }
    assert_eq!(KanbanLane::parse("unknown"), None);
    assert_eq!(KanbanLane::parse(" \t"), None);
}

#[test]
fn ensure_board_rebuilds_an_empty_store() {
    let mut store = KanbanStore {
        schema_version: 0,
        current_board_id: "missing".to_owned(),
        boards: Vec::new(),
    };

    let board = ensure_board(&mut store, None);
    assert_eq!(board.id, "main");
    assert_eq!(board.name, "Main");
    assert_eq!(board.next_task_seq, 1);
    assert_eq!(store.current_board_id, "main");
    assert_eq!(store.schema_version, KANBAN_SCHEMA_VERSION);
    assert_eq!(store.boards.len(), 1);
}

#[test]
fn ensure_board_selects_by_id_or_name_and_repairs_a_stale_selection() {
    let mut store = KanbanStore::default();
    create_or_select_board(&mut store, "Release Notes", None);
    store.current_board_id = "main".to_owned();

    assert_eq!(
        ensure_board(&mut store, Some("release-notes")).name,
        "Release Notes"
    );
    assert_eq!(store.current_board_id, "release-notes");

    assert_eq!(ensure_board(&mut store, Some("mAiN")).id, "main");
    assert_eq!(store.current_board_id, "main");

    assert_eq!(
        ensure_board(&mut store, Some("RELEASE NOTES")).id,
        "release-notes"
    );
    assert_eq!(
        ensure_board(&mut store, Some("unknown")).id,
        "release-notes"
    );
    assert_eq!(store.current_board_id, "release-notes");

    store.current_board_id = "deleted-board".to_owned();
    assert_eq!(ensure_board(&mut store, Some("unknown")).id, "main");
    assert_eq!(store.current_board_id, "main");
}

#[test]
fn selecting_an_existing_board_fills_but_does_not_replace_its_project() {
    let mut store = KanbanStore::default();
    create_or_select_board(&mut store, "Release Notes", None).updated_at = OLD_TIMESTAMP.to_owned();
    store.current_board_id = "main".to_owned();

    let board = create_or_select_board(&mut store, "RELEASE NOTES", Some("project-a".to_owned()));
    assert_eq!(board.id, "release-notes");
    assert_eq!(board.name, "Release Notes");
    assert_eq!(board.project_path.as_deref(), Some("project-a"));
    assert_ne!(board.updated_at, OLD_TIMESTAMP);

    board.updated_at = OLD_TIMESTAMP.to_owned();
    assert_eq!(store.current_board_id, "release-notes");
    assert_eq!(store.boards.len(), 2);

    let board = create_or_select_board(&mut store, "release notes", Some("project-b".to_owned()));
    assert_eq!(board.project_path.as_deref(), Some("project-a"));
    assert_eq!(board.updated_at, OLD_TIMESTAMP);
    assert_eq!(store.boards.len(), 2);
}

#[test]
fn blank_lookup_does_not_match_an_empty_title() {
    let mut store = KanbanStore::default();
    let board = ensure_board(&mut store, None);
    let task = add_task(board, task_input(" \t", KanbanLane::Todo));

    assert!(task.title.is_empty());
    assert!(find_task_mut(board, " \t\n").is_none());
    assert_eq!(find_task_mut(board, &task.id).as_deref(), Some(&task));
}

#[test]
fn blocking_updates_the_task_and_starting_clears_the_reason() {
    let mut store = KanbanStore::default();
    let board = ensure_board(&mut store, None);
    let mut task = add_task(board, task_input("Review", KanbanLane::Todo));
    task.updated_at = OLD_TIMESTAMP.to_owned();

    set_blocked(&mut task, Some("  Waiting for review \n".to_owned()));
    assert_eq!(task.lane, KanbanLane::Blocked);
    assert_eq!(task.blocked_reason.as_deref(), Some("Waiting for review"));
    assert_ne!(task.updated_at, OLD_TIMESTAMP);
    assert!(task.started_at.is_none());

    set_blocked(&mut task, Some(" \t".to_owned()));
    assert!(task.blocked_reason.is_none());
    set_blocked(&mut task, Some("Waiting for approval".to_owned()));

    move_task(&mut task, KanbanLane::Doing, None);
    assert_eq!(task.lane, KanbanLane::Doing);
    assert_eq!(task.started_at.as_deref(), Some(task.updated_at.as_str()));
    assert!(task.blocked_reason.is_none());
}

#[test]
fn lane_counts_include_empty_lanes_but_exclude_archived_tasks() {
    use KanbanLane::{Blocked, Doing, Done, Todo, Triage};

    let mut store = KanbanStore::default();
    let board = ensure_board(&mut store, None);
    let archived = add_task(board, task_input("Old task", Done));
    assert_eq!(archive_done(board), 1);
    assert_eq!(board.archived, [archived]);

    for (title, lane) in [
        ("Incoming", Triage),
        ("First queued", Todo),
        ("Second queued", Todo),
        ("Recently finished", Done),
    ] {
        add_task(board, task_input(title, lane));
    }

    assert_eq!(
        lane_counts(board),
        [(Triage, 1), (Todo, 2), (Doing, 0), (Blocked, 0), (Done, 1)]
    );
}

#[test]
fn board_slugs_collapse_separator_runs_and_trim_the_edges() {
    assert_eq!(
        slugify_board_id("  ///Release__Candidate !! 2...  "),
        "release-candidate-2"
    );
    assert!(slugify_board_id(" /é__! ").is_empty());
}

#[test]
fn board_ids_use_the_first_available_suffix_or_a_uuid_after_exhaustion() {
    let mut store = KanbanStore::default();
    let template = store.boards[0].clone();
    assert_eq!(unique_board_id(&store, ""), "main-2");

    store.boards.extend((2..10_000).map(|suffix| KanbanBoard {
        id: format!("main-{suffix}"),
        ..template.clone()
    }));
    assert_eq!(store.boards.remove(2).id, "main-3");
    assert_eq!(unique_board_id(&store, "main"), "main-3");

    store.boards.push(KanbanBoard {
        id: "main-3".to_owned(),
        ..template
    });
    let generated = unique_board_id(&store, "");
    let suffix = generated
        .strip_prefix("main-")
        .expect("main board ID prefix");
    assert_eq!(suffix.len(), 32);
    assert_eq!(
        uuid::Uuid::parse_str(suffix)
            .expect("UUID suffix")
            .get_version_num(),
        4
    );
    assert!(!store.boards.iter().any(|board| board.id == generated));
}

#[test]
fn dependency_deduplication_keeps_first_spelling_and_order() {
    let values = [
        " K-0002 ", " \t\n", "k-0002", "K-0001", " k-0003 ", "k-0001",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    assert_eq!(dedupe_strings(values), ["K-0002", "K-0001", "k-0003"]);
}

fn run_contextlattice_case(case: &str, vars: &[(&str, &str)]) {
    // Configure a child test process so other tests never see these settings.
    let home = tempfile::tempdir().expect("temporary home");
    let executable = env::current_exe().expect("test executable");
    let (_, module) = module_path!().split_once("::").expect("child module path");
    let test_name = format!("{module}::contextlattice_configuration_and_invalid_url");
    let mut command = Command::new(executable);
    command
        .args(["--exact", test_name.as_str(), "--nocapture"])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env(CONTEXTLATTICE_CASE, case);

    for key in [
        "CONTEXTLATTICE_ORCHESTRATOR_URL",
        "MEMMCP_ORCHESTRATOR_URL",
        "HERMES_KANBAN_CONTEXTLATTICE_TIMEOUT_SECS",
        "CONTEXTLATTICE_TIMEOUT_SECS",
        "HERMES_KANBAN_CONTEXTLATTICE_SYNC",
        "CONTEXTLATTICE_API_KEY",
    ] {
        command.env_remove(key);
    }

    let output = command
        .envs(vars.iter().copied())
        .output()
        .expect("run isolated ContextLattice test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(CONTEXTLATTICE_CHECKED),
        "ContextLattice case {case} did not pass ({})\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn contextlattice_configuration_and_invalid_url() {
    match env::var(CONTEXTLATTICE_CASE).as_deref() {
        Ok("defaults") => {
            assert_eq!(contextlattice_orchestrator_url(), "http://127.0.0.1:8075");
            assert_eq!(contextlattice_timeout(), Duration::from_secs(10));
        }
        Ok("legacy") => {
            assert_eq!(contextlattice_orchestrator_url(), "https://legacy.invalid");
            assert_eq!(contextlattice_timeout(), Duration::from_millis(2_500));
        }
        Ok("upper-bound") => {
            assert_eq!(contextlattice_timeout(), Duration::from_secs(60));
        }
        Ok("invalid-url") => {
            assert_eq!(contextlattice_orchestrator_url(), "http://[");
            assert_eq!(contextlattice_timeout(), Duration::from_secs(1));

            // The invalid host fails during request construction, before any I/O.
            let mut store = KanbanStore::default();
            let board = ensure_board(&mut store, None);
            let result = maybe_checkpoint_to_contextlattice(
                board,
                KanbanActionInput {
                    action: "move".to_owned(),
                    task_id: Some("K-0001".to_owned()),
                    lane: Some(KanbanLane::Blocked),
                    summary: "Waiting\nfor review".to_owned(),
                },
            );
            assert!(result.attempted);
            assert!(!result.succeeded);
            assert!(
                result
                    .detail
                    .starts_with("ContextLattice checkpoint failed: builder error"),
                "{}",
                result.detail
            );
        }
        _ => {
            run_contextlattice_case("defaults", &[]);
            run_contextlattice_case(
                "legacy",
                &[
                    ("MEMMCP_ORCHESTRATOR_URL", "https://legacy.invalid///"),
                    ("CONTEXTLATTICE_TIMEOUT_SECS", "2.5"),
                ],
            );
            run_contextlattice_case("upper-bound", &[("CONTEXTLATTICE_TIMEOUT_SECS", "120")]);
            run_contextlattice_case(
                "invalid-url",
                &[
                    ("CONTEXTLATTICE_ORCHESTRATOR_URL", "http://[///"),
                    ("MEMMCP_ORCHESTRATOR_URL", "https://legacy.invalid"),
                    ("HERMES_KANBAN_CONTEXTLATTICE_TIMEOUT_SECS", "0.25"),
                    ("CONTEXTLATTICE_TIMEOUT_SECS", "2.5"),
                    ("HERMES_KANBAN_CONTEXTLATTICE_SYNC", "yes"),
                    ("CONTEXTLATTICE_API_KEY", "test-key"),
                ],
            );
            return;
        }
    }
    println!("{CONTEXTLATTICE_CHECKED}");
}
