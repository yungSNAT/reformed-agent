use serde::{Deserialize, Serialize};

/// The wire format and handshake selected for a local MCP server.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStdioMode {
    /// Hermes' original Content-Length framing and initialize handshake, for
    /// a server that does not read one JSON message per line.
    LegacyFramed,
    /// Use newline-delimited JSON and try modern discovery before initialize.
    StandardStdio,
    /// Newline-delimited JSON, as the MCP specification's stdio transport
    /// frames it, and the initialize handshake. The default.
    #[default]
    StandardLegacy,
}
