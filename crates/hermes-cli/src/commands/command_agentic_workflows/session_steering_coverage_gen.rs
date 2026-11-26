use super::*;

fn example_subgoal(text: &str, status: &str) -> SubgoalItem {
    SubgoalItem {
        text: text.to_owned(),
        status: status.to_owned(),
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        source: "user".to_owned(),
    }
}

#[test]
fn branch_checkpoint_name_uses_default_label_and_limits_session_prefix() {
    assert_eq!(
        branch_checkpoint_name("123456789abcdef", None),
        "branch-12345678-branch"
    );
    assert_eq!(branch_checkpoint_name("short", None), "branch-short-branch");
}

#[test]
fn branch_checkpoint_name_sanitizes_labels_without_splitting_utf8() {
    assert_eq!(
        branch_checkpoint_name("1234567é-tail", Some("  /Release-Candidate/v2_1--  ")),
        "branch-1234567-Release-Candidate-v2_1"
    );
    assert_eq!(
        branch_checkpoint_name("session-id", Some("café_2")),
        "branch-session--caf-_2"
    );
    for label in ["", "   ", " /💡/ "] {
        assert_eq!(
            branch_checkpoint_name("short", Some(label)),
            "branch-short-checkpoint"
        );
    }
}

#[test]
fn subgoal_checklist_render_distinguishes_empty_and_populated_checklists() {
    let mut checklist = SubgoalChecklist::for_session("render-test", Some("  Ship release  "));
    let empty = render_subgoal_checklist(&checklist);
    assert!(empty.starts_with(
        "Subgoal checklist\nsession: render-test\nobjective: Ship release\nitems: (none)\n"
    ));

    checklist.items = vec![
        example_subgoal("Draft notes", "completed"),
        example_subgoal("Recover old artifacts", "impossible"),
        example_subgoal("Publish notes", "pending"),
    ];
    let populated = render_subgoal_checklist(&checklist);
    assert!(populated.contains(
        "[x] 1. Draft notes (completed)\n[!] 2. Recover old artifacts (impossible)\n\
         [ ] 3. Publish notes (pending)\n"
    ));
    assert!(!populated.contains("items: (none)"));
}

#[test]
fn subgoal_checklist_save_creates_parent_and_round_trips() {
    const SANDBOX_ENV: &str = "HERMES_CLI_STEERING_COVERAGE_SANDBOX";
    const RECEIPT: &str = "subgoal-save-test-receipt";

    // A child test process can use a temporary Hermes home without changing
    // the environment seen by other tests.
    let Some(sandbox) = std::env::var_os(SANDBOX_ENV) else {
        let temp = tempfile::tempdir().unwrap();
        let sandbox = temp.path().canonicalize().unwrap();
        let receipt = sandbox.join(RECEIPT);
        std::fs::write(&receipt, "ready").unwrap();

        let (_, module) = module_path!().split_once("::").unwrap();
        let test_name = format!("{module}::subgoal_checklist_save_creates_parent_and_round_trips");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(test_name)
            .env(SANDBOX_ENV, &sandbox)
            .env("HERMES_HOME", &sandbox)
            .env("HOME", &sandbox)
            .env("USERPROFILE", &sandbox)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated test failed ({})\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read_to_string(receipt).unwrap(), "verified");
        return;
    };

    let sandbox = std::path::PathBuf::from(sandbox);
    assert!(sandbox.is_absolute());
    let receipt = sandbox.join(RECEIPT);
    assert_eq!(std::fs::read_to_string(&receipt).unwrap(), "ready");
    let home = hermes_config::hermes_home();
    assert!(
        home.starts_with(&sandbox),
        "unexpected Hermes home: {home:?}"
    );

    let session_id = "subgoal-save-test";
    let parent = home.join(SUBGOAL_DIR);
    assert!(!parent.exists());
    assert!(load_subgoal_checklist(session_id).is_none());

    let mut checklist = SubgoalChecklist::for_session(session_id, Some("  Document changes  "));
    checklist
        .items
        .push(example_subgoal("Write notes", "pending"));
    let path = save_subgoal_checklist(&checklist).unwrap();
    assert_eq!(path, parent.join("subgoal-save-test.json"));
    assert!(path.is_file());

    let loaded = load_subgoal_checklist(session_id).unwrap();
    assert_eq!(loaded.session_id, session_id);
    assert_eq!(loaded.objective.as_deref(), Some("Document changes"));
    assert_eq!(loaded.updated_at, checklist.updated_at);
    assert_eq!(loaded.items.len(), 1);
    assert_eq!(loaded.items[0].text, "Write notes");
    assert_eq!(loaded.items[0].status, "pending");
    assert_eq!(loaded.items[0].source, "user");

    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(&parent).unwrap();
    std::fs::write(&parent, "not a directory").unwrap();
    let Err(AgentError::Io(message)) = save_subgoal_checklist(&checklist) else {
        panic!("saving under a regular file should return an I/O error");
    };
    assert!(message.starts_with(&format!("Failed to create {}:", parent.display())));
    assert_eq!(std::fs::read_to_string(parent).unwrap(), "not a directory");

    std::fs::write(receipt, "verified").unwrap();
}
