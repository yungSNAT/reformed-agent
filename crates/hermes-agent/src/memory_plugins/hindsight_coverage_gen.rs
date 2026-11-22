use super::*;

use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

// Reqwest rejects this scheme before it can resolve a host or open a socket.
const OFFLINE_BASE: &str = "hindsight-test://offline.invalid/api";
const OFFLINE_BANK: &str = "team%20%2F%20notes";

fn offline_config() -> HindsightConfig {
    HindsightConfig {
        api_key: "unit-test-key".into(),
        api_url: format!("{OFFLINE_BASE}///"),
        bank_id: "team / notes".into(),
        bank_id_template: String::new(),
        budget: "low".into(),
        mode: "cloud".into(),
        memory_mode: "hybrid".into(),
        prefetch_method: "recall".into(),
        auto_retain: true,
        auto_recall: true,
        retain_every_n_turns: 3,
        retain_context: "test conversation".into(),
        recall_max_tokens: 128,
        recall_max_input_chars: 32,
        recall_types: vec!["observation".into()],
        recall_prompt_preamble: String::new(),
        bank_mission: String::new(),
        retain_async: false,
        retain_tags: vec!["default".into()],
        observation_scopes: Some(json!("per_tag")),
        timeout_secs: 1,
    }
}

fn configured_plugin(config: HindsightConfig) -> HindsightPlugin {
    let plugin = HindsightPlugin::default();
    *plugin.config.lock().unwrap() = Some(config);
    plugin
}

// Keep environment overrides and the background worker's tracing subscriber
// out of the process running the rest of the test suite.
fn in_isolated_process(test_name: &str, exercise: impl FnOnce(&Path)) {
    const CHILD_HOME: &str = "HERMES_HINDSIGHT_COVERAGE_CHILD_HOME";
    const COMPLETED: &str = "hindsight coverage child completed";

    if let Some(home) = std::env::var_os(CHILD_HOME) {
        exercise(Path::new(&home));
        println!("{COMPLETED}");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg(format!("{module}::{test_name}"))
        .args(["--exact", "--nocapture"])
        .env(CHILD_HOME, home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("HERMES_HOME", home.path())
        .env("HINDSIGHT_MODE", " local_embedded ")
        .env("HINDSIGHT_BANK_ID", "environment-bank")
        .env("HINDSIGHT_BUDGET", "mid")
        .env_remove("HINDSIGHT_API_KEY")
        .env_remove("HINDSIGHT_API_URL")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(COMPLETED),
        "child status: {}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

struct EventWriter(Sender<String>);

impl std::io::Write for EventWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0
            .send(String::from_utf8_lossy(data).into_owned())
            .map_err(std::io::Error::other)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn debug_events() -> (impl tracing::Subscriber + Send + Sync, Receiver<String>) {
    let (send, receive) = mpsc::channel();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || EventWriter(send.clone()))
        .finish();
    (subscriber, receive)
}

fn event_containing(events: &Receiver<String>, expected: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| panic!("expected a trace containing {expected:?}: {error}"));
        if event.contains(expected) {
            return event;
        }
    }
}

#[test]
fn profile_configuration_and_local_availability() {
    in_isolated_process("profile_configuration_and_local_availability", |home| {
        let plugin = HindsightPlugin::default();
        assert!(plugin.is_available());

        #[cfg(unix)]
        assert_eq!(plugin.backup_paths(), vec![home.join(".hindsight")]);

        let profile = home.join("profile");
        let directory = profile.join("hindsight");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.json");
        let load = |contents: Value| {
            std::fs::write(&path, contents.to_string()).unwrap();
            HindsightConfig::load(profile.to_str().unwrap())
        };

        let config = load(json!({
            "apiKey": "fixture-key",
            "api_url": OFFLINE_BASE,
            "mode": "cloud",
            "bank_id": "direct-bank",
            "banks": {"hermes": {"bankId": "legacy-bank", "budget": "low"}},
            "recall_budget": "high",
            "budget": "low",
            "bank_id_template": "{profile}--{session}",
            "memory_mode": "context",
            "recall_prefetch_method": "reflect",
            "auto_retain": false,
            "auto_recall": false,
            "retain_every_n_turns": 0,
            "retain_context": "project history",
            "recall_max_tokens": 256,
            "recall_max_input_chars": 80,
            "recall_types": [" experience ", "observation"],
            "recall_prompt_preamble": "Prior decisions:",
            "bank_mission": "Record project decisions",
            "retain_async": false,
            "retain_tags": [" project ", "project", "decision"],
            "observation_scopes": " combined ",
            "hindsight_timeout": 0,
            "timeout": 42
        }));
        assert_eq!(config.api_key, "fixture-key");
        assert_eq!(config.api_url, OFFLINE_BASE);
        assert_eq!(config.mode, "cloud");
        assert_eq!(config.bank_id, "direct-bank");
        assert_eq!(config.budget, "high");
        assert_eq!(config.bank_id_template, "{profile}--{session}");
        assert_eq!(config.memory_mode, "context");
        assert_eq!(config.prefetch_method, "reflect");
        assert!(!config.auto_retain);
        assert!(!config.auto_recall);
        assert_eq!(config.retain_every_n_turns, 1);
        assert_eq!(config.retain_context, "project history");
        assert_eq!(config.recall_max_tokens, 256);
        assert_eq!(config.recall_max_input_chars, 80);
        assert_eq!(config.recall_types, ["experience", "observation"]);
        assert_eq!(config.recall_prompt_preamble, "Prior decisions:");
        assert_eq!(config.bank_mission, "Record project decisions");
        assert!(!config.retain_async);
        assert_eq!(config.retain_tags, ["project", "decision"]);
        assert_eq!(config.observation_scopes, Some(json!("combined")));
        assert_eq!(config.timeout_secs, 1);

        let legacy = load(json!({
            "api_url": "",
            "banks": {"hermes": {"bankId": "legacy-bank", "budget": "low"}},
            "budget": "unsupported",
            "memory_mode": "unsupported",
            "recall_prefetch_method": "unsupported"
        }));
        assert_eq!(legacy.bank_id, "legacy-bank");
        assert_eq!(legacy.budget, "low");
        assert_eq!(legacy.mode, "local_external");
        assert_eq!(legacy.api_url, "http://localhost:8888");
        assert_eq!(legacy.memory_mode, "hybrid");
        assert_eq!(legacy.prefetch_method, "recall");

        let cloud = load(json!({
            "mode": "cloud",
            "banks": {"hermes": {"budget": "unsupported"}}
        }));
        assert_eq!(cloud.bank_id, "environment-bank");
        assert_eq!(cloud.budget, "mid");
        assert_eq!(cloud.api_url, "https://api.hindsight.vectorize.io");
    });
}

#[test]
fn session_hooks_leave_unconfigured_turns_and_ignore_blank_switches() {
    let plugin = HindsightPlugin::default();
    assert!(plugin.system_prompt_block().is_empty());
    assert!(plugin.take_pending_turns().is_empty());

    plugin.on_turn_start(17, "question");
    plugin.sync_turn("question", "answer", "session");
    assert_eq!(*plugin.turn_counter.lock().unwrap(), 17);
    assert!(plugin.take_pending_turns().is_empty());

    *plugin.session_id.lock().unwrap() = "original".into();
    *plugin.document_id.lock().unwrap() = "original-document".into();
    *plugin.prefetch_result.lock().unwrap() = "cached memory".into();
    *plugin.retain_batch_counter.lock().unwrap() = 4;
    plugin.session_turns.lock().unwrap().push("pending".into());

    plugin.on_session_end(&[]);
    plugin.shutdown();
    plugin.on_session_switch(" \t ", "original", true);

    assert_eq!(*plugin.session_id.lock().unwrap(), "original");
    assert_eq!(*plugin.document_id.lock().unwrap(), "original-document");
    assert_eq!(*plugin.prefetch_result.lock().unwrap(), "cached memory");
    assert_eq!(*plugin.turn_counter.lock().unwrap(), 17);
    assert_eq!(*plugin.retain_batch_counter.lock().unwrap(), 4);
    assert_eq!(plugin.take_pending_turns(), ["pending"]);

    let mut config = offline_config();
    config.auto_retain = false;
    *plugin.config.lock().unwrap() = Some(config);
    plugin.sync_turn("another question", "another answer", "original");
    plugin.on_session_end(&[]);
    plugin.shutdown();
    assert_eq!(*plugin.turn_counter.lock().unwrap(), 17);
    assert_eq!(*plugin.retain_batch_counter.lock().unwrap(), 4);
    assert!(plugin.take_pending_turns().is_empty());
}

#[test]
fn config_schema_exposes_connection_and_memory_settings() {
    let schema = HindsightPlugin::default().get_config_schema().unwrap();
    let fields = schema.as_array().unwrap();
    let field = |key: &str| fields.iter().find(|field| field["key"] == key).unwrap();

    assert_eq!(field("mode")["default"], "cloud");
    assert_eq!(field("mode")["choices"], json!(["cloud", "local_external"]));
    assert_eq!(field("api_url")["default"], DEFAULT_API_URL);
    assert_eq!(field("api_key")["secret"], true);
    assert_eq!(field("api_key")["env_var"], "HINDSIGHT_API_KEY");
    assert_eq!(field("bank_id")["default"], "hermes");
    assert_eq!(field("bank_id_template")["default"], "");
    assert_eq!(field("hindsight_timeout")["default"], 120);
    assert_eq!(
        field("recall_budget")["choices"],
        json!(["low", "mid", "high"])
    );
    assert_eq!(field("recall_types")["default"], "observation");
    assert_eq!(
        field("memory_mode")["choices"],
        json!(["hybrid", "context", "tools"])
    );
    assert_eq!(field("retain_tags")["default"], "");
    assert_eq!(field("observation_scopes")["default"], "");
}

#[test]
fn prefetch_consumes_cached_text_with_default_or_custom_preamble() {
    let plugin = configured_plugin(offline_config());
    assert!(plugin.prefetch("query", "session").is_empty());

    *plugin.prefetch_result.lock().unwrap() = "- Prefers tea".into();
    assert_eq!(
        plugin.prefetch("query", "session"),
        "# Hindsight Memory (persistent cross-session context)\n\
         Use this to answer questions about the user and prior sessions. \
         Do not call tools to look up information that is already present here.\n\n\
         - Prefers tea"
    );
    assert!(plugin.prefetch("query", "session").is_empty());

    plugin
        .config
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .recall_prompt_preamble = "Prior decisions:".into();
    *plugin.prefetch_result.lock().unwrap() = "Use the blue theme.".into();
    assert_eq!(
        plugin.prefetch("query", "session"),
        "Prior decisions:\n\nUse the blue theme."
    );
    assert!(plugin.prefetch("query", "session").is_empty());
}

#[test]
fn queue_prefetch_skips_unconfigured_tools_only_and_disabled_recall() {
    let plugin = HindsightPlugin::default();
    plugin.queue_prefetch("question", "session");
    assert_eq!(Arc::strong_count(&plugin.prefetch_result), 1);
    assert!(plugin.prefetch("question", "session").is_empty());

    for (mode, auto_recall) in [("tools", true), ("hybrid", false)] {
        let mut config = offline_config();
        config.memory_mode = mode.into();
        config.auto_recall = auto_recall;
        config.recall_max_input_chars = 1;
        let plugin = configured_plugin(config);
        *plugin.prefetch_result.lock().unwrap() = "previous result".into();

        // These guards must return before trying to truncate a multibyte query.
        plugin.queue_prefetch("éclair", "session");
        assert_eq!(Arc::strong_count(&plugin.prefetch_result), 1);
        assert_eq!(*plugin.prefetch_result.lock().unwrap(), "previous result");
    }
}

#[test]
fn background_prefetch_reports_recall_and_reflect_request_errors() {
    in_isolated_process(
        "background_prefetch_reports_recall_and_reflect_request_errors",
        |_| {
            let (subscriber, events) = debug_events();
            tracing::subscriber::set_global_default(subscriber).unwrap();

            for (method, route) in [("recall", "memories/recall"), ("reflect", "reflect")] {
                let mut config = offline_config();
                config.prefetch_method = method.into();
                config.recall_max_input_chars = 5;
                let plugin = configured_plugin(config);

                plugin.queue_prefetch("a longer query", "session");
                let event =
                    event_containing(&events, &format!("Hindsight {method} prefetch failed"));
                assert!(
                    event.contains(&format!(
                        "{OFFLINE_BASE}/v1/default/banks/{OFFLINE_BANK}/{route}"
                    )),
                    "{event}"
                );
                assert!(plugin.prefetch("query", "session").is_empty());
            }
        },
    );
}

fn tool_output(plugin: &HindsightPlugin, name: &str, args: &Value) -> Value {
    serde_json::from_str(&plugin.handle_tool_call(name, args)).unwrap()
}

#[test]
fn tools_reject_missing_parameters_and_unknown_names() {
    let unconfigured = HindsightPlugin::default();
    assert_eq!(
        tool_output(&unconfigured, "hindsight_recall", &json!({"query": "tea"})),
        json!({"error": "Hindsight not initialized"})
    );

    let plugin = configured_plugin(offline_config());
    for (name, field) in [
        ("hindsight_retain", "content"),
        ("hindsight_recall", "query"),
        ("hindsight_reflect", "query"),
    ] {
        let expected = json!({"error": format!("Missing required parameter: {field}")});
        assert_eq!(tool_output(&plugin, name, &json!({})), expected);
        let mut args = json!({});
        for invalid in [json!(""), json!(42)] {
            args[field] = invalid;
            assert_eq!(tool_output(&plugin, name, &args), expected);
        }
    }
    assert_eq!(
        tool_output(&plugin, "hindsight_other", &json!({})),
        json!({"error": "Unknown tool: hindsight_other"})
    );
}

#[test]
fn tools_report_endpoint_and_authorization_builder_errors_without_io() {
    let plugin = configured_plugin(offline_config());
    let client = Client::builder().no_proxy().build().unwrap();
    let invalid_key = "bad\nkey";
    let calls = [
        (
            "hindsight_retain",
            "memories",
            json!({"content": "Prefers tea", "context": "preference", "tags": [" default ", "tea"]}),
        ),
        (
            "hindsight_recall",
            "memories/recall",
            json!({"query": "tea"}),
        ),
        ("hindsight_reflect", "reflect", json!({"query": "tea"})),
    ];

    for (name, route, args) in &calls {
        let endpoint = format!("{OFFLINE_BASE}/v1/default/banks/{OFFLINE_BANK}/{route}");
        let output = tool_output(&plugin, name, args);
        let error = output["error"].as_str().unwrap();
        assert!(error.contains(&endpoint), "{name}: {output}");
        assert!(output.get("result").is_none());
    }

    plugin.config.lock().unwrap().as_mut().unwrap().api_key = invalid_key.into();
    for (name, route, args) in &calls {
        let endpoint = format!("{OFFLINE_BASE}/v1/default/banks/{OFFLINE_BANK}/{route}");
        let expected = client
            .post(endpoint)
            .bearer_auth(invalid_key)
            .build()
            .unwrap_err();
        assert_eq!(
            tool_output(&plugin, name, args),
            json!({"error": expected.to_string()}),
            "{name}"
        );
    }
}

#[test]
fn batch_retain_skips_empty_turns_and_logs_failed_nonempty_batches() {
    let config = offline_config();
    let (subscriber, events) = debug_events();
    tracing::subscriber::with_default(subscriber, || {
        retain_hindsight_turns(&config, " ", "batch-1", &[], "empty_batch");
        assert!(events
            .try_iter()
            .all(|event| !event.contains("Hindsight retain_batch")));

        let turn = hindsight_turn_payload("question", "answer", "2026-01-01T00:00:00Z");
        retain_hindsight_turns(&config, " ", "batch-1", &[turn], "offline_batch");
    });
    let event = event_containing(
        &events,
        "Hindsight retain_batch error (reason=offline_batch)",
    );
    assert!(
        event.contains(&format!(
            "{OFFLINE_BASE}/v1/default/banks/{OFFLINE_BANK}/memories"
        )),
        "{event}"
    );
}

struct CapabilityCacheKey(String);

impl Drop for CapabilityCacheKey {
    fn drop(&mut self) {
        if let Some(cache) = APPEND_CAPABILITY_CACHE.get() {
            let _ = cache.lock().unwrap().remove(&self.0);
        }
    }
}

#[test]
fn append_detection_uses_normalized_cache_keys_and_batch_fallbacks() {
    let client = Client::builder().no_proxy().build().unwrap();
    let key = CapabilityCacheKey(format!("hindsight-test://{}.invalid", uuid::Uuid::new_v4()));
    let base_with_slashes = format!("{}///", key.0);
    let cache = APPEND_CAPABILITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    assert!(!hindsight_api_supports_append(&client, "///", ""));
    assert!(!cache.lock().unwrap().contains_key(""));

    let (subscriber, events) = debug_events();
    let supported = tracing::subscriber::with_default(subscriber, || {
        hindsight_api_supports_append(&client, &base_with_slashes, "test-key")
    });
    assert!(!supported);
    let warning = event_containing(&events, "using unique batch document ids");
    assert!(warning.contains(&key.0), "{warning}");
    assert_eq!(cache.lock().unwrap().get(&key.0).copied(), Some(false));
    assert_eq!(
        hindsight_retain_target(&client, &base_with_slashes, "", "session", "batch-1"),
        ("batch-1".to_string(), None)
    );

    assert_eq!(
        cache.lock().unwrap().insert(key.0.clone(), true),
        Some(false)
    );
    assert!(hindsight_api_supports_append(
        &client,
        &base_with_slashes,
        ""
    ));
    assert_eq!(
        hindsight_retain_target(&client, &base_with_slashes, "", " session ", "batch-1"),
        ("session".to_string(), Some("append"))
    );
    assert_eq!(
        hindsight_retain_target(&client, &base_with_slashes, "", "\t ", "batch-1"),
        ("batch-1".to_string(), None)
    );
}

#[test]
fn bank_templates_and_document_ids_normalize_blank_inputs() {
    assert_eq!(nonempty_str(" \t\n"), None);
    assert_eq!(urlencode_path("Bank-_.~ /?#%"), "Bank-_.~%20%2F%3F%23%25");
    let placeholders = [("workspace", " North / West ".to_string())];
    assert_eq!(
        resolve_bank_id_template(
            "__team----{workspace}____archive--",
            "fallback",
            &placeholders
        ),
        "team-North-West_archive"
    );
    assert_eq!(
        resolve_bank_id_template(
            "---{workspace}___",
            "fallback",
            &[("workspace", " ! ".into())]
        ),
        "fallback"
    );

    let first = scoped_document_id(" \t");
    let second = scoped_document_id("");
    assert_ne!(first, second);
    let pid = std::process::id().to_string();
    for id in [&first, &second] {
        let parts: Vec<_> = id.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], pid);
        assert!(parts[1].parse::<u128>().is_ok());
        assert!(parts[2].parse::<u64>().is_ok());
    }
}

#[test]
fn malformed_configuration_lists_and_minimum_version_are_rejected() {
    assert_eq!(parse_recall_types_value(&json!([" ", 7, null])), None);
    assert_eq!(parse_retain_tags_value(&json!([" ", 7, null])), None);
    assert_eq!(
        parse_observation_scopes_value(&json!([[" "], [], null, 7])),
        None
    );
    assert!(hindsight_version_meets_minimum(Some("v0.5.1"), "0.5.0"));
    assert!(!hindsight_version_meets_minimum(Some("v0.5.1"), "0.bad.0"));
}
