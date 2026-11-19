//! MCP Transport Layer
//!
//! Provides transport abstraction for MCP communication:
//! - **StdioTransport**: Communicates via stdin/stdout with a child process (JSON-RPC)
//! - **HttpTransport**: Communicates via HTTP/SSE with a remote MCP server
//!
//! The transport layer handles:
//! - Message framing with Content-Length header + JSON body
//! - Connection lifecycle (start, send, receive, close)
//! - Error handling and connection lifecycle

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tracing::{error, info, trace};

use hermes_core::subprocess::CommandNoWindowExt;

use crate::McpError;

const MCP_PROTOCOL_VERSION_HEADER_VALUE: &str = "2025-03-26";
const DEFAULT_MCP_MAX_MESSAGE_BYTES_STRICT: usize = 1024 * 1024;
const DEFAULT_MCP_MAX_MESSAGE_BYTES_BALANCED: usize = 2 * 1024 * 1024;
const DEFAULT_MCP_MAX_MESSAGE_BYTES_RELAXED: usize = 8 * 1024 * 1024;
const DEFAULT_MCP_SSE_READ_TIMEOUT_SECS: u64 = 300;
const DEFAULT_MCP_SSE_CONNECT_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpSandboxProfile {
    Strict,
    Balanced,
    Relaxed,
}

impl McpSandboxProfile {
    fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Balanced => "balanced",
            Self::Relaxed => "relaxed",
        }
    }
}

fn parse_sandbox_profile(raw: &str) -> McpSandboxProfile {
    match raw.trim().to_ascii_lowercase().as_str() {
        "strict" => McpSandboxProfile::Strict,
        "relaxed" => McpSandboxProfile::Relaxed,
        _ => McpSandboxProfile::Balanced,
    }
}

fn sandbox_profile_from_env() -> McpSandboxProfile {
    parse_sandbox_profile(
        &std::env::var("HERMES_MCP_SANDBOX_PROFILE").unwrap_or_else(|_| "balanced".to_string()),
    )
}

fn mcp_max_message_bytes(profile: McpSandboxProfile) -> usize {
    std::env::var("HERMES_MCP_MAX_MESSAGE_BYTES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(match profile {
            McpSandboxProfile::Strict => DEFAULT_MCP_MAX_MESSAGE_BYTES_STRICT,
            McpSandboxProfile::Balanced => DEFAULT_MCP_MAX_MESSAGE_BYTES_BALANCED,
            McpSandboxProfile::Relaxed => DEFAULT_MCP_MAX_MESSAGE_BYTES_RELAXED,
        })
}

fn max_message_bytes_from_env() -> usize {
    mcp_max_message_bytes(sandbox_profile_from_env())
}

fn mcp_sse_read_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("HERMES_MCP_SSE_READ_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MCP_SSE_READ_TIMEOUT_SECS),
    )
}

fn mcp_sse_connect_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("HERMES_MCP_SSE_CONNECT_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MCP_SSE_CONNECT_TIMEOUT_SECS),
    )
}

fn parse_allowlist(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

fn command_basename(command: &str) -> String {
    std::path::Path::new(command)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| command.to_string())
        .to_ascii_lowercase()
}

fn command_allowed_by_profile(
    profile: McpSandboxProfile,
    command: &str,
    allowlist: &[String],
) -> bool {
    if matches!(profile, McpSandboxProfile::Relaxed) {
        return true;
    }
    let base = command_basename(command);
    if !allowlist.is_empty() && allowlist.iter().any(|entry| entry == &base) {
        return true;
    }

    match profile {
        McpSandboxProfile::Strict => false,
        McpSandboxProfile::Balanced => {
            // Block direct shell launches in balanced mode unless explicitly allowlisted.
            !matches!(base.as_str(), "sh" | "bash" | "zsh" | "fish")
        }
        McpSandboxProfile::Relaxed => true,
    }
}

fn default_allowlist() -> Vec<String> {
    parse_allowlist(
        &std::env::var("HERMES_MCP_SANDBOX_ALLOWED_COMMANDS")
            .unwrap_or_else(|_| "node,npx,python,python3,uv,uvx,deno".to_string()),
    )
}

fn is_sensitive_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("API_KEY")
        || upper.contains("TOKEN")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("AUTH")
        || upper.contains("COOKIE")
        || upper.contains("SESSION")
}

/// Variables a stdio server inherits from Hermes, as upstream's
/// `_build_safe_env` passes them; none holds a secret.
const INHERITED_ENV_KEYS: &[&str] = &[
    "PATH", "HOME", "USER", "LANG", "LC_ALL", "TERM", "SHELL", "TMPDIR",
];

/// Windows process and location variables, matched case-insensitively.
/// A Windows server often fails without them: `cmd.exe` needs `PATHEXT` to
/// run a bare `node`, and networking fails without `SYSTEMROOT`.
const INHERITED_WINDOWS_ENV_KEYS: &[&str] = &[
    "ALLUSERSPROFILE",
    "APPDATA",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "COMMONPROGRAMW6432",
    "COMPUTERNAME",
    "COMSPEC",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOCALAPPDATA",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "PATHEXT",
    "PROCESSOR_ARCHITECTURE",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "PUBLIC",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "USERDOMAIN",
    "USERNAME",
    "USERPROFILE",
    "WINDIR",
];

fn is_windows_env_key(key: &str) -> bool {
    INHERITED_WINDOWS_ENV_KEYS.contains(&key.to_ascii_uppercase().as_str())
}

fn is_inherited_env_key(key: &str) -> bool {
    // Windows variable names are case-insensitive, and `Path` is the usual
    // spelling there.
    let upper = key.to_ascii_uppercase();
    let name = if cfg!(windows) { upper.as_str() } else { key };
    INHERITED_ENV_KEYS.contains(&name) || is_windows_env_key(key) || name.starts_with("XDG_")
}

/// A server's environment before sanitising: the inherited variables, then
/// its configured `env`, which wins.
fn child_env_with_inherited(configured: &HashMap<String, String>) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(key, _)| is_inherited_env_key(key))
        .collect();
    for (key, value) in configured {
        if cfg!(windows) {
            env.retain(|inherited, _| !inherited.eq_ignore_ascii_case(key));
        }
        env.insert(key.clone(), value.clone());
    }
    env
}

/// The program to start for `command`. On Windows, Rust's `Command` finds
/// only `.exe` files for a bare name, so `npx` would miss `npx.cmd`. A bare
/// name is looked up on the server's own `PATH` through its `PATHEXT`,
/// keeping only the kinds `Command` can start (not `.js` or `.vbs`) and only
/// absolute entries, so a file planted in the working folder cannot run.
fn program_for(command: &str, env: &HashMap<String, String>) -> PathBuf {
    #[cfg(windows)]
    if !command.contains(['/', '\\']) {
        const STARTABLE: [&str; 4] = ["com", "exe", "bat", "cmd"];
        let startable = |ext: &str| STARTABLE.iter().any(|s| s.eq_ignore_ascii_case(ext));
        let var = |name: &str| {
            env.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        let named_extension = std::path::Path::new(command)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(startable);
        let candidates: Vec<String> = if named_extension {
            vec![command.to_string()]
        } else {
            var("PATHEXT")
                .filter(|exts| !exts.is_empty())
                .unwrap_or(".COM;.EXE;.BAT;.CMD")
                .split(';')
                .filter_map(|ext| ext.strip_prefix('.'))
                .filter(|ext| startable(ext))
                .map(|ext| format!("{command}.{ext}"))
                .collect()
        };
        let dirs = std::env::split_paths(var("PATH").unwrap_or_default());
        for dir in dirs.filter(|dir| dir.is_absolute()) {
            for candidate in &candidates {
                let path = dir.join(candidate);
                if path.is_file() {
                    return path;
                }
            }
        }
    }
    #[cfg(not(windows))]
    let _ = env;
    PathBuf::from(command)
}

fn sanitize_child_env(
    env: &HashMap<String, String>,
    profile: McpSandboxProfile,
) -> HashMap<String, String> {
    if matches!(profile, McpSandboxProfile::Relaxed) {
        return env.clone();
    }

    let mut out = HashMap::new();
    for (k, v) in env {
        let key_upper = k.to_ascii_uppercase();
        if is_sensitive_env_key(k) {
            continue;
        }
        if matches!(profile, McpSandboxProfile::Strict) {
            let allowed_strict = key_upper == "PATH"
                || key_upper == "HOME"
                || key_upper == "USER"
                || key_upper == "SHELL"
                || key_upper == "TMPDIR"
                || key_upper == "TEMP"
                || key_upper == "TZ"
                || key_upper == "LANG"
                || key_upper == "TERM"
                || key_upper.starts_with("LC_")
                || key_upper.starts_with("MCP_")
                || key_upper.starts_with("HERMES_MCP_")
                || is_windows_env_key(k);
            if !allowed_strict {
                continue;
            }
        }
        out.insert(k.clone(), v.clone());
    }
    out
}

fn sandbox_cwd(profile: McpSandboxProfile) -> Option<PathBuf> {
    if let Ok(v) = std::env::var("HERMES_MCP_SANDBOX_CWD") {
        let p = PathBuf::from(v.trim());
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    if matches!(profile, McpSandboxProfile::Strict) {
        return Some(hermes_home_dir());
    }
    None
}

fn hermes_home_dir() -> PathBuf {
    if let Ok(path) = std::env::var("HERMES_HOME") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join(".hermes-agent-ultra");
        }
    }
    PathBuf::from(".hermes-agent-ultra")
}

/// Stderr for a stdio MCP server: `logs/mcp-stderr.log`, opened for append
/// with a start banner, or discarded if the log cannot be opened.
///
/// The child writes the log itself. Draining a pipe through tokio instead
/// parks a read on the blocking pool on Windows, and runtime shutdown waits
/// for that read until the server exits.
fn mcp_stderr_log(command: &str) -> Stdio {
    use std::io::Write;

    let path = hermes_home_dir().join("logs").join("mcp-stderr.log");
    let log = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        });
    let Ok(mut log) = log else {
        return Stdio::null();
    };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(
        log,
        "\n===== [ts={ts}] starting MCP stdio server '{command}' ====="
    );
    Stdio::from(log)
}

fn classify_http_status_error(status: reqwest::StatusCode, body: &str) -> McpError {
    let msg = format!("HTTP error {}: {}", status, body);
    match status.as_u16() {
        401 => McpError::Auth(msg),
        403 => McpError::Forbidden(msg),
        404 => McpError::ResourceNotFound(msg),
        400 | 422 => McpError::InvalidParams(msg),
        501 | 503 => McpError::NotConfigured(msg),
        _ => McpError::ConnectionError(msg),
    }
}

fn enforce_message_size(content_length: usize, max_message_bytes: usize) -> Result<(), McpError> {
    if content_length > max_message_bytes {
        return Err(McpError::Protocol {
            code: -32600,
            message: format!(
                "MCP message exceeds limit: {} bytes > {} bytes",
                content_length, max_message_bytes
            ),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// McpTransport trait
// ---------------------------------------------------------------------------

/// Transport trait for MCP communication.
///
/// Implementations handle the low-level details of sending and receiving
/// JSON-RPC messages over different transport mechanisms.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Start the transport connection.
    async fn start(&mut self) -> Result<(), McpError>;

    /// Send a JSON-RPC message.
    async fn send(&mut self, message: Value) -> Result<(), McpError>;

    /// Receive a JSON-RPC message.
    async fn receive(&mut self) -> Result<Value, McpError>;

    /// Close the transport connection.
    async fn close(&mut self) -> Result<(), McpError>;
}

// ---------------------------------------------------------------------------
// StdioTransport
// ---------------------------------------------------------------------------

/// Transport that communicates via stdin/stdout with a child process.
///
/// Uses the MCP message framing protocol:
/// - Each message is prefixed with a `Content-Length` header
/// - The body is a UTF-8 encoded JSON string
/// - Messages are separated by `\r\n\r\n` between header and body
///
/// This is the standard transport for local MCP server processes.
pub struct StdioTransport {
    /// The command to execute.
    command: String,
    /// Arguments for the command.
    args: Vec<String>,
    /// Environment variables for the child process.
    env: HashMap<String, String>,
    /// The child process.
    child: Option<Child>,
    /// Background discovery should not let OAuth helpers prompt on stdin.
    suppress_interactive_oauth: bool,
    /// Whether the transport has been started.
    started: bool,
}

impl StdioTransport {
    /// Create a new stdio transport for the given command.
    pub fn new(
        command: impl Into<String>,
        args: &[String],
        env: &HashMap<String, String>,
        suppress_interactive_oauth: bool,
    ) -> Self {
        Self {
            command: command.into(),
            args: args.to_vec(),
            env: env.clone(),
            child: None,
            suppress_interactive_oauth,
            started: false,
        }
    }

    fn effective_child_env(&self, profile: McpSandboxProfile) -> HashMap<String, String> {
        let mut env = sanitize_child_env(&child_env_with_inherited(&self.env), profile);
        if self.suppress_interactive_oauth {
            env.insert(
                "HERMES_MCP_BACKGROUND_DISCOVERY".to_string(),
                "1".to_string(),
            );
            env.insert(
                "HERMES_MCP_SUPPRESS_INTERACTIVE_OAUTH".to_string(),
                "1".to_string(),
            );
            env.insert("MCP_NONINTERACTIVE".to_string(), "1".to_string());
        }
        env
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        if self.started {
            return Err(McpError::ConnectionError(
                "Transport already started".to_string(),
            ));
        }

        let sandbox_profile = sandbox_profile_from_env();
        let allowlist = default_allowlist();
        if !command_allowed_by_profile(sandbox_profile, &self.command, &allowlist) {
            return Err(McpError::Forbidden(format!(
                "MCP command '{}' blocked by sandbox profile '{}' (allowlist: {})",
                self.command,
                sandbox_profile.as_str(),
                if allowlist.is_empty() {
                    "(none)".to_string()
                } else {
                    allowlist.join(",")
                }
            )));
        }
        let sanitized_env = self.effective_child_env(sandbox_profile);

        info!(
            "Starting MCP stdio transport: {} {:?}",
            self.command, self.args
        );

        let mut cmd = Command::new(program_for(&self.command, &sanitized_env));
        cmd.args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(mcp_stderr_log(&self.command))
            // A server dropped mid-handshake (startup timeout, failed
            // reconnect) may never read stdin to see EOF.
            .kill_on_drop(true);

        if let Some(cwd) = sandbox_cwd(sandbox_profile) {
            cmd.current_dir(cwd);
        }

        cmd.env_clear();
        // Set environment variables
        for (key, value) in &sanitized_env {
            cmd.env(key, value);
        }
        cmd.suppress_windows_console();

        let child = cmd.spawn().map_err(|e| {
            McpError::ConnectionError(format!("Failed to spawn process '{}': {}", self.command, e))
        })?;

        self.child = Some(child);
        self.started = true;

        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let child = self
            .child
            .as_mut()
            .ok_or(McpError::ConnectionError("Process not started".to_string()))?;

        let stdin = child
            .stdin
            .as_mut()
            .ok_or(McpError::ConnectionError("stdin not available".to_string()))?;

        let body =
            serde_json::to_string(&message).map_err(|e| McpError::Serialization(e.to_string()))?;
        let max_message_bytes = max_message_bytes_from_env();
        enforce_message_size(body.len(), max_message_bytes)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        trace!("Sending MCP message: {} bytes", body.len());

        stdin
            .write_all(header.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write header failed: {}", e)))?;
        stdin
            .write_all(body.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write body failed: {}", e)))?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Flush failed: {}", e)))?;

        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        let child = self
            .child
            .as_mut()
            .ok_or(McpError::ConnectionError("Process not started".to_string()))?;

        let stdout = child.stdout.as_mut().ok_or(McpError::ConnectionError(
            "stdout not available".to_string(),
        ))?;

        // Read Content-Length header
        let mut reader = tokio::io::BufReader::new(stdout);

        // Read header lines until we get Content-Length or empty line
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .map_err(|e| McpError::ConnectionError(format!("Read header failed: {}", e)))?;

            if line.is_empty() {
                return Err(McpError::ConnectionClosed);
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                // End of headers
                break;
            }

            if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
                content_length =
                    len_str
                        .trim()
                        .parse::<usize>()
                        .map_err(|e| McpError::Protocol {
                            code: -32700,
                            message: format!("Invalid Content-Length: {}", e),
                        })?;
            }
        }

        if content_length == 0 {
            return Err(McpError::Protocol {
                code: -32700,
                message: "Missing Content-Length header".to_string(),
            });
        }
        enforce_message_size(content_length, max_message_bytes_from_env())?;

        // Read the JSON body
        let mut body_buf = vec![0u8; content_length];
        reader
            .read_exact(&mut body_buf)
            .await
            .map_err(|e| McpError::ConnectionError(format!("Read body failed: {}", e)))?;

        trace!("Received MCP message: {} bytes", body_buf.len());

        let value: Value = serde_json::from_slice(&body_buf).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("JSON parse error: {}", e),
        })?;

        Ok(value)
    }

    async fn close(&mut self) -> Result<(), McpError> {
        if let Some(mut child) = self.child.take() {
            info!("Shutting down MCP stdio process");
            // Try graceful shutdown first
            match child.kill().await {
                Ok(()) => {
                    // Wait for the process to exit
                    let _ = child.wait().await;
                }
                Err(e) => {
                    error!("Failed to kill MCP process: {}", e);
                }
            }
        }
        self.started = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ServerStdioTransport — server-side stdio (reads own stdin, writes own stdout)
// ---------------------------------------------------------------------------

/// Server-side stdio transport that reads from the process's own stdin
/// and writes to stdout. Used when hermes itself acts as an MCP server
/// (e.g. `hermes mcp serve`).
pub struct ServerStdioTransport {
    started: bool,
}

impl ServerStdioTransport {
    pub fn new() -> Self {
        Self { started: false }
    }
}

impl Default for ServerStdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl McpTransport for ServerStdioTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        if self.started {
            return Err(McpError::ConnectionError(
                "Transport already started".to_string(),
            ));
        }
        self.started = true;
        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let body =
            serde_json::to_string(&message).map_err(|e| McpError::Serialization(e.to_string()))?;
        let max_message_bytes = max_message_bytes_from_env();
        enforce_message_size(body.len(), max_message_bytes)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(header.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write header failed: {}", e)))?;
        stdout
            .write_all(body.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write body failed: {}", e)))?;
        stdout
            .flush()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Flush failed: {}", e)))?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        let stdin = tokio::io::stdin();
        let mut reader = tokio::io::BufReader::new(stdin);

        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| McpError::ConnectionError(format!("Read header failed: {}", e)))?;
            if n == 0 {
                return Err(McpError::ConnectionClosed);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
                content_length =
                    len_str
                        .trim()
                        .parse::<usize>()
                        .map_err(|e| McpError::Protocol {
                            code: -32700,
                            message: format!("Invalid Content-Length: {}", e),
                        })?;
            }
        }

        if content_length == 0 {
            return Err(McpError::Protocol {
                code: -32700,
                message: "Missing Content-Length header".to_string(),
            });
        }
        enforce_message_size(content_length, max_message_bytes_from_env())?;

        let mut body_buf = vec![0u8; content_length];
        reader
            .read_exact(&mut body_buf)
            .await
            .map_err(|e| McpError::ConnectionError(format!("Read body failed: {}", e)))?;

        serde_json::from_slice(&body_buf).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("JSON parse error: {}", e),
        })
    }

    async fn close(&mut self) -> Result<(), McpError> {
        self.started = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HttpTransport
// ---------------------------------------------------------------------------

/// Transport that communicates via HTTP with a remote MCP server.
///
/// Uses HTTP POST for sending JSON-RPC messages and receives responses
/// directly. For servers supporting SSE (Server-Sent Events), this
/// transport can also handle streaming responses.
pub struct HttpTransport {
    /// The base URL of the MCP server.
    url: String,
    /// Optional bearer token for authentication.
    auth_token: Option<String>,
    /// HTTP client.
    client: reqwest::Client,
    /// Whether the transport has been started.
    started: bool,
    /// Pending response from the server.
    pending_response: Option<Value>,
}

impl HttpTransport {
    /// Create a new HTTP transport for the given URL.
    pub fn new(url: &str, auth_token: Option<String>) -> Self {
        Self {
            url: url.to_string(),
            auth_token,
            client: reqwest::Client::new(),
            started: false,
            pending_response: None,
        }
    }

    /// Build the full URL for an MCP endpoint.
    fn endpoint_url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// Build a request with optional authentication.
    fn build_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut builder = self.client.request(method, url);
        builder = builder.header("Content-Type", "application/json");
        builder = builder.header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION_HEADER_VALUE);
        if let Some(ref token) = self.auth_token {
            builder = builder.header("Authorization", format!("Bearer {}", token));
        }
        builder
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        // For HTTP transport, "starting" means verifying the server is reachable
        if self.started {
            return Err(McpError::ConnectionError(
                "Transport already started".to_string(),
            ));
        }

        // Verify the server is reachable
        let url = self.endpoint_url("/health");
        match self.client.get(&url).send().await {
            Ok(_) => {
                info!("MCP HTTP transport connected to: {}", self.url);
                self.started = true;
                Ok(())
            }
            Err(_) => {
                // Health endpoint may not exist; try the base URL
                let base_url = &self.url;
                match self.client.get(base_url).send().await {
                    Ok(_) => {
                        info!("MCP HTTP transport connected to: {}", self.url);
                        self.started = true;
                        Ok(())
                    }
                    Err(e) => Err(McpError::ConnectionError(format!(
                        "Failed to connect to MCP server at {}: {}",
                        self.url, e
                    ))),
                }
            }
        }
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let url = self.endpoint_url("/message");
        let builder = self.build_request(reqwest::Method::POST, &url);
        let response = builder
            .json(&message)
            .send()
            .await
            .map_err(|e| McpError::ConnectionError(format!("HTTP send failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(classify_http_status_error(status, &body));
        }

        // Store the response for receive()
        let body = response
            .bytes()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Failed to read response: {}", e)))?;
        let max_message_bytes = max_message_bytes_from_env();
        enforce_message_size(body.len(), max_message_bytes)?;
        let value: Value = serde_json::from_slice(&body).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("Invalid JSON response: {}", e),
        })?;

        self.pending_response = Some(value);
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        // For HTTP transport, the response is already stored from send()
        match self.pending_response.take() {
            Some(value) => Ok(value),
            None => Err(McpError::ConnectionError(
                "No pending response. Call send() first.".to_string(),
            )),
        }
    }

    async fn close(&mut self) -> Result<(), McpError> {
        info!("Closing MCP HTTP transport");
        self.started = false;
        self.pending_response = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HttpSseTransport
// ---------------------------------------------------------------------------

/// Transport for remote MCP servers over HTTP.
///
/// It speaks Streamable HTTP, the transport the MCP specification defines
/// since 2025-03-26: each JSON-RPC message is POSTed to the server's URL with
/// `Accept: application/json, text/event-stream`; the reply is a JSON body,
/// an SSE stream of messages or `202 Accepted` for a notification; and the
/// `Mcp-Session-Id` the server assigns at `initialize` goes back on every
/// later request.
///
/// As the specification's backwards-compatibility section advises, the first
/// POST goes to the URL; only when it fails with a 4xx other than 401 or 403
/// and `<url>/sse` answers is the server treated as the older HTTP+SSE kind this
/// transport spoke before: messages are then POSTed to the endpoint its
/// `mcp-message-endpoint` header or first `endpoint` event names (within the
/// server's origin), else `<url>/message`. That GET's event stream is held
/// open, as such a server may answer a POST with `202 Accepted` and send the
/// reply as a `message` event there.
pub struct HttpSseTransport {
    /// The base URL of the MCP server.
    url: String,
    /// Optional bearer token for authentication.
    auth_token: Option<String>,
    /// Extra headers sent with every request, such as a hosted server's API
    /// key header.
    headers: Vec<(String, String)>,
    /// HTTP client.
    client: reqwest::Client,
    /// Whether the transport has been started.
    started: bool,
    /// Messages from the last POST not yet received, oldest first.
    pending: VecDeque<Value>,
    /// Whether the server is the older HTTP+SSE kind.
    legacy_sse: bool,
    /// Whether a POST has succeeded, settling which kind the server is.
    kind_known: bool,
    /// The message endpoint the older kind's SSE response named.
    message_endpoint: Option<String>,
    /// The older kind's held GET event stream, its bytes not yet taken as
    /// events, and where the next event starts in them.
    legacy_stream: Option<reqwest::Response>,
    legacy_bytes: Vec<u8>,
    legacy_events: SseResponseFinder,
    /// The session id a Streamable HTTP server assigned.
    session_id: Option<String>,
    /// Read timeout used for SSE and POST responses.
    sse_read_timeout: Duration,
    /// Connect timeout used for initial SSE handshake.
    connect_timeout: Duration,
    /// The largest message a reply or event may hold.
    max_message_bytes: usize,
}

impl HttpSseTransport {
    /// Create a new HTTP transport for the given URL.
    pub fn new(url: &str, auth_token: Option<String>) -> Self {
        Self {
            url: url.to_string(),
            auth_token,
            headers: Vec::new(),
            // reqwest drops only Authorization and cookies on a redirect to
            // another origin; a configured header or the session id would go
            // along, so no redirect leaves the server's origin.
            client: reqwest::Client::builder()
                .redirect(same_origin_redirects())
                .build()
                .expect("HTTP client builds as Client::new() does"),
            started: false,
            pending: VecDeque::new(),
            legacy_sse: false,
            kind_known: false,
            message_endpoint: None,
            legacy_stream: None,
            legacy_bytes: Vec::new(),
            legacy_events: SseResponseFinder::default(),
            session_id: None,
            sse_read_timeout: mcp_sse_read_timeout(),
            connect_timeout: mcp_sse_connect_timeout(),
            max_message_bytes: max_message_bytes_from_env(),
        }
    }

    /// Send these headers with every request.
    pub fn with_headers(mut self, headers: impl IntoIterator<Item = (String, String)>) -> Self {
        self.headers = headers.into_iter().collect();
        self
    }

    /// Build the full URL for an MCP endpoint.
    fn endpoint_url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// The URL used for POSTing JSON-RPC messages.
    fn post_url(&self) -> String {
        match (&self.message_endpoint, self.legacy_sse) {
            (Some(ep), _) => ep.clone(),
            (None, true) => self.endpoint_url("/message"),
            (None, false) => self.url.clone(),
        }
    }

    /// Add the authentication, configured and session headers.
    fn with_auth(&self, mut builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        for (name, value) in &self.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(ref token) = self.auth_token {
            builder = builder.header("Authorization", format!("Bearer {}", token));
        }
        if let Some(ref session_id) = self.session_id {
            builder = builder.header("Mcp-Session-Id", session_id.as_str());
        }
        builder
    }

    /// Build a request with optional authentication.
    fn build_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut builder = self.client.request(method, url);
        builder = builder.header("Content-Type", "application/json");
        builder = builder.header("Accept", "application/json, text/event-stream");
        builder = builder.header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION_HEADER_VALUE);
        builder = builder.timeout(self.sse_read_timeout);
        self.with_auth(builder)
    }

    async fn post(&self, message: &Value) -> Result<reqwest::Response, McpError> {
        self.build_request(reqwest::Method::POST, &self.post_url())
            .json(message)
            .send()
            .await
            .map_err(|e| McpError::ConnectionError(format!("HTTP send failed: {}", e)))
    }

    /// Whether the server is the older HTTP+SSE kind: `<url>/sse` answers.
    /// A message endpoint it names on another origin is refused, as the
    /// credentials would go there.
    async fn switch_to_legacy(&mut self) -> Result<bool, McpError> {
        let sse_url = self.endpoint_url("/sse");
        // Only the wait for the headers is bounded: the stream stays open
        // for the server's replies.
        let request = self
            .with_auth(self.client.get(&sse_url))
            .header("Accept", "text/event-stream")
            .send();
        let Ok(Ok(resp)) = tokio::time::timeout(self.connect_timeout, request).await else {
            return Ok(false);
        };
        if !resp.status().is_success() {
            return Ok(false);
        }
        if let Some(ep) = resp
            .headers()
            .get("mcp-message-endpoint")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            self.message_endpoint = Some(self.same_origin_endpoint(ep)?);
        }
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(is_event_stream);
        self.legacy_sse = true;
        if is_sse {
            self.legacy_stream = Some(resp);
            // The specification's server names its endpoint in its first event.
            if self.message_endpoint.is_none() {
                let deadline = tokio::time::Instant::now() + self.connect_timeout;
                if let Some((name, data)) = self.next_legacy_event(deadline).await? {
                    if name == "endpoint" {
                        self.message_endpoint = Some(self.same_origin_endpoint(data.trim())?);
                    } else if name == "message" {
                        self.queue_legacy_message(&data)?;
                    }
                }
            }
        }
        info!("MCP HTTP+SSE transport connected to: {}", self.url);
        Ok(true)
    }

    /// The name and data of the next event with data on the older kind's
    /// held stream, or `None` once it has ended or `deadline` has passed.
    /// Comments and other events do not extend the deadline.
    async fn next_legacy_event(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<Option<(String, String)>, McpError> {
        loop {
            if let Some(event) = self.legacy_events.next_event(&self.legacy_bytes) {
                // Each event is held to the limit on its own, however the
                // server grouped events into chunks.
                enforce_message_size(event.len(), self.max_message_bytes)?;
                let text = String::from_utf8(self.legacy_bytes.drain(..event.end).collect())
                    .map_err(|e| McpError::Protocol {
                        code: -32700,
                        message: format!("Invalid UTF-8 in event stream: {}", e),
                    })?;
                self.legacy_events.consume(event.end);
                if let Some(fields) = sse_event_fields(&text) {
                    return Ok(Some(fields));
                }
                continue;
            }
            // What is left is part of one event.
            enforce_message_size(self.legacy_bytes.len(), self.max_message_bytes)?;
            let Some(stream) = self.legacy_stream.as_mut() else {
                return Ok(None);
            };
            let Ok(chunk) = tokio::time::timeout_at(deadline, stream.chunk()).await else {
                return Ok(None);
            };
            match chunk.map_err(|e| {
                McpError::ConnectionError(format!("Failed to read event stream: {}", e))
            })? {
                Some(chunk) => self.legacy_bytes.extend_from_slice(&chunk),
                None => {
                    self.legacy_stream = None;
                    return Ok(None);
                }
            }
        }
    }

    /// Queue the JSON-RPC message (or batch) an event carried.
    fn queue_legacy_message(&mut self, data: &str) -> Result<(), McpError> {
        let message: Value = serde_json::from_str(data).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("Invalid JSON in event stream: {}", e),
        })?;
        match message {
            Value::Array(items) => self.pending.extend(items),
            value => self.pending.push_back(value),
        }
        Ok(())
    }

    /// `endpoint`, resolved against the server URL, if it keeps its origin.
    fn same_origin_endpoint(&self, endpoint: &str) -> Result<String, McpError> {
        let base = reqwest::Url::parse(&self.url)
            .map_err(|e| McpError::ConnectionError(format!("Invalid MCP server URL: {e}")))?;
        let resolved = base
            .join(endpoint)
            .map_err(|e| McpError::ConnectionError(format!("Invalid MCP message endpoint: {e}")))?;
        if resolved.origin() != base.origin() {
            return Err(McpError::ConnectionError(format!(
                "The MCP server named a message endpoint on another origin ({}); it is not used.",
                resolved.origin().ascii_serialization()
            )));
        }
        Ok(resolved.into())
    }
}

/// Follows up to 10 redirects within the first URL's origin and stops at one
/// that leaves it.
fn same_origin_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|first| first.origin() == attempt.url().origin());
        if same_origin && attempt.previous().len() <= 10 {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// Finds, as an SSE reply's bytes arrive, the end of the first event that
/// holds a JSON-RPC response. Each byte is looked at once and each complete
/// event decoded once. A line ends in CRLF, LF or CR; the LF of a CRLF split
/// across chunks is not taken for another line end.
#[derive(Default)]
struct SseResponseFinder {
    /// The next byte to look at.
    pos: usize,
    /// Where the current line starts.
    line_start: usize,
    /// The last byte looked at was a CR.
    after_cr: bool,
    /// Where the event after the last complete one starts.
    event_start: usize,
}

impl SseResponseFinder {
    /// The end of the response's event, once `body` (which only grows
    /// between calls) holds it in full.
    fn find(&mut self, body: &[u8]) -> Result<Option<usize>, McpError> {
        while let Some(event) = self.next_event(body) {
            let end = event.end;
            let event = std::str::from_utf8(&body[event]).map_err(|e| McpError::Protocol {
                code: -32700,
                message: format!("Invalid UTF-8 in response: {}", e),
            })?;
            if http_reply_messages(event, true)?.iter().any(is_response) {
                return Ok(Some(end));
            }
        }
        Ok(None)
    }

    /// The bytes of the next complete event, once `body` (which only grows
    /// between calls, but for what `consume` removes) holds it.
    fn next_event(&mut self, body: &[u8]) -> Option<std::ops::Range<usize>> {
        while self.pos < body.len() {
            let byte = body[self.pos];
            self.pos += 1;
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                self.line_start = self.pos;
                continue;
            }
            if byte != b'\r' && byte != b'\n' {
                continue;
            }
            self.after_cr = byte == b'\r';
            let blank = self.pos - 1 == self.line_start;
            self.line_start = self.pos;
            if blank {
                let start = std::mem::replace(&mut self.event_start, self.pos);
                return Some(start..self.pos);
            }
        }
        None
    }

    /// The caller removed the first `n` bytes, all of them already taken
    /// as events, from the body.
    fn consume(&mut self, n: usize) {
        self.pos -= n;
        self.line_start -= n;
        self.event_start -= n;
    }
}

/// Whether a Content-Type value names an event stream, whatever the case
/// of its media type and whatever parameters follow it.
fn is_event_stream(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// The name (`message` by default) and data of one SSE event, or `None`
/// for an event with no data, such as a comment.
fn sse_event_fields(event: &str) -> Option<(String, String)> {
    let event = event.replace("\r\n", "\n").replace('\r', "\n");
    let mut name = None;
    let mut data: Vec<&str> = Vec::new();
    for line in event.split('\n') {
        let field = |prefix: &str| {
            line.strip_prefix(prefix)
                .map(|value| value.strip_prefix(' ').unwrap_or(value))
        };
        if let Some(value) = field("event:") {
            name = Some(value.to_string());
        } else if let Some(value) = field("data:") {
            data.push(value);
        }
    }
    if data.is_empty() {
        return None;
    }
    Some((
        name.unwrap_or_else(|| "message".to_string()),
        data.join("\n"),
    ))
}

/// Whether `message` is a JSON-RPC response (a result or error for an id).
fn is_response(message: &Value) -> bool {
    message.get("id").is_some() && message.get("method").is_none()
}

/// The JSON-RPC messages in a POST reply: each SSE event's data, or the JSON
/// body (an object, or an array of them).
fn http_reply_messages(body: &str, is_sse: bool) -> Result<Vec<Value>, McpError> {
    let invalid = |e: serde_json::Error| McpError::Protocol {
        code: -32700,
        message: format!("Invalid JSON response: {}", e),
    };
    if !is_sse {
        return Ok(match serde_json::from_str(body).map_err(invalid)? {
            Value::Array(items) => items,
            value => vec![value],
        });
    }
    let body = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut messages = Vec::new();
    let mut data: Vec<&str> = Vec::new();
    for line in body.split('\n').chain(std::iter::once("")) {
        if line.is_empty() {
            if !data.is_empty() {
                messages.push(serde_json::from_str(&data.join("\n")).map_err(invalid)?);
                data.clear();
            }
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    Ok(messages)
}

#[async_trait]
impl McpTransport for HttpSseTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        if self.started {
            return Err(McpError::ConnectionError(
                "Transport already started".to_string(),
            ));
        }

        // Reachability only: a Streamable HTTP server need not answer a GET,
        // and the first POST settles which kind the server is.
        self.with_auth(self.client.get(&self.url))
            .timeout(self.connect_timeout)
            .send()
            .await
            .map_err(|e| {
                McpError::ConnectionError(format!(
                    "Failed to connect to MCP server at {}: {}",
                    self.url, e
                ))
            })?;
        info!("MCP HTTP transport connected to: {}", self.url);

        self.started = true;
        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let mut response = self.post(&message).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            // A 4xx other than an authentication refusal, as the MCP
            // specification's backwards-compatibility section gives.
            let fallback = !self.kind_known
                && status.is_client_error()
                && !matches!(status.as_u16(), 401 | 403);
            if !(fallback && self.switch_to_legacy().await?) {
                return Err(classify_http_status_error(status, &body));
            }
            response = self.post(&message).await?;
            let status = response.status();
            if !status.is_success() {
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "unknown error".to_string());
                return Err(classify_http_status_error(status, &body));
            }
        }
        self.kind_known = true;
        if let Some(session_id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(session_id.to_string());
        }
        if response.status() == reqwest::StatusCode::ACCEPTED {
            // No reply here, whatever the body says (an older server writes
            // "Accepted"); one may follow on the held stream.
            return Ok(());
        }
        let is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));

        // Read as the body arrives: a server may leave an SSE stream open
        // after the response, so reading stops at the event holding it, and
        // what follows in the same chunk is neither decoded nor counted.
        let max_message_bytes = self.max_message_bytes;
        let mut bytes = Vec::new();
        let mut finder = SseResponseFinder::default();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Failed to read response: {}", e)))?
        {
            bytes.extend_from_slice(&chunk);
            if is_sse {
                if let Some(end) = finder.find(&bytes)? {
                    bytes.truncate(end);
                    enforce_message_size(bytes.len(), max_message_bytes)?;
                    break;
                }
            }
            enforce_message_size(bytes.len(), max_message_bytes)?;
        }
        let body = String::from_utf8(bytes).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("Invalid UTF-8 in response: {}", e),
        })?;
        if body.trim().is_empty() {
            // 202 Accepted for a notification or a response: nothing to read.
            return Ok(());
        }
        // An older server may send SSE framing without the content type.
        let is_sse = is_sse || body.lines().any(|line| line.starts_with("data:"));
        self.pending.extend(http_reply_messages(&body, is_sse)?);
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        // As long as a reply in a POST body may take.
        let deadline = tokio::time::Instant::now() + self.sse_read_timeout;
        loop {
            if let Some(value) = self.pending.pop_front() {
                return Ok(value);
            }
            // The older kind may send its replies on the held stream instead.
            match self.next_legacy_event(deadline).await? {
                Some((name, data)) if name == "message" => self.queue_legacy_message(&data)?,
                Some(_) => {}
                None if self.legacy_stream.is_some() => {
                    return Err(McpError::ConnectionError(format!(
                        "MCP server sent no message on its event stream within {}s",
                        self.sse_read_timeout.as_secs()
                    )))
                }
                None => {
                    return Err(McpError::ConnectionError(
                        "No pending response. Call send() first.".to_string(),
                    ))
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), McpError> {
        info!("Closing MCP HTTP transport");
        self.started = false;
        self.pending.clear();
        self.legacy_sse = false;
        self.kind_known = false;
        self.message_endpoint = None;
        self.legacy_stream = None;
        self.legacy_bytes.clear();
        self.legacy_events = SseResponseFinder::default();
        self.session_id = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn dropping_started_stdio_transport_stops_child() {
        let mut transport =
            StdioTransport::new("sleep", &["30".to_string()], &HashMap::new(), false);
        transport.start().await.expect("start sleep");
        let pid = transport
            .child
            .as_ref()
            .and_then(Child::id)
            .expect("child pid");
        drop(transport);

        // Gone, or a zombie awaiting reaping, once the kill lands.
        let stopped = || match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z')),
            Err(_) => true,
        };
        for _ in 0..50 {
            if stopped() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("stdio MCP child {pid} still running after its transport was dropped");
    }

    #[test]
    fn test_stdio_transport_new() {
        let env = HashMap::new();
        let transport = StdioTransport::new("echo", &[], &env, false);
        assert_eq!(transport.command, "echo");
        assert!(transport.args.is_empty());
        assert!(!transport.suppress_interactive_oauth);
        assert!(!transport.started);
    }

    #[test]
    fn stdio_transport_marks_background_discovery_noninteractive() {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/bin".to_string());
        let manual = StdioTransport::new("echo", &[], &env, false);
        let background = StdioTransport::new("echo", &[], &env, true);

        assert!(!manual
            .effective_child_env(McpSandboxProfile::Balanced)
            .contains_key("HERMES_MCP_SUPPRESS_INTERACTIVE_OAUTH"));

        let child_env = background.effective_child_env(McpSandboxProfile::Balanced);
        assert_eq!(
            child_env
                .get("HERMES_MCP_BACKGROUND_DISCOVERY")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            child_env
                .get("HERMES_MCP_SUPPRESS_INTERACTIVE_OAUTH")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            child_env.get("MCP_NONINTERACTIVE").map(String::as_str),
            Some("1")
        );
    }

    fn path_entries(env: &HashMap<String, String>) -> Vec<&String> {
        env.iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("PATH"))
            .map(|(_, value)| value)
            .collect()
    }

    #[test]
    fn stdio_child_inherits_path_and_no_other_hermes_variable() {
        let transport = StdioTransport::new("echo", &[], &HashMap::new(), false);
        let env = transport.effective_child_env(McpSandboxProfile::Balanced);

        let parent_path = std::env::var("PATH").expect("the test runs with a PATH");
        assert_eq!(path_entries(&env), vec![&parent_path]);
        let foreign: Vec<_> = env.keys().filter(|k| !is_inherited_env_key(k)).collect();
        assert!(
            foreign.is_empty(),
            "variables outside the inherited set: {foreign:?}"
        );
    }

    #[test]
    fn stdio_child_configured_path_replaces_the_inherited_one() {
        let mut configured = HashMap::new();
        configured.insert("PATH".to_string(), "/configured/bin".to_string());
        let transport = StdioTransport::new("echo", &[], &configured, false);
        let env = transport.effective_child_env(McpSandboxProfile::Balanced);

        assert_eq!(path_entries(&env), vec!["/configured/bin"]);
    }

    /// A bare name resolves only to a file `Command` can start, in the
    /// server's own `PATHEXT` order, from an absolute `PATH` entry.
    #[cfg(windows)]
    #[test]
    fn program_lookup_uses_the_servers_path_and_pathext() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // A folder under the working directory, reachable by a relative entry.
        let near = tempfile::tempdir_in(".").unwrap();
        let near_name = near.path().file_name().unwrap().to_string_lossy();
        std::fs::write(a.path().join("probe.js"), "").unwrap();
        std::fs::write(b.path().join("probe.bat"), "").unwrap();
        std::fs::write(b.path().join("probe.cmd"), "").unwrap();
        std::fs::write(near.path().join("probe.cmd"), "").unwrap();
        let env = |path: String, pathext: &str| {
            HashMap::from([
                ("Path".to_string(), path),
                ("PATHEXT".to_string(), pathext.to_string()),
            ])
        };
        let ab = format!("{};{}", a.path().display(), b.path().display());

        // `.js` is in Windows' default PATHEXT but `Command` cannot start it.
        let found = program_for("probe", &env(ab.clone(), ".JS;.BAT;.CMD"));
        assert_eq!(found, b.path().join("probe.bat"));
        // The server's PATHEXT order wins over the parent's.
        let found = program_for("probe", &env(ab.clone(), ".CMD;.BAT"));
        assert_eq!(found, b.path().join("probe.cmd"));
        // An empty PATHEXT means the default list.
        let found = program_for("probe", &env(ab, ""));
        assert_eq!(found, b.path().join("probe.bat"));
        // A relative entry would run a file planted in the working directory.
        let found = program_for(
            "probe",
            &env(format!("{near_name};{}", b.path().display()), ".CMD"),
        );
        assert_eq!(found, b.path().join("probe.cmd"));
    }

    /// `npx` is `npx.cmd` on Windows, which Rust's `Command` does not find by
    /// its bare name; the script also reports the `SystemRoot` it was given.
    #[cfg(windows)]
    #[tokio::test]
    async fn stdio_transport_starts_a_cmd_script_named_without_its_extension() {
        let bin = tempfile::tempdir().unwrap();
        let marker = bin.path().join("started.txt");
        std::fs::write(
            bin.path().join("hermes-mcp-probe.cmd"),
            format!("@echo %SystemRoot%> \"{}\"\r\n", marker.display()),
        )
        .unwrap();
        // Node installs an extensionless shell script beside `npx.cmd`.
        std::fs::write(bin.path().join("hermes-mcp-probe"), "#!/bin/sh\n").unwrap();
        let mut configured = HashMap::new();
        configured.insert("PATH".to_string(), bin.path().display().to_string());
        let mut transport = StdioTransport::new("hermes-mcp-probe", &[], &configured, false);

        transport.start().await.expect("start the .cmd server");
        let mut written = None;
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&marker) {
                written = Some(text);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        transport.close().await.unwrap();

        let written = written.expect("the .cmd server ran");
        assert!(
            !written.trim().is_empty() && !written.contains('%'),
            "the server had no SystemRoot: {written:?}"
        );
    }

    #[test]
    fn test_http_transport_new() {
        let transport = HttpTransport::new("http://localhost:8080/mcp", None);
        assert_eq!(transport.url, "http://localhost:8080/mcp");
        assert!(transport.auth_token.is_none());
        assert!(!transport.started);
    }

    #[test]
    fn test_http_transport_with_auth() {
        let transport = HttpTransport::new(
            "http://localhost:8080/mcp",
            Some("secret-token".to_string()),
        );
        assert_eq!(transport.auth_token, Some("secret-token".to_string()));
    }

    #[test]
    fn test_http_transport_seeds_protocol_header() {
        let transport = HttpTransport::new("http://localhost:8080/mcp", None);
        let req = transport
            .build_request(reqwest::Method::POST, "http://localhost:8080/mcp/message")
            .build()
            .expect("build request");
        assert_eq!(
            req.headers()
                .get("MCP-Protocol-Version")
                .and_then(|v| v.to_str().ok()),
            Some(MCP_PROTOCOL_VERSION_HEADER_VALUE)
        );
    }

    #[test]
    fn test_http_sse_transport_seeds_protocol_header() {
        let transport = HttpSseTransport::new("http://localhost:8080/mcp", None);
        let req = transport
            .build_request(reqwest::Method::POST, "http://localhost:8080/mcp/message")
            .build()
            .expect("build request");
        assert_eq!(
            req.headers()
                .get("MCP-Protocol-Version")
                .and_then(|v| v.to_str().ok()),
            Some(MCP_PROTOCOL_VERSION_HEADER_VALUE)
        );
    }

    #[test]
    fn http_reply_messages_reads_every_sse_event() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n: comment\ndata: {\"jsonrpc\":\"2.0\",\ndata: \"id\":3,\"result\":{}}\n";
        let messages = http_reply_messages(body, true).expect("sse body");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["method"], "notifications/progress");
        assert_eq!(messages[1]["id"], 3);
    }

    #[test]
    fn http_reply_messages_reads_a_json_object_or_batch() {
        let one = http_reply_messages("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}", false)
            .expect("object");
        assert_eq!(one.len(), 1);
        let batch = http_reply_messages("[{\"id\":1},{\"id\":2}]", false).expect("batch");
        assert_eq!(batch.len(), 2);
        assert!(http_reply_messages("not json", false).is_err());
    }

    #[test]
    fn streamable_http_posts_to_the_url_with_headers_and_session() {
        let mut transport = HttpSseTransport::new("https://catalogue.example/mcp", None)
            .with_headers([("x-consumer-api-key".to_string(), "k".to_string())]);
        assert_eq!(transport.post_url(), "https://catalogue.example/mcp");
        transport.session_id = Some("s-1".to_string());
        let req = transport
            .build_request(reqwest::Method::POST, &transport.post_url())
            .build()
            .expect("build request");
        let header = |name: &str| req.headers().get(name).and_then(|v| v.to_str().ok());
        assert_eq!(header("x-consumer-api-key"), Some("k"));
        assert_eq!(header("mcp-session-id"), Some("s-1"));
        assert_eq!(
            header("accept"),
            Some("application/json, text/event-stream")
        );

        transport.legacy_sse = true;
        assert_eq!(
            transport.post_url(),
            "https://catalogue.example/mcp/message"
        );
    }

    #[test]
    fn test_endpoint_url() {
        let transport = HttpTransport::new("http://localhost:8080/mcp/", None);
        assert_eq!(
            transport.endpoint_url("/message"),
            "http://localhost:8080/mcp/message"
        );
        assert_eq!(
            transport.endpoint_url("message"),
            "http://localhost:8080/mcp/message"
        );
    }

    #[test]
    fn test_classify_http_status_error_forbidden() {
        let e = classify_http_status_error(reqwest::StatusCode::FORBIDDEN, "no capability");
        assert!(matches!(e, McpError::Forbidden(_)));
    }

    #[test]
    fn test_classify_http_status_error_not_configured() {
        let e =
            classify_http_status_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, "not configured");
        assert!(matches!(e, McpError::NotConfigured(_)));
    }

    #[test]
    fn test_parse_sandbox_profile() {
        assert_eq!(parse_sandbox_profile("strict"), McpSandboxProfile::Strict);
        assert_eq!(parse_sandbox_profile("relaxed"), McpSandboxProfile::Relaxed);
        assert_eq!(
            parse_sandbox_profile("something-else"),
            McpSandboxProfile::Balanced
        );
    }

    #[test]
    fn test_command_allowed_by_profile() {
        let allowlist = vec!["python3".to_string()];
        assert!(command_allowed_by_profile(
            McpSandboxProfile::Balanced,
            "python3",
            &allowlist
        ));
        assert!(!command_allowed_by_profile(
            McpSandboxProfile::Balanced,
            "/bin/sh",
            &[]
        ));
        assert!(command_allowed_by_profile(
            McpSandboxProfile::Relaxed,
            "/bin/sh",
            &[]
        ));
        assert!(!command_allowed_by_profile(
            McpSandboxProfile::Strict,
            "node",
            &[]
        ));
    }

    #[test]
    fn test_sanitize_child_env_balanced_removes_secrets() {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("OPENAI_API_KEY".to_string(), "secret".to_string());
        env.insert("CUSTOM_FLAG".to_string(), "1".to_string());
        let sanitized = sanitize_child_env(&env, McpSandboxProfile::Balanced);
        assert!(sanitized.contains_key("PATH"));
        assert!(sanitized.contains_key("CUSTOM_FLAG"));
        assert!(!sanitized.contains_key("OPENAI_API_KEY"));
    }

    #[test]
    fn test_sanitize_child_env_strict_allows_whitelist_only() {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("HOME".to_string(), "/tmp".to_string());
        env.insert("CUSTOM_FLAG".to_string(), "1".to_string());
        env.insert("MCP_ENDPOINT".to_string(), "x".to_string());
        let sanitized = sanitize_child_env(&env, McpSandboxProfile::Strict);
        assert!(sanitized.contains_key("PATH"));
        assert!(sanitized.contains_key("HOME"));
        assert!(sanitized.contains_key("MCP_ENDPOINT"));
        assert!(!sanitized.contains_key("CUSTOM_FLAG"));
    }

    #[test]
    fn test_enforce_message_size() {
        assert!(enforce_message_size(128, 256).is_ok());
        let err = enforce_message_size(257, 256).expect_err("must fail");
        assert!(matches!(err, McpError::Protocol { .. }));
    }

    /// Reads one HTTP request from `listener`, body included, and answers
    /// `reply`; returns the request's head, lower-cased. The reply says
    /// `Connection: close`, as the connection is dropped after it: a client
    /// that pooled it could send its next request there and fail. The whole
    /// request is read first, as dropping a connection with unread bytes
    /// resets it, and the client can then fail before reading the reply.
    /// Either failure leaves the next `answer_once` waiting for a connection
    /// that never comes.
    async fn answer_once(listener: &tokio::net::TcpListener, reply: String) -> String {
        use tokio::io::AsyncWriteExt;
        let (mut stream, _) = listener.accept().await.expect("accept");
        let (head, _) = read_request(&mut stream).await;
        let reply = reply.replacen("\r\n", "\r\nConnection: close\r\n", 1);
        stream
            .write_all(reply.as_bytes())
            .await
            .expect("write reply");
        head
    }

    #[tokio::test]
    async fn redirects_stay_within_the_origin() {
        let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let first_url = format!("http://{}", first.local_addr().unwrap());
        let other_url = format!("http://{}", other.local_addr().unwrap());
        let redirect = |to: &str| {
            format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: {to}\r\nContent-Length: 0\r\n\r\n"
            )
        };
        let ok = "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n".to_string();
        let message = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});

        // Within the origin the redirect is followed, header and all.
        let mut transport = HttpSseTransport::new(&format!("{first_url}/mcp"), None)
            .with_headers([("x-consumer-api-key".to_string(), "k".to_string())]);
        let server = async {
            answer_once(&first, redirect(&format!("{first_url}/moved"))).await;
            answer_once(&first, ok.clone()).await
        };
        let (sent, followed) = tokio::join!(transport.send(message.clone()), server);
        sent.expect("same-origin redirect followed");
        assert!(followed.starts_with("post /moved"), "{followed}");
        assert!(followed.contains("x-consumer-api-key: k"), "{followed}");

        // To another origin it is not, so the key never leaves.
        // (Were it followed, the other origin would never answer and the
        // send would wait; the timeout turns that into a failure.)
        let (sent, _) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                transport.send(message),
                answer_once(&first, redirect(&format!("{other_url}/mcp")))
            )
        })
        .await
        .expect("the redirect to the other origin was followed");
        assert!(sent.is_err(), "the 307 itself is the answer");
        let reached = tokio::time::timeout(Duration::from_millis(300), other.accept()).await;
        assert!(reached.is_err(), "the other origin was contacted");

        // Nor without configured headers: the session id stays home too.
        let mut transport = HttpSseTransport::new(&format!("{first_url}/mcp"), None);
        transport.session_id = Some("s-1".to_string());
        let (sent, _) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                transport.send(serde_json::json!({"jsonrpc": "2.0", "method": "ping"})),
                answer_once(&first, redirect(&format!("{other_url}/mcp")))
            )
        })
        .await
        .expect("the redirect to the other origin was followed");
        assert!(sent.is_err(), "the 307 itself is the answer");
        let reached = tokio::time::timeout(Duration::from_millis(300), other.accept()).await;
        assert!(reached.is_err(), "the other origin was contacted");
    }

    fn json_reply(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    // Each stub closes the connection after one reply, so it says so: a
    // client that pooled it would otherwise send the next request into a
    // closed socket.
    const NOT_FOUND: &str =
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    #[tokio::test]
    async fn the_first_post_goes_to_the_url_even_where_sse_would_answer() {
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", server.local_addr().unwrap());
        let mut transport = HttpSseTransport::new(&url, None);
        // The server answers any GET with 200, as one serving both kinds would.
        let (started, probe) = tokio::join!(
            transport.start(),
            answer_once(
                &server,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\n\r\n"
                    .to_string()
            )
        );
        started.expect("start");
        assert!(probe.starts_with("get /mcp "), "{probe}");
        let (sent, head) = tokio::join!(
            transport.send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"})),
            answer_once(
                &server,
                json_reply(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#)
            )
        );
        sent.expect("streamable reply");
        assert!(head.starts_with("post /mcp "), "{head}");
        assert!(!transport.legacy_sse);
        assert_eq!(transport.receive().await.unwrap()["id"], 1);
        // Settled: a later 404 is an error, not a reason to probe /sse.
        let (sent, _) = tokio::join!(
            transport.send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})),
            answer_once(&server, NOT_FOUND.to_string())
        );
        assert!(sent.is_err());
        let probed = tokio::time::timeout(Duration::from_millis(300), server.accept()).await;
        assert!(probed.is_err(), "a settled transport probed /sse");
    }

    #[tokio::test]
    async fn an_older_server_is_found_after_a_404_and_its_endpoint_kept_in_origin() {
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", server.local_addr().unwrap());
        let sse = |endpoint: &str| {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nmcp-message-endpoint: {endpoint}\r\nContent-Length: 0\r\n\r\n"
            )
        };
        let initialize = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});

        let mut transport = HttpSseTransport::new(&url, None);
        let script = async {
            let first = answer_once(&server, NOT_FOUND.to_string()).await;
            let probe = answer_once(&server, sse("/mcp/message")).await;
            let posted = answer_once(
                &server,
                json_reply(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            )
            .await;
            (first, probe, posted)
        };
        // Bounded: a client that fails early leaves the script waiting for
        // a connection for good.
        let (sent, (first, probe, posted)) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(transport.send(initialize.clone()), script)
        })
        .await
        .expect("the exchange with the stub waited");
        sent.expect("legacy reply");
        assert!(first.starts_with("post /mcp "), "{first}");
        assert!(probe.starts_with("get /mcp/sse "), "{probe}");
        assert!(posted.starts_with("post /mcp/message "), "{posted}");
        assert_eq!(transport.receive().await.unwrap()["id"], 1);

        // An endpoint on another origin is refused before anything is sent.
        let other_endpoint = format!("http://{}/steal", other.local_addr().unwrap());
        let mut transport = HttpSseTransport::new(&url, Some("token".to_string()));
        let script = async {
            answer_once(&server, NOT_FOUND.to_string()).await;
            answer_once(&server, sse(&other_endpoint)).await
        };
        let (sent, _) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(transport.send(initialize), script)
        })
        .await
        .expect("the exchange with the stub waited");
        let err = sent.expect_err("endpoint on another origin");
        assert!(err.to_string().contains("another origin"), "{err}");
        let reached = tokio::time::timeout(Duration::from_millis(300), other.accept()).await;
        assert!(reached.is_err(), "the other origin was contacted");
    }

    #[tokio::test]
    async fn an_sse_reply_left_open_returns_at_its_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", server.local_addr().unwrap());
        let held = tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.expect("accept");
            let mut buf = vec![0; 8192];
            let _ = stream.read(&mut buf).await;
            let events = concat!(
                "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
                "data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{}}\n\n",
            );
            stream
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n{events}")
                        .as_bytes(),
                )
                .await
                .expect("write");
            // The stream stays open.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let mut transport = HttpSseTransport::new(&url, None);
        tokio::time::timeout(
            Duration::from_secs(5),
            transport.send(serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"})),
        )
        .await
        .expect("send waited for the stream to close")
        .expect("sse reply");
        assert_eq!(
            transport.receive().await.unwrap()["method"],
            "notifications/progress"
        );
        assert_eq!(transport.receive().await.unwrap()["id"], 7);
        held.abort();
    }

    #[test]
    fn an_sse_response_ends_at_its_own_event_whatever_the_line_endings() {
        let response = "data: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}";
        for ending in ["\n\n", "\r\n\r\n", "\r\n\n", "\r\r"] {
            let progress = format!("data: {{\"jsonrpc\":\"2.0\",\"method\":\"p\"}}{ending}");
            let head = format!("{progress}{response}{ending}");
            // What follows: half of a 2-byte character, then a large event.
            let mut body = head.clone().into_bytes();
            body.extend_from_slice(&"\u{e9}".as_bytes()[..1]);
            body.extend_from_slice(format!("data: {}", "x".repeat(4096)).as_bytes());
            let end = SseResponseFinder::default()
                .find(&body)
                .unwrap()
                .expect("response found");
            // At the blank line's end (a CRLF one ends at its CR).
            assert!(
                end <= head.len() && head.len() - end <= 1,
                "{ending:?}: {end}"
            );
            let messages =
                http_reply_messages(std::str::from_utf8(&body[..end]).unwrap(), true).unwrap();
            assert_eq!(messages.len(), 2, "{ending:?}");
            assert_eq!(messages[1]["id"], 3, "{ending:?}");

            // Arriving a byte at a time, so every split falls somewhere,
            // including between a CR and its LF: the same end.
            let mut finder = SseResponseFinder::default();
            let found = (0..=body.len()).find_map(|k| finder.find(&body[..k]).unwrap());
            assert_eq!(found, Some(end), "{ending:?}");
        }
        // Not complete yet: nothing found.
        let partial = b"data: {\"jsonrpc\":\"2.0\",\"method\":\"p\"}\n\ndata: {\"id\":";
        assert_eq!(SseResponseFinder::default().find(partial).unwrap(), None);
    }

    #[tokio::test]
    async fn the_older_kind_is_tried_after_any_4xx_but_an_authentication_refusal() {
        let sse = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\n\r\n";
        let initialize = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        for (status, tries_older) in [
            ("400 Bad Request", true),
            ("405 Method Not Allowed", true),
            ("406 Not Acceptable", true),
            ("415 Unsupported Media Type", true),
            ("401 Unauthorized", false),
            ("403 Forbidden", false),
        ] {
            let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/mcp", server.local_addr().unwrap());
            let refused = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
            let mut transport = HttpSseTransport::new(&url, None);
            if tries_older {
                let script = async {
                    answer_once(&server, refused).await;
                    let probe = answer_once(&server, sse.to_string()).await;
                    let posted = answer_once(
                        &server,
                        json_reply(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
                    )
                    .await;
                    (probe, posted)
                };
                let (sent, (probe, posted)) =
                    tokio::join!(transport.send(initialize.clone()), script);
                sent.expect(status);
                assert!(probe.starts_with("get /mcp/sse "), "{status}: {probe}");
                assert!(
                    posted.starts_with("post /mcp/message "),
                    "{status}: {posted}"
                );
            } else {
                let (sent, _) = tokio::join!(
                    transport.send(initialize.clone()),
                    answer_once(&server, refused)
                );
                assert!(sent.is_err(), "{status}");
                let probed =
                    tokio::time::timeout(Duration::from_millis(300), server.accept()).await;
                assert!(probed.is_err(), "{status} probed /sse");
            }
        }
    }

    #[tokio::test]
    async fn an_open_sse_reply_with_a_split_character_after_its_response_is_read() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", server.local_addr().unwrap());
        let held = tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.expect("accept");
            let mut buf = vec![0; 8192];
            let _ = stream.read(&mut buf).await;
            let mut reply = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n".to_vec();
            reply.extend_from_slice(b"data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{}}\r\r");
            reply.extend_from_slice(&"\u{e9}".as_bytes()[..1]);
            stream.write_all(&reply).await.expect("write");
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let mut transport = HttpSseTransport::new(&url, None);
        tokio::time::timeout(
            Duration::from_secs(5),
            transport.send(serde_json::json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list"})),
        )
        .await
        .expect("send waited for the stream to close")
        .expect("sse reply");
        assert_eq!(transport.receive().await.unwrap()["id"], 9);
        held.abort();
    }

    /// Reads one HTTP request, body included: its head, lower-cased, and its
    /// body.
    async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, String) {
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        let mut buf = vec![0; 8192];
        loop {
            let n = stream.read(&mut buf).await.expect("read request");
            bytes.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&bytes).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let head = text[..end].to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .map_or(0, |v| v.trim().parse::<usize>().expect("length"));
                if n == 0 || bytes.len() >= end + 4 + length {
                    return (head, text[end + 4..].to_string());
                }
            }
            assert!(n > 0, "connection closed mid-request");
        }
    }

    /// An older HTTP+SSE server as the 2024-11-05 specification describes
    /// it: the GET stream's first event names the message endpoint, each
    /// POST is answered `202 Accepted`, and its reply comes as a `message`
    /// event on the held stream.
    #[tokio::test]
    async fn an_older_server_replying_on_its_held_stream_is_read() {
        let (mut transport, stub) =
            older_server("text/event-stream", ENDPOINT, OlderStream::Replies).await;
        let exchange = async {
            transport
                .send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))
                .await
                .expect("initialize sent");
            let first = transport.receive().await.expect("initialize answered");
            transport
                .send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .await
                .expect("notification sent");
            transport
                .send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
                .await
                .expect("tools/list sent");
            let second = transport.receive().await.expect("tools/list answered");
            (first, second)
        };
        let (first, second) = tokio::time::timeout(Duration::from_secs(10), exchange)
            .await
            .expect("the exchange waited");
        assert!(transport.legacy_sse);
        assert_eq!(first["id"], 1);
        assert_eq!(first["result"]["posted"], 1);
        assert_eq!(second["id"], 2);
        assert_eq!(second["result"]["posted"], 3);
        stub.abort();
    }

    /// How the older server stub writes its held event stream.
    #[derive(Clone, Copy, PartialEq)]
    enum OlderStream {
        /// Names its endpoint, then sends each reply as an event.
        Replies,
        /// Names its endpoint, then holds each reply until the next and
        /// writes the 2 events at once.
        RepliesInPairs,
        /// Sends a comment every 20 ms and never names its endpoint.
        PingsBeforeEndpoint,
        /// Names its endpoint, then sends a comment every 20 ms and no reply.
        PingsInsteadOfReplies,
        /// Names its endpoint, then writes one event 40 bytes at a time and
        /// never ends it.
        UnendingEvent,
    }

    /// An older HTTP+SSE server on `server` whose stream has `content_type`
    /// and names `endpoint`, answering each POST there `202 Accepted`.
    fn spawn_older_server(
        server: tokio::net::TcpListener,
        content_type: &'static str,
        endpoint: &'static str,
        kind: OlderStream,
    ) -> tokio::task::JoinHandle<()> {
        use tokio::io::AsyncWriteExt;
        tokio::spawn(async move {
            let (replies, to_stream) = tokio::sync::mpsc::unbounded_channel::<Value>();
            let mut to_stream = Some(to_stream);
            let mut posted = 0;
            loop {
                let (mut stream, _) = server.accept().await.expect("accept");
                let (head, body) = read_request(&mut stream).await;
                if head.starts_with("get /mcp/sse ") {
                    let mut to_stream = to_stream.take().expect("one event stream");
                    tokio::spawn(async move {
                        let mut opening =
                            format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\r\n");
                        if kind != OlderStream::PingsBeforeEndpoint {
                            opening.push_str(": a comment first\r\n\r\n");
                            opening
                                .push_str(&format!("event: endpoint\r\ndata: {endpoint}\r\n\r\n"));
                        }
                        stream
                            .write_all(opening.as_bytes())
                            .await
                            .expect("write stream head");
                        let event = |reply: Value| format!("event: message\ndata: {reply}\n\n");
                        match kind {
                            OlderStream::Replies => {
                                while let Some(reply) = to_stream.recv().await {
                                    let _ = stream.write_all(event(reply).as_bytes()).await;
                                }
                            }
                            OlderStream::RepliesInPairs => {
                                while let (Some(a), Some(b)) =
                                    (to_stream.recv().await, to_stream.recv().await)
                                {
                                    let both = event(a) + &event(b);
                                    let _ = stream.write_all(both.as_bytes()).await;
                                }
                            }
                            OlderStream::PingsBeforeEndpoint
                            | OlderStream::PingsInsteadOfReplies => {
                                while stream.write_all(b": ping\n\n").await.is_ok() {
                                    tokio::time::sleep(Duration::from_millis(20)).await;
                                }
                            }
                            OlderStream::UnendingEvent => {
                                let _ = stream.write_all(b"data: ").await;
                                while stream.write_all(&[b'x'; 40]).await.is_ok() {
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                }
                            }
                        }
                    });
                } else if head.starts_with("post /mcp/messages?session_id=s1 ") {
                    let message: Value = serde_json::from_str(&body).expect("posted json");
                    posted += 1;
                    if let Some(id) = message.get("id") {
                        let _ = replies.send(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"posted": posted}}));
                    }
                    stream
                        .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 8\r\nConnection: close\r\n\r\nAccepted")
                        .await
                        .expect("write 202");
                } else {
                    stream
                        .write_all(NOT_FOUND.as_bytes())
                        .await
                        .expect("write 404");
                }
            }
        })
    }

    /// Sends initialize, the initialized notification and tools/list, then
    /// reads 2 replies.
    async fn exchange_three(transport: &mut HttpSseTransport) -> Result<(Value, Value), McpError> {
        transport
            .send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))
            .await?;
        transport
            .send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await?;
        transport
            .send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .await?;
        Ok((transport.receive().await?, transport.receive().await?))
    }

    async fn older_server(
        content_type: &'static str,
        endpoint: &'static str,
        kind: OlderStream,
    ) -> (HttpSseTransport, tokio::task::JoinHandle<()>) {
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", server.local_addr().unwrap());
        let stub = spawn_older_server(server, content_type, endpoint, kind);
        (HttpSseTransport::new(&url, None), stub)
    }

    const ENDPOINT: &str = "/mcp/messages?session_id=s1";

    #[tokio::test]
    async fn an_older_server_whose_content_type_is_in_another_case_is_read() {
        let (mut transport, stub) = older_server(
            "Text/Event-Stream; charset=utf-8",
            ENDPOINT,
            OlderStream::Replies,
        )
        .await;
        let (first, second) =
            tokio::time::timeout(Duration::from_secs(10), exchange_three(&mut transport))
                .await
                .expect("the exchange waited")
                .expect("the exchange");
        assert_eq!(first["id"], 1);
        assert_eq!(second["id"], 2);
        stub.abort();
    }

    #[tokio::test]
    async fn older_server_events_are_held_to_the_size_limit_one_by_one() {
        let (mut transport, stub) =
            older_server("text/event-stream", ENDPOINT, OlderStream::RepliesInPairs).await;
        // Each event is about 70 bytes; the 2 written together are not.
        transport.max_message_bytes = 100;
        let (first, second) =
            tokio::time::timeout(Duration::from_secs(10), exchange_three(&mut transport))
                .await
                .expect("the exchange waited")
                .expect("2 events under the limit, read in one chunk");
        assert_eq!(first["id"], 1);
        assert_eq!(second["id"], 2);
        stub.abort();
    }

    #[tokio::test]
    async fn an_older_server_event_that_never_ends_is_refused_once_over_the_limit() {
        let (mut transport, stub) =
            older_server("text/event-stream", ENDPOINT, OlderStream::UnendingEvent).await;
        // Each chunk is under the limit; the event they add up to is not.
        transport.max_message_bytes = 100;
        let refused = tokio::time::timeout(Duration::from_secs(10), exchange_three(&mut transport))
            .await
            .expect("the exchange waited");
        assert!(
            matches!(refused, Err(McpError::Protocol { code: -32600, .. })),
            "{refused:?}"
        );
        stub.abort();
    }

    #[tokio::test]
    async fn an_older_server_event_over_the_size_limit_is_refused() {
        let (mut transport, stub) =
            older_server("text/event-stream", ENDPOINT, OlderStream::Replies).await;
        transport.max_message_bytes = 60;
        let refused = tokio::time::timeout(Duration::from_secs(10), exchange_three(&mut transport))
            .await
            .expect("the exchange waited");
        assert!(refused.is_err(), "{refused:?}");
        stub.abort();
    }

    #[tokio::test]
    async fn comments_do_not_extend_the_wait_for_an_older_server_endpoint() {
        let (mut transport, stub) = older_server(
            "text/event-stream",
            ENDPOINT,
            OlderStream::PingsBeforeEndpoint,
        )
        .await;
        transport.connect_timeout = Duration::from_millis(300);
        let sent = tokio::time::timeout(
            Duration::from_secs(5),
            transport.send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"})),
        )
        .await;
        assert!(sent.is_ok(), "comments held the endpoint wait open");
        stub.abort();
    }

    #[tokio::test]
    async fn comments_do_not_extend_the_wait_for_an_older_server_reply() {
        let (mut transport, stub) = older_server(
            "text/event-stream",
            ENDPOINT,
            OlderStream::PingsInsteadOfReplies,
        )
        .await;
        transport.sse_read_timeout = Duration::from_millis(300);
        transport
            .send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}))
            .await
            .expect("initialize sent");
        let received = tokio::time::timeout(Duration::from_secs(5), transport.receive())
            .await
            .expect("comments held the reply wait open");
        assert!(received.is_err(), "{received:?}");
        stub.abort();
    }

    #[tokio::test]
    async fn an_older_server_endpoint_on_another_origin_is_refused() {
        let (mut transport, stub) = older_server(
            "text/event-stream",
            "http://elsewhere.invalid/messages",
            OlderStream::Replies,
        )
        .await;
        let sent = tokio::time::timeout(
            Duration::from_secs(10),
            transport.send(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"})),
        )
        .await
        .expect("the send waited");
        assert!(sent.is_err(), "{sent:?}");
        stub.abort();
    }
}
