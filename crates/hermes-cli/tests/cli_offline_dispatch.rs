#[path = "support/isolated_hermes.rs"]
mod support;

use std::time::Duration;

use support::isolated_hermes;

#[tokio::test]
async fn offline_oneshot_app_and_chat_stdin_print_the_model_reply() {
    use serde_json::json;
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(json!({"model": "deterministic"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "deterministic",
            "choices": [{"message": {"role": "assistant", "content": "Under the willow bridge"}, "finish_reason": "stop"}]
        })))
        .expect(2)
        .mount(&server)
        .await;

    let sandbox = tempfile::tempdir().unwrap();
    write_query_provider_config(sandbox.path(), &server.uri(), &[]);

    let mut oneshot = isolated_hermes(sandbox.path());
    oneshot
        .env("HERMES_ONESHOT_APP_RUNTIME", "1")
        .env("HTTPS_PROXY", "http://127.0.0.1:0")
        .args(["--oneshot", "Where did the heron nest?"]);
    let output = tokio::time::timeout(Duration::from_secs(15), oneshot.output())
        .await
        .expect("one-shot app query timed out")
        .unwrap();
    assert!(output.status.success(), "one-shot app: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "Under the willow bridge"),
        "one-shot app: {output:?}"
    );

    let mut chat = isolated_hermes(sandbox.path());
    chat.env("HTTPS_PROXY", "http://127.0.0.1:0")
        .args(["chat", "--query", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = chat.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(b"Where did the heron nest?\n")
        .await
        .unwrap();
    drop(input);
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .expect("chat stdin query timed out")
        .unwrap();
    assert!(output.status.success(), "chat stdin: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "Under the willow bridge"),
        "chat stdin: {output:?}"
    );
}

fn write_query_provider_config(sandbox: &std::path::Path, base_url: &str, tools: &[&str]) {
    let home = sandbox.join("hermes");
    std::fs::create_dir(&home).unwrap();
    let mut config = serde_json::json!({
        "model": "local-test:deterministic",
        "tools": [],
        "streaming": {"enabled": false},
        "llm_providers": {"local-test": {
            "base_url": base_url,
            "api_key": "fixture-only",
            "discover_models": false,
            "models": {"deterministic": {}}
        }}
    });
    if !tools.is_empty() {
        config["platform_toolsets"] = serde_json::json!({"cli": tools});
    }
    std::fs::write(home.join("config.yaml"), config.to_string()).unwrap();
}

#[tokio::test]
async fn offline_chat_prints_tool_preview_and_completion() {
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let sandbox = tempfile::tempdir().unwrap();
    let filename = "fixture-notes.txt";
    let file_marker = "Willow bridge field notes";
    let contents = format!(
        "{file_marker}\n{}\n",
        "The heron returned before dawn. ".repeat(5)
    );
    std::fs::write(sandbox.path().join(filename), &contents).unwrap();

    let server = MockServer::start().await;
    write_query_provider_config(sandbox.path(), &server.uri(), &["read_file"]);

    let prompt = "Read the supplied field notes";
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(prompt))
        .and(body_string_contains(r#""name":"read_file""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "deterministic",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "fixture-read",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": json!({"path": filename}).to_string()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    let reply = "Notes read successfully.";
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(file_marker))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "deterministic",
            "choices": [{"message": {"role": "assistant", "content": reply}, "finish_reason": "stop"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut chat = isolated_hermes(sandbox.path());
    chat.env("HTTPS_PROXY", "http://127.0.0.1:0").args([
        "--allow-tools",
        "chat",
        "--query",
        prompt,
    ]);
    let output = tokio::time::timeout(Duration::from_secs(15), chat.output())
        .await
        .expect("query with a fixture tool timed out")
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<_> = stdout.lines().collect();
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("┊ 📖 read_file") && line.ends_with(filename)),
        "{stdout}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("┊ 📖 read_file")
            && line.contains("done:")
            && line.ends_with("...")),
        "{stdout}"
    );
    assert_eq!(lines.last(), Some(&reply), "{stdout}");
}

#[tokio::test]
async fn offline_model_completions_list_unique_providers_and_qualified_models() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    std::fs::create_dir(&home).unwrap();

    // Give every built-in provider an explicit catalogue to keep both
    // completion commands independent of credentials and live discovery.
    let mut providers = hermes_cli::model_switch::curated_provider_slugs();
    providers.push("fixture-local");
    let entries = providers
        .iter()
        .map(|provider| {
            (
                (*provider).to_string(),
                serde_json::json!({
                    "discover_models": false,
                    "models": ["fixture-model", "FIXTURE-MODEL", "second-model"]
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    std::fs::write(
        home.join("config.yaml"),
        serde_json::json!({"llm_providers": entries}).to_string(),
    )
    .unwrap();

    for (flag, expected) in [
        (
            "--completion-providers",
            providers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
        ),
        (
            "--completion-values",
            providers
                .iter()
                .flat_map(|provider| {
                    [
                        format!("{provider}:fixture-model"),
                        format!("{provider}:second-model"),
                    ]
                })
                .collect(),
        ),
    ] {
        let mut command = isolated_hermes(sandbox.path());
        command
            .args(["model", flag])
            .env("HTTPS_PROXY", "http://127.0.0.1:0");
        let output = tokio::time::timeout(Duration::from_secs(15), command.output())
            .await
            .expect("model completions timed out")
            .unwrap();
        assert!(output.status.success(), "{flag}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.lines().collect::<Vec<_>>(), expected, "{flag}");
    }
}

#[tokio::test]
async fn offline_insights_counts_recent_sessions_for_the_selected_source() {
    use serde_json::json;
    use std::fs::{self, FileTimes, OpenOptions};
    use std::time::SystemTime;

    let sandbox = tempfile::tempdir().unwrap();
    let mut command = isolated_hermes(sandbox.path());
    command.args(["insights", "--days", "7", "--source", "cli"]);
    let empty = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .expect("insights without sessions timed out")
        .unwrap();
    assert!(empty.status.success(), "{empty:?}");
    assert!(String::from_utf8_lossy(&empty.stdout).contains("No sessions directory found."));

    let sessions = sandbox.path().join("hermes/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let first = json!({
        "source": "cli",
        "model": "fixture-model",
        "messages": [{"role": "user", "content": "Record one sample"}],
        "usage": {"input_tokens": 6, "output_tokens": 3, "cost": 12.5}
    });
    let second = json!({
        "source": "cli",
        "model": "fixture-model",
        "messages": [
            {"role": "user", "content": "Read the sample"},
            {"role": "assistant", "content": "One sample"}
        ],
        "usage": {"output_tokens": 5}
    });
    let ignored = json!({
        "source": "telegram",
        "model": "ignored-model",
        "usage": {"input_tokens": 100, "output_tokens": 100}
    });
    for (name, session) in [
        ("first.json", &first),
        ("second.json", &second),
        (
            "unknown-source.json",
            &json!({"model": "unknown-source-model"}),
        ),
        ("other-source.json", &ignored),
        ("old.json", &first),
        ("notes.txt", &first),
    ] {
        fs::write(sessions.join(name), session.to_string()).unwrap();
    }
    let old = SystemTime::now() - Duration::from_secs(14 * 24 * 60 * 60);
    OpenOptions::new()
        .write(true)
        .open(sessions.join("old.json"))
        .unwrap()
        .set_times(FileTimes::new().set_modified(old))
        .unwrap();
    fs::write(sessions.join("malformed.json"), "{").unwrap();
    fs::create_dir(sessions.join("directory.json")).unwrap();

    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .expect("insights report timed out")
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "Usage Insights (last 7 days)",
        "Filter: source=cli",
        "Sessions:       2",
        "Messages:       3",
        "Input tokens:   6",
        "Output tokens:  8",
        "Total tokens:   14",
        "Estimated cost: $0.1250",
        "Averages per session:",
        "Messages: 1.5",
        "Tokens:   7",
    ] {
        assert!(
            text.lines().any(|line| line.trim_start() == expected),
            "missing {expected:?} in {text}"
        );
    }
    let model = text
        .lines()
        .find(|line| line.starts_with("  fixture-model"))
        .unwrap();
    assert!(model
        .split_whitespace()
        .eq(["fixture-model", "2", "session(s)"]));
    assert!(!text.contains("ignored-model"));
    assert!(!text.contains("unknown-source-model"));
}

#[tokio::test]
async fn offline_dump_exports_the_selected_snapshot_and_reports_output_errors() {
    use serde_json::{json, Value};
    use std::fs::{self, FileTimes, OpenOptions};
    use std::time::SystemTime;

    let sandbox = tempfile::tempdir().unwrap();
    let sessions = sandbox.path().join("hermes/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let requested = sessions.join("fixture.json");
    fs::write(
        &requested,
        json!({
            "session_info": {
                "session_id": "fixture-session",
                "model": "fixture:local"
            },
            "messages": [
                {"role": "System", "content": "Keep the sample local"},
                {"role": "User", "content": "Summarise the sample"}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let earlier = SystemTime::now() - Duration::from_secs(60);
    OpenOptions::new()
        .write(true)
        .open(requested)
        .unwrap()
        .set_times(FileTimes::new().set_modified(earlier))
        .unwrap();
    fs::write(
        sessions.join("newer.json"),
        json!({
            "session_info": {"session_id": "newer-session"},
            "messages": [{"role": "User", "content": "A different sample"}]
        })
        .to_string(),
    )
    .unwrap();

    let saved = sandbox.path().join("exports/session.json");
    let mut command = isolated_hermes(sandbox.path());
    command.args(["dump", "fixture"]).arg(&saved);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .expect("session dump timed out")
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Wrote dump to "));
    let contents = fs::read(&saved).unwrap();
    let export: Value = serde_json::from_slice(&contents).unwrap();
    assert_eq!(export["session_id"], "fixture-session");
    assert_eq!(export["resolved_id"], "fixture");
    assert_eq!(export["model"], "fixture:local");
    assert_eq!(export["system_prompt"], "Keep the sample local");
    assert_eq!(export["messages"][1]["content"], "Summarise the sample");

    for (target, diagnostic) in [
        (
            saved.join("blocked.json"),
            "Failed to create dump output directory",
        ),
        (sandbox.path().join("exports"), "Failed to write dump"),
    ] {
        let mut command = isolated_hermes(sandbox.path());
        command.args(["dump", "fixture"]).arg(target);
        let output = tokio::time::timeout(Duration::from_secs(15), command.output())
            .await
            .expect("session dump failure timed out")
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{stderr}");
    }
    assert_eq!(fs::read(saved).unwrap(), contents);
}

#[tokio::test]
async fn offline_systems_replay_reports_damaged_traces_and_exports_json() {
    let sandbox = tempfile::tempdir().unwrap();
    let replay = sandbox.path().join("hermes/logs/replay");
    std::fs::create_dir_all(&replay).unwrap();
    std::fs::write(
        replay.join("damaged.jsonl"),
        concat!(
            "{\"seq\":1,\"event_hash\":\"first\"}\n",
            "incomplete fixture record\n",
            "{\"seq\":3,\"prev_hash\":\"missing\",\"event_hash\":\"third\"}\n",
        ),
    )
    .unwrap();
    std::fs::write(replay.join("notes.txt"), "Local fixture notes").unwrap();

    let mut command = isolated_hermes(sandbox.path());
    command.args(["systems", "replay", "--output", "reports/replay.json"]);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .expect("systems replay timed out")
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Replay trace logs: 1 file(s)"), "{text}");
    assert!(
        text.lines()
            .any(|line| { line.contains("damaged.jsonl entries=2 parse_errors=1 chain_breaks=1") }),
        "{text}"
    );

    let report_path = sandbox.path().join("reports/replay.json");
    let report_bytes = std::fs::read(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
    assert_eq!(report["kind"], "hermes.replay.report");
    assert_eq!(report["passed"], false);
    assert_eq!(report["log_count"], 1);
    let logs = report["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 1);
    let log = &logs[0];
    assert!(std::path::Path::new(log["path"].as_str().unwrap()).ends_with("damaged.jsonl"));
    assert_eq!(log["entries"], 2);
    assert_eq!(log["parse_errors"], 1);
    assert_eq!(log["chain_breaks"], 1);
    assert_eq!(log["first_seq"], 1);
    assert_eq!(log["last_seq"], 3);

    let mut blocked = isolated_hermes(sandbox.path());
    blocked.args([
        "systems",
        "replay",
        "--output",
        "reports/replay.json/nested.json",
    ]);
    let failure = tokio::time::timeout(Duration::from_secs(15), blocked.output())
        .await
        .expect("systems replay with blocked output timed out")
        .unwrap();
    assert_eq!(failure.status.code(), Some(1), "{failure:?}");
    let error = String::from_utf8(failure.stderr).unwrap();
    assert!(
        error
            .replace('\\', "/")
            .contains("mkdir reports/replay.json"),
        "{error}"
    );
    assert_eq!(std::fs::read(report_path).unwrap(), report_bytes);
}

#[tokio::test]
async fn offline_systems_rejects_unknown_topics_without_creating_reports() {
    let sandbox = tempfile::tempdir().unwrap();
    for (action, label) in [
        ("agent-card", "agent-card"),
        ("mcp", "MCP"),
        ("acp", "ACP"),
        ("handoff", "handoff"),
        ("provenance", "provenance"),
    ] {
        let mut command = isolated_hermes(sandbox.path());
        command.args([
            "systems",
            action,
            "fixture-unknown",
            "--json",
            "--output",
            "reports/result.json",
        ]);
        let result = tokio::time::timeout(Duration::from_secs(15), command.output())
            .await
            .expect("systems validation timed out")
            .unwrap();
        assert_eq!(result.status.code(), Some(1), "{action}: {result:?}");
        assert!(result.stdout.is_empty(), "{action}: {result:?}");
        let stderr = String::from_utf8(result.stderr).unwrap();
        assert!(
            stderr.contains(&format!("unknown {label} action 'fixture-unknown'")),
            "{stderr}"
        );
        assert!(!sandbox.path().join("reports").exists());
    }
}

#[tokio::test]
async fn offline_pairing_approves_masks_and_revokes_a_disposable_device() {
    use hermes_cli::pairing_store::{PairedDevice, PairingStatus, PairingStore};

    let root = tempfile::tempdir().unwrap();
    let store = PairingStore::new(root.path().join("hermes/pairing.json"));
    let run_pairing = |args: &[&str]| {
        let mut command = isolated_hermes(root.path());
        command.arg("pairing").args(args);
        async move {
            tokio::time::timeout(Duration::from_secs(15), command.output())
                .await
                .expect("pairing command timed out")
                .expect("pairing command should run")
        }
    };

    let empty = run_pairing(&[]).await;
    assert!(empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stdout).contains("No paired devices."));

    let devices = ["fixture-device", "fixture-expired"].map(|id| PairedDevice {
        device_id: id.to_string(),
        name: None,
        status: PairingStatus::Pending,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        last_seen: None,
        shared_secret: None,
    });
    store.save(&devices).unwrap();

    let missing = run_pairing(&["approve"]).await;
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("Missing --device-id"));
    assert_eq!(store.list().unwrap()[0].status, PairingStatus::Pending);

    let approved = run_pairing(&["approve", "--device-id", "fixture-device"]).await;
    assert!(approved.status.success());
    let saved = store.list().unwrap();
    assert_eq!(saved[0].device_id, "fixture-device");
    assert_eq!(saved[0].status, PairingStatus::Approved);
    let secret = saved[0].shared_secret.as_deref().unwrap();
    let stdout = String::from_utf8_lossy(&approved.stdout);
    let masked = hermes_core::secret_values::mask_secret(secret);
    assert!(stdout.contains("Device 'fixture-device' approved."));
    assert!(stdout
        .lines()
        .any(|line| line.trim() == format!("Shared secret: {masked}")));
    assert!(!stdout.contains(secret));
    assert!(!String::from_utf8_lossy(&approved.stderr).contains(secret));

    let cleared = run_pairing(&["clear-pending"]).await;
    assert!(cleared.status.success());
    assert!(String::from_utf8_lossy(&cleared.stdout).contains("Cleared 1 pending pairing request"));
    let saved = store.list().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].device_id, "fixture-device");
    assert_eq!(saved[0].status, PairingStatus::Approved);

    let revoked = run_pairing(&["revoke", "--device-id", "fixture-device"]).await;
    assert!(revoked.status.success());
    assert!(String::from_utf8_lossy(&revoked.stdout).contains("Device 'fixture-device' revoked."));
    let saved = store.list().unwrap();
    assert_eq!(saved[0].status, PairingStatus::Revoked);
    assert!(saved[0].shared_secret.is_none());

    let list = run_pairing(&["list"]).await;
    assert!(list.status.success());
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout
        .lines()
        .any(|line| line.contains("fixture-device") && line.contains("revoked")));
    assert!(!stdout.contains("fixture-expired"));
}

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
