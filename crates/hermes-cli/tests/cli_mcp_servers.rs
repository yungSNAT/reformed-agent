//! Stdio MCP servers listed in config.yaml must contribute their tools to CLI
//! sessions. This binary is both the test and, started with `SERVE_FLAG`, the
//! MCP server the test configures, so it runs without the libtest harness.

use std::io::{BufRead, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use clap::Parser;
use hermes_cli::app::App;
use hermes_cli::cli::Cli;
use serde_json::{json, Value};

const SERVE_FLAG: &str = "--serve-mcp-fixture";
const FIXTURE_REPLY: &str = "hello, friend";
/// The fixture's `ping` tool as registered from the `fixture` server.
const SESSION_TOOL: &str = "mcp_fixture_ping";

/// Read one message in the stdio client's `Content-Length` framing.
fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Serve one `ping` tool over stdio, answering each request in turn. One
/// reader lives for the whole session: the client sends its `initialized`
/// notification and `tools/list` back to back. The client clears the child
/// environment, so the marker path arrives as an argument.
fn serve_fixture(marker: &str) {
    std::fs::write(marker, b"started").expect("write fixture marker");
    let mut reader = std::io::stdin().lock();
    let mut writer = std::io::stdout().lock();
    while let Some(request) = read_message(&mut reader) {
        let Some(id) = request.get("id").cloned() else {
            continue; // a notification
        };
        let mut reply = json!({"jsonrpc": "2.0", "id": id});
        match request["method"].as_str() {
            Some("initialize") => {
                reply["result"] = json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fixture", "version": "0"}
                });
            }
            Some("tools/list") => {
                reply["result"] = json!({"tools": [{
                    "name": "ping",
                    "description": "Fixture tool",
                    "inputSchema": {"type": "object"}
                }]});
            }
            Some("tools/call") => {
                reply["result"] = json!({"content": [{"type": "text", "text": FIXTURE_REPLY}]});
            }
            _ => reply["error"] = json!({"code": -32601, "message": "method not found"}),
        }
        let body = reply.to_string();
        write!(writer, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("write reply");
        writer.flush().expect("flush reply");
    }
}

/// Configure the fixture as config.yaml lists it on ll (name and command
/// only), with the `mcp_servers.json` record `hermes mcp add` keeps beside it.
fn write_home(home: &Path, marker: &Path) {
    let exe = std::env::current_exe().expect("test binary path");
    let exe = exe.to_str().expect("UTF-8 test binary path");
    // A JSON string is a valid YAML scalar, which keeps Windows paths intact.
    let exe_yaml = serde_json::to_string(exe).expect("quote path");
    std::fs::write(
        home.join("config.yaml"),
        format!(
            r#"
model: local-test:deterministic
llm_providers:
  local-test:
    base_url: http://127.0.0.1:9/v1
    discover_models: false
    models:
      deterministic: {{}}
mcp_servers:
  - name: fixture
    command: {exe_yaml}
"#
        ),
    )
    .expect("write config.yaml");
    let record = json!({
        "fixture": {
            "command": exe,
            "args": [SERVE_FLAG, marker.to_str().expect("UTF-8 marker path")],
        }
    });
    std::fs::write(home.join("mcp_servers.json"), record.to_string())
        .expect("write mcp_servers.json");
}

/// The TUI and the app-runtime one-shot build their session with `App::new`.
fn app_session_registers_configured_server_tools(home: &Path) {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let cli = Cli::parse_from(["hermes", "--config-dir", home.to_str().expect("UTF-8 home")]);
    let app = runtime.block_on(App::new(cli)).expect("session starts");

    assert!(
        app.tool_schemas
            .iter()
            .any(|schema| schema.name == SESSION_TOOL),
        "{SESSION_TOOL} missing from the session's tools"
    );
    let reply = runtime.block_on(app.tool_registry.dispatch_async(SESSION_TOOL, json!({})));
    assert!(reply.contains(FIXTURE_REPLY), "unexpected reply: {reply}");

    // The binary drops its runtime like this when a session ends. On Windows
    // a stderr pipe drained through tokio stalled that drop while the server
    // lived.
    drop(app);
    let bound = Duration::from_secs(20);
    let shutdown = Instant::now();
    runtime.shutdown_timeout(bound);
    assert!(
        shutdown.elapsed() < bound,
        "the session's runtime took over {bound:?} to shut down"
    );
}

/// `-z` without the app runtime builds its session in `handle_cli_chat`. The
/// model call then fails against the closed port, so only the marker the
/// server writes on start-up is checked.
fn oneshot_session_starts_configured_servers(home: &Path, marker: &Path) {
    let _ = std::fs::remove_file(marker);
    let _ = assert_cmd::Command::cargo_bin("hermes")
        .expect("hermes binary")
        .env("HERMES_HOME", home)
        .args(["-z", "reply with 1"])
        .timeout(Duration::from_secs(180))
        .output();
    assert!(
        marker.exists(),
        "`hermes -z` did not start the configured MCP server"
    );
}

const TEST_NAMES: [&str; 2] = [
    "app_session_registers_configured_server_tools",
    "oneshot_session_starts_configured_servers",
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == SERVE_FLAG) {
        serve_fixture(args.get(index + 1).expect("marker path argument"));
        return;
    }
    // Honour libtest listing and name filters (`cargo test -p hermes-cli <name>`).
    if args.iter().any(|arg| arg == "--list") {
        for name in TEST_NAMES {
            println!("{name}: test");
        }
        return;
    }
    // shortcut: a flag's separate value (`--test-threads 4`) reads as a filter;
    // write `--flag=value`, or parse libtest's value flags if that bites.
    let filters: Vec<&str> = args[1..]
        .iter()
        .map(String::as_str)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let selected = |name: &str| filters.is_empty() || filters.iter().any(|f| name.contains(f));
    let [app_test, oneshot_test] = TEST_NAMES;
    if !selected(app_test) && !selected(oneshot_test) {
        return;
    }

    let home = tempfile::tempdir().expect("temp home");
    let marker = home.path().join("fixture-started");
    write_home(home.path(), &marker);
    // App::new reads skills, memory and sessions through HERMES_HOME, and the
    // `-z` child inherits this environment. The rest would block the fixture
    // or skip the code under test. No other threads exist yet.
    std::env::set_var("HERMES_HOME", home.path());
    for key in [
        "HERMES_MCP_SANDBOX_PROFILE",
        "HERMES_ONESHOT_APP_RUNTIME",
        "HERMES_QUORUM_AUTO_ARM",
        "HERMES_QUERY_DISABLE_TOOLS",
    ] {
        std::env::remove_var(key);
    }

    if selected(app_test) {
        app_session_registers_configured_server_tools(home.path());
        println!("{app_test}: ok");
    }
    if selected(oneshot_test) {
        oneshot_session_starts_configured_servers(home.path(), &marker);
        println!("{oneshot_test}: ok");
    }
}
