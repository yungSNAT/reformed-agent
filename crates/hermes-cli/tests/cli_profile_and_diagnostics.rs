#[path = "support/isolated_hermes.rs"]
mod support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use support::isolated_hermes;

async fn command(sandbox: &Path, args: &[&str], expected_exit: i32) -> String {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(sandbox).args(args).output(),
    )
    .await
    .expect("local CLI command timed out")
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected_exit),
        "{args:?}: {output:?}"
    );
    String::from_utf8(if expected_exit == 0 {
        output.stdout
    } else {
        output.stderr
    })
    .unwrap()
}

#[tokio::test]
async fn profile_rename_preserves_active_selection_alias_and_exported_content() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path();
    let home = root.join("hermes");
    let profiles = home.join("profiles");
    let marker = profiles.join(".active_profile");
    let aliases = profiles.join("aliases.json");
    let input = root.join("survey.yaml");
    let survey = profiles.join("survey.yaml");
    let archive = profiles.join("archive.yaml");
    let notebook = profiles.join("notebook.yaml");

    assert!(command(root, &["profile", "export"], 1)
        .await
        .contains("no active profile and no name provided"));
    assert!(command(root, &["profile", "import", "survey.yaml"], 1)
        .await
        .contains("profile import source not found"));
    fs::write(
        &input,
        "name: survey-template\nmodel: offline:survey\npersonality: quiet\nmax_turns: 7\n",
    )
    .unwrap();
    assert!(command(
        root,
        &["profile", "import", "survey.yaml", "--name", "field"],
        0
    )
    .await
    .contains("Added alias 'field' -> 'survey'."));
    let imported = fs::read(&survey).unwrap();
    fs::write(&input, "name: replacement\nmodel: offline:replacement\n").unwrap();
    assert!(command(root, &["profile", "import", "survey.yaml"], 1)
        .await
        .contains("Target profile exists"));
    assert_eq!(fs::read(&survey).unwrap(), imported);

    command(root, &["profile", "use", "field"], 0).await;
    assert_eq!(fs::read_to_string(&marker).unwrap().trim(), "survey");
    let config: serde_yaml::Value =
        serde_yaml::from_str(&fs::read_to_string(home.join("config.yaml")).unwrap()).unwrap();
    assert_eq!(config["model"].as_str(), Some("offline:survey"));
    assert_eq!(config["personality"].as_str(), Some("quiet"));
    assert_eq!(config["max_turns"].as_u64(), Some(7));
    command(root, &["profile", "export", "--output", "export.yaml"], 0).await;
    assert_eq!(fs::read(root.join("export.yaml")).unwrap(), imported);

    let export_dir = root.join("export-target");
    fs::create_dir(&export_dir).unwrap();
    fs::write(export_dir.join("keep.txt"), "Field notes").unwrap();
    let refused = command(root, &["profile", "export", "--output", "export-target"], 1).await;
    assert!(
        refused.contains("copy ") && refused.contains("export-target"),
        "{refused}"
    );
    assert_eq!(fs::read(&survey).unwrap(), imported);
    assert_eq!(
        fs::read_to_string(export_dir.join("keep.txt")).unwrap(),
        "Field notes"
    );

    command(
        root,
        &[
            "profile",
            "import",
            "export.yaml",
            "--import-name",
            "archive",
        ],
        0,
    )
    .await;
    let archived = fs::read(&archive).unwrap();
    assert!(
        command(root, &["profile", "rename", "survey", "archive"], 1)
            .await
            .contains("Target profile 'archive' already exists")
    );
    assert_eq!(fs::read(&archive).unwrap(), archived);
    assert_eq!(fs::read(&survey).unwrap(), imported);

    command(root, &["profile", "rename", "survey", "notebook"], 0).await;
    assert!(!survey.exists());
    assert_eq!(fs::read_to_string(&marker).unwrap().trim(), "notebook");
    let profile = command(root, &["profile", "show", "field"], 0).await;
    let profile: serde_yaml::Value = serde_yaml::from_str(&profile).unwrap();
    assert_eq!(profile["name"].as_str(), Some("notebook"));
    assert_eq!(profile["model"].as_str(), Some("offline:survey"));

    let saved_profile = fs::read(&notebook).unwrap();
    let saved_aliases = fs::read(&aliases).unwrap();
    let refused = command(root, &["profile", "delete", "field"], 0).await;
    assert!(refused.contains("Delete profile 'notebook'"), "{refused}");
    assert!(refused.contains("Aborted."), "{refused}");
    assert_eq!(fs::read(&notebook).unwrap(), saved_profile);
    assert_eq!(fs::read(&aliases).unwrap(), saved_aliases);
    assert_eq!(fs::read_to_string(&marker).unwrap().trim(), "notebook");

    command(root, &["profile", "delete", "field", "--yes"], 0).await;
    assert!(!notebook.exists());
    assert!(!marker.exists());
    let aliases: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(aliases).unwrap()).unwrap();
    assert_eq!(aliases, serde_json::json!({}));
    assert_eq!(fs::read(&archive).unwrap(), archived);
}

#[tokio::test]
async fn diagnostics_list_and_remove_local_reports_and_read_recent_logs() {
    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path();
    let home = root.join("hermes");
    assert!(command(root, &["debug", "list"], 0)
        .await
        .contains("No debug reports"));
    let reports = home.join("debug-reports");
    let report = reports.join("survey-debug-report.md");
    let other = reports.join("archive-debug-report.md");
    fs::write(&report, "Synthetic survey report\n").unwrap();
    fs::write(&other, "Synthetic archive report\n").unwrap();
    let listing = command(root, &["debug", "list"], 0).await;
    assert!(listing.contains("Debug reports ("));
    let listed: Vec<_> = listing
        .lines()
        .filter_map(|line| Path::new(line.trim()).file_name()?.to_str())
        .filter(|name| name.ends_with("-debug-report.md"))
        .collect();
    assert_eq!(
        listed,
        ["archive-debug-report.md", "survey-debug-report.md"]
    );

    let delete = [
        "debug",
        "delete",
        "hermes/debug-reports/survey-debug-report.md",
    ];
    assert!(command(root, &delete, 0)
        .await
        .contains("Removed debug report"));
    assert!(!report.exists());
    assert!(command(root, &delete, 0)
        .await
        .contains("Debug report not found"));
    assert_eq!(
        fs::read_to_string(other).unwrap(),
        "Synthetic archive report\n"
    );

    assert!(command(root, &["logs"], 0)
        .await
        .contains("No log file found"));
    let logs = home.join("logs");
    fs::create_dir_all(&logs).unwrap();
    fs::write(
        logs.join("hermes.log"),
        "First sample\nSecond sample\nThird sample\n",
    )
    .unwrap();
    let recent = command(root, &["logs", "2"], 0).await;
    assert!(!recent.contains("First sample"));
    assert!(recent.contains("Second sample\nThird sample"));
    assert!(recent.contains("Showing last 2 of 3 lines"));
}

#[tokio::test]
async fn doctor_snapshot_verification_detects_tampering_and_key_rotation() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let sandbox = tempfile::tempdir().unwrap();
    let root = sandbox.path();
    let home = root.join("hermes");
    fs::create_dir(&home).unwrap();
    let server = MockServer::start().await;
    let ready = "ollama-local";
    let backends = hermes_provider_runtime::local_backend_specs();
    let providers: serde_json::Map<String, Value> = backends
        .iter()
        .map(|spec| {
            let endpoint = if spec.provider == ready {
                "ready"
            } else {
                "unavailable"
            };
            (
                spec.provider.to_string(),
                json!({"base_url": format!("{}/{endpoint}", server.uri())}),
            )
        })
        .collect();
    fs::write(
        home.join("config.yaml"),
        json!({"llm_providers": providers}).to_string(),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/ready/models"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/unavailable/models"))
        .respond_with(ResponseTemplate::new(503))
        .expect((backends.len() - 1) as u64)
        .mount(&server)
        .await;

    let snapshot = "doctor.json";
    let signed = command(
        root,
        &["doctor", "--snapshot", "--snapshot-path", snapshot],
        0,
    )
    .await;
    assert!(signed.contains("Doctor snapshot: doctor.json"));
    assert!(signed.contains("Snapshot signature: doctor.json.sig.json"));
    let snapshot_path = root.join(snapshot);
    let original = fs::read(&snapshot_path).unwrap();
    let report: Value = serde_json::from_slice(&original).unwrap();
    let checks = report["checks"].as_array().unwrap();
    for (name, reachable) in [
        ("local_backend_ollama-local", true),
        ("local_backend_vllm", false),
    ] {
        let check = checks.iter().find(|check| check["name"] == name).unwrap();
        assert_eq!(check["reachable"], reachable, "{name}");
    }

    let verify = ["verify-provenance", snapshot, "--json"];
    let verified: Value = serde_json::from_str(&command(root, &verify, 0).await).unwrap();
    assert_eq!(verified["ok"], true);
    assert_eq!(verified["code"], "ok");

    fs::write(&snapshot_path, b"{}\n").unwrap();
    let changed: Value = serde_json::from_str(&command(root, &verify, 0).await).unwrap();
    assert_eq!(changed["ok"], false);
    assert_eq!(changed["code"], "artifact_sha256_mismatch");
    assert!(
        command(root, &["verify-provenance", snapshot, "--strict"], 1)
            .await
            .contains("artifact_sha256 mismatch")
    );
    fs::write(&snapshot_path, original).unwrap();

    let rotation = command(root, &["rotate-provenance-key"], 0).await;
    assert!(rotation.contains("Rotated provenance signing key."));
    assert!(rotation.contains("Archived previous key:"));
    let after_rotation: Value = serde_json::from_str(&command(root, &verify, 0).await).unwrap();
    assert_eq!(after_rotation["ok"], false);
    assert_eq!(after_rotation["code"], "signature_mismatch");
}
