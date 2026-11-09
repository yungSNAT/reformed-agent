//! CLI argument parsing using clap (Requirement 9.7).
//!
//! Defines the command-line interface for the hermes binaries.

use clap::{Args, Parser, Subcommand};

// ---------------------------------------------------------------------------
// CliCommand
// ---------------------------------------------------------------------------

// A subcommand with arguments keeps them in its own `Args` struct. Clap's derive
// builds the arguments of every inline variant in one function, and in a debug
// build that function's stack frame grew to 1.8 MiB, most of the 2 MiB a test
// thread or Tokio worker gets.

/// Top-level subcommands for the hermes CLI.
#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    /// Start an interactive session (default when no subcommand is given).
    #[command(name = "hermes")]
    Hermes,

    /// Show or set the current model.
    ///
    /// Examples:
    ///   hermes model                    — show current model
    ///   hermes model openai:gpt-4o      — switch to gpt-4o via openai provider
    Model(ModelArgs),

    /// List or manage available tools.
    ///
    /// Examples:
    ///   hermes tools                    — list all registered tools
    ///   hermes tools enable web_search  — enable a specific tool
    ///   hermes tools disable bash       — disable a specific tool
    Tools(ToolsArgs),

    /// Manage the Computer Use cua-driver backend.
    #[command(name = "computer-use")]
    ComputerUse(ComputerUseArgs),

    /// Configuration management.
    ///
    /// Examples:
    ///   hermes config                   — show full configuration
    ///   hermes config get model         — get a specific config key
    ///   hermes config set model gpt-4o  — set a config key
    Config(ConfigArgs),

    /// Start or manage the gateway server.
    ///
    /// Examples:
    ///   hermes gateway start            — start the gateway
    ///   hermes gateway status           — check gateway status
    Gateway(GatewayArgs),

    /// Ensure the always-on Hermes service surface is installed/running when supported.
    Up(UpArgs),

    /// Run the interactive setup wizard.
    Setup(SetupArgs),

    /// Nous Portal OAuth setup and status.
    ///
    /// Examples:
    ///   hermes portal          — run Nous Portal device-code login
    ///   hermes portal setup    — run Nous Portal device-code login
    ///   hermes portal info     — show Nous Portal auth status
    Portal(PortalArgs),

    /// Nous Portal billing overview and explicit-confirmation billing actions.
    Billing(BillingArgs),

    /// Check dependencies and configuration health.
    Doctor(DoctorArgs),

    /// Check for updates.
    Update(UpdateArgs),

    /// Run consolidated elite diagnostics and release gates.
    EliteCheck(EliteCheckArgs),

    /// Inspect implemented system surfaces: release, replay, MCP, ACP, handoff, providers, provenance.
    Systems(SystemsArgs),

    /// Verify signed provenance sidecar for an artifact.
    VerifyProvenance(VerifyProvenanceArgs),

    /// Rotate the local provenance signing key.
    RotateProvenanceKey(RotateProvenanceKeyArgs),

    /// Inspect or reset smart-routing learning state.
    RouteLearning(RouteLearningArgs),

    /// Inspect computed smart-router health tiers from learned route telemetry.
    RouteHealth(RouteHealthArgs),

    /// Compute and optionally apply smart-routing policy overrides from learned route health.
    RouteAutotune(RouteAutotuneArgs),

    /// Build an incident support bundle with replay/provenance diagnostics.
    IncidentPack(IncidentPackArgs),

    /// Show running status (active sessions, model, uptime).
    Status,

    /// Manage the local Kanban board without starting an interactive session.
    Kanban(KanbanArgs),

    /// Start the dashboard-compatible local HTTP UI/API helper.
    Dashboard(DashboardArgs),

    /// Start the headless dashboard-compatible HTTP API helper.
    Serve(DashboardArgs),

    /// Debug diagnostics and support report helpers.
    Debug(DebugArgs),

    /// Show recent logs.
    ///
    /// Examples:
    ///   hermes logs              — show last 20 log entries
    ///   hermes logs 50           — show last 50 log entries
    ///   hermes logs --follow     — tail logs in real-time
    Logs(LogsArgs),

    /// Profile management (list, switch, create).
    ///
    /// Examples:
    ///   hermes profile              — show current profile
    ///   hermes profile list         — list all profiles
    ///   hermes profile create work  — create a new profile named "work"
    ///   hermes profile switch work  — switch to the "work" profile
    Profile(ProfileArgs),

    /// Authentication management.
    Auth(AuthArgs),

    /// Encrypted secret vault management.
    Secrets(SecretsArgs),

    /// Cron management commands.
    Cron(CronArgs),

    /// Webhook management commands.
    Webhook(WebhookArgs),

    /// Cloudflare workflow helpers.
    Cloudflare(CloudflareArgs),

    /// Microsoft Teams meeting summary pipeline.
    TeamsPipeline(TeamsPipelineArgs),

    /// Start an interactive chat session.
    Chat(ChatArgs),

    /// Skills management.
    Skills(SkillsArgs),

    /// Plugin management.
    Plugins(PluginsArgs),

    /// Memory provider management.
    Memory(MemoryArgs),

    /// MCP server management.
    Mcp(McpArgs),

    /// Session management.
    Sessions(SessionsArgs),

    /// Resume an interactive session from saved session state.
    Resume(ResumeArgs),

    /// Usage analytics and insights.
    Insights(InsightsArgs),

    /// Login to a provider.
    Login(LoginArgs),

    /// Logout from a provider.
    Logout(LogoutArgs),

    /// WhatsApp-specific configuration.
    Whatsapp(WhatsappArgs),

    /// Device pairing management.
    Pairing(PairingArgs),

    /// OpenClaw migration utilities.
    Claw(ClawArgs),

    /// ACP (Agent Communication Protocol) server.
    Acp(AcpArgs),

    /// Backup configuration and sessions.
    Backup(BackupArgs),

    /// Import configuration from backup.
    Import(ImportArgs),

    /// Convert a Hermes JSONL session export into resumable snapshots in a new profile.
    ImportHermesSessions(ImportHermesSessionsArgs),

    /// Show version information.
    Version,

    /// Export conversation/session dump.
    Dump(DumpArgs),

    /// Generate shell completion scripts.
    Completion(CompletionArgs),

    /// Uninstall helper (removes ~/.hermes by default).
    Uninstall(UninstallArgs),

    /// Lumio API Gateway login and setup.
    ///
    /// Examples:
    ///   hermes lumio                    — login to Lumio via OAuth
    ///   hermes lumio --model gpt-4o     — login and set model
    ///   hermes lumio logout             — remove saved Lumio token
    ///   hermes lumio status             — show current Lumio login status
    Lumio(LumioArgs),

    /// Reserved external command surface.
    ///
    /// Reformed Agent is Rust-only at runtime, so external Python plugin
    /// command bridges are rejected instead of being executed dynamically.
    #[command(external_subcommand)]
    PluginExternal(Vec<String>),
}

#[derive(Debug, Clone, Args)]
pub struct ModelArgs {
    /// Print provider:model completion candidates, one per line.
    #[arg(long, hide = true, conflicts_with = "completion_providers")]
    pub completion_values: bool,
    /// Print provider completion candidates, one per line.
    #[arg(long, hide = true)]
    pub completion_providers: bool,
    /// Provider:model identifier (e.g. "openai:gpt-4o", "anthropic:claude-3-opus").
    pub provider_model: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct ToolsArgs {
    /// Action: "list", "enable <name>", or "disable <name>".
    pub action: Option<String>,
    /// Tool name used by enable/disable actions.
    pub name: Option<String>,
    /// Optional platform scope for tool toggles.
    #[arg(long)]
    pub platform: Option<String>,
    /// Print compact per-platform summary (upstream parity alias).
    #[arg(long)]
    pub summary: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ComputerUseArgs {
    /// Action: status, doctor, manifest, install-hint.
    pub action: Option<String>,
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,
    /// Run only named doctor checks. Repeat for multiple checks.
    #[arg(long = "include")]
    pub include: Vec<String>,
    /// Skip named doctor checks. Repeat for multiple checks.
    #[arg(long = "skip")]
    pub skip: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct ConfigArgs {
    /// Action: "get", "set", or omitted to show all.
    pub action: Option<String>,
    /// Configuration key (e.g. "model", "max_turns").
    pub key: Option<String>,
    /// Configuration value (used with "set" action).
    pub value: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct GatewayArgs {
    /// Action: "run", "start", "stop", "restart", "status", "install", "uninstall", "setup", or "migrate-legacy".
    pub action: Option<String>,
    /// Compatibility flag for stale upstream docs; gateway lifecycle commands operate on all configured adapters.
    #[arg(long, hide = true)]
    pub platform: Option<String>,
    /// Target system-level service scope when supported.
    #[arg(long)]
    pub system: bool,
    /// Apply action across all known gateway processes.
    #[arg(long)]
    pub all: bool,
    /// Force reinstall/overwrite for install action.
    #[arg(long)]
    pub force: bool,
    /// Linux system service run-as user.
    #[arg(long)]
    pub run_as_user: Option<String>,
    /// Replace existing foreground process when running.
    #[arg(long)]
    pub replace: bool,
    /// Dry-run for migration/install checks.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip confirmation prompts.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Deep status checks.
    #[arg(long)]
    pub deep: bool,
}

#[derive(Debug, Clone, Args)]
pub struct UpArgs {
    /// Reinstall/overwrite the user service definition when supported.
    #[arg(long)]
    pub force: bool,
    /// Show the service plan without writing service files or starting anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SetupArgs {
    /// One-shot Nous Portal OAuth setup.
    #[arg(long)]
    pub portal: bool,
}

#[derive(Debug, Clone, Args)]
pub struct PortalArgs {
    /// Action: "setup", "login", "info", or "status".
    pub action: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct BillingArgs {
    /// Billing action and arguments. Use `hermes billing help`.
    #[arg(
        value_name = "ARGS",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    /// Run deeper diagnostics (gateway/runtime/memory endpoints).
    #[arg(long)]
    pub deep: bool,
    /// Perform safe local remediations (state dirs, stale pid, token perms).
    #[arg(long)]
    pub self_heal: bool,
    /// Write a machine-readable doctor snapshot JSON.
    #[arg(long)]
    pub snapshot: bool,
    /// Override snapshot output path.
    #[arg(long)]
    pub snapshot_path: Option<String>,
    /// Build a support bundle tar.gz with diagnostics artifacts.
    #[arg(long)]
    pub bundle: bool,
}

#[derive(Debug, Clone, Args)]
pub struct UpdateArgs {
    /// Compatibility flag for upstream parity (`hermes update --check`).
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Clone, Args)]
pub struct EliteCheckArgs {
    /// Print machine-readable JSON only.
    #[arg(long)]
    pub json: bool,
    /// Return non-zero on any failing elite gate.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SystemsArgs {
    /// Action: status/release/replay/mcp/acp/providers/handoff/provenance/agent-card.
    pub action: Option<String>,
    /// Optional action topic (e.g. mcp conformance, acp conformance, handoff template).
    pub topic: Option<String>,
    /// Print machine-readable JSON.
    #[arg(long)]
    pub json: bool,
    /// Write the JSON report/card to this path.
    #[arg(long)]
    pub output: Option<String>,
    /// Host for `systems agent-card serve`.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Port for `systems agent-card serve`.
    #[arg(long, default_value_t = 9127)]
    pub port: u16,
    /// Serve one agent-card HTTP request, then exit.
    #[arg(long)]
    pub once: bool,
}

#[derive(Debug, Clone, Args)]
pub struct VerifyProvenanceArgs {
    /// Artifact path to verify (e.g. doctor snapshot or replay manifest).
    pub path: String,
    /// Optional signature sidecar path override.
    #[arg(long)]
    pub signature: Option<String>,
    /// Enforce non-zero failure for any verification violation.
    #[arg(long)]
    pub strict: bool,
    /// Print compact machine-readable JSON only.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct RotateProvenanceKeyArgs {
    /// Print compact machine-readable JSON only.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct RouteLearningArgs {
    /// Action: show/list/reset/clear
    pub action: Option<String>,
    /// Print machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct RouteHealthArgs {
    /// Action: show/list/reset/clear
    pub action: Option<String>,
    /// Print machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct RouteAutotuneArgs {
    /// Action: show/plan/inspect/apply/reset/clear
    pub action: Option<String>,
    /// Persist generated overrides to route-autotune.env.
    #[arg(long)]
    pub apply: bool,
    /// Fail with non-zero status when health evidence is insufficient.
    #[arg(long)]
    pub strict: bool,
    /// Print machine-readable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct IncidentPackArgs {
    /// Optional existing doctor snapshot path.
    #[arg(long)]
    pub snapshot: Option<String>,
    /// Optional output path override (.tar.gz).
    #[arg(long)]
    pub output: Option<String>,
    /// Print machine-readable JSON summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct KanbanArgs {
    /// Kanban action and arguments. Use `hermes kanban help`.
    #[arg(
        value_name = "ARGS",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct DashboardArgs {
    /// Host bind address (default: 127.0.0.1).
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Port (default: 9119).
    #[arg(long, default_value_t = 9119)]
    pub port: u16,
    /// Do not auto-open browser.
    #[arg(long)]
    pub no_open: bool,
    /// Allow binding to non-localhost.
    #[arg(long)]
    pub insecure: bool,
}

#[derive(Debug, Clone, Args)]
pub struct DebugArgs {
    /// Action: share/delete.
    pub action: Option<String>,
    /// Optional URL argument for delete action.
    pub url: Option<String>,
    /// Number of log lines for debug share.
    #[arg(long, default_value_t = 200)]
    pub lines: u32,
    /// Expiry days for uploaded report.
    #[arg(long, default_value_t = 7)]
    pub expire: u32,
    /// Print report locally instead of uploading.
    #[arg(long)]
    pub local: bool,
}

#[derive(Debug, Clone, Args)]
pub struct LogsArgs {
    /// Number of recent log entries to show (default: 20).
    #[arg(default_value = "20")]
    pub lines: u32,
    /// Tail the log file in real-time.
    #[arg(short, long)]
    pub follow: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ProfileArgs {
    /// Action: "list", "use", "create", "delete", "show", "alias", "rename", "export", "import", or "switch" (legacy).
    pub action: Option<String>,
    /// Primary profile name or archive path depending on action.
    pub name: Option<String>,
    /// Secondary positional argument (e.g. new name for `rename`).
    pub secondary: Option<String>,
    /// Output path for `export`.
    #[arg(short, long)]
    pub output: Option<String>,
    /// Import profile name override for `import`.
    #[arg(long)]
    pub import_name: Option<String>,
    /// Optional wrapper alias name for `alias`.
    #[arg(long = "name")]
    pub alias_name: Option<String>,
    /// Remove wrapper alias for `alias`.
    #[arg(long)]
    pub remove: bool,
    /// Skip confirmation prompts.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Clone config essentials from active/source profile on create.
    #[arg(long)]
    pub clone: bool,
    /// Clone full profile state on create.
    #[arg(long = "clone-all")]
    pub clone_all: bool,
    /// Source profile when cloning.
    #[arg(long = "clone-from")]
    pub clone_from: Option<String>,
    /// Skip alias creation during profile create.
    #[arg(long = "no-alias")]
    pub no_alias: bool,
    /// Do not carry skill overrides from cloned source profiles.
    #[arg(long = "no-skills")]
    pub no_skills: bool,
}

#[derive(Debug, Clone, Args)]
pub struct AuthArgs {
    /// Action: "login", "logout", "status", "verify", "add", "list", "remove", or "reset".
    pub action: Option<String>,
    /// Provider: openai/anthropic/... / `telegram` / `weixin|wechat|wx` (write platform token to config.yaml) / `copilot`.
    /// If omitted, uses `HERMES_AUTH_DEFAULT_PROVIDER`, config model provider, or `nous`.
    pub provider: Option<String>,
    /// Action target (e.g. pooled credential index/id/label for `remove`).
    pub target: Option<String>,
    /// Credential type for `auth add` (oauth/api-key).
    #[arg(long = "type")]
    pub auth_type: Option<String>,
    /// Optional pooled credential label for `auth add`.
    #[arg(long)]
    pub label: Option<String>,
    /// API key value for `auth add` (otherwise interactive prompt).
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
    /// For Weixin/QQBot login: prefer QR flow (scan-to-auth onboard).
    #[arg(long)]
    pub qr: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SecretsArgs {
    /// Action: list/status/get/set/remove
    pub action: Option<String>,
    /// Provider key namespace (e.g. openai, anthropic, nous).
    pub provider: Option<String>,
    /// Secret value (used with `set`; if omitted, prompt is shown).
    #[arg(long)]
    pub value: Option<String>,
    /// Show full secret value for `get` (requires `HERMES_ALLOW_SECRET_STDOUT=1`; default masks output).
    #[arg(long)]
    pub show: bool,
}

#[derive(Debug, Clone, Args)]
pub struct CronArgs {
    /// Action: list/create/edit/pause/resume/run/remove/delete/history/status/tick
    pub action: Option<String>,
    /// Job id (edit/delete/pause/resume/run/history/remove).
    pub job_id: Option<String>,
    /// Job id (legacy flag alias).
    #[arg(long)]
    pub id: Option<String>,
    /// Cron schedule (create), e.g. "0 9 * * *".
    #[arg(long)]
    pub schedule: Option<String>,
    /// Prompt text (create).
    #[arg(long)]
    pub prompt: Option<String>,
    /// Optional human-friendly job name.
    #[arg(long)]
    pub name: Option<String>,
    /// Delivery target override.
    #[arg(long)]
    pub deliver: Option<String>,
    /// Delivery chat/channel id for cron result routing.
    #[arg(long = "deliver-chat-id")]
    pub deliver_chat_id: Option<String>,
    /// Repeat count override.
    #[arg(long)]
    pub repeat: Option<u32>,
    /// Replace skills list.
    #[arg(long = "skill")]
    pub skills: Vec<String>,
    /// Append skills list.
    #[arg(long = "add-skill")]
    pub add_skills: Vec<String>,
    /// Remove skills from list.
    #[arg(long = "remove-skill")]
    pub remove_skills: Vec<String>,
    /// Clear all attached skills.
    #[arg(long)]
    pub clear_skills: bool,
    /// Script path/content field.
    #[arg(long)]
    pub script: Option<String>,
    /// Run in script-only mode (skip LLM/agent loop). Requires `--script`.
    #[arg(long)]
    pub no_agent: bool,
    /// Re-enable agent execution mode when editing a script-only job.
    #[arg(long)]
    pub agent: bool,
    /// Per-job script timeout in seconds (script/no-agent mode).
    #[arg(long)]
    pub script_timeout_seconds: Option<u64>,
    /// Per-job shell override for inline scripts.
    #[arg(long)]
    pub script_shell: Option<String>,
    /// Absolute working directory for this cron job.
    #[arg(long)]
    pub workdir: Option<String>,
    /// Include inactive jobs (list action).
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Clone, Args)]
pub struct WebhookArgs {
    /// Action: subscribe/add/list/remove/rm/test.
    pub action: Option<String>,
    /// Route/subscription name.
    pub name: Option<String>,
    /// Webhook URL (add, or remove by URL).
    #[arg(long)]
    pub url: Option<String>,
    /// Entry id (remove by id).
    #[arg(long)]
    pub id: Option<String>,
    /// Prompt template for subscription routes.
    #[arg(long)]
    pub prompt: Option<String>,
    /// Comma-separated accepted events.
    #[arg(long)]
    pub events: Option<String>,
    /// Human-readable description.
    #[arg(long)]
    pub description: Option<String>,
    /// Comma-separated skills list.
    #[arg(long)]
    pub skills: Option<String>,
    /// Delivery target.
    #[arg(long)]
    pub deliver: Option<String>,
    /// Delivery chat id.
    #[arg(long = "deliver-chat-id")]
    pub deliver_chat_id: Option<String>,
    /// HMAC secret.
    #[arg(long)]
    pub secret: Option<String>,
    /// Skip agent execution and deliver prompt directly.
    #[arg(long = "deliver-only")]
    pub deliver_only: bool,
    /// Test payload JSON.
    #[arg(long)]
    pub payload: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct CloudflareArgs {
    /// Action: parse-temporary-deploy-output.
    pub action: Option<String>,
    /// Run parser self-test.
    #[arg(long)]
    pub selftest: bool,
}

#[derive(Debug, Clone, Args)]
pub struct TeamsPipelineArgs {
    /// Action: list/show/run/fetch/subscriptions/subscribe/renew-subscription/delete-subscription/maintain-subscriptions/token-health/validate.
    pub action: Option<String>,
    /// Job id or subscription id, depending on action.
    pub id: Option<String>,
    /// List limit.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Filter jobs by status.
    #[arg(long)]
    pub status: Option<String>,
    /// Override teams pipeline JSON store path.
    #[arg(long)]
    pub store_path: Option<String>,
    /// Meeting id for fetch.
    #[arg(long)]
    pub meeting_id: Option<String>,
    /// Join URL for fetch.
    #[arg(long)]
    pub join_web_url: Option<String>,
    /// Tenant id for fetch/job creation.
    #[arg(long)]
    pub tenant_id: Option<String>,
    /// Optional call record id for fetch.
    #[arg(long)]
    pub call_record_id: Option<String>,
    /// Graph subscription resource for subscribe.
    #[arg(long)]
    pub resource: Option<String>,
    /// Graph notification URL for subscribe.
    #[arg(long)]
    pub notification_url: Option<String>,
    /// Graph change type for subscribe.
    #[arg(long)]
    pub change_type: Option<String>,
    /// Graph subscription expiration timestamp.
    #[arg(long)]
    pub expiration: Option<String>,
    /// Graph subscription clientState.
    #[arg(long)]
    pub client_state: Option<String>,
    /// Graph lifecycle notification URL.
    #[arg(long)]
    pub lifecycle_notification_url: Option<String>,
    /// Latest supported TLS version for Graph subscriptions.
    #[arg(long, default_value = "v1_2")]
    pub latest_supported_tls_version: String,
    /// Force a fresh token for token-health.
    #[arg(long)]
    pub force_refresh: bool,
    /// Renewal threshold for maintain-subscriptions.
    #[arg(long, default_value_t = 24)]
    pub renew_within_hours: u32,
    /// Renewal extension for maintain-subscriptions.
    #[arg(long, default_value_t = 24)]
    pub extend_hours: u32,
    /// Dry-run maintain-subscriptions.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ChatArgs {
    /// Single-shot query (non-interactive).
    #[arg(short, long)]
    pub query: Option<String>,
    /// Preload a skill before chatting.
    #[arg(long)]
    pub preload_skill: Option<String>,
    /// Skip confirmation for dangerous tools.
    #[arg(long)]
    pub yolo: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SkillsArgs {
    /// Action: browse/search/install/inspect/list/check/update/audit/uninstall/publish/snapshot/tap/config/reset/subscribe/sync/opt-out/opt-in
    pub action: Option<String>,
    /// Skill name or search query.
    pub name: Option<String>,
    /// Additional argument (e.g. tap URL, snapshot path).
    #[arg(long)]
    pub extra: Option<String>,
    /// Also delete pristine bundled skills for `skills opt-out`.
    #[arg(long)]
    pub remove: bool,
    /// Skip confirmation for destructive skills maintenance actions.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Re-seed bundled skills immediately for `skills opt-in`.
    #[arg(long)]
    pub sync: bool,
}

#[derive(Debug, Clone, Args)]
pub struct PluginsArgs {
    /// Action: install/update/remove/list/enable/disable/inspect
    pub action: Option<String>,
    /// Plugin name.
    pub name: Option<String>,
    /// Git branch, tag, or commit to checkout after clone (remote installs only).
    #[arg(long = "ref")]
    pub git_ref: Option<String>,
    /// Allow clone from hosts outside the default allowlist (high risk).
    #[arg(long)]
    pub allow_untrusted_git_host: bool,
}

#[derive(Debug, Clone, Args)]
pub struct MemoryArgs {
    /// Action: setup/status/off/reset
    pub action: Option<String>,
    /// Provider for `memory setup` (e.g. mem0) or reset target (all|memory|user).
    pub target: Option<String>,
    /// Skip reset confirmation prompt.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Memory provider setup mode, currently used by Mem0: platform|selfhosted|oss.
    #[arg(long)]
    pub mode: Option<String>,
    /// Self-hosted memory provider host, currently used by Mem0.
    #[arg(long)]
    pub host: Option<String>,
    /// Memory provider API key, currently written to `$HERMES_HOME/.env` for Mem0.
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
    /// Show the provider setup plan without writing config or env files.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Args)]
pub struct McpArgs {
    /// Action: serve/add/remove/list/test/configure/login
    pub action: Option<String>,
    /// Server name.
    pub name: Option<String>,
    /// Legacy server name or URL option.
    #[arg(long)]
    pub server: Option<String>,
    /// URL for add action.
    #[arg(long)]
    pub url: Option<String>,
    /// Command for stdio server add action.
    #[arg(long)]
    pub command: Option<String>,
    /// Mark this MCP server as supporting parallel tool calls.
    #[arg(long)]
    pub parallel_tools: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SessionsArgs {
    /// Action: list/export/delete/prune/optimize/repair/stats/rename/browse
    pub action: Option<String>,
    /// Session ID/prefix for export/delete/rename, export output path, or day count for prune.
    pub session: Option<String>,
    /// Session ID.
    #[arg(long)]
    pub id: Option<String>,
    /// Session ID/prefix for export, matching upstream `sessions export --session-id`.
    #[arg(long = "session-id")]
    pub session_id: Option<String>,
    /// New name (for rename).
    #[arg(long)]
    pub name: Option<String>,
    /// Export format: json, jsonl, md, markdown, or html.
    #[arg(long)]
    pub format: Option<String>,
    /// Export a filtered view, currently `user-prompts`.
    #[arg(long)]
    pub only: Option<String>,
    /// Export output path. Use `-` for stdout where supported.
    #[arg(long)]
    pub output: Option<String>,
    /// Redact common secret fields and token-like strings in exported content.
    #[arg(long)]
    pub redact: bool,
    /// Confirm destructive actions without prompting.
    #[arg(long)]
    pub yes: bool,
    /// Restrict prune to sessions with this source marker.
    #[arg(long)]
    pub source: Option<String>,
    /// Prune sessions older than N days.
    #[arg(long = "older-than")]
    pub older_than: Option<u64>,
}

#[derive(Debug, Clone, Args)]
pub struct ResumeArgs {
    /// Session ID/file stem to resume, or `latest` (default).
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct InsightsArgs {
    /// Number of days to analyze.
    #[arg(long, default_value = "30")]
    pub days: u32,
    /// Filter by source.
    #[arg(long)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct LoginArgs {
    /// Provider name (openai/anthropic/nous/copilot/telegram/weixin).
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct LogoutArgs {
    /// Provider name.
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct WhatsappArgs {
    /// Action: setup/status/qr
    pub action: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct PairingArgs {
    /// Action: list/approve/revoke/clear-pending
    pub action: Option<String>,
    /// Device ID.
    #[arg(long)]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct ClawArgs {
    /// Action: migrate/cleanup
    pub action: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct AcpArgs {
    /// Action: start/status/stop/restart/check/setup/setup-browser/version
    pub action: Option<String>,
    /// Run ACP startup checks without starting the server.
    #[arg(long, conflicts_with_all = ["action", "setup", "setup_browser"])]
    pub check: bool,
    /// Run Hermes model/provider setup for ACP clients.
    #[arg(long, conflicts_with_all = ["action", "check", "setup_browser"])]
    pub setup: bool,
    /// Check browser tooling needed by ACP browser workflows.
    #[arg(long = "setup-browser", conflicts_with_all = ["action", "check", "setup"])]
    pub setup_browser: bool,
    /// Print version information without starting the ACP server.
    #[arg(
        long,
        conflicts_with_all = ["action", "check", "setup", "setup_browser"]
    )]
    pub version: bool,
    /// Assume yes / non-interactive mode for setup subcommands.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Clone, Args)]
pub struct BackupArgs {
    /// Output path for backup archive.
    pub output: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct ImportArgs {
    /// Path to backup archive.
    pub path: String,
}

#[derive(Debug, Clone, Args)]
pub struct ImportHermesSessionsArgs {
    /// JSONL file created by Hermes sessions export.
    pub export: std::path::PathBuf,
    /// New profile directory under an existing private directory.
    #[arg(long)]
    pub profile_dir: std::path::PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct DumpArgs {
    /// Session id or file stem.
    pub session: Option<String>,
    /// Output path.
    pub output: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct CompletionArgs {
    /// Shell type: bash/zsh/fish/powershell/elvish.
    pub shell: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct UninstallArgs {
    /// Confirm destructive cleanup.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Clone, Args)]
pub struct LumioArgs {
    /// Action: login (default), logout, status.
    pub action: Option<String>,
    /// Model to use after login (default: deepseek/deepseek-chat).
    #[arg(short, long)]
    pub model: Option<String>,
}

// ---------------------------------------------------------------------------
// Cli
// ---------------------------------------------------------------------------

/// Reformed Agent CLI.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "hermes-agent-ultra",
    version,
    about = "Reformed Agent · Printing press 2.0",
    long_about = "Reformed Agent is an autonomous AI agent that can use tools, execute code, \
                  and interact with various platforms. Start an interactive session with \
                  `hermes-agent-ultra` (or legacy alias `hermes`) or use subcommands for \
                  specific tasks."
)]
pub struct Cli {
    /// The subcommand to execute. Defaults to starting an interactive session.
    #[command(subcommand)]
    pub command: Option<CliCommand>,

    /// Enable verbose / debug logging.
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    /// Override the configuration directory path.
    #[arg(short = 'C', long, global = true)]
    pub config_dir: Option<String>,

    /// Override the default model (e.g. "openai:gpt-4o").
    #[arg(short = 'm', long, global = true)]
    pub model: Option<String>,

    /// Override the provider for this invocation (e.g. "nous", "anthropic").
    #[arg(long, global = true)]
    pub provider: Option<String>,

    /// One-shot prompt mode (non-interactive), aliasing upstream `-z`.
    #[arg(short = 'z', long, global = true)]
    pub oneshot: Option<String>,

    /// Force-enable tools in one-shot/query mode (`-z` / `chat --query`).
    ///
    /// Tools are enabled by default in query mode. Use
    /// `HERMES_QUERY_DISABLE_TOOLS=1` to force-disable.
    #[arg(long, global = true)]
    pub allow_tools: bool,

    /// Override the personality / persona.
    #[arg(short = 'p', long, global = true)]
    pub personality: Option<String>,

    /// Ignore user config files (`config.yaml`, `cli-config.yaml`, `gateway.json`) for this run.
    #[arg(long, global = true)]
    pub ignore_user_config: bool,

    /// Ignore local instruction/rules context injection (AGENTS.md/SOUL.md/etc.) for this run.
    #[arg(long, global = true)]
    pub ignore_rules: bool,
}

impl Cli {
    /// Return the effective command, defaulting to `CliCommand::Hermes`.
    pub fn effective_command(&self) -> CliCommand {
        self.command.clone().unwrap_or(CliCommand::Hermes)
    }
}

#[cfg(test)]
mod tests;
