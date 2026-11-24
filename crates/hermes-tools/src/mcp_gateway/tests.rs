use super::*;

use std::sync::Mutex;

use hermes_core::tool_schema;
use serde_json::json;

use crate::tool_policy::{ToolPolicyEngine, ToolPolicyMode};

/// Records the arguments of each call and answers with the tool's name.
struct EchoTool {
    schema: ToolSchema,
    calls: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl ToolHandler for EchoTool {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        self.calls.lock().unwrap().push(params);
        Ok(json!({ "called": self.schema.name }).to_string())
    }

    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }
}

fn register(
    registry: &ToolRegistry,
    name: &str,
    toolset: &str,
    description: &str,
) -> Arc<Mutex<Vec<Value>>> {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let schema = tool_schema(name, description, JsonSchema::new("object"));
    registry.register(
        name,
        toolset,
        schema.clone(),
        Arc::new(EchoTool {
            schema,
            calls: Arc::clone(&calls),
        }),
        Arc::new(|| true),
        Vec::new(),
        true,
        description,
        "x",
        None,
    );
    calls
}

/// A registry with 2 MCP servers' tools, one built-in tool and the gateway,
/// whose scope is every MCP tool except `mcp_docs_delete`.
fn gateway_registry() -> ToolRegistry {
    let registry = ToolRegistry::new();
    register(&registry, "terminal", "terminal", "Run a shell command");
    register(
        &registry,
        "mcp_github_search_issues",
        "mcp-github",
        "Search GitHub issues",
    );
    register(
        &registry,
        "mcp_github_create_issue",
        "mcp-github",
        "Create a GitHub issue",
    );
    register(
        &registry,
        "mcp_docs_search",
        "mcp-docs",
        "Search the documentation",
    );
    register(&registry, "mcp_docs_delete", "mcp-docs", "Delete a page");
    register_mcp_gateway(
        &registry,
        &["github".to_string(), "docs".to_string()],
        Arc::new(|registry: &ToolRegistry| {
            registry
                .list_tools()
                .into_iter()
                .filter(|tool| is_mcp_server_tool(registry, &tool.name))
                .map(|tool| tool.name)
                .filter(|name| name != "mcp_docs_delete")
                .collect()
        }),
    );
    registry
}

async fn gateway(registry: &ToolRegistry, params: Value) -> Value {
    serde_json::from_str(&registry.dispatch_async(MCP_GATEWAY_TOOL, params).await)
        .expect("gateway answers JSON")
}

#[test]
fn registers_one_gateway_tool_naming_the_servers() {
    let registry = gateway_registry();
    assert!(is_mcp_gateway_registered(&registry));
    let tool = registry
        .get_tool(MCP_GATEWAY_TOOL)
        .expect("gateway registered");
    assert_eq!(tool.toolset, MCP_GATEWAY_TOOLSET);
    let schema = registry
        .get_definitions()
        .into_iter()
        .find(|schema| schema.name == MCP_GATEWAY_TOOL)
        .expect("gateway schema listed");
    assert!(schema.description.contains("docs, github"));
    let properties = schema.parameters.properties.expect("object schema");
    assert!(properties.contains_key("action"));
    assert!(properties.contains_key("tool"));
    assert!(properties.contains_key("arguments"));
}

#[test]
fn gateway_is_not_an_mcp_server_tool_and_absent_by_default() {
    let registry = gateway_registry();
    assert!(!is_mcp_server_tool(&registry, MCP_GATEWAY_TOOL));
    assert!(!is_mcp_server_tool(&registry, "terminal"));
    assert!(is_mcp_server_tool(&registry, "mcp_docs_search"));
    assert!(!is_mcp_gateway_registered(&ToolRegistry::new()));
}

#[tokio::test]
async fn search_ranks_matching_tools_and_returns_their_schemas() {
    let registry = gateway_registry();
    let found = gateway(
        &registry,
        json!({"action": "search", "query": "github issue"}),
    )
    .await;
    let names: Vec<&str> = found["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["mcp_github_create_issue", "mcp_github_search_issues"]
    );
    assert_eq!(found["total"], 2);
    assert_eq!(found["tools"][0]["parameters"]["type"], "object");
}

#[tokio::test]
async fn search_without_a_query_lists_the_scope_only() {
    let registry = gateway_registry();
    let found = gateway(&registry, json!({"action": "search", "limit": 2})).await;
    assert_eq!(
        found["total"], 3,
        "built-in tools and out-of-scope tools are not listed"
    );
    assert_eq!(found["tools"].as_array().unwrap().len(), 2);
    let all = gateway(&registry, json!({"action": "search"}))
        .await
        .to_string();
    assert!(!all.contains("terminal"));
    assert!(!all.contains("mcp_docs_delete"));
    assert!(!all.contains("\"name\":\"mcp\""));
}

#[tokio::test]
async fn call_runs_the_named_mcp_tool_with_its_arguments() {
    let registry = ToolRegistry::new();
    let calls = register(&registry, "mcp_docs_search", "mcp-docs", "Search the docs");
    register_mcp_gateway(
        &registry,
        &["docs".to_string()],
        Arc::new(|_: &ToolRegistry| ["mcp_docs_search".to_string()].into()),
    );
    let out = gateway(
        &registry,
        json!({"action": "call", "tool": "mcp_docs_search", "arguments": {"q": "cache"}}),
    )
    .await;
    assert_eq!(out["called"], "mcp_docs_search");
    let out = gateway(
        &registry,
        json!({"action": "call", "tool": "mcp_docs_search", "arguments": "{\"q\": \"ttl\"}"}),
    )
    .await;
    assert_eq!(out["called"], "mcp_docs_search");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![json!({"q": "cache"}), json!({"q": "ttl"})]
    );
}

#[tokio::test]
async fn call_refuses_tools_outside_the_scope() {
    let registry = gateway_registry();
    for tool in ["terminal", "mcp_docs_delete", "mcp", "mcp_missing_tool"] {
        let out = gateway(&registry, json!({"action": "call", "tool": tool})).await;
        assert!(
            out["error"].as_str().is_some_and(|e| e.contains("search")),
            "{tool}: {out}"
        );
    }
}

#[tokio::test]
async fn call_keeps_the_inner_tools_policy() {
    let registry = gateway_registry();
    registry.set_policy(
        ToolPolicyEngine::new(ToolPolicyMode::Enforce).with_denylist(&["mcp_docs_search"]),
    );
    let out = gateway(
        &registry,
        json!({"action": "call", "tool": "mcp_docs_search"}),
    )
    .await;
    assert_eq!(out["policy"]["tool"], "mcp_docs_search", "{out}");
}

#[tokio::test]
async fn bad_actions_and_missing_names_are_errors() {
    let registry = gateway_registry();
    for params in [
        json!({}),
        json!({"action": "list"}),
        json!({"action": "call"}),
    ] {
        let out = gateway(&registry, params.clone()).await;
        assert!(out["error"].is_string(), "{params}: {out}");
    }
}

#[test]
fn the_gateway_does_not_keep_its_registry_alive() {
    let registry = gateway_registry();
    let weak = registry.downgrade();
    drop(registry);
    assert!(weak.upgrade().is_none());
}

/// Answers every call with the same text.
struct TextTool(String);

#[async_trait]
impl ToolHandler for TextTool {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        Ok(self.0.clone())
    }

    fn schema(&self) -> ToolSchema {
        tool_schema("text", "text", JsonSchema::new("object"))
    }
}

fn text_registry(registry: ToolRegistry, text: &str) -> ToolRegistry {
    registry.register(
        "mcp_docs_page",
        "mcp-docs",
        tool_schema("mcp_docs_page", "A page", JsonSchema::new("object")),
        Arc::new(TextTool(text.to_string())),
        Arc::new(|| true),
        Vec::new(),
        true,
        "A page",
        "x",
        None,
    );
    register_mcp_gateway(
        &registry,
        &["docs".to_string()],
        Arc::new(|_: &ToolRegistry| ["mcp_docs_page".to_string()].into()),
    );
    registry
}

async fn call_page(registry: &ToolRegistry) -> String {
    let out = gateway(registry, json!({"action": "call", "tool": "mcp_docs_page"})).await;
    out["result"].as_str().expect("text result").to_string()
}

#[tokio::test]
async fn a_cut_result_reports_what_the_inner_tool_lost() {
    let registry = text_registry(ToolRegistry::with_max_result_size(100), &"x".repeat(250));
    let page = call_page(&registry).await;
    assert!(
        page.ends_with("[... truncated 150 characters ...]"),
        "{page}"
    );
}

#[tokio::test]
async fn a_one_shot_raw_request_reaches_the_inner_tool() {
    let registry = text_registry(ToolRegistry::new(), "\u{1b}[31mred\u{1b}[0m");
    registry.set_raw_mode_once();
    assert_eq!(call_page(&registry).await, "\u{1b}[31mred\u{1b}[0m");
    assert_eq!(call_page(&registry).await, "red", "the request is spent");
}

#[tokio::test]
async fn rescoping_keeps_the_schema_and_changes_the_reach() {
    let registry = gateway_registry();
    let schema = |registry: &ToolRegistry| {
        registry
            .get_definitions()
            .into_iter()
            .find(|schema| schema.name == MCP_GATEWAY_TOOL)
    };
    let before = schema(&registry);
    assert!(rescope_mcp_gateway(
        &registry,
        Arc::new(|_: &ToolRegistry| HashSet::new())
    ));
    assert_eq!(before, schema(&registry));
    assert!(is_mcp_gateway_registered(&registry));
    let out = gateway(&registry, json!({"action": "search"})).await;
    assert_eq!(out["total"], 0);
    assert!(!rescope_mcp_gateway(
        &ToolRegistry::new(),
        Arc::new(|_: &ToolRegistry| HashSet::new())
    ));
}

#[tokio::test]
async fn another_tool_named_mcp_gets_no_pass_through() {
    let registry = text_registry(ToolRegistry::new(), "page");
    registry.register(
        MCP_GATEWAY_TOOL,
        MCP_GATEWAY_TOOLSET,
        tool_schema(MCP_GATEWAY_TOOL, "impostor", JsonSchema::new("object")),
        Arc::new(TextTool("\u{1b}[31mred\u{1b}[0m".to_string())),
        Arc::new(|| true),
        Vec::new(),
        true,
        "impostor",
        "x",
        None,
    );
    let out: Value =
        serde_json::from_str(&registry.dispatch_async(MCP_GATEWAY_TOOL, json!({})).await)
            .expect("JSON");
    assert_eq!(out["result"], "red", "filtered as any other tool");
}
