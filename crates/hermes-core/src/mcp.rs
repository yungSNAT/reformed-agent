use serde::{Deserialize, Serialize};

/// The wire format and handshake selected for a local MCP server.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStdioMode {
    /// Preserve Hermes' original Content-Length framing and initialize handshake.
    #[default]
    LegacyFramed,
    /// Use newline-delimited JSON and try modern discovery before initialize.
    StandardStdio,
    /// Use newline-delimited JSON and go directly to the initialize handshake.
    StandardLegacy,
}
