use assert_cmd::Command;
use std::fs;

#[test]
fn e2e_cli_model_command_prints_current_model() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("config.yaml"),
        r#"
model: local-test:deterministic
llm_providers:
  local-test:
    base_url: http://127.0.0.1:9/v1
    discover_models: false
    models:
      deterministic: {}
"#,
    )
    .expect("write config");
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.arg("model");
    cmd.assert().success();
}

#[test]
fn e2e_cli_model_command_lists_configured_custom_provider_models() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("config.yaml"),
        r#"
model: qianfan-coding:kimi-k2.5
llm_providers:
  qianfan-coding:
    base_url: https://qianfan.baidubce.com/v2/coding
    discover_models: false
    models:
      kimi-k2.5: {}
      glm-5: {}
"#,
    )
    .expect("write config");

    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.arg("model");
    let out = cmd.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(
        text.contains("qianfan-coding"),
        "expected custom provider in model list: {text:?}"
    );
    assert!(
        text.contains("kimi-k2.5") && text.contains("glm-5"),
        "expected configured model subset in model list: {text:?}"
    );
}

#[test]
fn e2e_cli_gateway_status_command_runs() {
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.args(["gateway", "status"]);
    cmd.assert().success();
}

#[test]
fn e2e_cli_tools_list_shows_registered_tools() {
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.args(["tools", "list"]);
    let out = cmd.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(
        text.contains("send_message"),
        "expected send_message in tools list: {text:?}"
    );
    assert!(
        text.contains("clarify"),
        "expected clarify in tools list: {text:?}"
    );
}

#[test]
fn e2e_cli_config_set_persists_model_to_yaml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.args(["config", "set", "model", "openai:gpt-4o-mini"]);
    cmd.assert().success();
    let cfg = dir.path().join("config.yaml");
    assert!(cfg.exists(), "config.yaml should be created");
    let raw = fs::read_to_string(&cfg).expect("read config");
    assert!(
        raw.contains("gpt-4o-mini"),
        "expected model in yaml: {}",
        raw
    );
}

#[test]
fn e2e_cli_config_set_dotted_llm_and_get_masks_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let mut set = Command::cargo_bin("hermes").expect("binary exists");
    set.env("HERMES_HOME", home);
    set.args(["config", "set", "llm.openai.api_key", "sk-testkey123456"]);
    set.assert().success();

    let mut get = Command::cargo_bin("hermes").expect("binary exists");
    get.env("HERMES_HOME", home);
    get.args(["config", "get", "llm.openai.api_key"]);
    let out = get.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(
        text.contains("***"),
        "expected masked api key, got: {text:?}"
    );
}

#[test]
fn e2e_cli_expands_tilde_in_hermes_home() {
    let user_home = tempfile::tempdir().expect("user home");
    let cwd = tempfile::tempdir().expect("cwd");
    let hermes_home = user_home.path().join("hermes-e2e");
    fs::create_dir_all(&hermes_home).expect("create hermes home");
    fs::write(
        hermes_home.join("config.yaml"),
        "model: local-test:tilde-home\n",
    )
    .expect("write config");

    // The home is named by the variable, then by a quoted --config-dir.
    for by_flag in [false, true] {
        let mut get = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
        get.current_dir(cwd.path());
        get.env("HOME", user_home.path());
        if by_flag {
            get.env_remove("HERMES_HOME");
            get.args(["--config-dir", "~/hermes-e2e"]);
        } else {
            get.env("HERMES_HOME", "~/hermes-e2e");
        }
        get.args(["config", "get", "model"]);
        let out = get.assert().success().get_output().stdout.clone();
        let text = std::str::from_utf8(&out).expect("utf8");
        assert!(
            text.contains("local-test:tilde-home"),
            "expected the model from ~/hermes-e2e/config.yaml (by flag: {by_flag}): {text:?}"
        );
    }
    assert!(
        !cwd.path().join("~").exists(),
        "a literal `~` directory was created under the working directory"
    );
}

#[test]
fn e2e_cli_config_check_reports_unknown_platform_toolsets() {
    let check = |config: &str, mcp_servers_json: Option<&str>| -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("config.yaml"), config).expect("write config");
        if let Some(json) = mcp_servers_json {
            fs::write(dir.path().join("mcp_servers.json"), json).expect("write mcp config");
        }
        let mut cmd = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
        cmd.env("HERMES_HOME", dir.path());
        cmd.args(["config", "check"]);
        let out = cmd.assert().success().get_output().stdout.clone();
        String::from_utf8(out).expect("utf8")
    };

    // The default selections name only toolsets the runtime resolves.
    let clean = check("model: local-test:deterministic\n", None);
    assert!(
        !clean.contains("platform_toolsets"),
        "default toolsets were reported: {clean:?}"
    );

    let stale = check(
        r#"
model: local-test:deterministic
mcp_servers:
  - name: yaml-server
    command: "true"
platform_toolsets:
  cli:
    - hermes-cli
    - web
    - todo
    - yaml-server
    - mcp_yaml_server_search
    - mcp-json_server
    - retired-toolset
  telegram:
    - hermes-telegram
    - mcp-removed
"#,
        Some(r#"{"servers": [{"name": "json-server", "command": "true"}]}"#),
    );
    for expected in [
        "platform_toolsets.cli names 'retired-toolset'",
        "platform_toolsets.telegram names 'mcp-removed'",
    ] {
        assert!(stale.contains(expected), "missing {expected:?}: {stale:?}");
    }
    assert_eq!(
        stale.matches("platform_toolsets.").count(),
        2,
        "known names were reported: {stale:?}"
    );

    // An unreadable MCP file is named, not passed over.
    let unreadable = check(
        "model: local-test:deterministic\nplatform_toolsets:\n  cli:\n    - json-server\n",
        Some("{not json"),
    );
    assert!(
        unreadable.contains("MCP servers were not read") && unreadable.contains("mcp_servers.json"),
        "the unreadable file was not reported: {unreadable:?}"
    );
}

#[test]
fn e2e_cli_model_switch_warns_on_stale_auxiliary_provider() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();

    let mut set_compression_provider = Command::cargo_bin("hermes").expect("binary exists");
    set_compression_provider.env("HERMES_HOME", home);
    set_compression_provider.args(["config", "set", "auxiliary.compression.provider", "nous"]);
    set_compression_provider.assert().success();

    let mut set_compression_model = Command::cargo_bin("hermes").expect("binary exists");
    set_compression_model.env("HERMES_HOME", home);
    set_compression_model.args(["config", "set", "auxiliary.compression.model", "hermes-4"]);
    set_compression_model.assert().success();

    let mut set_vision_auto = Command::cargo_bin("hermes").expect("binary exists");
    set_vision_auto.env("HERMES_HOME", home);
    set_vision_auto.args(["config", "set", "auxiliary.vision.provider", "auto"]);
    set_vision_auto.assert().success();

    let mut switch = Command::cargo_bin("hermes").expect("binary exists");
    switch.env("HERMES_HOME", home);
    switch.args(["model", "openrouter:anthropic/claude-opus-4.8"]);
    let out = switch.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(
        text.contains("Model switched to: openrouter:anthropic/claude-opus-4.8"),
        "expected model switch output, got: {text:?}"
    );
    assert!(
        text.contains(
            "Warning: 1 auxiliary task (compression=nous/hermes-4) still run on providers other than main 'openrouter'"
        ),
        "expected stale auxiliary warning, got: {text:?}"
    );
    assert!(
        !text.contains("vision"),
        "auto auxiliary provider should not be reported stale: {text:?}"
    );
}

#[test]
fn e2e_cli_profile_list_and_current_show_custom_alias() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();

    let mut create = Command::cargo_bin("hermes").expect("binary exists");
    create.env("HERMES_HOME", home);
    create.args(["profile", "create", "steve", "--no-alias"]);
    create.assert().success();

    let mut alias = Command::cargo_bin("hermes").expect("binary exists");
    alias.env("HERMES_HOME", home);
    alias.args(["profile", "alias", "steve", "--name", "qiaobusi"]);
    alias.assert().success();

    let mut list = Command::cargo_bin("hermes").expect("binary exists");
    list.env("HERMES_HOME", home);
    list.args(["profile", "list"]);
    let list_out = list.assert().success().get_output().stdout.clone();
    let list_text = std::str::from_utf8(&list_out).expect("utf8");
    assert!(
        list_text.contains("  steve (alias: qiaobusi)"),
        "expected custom alias in profile list, got: {list_text:?}"
    );

    let mut use_alias = Command::cargo_bin("hermes").expect("binary exists");
    use_alias.env("HERMES_HOME", home);
    use_alias.args(["profile", "use", "qiaobusi"]);
    use_alias.assert().success();

    let mut current = Command::cargo_bin("hermes").expect("binary exists");
    current.env("HERMES_HOME", home);
    current.arg("profile");
    let current_out = current.assert().success().get_output().stdout.clone();
    let current_text = std::str::from_utf8(&current_out).expect("utf8");
    assert!(
        current_text.contains("Active:      steve"),
        "expected resolved active profile, got: {current_text:?}"
    );
    assert!(
        current_text.contains("Alias:       qiaobusi"),
        "expected custom alias in current profile output, got: {current_text:?}"
    );
}

#[test]
fn e2e_cli_sessions_optimize_runs_against_isolated_home() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.args(["sessions", "optimize"]);
    let out = cmd.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    assert!(
        text.contains("Optimizing session store"),
        "expected optimize banner, got: {text:?}"
    );
    assert!(
        text.contains("Optimized") && text.contains("FTS index"),
        "expected FTS index summary, got: {text:?}"
    );
    assert!(
        text.contains("Database size:"),
        "expected database size summary, got: {text:?}"
    );
    assert!(dir.path().join("sessions.db").exists());
}

#[test]
fn e2e_cli_interactive_without_tty_reports_actionable_diagnostic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.env_remove("HERMES_ALLOW_PARALLEL_INTERACTIVE");
    let assert = cmd.assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("interactive Hermes requires a terminal (TTY)"),
        "expected TTY diagnostic in stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("hermes-ultra chat --query"),
        "expected non-interactive prompt guidance in stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("hermes-ultra doctor --deep --snapshot --bundle"),
        "expected doctor bundle guidance in stderr, got: {stderr:?}"
    );
}

#[test]
fn e2e_cli_resume_refuses_oneshot_prompt() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("config.yaml"),
        r#"
model: local-test:deterministic
llm_providers:
  local-test:
    base_url: http://127.0.0.1:9/v1
    discover_models: false
    models:
      deterministic: {}
"#,
    )
    .expect("write config");
    let mut cmd = Command::cargo_bin("hermes").expect("binary exists");
    cmd.env("HERMES_HOME", dir.path());
    cmd.args(["resume", "latest", "-z", "reply with 1"]);
    let assert = cmd.assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("`resume` cannot be combined with -z/--oneshot"),
        "expected resume/oneshot refusal in stderr, got: {stderr:?}"
    );
}

#[test]
fn e2e_cli_systems_status_and_reports_are_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["systems", "status", "--json"],
        vec!["systems", "release", "--json"],
        vec!["systems", "agent-card", "card", "--json"],
        vec!["systems", "mcp", "conformance", "--json"],
        vec!["systems", "acp", "conformance", "--json"],
        vec!["systems", "providers", "--json"],
        vec!["systems", "handoff", "template", "--json"],
        vec!["systems", "provenance", "--json"],
    ] {
        let mut cmd = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
        cmd.env("HERMES_HOME", dir.path());
        cmd.args(args);
        let out = cmd.assert().success().get_output().stdout.clone();
        let text = std::str::from_utf8(&out).expect("utf8");
        let value: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|err| panic!("json parse failed: {err}; {text}"));
        assert!(
            value.get("kind").is_some() || value.get("name").is_some(),
            "expected report/card json, got {value}"
        );
    }
}

#[test]
fn e2e_cli_systems_replay_reports_existing_trace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let replay_dir = dir.path().join("logs").join("replay");
    fs::create_dir_all(&replay_dir).expect("mkdir replay");
    fs::write(
        replay_dir.join("session.jsonl"),
        concat!(
            "{\"seq\":1,\"event\":\"start\",\"trace_id\":\"t\",\"prev_hash\":null,\"event_hash\":\"a\"}\n",
            "{\"seq\":2,\"event\":\"stop\",\"trace_id\":\"t\",\"prev_hash\":\"a\",\"event_hash\":\"b\"}\n"
        ),
    )
    .expect("write replay");

    let mut show = Command::cargo_bin("hermes-agent-ultra").expect("binary exists");
    show.env("HERMES_HOME", dir.path());
    show.args(["systems", "replay", "--json"]);
    let out = show.assert().success().get_output().stdout.clone();
    let text = std::str::from_utf8(&out).expect("utf8");
    let report: serde_json::Value = serde_json::from_str(text).expect("show json");
    assert_eq!(report.get("log_count").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(report.get("passed").and_then(|v| v.as_bool()), Some(true));
}

#[cfg(unix)]
#[test]
fn e2e_cli_runs_with_one_mib_main_thread_stack() {
    // Windows gives the main thread 1 MiB of stack; `ulimit -s` gives Linux the same.
    let dir = tempfile::tempdir().expect("tempdir");
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("ulimit -s 1024 && exec \"$0\" version")
        .arg(env!("CARGO_BIN_EXE_hermes-agent-ultra"))
        .env("HERMES_HOME", dir.path())
        .output()
        .expect("run hermes-agent-ultra");
    assert!(
        output.status.success(),
        "status: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run `hermes -z <prompt>` with HERMES_HOME `dir` and `env` against a mock
/// `/chat/completions` that answers with `status` and `body`.
async fn run_oneshot_against_mock(
    dir: tempfile::TempDir,
    env: &'static [(&'static str, &'static str)],
    status: u16,
    body: serde_json::Value,
) -> (tempfile::TempDir, std::process::Output) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    fs::write(
        dir.path().join("config.yaml"),
        format!(
            r#"
model: local-test:deterministic
llm_providers:
  local-test:
    base_url: {}
    api_key: test-key
    discover_models: false
    models:
      deterministic: {{}}
"#,
            server.uri()
        ),
    )
    .expect("write config");
    let home = dir.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("hermes")
            .expect("binary exists")
            .current_dir(&home)
            .env("HERMES_HOME", &home)
            .env_remove("HERMES_ONESHOT_APP_RUNTIME")
            .env_remove("HERMES_QUORUM_AUTO_ARM")
            .envs(env.iter().copied())
            .args(["-z", "where did the heron nest"])
            .output()
            .expect("run hermes")
    })
    .await
    .expect("join");
    (dir, output)
}

fn oneshot_reply() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-oneshot-test",
        "object": "chat.completion",
        "created": 0,
        "model": "deterministic",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "under the willow bridge"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_cli_oneshot_saves_session() {
    let (dir, output) = run_oneshot_against_mock(
        tempfile::tempdir().expect("tempdir"),
        &[],
        200,
        oneshot_reply(),
    )
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("under the willow bridge"),
        "stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!stderr.contains("not saved"), "stderr: {stderr}");

    let snapshots: Vec<_> = fs::read_dir(dir.path().join("sessions"))
        .expect("sessions dir")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(snapshots.len(), 1, "snapshots: {snapshots:?}");
    let snapshot: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&snapshots[0]).expect("read snapshot"))
            .expect("parse snapshot");
    assert_eq!(snapshot["source"], "oneshot");
    // The transcript is the agent result, system prompt included, as the App
    // and gateway persist it; the conversation itself is one exchange.
    let conversation: Vec<(&str, &str)> = snapshot["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|m| m["role"] != "System")
        .map(|m| (m["role"].as_str().unwrap(), m["content"].as_str().unwrap()))
        .collect();
    assert_eq!(
        conversation,
        [
            ("User", "where did the heron nest"),
            ("Assistant", "under the willow bridge")
        ]
    );
    let session_id = snapshot["session_info"]["session_id"]
        .as_str()
        .expect("session id")
        .to_string();

    let persistence = hermes_agent::SessionPersistence::new(dir.path());
    let messages = persistence.load_session(&session_id).expect("load session");
    assert_eq!(
        Some(messages.len()),
        snapshot["messages"].as_array().map(Vec::len)
    );
    let conn = rusqlite::Connection::open(persistence.db_path()).expect("open db");
    let (platform, title): (String, Option<String>) = conn
        .query_row(
            "SELECT platform, title FROM sessions WHERE id = ?1",
            [&session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("session row");
    assert_eq!(platform, "oneshot");
    assert_eq!(title, None);
    let hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'willow' AND session_id = ?1",
            [&session_id],
            |row| row.get(0),
        )
        .expect("search");
    assert_eq!(hits, 1, "the reply should be searchable");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_cli_failed_oneshot_saves_no_session() {
    let (dir, output) = run_oneshot_against_mock(
        tempfile::tempdir().expect("tempdir"),
        &[],
        400,
        serde_json::json!({"error": {"message": "bad request", "type": "invalid_request_error"}}),
    )
    .await;
    assert!(!output.status.success());
    assert!(!dir.path().join("sessions").exists());
    // Registering the session_search tool may create an empty sessions.db.
    let db = dir.path().join("sessions.db");
    if db.exists() {
        let conn = rusqlite::Connection::open(&db).expect("open db");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .expect("count sessions");
        assert_eq!(rows, 0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_cli_oneshot_at_snapshot_limit_warns_and_keeps_interactive_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sessions = dir.path().join("sessions");
    fs::create_dir_all(&sessions).expect("sessions dir");
    let interactive = sessions.join("interactive-a.json");
    fs::write(
        &interactive,
        r#"{"session_info":{"session_id":"interactive-a"},"messages":[]}"#,
    )
    .expect("write interactive snapshot");

    let (dir, output) = run_oneshot_against_mock(
        dir,
        &[("HERMES_SESSION_SNAPSHOT_MAX_FILES", "1")],
        200,
        oneshot_reply(),
    )
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("under the willow bridge"));
    assert!(
        stderr.contains("not kept in `sessions list`"),
        "stderr: {stderr}"
    );
    let snapshots: Vec<_> = fs::read_dir(&sessions)
        .expect("sessions dir")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(snapshots, [interactive]);
    let conn = rusqlite::Connection::open(dir.path().join("sessions.db")).expect("open db");
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE platform = 'oneshot'",
            [],
            |row| row.get(0),
        )
        .expect("count sessions");
    assert_eq!(rows, 1, "the run stays searchable");
}
