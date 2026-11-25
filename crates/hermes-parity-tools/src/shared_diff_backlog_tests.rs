use super::*;

fn classification(path: &str, kind: &str) -> Py {
    Py::dict([("path", path.into()), ("classification", kind.into())])
}

#[test]
fn longest_prefix_match_prefers_specific_prefix() {
    let items = [
        classification("tests", "functional"),
        classification("tests/gateway", "policy_only"),
    ];
    let matching = longest_prefix_match("tests/gateway/test_slack.py", &items).unwrap();
    assert!(std::ptr::eq(matching, &items[1]));
}

#[test]
fn fetch_branch_updates_tracking_ref_after_upstream_rewind() {
    let upstream = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let git = |root: &Path, args: &[&str]| git_text(root, args, true).unwrap();
    let upstream = upstream.path();
    let local = local.path();
    git(upstream, &["init", "-b", "main"]);
    for message in ["first", "second"] {
        git(
            upstream,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--no-gpg-sign",
                "--allow-empty",
                "-m",
                message,
            ],
        );
    }
    git(local, &["init"]);
    // Give this fixture no default fetch refspec, so only the explicit one can force the update.
    git(
        local,
        &["config", "remote.upstream.url", upstream.to_str().unwrap()],
    );
    fetch_branch(local, "upstream", "main").unwrap();
    let tracking = "refs/remotes/upstream/main";
    let second = git(upstream, &["rev-parse", "HEAD"]);
    assert_eq!(git(local, &["rev-parse", tracking]), second);
    git(
        upstream,
        &["update-ref", "refs/heads/main", "HEAD^", &second],
    );
    fetch_branch(local, "upstream", "main").unwrap();
    assert_eq!(
        git(local, &["rev-parse", tracking]),
        git(upstream, &["rev-parse", "HEAD"])
    );
}

#[test]
fn only_shared_different_blobs_are_classified() {
    let tree = |entries: &[(&str, &str)]| {
        entries
            .iter()
            .map(|(path, blob)| (path.to_string(), blob.to_string()))
            .collect()
    };
    let local = tree(&[
        ("README.md", "local-readme"),
        ("tests/gateway/test_a.py", "local-test"),
        ("tests/unchanged.py", "same"),
        ("only-local.py", "local-only"),
    ]);
    let upstream = tree(&[
        ("README.md", "upstream-readme"),
        ("tests/gateway/test_a.py", "upstream-test"),
        ("tests/unchanged.py", "same"),
        ("only-upstream.py", "upstream-only"),
    ]);
    let items = [
        classification("README.md", "policy_only"),
        classification("tests/gateway", "functional"),
    ];
    let entries = build_entries(&local, &upstream, &items);
    let summary = Summary::build(&entries);
    assert_eq!(entries.len(), 2);
    assert_eq!(summary.status_count("cleared_non_runtime"), 1);
    assert_eq!(summary.status_count("pending_review"), 1);
    assert_eq!(entries[0].path, "README.md");
    assert_eq!(entries[0].status(), "cleared_non_runtime");
    assert_eq!(entries[1].path, "tests/gateway/test_a.py");
    assert_eq!(
        entries[1].requirement(),
        "rust_equivalent_or_contract_test_required"
    );
}
