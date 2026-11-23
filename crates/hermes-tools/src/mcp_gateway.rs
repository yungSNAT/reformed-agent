//! The `mcp` gateway tool (`mcp_tool_mode: gateway`).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema};
use serde_json::Value;

use crate::registry::ToolRegistry;

pub const MCP_GATEWAY_TOOL: &str = "mcp";
pub const MCP_GATEWAY_TOOLSET: &str = "mcp";

pub type McpGatewayScope = Arc<dyn Fn(&ToolRegistry) -> HashSet<String> + Send + Sync>;

pub fn is_mcp_server_tool(_registry: &ToolRegistry, _name: &str) -> bool {
    false
}

pub fn is_mcp_gateway_registered(_registry: &ToolRegistry) -> bool {
    false
}

pub fn register_mcp_gateway(_registry: &ToolRegistry, _servers: &[String], _scope: McpGatewayScope) {
    let _ = (JsonSchema::new("object"), ToolError::NotFound(String::new()));
}

struct McpGateway;

#[async_trait]
impl ToolHandler for McpGateway {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        Ok(String::new())
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(MCP_GATEWAY_TOOL, "", JsonSchema::new("object"))
    }
}

#[cfg(test)]
mod tests;
