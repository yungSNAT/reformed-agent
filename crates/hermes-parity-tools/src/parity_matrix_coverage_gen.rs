use super::*;
use clap::Parser;
use std::process::Command;

fn to_json(value: &Py) -> serde_json::Value {
    serde_json::from_str(&py::dumps(value, None, false)).expect("valid json output")
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Coverage Test",
            "-c",
            "user.email=coverage@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn write_file(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
    fs::write(path, contents).expect("write file");
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn build_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    write_file(dir, "shared.txt", "same\n");
    write_file(dir, "diff.txt", "v1\n");
    write_file(dir, "crates/hermes-cli/x.rs", "fn x() {}\n");
    commit_all(dir, "base");

    git(dir, &["checkout", "-q", "-b", "up"]);
    write_file(dir, "diff.txt", "v2\n");
    write_file(dir, "tools/new.py", "a\nb\n");
    commit_all(dir, "upstream change");
    write_file(dir, "patch.txt", "p\n");
    commit_all(dir, "same patch");

    git(dir, &["checkout", "-q", "main"]);
    write_file(dir, "docs/local.md", "local\n");
    commit_all(dir, "local change");
    write_file(dir, "patch.txt", "p\n");
    commit_all(dir, "same patch");

    git(dir, &["update-ref", "refs/remotes/upstream/main", "up"]);
}

#[test]
fn risk_labels_and_gap_escalation() {
    assert_eq!(Risk::Medium.label(), "medium");
    assert_eq!(Risk::High.label(), "high");
    assert_eq!(Risk::Critical.label(), "critical");

    assert_eq!(Risk::Medium.with_gap(5).label(), "medium");
    assert_eq!(Risk::High.with_gap(5).label(), "high");
    assert_eq!(Risk::Medium.with_gap(120).label(), "medium");
    assert_eq!(Risk::High.with_gap(120).label(), "high");
    assert_eq!(Risk::Medium.with_gap(400).label(), "high");
    assert_eq!(Risk::Critical.with_gap(400).label(), "critical");
    assert_eq!(Risk::Medium.with_gap(1000).label(), "critical");
}

#[test]
fn workstream_report_effort_and_risk() {
    let mut ws = Workstream::new(42, "Name", Risk::Medium);
    assert_eq!(ws.gap(), 0);
    let json = to_json(&ws.report("WSX"));
    assert_eq!(json["workstream"], "WSX");
    assert_eq!(json["issue"], 42);
    assert_eq!(json["name"], "Name");
    assert_eq!(json["upstream_only"], 0);
    assert_eq!(json["shared_different"], 0);
    assert_eq!(json["local_only"], 0);
    assert_eq!(json["risk"], "medium");
    assert_eq!(json["effort"], "S");

    ws.upstream_only = 20;
    ws.shared_different = 10;
    ws.local_only = 7;
    assert_eq!(ws.gap(), 30);
    let json = to_json(&ws.report("WSX"));
    assert_eq!(json["effort"], "M");
    assert_eq!(json["local_only"], 7);
    assert_eq!(json["risk"], "medium");

    ws.upstream_only = 140;
    let json = to_json(&ws.report("WSX"));
    assert_eq!(json["effort"], "L");
    assert_eq!(json["risk"], "medium");

    ws.upstream_only = 500;
    let json = to_json(&ws.report("WSX"));
    assert_eq!(json["effort"], "XL");
    assert_eq!(json["risk"], "high");

    ws.upstream_only = 1000;
    let json = to_json(&ws.report("WSX"));
    assert_eq!(json["risk"], "critical");
}

#[test]
fn tree_comparison_classifies_paths() {
    let local: BTreeMap<String, String> = [("a", "1"), ("b", "2"), ("c", "3")]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let upstream: BTreeMap<String, String> =
        [("b", "2"), ("c", "different"), ("d", "4"), ("e", "5")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
    let trees = TreeComparison::compare(&local, &upstream);
    assert_eq!(trees.local_only, vec!["a"]);
    assert_eq!(trees.shared_different, vec!["c"]);
    assert_eq!(trees.upstream_only, vec!["d", "e"]);
    assert_eq!(trees.shared_identical, 1);
}

#[test]
fn workstream_matrix_sorts_by_gap_then_id() {
    let trees = TreeComparison {
        upstream_only: vec!["crates/hermes-cli/a.rs", "tools/x.py", "tools/y.py"],
        local_only: vec!["docs/a.md"],
        shared_different: vec!["tests/t.py"],
        shared_identical: 0,
    };
    let json = to_json(&workstream_matrix(&trees));
    let rows = json.as_array().expect("array");
    let ids: Vec<_> = rows
        .iter()
        .map(|row| row["workstream"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, vec!["WS3", "WS2", "WS6", "WS4", "WS5", "WS7", "WS8"]);
    assert_eq!(rows[0]["upstream_only"], 2);
    assert_eq!(rows[0]["issue"], 7);
    assert_eq!(rows[1]["upstream_only"], 1);
    assert_eq!(rows[2]["shared_different"], 1);
    let ws5 = rows.iter().find(|row| row["workstream"] == "WS5").unwrap();
    assert_eq!(ws5["local_only"], 1);
    assert_eq!(ws5["upstream_only"], 0);
}

#[test]
fn top_buckets_groups_and_limits() {
    let paths = [
        " a/b/c.rs ",
        "a/b/d.rs",
        "x",
        "   ",
        "e/f/g/h",
        "e/f/z",
        "q/r",
    ];
    let json = to_json(&top_buckets(&paths, 10));
    let rows = json.as_array().unwrap();
    let got: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row["path"].as_str().unwrap().to_owned(),
                row["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("a/b".to_owned(), 2),
            ("e/f".to_owned(), 2),
            ("x".to_owned(), 1),
            ("q/r".to_owned(), 1),
        ]
    );

    let limited = to_json(&top_buckets(&paths, 1));
    assert_eq!(limited.as_array().unwrap().len(), 1);
    assert_eq!(limited[0]["path"], "a/b");

    let empty = to_json(&top_buckets(&[], 5));
    assert!(empty.as_array().unwrap().is_empty());
}

#[test]
fn is_truthy_matches_python_semantics() {
    let cases = [
        ("null", false),
        ("true", true),
        ("false", false),
        ("0", false),
        ("3", true),
        ("0.0", false),
        ("1.5", true),
        ("\"\"", false),
        ("\"x\"", true),
        ("[]", false),
        ("[1]", true),
        ("{}", false),
        ("{\"a\": 1}", true),
    ];
    for (text, expected) in cases {
        let value = py::loads(text).expect("parse");
        assert_eq!(is_truthy(&value), expected, "case {text}");
    }
}

#[test]
fn divergence_coverage_handles_registry_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let trees = TreeComparison {
        upstream_only: vec!["tools/x.py"],
        local_only: vec!["docs/a.md", "docsx/b.md", "toolsx/c.py"],
        shared_different: vec!["src/z.rs"],
        shared_identical: 0,
    };

    let missing = dir.path().join("missing.json");
    let (report, covered) = divergence_coverage(&missing, &trees).unwrap();
    assert!(report.is_empty());
    assert_eq!(covered, 0);

    let not_dict = dir.path().join("list.json");
    fs::write(&not_dict, "[1, 2]").unwrap();
    let error = divergence_coverage(&not_dict, &trees).expect_err("error");
    assert!(error.contains("expected a registry object"), "{error}");

    let invalid = dir.path().join("invalid.json");
    fs::write(&invalid, "{not json").unwrap();
    assert!(divergence_coverage(&invalid, &trees).is_err());

    let no_items = dir.path().join("noitems.json");
    fs::write(&no_items, "{\"other\": 1}").unwrap();
    let (report, covered) = divergence_coverage(&no_items, &trees).unwrap();
    assert!(report.is_empty());
    assert_eq!(covered, 0);

    let registry = dir.path().join("registry.json");
    fs::write(
        &registry,
        r#"{"items": [
            {"id": "D1", "status": "accepted", "workstream": "WS5", "summary": "docs and tools",
             "path_prefixes": ["docs/", "", null, "   ", "tools", "docs"]},
            "not-a-dict",
            {"id": "D2", "path_prefixes": "not-a-list"},
            {"id": "D3", "path_prefixes": ["src/z.rs", "tools/"]}
        ]}"#,
    )
    .unwrap();
    let (report, covered) = divergence_coverage(&registry, &trees).unwrap();
    assert_eq!(report.len(), 3);
    assert_eq!(covered, 3);

    let json = to_json(&Py::List(report));
    assert_eq!(json[0]["id"], "D1");
    assert_eq!(json[0]["status"], "accepted");
    assert_eq!(json[0]["workstream"], "WS5");
    assert_eq!(json[0]["summary"], "docs and tools");
    assert_eq!(json[0]["matched_files"], 2);
    let sample: Vec<_> = json[0]["matched_sample"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(sample, vec!["docs/a.md", "tools/x.py"]);

    assert_eq!(json[1]["id"], "D2");
    assert_eq!(json[1]["status"], "");
    assert_eq!(json[1]["workstream"], "WS8");
    assert_eq!(json[1]["summary"], "");
    assert_eq!(json[1]["matched_files"], 0);

    assert_eq!(json[2]["id"], "D3");
    assert_eq!(json[2]["matched_files"], 2);
}

#[test]
fn cherry_and_diff_stats_against_real_repo() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    build_repo(dir.path());

    let upstream_patches = cherry(dir.path(), "main", "up").unwrap();
    assert_eq!(upstream_patches.missing.len(), 1);
    assert_eq!(upstream_patches.represented.len(), 1);
    assert_eq!(upstream_patches.missing[0].len(), 40);

    let local_patches = cherry(dir.path(), "up", "main").unwrap();
    assert_eq!(local_patches.missing.len(), 1);
    assert_eq!(local_patches.represented.len(), 1);

    let stats = diff_stats(dir.path(), "main", "up").unwrap();
    assert_eq!(stats, [3, 3, 2]);

    let empty = diff_stats(dir.path(), "main", "main").unwrap();
    assert_eq!(empty, [0, 0, 0]);
}

#[derive(Parser)]
struct TestCli {
    #[command(flatten)]
    args: ParityMatrixArgs,
}

#[test]
fn run_writes_json_and_markdown_reports() {
    if !git_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    build_repo(root);
    let root_str = root.to_str().expect("utf8 temp path").to_owned();
    git(root, &["remote", "add", "upstream", &root_str]);
    write_file(
        root,
        "docs/parity/intentional-divergence.json",
        r#"{"items": [{"id": "DOCS", "status": "accepted", "path_prefixes": ["docs"]}]}"#,
    );

    let cli = TestCli::try_parse_from([
        "parity-matrix",
        "--repo-root",
        &root_str,
        "--upstream-url",
        &root_str,
        "--no-fetch",
        "--top-n",
        "5",
    ])
    .expect("parse args");
    assert_eq!(run(cli.args).unwrap(), 0);

    let json_text = fs::read_to_string(root.join("docs/parity/parity-matrix.json")).unwrap();
    assert!(json_text.ends_with('\n'));
    let json: serde_json::Value = serde_json::from_str(&json_text).unwrap();

    assert_eq!(json["refs"]["local_ref"], "main");
    assert_eq!(json["refs"]["upstream_ref"], "upstream/main");
    assert_eq!(
        json["refs"]["local_sha"],
        git(root, &["rev-parse", "main"]).as_str()
    );
    assert_eq!(
        json["refs"]["upstream_sha"],
        git(root, &["rev-parse", "up"]).as_str()
    );
    assert!(json["refs"]["merge_base"].is_string());

    let summary = &json["summary"];
    assert_eq!(summary["commits_behind"], 2);
    assert_eq!(summary["commits_ahead"], 2);
    assert_eq!(summary["upstream_patch_missing"], 1);
    assert_eq!(summary["upstream_patch_represented"], 1);
    assert_eq!(summary["local_patch_unique"], 1);
    assert_eq!(summary["files_only_upstream"], 1);
    assert_eq!(summary["files_only_local"], 1);
    assert_eq!(summary["files_shared_identical"], 3);
    assert_eq!(summary["files_shared_different"], 1);
    assert_eq!(summary["tree_files_changed"], 3);
    assert_eq!(summary["tree_insertions"], 3);
    assert_eq!(summary["tree_deletions"], 2);

    assert_eq!(json["top_upstream_only"][0]["path"], "tools/new.py");
    assert_eq!(json["top_local_only"][0]["path"], "docs/local.md");
    assert_eq!(json["top_shared_different"][0]["path"], "diff.txt");
    assert_eq!(json["top_missing_from_local"], json["top_upstream_only"]);
    assert_eq!(json["top_unique_to_local"], json["top_local_only"]);
    assert_eq!(json["workstream_matrix"].as_array().unwrap().len(), 7);

    let mapping = &json["commit_mapping"];
    assert_eq!(mapping["upstream_patch_missing"], 1);
    assert_eq!(mapping["upstream_patch_represented"], 1);
    assert_eq!(mapping["local_patch_unique"], 1);
    assert_eq!(mapping["local_patch_equivalent_to_upstream"], 1);
    assert_eq!(
        mapping["upstream_patch_missing_sample"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        mapping["local_patch_equivalent_sample"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(mapping["intentional_divergence_items"], 1);
    assert_eq!(mapping["intentional_divergence_covered_files"], 1);

    assert_eq!(json["intentional_divergence"][0]["id"], "DOCS");
    assert_eq!(json["intentional_divergence"][0]["matched_files"], 1);

    let md = fs::read_to_string(root.join("docs/parity/parity-matrix.md")).unwrap();
    assert!(!md.trim().is_empty());
}
