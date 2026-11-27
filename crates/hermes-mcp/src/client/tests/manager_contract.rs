use super::*;
use serde_json::Value;
use std::collections::HashMap;

fn response(id: u32, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn tool_list(name: &str) -> Value {
    json!({"tools":[{"name":name,"inputSchema":{"type":"object"}}]})
}

#[tokio::test]
async fn manager_refreshes_the_registry_and_routes_requests_to_the_connected_server() {
    let registry = Arc::new(hermes_tools::ToolRegistry::new());
    let sent = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(AtomicBool::new(false));
    let resources = json!({"resources":[{"uri":"fixture://notes","name":"Notes"}]});
    let notes = json!({"contents":[{"uri":"fixture://notes","text":"Local notes"}]});
    let prompt = json!({"messages":[{"role":"user","content":"Hello, Ada"}]});
    let transport = FakeTransport::new_with_sent(
        vec![
            response(
                1,
                json!({"protocolVersion":"2024-11-05","capabilities":{},
                    "serverInfo":{"name":"fixture","version":"1"}}),
            ),
            response(2, tool_list("initial")),
            response(3, tool_list("refreshed")),
            response(4, tool_list("current")),
            json!({"jsonrpc":"2.0","id":99,"method":"sampling/createMessage","params":{
                "maxTokens":32,"messages":[{"role":"user","content":{"text":"Local sample"}}]
            }}),
            response(5, json!({"content":[{"type":"text","text":"tool result"}]})),
            response(6, resources.clone()),
            response(7, notes.clone()),
            response(8, json!({"prompts":[{"name":"greet","arguments":[]}]})),
            response(9, prompt),
            response(
                10,
                json!({"tools":[
                    {"name":"probe_one","inputSchema":{"type":"object"}},
                    {"name":"probe_two","inputSchema":{"type":"object"}}
                ]}),
            ),
            response(
                11,
                json!({"resources":[
                    {"uri":"fixture://outline","name":"Outline"},
                    {"uri":"fixture://draft","name":"Draft"}
                ]}),
            ),
            json!({"jsonrpc":"2.0","id":12,"error":{"code":-32603,"message":"probe failed"}}),
            json!({"jsonrpc":"2.0","id":13,"error":{"code":-32602,"message":"invalid fixture input"}}),
        ],
        Arc::clone(&closed),
        Arc::clone(&sent),
    );
    let mut manager = McpManager::new(Arc::clone(&registry));
    manager
        .connect_with_transport_for_test(
            "fixture",
            serde_json::from_value(json!({})).unwrap(),
            Box::new(transport),
        )
        .await
        .unwrap();
    assert!(Arc::ptr_eq(manager.tool_registry(), &registry));
    assert!(manager.is_connected("fixture"));
    assert_eq!(manager.connected_servers(), ["fixture"]);
    assert!(registry.get_tool("mcp_fixture_initial").is_some());
    assert_eq!(
        manager.discover_tools("fixture").await.unwrap()[0].name,
        "refreshed"
    );
    assert!(registry.get_tool("mcp_fixture_initial").is_none());
    assert!(registry.get_tool("mcp_fixture_refreshed").is_some());
    assert_eq!(
        manager.handle_tools_changed("fixture").await.unwrap()[0].name,
        "current"
    );
    assert!(registry.get_tool("mcp_fixture_refreshed").is_none());
    assert!(registry.get_tool("mcp_fixture_current").is_some());

    manager.set_sampling_config(SamplingConfig {
        model: Some("fixture-model".into()),
        max_tokens_cap: 12,
        ..SamplingConfig::default()
    });
    manager.set_sampling_callback(Arc::new(|request| {
        assert_eq!(request["model"], "fixture-model");
        assert_eq!(request["max_tokens"], 12);
        assert_eq!(request["messages"][0]["content"], "Local sample");
        Box::pin(async {
            Ok(json!({
                "model":"fixture-model",
                "choices":[{"message":{"role":"assistant","content":"sample result"},"finish_reason":"stop"}],
                "usage":{"total_tokens":9}
            }))
        })
    }));
    assert_eq!(manager.sampling_metrics("fixture").unwrap().requests, 0);
    assert_eq!(
        manager
            .call_tool("fixture", "current", json!({"tag":"local"}))
            .await
            .unwrap(),
        "tool result"
    );
    let metrics = manager.sampling_metrics("fixture").unwrap();
    assert_eq!((metrics.requests, metrics.tokens_used), (1, 9));

    assert_eq!(
        manager.list_resources("fixture").await.unwrap()[0].uri,
        "fixture://notes"
    );
    assert_eq!(
        manager
            .read_resource("fixture", "fixture://notes")
            .await
            .unwrap(),
        notes
    );
    assert_eq!(
        manager.list_prompts("fixture").await.unwrap()[0].name,
        "greet"
    );
    let prompt = manager
        .get_prompt(
            "fixture",
            "greet",
            HashMap::from([("name".into(), "Ada".into())]),
        )
        .await
        .unwrap();
    assert_eq!(prompt.messages.len(), 1);
    assert_eq!(prompt.messages[0].content, "Hello, Ada");

    let status_before = &manager.get_status()["fixture"];
    assert_eq!(
        (status_before.tool_count, status_before.resource_count),
        (1, 1)
    );
    let probe = manager.probe_server("fixture").await.unwrap();
    assert!(probe.reachable);
    assert_eq!(probe.tools, ["probe_one", "probe_two"]);
    assert_eq!(probe.resources, ["fixture://outline", "fixture://draft"]);
    let status = &manager.get_status()["fixture"];
    assert!(status.connected);
    assert_eq!((status.tool_count, status.resource_count), (2, 2));
    let failed_probe = manager.probe_server("fixture").await.unwrap();
    assert!(!failed_probe.reachable);
    assert!(failed_probe.tools.is_empty() && failed_probe.resources.is_empty());
    assert!(matches!(
        manager.call_tool("fixture", "current", json!({"tag":false})).await,
        Err(McpError::InvalidParams(message)) if message == "invalid fixture input"
    ));

    {
        let sent = sent.lock().unwrap();
        let methods = sent
            .iter()
            .filter_map(|message| message["method"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            methods,
            [
                "initialize",
                "notifications/initialized",
                "tools/list",
                "tools/list",
                "tools/list",
                "tools/call",
                "resources/list",
                "resources/read",
                "prompts/list",
                "prompts/get",
                "tools/list",
                "resources/list",
                "tools/list",
                "tools/call",
            ]
        );
        assert_eq!(
            sent.iter().find(|message| message["id"] == 5).unwrap()["params"],
            json!({"name":"current","arguments":{"tag":"local"}})
        );
        assert_eq!(
            sent.iter().find(|message| message["id"] == 7).unwrap()["params"]["uri"],
            "fixture://notes"
        );
        assert_eq!(
            sent.iter().find(|message| message["id"] == 9).unwrap()["params"],
            json!({"name":"greet","arguments":{"name":"Ada"}})
        );
        assert_eq!(
            sent.iter().find(|message| message["id"] == 99).unwrap()["result"]["content"]["text"],
            "sample result"
        );
    }
    assert!(!closed.load(Ordering::SeqCst));
    manager.disconnect_all().await.unwrap();
    assert!(closed.load(Ordering::SeqCst));
    assert!(!manager.is_connected("fixture"));
    assert!(registry.get_tool("mcp_fixture_current").is_none());
}

#[tokio::test]
async fn manager_operations_report_the_unknown_server_by_name() {
    let mut manager = McpManager::new(Arc::new(hermes_tools::ToolRegistry::new()));
    let name = "not-connected";
    for error in [
        manager.disconnect(name).await.unwrap_err(),
        manager.discover_tools(name).await.unwrap_err(),
        manager.handle_tools_changed(name).await.unwrap_err(),
        manager
            .call_tool(name, "tool", json!({}))
            .await
            .unwrap_err(),
        manager.list_resources(name).await.unwrap_err(),
        manager
            .read_resource(name, "fixture://notes")
            .await
            .unwrap_err(),
        manager.list_prompts(name).await.unwrap_err(),
        manager
            .get_prompt(name, "greet", HashMap::new())
            .await
            .unwrap_err(),
        manager.probe_server(name).await.unwrap_err(),
    ] {
        assert!(matches!(error, McpError::ServerNotFound(missing) if missing == name));
    }
    assert!(manager.sampling_metrics(name).is_none());
    assert!(manager.get_status().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn manager_reconnects_legacy_stdio_and_retries_an_interrupted_read() {
    let dir = TempDir::new().unwrap();
    let calls = dir.path().join("survey-reads");
    let script = r#"
import json
import sys
from pathlib import Path

calls = Path(sys.argv[1])
for line in sys.stdin:
    request = json.loads(line)
    method = request["method"]
    if method == "notifications/initialized":
        continue
    response = {"jsonrpc": "2.0", "id": request["id"]}
    if method == "server/discover":
        response["error"] = {"code": -32601, "message": "Method not found"}
    elif method == "initialize":
        response["result"] = {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "survey-fixture", "version": "1"},
        }
    elif method == "tools/list":
        secondary = "read_summary" if calls.exists() else "read_draft"
        response["result"] = {
            "tools": [{"name": name, "inputSchema": {"type": "object"}}
                      for name in ("read_survey", secondary)]
        }
    elif method == "tools/call":
        assert request["params"]["name"] == "read_survey"
        assert request["params"]["arguments"] == {"site": "estuary"}
        if not calls.exists():
            calls.write_text("interrupted\n")
            break
        with calls.open("a") as record:
            record.write("delivered\n")
        response["result"] = {"content": [{"type": "text", "text": "Survey complete"}]}
    else:
        raise AssertionError(method)
    print(json.dumps(response), flush=True)
"#;
    let mut config = McpServerConfig::stdio(
        "python3",
        vec![
            "-c".into(),
            script.into(),
            calls.to_string_lossy().into_owned(),
        ],
    );
    config.stdio_mode = Some(McpStdioMode::StandardStdio);
    let registry = Arc::new(hermes_tools::ToolRegistry::new());
    let mut manager = McpManager::new(Arc::clone(&registry));
    manager.connect("survey", config).await.unwrap();
    assert!(registry.get_tool("mcp_survey_read_draft").is_some());
    assert!(registry.get_tool("mcp_survey_read_summary").is_none());

    let value = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        manager.call_tool("survey", "read_survey", json!({"site":"estuary"})),
    )
    .await
    .expect("the legacy server should reconnect promptly")
    .unwrap();
    assert_eq!(value, "Survey complete");
    assert!(manager.is_connected("survey"));
    assert!(registry.get_tool("mcp_survey_read_draft").is_none());
    assert!(registry.get_tool("mcp_survey_read_summary").is_some());
    assert_eq!(
        std::fs::read_to_string(calls).unwrap(),
        "interrupted\ndelivered\n"
    );
    manager.disconnect("survey").await.unwrap();
}
