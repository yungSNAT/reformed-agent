#[test]
fn bundle_keeps_manifest_when_signing_environment_key_is_too_short() {
    let Some(cli) = isolated_cli(
        "bundle_keeps_manifest_when_signing_environment_key_is_too_short",
        &[
            ("HERMES_PROVENANCE_SIGNING_KEY", "aa"),
            ("HERMES_DETERMINISTIC_ARTIFACTS", "false"),
        ],
    ) else {
        return;
    };
    let key = provenance_key_path_for_cli(&cli);
    write_fixture(&key, TEST_KEY);
    assert!(matches!(
        load_or_create_provenance_key(&cli, true),
        Err(AgentError::Config(message))
            if message == "HERMES_PROVENANCE_SIGNING_KEY must be at least 16 bytes"
    ));

    let snapshot = write_doctor_snapshot(&cli, &json!({"checks": []}), None).unwrap();
    let output = hermes_state_root(&cli).join("bundles/unsigned.tar.gz");
    assert_eq!(
        build_doctor_support_bundle_with_options(&cli, &snapshot, Some(&output), true).unwrap(),
        output
    );
    let entries = bundle_entries(&output);
    assert!(!entries.contains_key("doctor/replay/manifest.sig.json"));
    let manifest: Value = serde_json::from_slice(&entries["doctor/replay/manifest.json"]).unwrap();
    let timestamp = manifest["generated_at"].as_str().unwrap();
    assert!(timestamp.parse::<jiff::Timestamp>().is_ok());
    assert_ne!(timestamp, "1970-01-01T00:00:00Z");
    assert_eq!(manifest["totals"]["files"], 0);
    assert_eq!(fs::read_to_string(key).unwrap(), TEST_KEY);
}

#[cfg(unix)]
#[test]
fn self_heal_fixes_vault_mode_and_leaves_active_or_unreadable_pid_alone() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let Some(cli) = isolated_cli(
        "self_heal_fixes_vault_mode_and_leaves_active_or_unreadable_pid_alone",
        &[],
    ) else {
        return;
    };
    let root = hermes_state_root(&cli);
    fs::create_dir_all(&root).unwrap();
    let blocked = root.join("profiles");
    symlink(root.join("missing/profiles"), &blocked).unwrap();

    let vault = secret_vault_path_for_cli(&cli);
    write_fixture(&vault, b"fixture vault");
    fs::set_permissions(&vault, fs::Permissions::from_mode(0o644)).unwrap();
    let pid_path = gateway_pid_path_for_cli(&cli);
    let pid = std::process::id().to_string();
    write_fixture(&pid_path, &pid);

    let actions = run_doctor_self_heal(&cli);
    assert!(actions.iter().any(|action| {
        action["ok"] == false
            && action["status"] == "error"
            && action["detail"].as_str().is_some_and(|detail| {
                detail.starts_with(&format!("directory {}:", blocked.display()))
            })
    }));
    assert!(actions.iter().any(|action| {
        action["ok"] == true
            && action["status"] == "fixed"
            && action["detail"] == format!("normalized permissions on {} to 600", vault.display())
    }));
    assert_eq!(
        fs::metadata(&vault).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(actions.iter().any(|action| {
        action["ok"] == true
            && action["status"] == "noop"
            && action["detail"] == format!("gateway pid {pid} is active")
    }));
    assert_eq!(fs::read_to_string(&pid_path).unwrap(), pid);

    fs::write(&pid_path, "not a pid\n").unwrap();
    let actions = run_doctor_self_heal(&cli);
    for detail in [
        format!(
            "pid file {} is unreadable; left unchanged",
            pid_path.display()
        ),
        format!("permissions already secure on {}", vault.display()),
    ] {
        assert!(actions.iter().any(|action| {
            action["ok"] == true && action["status"] == "noop" && action["detail"] == detail
        }));
    }
    assert_eq!(fs::read_to_string(pid_path).unwrap(), "not a pid\n");
    assert_eq!(fs::read(vault).unwrap(), b"fixture vault");
}

#[test]
fn self_heal_reports_database_open_and_schema_errors() {
    let Some(cli) = isolated_cli("self_heal_reports_database_open_and_schema_errors", &[]) else {
        return;
    };
    let root = hermes_state_root(&cli);
    let db_paths = [root.join("sessions.db"), root.join("sessions/sessions.db")];
    for path in &db_paths {
        fs::create_dir_all(path).unwrap();
    }
    let actions = run_doctor_self_heal(&cli);
    assert!(actions.iter().any(|action| {
        action["ok"] == false
            && action["status"] == "warn"
            && action["detail"]
                .as_str()
                .is_some_and(|detail| detail.starts_with("sessions.db does not open cleanly: "))
    }));

    for path in &db_paths {
        fs::remove_dir(path).unwrap();
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE coverage_bad (id INTEGER);
             PRAGMA writable_schema = ON;
             UPDATE sqlite_master SET sql = 'CREATE TABL coverage_bad (id INTEGER)'
               WHERE name = 'coverage_bad';
             PRAGMA writable_schema = OFF;",
        )
        .unwrap();
    }
    let actions = run_doctor_self_heal(&cli);
    let repair = actions
        .iter()
        .find(|action| {
            action["detail"].as_str().is_some_and(|detail| {
                detail.starts_with("repaired malformed sessions.db schema using ")
                    || detail.starts_with("sessions.db schema repair failed: ")
            })
        })
        .expect("malformed SQLite schema should produce a repair action");
    let detail = repair["detail"].as_str().unwrap();
    if detail.starts_with("repaired ") {
        assert_eq!(repair["status"], "fixed");
        assert_eq!(repair["ok"], true);
        assert!(detail.contains("(backup: "));
        assert!(SessionPersistence::new(&root).db_health_error().is_none());
    } else {
        assert_eq!(repair["status"], "error");
        assert_eq!(repair["ok"], false);
        assert!(!detail.trim_end().ends_with("failed:"));
    }
}

#[cfg(unix)]
fn install_shell_fixture(name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::current_dir().unwrap().join("bin").join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn elite_override_forwards_json_and_only_rejects_failure_in_strict_mode() {
    let Some(cli) = isolated_cli(
        "elite_override_forwards_json_and_only_rejects_failure_in_strict_mode",
        &[("HERMES_ELITE_GATE_CMD", "fixture-gate")],
    ) else {
        return;
    };
    assert!(matches!(
        run_legacy_elite_check("fixture-gate".into(), false, true).await,
        Err(AgentError::Io(message)) if message.starts_with("elite-check command failed to start:")
    ));
    install_shell_fixture(
        "bash",
        r#"
printf '%s\n%s\n' "$1" "$2" > gate-invocation
printf 'fixture gate stdout\n'
printf 'fixture gate stderr\n' >&2
test "$#" -eq 2 && test "$1" = '-lc' && test "$2" = 'fixture-gate --json'
"#,
    );
    let home = PathBuf::from(std::env::var_os("HERMES_HOME").unwrap());
    run_elite_check(doctor_cli(&home), true, true)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string("gate-invocation").unwrap(),
        "-lc\nfixture-gate --json\n"
    );
    assert!(matches!(
        run_elite_check(doctor_cli(&home), false, true).await,
        Err(AgentError::Config(message)) if message.starts_with("elite-check failed (status=")
    ));
    run_elite_check(cli, false, false).await.unwrap();
    assert_eq!(
        fs::read_to_string("gate-invocation").unwrap(),
        "-lc\nfixture-gate\n"
    );
}

#[cfg(unix)]
fn snapshot_check<'a>(snapshot: &'a Value, name: &str) -> &'a Value {
    snapshot["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("missing snapshot check {name}"))
}

#[cfg(unix)]
fn assert_probes_cannot_connect(cli: &Cli) {
    let config = load_config(cli.config_dir.as_deref()).ok();
    for spec in hermes_provider_runtime::local_backend_specs() {
        if let Some(base) =
            hermes_provider_runtime::local_backend_resolved_base_url(spec.provider, config.as_ref())
        {
            let url = format!("{}/models", base.trim_end_matches('/'));
            assert!(
                reqwest::Url::parse(&url).is_err(),
                "fixture must prevent a real request to {url}"
            );
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn deep_doctor_reports_gateway_health_pid_and_replay_integrity() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let Some(cli) = isolated_cli(
        "deep_doctor_reports_gateway_health_pid_and_replay_integrity",
        &[],
    ) else {
        // The parent has already run this scenario and checked the child's exit.
        return;
    };
    #[cfg(target_os = "macos")]
    {
        let plist = gateway_launchd_plist_path().expect("fixture launch agent path");
        assert!(plist.starts_with(std::env::current_dir().unwrap()));
        assert!(!plist.exists());
    }
    let server = MockServer::start().await;
    for (route, status) in [("/healthy", 200), ("/unavailable", 503)] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
    }

    let mut checks = Vec::new();
    let endpoint = format!("{}/unavailable", server.uri());
    append_deep_doctor_checks(&cli, &mut checks, &endpoint)
        .await
        .unwrap();
    let snapshot = json!({"checks": checks});
    assert_eq!(
        snapshot_check(&snapshot, "gateway_service_status")["ok"],
        true
    );
    let missing_pid = snapshot_check(&snapshot, "gateway_pid");
    assert_eq!(missing_pid["ok"], false);
    assert!(missing_pid["pid"].is_null());
    assert_eq!(
        snapshot_check(&snapshot, "contextlattice_health")["ok"],
        false
    );
    let empty = snapshot_check(&snapshot, "replay_traces");
    assert_eq!(empty["count"], 0);
    assert_eq!(empty["summaries"], json!([]));

    let pid = std::process::id();
    let pid_path = gateway_pid_path_for_cli(&cli);
    install_shell_fixture(
        "ps",
        &format!(
            r#"test "$*" = '-p {pid} -o command=' || exit 1
printf '%s\n' 'hermes-agent-ultra gateway'"#
        ),
    );
    write_fixture(&pid_path, pid.to_string());
    let replay = hermes_state_root(&cli).join("logs/replay");
    fs::create_dir_all(replay.join("ignore-directory")).unwrap();
    write_fixture(
        &replay.join("a.jsonl"),
        b"{\"seq\":1,\"event_hash\":\"first\"}\n",
    );
    write_fixture(&replay.join("b.jsonl"), b"invalid replay line\n");

    let mut checks = Vec::new();
    let endpoint = format!("{}/healthy", server.uri());
    append_deep_doctor_checks(&cli, &mut checks, &endpoint)
        .await
        .unwrap();
    let snapshot = json!({"checks": checks});
    let active_pid = snapshot_check(&snapshot, "gateway_pid");
    assert_eq!(active_pid["ok"], true);
    assert_eq!(active_pid["pid"], pid);
    assert_eq!(active_pid["pid_path"], pid_path.display().to_string());
    let health = snapshot_check(&snapshot, "contextlattice_health");
    assert_eq!(health["ok"], true);
    assert_eq!(health["url"], endpoint);
    let replay_check = snapshot_check(&snapshot, "replay_traces");
    assert_eq!(replay_check["ok"], true);
    assert_eq!(replay_check["count"], 2);
    assert_eq!(replay_check["chain_ok"], false);
    assert_eq!(replay_check["summaries"][0]["file"], "b.jsonl");
    assert_eq!(replay_check["summaries"][0]["invalid_lines"], 1);
    assert_eq!(replay_check["summaries"][1]["file"], "a.jsonl");
    assert_eq!(replay_check["summaries"][1]["last_event_hash"], "first");
}

#[cfg(unix)]
#[tokio::test]
async fn shallow_doctor_snapshots_check_results_and_can_write_a_bundle() {
    let mut vars = vec![
        ("HERMES_ELITE_GATE_CMD", "fixture-gate"),
        ("HERMES_OPENAI_API_KEY", "fixture"),
    ];
    for spec in hermes_provider_runtime::local_backend_specs() {
        vars.push((spec.base_url_env_var, "http://["));
    }
    let Some(cli) = isolated_cli(
        "shallow_doctor_snapshots_check_results_and_can_write_a_bundle",
        &vars,
    ) else {
        return;
    };
    install_shell_fixture("which", r#"test "$#" -eq 1 && test "$1" = git"#);

    let home = PathBuf::from(std::env::var_os("HERMES_HOME").unwrap());
    let cwd = std::env::current_dir().unwrap();
    write_fixture(&home.join("config.yaml"), "{}\n");
    fs::write(cwd.join(".env"), "ANTHROPIC_API_KEY=project-fixture\n").unwrap();

    let first_path = cwd.join("first.json");
    let first_sidecar = provenance_sidecar_path_for_artifact(&first_path);
    write_fixture(&first_sidecar.join("sentinel"), b"keep");
    assert_probes_cannot_connect(&cli);
    run_doctor(
        cli,
        false,
        false,
        true,
        Some(first_path.to_str().unwrap().to_owned()),
        false,
    )
    .await
    .unwrap();

    let first = read_json(&first_path);
    assert_eq!(first["deep"], false);
    assert_eq!(first["self_heal"], false);
    assert_eq!(first["self_heal_actions"], json!([]));
    assert_eq!(first["config_summary"]["loaded"], true);
    assert_eq!(snapshot_check(&first, "config_dir")["ok"], true);
    assert_eq!(snapshot_check(&first, "config_yaml")["ok"], true);
    assert_eq!(snapshot_check(&first, "config_load")["ok"], true);
    let env_file = snapshot_check(&first, "env_file");
    assert_eq!(env_file["ok"], true);
    assert_eq!(env_file["fallback"], cwd.join(".env").display().to_string());
    assert_eq!(snapshot_check(&first, "soul_md")["ok"], false);
    for (name, ok) in [
        ("api_key_HERMES_OPENAI_API_KEY", true),
        ("api_key_ANTHROPIC_API_KEY", true),
        ("api_key_OPENAI_API_KEY", false),
        ("api_key_OPENROUTER_API_KEY", false),
        ("bin_git", true),
        ("bin_ssh", false),
        ("bin_node", true),
    ] {
        assert_eq!(snapshot_check(&first, name)["ok"], ok, "{name}");
    }
    assert_eq!(snapshot_check(&first, "bin_node")["optional"], true);
    for spec in hermes_provider_runtime::local_backend_specs() {
        let backend = snapshot_check(&first, &format!("local_backend_{}", spec.provider));
        assert_eq!(backend["optional"], true);
        assert_eq!(backend["reachable"], false);
    }
    assert_eq!(first["elite"]["elite_gate"]["runner"], "shell_override");
    assert_eq!(first["elite"]["elite_gate"]["command"], "fixture-gate");
    assert_eq!(first["elite"]["elite_gate"]["script_available"], false);
    assert_eq!(first["elite"]["provenance"]["exists"], false);
    assert_eq!(fs::read(first_sidecar.join("sentinel")).unwrap(), b"keep");

    fs::write(
        home.join("config.yaml"),
        "model: fixture-model\nmax_turns: 23\n",
    )
    .unwrap();
    fs::write(home.join(".env"), "OPENAI_API_KEY=home-fixture\n").unwrap();
    fs::write(home.join("SOUL.md"), "Fixture persona\n").unwrap();
    install_shell_fixture("fixture-gate", "exit 0");
    let cli = doctor_cli(&home);
    let health = route_health_state_path_for_cli(&cli);
    write_fixture(&health, br#"{"summary":{"overall":"fixture-healthy"}}"#);

    // The snapshot describes the effective configuration, including any
    // defaults applied by the loader, rather than echoing the YAML.
    let config = load_config(cli.config_dir.as_deref()).unwrap();
    let expected_summary = json!({
        "loaded": true,
        "model": config.model,
        "max_turns": config.max_turns,
        "enabled_platforms": config.platforms.values().filter(|platform| platform.enabled).count(),
    });

    let second_path = cwd.join("second.json");
    let reports = debug_reports_dir_for_cli(&cli);
    assert_probes_cannot_connect(&cli);
    run_doctor(
        cli,
        false,
        true,
        false,
        Some(second_path.to_str().unwrap().to_owned()),
        true,
    )
    .await
    .unwrap();

    let second = read_json(&second_path);
    assert_eq!(second["self_heal"], true);
    let self_heal = snapshot_check(&second, "self_heal");
    assert_eq!(self_heal["ok"], true);
    assert_eq!(self_heal["actions"], second["self_heal_actions"]);
    assert!(!second["self_heal_actions"].as_array().unwrap().is_empty());
    assert_eq!(snapshot_check(&second, "config_load")["ok"], true);
    assert_eq!(snapshot_check(&second, "soul_md")["ok"], true);
    assert_eq!(
        snapshot_check(&second, "api_key_OPENAI_API_KEY")["ok"],
        true
    );
    assert_eq!(second["config_summary"], expected_summary);
    assert_eq!(second["elite"]["elite_gate"]["script_available"], true);
    assert_eq!(
        second["elite"]["route_health"]["summary"]["overall"],
        "fixture-healthy"
    );
    assert_eq!(second["elite"]["provenance"]["exists"], true);

    let elite_check = snapshot_check(&second, "elite_diagnostics");
    assert_eq!(elite_check["ok"], true);
    let details = &elite_check["details"];
    let elite = &second["elite"];
    for section in ["elite_gate", "provenance", "route_health", "tool_policy"] {
        assert_eq!(details[section], elite[section], "{section}");
    }
    // The two diagnostic reads can synthesize different saved-at timestamps
    // when no route-learning state has been saved yet.
    for field in ["path", "entries", "ttl_secs", "half_life_secs"] {
        assert_eq!(
            details["route_learning"][field], elite["route_learning"][field],
            "route_learning.{field}"
        );
    }

    assert!(
        verify_artifact_provenance(&doctor_cli(&home), &second_path, None)
            .unwrap()
            .ok
    );
    let bundle = fs::read_dir(reports)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("support-bundle-")
        })
        .expect("bundle option should create a support bundle");
    assert_eq!(
        bundle_entries(&bundle)["doctor/snapshot.json"],
        fs::read(second_path).unwrap()
    );
}

fn write_executable_fixture(path: &Path, bytes: impl AsRef<[u8]>) {
    write_fixture(path, bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn elite_gate_override_availability_handles_quotes_env_prefixes_and_pathext() {
    let Some(cli) = isolated_cli(
        "elite_gate_override_availability_handles_quotes_env_prefixes_and_pathext",
        &[
            (
                "HERMES_ELITE_GATE_CMD",
                "FOO_1=bar BAR='with space' cargo run --locked -q -p hermes-parity-tools -- run-elite-sync-gate",
            ),
            ("PATHEXT", ".com;.exe;.bat;.cmd"),
        ],
    ) else {
        return;
    };
    let cwd = std::env::current_dir().unwrap();
    let bin = cwd.join("bin");

    // Malformed shell quoting and env-assignment-only commands are unavailable.
    assert!(!elite_gate_override_available("\"unterminated"));
    assert!(!elite_gate_override_available("FOO=1 BAR=2"));

    // Quoted executable path with spaces and `=` in a path segment (not an env assignment).
    let spaced = cwd.join("my tools/v=1/custom gate");
    let quoted_spaced = shlex::try_quote(spaced.to_str().unwrap()).unwrap();
    let spaced_cmd = format!("FOO=1 {quoted_spaced} --json");
    assert!(!elite_gate_override_available(&spaced_cmd));
    #[cfg(unix)]
    {
        write_fixture(&spaced, b"#!/bin/sh\nexit 0\n");
        assert!(!elite_gate_override_available(&spaced_cmd));
    }
    write_executable_fixture(&spaced, b"#!/bin/sh\nexit 0\n");
    assert!(elite_gate_override_available(&spaced_cmd));

    // Explicit path without extension resolves via PATHEXT.
    let explicit_stem = cwd.join("gates/win-gate");
    let quoted_stem = shlex::try_quote(explicit_stem.to_str().unwrap()).unwrap();
    let explicit_cmd = format!("{quoted_stem} --json");
    assert!(!elite_gate_override_available(&explicit_cmd));
    write_executable_fixture(&cwd.join("gates/win-gate.cmd"), b"@echo off\r\n");
    assert!(elite_gate_override_available(&explicit_cmd));

    // Bare command with leading env assignments resolves via PATH + PATHEXT.
    let missing_cargo = build_elite_doctor_diagnostics(&cli);
    assert_eq!(missing_cargo["elite_gate"]["runner"], "shell_override");
    assert_eq!(missing_cargo["elite_gate"]["script_available"], false);

    write_executable_fixture(&bin.join("cargo.exe"), b"#!/bin/sh\nexit 0\n");
    let with_cargo_exe = build_elite_doctor_diagnostics(&cli);
    assert_eq!(with_cargo_exe["elite_gate"]["script_available"], true);
}

#[test]
fn elite_gate_override_availability_without_pathext_env_uses_platform_default() {
    let Some(_cli) = isolated_cli(
        "elite_gate_override_availability_without_pathext_env_uses_platform_default",
        &[],
    ) else {
        return;
    };
    let bin = std::env::current_dir().unwrap().join("bin");
    write_executable_fixture(&bin.join("cargo.exe"), b"#!/bin/sh\nexit 0\n");
    let cmd = "cargo run --locked -q -p hermes-parity-tools -- run-elite-sync-gate";
    assert_eq!(elite_gate_override_available(cmd), cfg!(windows));
    assert!(elite_gate_override_available(&format!("PATHEXT=.exe {cmd}")));
}

#[test]
fn elite_gate_override_availability_honours_leading_path_and_pathext_assignments() {
    let Some(_cli) = isolated_cli(
        "elite_gate_override_availability_honours_leading_path_and_pathext_assignments",
        &[("PATHEXT", ".com;.exe;.bat;.cmd")],
    ) else {
        return;
    };
    let cwd = std::env::current_dir().unwrap();
    let orig_bin = cwd.join("bin");
    let alt_bin = cwd.join("alt bin");
    let empty_bin = cwd.join("empty bin");
    fs::create_dir_all(&alt_bin).unwrap();
    fs::create_dir_all(&empty_bin).unwrap();

    write_executable_fixture(&orig_bin.join("orig-only"), b"#!/bin/sh\nexit 0\n");
    write_executable_fixture(&alt_bin.join("alt-only"), b"#!/bin/sh\nexit 0\n");
    write_executable_fixture(&alt_bin.join("win-only.cmd"), b"@echo off\r\n");

    let q_alt = shlex::try_quote(alt_bin.to_str().unwrap()).unwrap();
    let q_empty = shlex::try_quote(empty_bin.to_str().unwrap()).unwrap();

    // Without PATH override: orig-only is found, alt-only is not.
    assert!(elite_gate_override_available("orig-only --json"));
    assert!(!elite_gate_override_available("alt-only --json"));

    // Leading PATH override: alt-only resolves, orig-only no longer resolves, last PATH wins.
    assert!(elite_gate_override_available(&format!(
        "PATH={q_empty} PATH={q_alt} alt-only --json"
    )));
    assert!(!elite_gate_override_available(&format!(
        "PATH={q_alt} orig-only --json"
    )));
    assert!(!elite_gate_override_available("PATH= orig-only --json"));

    // Leading PATHEXT override: overrides or clears process PATHEXT; last PATHEXT wins.
    assert!(!elite_gate_override_available(&format!(
        "PATH={q_alt} PATHEXT=.exe win-only --json"
    )));
    assert!(elite_gate_override_available(&format!(
        "PATH={q_alt} PATHEXT=.exe PATHEXT=.cmd win-only --json"
    )));
    assert!(!elite_gate_override_available(&format!(
        "PATH={q_alt} PATHEXT= win-only --json"
    )));
}
