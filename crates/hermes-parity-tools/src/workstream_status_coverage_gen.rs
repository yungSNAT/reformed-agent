// Unit tests for the file-reading, JSON, workstream and Markdown helpers in
// `workstream_status.rs`. Code that needs git (`skill_counts`, `build_workstreams`,
// `run`) is not exercised here.
use super::*;

use std::path::PathBuf;

fn to_json(value: &crate::py::Py) -> serde_json::Value {
    serde_json::from_str(&crate::py::dumps(value, None, true)).expect("valid json output")
}

#[test]
fn read_text_normalizes_line_endings_and_reports_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("text.txt");
    std::fs::write(&file, "a\r\nb\rc\n").unwrap();
    assert_eq!(read_text(&file).unwrap(), "a\nb\nc\n");

    let missing = dir.path().join("missing.txt");
    let error = read_text(&missing).unwrap_err();
    assert!(error.contains("missing.txt"), "{error}");
}

#[test]
fn files_below_collects_nested_files_and_errors_on_missing_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
    std::fs::write(dir.path().join("top.txt"), "1").unwrap();
    std::fs::write(dir.path().join("a/mid.txt"), "2").unwrap();
    std::fs::write(dir.path().join("a/b/deep.txt"), "3").unwrap();

    let mut files: Vec<PathBuf> = Vec::new();
    files_below(dir.path(), &mut files).unwrap();
    files.sort();
    assert_eq!(files.len(), 3);
    assert!(files.contains(&dir.path().join("top.txt")));
    assert!(files.contains(&dir.path().join("a/mid.txt")));
    assert!(files.contains(&dir.path().join("a/b/deep.txt")));

    let mut more: Vec<PathBuf> = Vec::new();
    let error = files_below(&dir.path().join("nope"), &mut more).unwrap_err();
    assert!(error.contains("nope"), "{error}");
    assert!(more.is_empty());
}

#[test]
fn read_sources_joins_sorted_rust_files_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::write(root.join("a.rs"), "AAA").unwrap();
    std::fs::write(root.join("b/c.rs"), "CCC").unwrap();
    std::fs::write(root.join("d.txt"), "TXT").unwrap();
    std::fs::write(root.join("noext"), "NOEXT").unwrap();

    let sources = read_sources(&root).unwrap();
    assert_eq!(sources, "AAA\nCCC");

    // A plain file path is read directly.
    assert_eq!(read_sources(&root.join("d.txt")).unwrap(), "TXT");

    // A missing path propagates the read error.
    assert!(read_sources(&root.join("missing.rs")).is_err());
}

#[test]
fn read_json_list_and_string_helpers() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.json");
    std::fs::write(&file, r#"{"items": [1, 2], "name": "x", "n": 3}"#).unwrap();
    let report = read_json(&file).unwrap();

    assert_eq!(list(&report, "items").unwrap().len(), 2);
    assert_eq!(
        list(&report, "name").unwrap_err(),
        "expected name to be a list"
    );
    assert_eq!(
        list(&report, "absent").unwrap_err(),
        "expected absent to be a list"
    );
    assert_eq!(string(&report, "name"), Some("x"));
    assert_eq!(string(&report, "n"), None);
    assert_eq!(string(&report, "absent"), None);

    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "{not json").unwrap();
    let error = read_json(&bad).unwrap_err();
    assert!(error.contains("bad.json"), "{error}");

    assert!(read_json(&dir.path().join("missing.json")).is_err());
}

fn write_parity_files(parity: &std::path::Path, classification_items: &str) {
    std::fs::create_dir_all(parity).unwrap();
    std::fs::write(
        parity.join("shared-different-classification.json"),
        classification_items,
    )
    .unwrap();
    let entries = r#"{"entries": [
        {"status": "deferred_not_covered", "path": "tests/test_a.py"},
        {"status": "not_rechecked_since_upstream_changed", "path": "hermes_cli/b.py"},
        {"status": "covered", "path": "c.py"},
        {"status": 5, "path": "d.py"}
    ]}"#;
    for name in [
        "python-test-suite-coverage.json",
        "hermes-cli-test-coverage.json",
        "ui-tui-source-coverage.json",
    ] {
        std::fs::write(parity.join(name), entries).unwrap();
    }
}

#[test]
fn deferred_by_workstream_counts_deferred_rows() {
    let dir = tempfile::tempdir().unwrap();
    let parity = dir.path().join("parity");
    write_parity_files(
        &parity,
        r#"{"items": [
            {"classification": "deferred", "path": "agent/x.py"},
            {"classification": "done", "path": "y.py"}
        ]}"#,
    );
    let deferred = deferred_by_workstream(&parity).unwrap();
    let total: usize = deferred.values().sum();
    assert_eq!(total, 7);
}

#[test]
fn deferred_by_workstream_errors() {
    let dir = tempfile::tempdir().unwrap();
    let parity = dir.path().join("parity");
    write_parity_files(&parity, r#"{"items": [{"classification": "deferred"}]}"#);
    let error = deferred_by_workstream(&parity).unwrap_err();
    assert!(error.contains("expected a deferred path"), "{error}");

    write_parity_files(&parity, r#"{"items": "nope"}"#);
    let error = deferred_by_workstream(&parity).unwrap_err();
    assert_eq!(error, "expected items to be a list");

    let empty = dir.path().join("empty");
    assert!(deferred_by_workstream(&empty).is_err());
}

#[test]
fn workstream_state_defer_and_report() {
    let mut status = Workstream::new(
        "WS9",
        "Example",
        true,
        &["first evidence"],
        crate::py::Py::dict([("sites", 2usize.into())]),
    );
    assert_eq!(status.state(), "complete");
    assert_eq!(status.evidence, vec!["first evidence".to_owned()]);

    let report = to_json(&status.report());
    assert_eq!(report["workstream"], "WS9");
    assert_eq!(report["title"], "Example");
    assert_eq!(report["state"], "complete");
    assert_eq!(report["evidence"][0], "first evidence");
    assert_eq!(report["metrics"]["sites"], 2);

    status.defer(4);
    assert!(!status.complete);
    assert_eq!(status.state(), "in_progress");
    assert_eq!(status.evidence.len(), 2);
    assert!(status.evidence[1].starts_with("4 rows are deferred"));
    let metrics = to_json(&status.metrics);
    assert_eq!(metrics["rows_not_shown_matching"], 4);
    assert_eq!(metrics["sites"], 2);

    let report = to_json(&status.report());
    assert_eq!(report["state"], "in_progress");
    assert_eq!(report["evidence"].as_array().unwrap().len(), 2);
}

#[test]
fn render_markdown_lists_all_workstreams() {
    let statuses = vec![
        Workstream::new(
            "WS2",
            "Core",
            true,
            &["e1", "e2"],
            crate::py::Py::dict([("a", 1usize.into())]),
        ),
        Workstream::new(
            "WS3",
            "Tools",
            false,
            &["e3"],
            crate::py::Py::dict([("b", true.into())]),
        ),
    ];
    let markdown = render_markdown("abc123", "upstream/main", "def456", &statuses);
    assert!(markdown.starts_with("# Workstream Status\n\n- Local HEAD: `abc123`\n"));
    assert!(markdown.contains("- Upstream: `upstream/main` (`def456`)"));
    assert!(markdown.contains("| `WS2` | Core | **complete** |\n"));
    assert!(markdown.contains("| `WS3` | Tools | **in_progress** |\n"));
    assert!(markdown.contains("## WS2 — Core\n\n- State: **complete**\n"));
    assert!(markdown.contains("## WS3 — Tools\n\n- State: **in_progress**\n"));
    assert!(markdown.contains("- e1\n- e2\n"));
    assert!(markdown.contains("- e3\n"));
    let metrics_a = format!(
        "- Metrics: `{}`\n",
        crate::py::dumps(&statuses[0].metrics, None, true)
    );
    assert!(markdown.contains(&metrics_a));
    assert_eq!(markdown.matches("- Metrics: `").count(), 2);

    let empty = render_markdown("h", "u", "unknown", &[]);
    assert!(empty.ends_with("| --- | --- | --- |\n\n"));
}
