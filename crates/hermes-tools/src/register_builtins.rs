//! Registers all built-in tool handlers into a ToolRegistry.
//!
//! This is the Rust equivalent of Python's `_discover_tools()` in `model_tools.py`.
//! Each tool handler is instantiated with its default backend and registered
//! in the appropriate toolset.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::{SkillProvider, TerminalBackend, ToolError, ToolHandler, ToolSchema};
use serde_json::Value;

use crate::terminal_requirements::non_local_terminal_backend;
use crate::tools::browser::SharedBrowser;
use crate::ToolRegistry;

/// A tool that acts on this machine directly rather than through the
/// terminal backend. It runs only while that backend is local: with any
/// other, the user chose it for isolation, so the tool refuses.
///
/// With no parameters listed, every call refuses and the tool is registered
/// hidden (dispatch does not check availability, so a call by name must still
/// refuse). With parameters listed, the handler reads or writes the path one of
/// them names, so only a call giving one a value other than an `http://` or
/// `https://` URL refuses, and the tool stays offered.
struct HostOnly(Arc<dyn ToolHandler>, &'static [&'static str]);

#[async_trait]
impl ToolHandler for HostOnly {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        if let Some(backend) = non_local_terminal_backend() {
            let name = self.0.schema().name;
            if self.1.is_empty() {
                return Err(ToolError::ExecutionFailed(format!(
                    "`{name}` works on this machine directly, so it is off whilst the terminal \
                     backend is `{backend}`; use the terminal, read_file and write_file tools, \
                     which run in that backend."
                )));
            }
            let local = self.1.iter().find(|key| {
                params
                    .get(**key)
                    .and_then(Value::as_str)
                    .is_some_and(|value| {
                        !value.is_empty()
                            && !value.starts_with("https://")
                            && !value.starts_with("http://")
                    })
            });
            if let Some(key) = local {
                return Err(ToolError::ExecutionFailed(format!(
                    "`{name}` would use the path in `{key}` on this machine, which is off whilst \
                     the terminal backend is `{backend}`; leave `{key}` out or give an http(s) \
                     URL where the tool takes one."
                )));
            }
        }
        self.0.execute(params).await
    }

    fn schema(&self) -> ToolSchema {
        self.0.schema()
    }
}

/// Register all built-in tool handlers into the given registry.
///
/// `terminal_backend` is shared by terminal, file read/write handlers.
/// `skill_provider` is shared by the three skills handlers.
///
/// Backends that depend on environment variables (e.g. `OPENAI_API_KEY`,
/// `FAL_KEY`, `HASS_URL`) are constructed eagerly; if the vars are absent
/// the tools will return a clear error at *runtime*, not at registration.
pub fn register_builtin_tools(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
) {
    register_builtin_tools_with_browser(
        registry,
        terminal_backend,
        skill_provider,
        SharedBrowser::lazy(crate::backends::browser::browser_backend_from_env),
    );
}

/// [`register_builtin_tools`], with the browser tools driving `browser`.
pub fn register_builtin_tools_with_browser(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    browser: SharedBrowser,
) {
    register_builtin_tools_with_data_dir(
        registry,
        terminal_backend,
        skill_provider,
        hermes_data_dir(),
        browser,
    );
}

/// The 10 browser tools, all driving `browser`, each with its emoji.
pub fn browser_tool_handlers(browser: SharedBrowser) -> [(Arc<dyn ToolHandler>, &'static str); 10] {
    use crate::tools::browser::*;

    let vision = crate::backends::vision::OpenAiVisionBackend::from_env()
        .map(|backend| Arc::new(backend) as Arc<dyn crate::tools::vision::VisionBackend>)
        .map_err(|err| err.to_string());
    [
        (Arc::new(BrowserNavigateHandler::new(browser.clone())), "🌐"),
        (Arc::new(BrowserSnapshotHandler::new(browser.clone())), "📸"),
        (Arc::new(BrowserClickHandler::new(browser.clone())), "🖱️"),
        (Arc::new(BrowserTypeHandler::new(browser.clone())), "⌨️"),
        (Arc::new(BrowserScrollHandler::new(browser.clone())), "📜"),
        (Arc::new(BrowserBackHandler::new(browser.clone())), "⬅️"),
        (Arc::new(BrowserPressHandler::new(browser.clone())), "🔘"),
        (
            Arc::new(BrowserGetImagesHandler::new(browser.clone())),
            "🖼️",
        ),
        (
            Arc::new(BrowserVisionHandler::new(browser.clone(), vision)),
            "👁️",
        ),
        (Arc::new(BrowserConsoleHandler::new(browser)), "🔧"),
    ]
}

fn register_builtin_tools_with_data_dir(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    data_dir: PathBuf,
    browser: SharedBrowser,
) {
    fn reg(
        registry: &ToolRegistry,
        toolset: &str,
        handler: Arc<dyn ToolHandler>,
        emoji: &str,
        env_deps: Vec<String>,
    ) {
        let schema = handler.schema();
        let name = schema.name.clone();
        let desc = schema.description.clone();
        registry.register(
            name,
            toolset,
            schema,
            handler,
            Arc::new(|| true),
            env_deps,
            true,
            desc,
            emoji,
            None,
        );
    }
    fn reg_with_check(
        registry: &ToolRegistry,
        toolset: &str,
        handler: Arc<dyn ToolHandler>,
        emoji: &str,
        env_deps: Vec<String>,
        check_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        let schema = handler.schema();
        let name = schema.name.clone();
        let desc = schema.description.clone();
        registry.register(
            name, toolset, schema, handler, check_fn, env_deps, true, desc, emoji, None,
        );
    }

    let terminal_requirements_check: Arc<dyn Fn() -> bool + Send + Sync> =
        Arc::new(crate::terminal_requirements::check_terminal_requirements);
    let host_only_check: Arc<dyn Fn() -> bool + Send + Sync> =
        Arc::new(|| non_local_terminal_backend().is_none());

    // -- Web tools -----------------------------------------------------------
    reg(
        registry,
        "web",
        Arc::new(crate::tools::web::WebSearchHandler::new(
            crate::backends::web::search_backend_from_env_or_fallback(),
        )),
        "🔍",
        vec![],
    );
    reg(
        registry,
        "web",
        Arc::new(crate::tools::web::WebExtractHandler::new(
            crate::backends::web::extract_backend_from_env_or_fallback(),
        )),
        "📄",
        vec![],
    );
    reg(
        registry,
        "web",
        Arc::new(crate::tools::web::WebCrawlHandler::new(
            crate::backends::web::crawl_backend_from_env_or_fallback(),
        )),
        "🕸️",
        vec![],
    );

    // -- Terminal ------------------------------------------------------------
    reg_with_check(
        registry,
        "terminal",
        Arc::new(crate::tools::terminal::TerminalHandler::new(
            terminal_backend.clone(),
        )),
        "💻",
        vec![],
        terminal_requirements_check.clone(),
    );
    reg_with_check(
        registry,
        "terminal",
        Arc::new(crate::tools::terminal::ProcessHandler::new(Arc::new(
            crate::tools::terminal::TerminalProcessBackendAdapter::new(terminal_backend.clone()),
        ))),
        "🧵",
        vec![],
        terminal_requirements_check.clone(),
    );

    // -- File tools ----------------------------------------------------------
    reg_with_check(
        registry,
        "file",
        Arc::new(crate::tools::file::ReadFileHandler::new(
            terminal_backend.clone(),
        )),
        "📖",
        vec![],
        terminal_requirements_check.clone(),
    );
    reg_with_check(
        registry,
        "file",
        Arc::new(crate::tools::file::WriteFileHandler::new(
            terminal_backend.clone(),
        )),
        "✏️",
        vec![],
        terminal_requirements_check.clone(),
    );
    reg_with_check(
        registry,
        "file",
        Arc::new(HostOnly(
            Arc::new(crate::tools::file::PatchHandler::new(Arc::new(
                crate::backends::file::LocalPatchBackend::new(),
            ))),
            &[],
        )),
        "🩹",
        vec![],
        host_only_check.clone(),
    );
    reg_with_check(
        registry,
        "file",
        Arc::new(HostOnly(
            Arc::new(crate::tools::file::SearchFilesHandler::new(Arc::new(
                crate::backends::file::LocalSearchBackend::new(),
            ))),
            &[],
        )),
        "🔎",
        vec![],
        host_only_check.clone(),
    );

    // -- Project/workspace tools ----------------------------------------------
    reg_with_check(
        registry,
        "project",
        Arc::new(HostOnly(
            Arc::new(crate::tools::project_workspace::ProjectFactsHandler),
            &[],
        )),
        "🗂️",
        vec![],
        host_only_check.clone(),
    );
    reg_with_check(
        registry,
        "project",
        Arc::new(HostOnly(
            Arc::new(crate::tools::project_workspace::ProjectTreeHandler),
            &[],
        )),
        "🌳",
        vec![],
        host_only_check.clone(),
    );
    reg(
        registry,
        "project",
        Arc::new(crate::tools::harness_cockpit::HarnessCockpitHandler::new()),
        "🧭",
        vec![],
    );
    reg(
        registry,
        "ultra_autonomy",
        Arc::new(crate::tools::ultra_autonomy::UltraAutonomyHandler::new(
            data_dir.clone(),
        )),
        "🧠",
        vec![],
    );

    // -- Ultra feature coding-agent surfaces -----------------------------------------
    for (handler, toolset, emoji) in
        crate::tools::ultra_features::builtin_ultra_feature_handlers(data_dir.clone())
    {
        reg(registry, toolset, handler, emoji, vec![]);
    }

    // -- Vision --------------------------------------------------------------
    {
        let backend =
            crate::backends::vision::OpenAiVisionBackend::from_env().unwrap_or_else(|_| {
                crate::backends::vision::OpenAiVisionBackend::new(
                    String::new(),
                    "https://api.openai.com/v1".into(),
                    "gpt-4o".into(),
                )
            });
        let vision_backend: Arc<dyn crate::tools::vision::VisionBackend> = Arc::new(backend);
        reg(
            registry,
            "vision",
            Arc::new(HostOnly(
                Arc::new(crate::tools::vision::VisionAnalyzeHandler::new(
                    vision_backend.clone(),
                )),
                &["image_url"],
            )),
            "👁️",
            vec![
                "AUXILIARY_VISION_API_KEY".into(),
                "AUXILIARY_VISION_PROVIDER".into(),
                "AUXILIARY_VISION_MODEL".into(),
                "AUXILIARY_VISION_BASE_URL".into(),
                "HERMES_OPENAI_API_KEY".into(),
                "OPENAI_API_KEY".into(),
                "OPENROUTER_API_KEY".into(),
                "NOUS_API_KEY".into(),
                "GEMINI_API_KEY".into(),
                "XAI_API_KEY".into(),
            ],
        );
        reg_with_check(
            registry,
            "vision",
            Arc::new(HostOnly(
                Arc::new(crate::tools::video::VideoAnalyzeHandler::new(Arc::new(
                    crate::backends::video::VisionFrameSamplingVideoBackend::new(vision_backend),
                ))),
                &[],
            )),
            "🎬",
            vec!["HERMES_OPENAI_API_KEY".into(), "OPENAI_API_KEY".into()],
            host_only_check.clone(),
        );
    }

    // -- Image generation ----------------------------------------------------
    {
        let backend = crate::backends::image_gen::ImageGenRuntimeBackend::from_env_or_managed();
        let env_deps = backend.required_env_vars();
        reg(
            registry,
            "image_gen",
            Arc::new(crate::tools::image_gen::ImageGenerateHandler::new(
                Arc::new(backend),
            )),
            "🎨",
            env_deps,
        );
    }

    // -- Video generation ----------------------------------------------------
    {
        let backend = crate::backends::video_gen::VideoGenBackend::from_env_or_managed();
        let env_deps = backend.required_env_vars();
        reg(
            registry,
            "video_gen",
            Arc::new(crate::tools::video::VideoGenerateHandler::new(Arc::new(
                backend,
            ))),
            "🎞️",
            env_deps,
        );
    }

    // -- Spotify -------------------------------------------------------------
    {
        let backend: Arc<dyn crate::tools::spotify::SpotifyBackend> =
            match crate::backends::spotify::SpotifyWebApiBackend::from_env_or_auth_store() {
                Ok(backend) => Arc::new(backend),
                Err(_) => Arc::new(crate::backends::spotify::SpotifyWebApiBackend::unconfigured()),
            };
        let deps = vec![
            "HERMES_SPOTIFY_ACCESS_TOKEN".into(),
            "SPOTIFY_ACCESS_TOKEN".into(),
            "HERMES_AUTH_FILE".into(),
        ];
        for (tool, emoji) in [
            (crate::tools::spotify::SpotifyTool::Playback, "🎵"),
            (crate::tools::spotify::SpotifyTool::Devices, "🔈"),
            (crate::tools::spotify::SpotifyTool::Queue, "📻"),
            (crate::tools::spotify::SpotifyTool::Search, "🔎"),
            (crate::tools::spotify::SpotifyTool::Playlists, "📚"),
            (crate::tools::spotify::SpotifyTool::Albums, "💿"),
            (crate::tools::spotify::SpotifyTool::Library, "❤️"),
        ] {
            reg(
                registry,
                "spotify",
                Arc::new(crate::tools::spotify::SpotifyHandler::new(
                    tool,
                    backend.clone(),
                )),
                emoji,
                deps.clone(),
            );
        }
    }

    // -- Skills (3 tools) ----------------------------------------------------
    reg(
        registry,
        "skills",
        Arc::new(crate::tools::skills::SkillsListHandler::new(
            skill_provider.clone(),
        )),
        "📚",
        vec![],
    );
    reg(
        registry,
        "skills",
        Arc::new(crate::tools::skills::SkillViewHandler::new(
            skill_provider.clone(),
        )),
        "📖",
        vec![],
    );
    reg(
        registry,
        "skills",
        Arc::new(crate::tools::skills::SkillManageHandler::new(
            skill_provider,
        )),
        "⚙️",
        vec![],
    );

    // -- Memory --------------------------------------------------------------
    reg(
        registry,
        "memory",
        Arc::new(crate::tools::memory::MemoryHandler::new(Arc::new(
            crate::backends::memory::FileMemoryBackend::new(),
        ))),
        "🧠",
        vec![],
    );

    // -- Session search ------------------------------------------------------
    {
        let db_path = data_dir.join("sessions.db");
        if let Ok(backend) = crate::backends::session_search::SqliteSessionSearchBackend::new(
            &db_path.to_string_lossy(),
        ) {
            reg(
                registry,
                "session_search",
                Arc::new(crate::tools::session_search::SessionSearchHandler::new(
                    Arc::new(backend),
                )),
                "🔍",
                vec![],
            );
        } else {
            tracing::warn!("Failed to initialise session search DB; skipping session_search tool");
        }
    }

    // -- Todo ----------------------------------------------------------------
    {
        let todo_path = data_dir.join("todos.json");
        reg(
            registry,
            "todo",
            Arc::new(crate::tools::todo::TodoHandler::new(Arc::new(
                crate::backends::todo::FileTodoBackend::new(todo_path),
            ))),
            "📋",
            vec![],
        );
    }

    // -- Clarify -------------------------------------------------------------
    reg(
        registry,
        "clarify",
        Arc::new(crate::tools::clarify::ClarifyHandler::new(Arc::new(
            crate::backends::clarify::SignalClarifyBackend::new(),
        ))),
        "❓",
        vec![],
    );

    // -- Code execution ------------------------------------------------------
    reg_with_check(
        registry,
        "code_execution",
        Arc::new(HostOnly(
            Arc::new(crate::tools::code_execution::ExecuteCodeHandler::new(
                Arc::new(crate::backends::code_execution::LocalCodeExecutionBackend::default()),
            )),
            &[],
        )),
        "🖥️",
        vec![],
        host_only_check.clone(),
    );

    // -- Delegation ----------------------------------------------------------
    reg(
        registry,
        "delegation",
        Arc::new(crate::tools::delegation::DelegateTaskHandler::new(
            Arc::new(crate::backends::delegation::SignalDelegationBackend::new()),
        )),
        "🤝",
        vec![],
    );

    // -- Cronjob -------------------------------------------------------------
    reg(
        registry,
        "cronjob",
        Arc::new(crate::tools::cronjob::CronjobHandler::new(Arc::new(
            crate::backends::cronjob::SignalCronjobBackend::new(),
        ))),
        "⏰",
        vec![],
    );

    // -- Messaging -----------------------------------------------------------
    reg(
        registry,
        "messaging",
        Arc::new(crate::tools::messaging::SendMessageHandler::new(Arc::new(
            crate::backends::messaging::SignalMessagingBackend::new(),
        ))),
        "💬",
        vec![],
    );

    // -- Dashboard control ---------------------------------------------------
    reg(
        registry,
        "dashboard",
        Arc::new(crate::tools::dashboard_control::DashboardControlHandler),
        "🖥️",
        vec![],
    );

    // -- Home Assistant (4 tools) --------------------------------------------
    {
        let ha_backend: Arc<dyn crate::tools::homeassistant::HomeAssistantBackend> =
            match crate::backends::homeassistant::HaRestBackend::from_env() {
                Ok(b) => Arc::new(b),
                Err(_) => Arc::new(crate::backends::homeassistant::HaRestBackend::new(
                    String::new(),
                    String::new(),
                )),
            };
        let deps = vec!["HASS_URL".into(), "HASS_TOKEN".into()];
        reg(
            registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaListEntitiesHandler::new(
                ha_backend.clone(),
            )),
            "🏠",
            deps.clone(),
        );
        reg(
            registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaGetStateHandler::new(
                ha_backend.clone(),
            )),
            "🏠",
            deps.clone(),
        );
        reg(
            registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaListServicesHandler::new(
                ha_backend.clone(),
            )),
            "🏠",
            deps.clone(),
        );
        reg(
            registry,
            "homeassistant",
            Arc::new(crate::tools::homeassistant::HaCallServiceHandler::new(
                ha_backend,
            )),
            "🏠",
            deps,
        );
    }

    // -- TTS -----------------------------------------------------------------
    reg(
        registry,
        "tts",
        Arc::new(HostOnly(
            Arc::new(crate::tools::tts::TextToSpeechHandler::new(Arc::new(
                crate::backends::tts::MultiTtsBackend::new(),
            ))),
            &["output_path"],
        )),
        "🔊",
        vec![],
    );

    // -- Browser (10 tools) --------------------------------------------------
    for (handler, emoji) in browser_tool_handlers(browser) {
        reg(registry, "browser", handler, emoji, vec![]);
    }

    // -- Mixture of Agents ---------------------------------------------------
    reg(
        registry,
        "mixture_of_agents",
        Arc::new(crate::tools::mixture_of_agents::MixtureOfAgentsHandler),
        "🤖",
        vec![],
    );

    // -- Process registry ----------------------------------------------------
    reg_with_check(
        registry,
        "terminal",
        Arc::new(crate::tools::process_registry::ProcessRegistryHandler::default()),
        "📊",
        vec![],
        terminal_requirements_check,
    );

    // -- Transcription -------------------------------------------------------
    reg_with_check(
        registry,
        "voice",
        Arc::new(HostOnly(
            Arc::new(crate::tools::transcription::TranscriptionHandler),
            &[],
        )),
        "🎙️",
        vec!["HERMES_OPENAI_API_KEY".into(), "OPENAI_API_KEY".into()],
        host_only_check.clone(),
    );

    // -- Voice mode ----------------------------------------------------------
    reg(
        registry,
        "voice",
        Arc::new(crate::tools::voice_mode::VoiceModeHandler),
        "🎤",
        vec![],
    );

    // -- TTS Premium ---------------------------------------------------------
    reg(
        registry,
        "tts",
        Arc::new(crate::tools::tts_premium::TtsPremiumHandler::default()),
        "🎵",
        vec!["ELEVENLABS_API_KEY".into()],
    );

    // -- OSV Check -----------------------------------------------------------
    reg(
        registry,
        "security",
        Arc::new(crate::tools::osv_check::OsvCheckHandler),
        "🛡️",
        vec![],
    );

    // -- URL Safety ----------------------------------------------------------
    reg(
        registry,
        "security",
        Arc::new(crate::tools::url_safety::UrlSafetyHandler),
        "🔒",
        vec![],
    );

    // -- Env passthrough -----------------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::env_passthrough::EnvPassthroughHandler),
        "🔧",
        vec![],
    );

    // -- Computer Use --------------------------------------------------------
    reg_with_check(
        registry,
        "computer_use",
        Arc::new(HostOnly(
            Arc::new(crate::tools::computer_use::ComputerUseHandler::new(
                Arc::new(crate::backends::computer_use::CuaDriverBackend::from_env()),
            )),
            &[],
        )),
        "🖱️",
        vec!["HERMES_CUA_DRIVER_CMD".into()],
        Arc::new(move || {
            host_only_check()
                && crate::backends::computer_use::CuaDriverBackend::command_available_from_env()
        }),
    );

    // -- Credential files ----------------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::credential_files::CredentialFilesHandler),
        "🔑",
        vec![],
    );

    // -- Auth/provider snapshot ---------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::auth_snapshot::AuthSnapshotHandler),
        "🔐",
        vec![],
    );

    // -- Integration control-plane snapshot ---------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::integrations_snapshot::IntegrationsSnapshotHandler::new(registry)),
        "🧩",
        vec![],
    );

    // -- Alpha objective/mission snapshots ----------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::alpha_snapshot::ObjectiveSnapshotHandler::new()),
        "🎯",
        vec![],
    );
    reg(
        registry,
        "system",
        Arc::new(crate::tools::alpha_snapshot::MissionSnapshotHandler::new()),
        "🛰️",
        vec![],
    );

    // -- Tool policy simulation ---------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::tool_policy_simulate::ToolPolicySimulateHandler::new(registry)),
        "🧭",
        vec![],
    );

    // -- RTK raw trace control ----------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::raw_trace_control::RawTraceControlHandler::new(registry)),
        "🧾",
        vec![],
    );

    // -- Deterministic replay trace control ---------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::replay_trace_control::ReplayTraceControlHandler::new()),
        "🧬",
        vec![],
    );

    // -- Runtime recovery runbooks ------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::runbook::RunbookControlHandler::new()),
        "📘",
        vec![],
    );

    // -- Runtime telemetry snapshot -----------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::telemetry_snapshot::TelemetrySnapshotHandler::new()),
        "📡",
        vec![],
    );

    // -- Read-only ops snapshot ---------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::ops_snapshot::OpsSnapshotHandler::new(
            registry,
        )),
        "🛰️",
        vec![],
    );

    // -- Tool result storage -------------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::tool_result_storage::ToolResultStorageHandler::default()),
        "💾",
        vec![],
    );

    // -- Disk cleanup --------------------------------------------------------
    reg(
        registry,
        "system",
        Arc::new(crate::tools::disk_cleanup::DiskCleanupHandler::default()),
        "🧹",
        vec![],
    );

    tracing::info!("Registered {} built-in tools", registry.list_tools().len());
}

/// Return the Hermes data directory, creating it if needed.
fn hermes_data_dir() -> PathBuf {
    let dir = hermes_config::hermes_home();
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use crate::approval::TEST_ENV_LOCK;
    use async_trait::async_trait;
    use hermes_core::{AgentError, CommandOutput, Skill, SkillMeta};
    use serde_json::Value;

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = &self.original {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    pub(crate) struct MockTerminalBackend;

    #[async_trait]
    impl TerminalBackend for MockTerminalBackend {
        async fn execute_command(
            &self,
            _command: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _background: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<u64>,
            _limit: Option<u64>,
        ) -> Result<String, AgentError> {
            Ok(String::new())
        }

        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
            Ok(())
        }

        async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
            Ok(false)
        }

        async fn list_processes(&self) -> Result<Value, AgentError> {
            Ok(serde_json::json!([]))
        }
    }

    pub(crate) struct MockSkillProvider;

    #[async_trait]
    impl SkillProvider for MockSkillProvider {
        async fn create_skill(
            &self,
            name: &str,
            content: &str,
            category: Option<&str>,
        ) -> Result<Skill, AgentError> {
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: category.map(String::from),
                description: None,
            })
        }

        async fn get_skill(&self, _name: &str) -> Result<Option<Skill>, AgentError> {
            Ok(None)
        }

        async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError> {
            Ok(Vec::new())
        }

        async fn update_skill(&self, name: &str, content: &str) -> Result<Skill, AgentError> {
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: None,
                description: None,
            })
        }

        async fn delete_skill(&self, _name: &str) -> Result<(), AgentError> {
            Ok(())
        }
    }

    #[test]
    fn dropping_a_registry_frees_its_builtin_tools() {
        let registry = ToolRegistry::new();
        register_builtin_tools(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
        );
        let weak = registry.downgrade();
        drop(registry);
        assert!(
            weak.upgrade().is_none(),
            "a builtin tool keeps the registry that holds it alive"
        );
    }

    fn registered_names() -> Vec<String> {
        let registry = ToolRegistry::new();
        register_builtin_tools(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
        );
        let mut names: Vec<String> = registry
            .get_definitions()
            .into_iter()
            .map(|s| s.name)
            .collect();
        names.sort();
        names
    }

    fn registered_all_names_with_data_dir(data_dir: PathBuf) -> Vec<String> {
        let registry = ToolRegistry::new();
        register_builtin_tools_with_data_dir(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
            data_dir,
            SharedBrowser::lazy(crate::backends::browser::browser_backend_from_env),
        );
        let mut names: Vec<String> = registry
            .list_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn local_backend_exposes_terminal_and_terminal_backed_file_tools() {
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _terminal_env = EnvGuard::set("TERMINAL_ENV", "local");
        let names = registered_names();

        for expected in [
            "terminal",
            "process",
            "process_registry",
            "read_file",
            "write_file",
            "patch",
            "search_files",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "local backend should expose {expected}"
            );
        }
    }

    #[test]
    fn invalid_backend_hides_terminal_backed_tools_but_keeps_local_file_tools() {
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _terminal_env = EnvGuard::set("TERMINAL_ENV", "unknown-backend");
        let _ssh_host = EnvGuard::remove("TERMINAL_SSH_HOST");
        let _ssh_user = EnvGuard::remove("TERMINAL_SSH_USER");
        let names = registered_names();

        // patch, search_files and execute_code act on this machine directly,
        // so a backend that is not local hides them too.
        for hidden in [
            "terminal",
            "process",
            "process_registry",
            "read_file",
            "write_file",
            "patch",
            "search_files",
            "execute_code",
        ] {
            assert!(
                !names.contains(&hidden.to_string()),
                "invalid backend should hide {hidden}"
            );
        }
        assert!(
            names.contains(&"ultra-feature-1".to_string()),
            "invalid backend should keep Rust-native ultra-feature tools"
        );
        assert!(
            names.contains(&"tool_policy_simulate".to_string()),
            "invalid backend should keep policy simulator"
        );
        assert!(
            names.contains(&"auth_snapshot".to_string()),
            "invalid backend should keep read-only auth snapshots"
        );
        assert!(
            names.contains(&"integrations_snapshot".to_string()),
            "invalid backend should keep read-only integration snapshots"
        );
        assert!(
            names.contains(&"objective_snapshot".to_string()),
            "invalid backend should keep read-only objective snapshots"
        );
        assert!(
            names.contains(&"mission_snapshot".to_string()),
            "invalid backend should keep read-only mission snapshots"
        );
        assert!(
            names.contains(&"raw_trace_control".to_string()),
            "invalid backend should keep raw trace control"
        );
        assert!(
            names.contains(&"replay_trace_control".to_string()),
            "invalid backend should keep replay trace control"
        );
        assert!(
            names.contains(&"runbook_control".to_string()),
            "invalid backend should keep runtime runbooks"
        );
        assert!(
            names.contains(&"telemetry_snapshot".to_string()),
            "invalid backend should keep telemetry snapshots"
        );
        assert!(
            names.contains(&"harness_cockpit".to_string()),
            "invalid backend should keep harness cockpit"
        );
        assert!(
            names.contains(&"ultra_autonomy".to_string()),
            "invalid backend should keep autonomy cockpit"
        );
        assert!(
            names.contains(&"ops_snapshot".to_string()),
            "invalid backend should keep read-only ops snapshots"
        );
    }

    /// With a terminal backend other than local, the tools that act on this
    /// machine directly are hidden and refuse a call by name, whose dispatch
    /// does not check availability: the user chose that backend for isolation.
    #[test]
    fn host_tools_refuse_when_the_terminal_backend_is_not_local() {
        let _global_env = hermes_config::managed_gateway::test_lock::lock();
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _alias = EnvGuard::remove("TERMINAL_BACKEND");
        for backend in ["docker", "modal", "vercel_sandbox"] {
            let _terminal_env = EnvGuard::set("TERMINAL_ENV", backend);
            assert_host_tools_refuse(backend, home.path());
        }
    }

    /// `terminal.backend: docker` in config.yaml alone, with neither
    /// TERMINAL_ENV nor TERMINAL_BACKEND set, turns the host tools off once the
    /// configuration is loaded, as every entry point loads it before it
    /// registers the tools.
    #[test]
    fn host_tools_refuse_when_only_config_yaml_names_the_backend() {
        let _global_env = hermes_config::managed_gateway::test_lock::lock();
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _hermes_home = EnvGuard::remove("HERMES_HOME");
        let _managed = EnvGuard::remove("HERMES_MANAGED_DIR");
        let _ignore = EnvGuard::remove("HERMES_IGNORE_USER_CONFIG");
        let _alias = EnvGuard::remove("TERMINAL_BACKEND");
        let _terminal_env = EnvGuard::remove("TERMINAL_ENV");
        std::fs::write(
            home.path().join("config.yaml"),
            "terminal:\n  backend: docker\n",
        )
        .unwrap();
        // The loaded configuration is not printed: it can hold keys from the
        // environment.
        let loaded = hermes_config::load_config(Some(home.path().to_string_lossy().as_ref()));
        assert!(
            loaded.is_ok_and(|config| {
                config.terminal.backend == hermes_config::TerminalBackendType::Docker
            }),
            "config.yaml's docker backend did not load"
        );
        assert_host_tools_refuse("config.yaml docker", home.path());
    }

    /// The tools that act on this machine are not offered and refuse a call by
    /// name; `home` holds the host file they must leave alone.
    fn assert_host_tools_refuse(backend: &str, home: &std::path::Path) {
        let file = home.join("host.txt");
        std::fs::write(&file, "host text").unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let registry = ToolRegistry::new();
        register_builtin_tools(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
        );
        for tool in [
            "patch",
            "search_files",
            "execute_code",
            "project_facts",
            "project_tree",
            "transcription",
            "video_analyze",
            "computer_use",
        ] {
            assert!(!registry.is_available(tool), "{backend}: {tool} offered");
        }
        let path = file.to_string_lossy();
        let speech = home.join("speech.mp3");
        let calls = [
            (
                "patch",
                serde_json::json!({
                    "path": file.to_string_lossy(),
                    "old_string": "host",
                    "new_string": "changed",
                }),
            ),
            (
                "search_files",
                serde_json::json!({ "pattern": "host", "path": home.to_string_lossy() }),
            ),
            (
                "execute_code",
                serde_json::json!({ "code": "print(1)", "language": "python" }),
            ),
            (
                "project_facts",
                serde_json::json!({ "cwd": home.to_string_lossy() }),
            ),
            (
                "project_tree",
                serde_json::json!({ "cwd": home.to_string_lossy() }),
            ),
            ("transcription", serde_json::json!({ "audio_path": path })),
            ("video_analyze", serde_json::json!({ "video_url": path })),
            (
                "computer_use",
                serde_json::json!({ "action": "screenshot" }),
            ),
            (
                "vision_analyze",
                serde_json::json!({ "image_url": path, "question": "What is this?" }),
            ),
            (
                "text_to_speech",
                serde_json::json!({ "text": "hi", "output_path": speech.to_string_lossy() }),
            ),
            // output_path is always a path on this machine: a URL is no exception.
            (
                "text_to_speech",
                serde_json::json!({ "text": "hi", "output_path": "https://example.com/speech.mp3" }),
            ),
        ];
        for (tool, params) in calls {
            let out = runtime.block_on(registry.dispatch_async(tool, params));
            assert!(
                out.contains("terminal backend") && !out.contains("host text"),
                "{backend}: {tool} ran on the host: {out}"
            );
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "host text");
        assert!(
            !speech.exists() && !std::path::Path::new("https:").exists(),
            "{backend}: text_to_speech wrote on the host"
        );
    }

    struct Ran;

    #[async_trait]
    impl ToolHandler for Ran {
        async fn execute(&self, _params: Value) -> Result<String, ToolError> {
            Ok("ran".into())
        }

        fn schema(&self) -> ToolSchema {
            hermes_core::tool_schema(
                "ran",
                "Reports that it ran.",
                hermes_core::JsonSchema::object(Default::default(), vec![]),
            )
        }
    }

    /// A tool guarded on a path parameter still runs for a web URL, and refuses
    /// any other value, which its handler would read as a path on this machine.
    #[test]
    fn a_path_guarded_tool_runs_only_for_web_urls_under_a_non_local_backend() {
        let _global_env = hermes_config::managed_gateway::test_lock::lock();
        let _lock = lock_env();
        let _alias = EnvGuard::remove("TERMINAL_BACKEND");
        let _terminal_env = EnvGuard::set("TERMINAL_ENV", "docker");
        let tool = HostOnly(Arc::new(Ran), &["image_url"]);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let run = |params: Value| runtime.block_on(tool.execute(params));
        for url in ["https://example.com/a.png", "http://example.com/a.png"] {
            assert_eq!(run(serde_json::json!({ "image_url": url })).unwrap(), "ran");
        }
        assert_eq!(run(serde_json::json!({ "question": "q" })).unwrap(), "ran");
        for path in [
            "/etc/hostname",
            "a.png",
            "HTTPS://example.com/a.png",
            "file:///etc/hostname",
        ] {
            let err = run(serde_json::json!({ "image_url": path })).unwrap_err();
            assert!(
                err.to_string().contains("terminal backend"),
                "{path}: {err}"
            );
        }
    }

    #[test]
    fn builtin_registry_exposes_snapshot_surfaces_to_cli_list() {
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _terminal_env = EnvGuard::set("TERMINAL_ENV", "local");
        let registry = ToolRegistry::new();
        register_builtin_tools(
            &registry,
            Arc::new(MockTerminalBackend),
            Arc::new(MockSkillProvider),
        );

        let names: Vec<String> = registry
            .list_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(
            names.contains(&"integrations_snapshot".to_string()),
            "`hermes tools list` should expose integrations_snapshot"
        );
        assert!(
            names.contains(&"objective_snapshot".to_string()),
            "`hermes tools list` should expose objective_snapshot"
        );
        assert!(
            names.contains(&"mission_snapshot".to_string()),
            "`hermes tools list` should expose mission_snapshot"
        );
    }

    #[test]
    fn builtin_registry_registers_terminal_tools_for_managed_modal_surface() {
        let _global_env = hermes_config::managed_gateway::test_lock::lock();
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let hermes_home = tempfile::tempdir().expect("temp hermes home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _hermes_home =
            EnvGuard::set("HERMES_HOME", hermes_home.path().to_string_lossy().as_ref());
        let names = registered_all_names_with_data_dir(hermes_home.path().to_path_buf());

        assert!(names.contains(&"terminal".to_string()));
        assert!(names.contains(&"process".to_string()));
        assert!(names.contains(&"execute_code".to_string()));
        assert!(
            hermes_home.path().join("sessions.db").is_file(),
            "session search should initialize under HERMES_HOME"
        );
        assert!(
            !home.path().join(".hermes").join("sessions.db").exists(),
            "HERMES_HOME must not be nested below HOME/.hermes"
        );
    }

    /// The fork has no trainer or model behind RL training, so no tool may
    /// offer it: the old rl_* tools reported simulated runs and canned
    /// inference as real.
    #[test]
    fn no_builtin_tool_or_toolset_offers_rl_training() {
        let _global_env = hermes_config::managed_gateway::test_lock::lock();
        let _lock = lock_env();
        let hermes_home = tempfile::tempdir().expect("temp hermes home");
        let _hermes_home =
            EnvGuard::set("HERMES_HOME", hermes_home.path().to_string_lossy().as_ref());
        let names = registered_all_names_with_data_dir(hermes_home.path().to_path_buf());
        let rl: Vec<_> = names
            .iter()
            .filter(|name| name.starts_with("rl_"))
            .collect();
        assert!(rl.is_empty(), "RL tools registered: {rl:?}");

        let toolsets = crate::toolset::ToolsetManager::new(Arc::new(ToolRegistry::new()));
        assert!(toolsets.resolve_toolset_unfiltered("rl_training").is_err());
        for composite in ["hermes-cli", "hermes-api-server"] {
            let tools = toolsets
                .resolve_toolset_unfiltered(composite)
                .expect("composite toolset");
            assert!(
                !tools.iter().any(|tool| tool.starts_with("rl_")),
                "{composite} offers RL tools"
            );
        }
    }

    #[test]
    fn data_dir_defaults_to_the_own_home_not_upstreams() {
        let _global_env = hermes_config::managed_gateway::test_lock::lock();
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _hermes_home = EnvGuard::remove("HERMES_HOME");
        let _ultra_home = EnvGuard::remove("HERMES_AGENT_ULTRA_HOME");

        assert_eq!(hermes_data_dir(), home.path().join(".hermes-agent-ultra"));
        assert!(!home.path().join(".hermes").exists());
    }

    #[test]
    fn default_writers_use_the_own_home_not_upstreams() {
        use crate::tools::memory::MemoryBackend;
        use crate::tools::todo::TodoBackend;

        let home = tempfile::tempdir().expect("temp home");
        let upstream = home.path().join(".hermes");
        std::fs::create_dir_all(upstream.join("memories")).expect("upstream home");
        std::fs::write(upstream.join("memories").join("MEMORY.md"), "upstream\n")
            .expect("upstream memory");
        let (memory, todos, sessions) = {
            let _global_env = hermes_config::managed_gateway::test_lock::lock();
            let _lock = lock_env();
            let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
            let _hermes_home = EnvGuard::remove("HERMES_HOME");
            let _ultra_home = EnvGuard::remove("HERMES_AGENT_ULTRA_HOME");
            (
                crate::backends::memory::FileMemoryBackend::new(),
                crate::backends::todo::FileTodoBackend::default_path(),
                crate::backends::session_search::SqliteSessionSearchBackend::default_path(),
            )
        };
        sessions.expect("sessions database");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime
            .block_on(memory.add("memory", "own note"))
            .expect("memory add");
        runtime
            .block_on(todos.create("own task", None, None))
            .expect("todo create");

        let own = home.path().join(".hermes-agent-ultra");
        let read = |path: std::path::PathBuf| std::fs::read_to_string(path).unwrap_or_default();
        let memory_md = |root: &std::path::Path| root.join("memories").join("MEMORY.md");
        let checks = [
            (
                "memory",
                read(memory_md(&own)).contains("own note")
                    && read(memory_md(&upstream)) == "upstream\n",
            ),
            (
                "todos",
                own.join("todos.json").is_file() && !upstream.join("todos.json").exists(),
            ),
            (
                "session search",
                own.join("sessions.db").is_file() && !upstream.join("sessions.db").exists(),
            ),
        ];
        let wrong: Vec<_> = checks
            .iter()
            .filter(|(_, ok)| !ok)
            .map(|(name, _)| *name)
            .collect();
        assert!(wrong.is_empty(), "written outside the own home: {wrong:?}");
    }

    #[test]
    fn builtin_registry_registers_core_tool_surfaces_for_parity() {
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let _terminal_env = EnvGuard::set("TERMINAL_ENV", "local");
        let names = registered_names();

        for expected in [
            "terminal",
            "process",
            "process_registry",
            "project_facts",
            "project_tree",
            "ultra_autonomy",
            "todo",
            "text_to_speech",
            "tts_premium",
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_scroll",
            "browser_back",
            "browser_press",
            "browser_get_images",
            "browser_vision",
            "browser_console",
            "ultra-feature-1",
            "ultra-feature-2",
            "ultra-feature-3",
            "ultra-feature-4",
            "ultra-feature-5",
            "ultra-feature-6",
            "ultra-feature-7",
            "ultra-feature-8",
            "ultra-feature-9",
            "ultra-feature-10",
            "ultra-feature-11",
            "ultra-feature-12",
            "ultra-feature-13",
            "ultra-feature-14",
            "ultra-feature-15",
            "ultra-feature-16",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "builtin registry should expose {expected}"
            );
        }
    }

    #[test]
    fn browser_tool_surface_exposes_current_commands_without_legacy_close() {
        let _lock = lock_env();
        let home = tempfile::tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", home.path().to_string_lossy().as_ref());
        let names = registered_names();

        for expected in [
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_scroll",
            "browser_back",
            "browser_press",
            "browser_get_images",
            "browser_vision",
            "browser_console",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "browser surface should expose {expected}"
            );
        }
        assert!(
            !names.contains(&"browser_close".to_string()),
            "Rust browser backend intentionally has no Python supervisor close tool"
        );
    }
}
