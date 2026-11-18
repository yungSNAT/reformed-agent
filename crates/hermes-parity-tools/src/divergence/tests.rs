use serde_json::{json, Value};

use super::*;

#[test]
fn iso_dates_parse_as_cpython_fromisoformat_does() {
    let ok = [
        ("2026-11-16", "2026-11-16"),
        ("20261116", "2026-11-16"),
        ("2026111612", "2026-11-16"),
        ("2026-W46-1", "2026-11-09"),
        ("2026W461", "2026-11-09"),
        ("2026-W46", "2026-11-09"),
        ("2026W46", "2026-11-09"),
        ("2026W46123", "2026-11-09"),
        ("2026-W53-1", "2026-12-28"),
        ("2020-W53-7", "2021-01-03"),
    ];
    for (raw, expected) in ok {
        assert_eq!(
            parse_iso_date(raw).map(|d| d.to_string()).as_deref(),
            Some(expected),
            "{raw}"
        );
    }
    for raw in [
        "2026-W4612",
        "2026-11",
        "0000-01-01",
        "2026-02-30",
        " 2026-11-16",
        "2026-11-1x",
        "2026-1116",
        "2025-W53-1",
        "2026-W46-8",
        "",
    ] {
        assert_eq!(parse_iso_date(raw), None, "{raw}");
    }
}

fn item(value: Value) -> Py {
    py::loads(&value.to_string()).unwrap()
}

fn paths(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|p| p.to_string()).collect()
}

fn today() -> Date {
    jiff::civil::date(2026, 11, 17)
}

#[test]
fn a_complete_item_passes_with_its_matched_paths() {
    let row = validate_item(
        &item(json!({
            "id": "x", "status": "approved", "workstream": "W", "summary": "s",
            "owner": " yungSNAT ", "ticket": 7, "last_reviewed": "2026-11-01",
            "review_date": "2027-01-01", "rationale": "long enough reason",
            "path_prefixes": ["docs/", "  "]
        })),
        &paths(&["docs/a.md", "docs", "docsx/b.md", "other.md"]),
        today(),
    );
    assert_eq!(
        py::dumps(&row, None, true),
        "{\"errors\": [], \"id\": \"x\", \"last_reviewed\": \"2026-11-01\", \"matched_files\": 2, \
         \"matched_sample\": [\"docs\", \"docs/a.md\"], \"overdue\": false, \"owner\": \"yungSNAT\", \
         \"review_date\": \"2027-01-01\", \"status\": \"approved\", \"ticket\": 7, \"warnings\": []}"
    );
}

#[test]
fn a_broken_item_lists_every_error_in_python_order() {
    let row = validate_item(
        &item(json!({
            "status": "maybe", "owner": "", "ticket": 0, "review_date": "soon",
            "last_reviewed": "", "rationale": "short", "path_prefixes": []
        })),
        &paths(&["docs/a.md"]),
        today(),
    );
    let errors = row.get("errors").unwrap();
    assert_eq!(
        errors,
        &Py::strs([
            "missing field: id",
            "missing field: workstream",
            "missing field: summary",
            "invalid status: maybe",
            "owner must be non-empty",
            "ticket must be a positive integer",
            "review_date must be ISO-8601 date (YYYY-MM-DD)",
            "last_reviewed must be non-empty",
            "rationale too short",
            "path_prefixes must be a non-empty list",
        ])
    );
    assert_eq!(
        row.get("warnings"),
        Some(&Py::strs([
            "no paths currently matched in local/upstream trees"
        ]))
    );
    assert_eq!(row.get("id"), Some(&Py::from("")));
}

#[test]
fn tickets_and_owners_follow_python_types() {
    let check = |ticket: &str| {
        let text = format!(r#"{{"ticket": {ticket}, "owner": 5}}"#);
        let row = validate_item(&py::loads(&text).unwrap(), &paths(&[]), today());
        let errors = py::dumps(row.get("errors").unwrap(), None, false);
        (errors.contains("ticket must be"), row.get("owner").cloned())
    };
    assert_eq!(check("true"), (false, Some(Py::from("5"))));
    assert!(check("false").0);
    assert!(check("3.0").0);
    assert!(check(r#""3""#).0);
    assert!(check("-1").0);
    assert!(!check("18446744073709551615").0);
    // Past u64: Python's int still reads 2**64 as a positive integer.
    assert!(!check("18446744073709551616").0);
}

#[test]
fn an_object_owner_prints_its_keys_in_document_order() {
    // str({"z": 1, "a": 2}) after json.loads keeps the document's order.
    let row = validate_item(
        &py::loads(r#"{"owner": {"z": 1, "a": 2}}"#).unwrap(),
        &paths(&[]),
        today(),
    );
    assert_eq!(row.get("owner"), Some(&Py::from("{'z': 1, 'a': 2}")));
}

#[test]
fn a_past_review_date_is_overdue() {
    let row = validate_item(
        &item(json!({ "review_date": "2026-11-16", "path_prefixes": ["a"] })),
        &paths(&["a"]),
        today(),
    );
    assert_eq!(row.get("overdue"), Some(&Py::Bool(true)));
    assert_eq!(
        row.get("warnings"),
        Some(&Py::strs(["review_date is overdue"]))
    );
}

#[test]
fn registry_path_is_reported_relative_to_the_repository() {
    let root = Path::new("/tmp/repo");
    assert_eq!(
        repo_relative(&root.join("docs/parity/intentional-divergence.json"), root),
        "docs/parity/intentional-divergence.json"
    );
    let outside = Path::new("/tmp/elsewhere/registry.json");
    assert_eq!(repo_relative(outside, root), py::path_str(outside));
}

#[test]
fn the_committed_report_names_the_registry_by_its_repository_path() {
    let report = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/parity/divergence-validation.json"),
    )
    .unwrap();
    let report = py::loads(&report).unwrap();
    assert_eq!(
        report.get("divergence_file"),
        Some(&Py::from("docs/parity/intentional-divergence.json"))
    );
}

#[test]
fn the_branch_comes_from_the_remote_ref() {
    assert_eq!(
        branch_from_remote_ref("upstream", "upstream/parity-reference"),
        "parity-reference"
    );
    assert_eq!(branch_from_remote_ref("upstream", "upstream/"), "main");
    assert_eq!(branch_from_remote_ref("upstream", "origin/x"), "main");
}
