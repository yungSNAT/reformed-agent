use super::*;
use std::{fs, path::Path, process::Command};

#[test]
fn utility_terms_deduplicate_and_stop_after_ten_unique_terms() {
    let terms = extract_utility_terms(
        "Profit PROFIT latency reliability safety parity accuracy throughput storage memory bandwidth ignored",
    );
    let actual: Vec<_> = terms
        .iter()
        .map(|term| (term.name.as_str(), term.weight))
        .collect();

    assert_eq!(
        actual,
        [
            ("profit", 1.25),
            ("latency", 1.25),
            ("reliability", 1.25),
            ("safety", 1.25),
            ("parity", 1.25),
            ("accuracy", 1.25),
            ("throughput", 1.0),
            ("storage", 1.0),
            ("memory", 1.0),
            ("bandwidth", 1.0),
        ]
    );
}

#[test]
fn utility_terms_default_to_correctness_when_no_token_qualifies() {
    let terms = extract_utility_terms("I, a, or 123!");
    let actual: Vec<_> = terms
        .iter()
        .map(|term| (term.name.as_str(), term.weight))
        .collect();

    assert_eq!(actual, [("correctness", 1.0)]);
}

fn assert_config_error(result: Result<ObjectiveContract, AgentError>, expected: &str) {
    match result {
        Err(AgentError::Config(message)) => assert_eq!(message, expected),
        Err(other) => panic!("expected config error {expected:?}, got {other}"),
        Ok(_) => panic!("expected config error {expected:?}, got a contract"),
    }
}

fn check_missing_contract(path: &Path) {
    clear_objective_contract().unwrap();
    assert!(load_objective_contract().unwrap().is_none());

    let expected = "objective contract is not initialized; run `/objective <text>` first";
    for result in [
        append_counterfactual("Try a different plan", "Less rework"),
        set_objective_contract_lifecycle_status("active", None),
        set_objective_contract_wait_pid(123, None),
        set_objective_contract_wait_session("test-session", None),
        set_objective_contract_wait_seconds(30, None),
        clear_objective_contract_wait_barrier(),
        set_objective_contract_behavior_mode("balanced"),
    ] {
        assert_config_error(result, expected);
    }

    assert!(!path.exists());
}

fn check_missing_directives_are_restored(path: &Path) {
    let objective = "Review the local notes";
    upsert_objective_contract(objective, false).unwrap();

    let mut legacy = set_objective_contract_behavior_mode("mission").unwrap();
    assert_eq!(legacy.behavior_mode, "mission");
    let expected = std::mem::take(&mut legacy.behavior_directives);
    assert!(!expected.is_empty());
    write_json_file(path, &legacy).unwrap();
    assert!(load_objective_contract()
        .unwrap()
        .unwrap()
        .behavior_directives
        .is_empty());

    let updated = upsert_objective_contract(objective, false).unwrap();
    assert_eq!(updated.objective_text, objective);
    assert_eq!(updated.behavior_mode, "mission");
    assert_eq!(updated.behavior_directives, expected);

    let stored = load_objective_contract().unwrap().unwrap();
    assert_eq!(stored.behavior_mode, "mission");
    assert_eq!(stored.behavior_directives, expected);
}

fn check_wait_rejections_and_lifecycle_update(path: &Path) {
    let before = fs::read(path).unwrap();
    assert_config_error(
        set_objective_contract_wait_pid(0, None),
        "objective wait pid must be positive",
    );
    assert_config_error(
        set_objective_contract_wait_session(" \t\n", None),
        "objective wait session id cannot be empty",
    );
    assert_config_error(
        set_objective_contract_wait_seconds(0, None),
        "objective wait seconds must be positive",
    );
    assert_eq!(fs::read(path).unwrap(), before);

    let waiting = set_objective_contract_wait_seconds(30, Some("  finishing checks  ")).unwrap();
    assert!(waiting.waiting_until_unix_ms > 0);
    assert_eq!(waiting.waiting_reason, "finishing checks");
    assert!(waiting.status_reason.is_empty());

    let inactive = [
        "paused",
        "completed",
        "cancelled",
        "abandoned",
        "inactive",
        "blocked",
        "stopped",
        "failed",
        "done",
    ]
    .into_iter()
    .map(canonical_objective_lifecycle_status)
    .find(|status| !objective_lifecycle_is_active(status))
    .expect("an inactive objective lifecycle status");
    let updated = set_objective_contract_lifecycle_status(&inactive, Some(" \t ")).unwrap();
    assert_eq!(updated.lifecycle_status, inactive);
    assert_eq!(updated.status_reason, "operator update");
    assert_eq!(updated.waiting_until_unix_ms, 0);
    assert!(updated.waiting_reason.is_empty());
    assert_eq!(objective_wait_remaining_seconds(&updated), None);

    let before = fs::read(path).unwrap();
    let expected = "objective wait requires an active objective";
    assert_config_error(set_objective_contract_wait_pid(123, None), expected);
    assert_config_error(
        set_objective_contract_wait_session("test-session", None),
        expected,
    );
    assert_config_error(set_objective_contract_wait_seconds(30, None), expected);
    assert_eq!(fs::read(path).unwrap(), before);
}

fn check_clear_contract(path: &Path) {
    assert!(path.is_file());
    clear_objective_contract().unwrap();
    assert!(!path.exists());
    assert!(load_objective_contract().unwrap().is_none());

    fs::create_dir(path).unwrap();
    match clear_objective_contract() {
        Err(AgentError::Io(message)) => {
            let prefix = format!("remove {} failed:", path.display());
            assert!(message.starts_with(&prefix), "{message}");
        }
        Err(other) => panic!("expected a removal error, got {other}"),
        Ok(()) => panic!("clearing a directory as a contract file unexpectedly succeeded"),
    }
    assert!(path.is_dir());
    fs::remove_dir(path).unwrap();
}

#[test]
fn objective_contract_disk_operations() {
    const SANDBOX_ENV: &str = "HERMES_OBJECTIVE_CONTRACT_COVERAGE_SANDBOX";
    const DONE_FILE: &str = "objective-contract-coverage-complete";

    // Run only this test in a child with an isolated home. Other tests keep
    // their original environment.
    if let Some(root) = std::env::var_os(SANDBOX_ENV) {
        let root = Path::new(&root);
        let path = objective_contract_path();
        assert!(
            path.starts_with(root) && path != root,
            "objective contract path escaped the test sandbox: {}",
            path.display()
        );

        check_missing_contract(&path);
        check_missing_directives_are_restored(&path);
        check_wait_rejections_and_lifecycle_update(&path);
        check_clear_contract(&path);
        fs::write(root.join(DONE_FILE), b"ok").unwrap();
        return;
    }

    let sandbox = tempfile::tempdir().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(format!("{module}::objective_contract_disk_operations"))
        .current_dir(sandbox.path())
        .env(SANDBOX_ENV, sandbox.path())
        .env("HERMES_HOME", sandbox.path().join(".hermes"))
        .env("HOME", sandbox.path())
        .env("USERPROFILE", sandbox.path())
        .output()
        .unwrap();

    assert!(
        output.status.success() && sandbox.path().join(DONE_FILE).is_file(),
        "isolated objective contract test did not complete ({})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
