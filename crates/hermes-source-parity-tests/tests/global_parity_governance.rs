use std::path::PathBuf;

use serde_json::Value;

mod support;
use support::rust_test_exists;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(path: &str) -> Value {
    let full = repo_root().join(path);
    let raw = std::fs::read_to_string(&full)
        .unwrap_or_else(|e| panic!("failed reading {}: {}", full.display(), e));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("failed parsing {}: {}", full.display(), e))
}

#[test]
fn test_intent_mapping_ratio_meets_gate() {
    let payload = read_json("docs/parity/test-intent-mapping.json");
    let ratio = payload["summary"]["mapping_ratio"]
        .as_f64()
        .expect("mapping_ratio should be number");
    assert!(
        ratio >= 0.9,
        "test-intent mapping ratio below gate: {}",
        ratio
    );
}

#[test]
fn test_coverage_audit_gate_passes() {
    let payload = read_json("docs/parity/test-coverage-audit.json");
    assert_eq!(
        payload["audit_gate"]["pass"].as_bool(),
        Some(true),
        "test coverage audit gate must pass"
    );
    assert_eq!(
        payload["audit_gate"]["critical_gaps"].as_u64(),
        Some(0),
        "test coverage audit must have zero critical gaps"
    );
    assert_eq!(
        payload["summary"]["missing_rust_test_refs"].as_u64(),
        Some(0),
        "test coverage audit must have zero missing Rust test refs"
    );
    // Every tracked row is covered, deferred or not rechecked at the pin; the last 2 are
    // counted and ratcheted by the deferred and not-rechecked tests below.
    let summary = &payload["summary"];
    let count = |key: &str| {
        summary[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{key} should be integer"))
    };
    assert_eq!(
        count("covered_behavior_rows")
            + count("coverage_manifest_deferred_entries")
            + count("coverage_manifest_not_rechecked_entries"),
        count("tracked_behavior_rows"),
        "every tracked behaviour row must be covered, deferred or counted as not rechecked"
    );
    // The committed audit leaves out the Rust test count, which moves with every added test.
    // The manifests cite Rust tests and none is missing, so the audit's scan found them.
    let referenced: u64 = payload["coverage_manifests"]
        .as_array()
        .expect("coverage_manifests should be array")
        .iter()
        .map(|manifest| manifest["referenced_rust_tests"].as_u64().unwrap_or(0))
        .sum();
    assert!(
        referenced > 0,
        "the manifests must cite Rust tests for the missing-ref check to mean anything"
    );
}

#[test]
fn test_sota_harness_matrix_gate_passes() {
    let payload = read_json("docs/parity/sota-harness-matrix.json");
    assert_eq!(
        payload["gate"]["pass"].as_bool(),
        Some(true),
        "SOTA harness matrix gate must pass"
    );
    assert_eq!(
        payload["gate"]["critical_gaps"].as_u64(),
        Some(0),
        "SOTA harness matrix must have zero critical gaps"
    );
    assert_eq!(
        payload["gate"]["missing_rust_test_refs"].as_u64(),
        Some(0),
        "SOTA harness matrix must have zero missing Rust test refs"
    );
    let ratio = payload["summary"]["domain_coverage_ratio"]
        .as_f64()
        .expect("domain_coverage_ratio should be number");
    assert!(
        ratio >= 1.0,
        "SOTA harness domain coverage ratio below gate: {}",
        ratio
    );
}

#[test]
fn test_behavioral_similarity_diff_gate_passes_and_references_real_tests() {
    let payload = read_json("docs/parity/behavioral-similarity-diff.json");
    assert_eq!(
        payload["gate"]["pass"].as_bool(),
        Some(true),
        "behavioral similarity diff gate must pass"
    );
    assert_eq!(
        payload["summary"]["behavioral_similarity_ratio"].as_f64(),
        Some(1.0),
        "behavioral similarity must be perfect for release"
    );
    assert_eq!(
        payload["summary"]["regressions"].as_u64(),
        Some(0),
        "behavioral diff must have zero regressions"
    );
    assert_eq!(
        payload["summary"]["gaps"].as_u64(),
        Some(0),
        "behavioral diff must have zero gaps"
    );
    assert_eq!(
        payload["summary"]["unverified_cases"].as_u64(),
        Some(0),
        "behavioral diff must have zero unverified cases"
    );
    assert_eq!(
        payload["summary"]["missing_rust_test_refs"].as_u64(),
        Some(0),
        "behavioral diff must have zero missing Rust test refs"
    );

    let superior_cases = payload["summary"]["superior_cases"]
        .as_u64()
        .expect("superior_cases should be integer");
    let required_superior = payload["summary"]["min_superiority_cases"]
        .as_u64()
        .expect("min_superiority_cases should be integer");
    assert!(
        superior_cases >= required_superior,
        "behavioral superiority below gate: {} < {}",
        superior_cases,
        required_superior
    );

    let cases = payload["cases"].as_array().expect("cases should be array");
    assert!(!cases.is_empty(), "behavioral diff must declare cases");
    for case in cases {
        let id = case["id"].as_str().expect("case id");
        let classification = case["classification"]
            .as_str()
            .expect("classification should be string");
        assert!(
            matches!(
                classification,
                "equivalent" | "superior" | "intentional_divergence"
            ),
            "case {id} has non-passing classification {classification}"
        );
        assert_eq!(
            case["verified"].as_bool(),
            Some(true),
            "case {id} must be verified"
        );
        assert!(
            case["issues"].as_array().expect("issues array").is_empty(),
            "case {id} must have no validation issues"
        );
        let tests = case["rust_tests"].as_array().expect("rust_tests array");
        assert!(!tests.is_empty(), "case {id} must cite Rust tests");
        for test in tests {
            let file = test["file"].as_str().expect("test file");
            let name = test["name"].as_str().expect("test name");
            assert!(
                rust_test_exists(file, name),
                "case {id} references missing or non-test Rust function {file}::{name}"
            );
        }
    }
}

#[test]
fn test_deep_problem_solving_diff_gate_passes_and_references_real_tests() {
    let payload = read_json("docs/parity/deep-problem-solving-diff.json");
    assert_eq!(
        payload["gate"]["pass"].as_bool(),
        Some(true),
        "deep problem-solving diff gate must pass"
    );
    assert_eq!(
        payload["summary"]["deep_problem_solving_ratio"].as_f64(),
        Some(1.0),
        "deep problem-solving ratio must be perfect for release"
    );
    assert_eq!(
        payload["summary"]["regressions"].as_u64(),
        Some(0),
        "deep problem-solving diff must have zero regressions"
    );
    assert_eq!(
        payload["summary"]["gaps"].as_u64(),
        Some(0),
        "deep problem-solving diff must have zero gaps"
    );
    assert_eq!(
        payload["summary"]["unverified_cases"].as_u64(),
        Some(0),
        "deep problem-solving diff must have zero unverified cases"
    );
    assert_eq!(
        payload["summary"]["missing_rust_test_refs"].as_u64(),
        Some(0),
        "deep problem-solving diff must have zero missing Rust test refs"
    );

    let superior_cases = payload["summary"]["superior_cases"]
        .as_u64()
        .expect("superior_cases should be integer");
    let required_superior = payload["summary"]["min_superiority_cases"]
        .as_u64()
        .expect("min_superiority_cases should be integer");
    assert!(
        superior_cases >= required_superior,
        "deep problem-solving superiority below gate: {} < {}",
        superior_cases,
        required_superior
    );
    let total_cases = payload["summary"]["total_cases"]
        .as_u64()
        .expect("total_cases should be integer");
    let required_total = payload["summary"]["min_total_cases"]
        .as_u64()
        .expect("min_total_cases should be integer");
    assert!(
        total_cases >= required_total,
        "deep problem-solving case count below gate: {} < {}",
        total_cases,
        required_total
    );

    let cases = payload["cases"].as_array().expect("cases should be array");
    assert!(
        !cases.is_empty(),
        "deep problem-solving diff must declare cases"
    );
    for case in cases {
        let id = case["id"].as_str().expect("case id");
        let classification = case["classification"]
            .as_str()
            .expect("classification should be string");
        assert!(
            matches!(
                classification,
                "equivalent" | "superior" | "intentional_divergence"
            ),
            "case {id} has non-passing classification {classification}"
        );
        assert_eq!(
            case["verified"].as_bool(),
            Some(true),
            "case {id} must be verified"
        );
        assert!(
            case["issues"].as_array().expect("issues array").is_empty(),
            "case {id} must have no validation issues"
        );
        let tests = case["rust_tests"].as_array().expect("rust_tests array");
        assert!(!tests.is_empty(), "case {id} must cite Rust tests");
        for test in tests {
            let file = test["file"].as_str().expect("test file");
            let name = test["name"].as_str().expect("test name");
            assert!(
                rust_test_exists(file, name),
                "case {id} references missing or non-test Rust function {file}::{name}"
            );
        }
    }
}

#[test]
fn test_adapter_matrix_has_no_placeholder_status() {
    let payload = read_json("docs/parity/adapter-feature-matrix.json");
    let non_native = payload["summary"]["non_rust_native"]
        .as_i64()
        .expect("non_rust_native should be integer");
    let placeholders = payload["summary"]["placeholder_status_entries"]
        .as_i64()
        .expect("placeholder_status_entries should be integer");
    assert_eq!(
        non_native, 0,
        "adapter matrix has non-rust-native entries: {}",
        non_native
    );
    assert_eq!(
        placeholders, 0,
        "adapter matrix has placeholder entries: {}",
        placeholders
    );
}

#[test]
fn test_divergence_registry_has_ownership_and_review_fields() {
    let payload = read_json("docs/parity/intentional-divergence.json");
    let items = payload["items"].as_array().expect("items should be array");
    assert!(!items.is_empty(), "expected intentional divergence entries");
    for item in items {
        for field in [
            "id",
            "status",
            "owner",
            "ticket",
            "last_reviewed",
            "review_date",
            "rationale",
        ] {
            assert!(
                item.get(field).is_some(),
                "divergence item missing required field: {}",
                field
            );
        }
        let owner = item["owner"].as_str().unwrap_or_default();
        assert!(
            !owner.trim().is_empty(),
            "divergence item owner must be non-empty"
        );
    }
}

#[test]
fn test_shared_diff_classification_covers_matrix_items() {
    let matrix = read_json("docs/parity/parity-matrix.json");
    let classification = read_json("docs/parity/shared-different-classification.json");

    let matrix_paths = matrix["top_shared_different"]
        .as_array()
        .expect("top_shared_different should be array");
    let class_items = classification["items"]
        .as_array()
        .expect("classification items should be array");
    let class_paths: std::collections::BTreeSet<String> = class_items
        .iter()
        .filter_map(|item| item["path"].as_str().map(|s| s.to_string()))
        .collect();

    for row in matrix_paths {
        let p = row["path"].as_str().expect("path should be string");
        assert!(
            class_paths.contains(p),
            "shared-different path is unclassified: {}",
            p
        );
    }
}

#[test]
fn test_exact_file_functional_classifications_are_current_shared_diffs() {
    let backlog = read_json("docs/parity/shared-diff-backlog.json");
    let classification = read_json("docs/parity/shared-different-classification.json");

    let backlog_entries = backlog["entries"]
        .as_array()
        .expect("entries should be array");
    let active_paths: std::collections::BTreeSet<String> = backlog_entries
        .iter()
        .filter_map(|entry| entry["path"].as_str().map(|s| s.to_string()))
        .collect();
    let active_classification_paths: std::collections::BTreeSet<String> = backlog_entries
        .iter()
        .filter_map(|entry| entry["classification_path"].as_str().map(|s| s.to_string()))
        .collect();

    let class_items = classification["items"]
        .as_array()
        .expect("classification items should be array");
    for item in class_items {
        if !matches!(
            item["classification"].as_str(),
            Some("functional" | "deferred")
        ) {
            continue;
        }
        let path = item["path"].as_str().expect("path should be string");
        let basename = path.rsplit('/').next().unwrap_or(path);
        if !basename.contains('.') {
            continue;
        }
        assert!(
            active_paths.contains(path) || active_classification_paths.contains(path),
            "exact-file functional classification is stale or unbacked by the current shared-diff backlog: {}",
            path
        );
    }
}

#[test]
fn test_shared_diff_backlog_has_no_pending_entries() {
    let backlog = read_json("docs/parity/shared-diff-backlog.json");

    assert_eq!(
        backlog["summary"]["pending_classification"].as_u64(),
        Some(0),
        "shared-diff backlog must have zero pending classifications"
    );
    assert_eq!(
        backlog["summary"]["pending_review"].as_u64(),
        Some(0),
        "shared-diff backlog must have zero pending functional reviews"
    );

    let pending: Vec<&str> = backlog["entries"]
        .as_array()
        .expect("entries should be array")
        .iter()
        .filter(|entry| {
            entry["status"]
                .as_str()
                .is_some_and(|status| status.starts_with("pending_"))
        })
        .filter_map(|entry| entry["path"].as_str())
        .collect();
    assert!(
        pending.is_empty(),
        "shared-diff backlog has pending entries: {}",
        pending.join(", ")
    );
}

/// Each deferred row says why, names its ticket and ledger line, and has a review date.
fn assert_deferred_row_is_explained(source: &str, row: &Value) {
    let path = row["path"].as_str().unwrap_or("<no path>");
    for field in ["rationale", "ledger"] {
        assert!(
            !row[field].as_str().unwrap_or_default().trim().is_empty(),
            "{source}: deferred row {path} needs a {field}"
        );
    }
    assert!(
        row["ticket"].as_u64().is_some()
            || !row["ticket"].as_str().unwrap_or_default().trim().is_empty(),
        "{source}: deferred row {path} needs a ticket"
    );
    let date = regex::Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("date pattern");
    assert!(
        date.is_match(row["review_date"].as_str().unwrap_or_default()),
        "{source}: deferred row {path} needs a review_date as YYYY-MM-DD"
    );
}

/// Deferred rows allowed at the v0.23.2 pin. Each count must equal the rows left deferred:
/// a change that resolves rows lowers its number here, and one that raises a number is a
/// governance change reviewers see in this file.
const DEFERRED_SHARED_DIFF: u64 = 22;
const DEFERRED_MANIFEST_ROWS: [(&str, u64); 3] = [
    ("python-test-suite-coverage.json", 46),
    ("hermes-cli-test-coverage.json", 36),
    ("ui-tui-source-coverage.json", 59),
];

#[test]
fn test_deferred_rows_are_explained_and_match_the_ratchet() {
    let classification = read_json("docs/parity/shared-different-classification.json");
    for item in classification["items"].as_array().expect("items") {
        if item["classification"].as_str() == Some("deferred") {
            assert_deferred_row_is_explained("shared-different-classification.json", item);
        }
    }
    let backlog = read_json("docs/parity/shared-diff-backlog.json");
    let deferred = backlog["entries"]
        .as_array()
        .expect("entries should be array")
        .iter()
        .filter(|entry| entry["status"].as_str() == Some("deferred_review"))
        .count() as u64;
    assert_eq!(
        backlog["summary"]["deferred_review"].as_u64(),
        Some(deferred),
        "backlog summary deferred_review must count the deferred entries"
    );
    assert_eq!(
        deferred, DEFERRED_SHARED_DIFF,
        "deferred shared-diff rows must equal DEFERRED_SHARED_DIFF"
    );

    for (name, allowed) in DEFERRED_MANIFEST_ROWS {
        let manifest = read_json(&format!("docs/parity/{name}"));
        let rows: Vec<&Value> = manifest["entries"]
            .as_array()
            .expect("entries should be array")
            .iter()
            .filter(|entry| entry["status"].as_str() == Some("deferred_not_covered"))
            .collect();
        for row in &rows {
            assert_deferred_row_is_explained(name, row);
        }
        assert_eq!(
            manifest["summary"]["deferred_paths"].as_u64(),
            Some(rows.len() as u64),
            "{name}: summary deferred_paths must count the deferred rows"
        );
        assert_eq!(
            rows.len() as u64,
            allowed,
            "{name}: deferred rows must equal DEFERRED_MANIFEST_ROWS"
        );
    }
}

/// Covered manifest rows whose upstream file changed after the row was judged, so the
/// coverage they cite was not rechecked at the pin. Each carries `not_rechecked_at_pin`;
/// a recheck removes the field and lowers its number here. A re-pin's review sets the field:
/// nothing here detects a covered row whose upstream file changes after that.
const NOT_RECHECKED_MANIFEST_ROWS: [(&str, u64); 3] = [
    ("python-test-suite-coverage.json", 131),
    ("hermes-cli-test-coverage.json", 110),
    ("ui-tui-source-coverage.json", 96),
];

#[test]
fn test_rows_not_rechecked_at_the_pin_are_counted_and_match_the_ratchet() {
    let pin = std::fs::read_to_string(repo_root().join("docs/parity/upstream-reference.txt"))
        .expect("upstream-reference.txt");
    for (name, allowed) in NOT_RECHECKED_MANIFEST_ROWS {
        let manifest = read_json(&format!("docs/parity/{name}"));
        let rows: Vec<&Value> = manifest["entries"]
            .as_array()
            .expect("entries should be array")
            .iter()
            .filter(|entry| !entry["not_rechecked_at_pin"].is_null())
            .collect();
        for row in &rows {
            let path = row["path"].as_str().unwrap_or("<no path>");
            assert_eq!(
                row["not_rechecked_at_pin"].as_str(),
                Some(pin.trim()),
                "{name}: {path} must name the current pin"
            );
            assert_eq!(
                row["status"].as_str(),
                Some("not_rechecked_since_upstream_changed"),
                "{name}: {path} must not read as covered"
            );
            assert!(
                row["rationale"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("Not rechecked at "),
                "{name}: {path} rationale must say it was not rechecked"
            );
        }
        assert_eq!(
            manifest["summary"]["not_rechecked_paths"].as_u64(),
            Some(rows.len() as u64),
            "{name}: summary not_rechecked_paths must count the rows"
        );
        assert_eq!(
            rows.len() as u64,
            allowed,
            "{name}: rows not rechecked must equal NOT_RECHECKED_MANIFEST_ROWS"
        );
    }
}

/// Rows not shown matching hold the release gate: deferred shared-diff rows, manifest rows
/// not rechecked at the pin, and deferred rows past their review date. The last also fails
/// the CI profile, which the daily Parity Audit checks.
#[test]
fn test_release_gate_holds_rows_not_shown_matching() {
    let thresholds = read_json("docs/parity/global-parity-thresholds.json");
    let proof = read_json("docs/parity/global-parity-proof.json");
    let backlog = read_json("docs/parity/shared-diff-backlog.json");
    let audit = read_json("docs/parity/test-coverage-audit.json");
    let today = &proof["generated_at_utc"]
        .as_str()
        .expect("generated_at_utc")[..10];
    let mut overdue = read_json("docs/parity/shared-different-classification.json")["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["classification"].as_str() == Some("deferred"))
        .filter(|item| item["review_date"].as_str().expect("review_date") < today)
        .count();
    for manifest in audit["coverage_manifests"]
        .as_array()
        .expect("coverage_manifests")
    {
        let manifest = read_json(manifest["path"].as_str().expect("manifest path"));
        overdue += manifest["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .filter(|entry| entry["status"].as_str() == Some("deferred_not_covered"))
            .filter(|entry| entry["review_date"].as_str().expect("review_date") < today)
            .count();
    }
    let expected = [
        (
            "max_shared_diff_deferred_rows",
            backlog["summary"]["deferred_review"]
                .as_f64()
                .expect("deferred_review"),
        ),
        (
            "max_test_coverage_not_rechecked_rows",
            audit["summary"]["coverage_manifest_not_rechecked_entries"]
                .as_f64()
                .expect("coverage_manifest_not_rechecked_entries"),
        ),
        ("max_deferred_review_overdue", overdue as f64),
    ];
    let checks = proof["release_gate"]["checks"].as_array().expect("checks");
    for (metric, actual) in expected {
        assert_eq!(
            thresholds["release_thresholds"]["metric_thresholds"][metric].as_f64(),
            Some(0.0),
            "{metric} must have a release threshold of 0"
        );
        let check = checks
            .iter()
            .find(|check| check["metric"].as_str() == Some(metric))
            .unwrap_or_else(|| panic!("release gate has no {metric} check"));
        assert_eq!(check["actual"].as_f64(), Some(actual), "{metric}");
        let status = if actual > 0.0 { "fail" } else { "pass" };
        assert_eq!(check["status"].as_str(), Some(status), "{metric}");
    }
    assert_eq!(
        thresholds["ci_thresholds"]["max_deferred_review_overdue"].as_f64(),
        Some(0.0),
        "an overdue deferred row must fail the daily audit"
    );
}
