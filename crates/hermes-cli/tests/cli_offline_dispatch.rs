#[path = "support/isolated_hermes.rs"]
mod support;

use std::time::Duration;

use support::isolated_hermes;

#[tokio::test]
async fn offline_hermes_export_import_publishes_only_valid_history() {
    const EXPORT: &str =
        include_str!("../../hermes-parity-tests/fixtures/session_import/hermes_export.jsonl");
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("export.jsonl");
    let profile = sandbox.path().join("imported");
    let run_import = || {
        let mut command = isolated_hermes(sandbox.path());
        command
            .arg("import-hermes-sessions")
            .arg(&source)
            .arg("--profile-dir")
            .arg(&profile);
        tokio::time::timeout(Duration::from_secs(15), command.output())
    };

    std::fs::write(&source, format!("{}\n{{}}\n", EXPORT.trim_end())).unwrap();
    let rejected = run_import().await.unwrap().unwrap();
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    assert!(String::from_utf8_lossy(&rejected.stderr)
        .contains("Hermes export line 3: invalid session record"));
    assert!(!profile.exists());

    std::fs::write(&source, EXPORT).unwrap();
    let accepted = run_import().await.unwrap().unwrap();
    assert!(accepted.status.success(), "{accepted:?}");
    let stdout = String::from_utf8_lossy(&accepted.stdout);
    assert!(stdout.contains("Imported 2 sessions with 5 active messages"));
    assert!(stdout.contains("Excluded 2 inactive or metadata rows"));
    assert!(profile.join("sessions.db").is_file());
    let history = hermes_agent::SessionPersistence::new(&profile)
        .load_session("20261109_103200_c0ffeed1")
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].content.as_deref(), Some("Continue the notes."));
    assert!(!sandbox.path().join("hermes").exists());
}

#[tokio::test]
async fn offline_cli_rejects_two_explicit_homes_before_creating_either() {
    let sandbox = tempfile::tempdir().unwrap();
    let custom_home = sandbox.path().join("custom");
    let mut command = isolated_hermes(sandbox.path());
    command
        .args(["--profile", "office", "--config-dir"])
        .arg(&custom_home)
        .arg("status");
    let rejected = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    assert!(String::from_utf8_lossy(&rejected.stderr)
        .contains("--profile and --config-dir each choose the home"));
    assert!(!custom_home.exists());
    assert!(!sandbox.path().join("hermes").exists());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn offline_up_explains_linux_service_fallback_and_dry_run() {
    for (args, expected) in [
        (
            &["up", "--dry-run", "--force"][..],
            [
                "service install/start is not implemented for this platform",
                "Dry-run: would ensure the gateway service is installed",
            ],
        ),
        (
            &["up"][..],
            [
                "use `hermes-ultra gateway start` for foreground runtime",
                "Use `hermes-ultra logs --follow` for runtime logs.",
            ],
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            isolated_hermes(root.path()).args(args).output(),
        )
        .await
        .expect("up command timed out")
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Reformed Agent up"), "{stdout}");
        for line in expected {
            assert!(stdout.contains(line), "{stdout}");
        }
        assert!(!root.path().join("hermes/gateway.lock").exists());
    }
}

#[tokio::test]
async fn offline_plugin_toggle_persists_general_and_provider_choices() {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let root = tempfile::tempdir().unwrap();
    let plugins = root.path().join("hermes/plugins");
    let general = plugins.join("fixture-general");
    let provider_a = plugins.join("fixture-provider-a");
    let provider_b = plugins.join("fixture-provider-b");
    for (dir, name, kind) in [
        (&general, "fixture-general", ""),
        (&provider_a, "fixture-provider-a", "kind: exclusive\n"),
        (&provider_b, "fixture-provider-b", "kind: exclusive\n"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("plugin.yaml"),
            format!("name: {name}\nversion: '1.0'\ndescription: Offline fixture\n{kind}"),
        )
        .unwrap();
    }
    let general_marker = general.join(".disabled");
    let marker_a = provider_a.join(".disabled");
    let marker_b = provider_b.join(".disabled");
    std::fs::write(&marker_b, "").unwrap();

    for (input, general_disabled, message) in [
        ("1\n3\n", true, "Updated plugin state:"),
        ("1\n1\n", false, "keeping provider state unchanged"),
    ] {
        let child = isolated_hermes(root.path())
            .arg("plugins")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = tokio::time::timeout(Duration::from_secs(15), async {
            let mut child = child;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(input.as_bytes()).await.unwrap();
            drop(stdin);
            child.wait_with_output().await.unwrap()
        })
        .await
        .expect("plugin toggle timed out");
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("General Plugins"), "{stdout}");
        assert!(stdout.contains("Provider Plugins"), "{stdout}");
        assert!(stdout.contains(message), "{stdout}");
        assert_eq!(general_marker.is_file(), general_disabled);
        assert!(marker_a.is_file());
        assert!(!marker_b.exists());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn offline_backup_and_import_round_trip_a_disposable_profile() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let source_home = source.path().join("hermes");
    let target_home = target.path().join("hermes");
    let relative = std::path::Path::new("notes").join("fixture.txt");
    let contents = b"Local backup fixture\n";
    std::fs::create_dir_all(source_home.join("notes")).unwrap();
    std::fs::write(source_home.join(&relative), contents).unwrap();

    let archive = source.path().join("profile.tar.gz");
    let backup = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(source.path())
            .arg("backup")
            .arg(&archive)
            .output(),
    )
    .await
    .expect("backup timed out")
    .unwrap();
    assert!(backup.status.success(), "{backup:?}");
    assert!(String::from_utf8_lossy(&backup.stdout).contains("Backup complete"));
    assert!(archive.is_file());

    let restored = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(target.path())
            .arg("import")
            .arg(&archive)
            .output(),
    )
    .await
    .expect("import timed out")
    .unwrap();
    assert!(restored.status.success(), "{restored:?}");
    assert!(String::from_utf8_lossy(&restored.stdout).contains("Import complete"));
    assert_eq!(
        std::fs::read(target_home.join(&relative)).unwrap(),
        contents
    );

    let missing = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(target.path())
            .arg("import")
            .arg(target.path().join("missing.tar.gz"))
            .output(),
    )
    .await
    .expect("missing archive check timed out")
    .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("Backup archive not found"));
    assert_eq!(std::fs::read(target_home.join(relative)).unwrap(), contents);
}

#[tokio::test]
async fn offline_status_reports_route_health_and_local_state() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    let logs = home.join("logs");
    let route_health = logs.join("route-health.json");
    let status = || async {
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            isolated_hermes(sandbox.path())
                .env("HERMES_TOOL_POLICY_MODE", "audit")
                .env("HERMES_TOOL_POLICY_PRESET", "strict")
                .arg("status")
                .output(),
        )
        .await
        .expect("status timed out")
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };

    assert!(status().await.contains("Route health: (not generated)"));

    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(&route_health, "fixture invalid JSON").unwrap();
    assert!(status()
        .await
        .contains("Route health: unavailable (failed to parse"));

    std::fs::write(
        &route_health,
        r#"{"summary":{"overall":"healthy","entries":2,"average_score":0.85}}"#,
    )
    .unwrap();
    std::fs::write(
        home.join("config.yaml"),
        "model: openai:gpt-4o-mini\nplatforms:\n  api_server:\n    enabled: true\n",
    )
    .unwrap();
    let sessions = home.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("fixture.json"), "{}").unwrap();
    let profiles = home.join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(profiles.join("sample.yml"), "{}").unwrap();
    std::fs::write(profiles.join("ignored.txt"), "not a profile").unwrap();

    let summary = status().await;
    for expected in [
        "Model:   openai:gpt-4o-mini",
        "Platforms: api_server",
        "Tool policy: mode=audit preset=strict",
        "Route health: overall=healthy entries=2 avg_score=0.850",
        "Saved sessions: 1",
        "Profiles: sample",
    ] {
        assert!(
            summary.lines().any(|line| line.starts_with(expected)),
            "{summary}"
        );
    }
    assert!(!summary.contains("ignored"));
}

#[tokio::test]
async fn offline_sessions_repair_preserves_a_backup_and_restores_search() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    let persistence = hermes_agent::SessionPersistence::new(&home);
    let repair = || async {
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            isolated_hermes(sandbox.path())
                .args(["sessions", "repair"])
                .output(),
        )
        .await
        .expect("sessions repair timed out")
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };

    assert!(repair().await.contains("nothing to repair"));
    let messages = [
        hermes_core::Message::user("Can you find the fixture?"),
        hermes_core::Message::assistant("The orchard is in the fixture."),
    ];
    persistence
        .persist_session("repair-fixture", &messages, None, None, None, None)
        .unwrap();
    let path = persistence.db_path();
    {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute_batch("PRAGMA writable_schema=ON")
            .unwrap();
        let duplicates = connection
            .execute(
                "INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql)
                 SELECT type, name, tbl_name, rootpage, sql
                 FROM sqlite_master
                 WHERE name = 'messages_fts'",
                [],
            )
            .unwrap();
        assert_eq!(duplicates, 1);
    }
    let reason = persistence.db_health_error().unwrap();
    assert!(hermes_agent::SessionPersistence::is_malformed_db_error_message(&reason));
    let original = std::fs::read(path).unwrap();

    let repaired = repair().await;
    assert!(repaired.contains("Repaired sessions.db (strategy: dedup_schema)."));
    let backup = repaired
        .lines()
        .find_map(|line| line.strip_prefix("Backup: "))
        .map(std::path::Path::new)
        .unwrap();
    assert_eq!(backup.parent(), Some(home.as_path()));
    assert_eq!(std::fs::read(backup).unwrap(), original);
    assert!(repair().await.contains("opens cleanly; no repair needed"));

    let connection = rusqlite::Connection::open(path).unwrap();
    let content: String = connection
        .query_row(
            "SELECT content FROM messages_fts WHERE messages_fts MATCH 'orchard'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(content, "The orchard is in the fixture.");
}

#[cfg(unix)]
#[tokio::test]
async fn offline_legacy_migration_preserves_existing_files_and_allows_cleanup_to_be_cancelled() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join(".openclaw");
    let target = sandbox.path().join("hermes");
    let profile = sandbox.path().join(".bashrc");
    let shell_config = "alias openclaw='echo fixture'\n";
    std::fs::create_dir_all(source.join("skills/fixture")).unwrap();
    std::fs::create_dir(&target).unwrap();
    std::fs::write(source.join("USER.md"), "Legacy user fixture\n").unwrap();
    std::fs::write(source.join("SOUL.md"), "Legacy personality\n").unwrap();
    std::fs::write(source.join("skills/fixture/SKILL.md"), "A fixture skill\n").unwrap();
    std::fs::write(target.join("SOUL.md"), "Current personality\n").unwrap();
    std::fs::write(&profile, shell_config).unwrap();

    for (action, expected) in [
        ("status", "Shell config references found:"),
        ("migrate", "Migration complete:"),
        ("cleanup", "Cleanup cancelled."),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            isolated_hermes(sandbox.path())
                .args(["claw", action])
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await
        .expect("legacy command timed out")
        .unwrap();
        assert!(output.status.success(), "{action}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(expected), "{action}: {stdout}");
        if action == "cleanup" {
            assert!(stdout.contains("~3 file(s)"), "{stdout}");
        }
    }
    assert_eq!(std::fs::read_to_string(profile).unwrap(), shell_config);
    assert_eq!(
        std::fs::read_to_string(source.join("USER.md")).unwrap(),
        "Legacy user fixture\n"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("USER.md")).unwrap(),
        "Legacy user fixture\n"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("SOUL.md")).unwrap(),
        "Current personality\n"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("skills/openclaw-imports/fixture/SKILL.md")).unwrap(),
        "A fixture skill\n"
    );
}

#[tokio::test]
async fn offline_tool_toggles_persist_without_duplicate_config_entries() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("hermes/config.yaml");
    for (args, expected) in [
        (&["tools", "list", "--summary"][..], "enabled: (none)"),
        (&["tools", "disable", "todo"], "Disabled tool 'todo'"),
        (&["tools", "disable", "todo"], "Disabled tool 'todo'"),
        (&["tools", "list"], "todo [disabled]"),
        (
            &["tools", "list", "--platform", "telegram", "--summary"],
            "Tool summary (platform=telegram)",
        ),
        (&["tools", "enable", "todo"], "Enabled tool 'todo'"),
        (&["tools", "enable", "todo"], "Enabled tool 'todo'"),
        (&["tools", "list"], "todo [enabled]"),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            isolated_hermes(root.path()).args(args).output(),
        )
        .await
        .expect("tool configuration command timed out")
        .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(expected), "{args:?}: {stdout}");
        if args.get(1) == Some(&"disable") {
            let config = hermes_config::load_user_config_file(&config_path).unwrap();
            assert_eq!(config.tools_config.disabled, ["todo"]);
            assert!(config.tools_config.enabled.is_empty());
        }
        if args.get(1) == Some(&"enable") {
            let config = hermes_config::load_user_config_file(&config_path).unwrap();
            assert_eq!(config.tools_config.enabled, ["todo"]);
            assert!(config.tools_config.disabled.is_empty());
        }
    }
    let saved = std::fs::read(&config_path).unwrap();
    for action in ["disable", "enable"] {
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            isolated_hermes(root.path())
                .args(["tools", action])
                .output(),
        )
        .await
        .expect("missing tool name timed out")
        .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("tools {action}: usage")));
        assert_eq!(std::fs::read(&config_path).unwrap(), saved);
    }
}

#[tokio::test]
async fn offline_session_eval_applies_the_cli_limit_and_reports_write_errors() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("sample-sessions");
    std::fs::create_dir(&sessions).unwrap();
    let fixture = r#"{"messages":[{"role":"user","content":"Read the fixture"},{"role":"assistant","content":"<tool_call>read_fixture</tool_call>"}]}"#;
    for file in ["alpha.json", "beta.json"] {
        std::fs::write(sessions.join(file), fixture).unwrap();
    }

    let output = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(root.path())
            .args([
                "session-eval",
                "--sessions-dir",
                "sample-sessions",
                "--max-sessions",
                "1",
                "--out-json",
                "report.json",
                "--json",
            ])
            .output(),
    )
    .await
    .expect("session-eval timed out")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["summary"]["sessions_analyzed"], 1);
    let reported_sessions = std::path::Path::new(report["sessions_dir"].as_str().unwrap());
    assert_eq!(
        reported_sessions.canonicalize().unwrap(),
        sessions.canonicalize().unwrap()
    );
    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("report.json")).unwrap()).unwrap();
    assert_eq!(report, on_disk);

    let output = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(root.path())
            .args(["session-eval", "--out-json", "report.json/nested"])
            .output(),
    )
    .await
    .expect("session-eval timed out")
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error:") && stderr.contains("report.json"),
        "{stderr}"
    );
    let preserved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("report.json")).unwrap()).unwrap();
    assert_eq!(preserved, report);
}

#[tokio::test]
async fn offline_agent_import_plans_then_applies_into_the_explicit_home() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("fixture-agent");
    std::fs::create_dir(&source).unwrap();
    let guidance = "# Agent guide\n- Verify fixtures before publishing.\n";
    std::fs::write(source.join("AGENTS.md"), guidance).unwrap();
    let selected = root.path().join("selected");
    let memory = selected.join("memories/MEMORY.md");

    for (apply, expected) in [(false, "would import"), (true, "imported")] {
        let mut command = isolated_hermes(root.path());
        command
            .arg("--config-dir")
            .arg(&selected)
            .args(["import-agent", "codex", "--source"])
            .arg(&source);
        if apply {
            command.arg("--apply");
        }
        let output = tokio::time::timeout(Duration::from_secs(15), command.output())
            .await
            .expect("import-agent timed out")
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.lines().any(|line| {
            line.starts_with(expected) && line.contains("AGENTS.md: 1 of 1 entries")
        }));
        assert_eq!(memory.is_file(), apply);
    }
    assert!(std::fs::read_to_string(memory)
        .unwrap()
        .contains("Verify fixtures before publishing."));
    assert_eq!(
        std::fs::read_to_string(source.join("AGENTS.md")).unwrap(),
        guidance
    );
    assert!(!root.path().join("hermes/memories/MEMORY.md").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn native_elite_check_records_each_gate_and_enforces_strict_mode() {
    use std::os::unix::fs::PermissionsExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    let docs = root.path().join("docs/parity");
    std::fs::create_dir(&bin).unwrap();
    std::fs::create_dir_all(&docs).unwrap();
    let bash = bin.join("bash");
    std::fs::write(
        &bash,
        r#"#!/bin/sh
[ "$#" -eq 2 ] && [ "$1" = "-lc" ] || exit 64
case "$2" in
  'scripts/check-rust-runtime-no-python.sh'|'scripts/check-runtime-placeholders.sh'|\
  'cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture'|\
  'cargo test -p hermes-mcp stale_transport_marker_detection_matches_known_variants -- --nocapture')
    printf '%s' 'fixture probe passed'
    ;;
  *) exit 65 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&bash, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        docs.join("global-parity-proof.json"),
        r#"{"release_gate":{"pass":true}}"#,
    )
    .unwrap();

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/memory/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "degraded": false, "result_state": "fixture",
        })))
        .expect(3)
        .mount(&server)
        .await;

    for (pending, strict, expected_exit) in [(1, false, 0), (1, true, 1), (0, true, 0)] {
        std::fs::write(
            docs.join("shared-diff-backlog.json"),
            format!(r#"{{"summary":{{"pending_classification":0,"pending_review":{pending}}}}}"#),
        )
        .unwrap();
        let mut command = isolated_hermes(root.path());
        command
            .env("PATH", &bin)
            .env("CONTEXTLATTICE_ORCHESTRATOR_URL", server.uri())
            .args(["elite-check", "--json"]);
        if strict {
            command.arg("--strict");
        }
        let output = tokio::time::timeout(Duration::from_secs(15), command.output())
            .await
            .expect("elite-check timed out")
            .unwrap();
        assert_eq!(output.status.code(), Some(expected_exit), "{output:?}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["ok"], pending == 0);
        assert_eq!(report["summary"]["total_sections"], 8);
        assert_eq!(report["summary"]["failed_sections"], pending);
        assert_eq!(report["sections"]["shared_backlog"]["ok"], pending == 0);
        for gate in ["runtime_python_guard", "hotpath", "mcp_stale_recovery"] {
            assert_eq!(
                report["sections"][gate]["stdout_tail"],
                "fixture probe passed"
            );
        }
        assert_eq!(report["sections"]["performance_autopilot"]["ok"], true);
        let report_path = std::path::Path::new(report["report_path"].as_str().unwrap())
            .canonicalize()
            .unwrap();
        assert!(report_path.starts_with(root.path().join(".sync-reports").canonicalize().unwrap()));
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
        assert_eq!(persisted, report);
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 6);
}

#[cfg(unix)]
#[tokio::test]
async fn offline_slo_violations_reach_the_exit_code_without_rolling_back_in_dry_run() {
    let root = tempfile::tempdir().unwrap();
    for (check, exit_code, violated) in [("exit 0", 0, false), ("exit 7", 1, true)] {
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            isolated_hermes(root.path())
                .args([
                    "slo-rollback",
                    "--check-cmd",
                    check,
                    "--rollback-cmd",
                    "echo ran > rollback-marker",
                    "--dry-run",
                    "--report-path",
                    "slo.json",
                    "--json",
                ])
                .output(),
        )
        .await
        .expect("slo-rollback timed out")
        .unwrap();
        assert_eq!(output.status.code(), Some(exit_code));
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["violated"], violated);
        assert_eq!(report["check"]["exit_code"], if violated { 7 } else { 0 });
        if violated {
            assert_eq!(report["rollback"]["reason"], "dry_run");
        }
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("slo.json")).unwrap()).unwrap();
        assert_eq!(report, on_disk);
        assert!(!root.path().join("rollback-marker").exists());
    }
}
