use super::*;

struct SavedEnv {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl SavedEnv {
    fn capture(key: &'static str) -> Self {
        Self {
            key,
            previous: std::env::var_os(key),
        }
    }

    fn set(&self, value: &str) {
        std::env::set_var(self.key, value);
    }
}

impl Drop for SavedEnv {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
fn stream_extra_event_leaves_an_empty_handle_slot_available() {
    if run_isolated("stream_extra_event_leaves_an_empty_handle_slot_available") {
        return;
    }

    let shared = Arc::new(StdMutex::new(None));

    App::push_stream_extra_event(&shared, serde_json::json!({"event": "test"}));

    assert!(shared
        .try_lock()
        .expect("pushing an event should leave the slot unlocked")
        .is_none());
}

#[test]
fn status_preview_collapses_whitespace_and_truncates_by_character() {
    if run_isolated("status_preview_collapses_whitespace_and_truncates_by_character") {
        return;
    }

    assert_eq!(App::preview_for_status(" \t\n ", 8), "");
    assert_eq!(App::preview_for_status(" é\t中\n🙂 ", 5), "é 中 🙂");
    assert_eq!(App::preview_for_status("  é中🙂\tfin  ", 4), "é中🙂…");
    assert_eq!(App::preview_for_status("é中", 1), "…");
    assert_eq!(App::preview_for_status("é中", 0), "…");
}

#[test]
fn objective_runtime_reads_normalized_environment() {
    if run_isolated("objective_runtime_reads_normalized_environment") {
        return;
    }

    // The retry and enforcer keys are shared with the rest of the crate.
    // Run the environment cases in a separate copy of the test binary.
    const CHILD: &str = "HERMES_OBJECTIVE_RUNTIME_COVERAGE_CHILD";
    if std::env::var(CHILD).ok().as_deref() != Some("1") {
        let (_, module) = module_path!()
            .split_once("::")
            .expect("this test belongs to a child module");
        let test = format!("{module}::objective_runtime_reads_normalized_environment");
        let output = std::process::Command::new(
            std::env::current_exe().expect("locate the current test binary"), // nosemgrep: rust.lang.security.current-exe.current-exe
        )
        .args(["--exact", test.as_str(), "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .expect("run the isolated environment test");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("running 1 test"),
            "isolated test exited with {}\nstdout:\n{stdout}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let flag = SavedEnv::capture("HERMES_TEST_OBJECTIVE_RUNTIME_FLAG");
    flag.set("original");
    assert!(!App::set_env_if_changed(flag.key, " \t\n "));
    assert_eq!(std::env::var(flag.key).as_deref().ok(), Some("original"));
    assert!(App::set_env_if_changed(flag.key, " \tTrUe\n "));
    assert_eq!(std::env::var(flag.key).as_deref().ok(), Some("TrUe"));
    assert!(!App::set_env_if_changed(flag.key, " TrUe "));
    assert_eq!(std::env::var(flag.key).as_deref().ok(), Some("TrUe"));

    for value in [" 1 ", " TrUe ", " YES ", " On "] {
        flag.set(value);
        assert_eq!(App::bool_env(flag.key), Some(true), "{value:?}");
    }
    for value in [" 0 ", " FaLsE ", " NO ", " Off "] {
        flag.set(value);
        assert_eq!(App::bool_env(flag.key), Some(false), "{value:?}");
    }
    flag.set(" maybe ");
    assert_eq!(App::bool_env(flag.key), None);
    std::env::remove_var(flag.key);
    assert_eq!(App::bool_env(flag.key), None);

    let auth = SavedEnv::capture("HERMES_AUTH_REFRESH_MAX_RETRIES");
    let quorum = SavedEnv::capture("HERMES_QUORUM_VOTER_MAX_RETRIES");
    auth.set("9");
    for value in [" Off ", " UnLiMiTeD ", " infinite ", " MAX "] {
        quorum.set(value);
        assert_eq!(App::quorum_voter_retry_limit(), 16, "{value:?}");
    }
    quorum.set(" 1 ");
    assert_eq!(App::quorum_voter_retry_limit(), 2);
    quorum.set(" 4 ");
    assert_eq!(App::quorum_voter_retry_limit(), 4);
    for value in ["0", "invalid"] {
        quorum.set(value);
        assert_eq!(App::quorum_voter_retry_limit(), 9, "{value:?}");
    }
    auth.set("1");
    assert_eq!(App::quorum_voter_retry_limit(), 6);

    let enforcer = SavedEnv::capture("HERMES_OBJECTIVE_EXECUTION_ENFORCER");
    for value in [" 0 ", " FaLsE ", " OFF ", " No "] {
        enforcer.set(value);
        assert!(!App::objective_execution_enforcer_enabled(), "{value:?}");
    }
    enforcer.set(" yes ");
    assert!(App::objective_execution_enforcer_enabled());
    std::env::remove_var(enforcer.key);
    assert!(App::objective_execution_enforcer_enabled());
}

#[test]
fn transient_retry_classification_checks_messages_across_error_variants() {
    if run_isolated("transient_retry_classification_checks_messages_across_error_variants") {
        return;
    }

    for error in [
        AgentError::LlmApi("RATE LIMIT exceeded".into()),
        AgentError::Config("Connection REFUSED while loading settings".into()),
        AgentError::ToolExecution("Worker TIMED OUT".into()),
        AgentError::Gateway("Upstream returned HTTP 502".into()),
        AgentError::AuthFailed("Provider REJECTED the request".into()),
        AgentError::Io("Connection RESET by peer".into()),
    ] {
        assert!(App::is_transient_retryable_error(&error), "{error:?}");
    }

    for error in [
        AgentError::Config("Unknown option".into()),
        AgentError::AuthFailed("Invalid credentials".into()),
        AgentError::Io("Permission denied".into()),
    ] {
        assert!(!App::is_transient_retryable_error(&error), "{error:?}");
    }
}

#[test]
fn status_only_output_distinguishes_blank_output_from_execution_evidence() {
    if run_isolated("status_only_output_distinguishes_blank_output_from_execution_evidence") {
        return;
    }

    assert!(App::looks_like_status_only_output(" \n\t "));
    assert!(App::looks_like_status_only_output("I WILL check the file."));
    assert!(!App::looks_like_status_only_output(
        "Verified the result: exit code 0. Next I will summarize it."
    ));
}

#[cfg(unix)]
#[test]
fn objective_pid_probe_reports_the_current_process_alive() {
    if run_isolated("objective_pid_probe_reports_the_current_process_alive") {
        return;
    }

    assert!(App::objective_pid_is_alive(std::process::id()));
}

#[cfg(target_os = "linux")]
#[test]
fn objective_pid_probe_rejects_a_pid_above_the_kernel_limit() {
    if run_isolated("objective_pid_probe_rejects_a_pid_above_the_kernel_limit") {
        return;
    }

    // Linux cannot allocate a PID this large, even with pid_max raised.
    assert!(!App::objective_pid_is_alive(0x7fff_ffff));
}

/// Re-runs one test of this module alone in a child test process with the isolation marker set,
/// because these tests change process-wide state. Returns true in the parent (which then returns)
/// once the child passed, and false inside the child, which runs the test body.
fn run_isolated(test: &str) -> bool {
    if std::env::var_os("HERMES_COVERAGE_GEN_ISOLATED").is_some() {
        return false;
    }
    let path = format!("{}::{test}", module_path!());
    let name = path.split_once("::").map_or(path.as_str(), |(_, rest)| rest);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary")) // nosemgrep: rust.lang.security.current-exe.current-exe
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
    true
}
