//! The local CI runner (`scripts/run-repo-ci.sh`) mirrors the hosted
//! workflow's gates, and both compare against the pinned upstream revision.

use std::fs;
use std::path::PathBuf;

fn read(path: &str) -> String {
    let full = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    fs::read_to_string(&full)
        .unwrap_or_else(|err| panic!("read {}: {err}", full.display()))
        .replace("\r\n", "\n")
}

#[test]
fn repo_ci_runner_exists_and_declares_authority() {
    let text = read("scripts/run-repo-ci.sh");
    assert!(text.contains("repository-local CI contract"));
    assert!(text.contains("hosted GitHub Actions are a convenience mirror"));
    assert!(text.contains("+${UPSTREAM_REVISION}:refs/remotes/${UPSTREAM_REF}"));
    assert!(text.contains("[repo-ci] PASS"));
}

#[test]
fn parity_gates_use_the_pinned_upstream_revision() {
    let revision = read("docs/parity/upstream-reference.txt");
    let revision = revision.trim();
    assert_eq!(revision.len(), 40, "{revision}");
    assert!(revision
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

    let script = read("scripts/run-repo-ci.sh");
    let workflow = read(".github/workflows/ci.yml");
    assert!(script.contains("docs/parity/upstream-reference.txt"));
    assert!(workflow.contains("docs/parity/upstream-reference.txt"));
    // A gate that fetched or named upstream's live main would move with it.
    assert!(!script.contains("refs/heads/main"));
    assert!(!workflow.contains("upstream/main"));
    assert!(!workflow.contains("upstream main"));
}

#[test]
fn repo_ci_runner_covers_workflow_test_job_gates() {
    let script = read("scripts/run-repo-ci.sh");
    let workflow = read(".github/workflows/ci.yml");
    for command in [
        "bash scripts/run-python-parity-governance-tests.sh",
        "cargo run --locked -q -p hermes-parity-tools -- generate-parity-matrix --local-ref HEAD",
        "python3 scripts/generate-workstream-status.py",
        "cargo run --locked -q -p hermes-parity-tools -- generate-test-intent-mapping",
        "cargo run --locked -q -p hermes-parity-tools -- generate-test-coverage-audit --check",
        "cargo run --locked -q -p hermes-parity-tools -- generate-adapter-matrix",
        "cargo run --locked -q -p hermes-parity-tools -- validate-intentional-divergence --check --allow-warnings",
        "python3 scripts/generate-shared-diff-backlog.py --local-ref HEAD --no-fetch",
        "python3 scripts/generate-upstream-patch-queue.py --local-ref HEAD --max-commits 0",
        "python3 scripts/generate-global-parity-proof.py",
        "cargo fmt --all --check",
        "bash scripts/clippy-warning-gate.sh --check",
        "cargo test -p hermes-source-parity-tests --test rust_module_size_policy -- --nocapture",
        "bash scripts/check-runtime-placeholders.sh",
        "cargo test --workspace",
        "cargo test -p hermes-parity-tests",
        "cargo test -p hermes-source-parity-tests --test cli_command_contract",
        "cargo test -p hermes-protocol-parity-tests --test protocol_differential_contracts",
        "cargo test -p hermes-source-parity-tests --test global_parity_governance",
        "cargo run --locked -q -p hermes-parity-tools -- run-upstream-slash-parity-gate --upstream-ref",
        "cargo run --locked -q -p hermes-parity-tools -- run-upstream-surface-coverage-gate --repo-root . --upstream-ref",
    ] {
        assert!(workflow.contains(command), "ci.yml lacks: {command}");
        assert!(script.contains(command), "run-repo-ci.sh lacks: {command}");
    }
}

#[test]
fn readme_names_repo_ci_as_gold_standard() {
    let text = read("README.md");
    assert!(text.contains("Repo-local CI is the authoritative gate"));
    assert!(text.contains("GitHub Actions are a hosted mirror"));
    assert!(text.contains("bash scripts/run-repo-ci.sh"));
}

#[test]
fn cross_images_are_pinned_by_digest_for_every_built_target() {
    let workflow = read(".github/workflows/ci.yml");
    let cross = read("Cross.toml");
    let job = &workflow[workflow.find("  cross-build:").expect("cross-build job")..];
    let job = &job[..job.find("\n  macos-build:").unwrap_or(job.len())];
    let targets: Vec<&str> = job
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- "))
        .filter(|item| !item.contains(':') && !item.contains(' '))
        .collect();
    assert!(targets.len() >= 4, "{targets:?}");
    let pinned =
        regex::Regex::new(r#"(?m)^image = "ghcr\.io/cross-rs/([a-z0-9_-]+)@sha256:[0-9a-f]{64}"$"#)
            .unwrap();
    for target in targets {
        let section = format!("[target.{target}]");
        let start = cross
            .find(&section)
            .unwrap_or_else(|| panic!("Cross.toml lacks {section}"));
        let body = &cross[start + section.len()..];
        let body = &body[..body.find("\n[").unwrap_or(body.len())];
        let image = pinned
            .captures(body)
            .unwrap_or_else(|| panic!("{section} has no image pinned by digest"));
        assert_eq!(&image[1], target, "{section} pulls another target's image");
    }
}
