use std::collections::{HashMap, HashSet};

use super::*;

struct FakeTrees {
    upstream: HashMap<&'static str, Vec<&'static str>>,
    present: HashSet<&'static str>,
}

impl Trees for FakeTrees {
    fn upstream_files(&self, prefix: &str) -> Result<Vec<String>, String> {
        Ok(self.upstream[prefix]
            .iter()
            .map(|p| p.to_string())
            .collect())
    }

    fn present_locally(&self, paths: &[String]) -> Result<Vec<bool>, String> {
        Ok(paths
            .iter()
            .map(|p| self.present.contains(p.as_str()))
            .collect())
    }
}

fn report(prefixes: &[&str], trees: &FakeTrees, items: &[DivergenceItem]) -> Py {
    let prefixes: Vec<String> = prefixes.iter().map(|p| p.to_string()).collect();
    build_report(
        &ReportInput {
            repo_root: Path::new("/repo"),
            local_ref: "HEAD",
            local_mode: LocalMode::Ref,
            upstream_ref: "upstream/main",
            prefixes: &prefixes,
            divergence_items: items,
        },
        trees,
    )
    .unwrap()
}

fn summary(report: &Py, key: &str) -> Py {
    report.get("summary").unwrap().get(key).unwrap().clone()
}

#[test]
fn build_report_flags_missing() {
    let trees = FakeTrees {
        upstream: HashMap::from([
            ("skills", vec!["skills/a.md", "skills/b.md"]),
            ("website", vec!["website/docs/one.md"]),
        ]),
        present: HashSet::from(["skills/a.md", "website/docs/one.md"]),
    };
    let report = report(&["skills", "website"], &trees, &[]);
    assert_eq!(report.get("ok"), Some(&Py::Bool(false)));
    assert_eq!(summary(&report, "missing_total"), Py::Int(1));
    assert_eq!(
        report.get("missing_paths"),
        Some(&Py::strs(["skills/b.md"]))
    );
    let skills = report.get("by_prefix").unwrap().get("skills").unwrap();
    assert_eq!(skills.get("coverage_ratio"), Some(&Py::Float(0.5)));
}

#[test]
fn build_report_passes_when_all_present() {
    let trees = FakeTrees {
        upstream: HashMap::from([("plugins", vec!["plugins/a.py", "plugins/b.py"])]),
        present: HashSet::from(["plugins/a.py", "plugins/b.py"]),
    };
    let report = report(&["plugins"], &trees, &[]);
    assert_eq!(report.get("ok"), Some(&Py::Bool(true)));
    assert_eq!(summary(&report, "missing_total"), Py::Int(0));
    assert_eq!(summary(&report, "present_locally"), Py::Int(2));
}

#[test]
fn divergence_items_own_missing_paths_by_first_matching_prefix() {
    let trees = FakeTrees {
        upstream: HashMap::from([(
            "plugins",
            vec!["plugins/x/a.py", "plugins/xy/b.py", "plugins/y.py"],
        )]),
        present: HashSet::new(),
    };
    let items = [
        DivergenceItem {
            id: "first".into(),
            status: "approved".into(),
            workstream: "W".into(),
            path_prefixes: vec!["./plugins/x/".into()],
        },
        DivergenceItem {
            id: "second".into(),
            status: "temporary".into(),
            workstream: "W".into(),
            path_prefixes: vec!["plugins//x".into(), "plugins/y.py".into()],
        },
    ];
    let report = report(&["plugins"], &trees, &items);
    assert_eq!(
        report.get("missing_paths"),
        Some(&Py::strs(["plugins/xy/b.py"]))
    );
    let owned = report.get("intentional_divergence_paths").unwrap();
    assert_eq!(
        py::dumps(owned, None, false),
        "[{\"path\": \"plugins/x/a.py\", \"id\": \"first\", \"status\": \"approved\", \
         \"workstream\": \"W\", \"matched_prefix\": \"plugins/x\"}, {\"path\": \"plugins/y.py\", \
         \"id\": \"second\", \"status\": \"temporary\", \"workstream\": \"W\", \
         \"matched_prefix\": \"plugins/y.py\"}]"
    );
    assert_eq!(
        py::dumps(
            report.get("intentional_divergence_by_id").unwrap(),
            None,
            false
        ),
        "{\"first\": 1, \"second\": 1}"
    );
}

#[test]
fn a_repeated_prefix_replaces_its_entry_but_counts_missing_twice() {
    let trees = FakeTrees {
        upstream: HashMap::from([("docs", vec!["docs/a.md"])]),
        present: HashSet::new(),
    };
    let report = report(&["docs", "docs"], &trees, &[]);
    assert_eq!(summary(&report, "upstream_file_count"), Py::Int(1));
    assert_eq!(summary(&report, "missing_total"), Py::Int(2));
    assert_eq!(
        report.get("missing_paths"),
        Some(&Py::strs(["docs/a.md", "docs/a.md"]))
    );
}

#[test]
fn an_empty_prefix_reports_full_coverage() {
    let trees = FakeTrees {
        upstream: HashMap::from([("ui-tui", vec![])]),
        present: HashSet::new(),
    };
    let report = report(&["ui-tui"], &trees, &[]);
    let entry = report.get("by_prefix").unwrap().get("ui-tui").unwrap();
    assert_eq!(entry.get("coverage_ratio"), Some(&Py::Float(1.0)));
}

#[test]
fn rust_only_prefix_violations_blocks_test_prefixes() {
    let prefixes: Vec<String> = ["skills", "tests", "tests/cli", "./test/helpers", "docs"]
        .iter()
        .map(|p| p.to_string())
        .collect();
    assert_eq!(
        rust_only_prefix_violations(&prefixes),
        ["./test/helpers", "tests", "tests/cli"]
    );
}

#[test]
fn rust_only_prefix_violations_allows_core_prefixes() {
    let prefixes: Vec<String> = DEFAULT_PREFIXES.iter().map(|p| p.to_string()).collect();
    assert!(rust_only_prefix_violations(&prefixes).is_empty());
}

#[test]
fn normalize_prefix_strips_leading_dots_and_slashes_as_lstrip_does() {
    assert_eq!(normalize_prefix(" ./..//docs//a// "), "docs/a/");
    assert_eq!(normalize_prefix(".github/x"), "github/x");
    assert!(!path_matches_prefix("docs/a", "/"));
    assert!(path_matches_prefix("docs/a", "docs/"));
    assert!(!path_matches_prefix("docsx/a", "docs"));
}

#[test]
fn registry_keeps_only_active_items_with_list_prefixes() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("reg.json");
    std::fs::write(
        &registry,
        r#"{"items": [
            {"id": "a", "status": "approved", "workstream": "W", "path_prefixes": ["x/"]},
            {"id": "b", "status": "deprecated", "path_prefixes": ["y/"]},
            {"id": "c", "status": "temporary", "path_prefixes": "z/"},
            {"id": null, "status": "temporary"},
            "not an object"
        ]}"#,
    )
    .unwrap();
    let items = load_divergence_items(dir.path(), "reg.json").unwrap();
    let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["a", "None"]);
    assert!(items[1].path_prefixes.is_empty());
    assert!(load_divergence_items(dir.path(), " ").unwrap().is_empty());
    assert!(load_divergence_items(dir.path(), "absent.json")
        .unwrap()
        .is_empty());

    std::fs::write(&registry, r#"{"items": {}}"#).unwrap();
    assert!(load_divergence_items(dir.path(), "reg.json").is_err());
}
