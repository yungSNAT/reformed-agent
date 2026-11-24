use super::*;
use std::sync::{Mutex, MutexGuard};

fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct TempEnv {
    key: &'static str,
    previous: Option<OsString>,
}

impl TempEnv {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for TempEnv {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

struct DoneProvider;

#[async_trait::async_trait]
impl LlmProvider for DoneProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>, // nosemgrep: codacy.yaml.security.hard-coded-tokens
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<hermes_core::LlmResponse, hermes_core::AgentError> {
        Ok(hermes_core::LlmResponse {
            message: Message::assistant("done"),
            usage: None,
            model: "mock".to_string(),
            finish_reason: Some("stop".to_string()),
        })
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> futures::stream::BoxStream<
        'static,
        Result<hermes_core::StreamChunk, hermes_core::AgentError>,
    > {
        Box::pin(futures::stream::empty())
    }
}

fn runner_with(registry: ToolRegistry) -> CronRunner {
    CronRunner::new(Arc::new(DoneProvider), Arc::new(registry))
}

fn runner() -> CronRunner {
    runner_with(ToolRegistry::new())
}

fn register_tool(registry: &mut ToolRegistry, name: &str) {
    registry.register(
        name,
        hermes_core::tool_schema(
            name,
            format!("{name} coverage tool"),
            hermes_core::JsonSchema::new("object"),
        ),
        Arc::new(
            |_params: serde_json::Value| -> Result<String, hermes_core::ToolError> {
                Ok("ok".to_string())
            },
        ),
    );
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn deliver_config(target: &str) -> DeliverConfig {
    let cap = capitalize(target);
    let candidates = [
        serde_json::json!({ "target": target }),
        serde_json::json!({ "target": cap }),
        serde_json::json!(target),
        serde_json::json!(cap),
    ];
    for candidate in candidates {
        if let Ok(cfg) = serde_json::from_value::<DeliverConfig>(candidate) {
            return cfg;
        }
    }
    panic!("could not build DeliverConfig for target {target}");
}

#[test]
fn env_var_guard_restores_previous_state() {
    if run_isolated("env_var_guard_restores_previous_state") {
        return;
    }

    let _lock = env_lock();
    const KEY: &str = "HERMES_CRON_COVERAGE_GEN_GUARD_KEY";
    std::env::remove_var(KEY);
    {
        let _guard = EnvVarGuard::set(KEY, Path::new("coverage-gen-value"));
        assert_eq!(
            std::env::var_os(KEY),
            Some(OsString::from("coverage-gen-value"))
        );
    }
    assert_eq!(std::env::var_os(KEY), None);

    std::env::set_var(KEY, "original");
    {
        let _guard = EnvVarGuard::set(KEY, Path::new("temporary"));
        assert_eq!(std::env::var_os(KEY), Some(OsString::from("temporary")));
    }
    assert_eq!(std::env::var_os(KEY), Some(OsString::from("original")));
    std::env::remove_var(KEY);
}

#[test]
fn trim_script_output_truncates_long_text() {
    if run_isolated("trim_script_output_truncates_long_text") {
        return;
    }

    let long = "x".repeat(MAX_SCRIPT_OUTPUT_CHARS + 10);
    let out = trim_script_output(&long);
    assert_eq!(out.chars().count(), MAX_SCRIPT_OUTPUT_CHARS + 1);
    assert!(out.ends_with('…'));
    assert_eq!(trim_script_output("  short  "), "short");
}

#[test]
fn parse_script_control_handles_blank_lines_and_non_control_json() {
    if run_isolated("parse_script_control_handles_blank_lines_and_non_control_json") {
        return;
    }

    let control = parse_script_control("line one\n{\"status\": \"ok\"}\n\n   \n");
    assert_eq!(control.wake_agent, None);
    assert_eq!(control.stripped_output, "line one\n{\"status\": \"ok\"}");

    let control = parse_script_control("\n  \n");
    assert_eq!(control.wake_agent, None);
    assert_eq!(control.stripped_output, "");

    let control = parse_script_control("data\n{\"wakeAgent\": \"yes\"}");
    assert_eq!(control.wake_agent, None);
    assert_eq!(control.stripped_output, "data\n{\"wakeAgent\": \"yes\"}");

    let control = parse_script_control("data\n{\"wakeAgent\": true}");
    assert_eq!(control.wake_agent, Some(true));
    assert_eq!(control.stripped_output, "data");
}

#[test]
fn script_timeout_prefers_env_then_job_then_default() {
    if run_isolated("script_timeout_prefers_env_then_job_then_default") {
        return;
    }

    let _lock = env_lock();
    let mut job = CronJob::new("* * * * *", "unused");
    job.script_timeout_seconds = Some(30);

    {
        let _env = TempEnv::set("HERMES_CRON_SCRIPT_TIMEOUT", " 45 ");
        assert_eq!(script_timeout_secs(&job), 45);
    }
    {
        let _env = TempEnv::set("HERMES_CRON_SCRIPT_TIMEOUT", "0");
        assert_eq!(script_timeout_secs(&job), 30);
    }
    {
        let _env = TempEnv::set("HERMES_CRON_SCRIPT_TIMEOUT", "abc");
        job.script_timeout_seconds = Some(0);
        assert_eq!(script_timeout_secs(&job), DEFAULT_SCRIPT_TIMEOUT_SECS);
    }
    {
        let _env = TempEnv::remove("HERMES_CRON_SCRIPT_TIMEOUT");
        job.script_timeout_seconds = Some(12);
        assert_eq!(script_timeout_secs(&job), 12);
        job.script_timeout_seconds = None;
        assert_eq!(script_timeout_secs(&job), DEFAULT_SCRIPT_TIMEOUT_SECS);
    }
}

#[test]
fn python_for_scripts_uses_env_overrides_then_default() {
    if run_isolated("python_for_scripts_uses_env_overrides_then_default") {
        return;
    }

    let _lock = env_lock();
    {
        let _a = TempEnv::set("HERMES_CRON_PYTHON", "/opt/cov/python-cov");
        let _b = TempEnv::set("PYTHON", "python-other");
        assert_eq!(python_for_scripts(), "/opt/cov/python-cov");
    }
    {
        let _a = TempEnv::set("HERMES_CRON_PYTHON", "   ");
        let _b = TempEnv::set("PYTHON", "python-alt");
        assert_eq!(python_for_scripts(), "python-alt");
    }
    {
        let _a = TempEnv::remove("HERMES_CRON_PYTHON");
        let _b = TempEnv::remove("PYTHON");
        assert_eq!(python_for_scripts(), "python3");
    }
}

#[test]
fn command_for_python_script_uses_python_interpreter() {
    if run_isolated("command_for_python_script_uses_python_interpreter") {
        return;
    }

    let _lock = env_lock();
    let _a = TempEnv::set("HERMES_CRON_PYTHON", "python-cov-gen");
    let script = Path::new("job.py");
    let command = command_for_script_path(script);
    assert_eq!(
        command.as_std().get_program(),
        std::ffi::OsStr::new("python-cov-gen")
    );
    let args: Vec<_> = command.as_std().get_args().collect();
    assert_eq!(args, vec![std::ffi::OsStr::new("job.py")]);

    let command = command_for_script_path(Path::new("job.SH"));
    assert_eq!(
        command.as_std().get_program(),
        std::ffi::OsStr::new(DEFAULT_SCRIPT_SHELL)
    );
}

#[test]
fn invisible_unicode_ranges_are_detected() {
    if run_isolated("invisible_unicode_ranges_are_detected") {
        return;
    }

    assert!(has_invisible_unicode("a\u{202a}b"));
    assert!(has_invisible_unicode("a\u{202e}b"));
    assert!(has_invisible_unicode("a\u{2066}b"));
    assert!(has_invisible_unicode("a\u{2069}b"));
    assert!(!has_invisible_unicode("plain text"));
    assert_eq!(
        detect_cron_prompt_injection("x\u{2067}y"),
        Some("invisible_unicode")
    );
}

#[test]
fn windows_absolute_path_detection() {
    if run_isolated("windows_absolute_path_detection") {
        return;
    }

    assert!(windows_absolute_path("C:\\scripts\\x.sh"));
    assert!(windows_absolute_path("d:/scripts/x.sh"));
    assert!(!windows_absolute_path("C:x.sh"));
    assert!(!windows_absolute_path("C:"));
    assert!(!windows_absolute_path("1:/x"));
}

#[test]
fn looks_like_script_path_variants() {
    if run_isolated("looks_like_script_path_variants") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("watchdog"), "echo hi\n").unwrap();

    assert!(looks_like_script_path("/abs/path/run", dir.path()));
    assert!(looks_like_script_path("~/run", dir.path()));
    assert!(looks_like_script_path("C:\\run", dir.path()));
    assert!(looks_like_script_path("watchdog", dir.path()));
    assert!(looks_like_script_path("thing.py", dir.path()));
    assert!(!looks_like_script_path("missing-script", dir.path()));
    assert!(!looks_like_script_path("echo hello", dir.path()));
}

#[test]
fn resolve_cron_script_path_blocks_home_and_drive_paths() {
    if run_isolated("resolve_cron_script_path_blocks_home_and_drive_paths") {
        return;
    }

    let err = resolve_cron_script_path("~/evil.sh").expect_err("blocked");
    assert!(matches!(err, CronError::InvalidJob(_)));
    assert!(err.to_string().contains("outside Hermes scripts directory"));

    let err = resolve_cron_script_path("C:\\evil.sh").expect_err("blocked");
    assert!(err.to_string().contains("outside Hermes scripts directory"));

    assert!(matches!(
        resolve_cron_script_path("echo hello world"),
        Ok(None)
    ));
}

#[cfg(unix)]
#[test]
fn resolve_cron_script_path_reports_missing_absolute_script() {
    if run_isolated("resolve_cron_script_path_reports_missing_absolute_script") {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing-cov-gen.sh");
    let err = resolve_cron_script_path(missing.to_str().unwrap()).expect_err("missing");
    assert!(matches!(err, CronError::InvalidJob(_)));
    assert!(err.to_string().contains("cron script not found"), "{err}");
}

include!("runner_part2_coverage_gen.rs");
