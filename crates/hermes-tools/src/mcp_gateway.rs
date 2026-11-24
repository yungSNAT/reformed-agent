//! The `mcp` gateway tool (`mcp_tool_mode: gateway`).
//!
//! The model sees this one tool in place of every MCP server's tools: it
//! searches them, then calls one by its registered name. The tool list sent
//! to the provider then stays the same however many servers connect, fail or
//! change their tools, so the provider's prompt cache keeps hitting.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema};
use serde_json::{json, Value};

use crate::registry::{ToolRegistry, WeakToolRegistry};

/// The gateway's tool name. MCP server tools are `mcp_<server>_<tool>`.
pub const MCP_GATEWAY_TOOL: &str = "mcp";
/// The gateway's toolset. MCP server toolsets are `mcp-<server>`.
pub const MCP_GATEWAY_TOOLSET: &str = "mcp";

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;
/// Search results carry at most this much of each description: the
/// gateway's result has no size limit of its own.
const MAX_DESCRIPTION_CHARS: usize = 1_000;

/// The MCP tool names the gateway may search and call now.
pub type McpGatewayScope = Arc<dyn Fn(&ToolRegistry) -> HashSet<String> + Send + Sync>;

/// Whether `name` is a tool an MCP server supplied (toolset `mcp-<server>`).
pub fn is_mcp_server_tool(registry: &ToolRegistry, name: &str) -> bool {
    registry
        .get_tool(name)
        .is_some_and(|tool| tool.toolset.starts_with("mcp-"))
}

/// Whether the session runs in gateway mode.
pub fn is_mcp_gateway_registered(registry: &ToolRegistry) -> bool {
    registry
        .get_tool(MCP_GATEWAY_TOOL)
        .is_some_and(|tool| tool.toolset == MCP_GATEWAY_TOOLSET)
}

/// Register the gateway in `registry`. `servers` are the configured server
/// names, named in its description; `scope` decides what it can reach.
pub fn register_mcp_gateway(registry: &ToolRegistry, servers: &[String], scope: McpGatewayScope) {
    let mut servers = servers.to_vec();
    servers.sort();
    let description = format!(
        "Search and call the tools of the MCP servers ({}). Call with action 'search' and a \
         query to get matching tools with their input schemas, then with action 'call', the \
         tool's name and its arguments.",
        servers.join(", ")
    );
    let parameters: JsonSchema = serde_json::from_value(json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["search", "call"]},
            "query": {"type": "string", "description": "search: words to match in tool names and descriptions; empty lists every tool"},
            "limit": {"type": "integer", "description": "search: most tools to return (default 10, at most 50)"},
            "tool": {"type": "string", "description": "call: the tool name a search returned"},
            "arguments": {"type": "object", "description": "call: the tool's arguments, as its input schema describes"}
        },
        "required": ["action"]
    }))
    .expect("gateway schema");
    let schema = ToolSchema::new(MCP_GATEWAY_TOOL, description, parameters);
    register_handler(registry, schema, scope);
}

/// Give the registered gateway a new scope, as after a config change. Its
/// schema stays, so the tool list stays byte-identical. False when no
/// gateway is registered.
pub fn rescope_mcp_gateway(registry: &ToolRegistry, scope: McpGatewayScope) -> bool {
    if !is_mcp_gateway_registered(registry) {
        return false;
    }
    let Some(schema) = registry
        .get_definitions()
        .into_iter()
        .find(|schema| schema.name == MCP_GATEWAY_TOOL)
    else {
        return false;
    };
    register_handler(registry, schema, scope);
    true
}

fn register_handler(registry: &ToolRegistry, schema: ToolSchema, scope: McpGatewayScope) {
    let description = schema.description.clone();
    let handler: Arc<dyn ToolHandler> = Arc::new(McpGateway {
        registry: registry.downgrade(),
        scope,
        schema: schema.clone(),
    });
    // Replaced in place, with no deregister, so no call finds it missing.
    registry.set_passthrough_handler(&handler);
    registry.register(
        MCP_GATEWAY_TOOL,
        MCP_GATEWAY_TOOLSET,
        schema,
        handler,
        Arc::new(|| true),
        Vec::new(),
        true,
        description,
        "mcp",
        // The inner tool's own limit applies; a second cut here would hide
        // how much the first one removed.
        Some(usize::MAX),
    );
}

struct McpGateway {
    // Weak: the registry holds this handler, and a strong handle would keep
    // the registry, its MCP clients and their server processes alive.
    registry: WeakToolRegistry,
    scope: McpGatewayScope,
    schema: ToolSchema,
}

#[async_trait]
impl ToolHandler for McpGateway {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let registry = self.registry.upgrade_for_call()?;
        let scope = (self.scope)(&registry);
        match params.get("action").and_then(Value::as_str) {
            Some("search") => {
                let query = params.get("query").and_then(Value::as_str).unwrap_or("");
                let limit = params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map_or(DEFAULT_LIMIT, |n| (n as usize).clamp(1, MAX_LIMIT));
                Ok(search(&registry, &scope, query, limit).to_string())
            }
            Some("call") => {
                let tool = params
                    .get("tool")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::InvalidParams("call needs 'tool'".to_string()))?;
                if !scope.contains(tool) {
                    return Err(ToolError::NotFound(format!(
                        "'{tool}' is not an MCP tool this session can call; use action 'search' to find one"
                    )));
                }
                let arguments = match params.get("arguments") {
                    None | Some(Value::Null) => json!({}),
                    // Some models send the arguments as a JSON string.
                    Some(Value::String(raw)) => serde_json::from_str(raw).map_err(|e| {
                        ToolError::InvalidParams(format!("'arguments' is not a JSON object: {e}"))
                    })?,
                    Some(other) => other.clone(),
                };
                // Through the registry, so the tool's own policy, secret
                // refusal, masking and size limit apply as to a direct call.
                Ok(registry.dispatch_async(tool, arguments).await)
            }
            _ => Err(ToolError::InvalidParams(
                "action must be 'search' or 'call'".to_string(),
            )),
        }
    }

    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }
}

/// The tools in `scope` matching most of `query`'s words, then by name.
fn search(registry: &ToolRegistry, scope: &HashSet<String>, query: &str, limit: usize) -> Value {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut hits: Vec<(usize, ToolSchema)> = registry
        .get_definitions()
        .into_iter()
        .filter(|schema| scope.contains(&schema.name))
        .filter_map(|schema| {
            let text = format!("{} {}", schema.name, schema.description).to_lowercase();
            let score = words
                .iter()
                .filter(|word| text.contains(word.as_str()))
                .count();
            (words.is_empty() || score > 0).then_some((score, schema))
        })
        .collect();
    hits.sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| x.name.cmp(&y.name)));
    let total = hits.len();
    let tools: Vec<Value> = hits
        .into_iter()
        .take(limit)
        .map(|(_, schema)| {
            json!({
                "name": schema.name,
                "description": schema.description.chars().take(MAX_DESCRIPTION_CHARS).collect::<String>(),
                "parameters": schema.parameters,
            })
        })
        .collect();
    json!({ "tools": tools, "total": total })
}

#[cfg(test)]
mod tests;
