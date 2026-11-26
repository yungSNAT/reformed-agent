use std::path::PathBuf;

use super::*;

fn parse(text: &str) -> Py {
    match py::loads(text) {
        Ok(value) => value,
        Err(_) => panic!("invalid test json: {text}"),
    }
}

fn write_file(dir: &std::path::Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write test file");
    path
}

fn load_prior_err(root: &std::path::Path, output: &std::path::Path) -> String {
    match Prior::load(root, "HEAD", output) {
        Ok(_) => panic!("expected an error"),
        Err(error) => error,
    }
}

fn load_prior_ok(root: &std::path::Path, output: &std::path::Path) -> Prior {
    match Prior::load(root, "HEAD", output) {
        Ok(prior) => prior,
        Err(error) => panic!("unexpected error: {error}"),
    }
}

#[test]
fn disposition_default_is_pending_and_empty() {
    let value = Disposition::default();
    assert_eq!(value.status, "pending");
    assert_eq!(value.notes, "");
    assert_eq!(value.owner, "");
}

#[test]
fn disposition_append_note_joins_with_separator() {
    let mut value = Disposition::default();
    value.append_note("");
    assert_eq!(value.notes, "");
    value.append_note("first");
    assert_eq!(value.notes, "first");
    value.append_note("");
    assert_eq!(value.notes, "first");
    value.append_note("second");
    assert_eq!(value.notes, "first | second");
}

#[test]
fn disposition_apply_override_replaces_only_nonempty_fields() {
    let mut value = Disposition {
        status: "pending".to_owned(),
        notes: "old notes".to_owned(),
        owner: "alice".to_owned(),
    };
    let empty = Disposition {
        status: String::new(),
        notes: String::new(),
        owner: String::new(),
    };
    value.apply_override(&empty);
    assert_eq!(value.status, "pending");
    assert_eq!(value.notes, "old notes");
    assert_eq!(value.owner, "alice");

    let full = Disposition {
        status: "ported".to_owned(),
        notes: "new notes".to_owned(),
        owner: "bob".to_owned(),
    };
    value.apply_override(&full);
    assert_eq!(value.status, "ported");
    assert_eq!(value.notes, "new notes");
    assert_eq!(value.owner, "bob");
}

#[test]
fn text_and_trimmed_helpers() {
    let item = parse(r#"{"a": "  padded  ", "b": "plain"}"#);
    assert_eq!(text(&item, "missing", "fallback"), "fallback");
    assert_eq!(text(&item, "b", "fallback"), "plain");
    assert_eq!(trimmed(&item, "a"), "padded");
    assert_eq!(trimmed(&item, "missing"), "");
}

#[test]
fn read_json_handles_missing_invalid_and_valid_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = read_json(&dir.path().join("missing.json"));
    assert!(missing.is_dict());
    assert!(!nonempty(&missing));

    let invalid = write_file(dir.path(), "invalid.json", "{not json");
    let invalid = read_json(&invalid);
    assert!(invalid.is_dict());
    assert!(!nonempty(&invalid));

    let valid = write_file(dir.path(), "valid.json", r#"{"key": "value"}"#);
    let valid = read_json(&valid);
    assert!(valid.is_dict());
    assert!(valid.get("key").is_some());
}

#[test]
fn nonempty_matches_python_truthiness() {
    assert!(!nonempty(&Py::Null));
    assert!(!nonempty(&parse("null")));
    assert!(nonempty(&parse("true")));
    assert!(!nonempty(&parse("false")));
    assert!(nonempty(&parse("1")));
    assert!(!nonempty(&parse("0")));
    assert!(nonempty(&parse("1.5")));
    assert!(!nonempty(&parse("0.0")));
    assert!(nonempty(&parse(r#""a""#)));
    assert!(!nonempty(&parse(r#""""#)));
    assert!(nonempty(&parse("[1]")));
    assert!(!nonempty(&parse("[]")));
    assert!(nonempty(&parse(r#"{"a": 1}"#)));
    assert!(!nonempty(&Py::Dict(Vec::new())));
}

#[test]
fn shown_path_relative_and_absolute() {
    let root = std::path::Path::new("/repo/root");
    let inside = root.join("data").join("queue.json");
    assert_eq!(shown_path(&inside, root), "data/queue.json");

    let outside = std::path::Path::new("/elsewhere/queue.json");
    assert_eq!(shown_path(outside, root), py::path_str(outside));
}

#[test]
fn prior_load_missing_file_outside_root_is_empty_worktree() {
    let root = tempfile::tempdir().expect("root");
    let other = tempfile::tempdir().expect("other");
    let output = other.path().join("queue.json");
    let prior = load_prior_ok(root.path(), &output);
    assert_eq!(prior.source, "worktree");
    assert_eq!(prior.first_sha, "");
    assert!(prior.by_sha.is_empty());
}

#[test]
fn prior_load_reads_commits_from_worktree() {
    let root = tempfile::tempdir().expect("root");
    let other = tempfile::tempdir().expect("other");
    let output = write_file(
        other.path(),
        "queue.json",
        r#"{"commits": [
            {"sha": "  abc  ", "disposition": "ported", "notes": "n1", "owner": "alice"},
            {"sha": ""},
            {"sha": "def"}
        ]}"#,
    );
    let prior = load_prior_ok(root.path(), &output);
    assert_eq!(prior.source, "worktree");
    assert_eq!(prior.first_sha, "abc");
    assert_eq!(prior.by_sha.len(), 2);

    let abc = prior.by_sha.get("abc").expect("abc present");
    assert_eq!(abc.status, "ported");
    assert_eq!(abc.notes, "n1");
    assert_eq!(abc.owner, "alice");

    let def = prior.by_sha.get("def").expect("def present");
    assert_eq!(def.status, "pending");
    assert_eq!(def.notes, "");
    assert_eq!(def.owner, "");
}

#[test]
fn prior_load_empty_string_commits_is_treated_as_empty() {
    let root = tempfile::tempdir().expect("root");
    let other = tempfile::tempdir().expect("other");
    let output = write_file(other.path(), "queue.json", r#"{"commits": ""}"#);
    let prior = load_prior_ok(root.path(), &output);
    assert_eq!(prior.first_sha, "");
    assert!(prior.by_sha.is_empty());
}

#[test]
fn prior_load_rejects_non_object_document() {
    let root = tempfile::tempdir().expect("root");
    let other = tempfile::tempdir().expect("other");
    let output = write_file(other.path(), "queue.json", "[1, 2, 3]");
    let error = load_prior_err(root.path(), &output);
    assert_eq!(error, "previous queue in worktree must contain an object");
}

#[test]
fn prior_load_rejects_non_list_commits() {
    let root = tempfile::tempdir().expect("root");
    let other = tempfile::tempdir().expect("other");
    let output = write_file(other.path(), "queue.json", r#"{"commits": 5}"#);
    let error = load_prior_err(root.path(), &output);
    assert_eq!(error, "previous queue commits in worktree must be a list");

    let output = write_file(other.path(), "queue_null.json", r#"{"commits": null}"#);
    let error = load_prior_err(root.path(), &output);
    assert_eq!(error, "previous queue commits in worktree must be a list");

    let output = write_file(other.path(), "queue_str.json", r#"{"commits": "x"}"#);
    let error = load_prior_err(root.path(), &output);
    assert_eq!(error, "previous queue commits in worktree must be a list");
}

#[test]
fn prior_load_rejects_non_object_commit() {
    let root = tempfile::tempdir().expect("root");
    let other = tempfile::tempdir().expect("other");
    let output = write_file(other.path(), "queue.json", r#"{"commits": [1]}"#);
    let error = load_prior_err(root.path(), &output);
    assert_eq!(
        error,
        "previous queue commit in worktree must contain an object"
    );
}

#[test]
fn prior_load_inside_root_falls_back_to_worktree_when_git_has_no_file() {
    let root = tempfile::tempdir().expect("root");
    let output = write_file(
        root.path(),
        "queue_cov_gen_unique.json",
        r#"{"commits": [{"sha": "w1"}]}"#,
    );
    // git may be unavailable in the test environment; only assert on success.
    if let Ok(prior) = Prior::load(root.path(), "HEAD", &output) {
        assert_eq!(prior.source, "worktree");
        assert_eq!(prior.first_sha, "w1");
        assert!(prior.by_sha.contains_key("w1"));
    }
}

#[test]
fn overrides_missing_file_uses_only_defaults() {
    let dir = tempfile::tempdir().expect("dir");
    let overrides = Overrides::load(&dir.path().join("missing.json"));
    assert!(overrides.sha("abc").is_none());
    assert_eq!(overrides.subject("feat: something"), Ok(None));
    assert_eq!(
        overrides.subject("  review(core): tidy up  "),
        Ok(Some((
            "superseded",
            "review-only upstream nit commit; no Rust-runtime behavior delta",
            "default_subject_pattern",
        )))
    );
    assert_eq!(
        overrides.subject("chore(release): map someone in AUTHOR_MAP"),
        Ok(Some((
            "superseded",
            "release metadata-only AUTHOR_MAP update from upstream Python pipeline",
            "default_subject_pattern",
        )))
    );
    assert_eq!(
        overrides.subject("CHORE(release): add someone to AUTHOR_MAP and more"),
        Ok(Some((
            "superseded",
            "release metadata-only AUTHOR_MAP update from upstream Python pipeline",
            "default_subject_pattern",
        )))
    );
}

#[test]
fn overrides_sha_rows_exact_prefix_and_duplicates() {
    let dir = tempfile::tempdir().expect("dir");
    let path = write_file(
        dir.path(),
        "overrides.json",
        r#"{"sha": {
            "abc": {"disposition": "first", "notes": "n0", "owner": "o0"},
            " abc ": {"disposition": " ported ", "notes": " n1 ", "owner": " alice "},
            "skipme": 5,
            "deadbeef": {"disposition": "superseded"}
        }}"#,
    );
    let overrides = Overrides::load(&path);

    let exact = overrides.sha("abc").expect("exact match");
    assert_eq!(exact.status, "ported");
    assert_eq!(exact.notes, "n1");
    assert_eq!(exact.owner, "alice");

    let prefix = overrides.sha("abcdef0123").expect("prefix match");
    assert_eq!(prefix.status, "ported");

    let other = overrides.sha("deadbeef").expect("deadbeef");
    assert_eq!(other.status, "superseded");
    assert_eq!(other.notes, "");
    assert_eq!(other.owner, "");

    assert!(overrides.sha("skipme").is_none());
    assert!(overrides.sha("zzz").is_none());
}

#[test]
fn overrides_subject_patterns_match_case_insensitively() {
    let dir = tempfile::tempdir().expect("dir");
    let path = write_file(
        dir.path(),
        "overrides.json",
        r#"{"subject_patterns": [
            7,
            {"pattern": "^docs: ", "disposition": ""},
            {"pattern": "", "disposition": "ported"},
            {"pattern": " ^fix: ", "disposition": " ported ", "notes": " fixed upstream "}
        ]}"#,
    );
    let overrides = Overrides::load(&path);
    assert_eq!(
        overrides.subject("FIX: something"),
        Ok(Some((
            "ported",
            "fixed upstream",
            "subject_override_pattern"
        )))
    );
    assert_eq!(overrides.subject("docs: readme"), Ok(None));
    assert_eq!(
        overrides.subject("review(x): y"),
        Ok(Some((
            "superseded",
            "review-only upstream nit commit; no Rust-runtime behavior delta",
            "default_subject_pattern",
        )))
    );
}

#[test]
fn overrides_invalid_subject_pattern_reports_error() {
    let dir = tempfile::tempdir().expect("dir");
    let path = write_file(
        dir.path(),
        "overrides.json",
        r#"{"subject_patterns": [{"pattern": "(", "disposition": "ported"}]}"#,
    );
    let overrides = Overrides::load(&path);
    match overrides.subject("anything") {
        Ok(_) => panic!("expected invalid pattern error"),
        Err(error) => assert!(
            error.starts_with("invalid override pattern: "),
            "unexpected error: {error}"
        ),
    }
}
