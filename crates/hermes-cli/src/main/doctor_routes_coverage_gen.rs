use super::*;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, process::Command};

const COVERAGE_CHILD: &str = "HERMES_DOCTOR_COVERAGE_CHILD";
const TEST_KEY: &str = "fixture signing key!";

fn doctor_cli(home: &Path) -> Cli {
    <Cli as clap::Parser>::try_parse_from([
        "hermes",
        "--config-dir",
        home.to_str().unwrap(),
        "doctor",
    ])
    .unwrap()
}

// Give each scenario its own environment and cwd without changing either in
// the test runner, where other CLI tests may be running concurrently.
fn isolated_cli(test: &str, vars: &[(&str, &str)]) -> Option<Cli> {
    if std::env::var(COVERAGE_CHILD).as_deref() == Ok(test) {
        let home = PathBuf::from(std::env::var_os("HERMES_HOME").unwrap());
        let cli = doctor_cli(&home);
        assert!(hermes_state_root(&cli).starts_with(home.parent().unwrap()));
        return Some(cli);
    }

    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path().canonicalize().unwrap();
    let user_home = root.join("user");
    let state_home = root.join("state");
    let bin = root.join("bin");
    fs::create_dir(&user_home).unwrap();
    fs::create_dir(&bin).unwrap();

    let (_, module) = module_path!().split_once("::").unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &format!("{module}::{test}"), "--nocapture"])
        .current_dir(&root)
        .env_clear()
        .env(COVERAGE_CHILD, test)
        .env("HERMES_HOME", &state_home)
        .env("HOME", &user_home)
        .env("USERPROFILE", &user_home)
        .env("APPDATA", &user_home)
        .env("LOCALAPPDATA", &user_home)
        .env("XDG_CONFIG_HOME", &user_home)
        .env("XDG_DATA_HOME", &user_home)
        .env("XDG_STATE_HOME", &user_home)
        .env("PATH", &bin)
        .env("TMPDIR", &root)
        .env("TMP", &root)
        .env("TEMP", &root);
    for key in ["SystemRoot", "WINDIR", "LLVM_PROFILE_FILE"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    for (key, value) in vars {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("running 1 test"),
        "{test}: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    None
}

#[cfg(unix)]
fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_fixture(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn provenance_material_accepts_hex_base64_and_literal_keys() {
    assert!(matches!(
        parse_provenance_key_material(" \t\n"),
        Err(AgentError::Config(message)) if message.contains("empty provenance key material")
    ));
    assert_eq!(parse_provenance_key_material(" 00fF\n").unwrap(), [0, 255]);
    assert_eq!(
        parse_provenance_key_material(" aGVsbG8= ").unwrap(),
        b"hello"
    );
    assert_eq!(
        parse_provenance_key_material(" fixture signing key! \n").unwrap(),
        TEST_KEY.as_bytes()
    );
}

#[test]
fn provenance_distinguishes_missing_artifact_bad_signature_and_missing_key() {
    let Some(cli) = isolated_cli(
        "provenance_distinguishes_missing_artifact_bad_signature_and_missing_key",
        &[],
    ) else {
        return;
    };
    let root = hermes_state_root(&cli);
    let artifact = root.join("artifact.json");
    let missing = verify_artifact_provenance(&cli, &artifact, None).unwrap();
    assert!(!missing.ok);
    assert_eq!(missing.code, "artifact_read_error");
    assert_eq!(missing.key_id, None);
    assert_eq!(missing.artifact_sha256, None);
    assert!(missing
        .reason
        .unwrap()
        .contains(artifact.to_string_lossy().as_ref()));

    write_fixture(&artifact, b"fixture");
    let sidecar = provenance_sidecar_path_for_artifact(&artifact);
    fs::write(&sidecar, "{not-json").unwrap();
    let malformed = verify_artifact_provenance(&cli, &artifact, None).unwrap();
    assert!(!malformed.ok);
    assert_eq!(malformed.code, "signature_parse_error");
    assert_eq!(malformed.key_id, None);
    assert_eq!(malformed.artifact_sha256, None);
    assert!(malformed
        .reason
        .unwrap()
        .contains(sidecar.to_string_lossy().as_ref()));

    let sig = ProvenanceSignature {
        generated_at: "1970-01-01T00:00:00Z".into(),
        algorithm: "hmac-sha256".into(),
        key_id: "fixture-key-id".into(),
        artifact_sha256: hex::encode(Sha256::digest(b"fixture")),
        signature_hex: "00".into(),
    };
    assert_eq!(write_provenance_sidecar(&artifact, &sig).unwrap(), sidecar);
    let unavailable = verify_artifact_provenance(&cli, &artifact, None).unwrap();
    let key_path = provenance_key_path_for_cli(&cli);
    assert!(!unavailable.ok);
    assert_eq!(unavailable.code, "key_unavailable");
    assert_eq!(unavailable.key_id.as_deref(), Some(sig.key_id.as_str()));
    assert_eq!(
        unavailable.artifact_sha256.as_deref(),
        Some(sig.artifact_sha256.as_str())
    );
    let reason = unavailable.reason.unwrap();
    assert!(reason.contains("provenance key not found"));
    assert!(reason.contains(key_path.to_string_lossy().as_ref()));
    assert!(!key_path.exists());

    write_fixture(&key_path, "abcd\n");
    assert!(matches!(
        load_or_create_provenance_key(&cli, true),
        Err(AgentError::Config(message))
            if message.contains("must be at least 16 bytes")
                && message.contains(key_path.to_string_lossy().as_ref())
    ));
    assert_eq!(fs::read_to_string(&key_path).unwrap(), "abcd\n");

    fs::write(&key_path, TEST_KEY).unwrap();
    let diagnostics = build_elite_doctor_diagnostics(&cli);
    let expected_id = hex::encode(Sha256::digest(TEST_KEY.as_bytes()));
    assert_eq!(diagnostics["provenance"]["exists"], true);
    assert_eq!(diagnostics["provenance"]["key_id"], &expected_id[..16]);
    assert_eq!(diagnostics["elite_gate"]["runner"], "native");
}

#[tokio::test]
async fn verify_route_honors_signature_path_and_strictness() {
    let Some(cli) = isolated_cli(
        "verify_route_honors_signature_path_and_strictness",
        &[("HERMES_PROVENANCE_SIGNING_KEY", TEST_KEY)],
    ) else {
        return;
    };
    let root = hermes_state_root(&cli);
    let artifact = root.join("artifact");
    write_fixture(&artifact, b"original");
    let sig = sign_artifact_bytes(&cli, b"original", false).unwrap();
    assert!(!provenance_key_path_for_cli(&cli).exists());
    let explicit = root.join("explicit.sig.json");
    fs::write(&explicit, serde_json::to_vec(&sig).unwrap()).unwrap();

    let home = PathBuf::from(std::env::var_os("HERMES_HOME").unwrap());
    let path = || artifact.to_str().unwrap().to_owned();
    run_verify_provenance(
        doctor_cli(&home),
        path(),
        Some(explicit.to_str().unwrap().to_owned()),
        true,
        true,
    )
    .await
    .unwrap();

    write_provenance_sidecar(&artifact, &sig).unwrap();
    run_verify_provenance(doctor_cli(&home), path(), None, true, false)
        .await
        .unwrap();

    fs::write(&artifact, b"changed").unwrap();
    run_verify_provenance(doctor_cli(&home), path(), None, false, false)
        .await
        .unwrap();
    assert!(matches!(
        run_verify_provenance(cli, path(), None, true, true).await,
        Err(AgentError::Config(message)) if message == "artifact_sha256 mismatch"
    ));
}

#[test]
fn replay_summaries_sort_limit_and_report_each_integrity_problem() {
    let sandbox = tempfile::tempdir().unwrap();
    let dir = sandbox.path().join("replay");
    assert!(replay_integrity_summaries(&dir, 5).is_empty());

    let good = b" \n{\"seq\":1,\"event_hash\":\"a\"}\n{\"seq\":2,\"prev_hash\":\"a\",\"event_hash\":\"b\"}\n";
    write_fixture(&dir.join("10-good.jsonl"), good);
    fs::write(
        dir.join("20-invalid.jsonl"),
        "{\"seq\":1,\"event_hash\":\"a\"}\nnot-json\n",
    )
    .unwrap();
    fs::create_dir(dir.join("99-directory")).unwrap();

    let summaries = replay_integrity_summaries(&dir, 2);
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].file, "20-invalid.jsonl");
    assert_eq!(summaries[0].events, 1);
    assert_eq!(summaries[0].invalid_lines, 1);
    assert!(!summaries[0].hash_chain_ok);
    assert_eq!(summaries[1].file, "10-good.jsonl");
    assert_eq!(summaries[1].events, 2);
    assert_eq!(summaries[1].invalid_lines, 0);
    assert!(summaries[1].hash_chain_ok);
    assert_eq!(summaries[1].last_event_hash.as_deref(), Some("b"));
    assert_eq!(
        summaries[1].checksum_sha256,
        Some(hex::encode(Sha256::digest(good)))
    );

    let newest = replay_integrity_summaries(&dir, 1);
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0].file, "20-invalid.jsonl");

    let broken = dir.join("broken.jsonl");
    for (line, last_hash) in [
        (r#"{"seq":3,"prev_hash":"a","event_hash":"b"}"#, "b"),
        (r#"{"seq":2,"prev_hash":"a"}"#, "a"),
    ] {
        fs::write(
            &broken,
            format!("{{\"seq\":1,\"event_hash\":\"a\"}}\n{line}\n"),
        )
        .unwrap();
        let summary = replay_integrity_for_file(&broken);
        assert_eq!(summary.events, 2);
        assert_eq!(summary.invalid_lines, 0);
        assert!(!summary.hash_chain_ok, "{line}");
        assert_eq!(summary.last_event_hash.as_deref(), Some(last_hash));
    }

    let missing = replay_integrity_for_file(&dir.join("missing.jsonl"));
    assert_eq!(missing.events, 0);
    assert_eq!(missing.invalid_lines, 1);
    assert!(!missing.hash_chain_ok);
    assert_eq!(missing.checksum_sha256, None);
    assert_eq!(missing.last_event_hash, None);
}

fn bundle_entries(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let decoder = flate2::read::GzDecoder::new(fs::File::open(path).unwrap());
    let mut archive = tar::Archive::new(decoder);
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            assert_eq!(entry.header().mode().unwrap(), 0o600);
            let name = entry.path().unwrap().to_string_lossy().replace('\\', "/");
            let mut body = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut body).unwrap();
            (name, body)
        })
        .collect()
}

#[test]
fn default_bundle_contains_logs_and_signed_replay_manifest() {
    let Some(cli) = isolated_cli(
        "default_bundle_contains_logs_and_signed_replay_manifest",
        &[("HERMES_DETERMINISTIC_ARTIFACTS", " On ")],
    ) else {
        return;
    };
    let root = hermes_state_root(&cli);
    let payload = json!({"coverage": "bundle fixture"});
    let snapshot = write_doctor_snapshot(&cli, &payload, None).unwrap();
    let replay = b"{\"seq\":1,\"event_hash\":\"first\"}\n";
    write_fixture(&root.join("logs/replay/01.jsonl"), replay);
    write_fixture(&root.join("logs/hermes.log"), b"local log\n");
    write_fixture(&root.join("logs/mcp-stderr.log"), b"local mcp log\n");

    let bundle = build_doctor_support_bundle(&cli, &snapshot).unwrap();
    assert_eq!(
        bundle.parent(),
        Some(debug_reports_dir_for_cli(&cli).as_path())
    );
    let filename = bundle.file_name().unwrap().to_str().unwrap();
    assert!(filename.starts_with("support-bundle-") && filename.ends_with(".tar.gz"));
    let entries = bundle_entries(&bundle);
    assert_eq!(
        serde_json::from_slice::<Value>(&entries["doctor/snapshot.json"]).unwrap(),
        payload
    );
    assert!(!entries["doctor/debug-report.md"].is_empty());
    assert_eq!(entries["doctor/logs/hermes.log"], b"local log\n");
    assert_eq!(entries["doctor/logs/mcp-stderr.log"], b"local mcp log\n");
    assert_eq!(entries["doctor/replay/01.jsonl"], replay);

    let manifest_body = &entries["doctor/replay/manifest.json"];
    let manifest: Value = serde_json::from_slice(manifest_body).unwrap();
    assert_eq!(manifest["generated_at"], "1970-01-01T00:00:00Z");
    assert_eq!(
        manifest["totals"],
        json!({"files": 1, "events": 1, "invalid_lines": 0, "hash_chain_ok": true})
    );
    assert_eq!(manifest["files"][0]["file"], "01.jsonl");
    assert_eq!(manifest["files"][0]["last_event_hash"], "first");

    let manifest_path = root.join("extracted/manifest.json");
    let signature_path = root.join("extracted/manifest.sig.json");
    write_fixture(&manifest_path, manifest_body);
    write_fixture(&signature_path, &entries["doctor/replay/manifest.sig.json"]);
    let verified = verify_artifact_provenance(&cli, &manifest_path, Some(&signature_path)).unwrap();
    assert!(verified.ok, "{verified:?}");

    let occupied = root.join("occupied.tar.gz");
    write_fixture(&occupied.join("sentinel"), b"keep");
    assert!(matches!(
        build_doctor_support_bundle_with_options(&cli, &snapshot, Some(&occupied), true),
        Err(AgentError::Io(message))
            if message.starts_with("finalize ")
                && message.contains(occupied.to_string_lossy().as_ref())
    ));
    assert_eq!(fs::read(occupied.join("sentinel")).unwrap(), b"keep");

    let blocker = root.join("not-a-directory");
    fs::write(&blocker, b"keep").unwrap();
    let blocked_snapshot = blocker.join("snapshot.json");
    let blocked_bundle = blocker.join("bundle.tar.gz");
    for result in [
        write_doctor_snapshot(&cli, &payload, Some(blocked_snapshot.to_str().unwrap())),
        build_doctor_support_bundle_with_options(&cli, &snapshot, Some(&blocked_bundle), true),
    ] {
        assert!(matches!(
            result,
            Err(AgentError::Io(message))
                if message.starts_with("mkdir ")
                    && message.contains(blocker.to_string_lossy().as_ref())
        ));
    }
    assert_eq!(fs::read(blocker).unwrap(), b"keep");
}

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
    write_fixture(&cwd.join("scripts/run-elite-sync-gate.py"), b"# fixture\n");
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
