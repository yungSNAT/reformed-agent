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
    let mut command = Command::new(std::env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
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
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", std::env::current_dir().unwrap().join(profile));
    }
    for key in ["SystemRoot", "WINDIR"] {
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

include!("doctor_routes_part2_coverage_gen.rs");
