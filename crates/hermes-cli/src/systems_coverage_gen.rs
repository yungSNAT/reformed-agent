use std::thread;
use std::time::Duration;

use super::*;

fn opts(dir: &Path, action: &str, topic: Option<&str>, output: Option<&Path>) -> SystemsCliOptions {
    SystemsCliOptions {
        config_dir: Some(dir.display().to_string()),
        action: Some(action.to_string()),
        topic: topic.map(str::to_string),
        json_only: false,
        output: output.map(|p| p.display().to_string()),
        host: "127.0.0.1".to_string(),
        port: 0,
        once: true,
    }
}

fn read_json(path: &Path) -> Value {
    let raw = fs::read_to_string(path).expect("read output");
    serde_json::from_str(&raw).expect("parse output")
}

fn assert_config_err(result: Result<(), AgentError>, needle: &str) {
    match result {
        Err(AgentError::Config(msg)) => assert!(msg.contains(needle), "unexpected msg: {msg}"),
        other => panic!("expected config error containing {needle}, got {other:?}"),
    }
}

#[test]
fn report_output_writes_nested_output_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out = temp.path().join("a").join("b").join("report.json");
    let value = json!({"kind": "test", "n": 3});
    report_output(&value, false, Some(out.to_str().expect("utf8"))).expect("write");
    assert_eq!(read_json(&out), value);
}

#[test]
fn report_output_ignores_blank_output_path() {
    let value = json!({"kind": "blank"});
    assert!(report_output(&value, false, Some("   ")).is_ok());
}

#[tokio::test]
async fn cli_status_human_writes_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out = temp.path().join("out").join("status.json");
    handle_cli_systems(opts(temp.path(), "status", None, Some(&out)))
        .await
        .expect("status");
    let value = read_json(&out);
    assert_eq!(value["kind"], "hermes.systems.status");
    assert_eq!(value["total"], json!(SYSTEM_COUNT));
}

#[tokio::test]
async fn cli_release_human_writes_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out = temp.path().join("release.json");
    handle_cli_systems(opts(temp.path(), "release", None, Some(&out)))
        .await
        .expect("release");
    let value = read_json(&out);
    assert_eq!(value["kind"], "hermes.systems.release_gate");
    assert!(value["gates"].as_array().is_some_and(|g| g.len() == 8));
}

#[tokio::test]
async fn cli_replay_human_writes_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("state");
    let out = temp.path().join("replay.json");
    handle_cli_systems(opts(state.path(), "replay", None, Some(&out)))
        .await
        .expect("replay");
    let value = read_json(&out);
    assert_eq!(value["kind"], "hermes.replay.report");
    assert_eq!(value["passed"], json!(true));
}

#[tokio::test]
async fn cli_agent_card_show_and_unknown_topic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out = temp.path().join("card.json");
    handle_cli_systems(opts(temp.path(), "card", Some(""), Some(&out)))
        .await
        .expect("card");
    let value = read_json(&out);
    assert_eq!(value["name"], "Reformed Agent");

    let err = handle_cli_systems(opts(temp.path(), "agent-card", Some("bogus"), None)).await;
    assert_config_err(err, "unknown agent-card action 'bogus'");
}

#[tokio::test]
async fn cli_mcp_human_and_unknown_topic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out = temp.path().join("mcp.json");
    handle_cli_systems(opts(temp.path(), "mcp", Some("conformance"), Some(&out)))
        .await
        .expect("mcp");
    let value = read_json(&out);
    assert_eq!(value["kind"], "hermes.mcp.conformance");
    assert_eq!(value["passed"], json!(true));

    let err = handle_cli_systems(opts(temp.path(), "mcp", Some("bogus"), None)).await;
    assert_config_err(err, "unknown MCP action 'bogus'");
}

#[tokio::test]
async fn cli_acp_human_and_unknown_topic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out = temp.path().join("acp.json");
    handle_cli_systems(opts(temp.path(), "acp", None, Some(&out)))
        .await
        .expect("acp");
    let value = read_json(&out);
    assert_eq!(value["kind"], "hermes.acp.conformance");
    assert_eq!(value["passed"], json!(true));

    let err = handle_cli_systems(opts(temp.path(), "acp", Some("bogus"), None)).await;
    assert_config_err(err, "unknown ACP action 'bogus'");
}

#[tokio::test]
async fn cli_providers_human_writes_matrix() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out = temp.path().join("matrix.json");
    handle_cli_systems(opts(temp.path(), "providers", None, Some(&out)))
        .await
        .expect("providers");
    let value = read_json(&out);
    assert_eq!(value["kind"], "hermes.capability_matrix");
    assert!(value["providers"].as_array().is_some());
}

#[tokio::test]
async fn cli_handoff_template_show_and_unknown() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out = temp.path().join("template.json");
    handle_cli_systems(opts(temp.path(), "handoff", Some("template"), Some(&out)))
        .await
        .expect("template");
    assert_eq!(read_json(&out)["kind"], "hermes.typed_handoff_contract");

    let out2 = temp.path().join("handoff.json");
    handle_cli_systems(opts(temp.path(), "handoff", None, Some(&out2)))
        .await
        .expect("handoff show");
    assert_eq!(read_json(&out2)["kind"], "hermes.handoff.report");

    let err = handle_cli_systems(opts(temp.path(), "handoff", Some("bogus"), None)).await;
    assert_config_err(err, "unknown handoff action 'bogus'");
}

#[tokio::test]
async fn cli_provenance_human_and_unknown_topic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let out = temp.path().join("prov.json");
    handle_cli_systems(opts(temp.path(), "provenance", None, Some(&out)))
        .await
        .expect("provenance");
    let value = read_json(&out);
    assert_eq!(value["kind"], "hermes.provenance.report");
    assert_eq!(value["key_exists"], json!(false));

    let err = handle_cli_systems(opts(temp.path(), "provenance", Some("bogus"), None)).await;
    assert_config_err(err, "unknown provenance action 'bogus'");
}

#[tokio::test]
async fn cli_unknown_action_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let err = handle_cli_systems(opts(temp.path(), "nonsense", None, None)).await;
    assert_config_err(err, "unknown systems action 'nonsense'");
}

#[test]
fn file_status_reports_missing_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    assert_eq!(file_status(&temp.path().join("nope")), "missing");
    assert_eq!(source_file_status(Some(temp.path()), "nope.rs"), "missing");
    let report = build_status_report_for_source_root(temp.path(), Some(temp.path()));
    assert_eq!(report.total, SYSTEM_COUNT);
    assert_eq!(report.implemented, SYSTEM_COUNT - 3);
}

#[test]
fn replay_report_empty_when_dir_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let report = build_replay_report(temp.path()).expect("report");
    assert_eq!(report.log_count, 0);
    assert!(report.passed);
    assert!(replay_log_paths(&temp.path().join("missing"))
        .expect("paths")
        .is_empty());
}

#[test]
fn replay_report_detects_parse_errors_and_chain_breaks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = replay_dir(temp.path());
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(
        dir.join("broken.jsonl"),
        concat!(
            "{\"seq\":1,\"prev_hash\":null,\"event_hash\":\"a\"}\n",
            "not json at all\n",
            "\n",
            "{\"seq\":2,\"prev_hash\":\"zzz\",\"event_hash\":\"b\"}\n"
        ),
    )
    .expect("write");
    fs::write(dir.join("ignored.txt"), "garbage").expect("write other");
    let report = build_replay_report(temp.path()).expect("report");
    assert!(!report.passed);
    assert_eq!(report.log_count, 1);
    let log = &report.logs[0];
    assert_eq!(log.entries, 2);
    assert_eq!(log.parse_errors, 1);
    assert_eq!(log.chain_breaks, 1);
    assert_eq!(log.first_seq, Some(1));
    assert_eq!(log.last_seq, Some(2));
    assert!(log.exists);
}

#[test]
fn agent_card_response_health_and_not_found() {
    let (status, content_type, body) = agent_card_response_for_path("/healthz");
    assert_eq!(status, 200);
    assert_eq!(content_type, "application/json");
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["status"], "ok");

    let (status, _, body) = agent_card_response_for_path("/missing");
    assert_eq!(status, 404);
    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["error"], "not_found");
    assert!(parsed["expected"].as_array().is_some_and(|a| a.len() == 3));
}

fn roundtrip_stream(request: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream.write_all(request.as_bytes()).expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        response
    });
    let (mut stream, _) = listener.accept().expect("accept");
    handle_agent_card_stream(&mut stream).expect("handle");
    drop(stream);
    client.join().expect("join client")
}

#[test]
fn handle_agent_card_stream_serves_health() {
    let response = roundtrip_stream("GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("hermes-agent-card"));
    assert!(response.contains("connection: close"));
}

#[test]
fn handle_agent_card_stream_serves_not_found() {
    let response = roundtrip_stream("GET /nope HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");
    assert!(response.contains("not_found"));
}

#[test]
fn serve_agent_card_once_serves_card() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    // A second accept must fail immediately if once mode regresses.
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let addr = listener.local_addr().expect("listener address");
    let timeout = Duration::from_secs(2);
    let mut client = TcpStream::connect_timeout(&addr, timeout).expect("connect");
    client
        .set_read_timeout(Some(timeout))
        .expect("read timeout");
    client
        .set_write_timeout(Some(timeout))
        .expect("write timeout");
    client
        .write_all(b"GET /.well-known/agent.json HTTP/1.1\r\nHost: x\r\n\r\n")
        .expect("write request");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("send EOF");

    serve_agent_card_on(&listener, true).expect("serve one client");
    let mut response = String::new();
    client.read_to_string(&mut response).expect("read response");
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("Reformed Agent"));
}

#[test]
fn fail_builds_failing_check() {
    let check = fail("ping", "boom");
    assert_eq!(check.method, "ping");
    assert_eq!(check.status, "fail");
    assert_eq!(check.detail, "boom");
}

#[test]
fn list_check_fails_on_unexpected_shape() {
    let check = list_check("tools/list", Ok(json!({"other": 1})), "tools");
    assert_eq!(check.status, "fail");
    assert!(check.detail.contains("unexpected response shape"));
}

#[tokio::test]
async fn list_check_fails_on_error_result() {
    let mut server = hermes_mcp::McpServer::new(Arc::new(hermes_tools::ToolRegistry::new()));
    server.add_resource_text(
        hermes_mcp::ResourceInfo {
            uri: "hermes://test/x".to_string(),
            name: "x".to_string(),
            description: None,
            mime_type: Some("text/plain".to_string()),
        },
        "x",
    );
    let result = server.handle_request("tools/call", json!({})).await;
    let expected = result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .expect("tools/call without name should error");
    let check = list_check("tools/call", result, "tools");
    assert_eq!(check.status, "fail");
    assert_eq!(check.detail, expected);
}

#[tokio::test]
async fn acp_error_message_formats_error_and_missing() {
    use hermes_acp::AcpHandler;

    let handler = hermes_acp::DefaultAcpHandler::default();
    let ok = handler
        .handle_request(acp_request(1, "initialize", None))
        .await;
    assert!(ok.result.is_some());
    assert_eq!(acp_error_message(&ok), "missing result");

    let bad = handler
        .handle_request(acp_request(2, "definitely/not-a-method", None))
        .await;
    let err = bad.error.as_ref().expect("unknown method should error");
    assert_eq!(
        acp_error_message(&bad),
        format!("{}: {}", err.code, err.message)
    );
}

#[test]
fn handoff_report_empty_when_queue_missing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let report = build_handoff_report(temp.path()).expect("report");
    assert_eq!(report.pending_requests, 0);
    assert!(report.request_files.is_empty());
    assert_eq!(report.kind, "hermes.handoff.report");
}
