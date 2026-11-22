use super::*;

#[test]
fn normalize_repo_relative_path_rejects_blank_and_quoted_empty() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::normalize_repo_relative_path_rejects_blank_and_quoted_empty");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let root = std::path::Path::new("/tmp/some-repo-root");
    assert_eq!(normalize_repo_relative_path(root, "   "), None);
    assert_eq!(normalize_repo_relative_path(root, "\"\""), None);
    assert_eq!(normalize_repo_relative_path(root, "``,"), None);
    assert_eq!(
        normalize_repo_relative_path(root, "\"src/main.rs\""),
        Some("src/main.rs".to_string())
    );
}

#[test]
fn objective_runtime_ledger_path_is_under_hermes_home_alpha() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::objective_runtime_ledger_path_is_under_hermes_home_alpha");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let path = objective_runtime_ledger_path();
    let home = hermes_config::hermes_home();
    assert!(path.starts_with(&home));
    assert!(path.ends_with(std::path::Path::new("alpha").join("objective_runtime_ledger.jsonl")));
    assert_eq!(
        path.file_name().and_then(|s| s.to_str()),
        Some("objective_runtime_ledger.jsonl")
    );
}

#[test]
fn read_replay_export_rows_missing_file_is_io_error() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::read_replay_export_rows_missing_file_is_io_error");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("does_not_exist.json");
    let err = read_replay_export_rows(&missing).unwrap_err();
    assert!(matches!(err, AgentError::Io(_)));
}

#[test]
fn read_replay_export_rows_invalid_json_is_config_error() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::read_replay_export_rows_invalid_json_is_config_error");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bad.json");
    std::fs::write(&path, "{ not json").unwrap();
    let err = read_replay_export_rows(&path).unwrap_err();
    assert!(matches!(err, AgentError::Config(_)));
}

#[test]
fn read_replay_export_rows_returns_rows_array() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::read_replay_export_rows_returns_rows_array");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("export.json");
    std::fs::write(
        &path,
        r#"{"rows":[{"event_hash":"a"},{"event_hash":"b"},{"other":1}]}"#,
    )
    .unwrap();
    let rows = read_replay_export_rows(&path).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0].get("event_hash").and_then(|v| v.as_str()),
        Some("a")
    );
}

#[test]
fn read_replay_export_rows_without_rows_key_is_empty() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::read_replay_export_rows_without_rows_key_is_empty");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("norows.json");
    std::fs::write(&path, r#"{"something":"else","rows":"not-an-array"}"#).unwrap();
    let rows = read_replay_export_rows(&path).unwrap();
    assert!(rows.is_empty());
}

#[test]
fn specpatch_block_reason_classifies_commands() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::specpatch_block_reason_classifies_commands");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    assert_eq!(
        specpatch_block_reason("sudo RM -RF / now"),
        Some("destructive command pattern")
    );
    assert_eq!(
        specpatch_block_reason("git reset --hard HEAD"),
        Some("history/destructive git command pattern")
    );
    assert_eq!(specpatch_block_reason("cargo test"), None);
}

#[test]
fn extract_marker_paths_finds_path_and_file_markers() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::extract_marker_paths_finds_path_and_file_markers");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let found = extract_marker_paths("saw path=src/a.rs, and file=b.txt; done");
    assert_eq!(found, vec!["src/a.rs".to_string(), "b.txt".to_string()]);
    assert!(extract_marker_paths("nothing here").is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn run_shell_capture_reports_exit_code_and_streams() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::run_shell_capture_reports_exit_code_and_streams");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let bash_ok = std::process::Command::new("bash")
        .arg("-c")
        .arg("true")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !bash_ok {
        return;
    }
    let (code, stdout, stderr) = run_shell_capture("echo hello_out; echo hello_err 1>&2; exit 3")
        .await
        .unwrap();
    assert_eq!(code, 3);
    assert!(stdout.ends_with("hello_out"), "stdout={stdout:?}");
    assert!(stderr.ends_with("hello_err"), "stderr={stderr:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn run_shell_capture_success_has_zero_exit() {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_none() {
        let name = concat!(module_path!(), "::run_shell_capture_success_has_zero_exit");
        let name = name.split_once("::").map_or(name, |(_, rest)| rest);
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env("HERMES_COVERAGE_GEN_ISOLATED", "1")
            .output()
            .expect("run the isolated test");
        assert!(
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed"),
            "isolated test {name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let bash_ok = std::process::Command::new("bash")
        .arg("-c")
        .arg("true")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !bash_ok {
        return;
    }
    let (code, stdout, _stderr) = run_shell_capture("printf 'line1\\nline2\\n'")
        .await
        .unwrap();
    assert_eq!(code, 0);
    assert!(stdout.ends_with("line1\nline2"), "stdout={stdout:?}");
}
