use super::*;
use serde_json::{json, Value};
use std::{env, process::Command};

fn fixture_entry<'a>(path: &'a str, classification: &str, source: Option<&'a Py>) -> Entry<'a> {
    Entry {
        path,
        classification: classification.to_owned(),
        source,
        local_blob: "local-blob",
        upstream_blob: "upstream-blob",
    }
}

fn report_value(report: &Py) -> Value {
    serde_json::from_str(&py::dumps(report, None, true)).unwrap()
}

#[test]
fn requirements_and_actions_distinguish_runtime_surfaces() {
    for (paths, requirement, action) in [
        (
            &["tests/example.py"][..],
            "rust_equivalent_or_contract_test_required",
            "Map upstream test intent to Rust/Python contract coverage; add missing regression tests.",
        ),
        (
            &["plugins/a.py", "tools/b.py", "gateway/c.py", "environments/d.py"][..],
            "rust_native_runtime_parity_required_if_needed",
            "Inspect upstream/local behavior; implement Rust-native parity only if local coverage is missing.",
        ),
        (
            &["ui-tui/a.py", "website/b.py", "web/c.py"][..],
            "rust_primary_ux_or_documented_divergence_required",
            "Compare UX behavior; port necessary interaction semantics or document stronger local UX.",
        ),
        (
            &["skills/a.py", "optional-skills/b.py"][..],
            "skill_catalog_contract_required_if_needed",
            "Verify skill catalog/runtime loader coverage; add native contract tests if missing.",
        ),
        (
            &["agent.py"][..],
            "rust_port_required_if_runtime_surface",
            "Inspect upstream/local behavior; implement Rust-native parity only if local coverage is missing.",
        ),
        (
            &["config.toml", "tests-extra/example.toml"][..],
            "needs_rust_relevance_review",
            "Inspect upstream/local behavior; implement Rust-native parity only if local coverage is missing.",
        ),
    ] {
        for &path in paths {
            let entry = fixture_entry(path, "functional", None);
            assert_eq!(entry.requirement(), requirement, "{path}");
            assert_eq!(entry.action(), action, "{path}");
        }
    }

    for (classification, action) in [
        (
            "branding_only",
            "No runtime implementation; keep rationale current.",
        ),
        (
            "policy_only",
            "No runtime implementation; keep rationale current.",
        ),
        (
            "intentional_divergence",
            "No vendoring; verify approved divergence remains current.",
        ),
    ] {
        let entry = fixture_entry("tests/example.py", classification, None);
        assert_eq!(entry.requirement(), "not_required_for_runtime");
        assert_eq!(entry.action(), action);
    }
}

#[test]
fn entry_reports_preserve_ticket_type_and_only_expose_deferral_fields_when_deferred() {
    let source = py::loads(
        r#"{
            "path": "docs",
            "owner": "docs-team",
            "ticket": 42,
            "rationale": "Needs a contract test",
            "ledger": "PARITY-42",
            "review_date": "2030-05-01"
        }"#,
    )
    .unwrap();

    let path = "docs/guide.md";
    let deferred = fixture_entry(path, "deferred", Some(&source));
    assert_eq!(
        report_value(&deferred.report()),
        json!({
            "path": path,
            "workstream": "WS8",
            "status": "deferred_review",
            "classification": "deferred",
            "classification_path": "docs",
            "necessity": "deferred",
            "rust_requirement": "needs_rust_relevance_review",
            "existing_coverage": "not_established",
            "owner": "docs-team",
            "ticket": 42,
            "action": "Review against upstream by the review date; see the rationale and ledger line.",
            "local_blob": "local-blob",
            "upstream_blob": "upstream-blob",
            "rationale": "Needs a contract test",
            "ledger": "PARITY-42",
            "review_date": "2030-05-01"
        })
    );

    let claimed = "claimed_by_shared_diff_classification";
    for (kind, source, status, necessity, coverage) in [
        (
            "functional",
            Some(&source),
            "pending_review",
            "necessary_review",
            claimed,
        ),
        (
            "branding_only",
            Some(&source),
            "cleared_non_runtime",
            "not_runtime_required",
            claimed,
        ),
        (
            "policy_only",
            Some(&source),
            "cleared_non_runtime",
            "not_runtime_required",
            claimed,
        ),
        (
            "intentional_divergence",
            Some(&source),
            "cleared_intentional_divergence",
            "approved_divergence",
            claimed,
        ),
        (
            "other",
            Some(&source),
            "pending_classification",
            "unknown",
            claimed,
        ),
        (
            "unclassified",
            None,
            "pending_classification",
            "unknown",
            "unclaimed",
        ),
    ] {
        let report = report_value(&fixture_entry(path, kind, source).report());
        assert_eq!(report["classification"], kind);
        assert_eq!(report["status"], status, "{kind}");
        assert_eq!(report["necessity"], necessity, "{kind}");
        assert_eq!(report["existing_coverage"], coverage, "{kind}");
        for field in ["rationale", "ledger", "review_date"] {
            assert!(report.get(field).is_none(), "{field} leaked into {kind}");
        }
        if source.is_none() {
            for field in ["classification_path", "owner", "ticket"] {
                assert_eq!(report[field], "", "{field}");
            }
            assert_eq!(
                report["action"],
                "Add classification, owner, issue, and surgical plan before implementation."
            );
        }
    }
}

#[test]
fn summary_and_markdown_count_entries_and_rank_functional_prefixes() {
    let zeta = Py::dict([("path", "docs/zeta".into())]);
    let alpha = Py::dict([("path", "docs/alpha".into())]);
    let wide = Py::dict([("path", "docs/wide".into())]);
    let deferred = Py::dict([
        ("rationale", "Needs review".into()),
        ("ledger", "PARITY-7".into()),
        ("review_date", "2030-06-15".into()),
    ]);
    let entries = [
        fixture_entry("docs/unassigned.md", "unclassified", None),
        fixture_entry("docs/zeta/guide.md", "functional", Some(&zeta)),
        fixture_entry("docs/alpha/guide.md", "functional", Some(&alpha)),
        fixture_entry("docs/wide/first.md", "functional", Some(&wide)),
        fixture_entry("docs/wide/second.md", "functional", Some(&wide)),
        fixture_entry("docs/on-hold.md", "deferred", Some(&deferred)),
        fixture_entry("docs/logo.svg", "branding_only", None),
    ];
    let summary = Summary::build(&entries);

    assert_eq!(
        report_value(&summary.report(entries.len())),
        json!({
            "total_shared_different": 7,
            "pending_classification": 1,
            "pending_review": 4,
            "deferred_review": 1,
            "cleared_non_runtime": 1,
            "cleared_intentional_divergence": 0,
            "by_status": {
                "pending_classification": 1,
                "pending_review": 4,
                "deferred_review": 1,
                "cleared_non_runtime": 1
            },
            "by_workstream": {"WS8": 7},
            "by_classification": {
                "unclassified": 1,
                "functional": 4,
                "deferred": 1,
                "branding_only": 1
            },
            "by_rust_requirement": {
                "needs_rust_relevance_review": 6,
                "not_required_for_runtime": 1
            }
        })
    );

    assert_eq!(
        render_markdown("2030-01-01T00:00:00Z", &entries, &summary),
        concat!(
            "# Shared-Different Backlog\n\n",
            "Generated: `2030-01-01T00:00:00Z`\n\n",
            "## Summary\n\n",
            "- Total shared-different paths: `7`\n",
            "- Pending classification: `1`\n",
            "- Pending functional review: `4`\n",
            "- Deferred review (counted, not cleared): `1`\n",
            "- Cleared non-runtime: `1`\n",
            "- Cleared intentional divergence: `0`\n",
            "\n## Status Counts\n\n",
            "| Status | Count |\n| --- | ---: |\n",
            "| `cleared_non_runtime` | 1 |\n",
            "| `deferred_review` | 1 |\n",
            "| `pending_classification` | 1 |\n",
            "| `pending_review` | 4 |\n",
            "\n## Workstream Counts\n\n",
            "| Workstream | Count |\n| --- | ---: |\n",
            "| `WS8` | 7 |\n",
            "\n## Pending Classification\n\n",
            "| Path | Workstream | Action |\n| --- | --- | --- |\n",
            "| `docs/unassigned.md` | `WS8` | Add classification, owner, issue, and surgical plan before implementation. |\n",
            "\n## Pending Functional Review By Prefix\n\n",
            "| Classification Path | Count |\n| --- | ---: |\n",
            "| `docs/wide` | 2 |\n",
            "| `docs/zeta` | 1 |\n",
            "| `docs/alpha` | 1 |\n",
            "\n## Deferred\n\n",
            "Not yet shown to match in Rust; each row is counted and has a review date.\n\n",
            "| Path | Review date | Ledger | Reason |\n| --- | --- | --- | --- |\n",
            "| `docs/on-hold.md` | 2030-06-15 | PARITY-7 | Needs review |\n",
        )
    );
}

#[test]
fn run_validates_registry_and_writes_reports_using_only_local_git() {
    const ISOLATED: &str = "HERMES_PARITY_SHARED_DIFF_COVERAGE_CHILD";
    if env::var_os(ISOLATED).is_some() {
        check_run_with_local_repositories();
        return;
    }

    // Isolate Git's configuration without changing the environment of other tests.
    let home = tempfile::tempdir().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let name = format!("{module}::run_validates_registry_and_writes_reports_using_only_local_git");
    let mut child = Command::new(env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            child.env_remove(key);
        }
    }
    let output = child
        .args(["--exact", &name, "--nocapture"])
        .env(ISOLATED, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("GIT_CONFIG_GLOBAL", home.path().join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("\"total_shared_different\""), "{stdout}");
}

fn commit_guide(repo: &Path, contents: &str) {
    fs::write(repo.join("docs/guide.md"), contents).unwrap();
    git_text(repo, &["add", "--", "docs/guide.md"], true).unwrap();
    git_text(
        repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--no-gpg-sign",
            "-m",
            contents,
        ],
        true,
    )
    .unwrap();
}

fn check_run_with_local_repositories() {
    let fixture = tempfile::tempdir().unwrap();
    let local = fixture.path().join("local");
    let upstream = fixture.path().join("upstream");
    for (repo, contents) in [(&local, "local guide"), (&upstream, "upstream guide")] {
        fs::create_dir_all(repo.join("docs")).unwrap();
        git_text(repo, &["init", "--template=", "-b", "main"], true).unwrap();
        commit_guide(repo, contents);
    }

    let args = |no_fetch| SharedDiffBacklogArgs {
        repo_root: local.to_str().unwrap().to_owned(),
        local_ref: "main".to_owned(),
        upstream_remote: "upstream".to_owned(),
        upstream_url: upstream.to_str().unwrap().to_owned(),
        upstream_branch: "main".to_owned(),
        no_fetch,
        classification: "registry.json".to_owned(),
        out_json: "backlog.json".to_owned(),
        out_md: "backlog.md".to_owned(),
    };
    let registry = local.join("registry.json");
    let read_report = || -> Value {
        serde_json::from_str(&fs::read_to_string(local.join("backlog.json")).unwrap()).unwrap()
    };
    let read_markdown = || fs::read_to_string(local.join("backlog.md")).unwrap();

    fs::write(
        &registry,
        r#"{"items":[{"path":"docs","classification":"functional","owner":"docs-team"}]}"#,
    )
    .unwrap();
    assert_eq!(run(args(false)), Ok(0));

    let report = read_report();
    let markdown = read_markdown();
    let local_sha = git_text(&local, &["rev-parse", "main"], true).unwrap();
    let upstream_sha = git_text(&upstream, &["rev-parse", "main"], true).unwrap();
    assert_eq!(
        report["refs"],
        json!({
            "local_ref": "main",
            "local_sha": local_sha.as_str(),
            "upstream_ref": "upstream/main",
            "upstream_sha": upstream_sha.as_str()
        })
    );
    assert_eq!(report["summary"]["total_shared_different"], 1);
    assert_eq!(report["summary"]["pending_review"], 1);
    let entries = report["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["path"], "docs/guide.md");
    assert_eq!(entries[0]["classification"], "functional");
    assert_eq!(entries[0]["classification_path"], "docs");
    assert_eq!(entries[0]["owner"], "docs-team");
    for (field, repo) in [("local_blob", &local), ("upstream_blob", &upstream)] {
        let blob = git_text(repo, &["rev-parse", "main:docs/guide.md"], true).unwrap();
        assert_eq!(entries[0][field], blob.as_str());
    }
    let generated = report["generated_at_utc"].as_str().unwrap();
    assert!(!generated.is_empty());
    assert!(markdown.contains(&format!("Generated: `{generated}`")));
    assert!(markdown.contains("| `docs` | 1 |"));

    for (document, expected) in [
        ("[]", "expected a classification object"),
        (
            r#"{"items":{}}"#,
            "expected classification items to be objects",
        ),
        (
            r#"{"items":[{},4]}"#,
            "expected classification items to be objects",
        ),
    ] {
        fs::write(&registry, document).unwrap();
        assert_eq!(run(args(true)).unwrap_err(), expected);
    }
    let malformed = "{";
    fs::write(&registry, malformed).unwrap();
    let Err(parse_error) = py::loads(malformed) else {
        panic!("fixture should not parse");
    };
    assert_eq!(run(args(true)).unwrap_err(), parse_error);

    let mut directory_input = args(true);
    directory_input.classification = "docs".to_owned();
    let read_error = run(directory_input).unwrap_err();
    assert!(read_error.contains("docs:"), "{read_error}");
    assert_eq!(read_report(), report);
    assert_eq!(read_markdown(), markdown);

    commit_guide(&upstream, "new upstream guide");
    assert_ne!(
        git_text(&upstream, &["rev-parse", "main"], true).unwrap(),
        upstream_sha
    );
    fs::write(&registry, "{}").unwrap();
    assert_eq!(run(args(true)), Ok(0));

    let report = read_report();
    assert_eq!(report["refs"]["upstream_sha"], upstream_sha.as_str());
    assert_eq!(report["summary"]["pending_review"], 0);
    assert_eq!(report["summary"]["pending_classification"], 1);
    assert_eq!(report["entries"][0]["classification"], "unclassified");
    assert_eq!(report["entries"][0]["classification_path"], "");
    assert_eq!(report["entries"][0]["existing_coverage"], "unclaimed");
    assert!(read_markdown().contains("| `docs/guide.md` | `WS8` | Add classification,"));
}
