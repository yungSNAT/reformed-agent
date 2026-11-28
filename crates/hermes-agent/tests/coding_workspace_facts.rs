use std::fs;

use hermes_agent::coding_context::{build_coding_workspace_block, detect_project_facts};

#[test]
fn workspace_facts_gather_local_verification_commands() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path();
    fs::create_dir(path.join("scripts")).unwrap();
    for (name, contents) in [
        (
            "package.json",
            r#"{"scripts":{"check":"tsc","dev":"vite"}}"#,
        ),
        (
            "Makefile",
            "# Local checks\n.PHONY: lint test\nlint:\ntest:\ntest:\ncheck fmt:\nserve:\n",
        ),
        ("scripts/run_tests.sh", ""),
        ("go.mod", "module example.com/reading-list\n"),
        ("pyproject.toml", "[tool.pytest.ini_options]\n"),
        ("poetry.lock", ""),
        ("AGENTS.md", "# Workspace rules\n"),
    ] {
        fs::write(path.join(name), contents).unwrap();
    }

    let facts = detect_project_facts(path);
    assert_eq!(
        facts.manifests,
        ["pyproject.toml", "package.json", "go.mod", "Makefile"]
    );
    assert_eq!(facts.package_managers, ["poetry"]);
    assert_eq!(facts.context_files, ["AGENTS.md"]);
    assert_eq!(
        facts.verify_commands,
        [
            "go test ./...",
            "make lint",
            "make test",
            "npm run check",
            "pytest",
            "scripts/run_tests.sh"
        ]
    );

    let block = build_coding_workspace_block(path).unwrap();
    assert!(block.contains("Project: pyproject.toml (poetry), package.json, go.mod, Makefile"));
    assert!(block.contains("Context files: AGENTS.md"));
    assert!(block.contains(&format!("Verify: {}", facts.verify_commands.join("; "))));
}

#[test]
fn workspace_facts_keep_valid_neighbours_when_project_metadata_cannot_be_read() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path();
    fs::write(path.join("Makefile"), [0xff]).unwrap();
    fs::write(path.join("pyproject.toml"), [0xff]).unwrap();
    fs::write(path.join("go.mod"), "module example.com/reading-list\n").unwrap();
    let package = path.join("package.json");

    for contents in [&[0xff][..], b"{", br#"{"scripts":[]}"#] {
        fs::write(&package, contents).unwrap();
        let facts = detect_project_facts(path);
        assert_eq!(
            facts.manifests,
            ["pyproject.toml", "package.json", "go.mod", "Makefile"]
        );
        assert_eq!(facts.verify_commands, ["go test ./..."]);
        assert!(facts.package_managers.is_empty());
    }

    fs::write(path.join("tox.ini"), "[pytest]\n").unwrap();
    assert_eq!(
        detect_project_facts(path).verify_commands,
        ["go test ./...", "pytest"]
    );
    assert!(build_coding_workspace_block(&path.join("absent")).is_none());
}
