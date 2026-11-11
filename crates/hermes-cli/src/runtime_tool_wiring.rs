//! Runtime wiring helpers for swapping signal backends to live implementations.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hermes_config::GatewayConfig;
use hermes_core::ToolHandler;
use hermes_cron::{CronScheduler, ScheduledCronjobBackend};
use hermes_gateway::tool_backends::{
    ChannelClarifyBackend, ClarifyDispatcher, GatewayMessagingBackend,
};
use hermes_gateway::Gateway;
use hermes_mcp::{McpManager, McpServerConfig};
use hermes_tools::tools::clarify::{ClarifyBackend, ClarifyHandler};
use hermes_tools::tools::cronjob::CronjobHandler;
use hermes_tools::tools::messaging::SendMessageHandler;
use hermes_tools::ToolRegistry;
use serde_json::json;

use crate::mcp_config::{load_mcp_config_if_exists, McpConfig};

/// Upper bound on MCP discovery when a session starts: the 60-second default
/// `connect_timeout` given in the Nix setup and bundled MCP skill docs.
const MCP_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

fn register_runtime_tool(
    tool_registry: &Arc<ToolRegistry>,
    name: &str,
    toolset: &str,
    handler: Arc<dyn ToolHandler>,
    description: &str,
    emoji: &str,
) {
    // Runtime backends intentionally replace baseline tool handlers.
    // Deregister first to avoid noisy overwrite warnings in interactive TUI sessions.
    let _ = tool_registry.deregister(name);
    let schema = handler.schema();
    tool_registry.register(
        name,
        toolset,
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        description,
        emoji,
        None,
    );
}

/// Replace `send_message` signal backend with a live gateway-backed sender.
pub fn wire_gateway_messaging_backend(tool_registry: &Arc<ToolRegistry>, gateway: Arc<Gateway>) {
    let backend = Arc::new(GatewayMessagingBackend::new(gateway));
    let handler: Arc<dyn ToolHandler> = Arc::new(SendMessageHandler::new(backend));
    register_runtime_tool(
        tool_registry,
        "send_message",
        "messaging",
        handler,
        "Send a message to a recipient on a specific platform.",
        "💬",
    );
}

/// Replace `cronjob` signal backend with a live scheduler-backed backend.
pub fn wire_cron_scheduler_backend(
    tool_registry: &Arc<ToolRegistry>,
    scheduler: Arc<CronScheduler>,
) {
    let backend = Arc::new(ScheduledCronjobBackend::new(scheduler));
    let handler: Arc<dyn ToolHandler> = Arc::new(CronjobHandler::new(backend));
    register_runtime_tool(
        tool_registry,
        "cronjob",
        "cronjob",
        handler,
        "Manage cron jobs: create, list, update, pause, resume, remove, or run scheduled tasks.",
        "⏰",
    );
}

/// Replace `clarify` signal backend with a channel-backed async clarify flow.
pub fn wire_gateway_clarify_backend(
    tool_registry: &Arc<ToolRegistry>,
    dispatcher: ClarifyDispatcher,
) {
    let backend = Arc::new(ChannelClarifyBackend::new(dispatcher));
    let handler: Arc<dyn ToolHandler> = Arc::new(ClarifyHandler::new(backend));
    register_runtime_tool(
        tool_registry,
        "clarify",
        "clarify",
        handler,
        "Ask the user a clarification question. Optionally provide choices for the user to select from.",
        "❓",
    );
}

/// Clarify backend for CLI/stdio runtimes.
///
/// Behavior:
/// - Interactive terminal: prompt for an answer.
/// - Non-interactive: use `HERMES_CLARIFY_AUTO_ANSWER` when set, otherwise first
///   choice when available, otherwise return an error.
pub struct StdioClarifyBackend;

impl StdioClarifyBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdioClarifyBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ClarifyBackend for StdioClarifyBackend {
    async fn ask(
        &self,
        question: &str,
        choices: Option<&[String]>,
    ) -> Result<String, hermes_core::ToolError> {
        let choices_vec = choices.map(|c| c.to_vec()).unwrap_or_default();
        let auto_answer = std::env::var("HERMES_CLARIFY_AUTO_ANSWER")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if auto_answer.is_some() || !std::io::stdin().is_terminal() {
            let answer = auto_answer
                .or_else(|| choices_vec.first().cloned())
                .ok_or_else(|| {
                    hermes_core::ToolError::ExecutionFailed(
                        "clarify requires interactive terminal input or HERMES_CLARIFY_AUTO_ANSWER"
                            .to_string(),
                    )
                })?;
            return Ok(json!({
                "type": "clarify_response",
                "question": question,
                "answer": answer,
                "source": "non_interactive_fallback",
            })
            .to_string());
        }

        let question_owned = question.to_string();
        let task =
            tokio::task::spawn_blocking(move || -> Result<String, hermes_core::ToolError> {
                use std::io::{self, Write};

                eprintln!("\n[clarify] {}", question_owned);
                if !choices_vec.is_empty() {
                    eprintln!("[clarify] choices:");
                    for (idx, choice) in choices_vec.iter().enumerate() {
                        eprintln!("  {}. {}", idx + 1, choice);
                    }
                }
                eprint!("[clarify] answer> ");
                io::stderr().flush().map_err(|e| {
                    hermes_core::ToolError::ExecutionFailed(format!("clarify flush failed: {e}"))
                })?;

                let mut input = String::new();
                io::stdin().read_line(&mut input).map_err(|e| {
                    hermes_core::ToolError::ExecutionFailed(format!("clarify read failed: {e}"))
                })?;

                let trimmed = input.trim();
                if trimmed.is_empty() {
                    return choices_vec.first().cloned().ok_or_else(|| {
                        hermes_core::ToolError::ExecutionFailed(
                            "empty clarify response with no fallback choices".to_string(),
                        )
                    });
                }
                if let Ok(n) = trimmed.parse::<usize>() {
                    if n >= 1 && n <= choices_vec.len() {
                        return Ok(choices_vec[n - 1].clone());
                    }
                }
                Ok(trimmed.to_string())
            })
            .await
            .map_err(|e| {
                hermes_core::ToolError::ExecutionFailed(format!("clarify join failed: {e}"))
            })??;

        Ok(json!({
            "type": "clarify_response",
            "question": question,
            "answer": task,
            "source": "stdio",
        })
        .to_string())
    }
}

/// Replace `clarify` signal backend with a stdio-backed clarify flow.
pub fn wire_stdio_clarify_backend(tool_registry: &Arc<ToolRegistry>) {
    let handler: Arc<dyn ToolHandler> =
        Arc::new(ClarifyHandler::new(Arc::new(StdioClarifyBackend::new())));
    register_runtime_tool(
        tool_registry,
        "clarify",
        "clarify",
        handler,
        "Ask the user a clarification question. Optionally provide choices for the user to select from.",
        "❓",
    );
}

/// Launch configs for the servers listed under `mcp_servers` in config.yaml.
///
/// config.yaml holds each server's URL or command line. `hermes mcp add` also
/// writes a `mcp_servers.json` record, which alone keeps `args`, `env` and
/// `enabled`; a same-named record applies only while it names the same target.
fn session_mcp_server_configs(
    servers: &[hermes_config::McpServerEntry],
    records: Option<&McpConfig>,
) -> Vec<(String, McpServerConfig)> {
    let mut configs = Vec::new();
    for server in servers {
        let name = server.name.trim();
        if name.is_empty() {
            tracing::warn!("Skipping an MCP server in config.yaml with no name");
            continue;
        }
        let url = server
            .url
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let command = server
            .command
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let (mut config, target) = match (url, command) {
            (Some(url), _) => (McpServerConfig::http(url), url),
            (None, Some(command)) => (McpServerConfig::stdio(command, Vec::new()), command),
            (None, None) => {
                tracing::warn!("Skipping MCP server '{name}': it has neither 'command' nor 'url'");
                continue;
            }
        };
        config.supports_parallel_tool_calls = server.supports_parallel_tool_calls;
        config.keepalive_interval = server.keepalive_interval;

        if let Some(record) = records.and_then(|records| records.get(name)) {
            let record_command = record.command.as_deref().unwrap_or_default();
            let record_line = std::iter::once(record_command)
                .chain(record.args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            // A config.yaml command naming an existing file is the whole
            // executable path, never a command line the record may split.
            let same_line = target == record_line && !Path::new(target).is_file();
            let same_target = match record.url.as_deref() {
                Some(record_url) => url == Some(record_url),
                None => url.is_none() && (target == record_command || same_line),
            };
            if !same_target {
                tracing::warn!(
                    "MCP server '{name}': mcp_servers.json names a different target; using config.yaml"
                );
            } else if !record.enabled {
                continue;
            } else {
                if url.is_none() {
                    config.command = Some(record_command.to_string());
                    config.args = record.args.clone();
                }
                config.env = record.env.clone().into_iter().collect();
            }
        }
        configs.push((name.to_string(), config));
    }
    configs
}

/// Connect the MCP servers configured in config.yaml and register their tools
/// in `tool_registry`, as ACP does for the servers its client supplies.
///
/// Servers that fail or time out are logged and skipped. Each registered tool
/// holds its own client, so connections last as long as `tool_registry`.
pub async fn connect_configured_mcp_servers(
    config: &GatewayConfig,
    state_root: &Path,
    tool_registry: &Arc<ToolRegistry>,
) {
    if config.mcp_servers.is_empty() {
        return;
    }
    let records =
        load_mcp_config_if_exists(&state_root.join("mcp_servers.json")).unwrap_or_else(|err| {
            tracing::warn!("Ignoring MCP launch records: {err}");
            None
        });
    let servers = session_mcp_server_configs(&config.mcp_servers, records.as_ref());
    if servers.is_empty() {
        return;
    }
    let mut manager = McpManager::new(Arc::clone(tool_registry));
    let discovery = manager.connect_all_parallel(servers);
    if tokio::time::timeout(MCP_STARTUP_TIMEOUT, discovery)
        .await
        .is_err()
    {
        tracing::warn!(
            "MCP discovery timed out after {}s; continuing with {:?}",
            MCP_STARTUP_TIMEOUT.as_secs(),
            manager.connected_servers()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Mutex;

    use hermes_core::{GatewayError, ParseMode, PlatformAdapter, SendMessageOptions};
    use hermes_cron::{CronRunner, FileJobPersistence, MinimalCronLlm};
    use hermes_gateway::{gateway::GatewayConfig, DmManager, SessionManager};
    use tempfile::TempDir;

    type SentRecord = (String, String, Option<String>, bool);

    struct RecordingAdapter {
        sent: Arc<Mutex<Vec<SentRecord>>>,
        running: bool,
        platform: String,
    }

    impl RecordingAdapter {
        fn new(platform: &str) -> Self {
            Self {
                sent: Arc::new(Mutex::new(Vec::new())),
                running: true,
                platform: platform.to_string(),
            }
        }
    }

    #[async_trait]
    impl PlatformAdapter for RecordingAdapter {
        async fn start(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_message(
            &self,
            chat_id: &str,
            text: &str,
            _parse_mode: Option<ParseMode>,
        ) -> Result<(), GatewayError> {
            self.sent
                .lock()
                .expect("recording adapter lock poisoned")
                .push((chat_id.to_string(), text.to_string(), None, false));
            Ok(())
        }

        async fn send_message_with_options(
            &self,
            chat_id: &str,
            text: &str,
            _parse_mode: Option<ParseMode>,
            options: SendMessageOptions,
        ) -> Result<(), GatewayError> {
            self.sent
                .lock()
                .expect("recording adapter lock poisoned")
                .push((
                    chat_id.to_string(),
                    text.to_string(),
                    options.thread_id,
                    options.explicit_chat_id,
                ));
            Ok(())
        }

        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &str,
            _text: &str,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_file(
            &self,
            _chat_id: &str,
            _file_path: &str,
            _caption: Option<&str>,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        fn is_running(&self) -> bool {
            self.running
        }

        fn platform_name(&self) -> &str {
            &self.platform
        }
    }

    #[tokio::test]
    async fn gateway_messaging_backend_sends_real_message() {
        let session_manager = Arc::new(SessionManager::new(
            hermes_config::session::SessionConfig::default(),
        ));
        let dm = DmManager::with_pair_behavior();
        let gateway = Arc::new(Gateway::new(session_manager, dm, GatewayConfig::default()));
        let adapter = Arc::new(RecordingAdapter::new("telegram"));
        let recorder = adapter.sent.clone();
        gateway.register_adapter("telegram", adapter).await;

        let registry = Arc::new(ToolRegistry::new());
        wire_gateway_messaging_backend(&registry, gateway.clone());

        let out = registry
            .dispatch_async(
                "send_message",
                json!({
                    "platform": "telegram",
                    "recipient": "12345",
                    "message": "hello"
                }),
            )
            .await;
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("send_message output should be json");
        assert_eq!(parsed["status"], "sent");

        let sent = recorder.lock().expect("recording lock poisoned");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "12345");
        assert_eq!(sent[0].1, "hello");
        assert_eq!(sent[0].2, None);
        assert!(sent[0].3);
    }

    #[tokio::test]
    async fn gateway_messaging_backend_preserves_slack_thread_id() {
        let session_manager = Arc::new(SessionManager::new(
            hermes_config::session::SessionConfig::default(),
        ));
        let dm = DmManager::with_pair_behavior();
        let gateway = Arc::new(Gateway::new(session_manager, dm, GatewayConfig::default()));
        let adapter = Arc::new(RecordingAdapter::new("slack"));
        let recorder = adapter.sent.clone();
        gateway.register_adapter("slack", adapter).await;

        let registry = Arc::new(ToolRegistry::new());
        wire_gateway_messaging_backend(&registry, gateway.clone());

        let out = registry
            .dispatch_async(
                "send_message",
                json!({
                    "platform": "slack",
                    "recipient": "C123",
                    "message": "reply",
                    "thread_ts": "171234.5"
                }),
            )
            .await;
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("send_message output should be json");
        assert_eq!(parsed["status"], "sent");
        assert_eq!(parsed["thread_id"], "171234.5");

        let sent = recorder.lock().expect("recording lock poisoned");
        assert_eq!(
            sent.as_slice(),
            [(
                "C123".to_string(),
                "reply".to_string(),
                Some("171234.5".to_string()),
                true,
            )]
        );
    }

    #[tokio::test]
    async fn cron_scheduler_backend_handles_create_and_list() {
        let tmp = TempDir::new().expect("tempdir");
        let persistence = Arc::new(FileJobPersistence::with_dir(tmp.path().to_path_buf()));

        let empty_tools = Arc::new(crate::app::bridge_tool_registry(&ToolRegistry::new()));
        let runner = Arc::new(CronRunner::new(Arc::new(MinimalCronLlm), empty_tools));
        let scheduler = Arc::new(CronScheduler::new(persistence, runner));

        let registry = Arc::new(ToolRegistry::new());
        wire_cron_scheduler_backend(&registry, scheduler);

        let create = registry
            .dispatch_async(
                "cronjob",
                json!({
                    "action": "create",
                    "name": "daily-test",
                    "schedule": "0 9 * * *",
                    "task": "ping",
                }),
            )
            .await;
        let created: serde_json::Value =
            serde_json::from_str(&create).expect("create output should be json");
        assert_eq!(created["action"], "created");

        let list = registry
            .dispatch_async("cronjob", json!({"action": "list"}))
            .await;
        let listed: serde_json::Value =
            serde_json::from_str(&list).expect("list output should be json");
        assert_eq!(listed["action"], "list");
        assert_eq!(listed["count"], 1);
    }

    fn yaml_server(
        name: &str,
        command: Option<&str>,
        url: Option<&str>,
    ) -> hermes_config::McpServerEntry {
        hermes_config::McpServerEntry {
            name: name.to_string(),
            command: command.map(str::to_string),
            url: url.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn session_mcp_servers_launch_config_yaml_entries() {
        let configs = session_mcp_server_configs(
            &[
                yaml_server("pentagi", Some("/opt/pentagi-mcp"), None),
                yaml_server("remote", None, Some("https://mcp.example.com/mcp")),
                yaml_server("unconfigured", None, None),
                yaml_server(" ", Some("nameless-mcp"), None),
            ],
            None,
        );

        assert_eq!(
            configs,
            vec![
                (
                    "pentagi".to_string(),
                    McpServerConfig::stdio("/opt/pentagi-mcp", Vec::new()),
                ),
                (
                    "remote".to_string(),
                    McpServerConfig::http("https://mcp.example.com/mcp"),
                ),
            ]
        );
    }

    #[test]
    fn session_mcp_servers_take_launch_details_from_matching_json_records() {
        let records = crate::mcp_config::parse_mcp_config_json(
            r#"{
              "sentrux": {"command": "sentrux", "args": ["mcp"], "env": {"RUST_LOG": "warn"}},
              "node": {"command": "node", "args": ["server.mjs"]},
              "off": {"command": "off-mcp", "enabled": false},
              "moved": {"command": "old-mcp", "enabled": false},
              "json-only": {"command": "unlisted-mcp"}
            }"#,
        )
        .expect("valid records");

        let configs = session_mcp_server_configs(
            &[
                yaml_server("sentrux", Some("sentrux mcp"), None),
                yaml_server("node", Some("node"), None),
                yaml_server("off", Some("off-mcp"), None),
                yaml_server("moved", None, Some("https://mcp.example.com/mcp")),
            ],
            Some(&records),
        );

        assert_eq!(
            configs,
            vec![
                (
                    "sentrux".to_string(),
                    McpServerConfig::stdio("sentrux", vec!["mcp".to_string()])
                        .with_env("RUST_LOG", "warn"),
                ),
                (
                    "node".to_string(),
                    McpServerConfig::stdio("node", vec!["server.mjs".to_string()]),
                ),
                (
                    "moved".to_string(),
                    McpServerConfig::http("https://mcp.example.com/mcp"),
                ),
            ]
        );
    }

    #[test]
    fn session_mcp_servers_keep_an_existing_executable_path_with_spaces() {
        let dir = TempDir::new().expect("temp dir");
        let executable = dir.path().join("My MCP");
        std::fs::write(&executable, b"").expect("write executable");
        let executable = executable.to_str().expect("UTF-8 path");
        let (command, arg) = executable.rsplit_once(' ').expect("path has a space");
        // The record splits the path into a different executable and argument.
        let records = crate::mcp_config::parse_mcp_config_json(
            &serde_json::json!({
                "spaced": {"command": command, "args": [arg], "enabled": false}
            })
            .to_string(),
        )
        .expect("valid records");

        let configs = session_mcp_server_configs(
            &[yaml_server("spaced", Some(executable), None)],
            Some(&records),
        );

        assert_eq!(
            configs,
            vec![(
                "spaced".to_string(),
                McpServerConfig::stdio(executable, Vec::new()),
            )]
        );
    }

    #[tokio::test]
    async fn stdio_clarify_backend_uses_auto_answer_in_non_interactive_mode() {
        std::env::set_var("HERMES_CLARIFY_AUTO_ANSWER", "from-env");
        let backend = StdioClarifyBackend::new();
        let out = backend
            .ask("pick?", Some(&["a".to_string(), "b".to_string()]))
            .await
            .expect("clarify should succeed with env override");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("json output");
        assert_eq!(parsed["answer"], "from-env");
        std::env::remove_var("HERMES_CLARIFY_AUTO_ANSWER");
    }
}
