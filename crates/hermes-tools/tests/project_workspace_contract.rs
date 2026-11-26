use hermes_core::ToolHandler;
use hermes_tools::{ProjectFactsHandler, ProjectTreeHandler};
use serde_json::{json, Value};

#[tokio::test]
async fn project_tools_report_verify_commands_and_limit_hidden_tree_entries() {
    let sandbox = tempfile::tempdir().expect("temporary workspace");
    let root = sandbox.path().join("project");
    let nested = root.join("src");
    std::fs::create_dir_all(&nested).expect("create project");
    std::fs::create_dir(root.join("scripts")).expect("create scripts directory");
    for (file, content) in [
        (".fixture-note", "note"),
        (
            "package.json",
            r#"{"scripts":{"lint":"fixture","start":"fixture"}}"#,
        ),
        ("pnpm-lock.yaml", ""),
        ("package-lock.json", "{}"),
        ("uv.lock", ""),
        ("pyproject.toml", "[tool.pytest.ini_options]\n"),
        ("go.mod", "module fixture.invalid/project\n"),
        ("Makefile", "# Fixture\ntest:\n\t@echo fixture\nunused:\n"),
        ("scripts/run_tests.sh", ""),
        ("src/data.txt", "fixture"),
    ] {
        std::fs::write(root.join(file), content).expect("write project fixture");
    }
    let cwd = format!("  {}  ", nested.display());
    let facts = ProjectFactsHandler;
    assert_eq!(facts.schema().name, "project_facts");
    let result = facts.execute(json!({"cwd": cwd})).await.expect("facts");
    let facts: Value = serde_json::from_str(&result).expect("parse project facts");
    assert_eq!(facts["status"], "ok");
    assert_eq!(
        facts["root"],
        root.canonicalize()
            .expect("project root")
            .display()
            .to_string()
    );
    assert_eq!(facts["packageManagers"], json!(["pnpm", "uv"]));
    assert_eq!(
        facts["verifyCommands"],
        json!([
            "go test ./...",
            "make test",
            "pnpm run lint",
            "pytest",
            "scripts/run_tests.sh"
        ])
    );

    let tree = ProjectTreeHandler;
    assert_eq!(tree.schema().name, "project_tree");
    let result = tree
        .execute(json!({"cwd": cwd, "max_entries": 0, "include_hidden": true}))
        .await
        .expect("bounded tree");
    let bounded: Value = serde_json::from_str(&result).expect("parse bounded tree");
    assert_eq!(bounded["maxEntries"], 1);
    assert_eq!(bounded["includeHidden"], true);
    assert_eq!(bounded["truncated"], true);
    assert_eq!(
        bounded["entries"],
        json!([{
            "path": ".fixture-note",
            "name": ".fixture-note",
            "kind": "file",
            "depth": 1,
            "sizeBytes": 4
        }])
    );

    let result = tree
        .execute(json!({"cwd": cwd, "max_depth": 0}))
        .await
        .expect("shallow tree");
    let shallow: Value = serde_json::from_str(&result).expect("parse shallow tree");
    assert_eq!(shallow["maxDepth"], 1);
    assert_eq!(shallow["includeHidden"], false);
    assert_eq!(shallow["truncated"], false);
    let entries = shallow["entries"].as_array().expect("tree entries");
    assert_eq!(entries.len(), 9);
    assert!(entries.iter().any(|entry| entry["name"] == "src"));
    assert!(entries.iter().all(|entry| entry["depth"] == 1));
    assert!(entries.iter().all(|entry| entry["name"] != ".fixture-note"));
}
