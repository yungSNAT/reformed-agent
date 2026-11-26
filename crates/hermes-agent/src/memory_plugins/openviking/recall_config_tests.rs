use super::*;

const CHILD: &str = "REFORMED_OPENVIKING_RECALL_PARITY_TEST";

/// Runs `test` alone in a child process with only `env` set, so the environment
/// it reads cannot leak into or from other tests.
fn run_isolated(test: &str, env: &[(&str, &str)]) -> bool {
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let name = format!("{module}::{test}");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .envs(env.iter().copied())
        .env(CHILD, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("HERMES_HOME", home.path())
        .current_dir(home.path())
        .args(["--exact", &name, "--color", "never"]);
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env(
            "LLVM_PROFILE_FILE",
            std::path::absolute(profile).expect("resolve coverage output path"),
        );
    }
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    let output = command.output().expect("run isolated recall config test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(&format!("test {name} ... ok")),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    false
}

fn load_with_file(file: &str) -> OpenVikingRecallConfig {
    let profile = tempfile::tempdir().unwrap();
    let home = profile.path().to_str().expect("UTF-8 fixture path");
    std::fs::write(OpenVikingConfig::config_path(home), file).unwrap();
    OpenVikingConfig::load(home).recall
}

#[test]
fn recall_file_values_use_the_python_plugin_bounds() {
    if !run_isolated("recall_file_values_use_the_python_plugin_bounds", &[]) {
        return;
    }
    let recall = load_with_file(
        r#"{"recall_limit": " 99 ", "recall_full_read_limit": 250,
            "recall_max_injected_chars": 1, "recall_timeout_seconds": 0,
            "recall_request_timeout_seconds": 600}"#,
    );
    assert_eq!(recall.limit, 99);
    assert_eq!(recall.full_read_limit, 100);
    assert_eq!(recall.max_injected_chars, 100);
    assert_eq!(recall.timeout, Duration::from_millis(250));
    assert_eq!(recall.request_timeout, Duration::from_secs(60));

    let recall = load_with_file(r#"{"recall_limit": -1, "recall_full_read_limit": "17.9"}"#);
    assert_eq!(recall.limit, 1);
    assert_eq!(recall.full_read_limit, 17);
}

#[test]
fn recall_environment_overrides_the_config_file() {
    if !run_isolated(
        "recall_environment_overrides_the_config_file",
        &[
            ("OPENVIKING_RECALL_LIMIT", "-1"),
            ("OPENVIKING_RECALL_MAX_INJECTED_CHARS", " 2500.9 "),
            ("OPENVIKING_RECALL_FULL_READ_LIMIT", "nan"),
            ("OPENVIKING_RECALL_TIMEOUT_SECONDS", "inf"),
            ("OPENVIKING_RECALL_PREFER_ABSTRACT", "maybe"),
            ("OPENVIKING_RECALL_RESOURCES", ""),
        ],
    ) {
        return;
    }
    let recall = load_with_file(
        r#"{"recall_limit": 3, "recall_max_injected_chars": 9000,
            "recall_full_read_limit": 4, "recall_timeout_seconds": 2,
            "recall_score_threshold": 0.5, "recall_prefer_abstract": true,
            "recall_resources": true}"#,
    );
    assert_eq!(
        recall.limit, 1,
        "a set variable beats the file, then clamps"
    );
    assert_eq!(recall.max_injected_chars, 2500);
    assert_eq!(
        recall.full_read_limit, 4,
        "an unparsable variable keeps the file value"
    );
    assert_eq!(recall.timeout, Duration::from_secs(60));
    assert_eq!(
        recall.score_threshold, 0.5,
        "an unset variable keeps the file value"
    );
    assert!(!recall.prefer_abstract, "any other word is false");
    assert!(
        recall.include_resources,
        "an empty variable keeps the file value"
    );
}
