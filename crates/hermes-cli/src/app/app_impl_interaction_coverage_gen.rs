use super::*;

struct HermesHomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HermesHomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let previous = std::env::var_os("HERMES_HOME");
        std::env::set_var("HERMES_HOME", path);
        Self { previous }
    }
}

impl Drop for HermesHomeGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("HERMES_HOME", value),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }
}

async fn make_app() -> App {
    let cli = <crate::cli::Cli as clap::Parser>::try_parse_from(["hermes"])
        .expect("default cli args should parse");
    App::new(cli).await.expect("app should construct")
}

#[tokio::test]
async fn interaction_surface_behaviour() {
    if run_isolated("interaction_surface_behaviour") {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let _guard = HermesHomeGuard::set(tmp.path());

    let mut app = make_app().await;

    // run_interactive returns immediately when not running.
    app.running = false;
    assert!(app.run_interactive().await.is_ok());
    assert!(!app.running);

    // Empty input is a no-op and is not stored in history.
    let history_before = app.input_history.len();
    assert!(app.handle_input("   ").await.is_ok());
    assert_eq!(app.input_history.len(), history_before);

    // Empty command is a no-op.
    assert!(app.handle_command("   ").await.is_ok());
    assert_eq!(app.input_history.len(), history_before);

    // Undo with no user messages yields nothing.
    app.messages.clear();
    assert!(app.undo_last().is_none());
    assert!(app.messages.is_empty());

    // Retry with no user messages is a no-op.
    assert!(app.retry_last().await.is_ok());
    assert!(app.messages.is_empty());

    // Finalizing an empty interrupted session is a no-op.
    app.session_objective = None;
    assert!(app
        .finalize_interrupted_tui_session(None, "test-exit")
        .is_ok());
    assert!(app.messages.is_empty());

    // Empty model names are rejected.
    let err = app
        .try_switch_model("   ")
        .expect_err("empty model must fail");
    assert!(matches!(err, AgentError::Config(_)));
    assert!(err.to_string().contains("model cannot be empty"));

    // A failed model switch keeps the previous model.
    let previous_model = app.current_model.clone();
    app.force_model_rebuild_failure_for_test("openai:coverage-fail-model");
    app.switch_model("openai:coverage-fail-model");
    assert_eq!(app.current_model, previous_model);

    // Personality switch updates state.
    app.switch_personality("pirate");
    assert_eq!(app.current_personality.as_deref(), Some("pirate"));

    // Passive auth verification reports a structured summary.
    let report = app
        .verify_runtime_auth(false)
        .await
        .expect("verify auth should succeed");
    assert!(report.starts_with("Auth verify\nprovider: "));
    assert!(report.contains("mode: passive"));
    assert!(report.contains("credential: "));
    assert!(report.contains(&format!("model: {}", app.current_model)));

    // Slash quit via handle_command stops the app.
    app.running = true;
    assert!(app.handle_command("/quit").await.is_ok());
    assert!(!app.running);

    // Command without leading slash is routed through handle_input.
    app.running = true;
    let before = app.input_history.len();
    assert!(app.handle_command("quit").await.is_ok());
    assert!(!app.running);
    assert_eq!(app.input_history.len(), before + 1);
    assert_eq!(app.input_history.last().map(String::as_str), Some("/quit"));
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
    true
}
