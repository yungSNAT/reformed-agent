use super::{
    command_on_path, handle_cli_mcp, SENTRUX_MCP_ARG, SENTRUX_MCP_COMMAND, SENTRUX_MCP_SERVER_NAME,
    UNREAL_MCP_SERVER_NAME, UNREAL_MCP_URL,
};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const CHILD_HOME: &str = "HERMES_COVERAGE_MCP_CHILD_HOME";
const ACTION: &str = "HERMES_COVERAGE_MCP_ACTION";
const NAME: &str = "HERMES_COVERAGE_MCP_NAME";
const SERVER: &str = "HERMES_COVERAGE_MCP_SERVER";
const URL: &str = "HERMES_COVERAGE_MCP_URL";
const COMMAND: &str = "HERMES_COVERAGE_MCP_COMMAND";
const PARALLEL: &str = "HERMES_COVERAGE_MCP_PARALLEL";
const SERVERS_FILE: &str = "mcp_servers.json";
const AUTH_FILE: &str = "mcp_auth.json";

struct McpSandbox {
    root: TempDir,
    bin: PathBuf,
}

impl McpSandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        Self { root, bin }
    }

    fn path(&self, file: &str) -> PathBuf {
        self.root.path().join(file)
    }

    fn cli(&self, action: Option<&str>) -> Command {
        // Re-execute only the worker to isolate the home, PATH, tokens, and stdio.
        let module = module_path!().split_once("::").unwrap().1;
        let mut child = Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
        child
            .args(["--ignored", "--exact", "--nocapture", "--quiet"])
            .arg(format!("{module}::coverage_mcp_child"))
            .current_dir(self.root.path())
            .stdin(Stdio::null())
            .env(CHILD_HOME, self.root.path())
            .env("HERMES_HOME", self.root.path())
            .env("HOME", self.root.path())
            .env("USERPROFILE", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path())
            .env("XDG_DATA_HOME", self.root.path())
            .env("XDG_CACHE_HOME", self.root.path())
            .env("APPDATA", self.root.path())
            .env("LOCALAPPDATA", self.root.path())
            .env("PATH", std::env::join_paths([&self.bin]).unwrap());
        for key in [ACTION, NAME, SERVER, URL, COMMAND, PARALLEL] {
            child.env_remove(key);
        }
        child.env_remove("MCP_WORKER_TOKEN");
        child.env_remove("MCP_PIPE_TOKEN_TOKEN");
        if let Some(action) = action {
            child.env(ACTION, action);
        }
        child
    }

    fn write_json(&self, file: &str, value: &Value) {
        fs::write(self.path(file), serde_json::to_vec(value).unwrap()).unwrap();
    }

    fn read_json(&self, file: &str) -> Value {
        serde_json::from_slice(&fs::read(self.path(file)).unwrap()).unwrap()
    }

    fn yaml_servers(&self) -> Vec<hermes_config::McpServerEntry> {
        hermes_config::load_user_config_file(&self.path("config.yaml"))
            .unwrap()
            .mcp_servers
    }
}

#[tokio::test]
#[ignore = "run by the MCP coverage tests with an isolated home and stdio"]
async fn coverage_mcp_child() {
    let Some(home) = std::env::var_os(CHILD_HOME) else {
        return;
    };
    assert_eq!(
        hermes_config::hermes_home().canonicalize().unwrap(),
        Path::new(&home).canonicalize().unwrap()
    );

    match handle_cli_mcp(
        std::env::var(ACTION).ok(),
        std::env::var(NAME).ok(),
        std::env::var(SERVER).ok(),
        std::env::var(URL).ok(),
        std::env::var(COMMAND).ok(),
        std::env::var_os(PARALLEL).is_some(),
    )
    .await
    {
        Ok(()) => eprintln!("coverage-result: ok"),
        Err(hermes_core::AgentError::Config(message)) => {
            eprintln!("coverage-result: config: {message}");
        }
        Err(hermes_core::AgentError::Io(message)) => {
            eprintln!("coverage-result: io: {message}");
        }
        Err(error) => panic!("unexpected MCP error: {error}"),
    }
}

fn run_child(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "MCP test worker failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn ok_output(command: &mut Command) -> Output {
    let output = run_child(command);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.lines().any(|line| line == "coverage-result: ok"),
        "MCP action did not succeed: {stderr}"
    );
    output
}

fn expect_ok(command: &mut Command, expected_stdout: &[&str]) {
    let output = ok_output(command);
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in expected_stdout {
        assert!(
            stdout.contains(expected),
            "expected {expected:?} in MCP output:\n{stdout}"
        );
    }
}

fn expect_error(command: &mut Command, expected: &str) {
    let output = run_child(command);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let prefix = format!("coverage-result: {expected}");
    assert!(
        stderr.lines().any(|line| line.starts_with(&prefix)),
        "expected {prefix:?} in MCP error output:\n{stderr}"
    );
}

#[test]
fn mcp_missing_arguments_and_missing_config() {
    let sandbox = McpSandbox::new();
    expect_ok(&mut sandbox.cli(None), &["No MCP servers configured ("]);
    expect_error(
        sandbox.cli(Some("add")).env(SERVER, "  "),
        "config: Missing server.",
    );
    expect_error(
        sandbox
            .cli(Some("add"))
            .env(NAME, "worker")
            .env(URL, " ")
            .env(COMMAND, " "),
        "config: mcp add with positional name requires --url or --command",
    );

    for action in ["remove", "test", "configure", "login"] {
        expect_error(
            &mut sandbox.cli(Some(action)),
            &format!("config: Missing server name. Usage: hermes mcp {action} <name>"),
        );
    }

    for (action, expected) in [
        ("remove", "No MCP config to modify."),
        ("test", "No MCP config found."),
        ("configure", "No MCP config found. Add a server first"),
    ] {
        expect_ok(sandbox.cli(Some(action)).env(SERVER, "worker"), &[expected]);
    }
    expect_error(
        sandbox.cli(Some("login")).env(NAME, "worker"),
        "config: No MCP config found at ",
    );
    expect_ok(
        &mut sandbox.cli(Some("not-an-action")),
        &[
            "MCP action 'not-an-action' is not recognized.",
            "Available actions: list, add, remove",
        ],
    );
    assert!(!sandbox.path(SERVERS_FILE).exists());
    assert!(!sandbox.path(AUTH_FILE).exists());
    assert!(!sandbox.path("config.yaml").exists());

    sandbox.write_json(SERVERS_FILE, &json!({}));
    expect_ok(&mut sandbox.cli(None), &["No MCP servers configured."]);
    expect_ok(
        sandbox.cli(Some("test")).env(NAME, "worker"),
        &["Server 'worker' not found in MCP config."],
    );
    expect_ok(
        sandbox.cli(Some("configure")).env(NAME, "worker"),
        &["Server 'worker' not found."],
    );
    expect_ok(
        sandbox.cli(Some("remove")).env(NAME, "worker"),
        &["MCP server 'worker' not found."],
    );
    expect_error(
        sandbox.cli(Some("login")).env(NAME, "worker"),
        "config: MCP server 'worker' is not configured",
    );
    assert_eq!(sandbox.read_json(SERVERS_FILE), json!({}));
    assert!(!sandbox.path(AUTH_FILE).exists());
}

#[test]
fn mcp_add_list_configure_and_test_without_connecting() {
    let sandbox = McpSandbox::new();
    let remote_url = "https://coverage.invalid/mcp";
    let legacy_url = "https://legacy-coverage.invalid/mcp";
    expect_ok(
        sandbox
            .cli(Some("add"))
            .env(NAME, " worker ")
            .env(COMMAND, "coverage-worker")
            .env(PARALLEL, "1"),
        &["MCP server 'worker' added", "Synced MCP server 'worker'"],
    );
    expect_ok(
        sandbox
            .cli(Some("add"))
            .env(NAME, "remote")
            .env(URL, remote_url),
        &["MCP server 'remote' added"],
    );
    expect_ok(
        sandbox
            .cli(Some("add"))
            .env(NAME, " ")
            .env(SERVER, format!(" {legacy_url} ")),
        &["Adding MCP server: https://legacy-coverage.invalid/mcp"],
    );

    let mut servers = sandbox.read_json(SERVERS_FILE);
    assert_eq!(
        servers,
        json!({
            "worker": {
                "command": "coverage-worker",
                "enabled": true,
                "supports_parallel_tool_calls": true
            },
            "remote": {
                "url": remote_url,
                "enabled": true,
                "supports_parallel_tool_calls": false
            },
            (legacy_url): {
                "url": legacy_url,
                "enabled": true,
                "supports_parallel_tool_calls": false
            }
        })
    );

    let yaml = sandbox.yaml_servers();
    assert_eq!(yaml.len(), 3);
    let worker = yaml.iter().find(|entry| entry.name == "worker").unwrap();
    assert_eq!(worker.command.as_deref(), Some("coverage-worker"));
    assert!(worker.url.is_none());
    assert!(worker.supports_parallel_tool_calls);
    for (name, url) in [("remote", remote_url), (legacy_url, legacy_url)] {
        let entry = yaml.iter().find(|entry| entry.name == name).unwrap();
        assert!(entry.command.is_none());
        assert_eq!(entry.url.as_deref(), Some(url));
        assert!(!entry.supports_parallel_tool_calls);
    }

    servers["remote"]["enabled"] = json!(false);
    servers["remote"]["keepalive_interval"] = json!(13);
    sandbox.write_json(SERVERS_FILE, &servers);
    expect_ok(
        &mut sandbox.cli(Some("list")),
        &[
            "MCP servers (",
            "worker",
            "coverage-worker",
            "enabled:on; parallel_tool_calls:on; keepalive:default",
            "enabled:off; parallel_tool_calls:off; keepalive:13s",
            legacy_url,
        ],
    );
    expect_ok(
        sandbox
            .cli(Some("configure"))
            .env(NAME, "worker")
            .env(SERVER, "unused"),
        &[
            "Current config for 'worker':",
            "\"command\": \"coverage-worker\"",
            "to modify settings.",
        ],
    );
    expect_ok(
        sandbox.cli(Some("test")).env(SERVER, "worker"),
        &[
            "Server: worker",
            "Target: coverage-worker",
            "Parallel tool calls: on",
            "Keepalive interval: default",
            "Status: stdio transport (not testable via HTTP)",
        ],
    );
    assert_eq!(sandbox.read_json(SERVERS_FILE), servers);
}

include!("mcp_commands_part2_coverage_gen.rs");
