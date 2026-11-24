use super::*;

const OFFLINE_URL: &str = "not-an-absolute-url";
const CHILD_CASE: &str = "HERMES_CONTEXTLATTICE_COVERAGE_CHILD_CASE";
const CHILD_DONE: &str = "contextlattice coverage child completed";

const PRESSURE_ENV: [&str; 7] = [
    "CONTEXTLATTICE_PRESSURE_AWARE",
    "CONTEXTLATTICE_PRESSURE_PENDING_THRESHOLD",
    "CONTEXTLATTICE_PRESSURE_COOLDOWN_THRESHOLD",
    "CONTEXTLATTICE_PRESSURE_STATUS_TTL_SECS",
    "CONTEXTLATTICE_PRESSURE_FAST_SOURCES",
    "CONTEXTLATTICE_PRESSURE_FORCE_FAST_MODE",
    "CONTEXTLATTICE_PRESSURE_FORCE_FAST_PREFETCH",
];

fn run_child(test: &str, case: &str, overrides: &[(&str, &str)]) {
    // Isolate environment overrides and the worker test's tracing subscriber
    // from the rest of the test harness.
    let module = module_path!().split_once("::").unwrap().1;
    let test_name = format!("{module}::{test}");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
    command
        .args(["--exact", test_name.as_str(), "--nocapture"])
        .env(CHILD_CASE, case);
    for key in PRESSURE_ENV {
        command.env_remove(key);
    }
    command.envs(overrides.iter().copied());

    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(CHILD_DONE),
        "{test_name} ({case}) did not complete:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn offline_config() -> ContextLatticeConfig {
    // Request construction fails locally, before DNS or a socket connection.
    ContextLatticeConfig {
        orchestrator_url: format!("{OFFLINE_URL}///"),
        project: "fixture-project".into(),
        topic_path: "fixture/topic".into(),
        agent_id: "fixture-agent".into(),
        timeout_secs: 1.0,
        include_grounding: true,
        include_retrieval_debug: false,
        default_retrieval_mode: "deep".into(),
        default_file_name: "notes/fixture.md".into(),
        pressure_aware: false,
        pressure_pending_threshold: 12,
        pressure_cooldown_threshold: 3,
        pressure_status_ttl_secs: 2.5,
        pressure_fast_sources: vec!["rollups".into()],
        pressure_force_fast_mode: true,
        pressure_force_fast_prefetch: true,
        api_key: Some("fixture-token".into()),
    }
}

#[test]
fn parses_pressure_flags_and_source_lists() {
    for input in ["1", " TRUE ", "Yes", "\tON\n"] {
        assert_eq!(parse_bool_flag(input), Some(true), "{input:?}");
    }
    for input in ["0", " FALSE ", "No", "\tOFF\n"] {
        assert_eq!(parse_bool_flag(input), Some(false), "{input:?}");
    }
    for input in ["", "enabled", "2"] {
        assert_eq!(parse_bool_flag(input), None, "{input:?}");
    }
    assert_eq!(
        parse_csv_sources(" rollups, , qdrant,\t,rollups,"),
        ["rollups", "qdrant", "rollups"]
    );
    assert!(parse_csv_sources(" ,\t,").is_empty());
}

#[test]
fn loads_file_and_environment_settings() {
    let case = std::env::var(CHILD_CASE).unwrap_or_default();
    match case.as_str() {
        "file" | "valid-env" | "invalid-env" => {}
        _ => {
            let test = "loads_file_and_environment_settings";
            run_child(test, "file", &[]);
            let valid = [
                " On ",
                " 71 ",
                " 9 ",
                " 0.01 ",
                " pg , , vector ",
                "1",
                "YES",
            ];
            let invalid = ["maybe", "-1", "lots", "soon", " , , ", "2", "maybe"];
            let valid_overrides: Vec<_> = PRESSURE_ENV.into_iter().zip(valid).collect();
            let invalid_overrides: Vec<_> = PRESSURE_ENV.into_iter().zip(invalid).collect();
            run_child(test, "valid-env", &valid_overrides);
            run_child(test, "invalid-env", &invalid_overrides);
            return;
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_str().unwrap();
    let path = dir.path().join("contextlattice.json");
    std::fs::write(
        &path,
        json!({
            "orchestrator_url": format!("{OFFLINE_URL}///"),
            "project": "file-project",
            "topic_path": "",
            "agent_id": "file-agent",
            "timeout_secs": 90,
            "include_grounding": false,
            "include_retrieval_debug": false,
            "retrieval_mode": "deep",
            "file_name": "notes/file.md",
            "pressure_aware": false,
            "pressure_pending_threshold": 27,
            "pressure_cooldown_threshold": 8,
            "pressure_status_ttl_secs": 45,
            "pressure_fast_sources": [" rollups ", 3, "", " qdrant "],
            "pressure_force_fast_mode": false,
            "pressure_force_fast_prefetch": false,
            "api_key": "file-token"
        })
        .to_string(),
    )
    .unwrap();

    let plugin = ContextLatticeMemoryPlugin::default();
    plugin.initialize("file-session", home);
    let cfg = plugin.config_snapshot().unwrap();
    assert_eq!(cfg.orchestrator_url, OFFLINE_URL);
    assert_eq!(cfg.project, "file-project");
    assert!(cfg.topic_path.is_empty());
    assert_eq!(cfg.agent_id, "file-agent");
    assert_eq!(
        Duration::from_secs_f64(cfg.timeout_secs),
        Duration::from_secs(60)
    );
    assert!(!cfg.include_grounding);
    assert!(!cfg.include_retrieval_debug);
    assert_eq!(cfg.default_retrieval_mode, "deep");
    assert_eq!(cfg.default_file_name, "notes/file.md");
    assert_eq!(cfg.api_key.as_deref(), Some("file-token"));
    assert!(plugin
        .system_prompt_block()
        .contains("Active. Project: file-project."));

    let ttl = Duration::from_secs_f64(cfg.pressure_status_ttl_secs);
    if case == "valid-env" {
        assert!(cfg.pressure_aware);
        assert_eq!(cfg.pressure_pending_threshold, 71);
        assert_eq!(cfg.pressure_cooldown_threshold, 9);
        assert_eq!(ttl, Duration::from_millis(200));
        assert_eq!(cfg.pressure_fast_sources, ["pg", "vector"]);
        assert!(cfg.pressure_force_fast_mode);
        assert!(cfg.pressure_force_fast_prefetch);
    } else {
        assert!(!cfg.pressure_aware);
        assert_eq!(cfg.pressure_pending_threshold, 27);
        assert_eq!(cfg.pressure_cooldown_threshold, 8);
        assert_eq!(ttl, Duration::from_secs(30));
        assert_eq!(cfg.pressure_fast_sources, ["rollups", "qdrant"]);
        assert!(!cfg.pressure_force_fast_mode);
        assert!(!cfg.pressure_force_fast_prefetch);
    }

    if case == "file" {
        std::fs::remove_file(&path).unwrap();
        let fallback = ContextLatticeConfig::load(home);
        std::fs::write(
            &path,
            r#"{
                "orchestrator_url":"",
                "project":"",
                "agent_id":"",
                "file_name":"",
                "api_key":"",
                "pressure_fast_sources":[false," "],
                "timeout_secs":0
            }"#,
        )
        .unwrap();
        let empty = ContextLatticeConfig::load(home);
        assert_eq!(empty.orchestrator_url, fallback.orchestrator_url);
        assert_eq!(empty.project, fallback.project);
        assert_eq!(empty.agent_id, fallback.agent_id);
        assert_eq!(empty.default_file_name, fallback.default_file_name);
        assert_eq!(empty.api_key, fallback.api_key);
        assert_eq!(empty.pressure_fast_sources, fallback.pressure_fast_sources);
        assert_eq!(
            Duration::from_secs_f64(empty.timeout_secs),
            Duration::from_secs(1)
        );
    }

    println!("{CHILD_DONE}");
}

#[test]
fn uninitialized_plugin_reports_error_and_prefetch_consumes_buffer() {
    let plugin = ContextLatticeMemoryPlugin::default();
    let expected_error = "ContextLattice plugin not initialized";
    assert_eq!(plugin.config_snapshot().unwrap_err(), expected_error);
    assert_eq!(
        serde_json::from_str::<Value>(
            &plugin.handle_tool_call("contextlattice_search", &json!({"query":"needle"}))
        )
        .unwrap(),
        json!({"error":expected_error})
    );
    let prompt = plugin.system_prompt_block();
    assert!(prompt.starts_with("# ContextLattice Memory\nActive. Project: contextlattice."));
    for tool in [
        "contextlattice_search",
        "contextlattice_context_pack",
        "contextlattice_write",
    ] {
        assert!(prompt.contains(tool), "{prompt}");
    }

    assert!(plugin.prefetch("needle", "session").is_empty());
    *plugin.prefetch_result.lock().unwrap() = "- [note.md] remembered".into();
    plugin.queue_prefetch(" \n\t ", "session");
    plugin.queue_prefetch("needle", "session");
    plugin.sync_turn(" ", "answer", "session");
    plugin.sync_turn("question", "answer", "session");

    assert_eq!(
        plugin.prefetch("needle", "session"),
        "## ContextLattice Memory\n- [note.md] remembered"
    );
    assert!(plugin.prefetch("needle", "session").is_empty());
}

#[test]
fn pressure_cache_ignores_disabled_and_expired_status() {
    let mut cfg = offline_config();
    let current = QueuePressureSnapshot {
        pending_total: 19,
        cooldown_active: 2,
        pressure_active: true,
        captured_at: Instant::now(),
    };
    let cache = Arc::new(Mutex::new(Some(current.clone())));
    assert!(ContextLatticeMemoryPlugin::queue_pressure_snapshot(&cfg, &cache).is_none());

    cfg.pressure_aware = true;
    let reused = ContextLatticeMemoryPlugin::queue_pressure_snapshot(&cfg, &cache).unwrap();
    assert_eq!(reused.captured_at, current.captured_at);
    assert_eq!(reused.pending_total, 19);
    assert_eq!(reused.cooldown_active, 2);
    assert!(reused.pressure_active);

    let expired = Instant::now() - Duration::from_secs(10);
    cache.lock().unwrap().as_mut().unwrap().captured_at = expired;
    assert!(ContextLatticeMemoryPlugin::queue_pressure_snapshot(&cfg, &cache).is_none());
    assert_eq!(cache.lock().unwrap().as_ref().unwrap().captured_at, expired);

    let error =
        ContextLatticeMemoryPlugin::send_no_body(&cfg, Method::GET, "//v1/info").unwrap_err();
    let prefix = format!("ContextLattice request GET {OFFLINE_URL}/v1/info failed: ");
    assert!(error.starts_with(&prefix), "{error}");
}

#[test]
fn pressure_tuning_leaves_payload_unchanged_without_active_pressure() {
    let cfg = offline_config();
    let inactive = QueuePressureSnapshot {
        pending_total: 1,
        cooldown_active: 0,
        pressure_active: false,
        captured_at: Instant::now(),
    };
    let original = json!({"retrieval_mode":"deep", "sources":["original"]});
    for pressure in [None, Some(&inactive)] {
        let mut payload = original.clone();
        ContextLatticeMemoryPlugin::apply_pressure_tuning_to_payload(
            &cfg,
            &mut payload,
            pressure,
            true,
        );
        assert_eq!(payload, original);
    }
}

#[test]
fn extracts_nested_results_and_skips_blank_summaries() {
    let short = "é".repeat(240);
    let response = json!({
        "results": null,
        "context_pack": {"results": [
            {"file":"one.md", "summary":"  first  ", "content":"not preferred"},
            {"file":"blank.md", "summary":" \t\n"},
            {"file":"long.md", "content":format!("{short}x")},
            {"text":"  last  "}
        ]}
    });
    assert_eq!(
        ContextLatticeMemoryPlugin::extract_search_lines(&response, 0),
        ["- [one.md] first"]
    );
    assert_eq!(
        ContextLatticeMemoryPlugin::extract_search_lines(&response, 2),
        ["- [one.md] first"]
    );
    assert_eq!(
        ContextLatticeMemoryPlugin::extract_search_lines(&response, 8),
        [
            "- [one.md] first".to_string(),
            format!("- [long.md] {short}"),
            "- [memory] last".to_string(),
        ]
    );
}

#[test]
fn tools_validate_arguments_and_report_local_request_errors() {
    let plugin = ContextLatticeMemoryPlugin::default();
    *plugin.config.lock().unwrap() = Some(offline_config());
    let call = |name: &str, args: &Value| {
        serde_json::from_str::<Value>(&plugin.handle_tool_call(name, args)).unwrap()
    };

    for name in ["contextlattice_search", "contextlattice_context_pack"] {
        assert_eq!(
            call(name, &json!({"query":" \t"})),
            json!({"error":"query is required"})
        );
        assert_eq!(
            call(name, &json!({"query":7})),
            json!({"error":"query is required"})
        );
    }
    assert_eq!(
        call("contextlattice_write", &json!({"content":"  "})),
        json!({"error":"content is required"})
    );
    assert_eq!(
        call("bogus", &json!({})),
        json!({"error":"Unknown tool: bogus"})
    );

    for (tool, path, args) in [
        (
            "contextlattice_search",
            "memory/search",
            json!({"query":" needle "}),
        ),
        (
            "contextlattice_search",
            "memory/search",
            json!({"query":"needle","topic_path":"scope","retrieval_mode":"deep","limit":3}),
        ),
        (
            "contextlattice_context_pack",
            "memory/context-pack",
            json!({"query":"needle"}),
        ),
        (
            "contextlattice_context_pack",
            "memory/context-pack",
            json!({"query":"needle","topic_path":" ","retrieval_mode":"fast"}),
        ),
        (
            "contextlattice_write",
            "memory/write",
            json!({"content":" checkpoint "}),
        ),
        (
            "contextlattice_write",
            "memory/write",
            json!({"content":"checkpoint","file_name":"notes/custom.md","topic_path":"scope"}),
        ),
    ] {
        let result = call(tool, &args);
        let error = result["error"].as_str().unwrap();
        let prefix = format!("ContextLattice request POST {OFFLINE_URL}/{path} failed: ");
        assert!(error.starts_with(&prefix), "{tool}: {error}");
    }
}

struct LogWriter(std::sync::mpsc::Sender<String>);

impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .send(String::from_utf8_lossy(bytes).into_owned())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn expect_log(receiver: &std::sync::mpsc::Receiver<String>, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let line = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| panic!("waiting for {text:?}: {error}"));
        if line.contains(text) {
            return;
        }
    }
}

#[test]
fn background_workers_report_failed_requests() {
    if std::env::var(CHILD_CASE).as_deref() != Ok("workers") {
        run_child("background_workers_report_failed_requests", "workers", &[]);
        return;
    }

    let (sender, receiver) = std::sync::mpsc::channel();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || LogWriter(sender.clone()))
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let plugin = ContextLatticeMemoryPlugin::default();
    let mut cfg = offline_config();
    cfg.pressure_aware = true;
    cfg.pressure_status_ttl_secs = 30.0;
    *plugin.config.lock().unwrap() = Some(cfg);
    *plugin.session_id.lock().unwrap() = "stored-session".into();

    for (force_fast, mode) in [(true, "fast"), (false, "balanced")] {
        plugin
            .config
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .pressure_force_fast_prefetch = force_fast;
        *plugin.pressure_cache.lock().unwrap() = Some(QueuePressureSnapshot {
            pending_total: 12,
            cooldown_active: 1,
            pressure_active: true,
            captured_at: Instant::now(),
        });
        *plugin.prefetch_result.lock().unwrap() = "- [older.md] preserved".into();
        plugin.queue_prefetch(" needle ", "stored-session");
        expect_log(
            &receiver,
            &format!("pending_total=12 cooldown_active=1 mode={mode} sources=[\"rollups\"]"),
        );
        expect_log(
            &receiver,
            &format!("ContextLattice prefetch failed: ContextLattice request POST {OFFLINE_URL}/memory/search failed: "),
        );
        assert_eq!(
            plugin.prefetch("needle", "stored-session"),
            "## ContextLattice Memory\n- [older.md] preserved"
        );
    }

    for session in ["", "explicit-session"] {
        plugin.sync_turn(" question ", " answer ", session);
        expect_log(
            &receiver,
            &format!("ContextLattice sync_turn write failed: ContextLattice request POST {OFFLINE_URL}/memory/write failed: "),
        );
    }

    println!("{CHILD_DONE}");
}

#[test]
fn config_schema_exposes_pressure_defaults_and_marks_api_key_secret() {
    let plugin = ContextLatticeMemoryPlugin::default();
    let schema = plugin.get_config_schema().unwrap();
    let fields = schema.as_array().unwrap();
    assert_eq!(
        fields
            .iter()
            .map(|field| field["key"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "orchestrator_url",
            "project",
            "topic_path",
            "agent_id",
            "timeout_secs",
            "pressure_aware",
            "pressure_pending_threshold",
            "pressure_cooldown_threshold",
            "pressure_status_ttl_secs",
            "pressure_fast_sources",
            "api_key",
        ]
    );
    assert_eq!(fields[0]["default"], DEFAULT_ORCHESTRATOR_URL);
    assert_eq!(fields[5]["default"], json!(true));
    assert_eq!(fields[6]["default"], json!(12));
    assert_eq!(fields[7]["default"], json!(3));
    assert_eq!(fields[8]["default"], json!(2.5));
    assert_eq!(
        fields[9]["default"],
        json!(["topic_rollups", "qdrant", "weaviate", "postgres_pgvector"])
    );
    assert_eq!(fields[10]["secret"], json!(true));
    assert_eq!(fields[10]["env_var"], "CONTEXTLATTICE_API_KEY");

    assert_eq!(
        plugin.save_config(&json!({"project":"new-project"})),
        Ok(())
    );
    for invalid in [json!(null), json!([]), json!("project")] {
        assert_eq!(
            plugin.save_config(&invalid).unwrap_err(),
            "config must be a JSON object"
        );
    }
}
