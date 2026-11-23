//! Tool planning with `mcp_tool_mode: gateway`.

use super::*;

use async_trait::async_trait;
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler};
use hermes_tools::mcp_gateway::{register_mcp_gateway, MCP_GATEWAY_TOOL};

struct NoopTool {
    schema: ToolSchema,
}

#[async_trait]
impl ToolHandler for NoopTool {
    async fn execute(&self, _params: serde_json::Value) -> Result<String, ToolError> {
        Ok("ok".to_string())
    }

    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }
}

fn register(reg: &ToolRegistry, name: &str, toolset: &str) {
    let schema = tool_schema(name, format!("{name} tool"), JsonSchema::new("object"));
    reg.register(
        name,
        toolset,
        schema.clone(),
        Arc::new(NoopTool { schema }),
        Arc::new(|| true),
        Vec::new(),
        true,
        format!("{name} tool"),
        "x",
        None,
    );
}

/// Built-in tools, 2 MCP servers' tools and, when `gateway`, the gateway
/// scoped as the CLI scopes it.
fn registry(cfg: &GatewayConfig, gateway: bool) -> Arc<ToolRegistry> {
    let reg = Arc::new(ToolRegistry::new());
    register(&reg, "terminal", "terminal");
    register(&reg, "read_file", "file");
    register(&reg, "web_search", "web");
    register(&reg, "mcp_github_search", "mcp-github");
    register(&reg, "mcp_docs_search", "mcp-docs");
    register(&reg, "mcp_docs_delete", "mcp-docs");
    if gateway {
        let cfg = cfg.clone();
        register_mcp_gateway(
            &reg,
            &["github".to_string(), "docs".to_string()],
            Arc::new(move |reg: &ToolRegistry| {
                platform_mcp_tool_names(&cfg, "cli", &Arc::new(reg.clone()))
            }),
        );
    }
    reg
}

fn schema_names(cfg: &GatewayConfig, reg: &Arc<ToolRegistry>) -> Vec<String> {
    let mut names: Vec<String> = resolve_platform_tool_schemas(cfg, "cli", reg)
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    names.sort();
    names
}

#[test]
fn gateway_replaces_every_mcp_tool_with_one_tool() {
    let cfg = GatewayConfig::default();
    let names = schema_names(&cfg, &registry(&cfg, true));
    assert!(names.contains(&MCP_GATEWAY_TOOL.to_string()), "{names:?}");
    assert!(names.contains(&"terminal".to_string()));
    assert!(!names.iter().any(|name| name.starts_with("mcp_")), "{names:?}");
}

#[test]
fn direct_mode_is_unchanged() {
    let cfg = GatewayConfig::default();
    let names = schema_names(&cfg, &registry(&cfg, false));
    assert!(names.contains(&"mcp_github_search".to_string()));
    assert!(!names.contains(&MCP_GATEWAY_TOOL.to_string()));
}

#[test]
fn gateway_tool_list_stays_byte_identical_when_mcp_tools_change() {
    let cfg = GatewayConfig::default();
    let reg = registry(&cfg, true);
    let before = serde_json::to_string(&resolve_platform_tool_schemas(&cfg, "cli", &reg)).unwrap();
    register(&reg, "mcp_late_ping", "mcp-late");
    assert!(reg.deregister("mcp_docs_search"));
    let after = serde_json::to_string(&resolve_platform_tool_schemas(&cfg, "cli", &reg)).unwrap();
    assert_eq!(before, after);
    let scope = platform_mcp_tool_names(&cfg, "cli", &reg);
    assert!(scope.contains("mcp_late_ping"));
    assert!(!scope.contains("mcp_docs_search"));
}

#[test]
fn gateway_scope_is_the_mcp_tools_direct_mode_would_list() {
    let mut cfg = GatewayConfig::default();
    cfg.tools_config.disabled = vec!["mcp_docs_delete".to_string()];
    cfg.agent.disabled_toolsets = vec!["mcp-github".to_string()];
    let reg = registry(&cfg, true);
    let scope = platform_mcp_tool_names(&cfg, "cli", &reg);
    assert_eq!(scope, ["mcp_docs_search".to_string()].into());
}

#[test]
fn disabling_the_gateway_hides_it_and_every_mcp_tool() {
    for disable in ["tools", "toolsets"] {
        let mut cfg = GatewayConfig::default();
        if disable == "tools" {
            cfg.tools_config.disabled = vec![MCP_GATEWAY_TOOL.to_string()];
        } else {
            cfg.agent.disabled_toolsets = vec![MCP_GATEWAY_TOOL.to_string()];
        }
        let names = schema_names(&cfg, &registry(&cfg, true));
        assert!(names.contains(&"terminal".to_string()), "{disable}");
        assert!(
            !names.iter().any(|name| name.starts_with("mcp")),
            "{disable}: {names:?}"
        );
    }
}

#[test]
fn custom_toolsets_get_the_gateway_only_with_mcp_tools() {
    let mut cfg = GatewayConfig::default();
    cfg.platform_toolsets
        .insert("cli".to_string(), vec!["web".to_string()]);
    assert_eq!(schema_names(&cfg, &registry(&cfg, true)), ["web_search"]);

    cfg.platform_toolsets.insert(
        "cli".to_string(),
        vec!["web".to_string(), "mcp-docs".to_string()],
    );
    let reg = registry(&cfg, true);
    assert_eq!(schema_names(&cfg, &reg), ["mcp", "web_search"]);
    let scope = platform_mcp_tool_names(&cfg, "cli", &reg);
    assert_eq!(
        scope,
        ["mcp_docs_delete".to_string(), "mcp_docs_search".to_string()].into()
    );
}

#[test]
fn unresolved_toolsets_fall_back_without_mcp_server_tools() {
    let mut cfg = GatewayConfig::default();
    cfg.platform_toolsets
        .insert("cli".to_string(), vec!["no_such_toolset".to_string()]);
    let names = schema_names(&cfg, &registry(&cfg, true));
    assert!(names.contains(&"terminal".to_string()), "{names:?}");
    assert!(!names.iter().any(|name| name.starts_with("mcp_")), "{names:?}");
}
