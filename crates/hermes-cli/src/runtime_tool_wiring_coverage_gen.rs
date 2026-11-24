use super::*;

use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use tracing::instrument::WithSubscriber;

const CLARIFY_CHILD_TEST: &str = "HERMES_TEST_RUNTIME_WIRING_CLARIFY_CHILD";

fn is_clarify_child(test_name: &str) -> bool {
    std::env::var_os(CLARIFY_CHILD_TEST).as_deref() == Some(std::ffi::OsStr::new(test_name))
}

fn run_clarify_child(test_name: &str, stdin: Stdio) -> String {
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let full_name = format!("{module}::{test_name}");
    // Isolate stdin and the auto-answer override from the rest of the test suite.
    let mut child = Command::new(std::env::current_exe().expect("test executable")) // nosemgrep: rust.lang.security.current-exe.current-exe
        .args(["--exact", full_name.as_str(), "--nocapture"])
        .env(CLARIFY_CHILD_TEST, test_name)
        .env_remove("HERMES_CLARIFY_AUTO_ANSWER")
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn clarify test subprocess");

    let deadline = Instant::now() + Duration::from_secs(10);
    let timed_out = loop {
        if child.try_wait().expect("poll clarify subprocess").is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop blocked clarify subprocess");
            break true;
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    let output = child
        .wait_with_output()
        .expect("collect clarify test output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !timed_out && output.status.success() && stdout.contains("1 passed"),
        "{full_name}: timed out = {timed_out}, status = {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    stderr.into_owned()
}

async fn assert_clarify_answer(
    backend: &StdioClarifyBackend,
    question: &str,
    choices: Option<&[String]>,
    answer: &str,
    source: &str,
) {
    let response = backend
        .ask(question, choices)
        .await
        .expect("clarify answer");
    let parsed: serde_json::Value = serde_json::from_str(&response).expect("clarify response JSON");
    assert_eq!(
        parsed,
        json!({
            "type": "clarify_response",
            "question": question,
            "answer": answer,
            "source": source,
        })
    );
}

fn assert_clarify_execution_error(error: hermes_core::ToolError, expected: &str) {
    match error {
        hermes_core::ToolError::ExecutionFailed(message) => assert_eq!(message, expected),
        other => panic!("expected a clarify execution error, got {other:?}"),
    }
}

#[tokio::test]
async fn stdio_clarify_default_falls_back_to_first_choice_or_reports_missing_input() {
    const TEST_NAME: &str =
        "stdio_clarify_default_falls_back_to_first_choice_or_reports_missing_input";
    if !is_clarify_child(TEST_NAME) {
        let stderr = run_clarify_child(TEST_NAME, Stdio::null());
        assert!(!stderr.contains("[clarify]"), "unexpected prompt: {stderr}");
        return;
    }

    assert!(!std::io::stdin().is_terminal());
    let default_backend = <StdioClarifyBackend as Default>::default;
    let backend = default_backend();
    let choices = ["first".to_string(), "second".to_string()];
    assert_clarify_answer(
        &backend,
        "Which option?",
        Some(&choices),
        "first",
        "non_interactive_fallback",
    )
    .await;
    assert_clarify_execution_error(
        backend
            .ask("What next?", None)
            .await
            .expect_err("no terminal, auto-answer, or choices"),
        "clarify requires interactive terminal input or HERMES_CLARIFY_AUTO_ANSWER",
    );
}

#[cfg(target_os = "linux")]
fn private_clarify_pty() -> (std::fs::File, std::fs::File) {
    use std::os::fd::{AsRawFd, FromRawFd};

    let (mut master, mut slave) = (-1, -1);
    // SAFETY: The fd outputs are valid writable pointers. Null optional
    // pointers request the default terminal settings and no device name.
    let result = unsafe {
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(
        result,
        0,
        "open private pty: {}",
        std::io::Error::last_os_error()
    );

    // SAFETY: openpty succeeded and returned two distinct, owned descriptors.
    let (master, slave) = unsafe {
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        (
            std::fs::File::from_raw_fd(master),
            std::fs::File::from_raw_fd(slave),
        )
    };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: Each fd is kept open by its File and F_SETFD takes an integer.
        let result = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) }; // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
        assert_eq!(
            result,
            0,
            "set close-on-exec: {}",
            std::io::Error::last_os_error()
        );
    }
    (master, slave)
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn stdio_clarify_parses_scripted_input_on_a_private_pty() {
    use std::io::Write;

    const TEST_NAME: &str = "stdio_clarify_parses_scripted_input_on_a_private_pty";
    if !is_clarify_child(TEST_NAME) {
        // A disposable pseudoterminal exercises IsTerminal without reading
        // from or writing prompts to the caller's terminal.
        let (mut master, slave) = private_clarify_pty();
        master
            .write_all(b"\n\n 2 \n 99 \n typed reply \n\x04")
            .expect("queue scripted terminal input");
        let stderr = run_clarify_child(TEST_NAME, Stdio::from(slave));
        drop(master);
        assert!(stderr.contains("[clarify] Pick a direction"), "{stderr}");
        assert!(
            stderr.contains("[clarify] choices:\n  1. north\n  2. south"),
            "{stderr}"
        );
        assert!(stderr.contains("[clarify] answer> "), "{stderr}");
        return;
    }

    assert!(std::io::stdin().is_terminal());
    let backend = StdioClarifyBackend::new();
    let choices = ["north".to_string(), "south".to_string()];

    assert_clarify_answer(
        &backend,
        "Pick a direction",
        Some(&choices),
        "north",
        "stdio",
    )
    .await;
    assert_clarify_execution_error(
        backend
            .ask("Answer without choices", None)
            .await
            .expect_err("blank input without choices"),
        "empty clarify response with no fallback choices",
    );
    assert_clarify_answer(&backend, "Pick by number", Some(&choices), "south", "stdio").await;
    assert_clarify_answer(
        &backend,
        "Keep an out-of-range number",
        Some(&choices),
        "99",
        "stdio",
    )
    .await;
    assert_clarify_answer(&backend, "Enter text", None, "typed reply", "stdio").await;
}

#[test]
fn http_launch_records_apply_only_to_matching_urls_and_defer_to_yaml_mode() {
    const URL: &str = "https://catalogue.example.invalid/mcp";
    let records = crate::mcp_config::parse_mcp_config_json(
        &json!({
            "inherited": {"url": URL, "modern_http": false, "env": {"MODE": "record"}},
            "explicit": {"url": URL, "modern_http": false},
            "disabled": {"url": URL, "enabled": false},
            "moved": {
                "url": "https://old.example.invalid/mcp",
                "enabled": false,
                "modern_http": false,
                "env": {"MODE": "stale"},
            },
        })
        .to_string(),
    )
    .expect("valid HTTP launch records");
    let server = |name: &str, modern_http| hermes_config::McpServerEntry {
        name: name.to_string(),
        url: Some(URL.to_string()),
        modern_http,
        ..Default::default()
    };

    let configs = session_mcp_server_configs(
        &[
            server("inherited", None),
            server("explicit", Some(true)),
            server("disabled", None),
            server("moved", None),
        ],
        Some(&records),
    );

    let mut inherited = McpServerConfig::http(URL).with_env("MODE", "record");
    inherited.modern_http = Some(false);
    let mut explicit = McpServerConfig::http(URL);
    explicit.modern_http = Some(true);
    let mut moved = McpServerConfig::http(URL);
    moved.modern_http = None;
    assert_eq!(
        configs,
        vec![
            ("inherited".to_string(), inherited),
            ("explicit".to_string(), explicit),
            ("moved".to_string(), moved),
        ]
    );
}

struct WarningCollector(Arc<Mutex<Vec<String>>>);

impl tracing::Subscriber for WarningCollector {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() == tracing::Level::WARN
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Messages<'a>(&'a mut Vec<String>);

        impl tracing::field::Visit for Messages<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0.push(format!("{value:?}"));
                }
            }
        }

        let mut messages = self.0.lock().expect("warning collector lock");
        event.record(&mut Messages(&mut messages));
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn corrupt_launch_records_are_warned_about_and_targetless_servers_are_skipped() {
    let state = tempfile::tempdir().expect("temporary state root");
    std::fs::write(state.path().join("mcp_servers.json"), b"{")
        .expect("write corrupt launch records");
    let config = GatewayConfig {
        mcp_servers: vec![hermes_config::McpServerEntry {
            name: "targetless".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let registry = Arc::new(ToolRegistry::new());
    let warnings = Arc::new(Mutex::new(Vec::new()));

    connect_configured_mcp_servers(&config, state.path(), &registry)
        .with_subscriber(WarningCollector(Arc::clone(&warnings)))
        .await;

    assert!(registry.list_tools().is_empty());
    let warnings = warnings.lock().expect("captured warnings");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("Ignoring MCP launch records:")),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|warning| {
            warning.contains("Skipping MCP server 'targetless': it has neither 'command' nor 'url'")
        }),
        "{warnings:?}"
    );
}
