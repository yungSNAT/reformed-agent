#[test]
fn mcp_test_rejects_invalid_http_url_during_config_load() {
    let sandbox = McpSandbox::new();
    let servers = json!({
        "invalid-url": {
            "url": "http://127.0.0.1:65536/mcp",
            "supports_parallel_tool_calls": false,
            "keepalive_interval": 17
        }
    });
    sandbox.write_json(SERVERS_FILE, &servers);

    let output = run_child(sandbox.cli(Some("test")).env(NAME, "invalid-url"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.lines().any(|line| {
            line.starts_with("coverage-result: config: invalid MCP config ")
                && line.contains("MCP server 'invalid-url' has invalid url")
                && line.contains("invalid port number")
        }),
        "expected an invalid URL configuration error:\n{stderr}"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Status:"));
    assert_eq!(sandbox.read_json(SERVERS_FILE), servers);
}

#[test]
fn mcp_login_and_remove_update_only_the_selected_server() {
    let sandbox = McpSandbox::new();
    for name in ["worker", "pipe-token"] {
        expect_ok(
            sandbox
                .cli(Some("add"))
                .env(NAME, name)
                .env(COMMAND, "coverage-worker"),
            &["added to"],
        );
    }

    // EOF on redirected stdin is an empty token without a real terminal.
    expect_error(
        sandbox.cli(Some("login")).env(NAME, "pipe-token"),
        "config: Empty token; aborting mcp login",
    );
    assert!(!sandbox.path(AUTH_FILE).exists());

    expect_ok(
        sandbox
            .cli(Some("login"))
            .env(NAME, "worker")
            .env("MCP_WORKER_TOKEN", "  fixture-env-token  "),
        &["Stored MCP auth token for 'worker'"],
    );
    let auth = sandbox.read_json(AUTH_FILE);
    assert_eq!(auth["worker"]["token"], "fixture-env-token");
    assert!(auth["worker"]["updated_at"]
        .as_str()
        .unwrap()
        .parse::<jiff::Timestamp>()
        .is_ok());

    let input = sandbox.path("token-input");
    fs::write(&input, b"  fixture-stdin-token  \n").unwrap();
    expect_ok(
        sandbox
            .cli(Some("login"))
            .env(SERVER, "pipe-token")
            .stdin(Stdio::from(fs::File::open(input).unwrap())),
        &[
            "Token for 'pipe-token':",
            "Stored MCP auth token for 'pipe-token'",
        ],
    );
    let updated_auth = sandbox.read_json(AUTH_FILE);
    assert_eq!(updated_auth["worker"], auth["worker"]);
    assert_eq!(updated_auth["pipe-token"]["token"], "fixture-stdin-token");

    expect_ok(
        sandbox
            .cli(Some("remove"))
            .env(NAME, "worker")
            .env(SERVER, "pipe-token"),
        &["MCP server 'worker' removed."],
    );
    let servers = sandbox.read_json(SERVERS_FILE);
    assert!(servers.get("worker").is_none());
    assert_eq!(servers["pipe-token"]["command"], "coverage-worker");
    let yaml = sandbox.yaml_servers();
    assert_eq!(yaml.len(), 1);
    assert_eq!(yaml[0].name, "pipe-token");
    assert_eq!(
        sandbox.read_json(AUTH_FILE),
        json!({ "pipe-token": updated_auth["pipe-token"] })
    );
}

#[test]
fn mcp_profile_commands_sync_json_and_yaml_and_report_path_status() {
    let sandbox = McpSandbox::new();
    expect_ok(
        &mut sandbox.cli(Some("sentrux-status")),
        &[
            "binary_on_path: no",
            "in_mcp_servers.json: no",
            "in_config.yaml: no",
        ],
    );
    fs::write(sandbox.path(SERVERS_FILE), "{").unwrap();
    expect_ok(
        &mut sandbox.cli(Some("setup-sentrux")),
        &[
            "not currently on PATH",
            "Configured MCP server",
            "Runtime hint",
        ],
    );

    // Detection checks existence; this fixture is never executed.
    fs::write(sandbox.bin.join(SENTRUX_MCP_COMMAND), "").unwrap();
    expect_ok(
        &mut sandbox.cli(Some("sentrux")),
        &["Detected", "Configured MCP server"],
    );
    expect_ok(
        &mut sandbox.cli(Some("sentrux-status")),
        &[
            "binary_on_path: yes",
            "in_mcp_servers.json: yes",
            "in_config.yaml: yes",
        ],
    );

    expect_ok(
        &mut sandbox.cli(Some("unreal-setup")),
        &["Configured MCP server", UNREAL_MCP_URL, "Runtime hint"],
    );
    expect_ok(
        &mut sandbox.cli(Some("unreal-engine-status")),
        &[
            UNREAL_MCP_URL,
            "in_mcp_servers.json: yes",
            "in_config.yaml: yes",
            "parallel_tool_calls: off",
        ],
    );
    let servers = sandbox.read_json(SERVERS_FILE);
    assert_eq!(
        servers[SENTRUX_MCP_SERVER_NAME],
        json!({
            "command": SENTRUX_MCP_COMMAND,
            "args": [SENTRUX_MCP_ARG],
            "enabled": true,
            "supports_parallel_tool_calls": true
        })
    );
    assert_eq!(
        servers[UNREAL_MCP_SERVER_NAME],
        json!({
            "url": UNREAL_MCP_URL,
            "enabled": true,
            "supports_parallel_tool_calls": false,
            "keepalive_interval": 10
        })
    );
    let yaml = sandbox.yaml_servers();
    assert_eq!(yaml.len(), 2);
    let sentrux = yaml
        .iter()
        .find(|entry| entry.name == SENTRUX_MCP_SERVER_NAME)
        .unwrap();
    assert_eq!(
        sentrux.command.as_deref(),
        Some(format!("{SENTRUX_MCP_COMMAND} {SENTRUX_MCP_ARG}").as_str())
    );
    assert!(sentrux.supports_parallel_tool_calls);
    let unreal = yaml
        .iter()
        .find(|entry| entry.name == UNREAL_MCP_SERVER_NAME)
        .unwrap();
    assert_eq!(unreal.url.as_deref(), Some(UNREAL_MCP_URL));
    assert_eq!(unreal.keepalive_interval, Some(10));
    assert!(!unreal.supports_parallel_tool_calls);

    expect_ok(&mut sandbox.cli(Some("sentrux-remove")), &["Removed"]);
    let remaining = sandbox.read_json(SERVERS_FILE);
    assert!(remaining.get(SENTRUX_MCP_SERVER_NAME).is_none());
    assert_eq!(
        remaining[UNREAL_MCP_SERVER_NAME],
        servers[UNREAL_MCP_SERVER_NAME]
    );

    // With only the YAML copy left, both removal commands must leave JSON absent.
    fs::remove_file(sandbox.path(SERVERS_FILE)).unwrap();
    expect_ok(
        &mut sandbox.cli(Some("unreal-engine-status")),
        &["in_mcp_servers.json: no", "in_config.yaml: yes"],
    );
    expect_ok(&mut sandbox.cli(Some("unreal-engine-remove")), &["Removed"]);
    expect_ok(&mut sandbox.cli(Some("sentrux-remove")), &["Removed"]);
    assert!(!sandbox.path(SERVERS_FILE).exists());
    assert!(sandbox.yaml_servers().is_empty());
}

#[test]
fn mcp_config_reads_report_io_errors() {
    let sandbox = McpSandbox::new();
    fs::create_dir(sandbox.path(SERVERS_FILE)).unwrap();
    for action in ["add", "remove", "sentrux", "unreal-engine"] {
        expect_error(
            sandbox
                .cli(Some(action))
                .env(NAME, "worker")
                .env(COMMAND, "coverage-worker"),
            "io: ",
        );
    }
    assert!(sandbox.path(SERVERS_FILE).is_dir());
    assert!(!sandbox.path("config.yaml").exists());

    fs::remove_dir(sandbox.path(SERVERS_FILE)).unwrap();
    sandbox.write_json(
        SERVERS_FILE,
        &json!({ "worker": { "command": "coverage-worker" } }),
    );
    fs::create_dir(sandbox.path(AUTH_FILE)).unwrap();
    expect_error(
        sandbox
            .cli(Some("login"))
            .env(NAME, "worker")
            .env("MCP_WORKER_TOKEN", "fixture-token"),
        "io: ",
    );
    assert!(sandbox.path(AUTH_FILE).is_dir());
}

#[test]
fn mcp_serve_starts_and_returns_on_stdin_eof() {
    let sandbox = McpSandbox::new();
    fs::write(sandbox.path("config.yaml"), "{}\n").unwrap();

    // The worker's null stdin supplies EOF without a terminal or a request
    // whose response could race with transport shutdown.
    let output = ok_output(&mut sandbox.cli(Some("serve")));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line == "Starting Hermes as MCP server on stdio..."),
        "expected stdio server startup message:\n{stderr}"
    );
}

#[test]
fn mcp_path_check_handles_blank_and_explicit_paths() {
    let sandbox = McpSandbox::new();
    let candidate = sandbox.bin.join("coverage-command");
    assert!(!command_on_path(" \t "));
    assert!(!command_on_path(candidate.to_str().unwrap()));
    fs::write(&candidate, "").unwrap();
    assert!(command_on_path(candidate.to_str().unwrap()));
}
