#[path = "support/isolated_hermes.rs"]
mod isolated_hermes;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn select(
    menu: &mut Lines<BufReader<ChildStdout>>,
    input: &mut ChildStdin,
    label_prefix: &str,
) {
    while let Some(line) = menu.next_line().await.expect("read setup menu") {
        let Some((marker, label)) = line.trim().split_once(". ") else {
            continue;
        };
        if !label.starts_with(label_prefix) {
            continue;
        }
        // Numbered checklist rows add a selection marker before the number.
        let Some(Ok(choice)) = marker
            .split_whitespace()
            .next_back()
            .map(str::parse::<usize>)
        else {
            continue;
        };
        input
            .write_all(format!("{choice}\n").as_bytes())
            .await
            .expect("choose setup option");
        return;
    }
    panic!("setup exited before offering {label_prefix}");
}

async fn run_setup(
    home: &Path,
    catalogue_url: &str,
    mode: &str,
    answers: &str,
    tool_to_enable: Option<&str>,
) -> String {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut child = isolated_hermes::isolated_hermes(home)
            .env("OLLAMA_BASE_URL", catalogue_url)
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("NO_PROXY", "127.0.0.1")
            .arg("setup")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start setup CLI");
        let mut menu = BufReader::new(child.stdout.take().expect("setup stdout")).lines();
        let mut input = child.stdin.take().expect("setup stdin");

        select(&mut menu, &mut input, mode).await;
        select(&mut menu, &mut input, "Ollama Local").await;
        select(&mut menu, &mut input, "ollama-local:fixture-catalog-model").await;
        input
            .write_all(answers.as_bytes())
            .await
            .expect("finish setup");
        if let Some(tool) = tool_to_enable {
            select(&mut menu, &mut input, tool).await;
            input.write_all(b"\n").await.expect("confirm tools setup");
        }
        drop(input);

        let mut stdout = String::new();
        menu.into_inner()
            .read_to_string(&mut stdout)
            .await
            .expect("read setup result");
        assert!(
            child.wait().await.expect("wait for setup CLI").success(),
            "{stdout}"
        );
        assert!(stdout.contains("Setup complete!"), "{stdout}");
        stdout
    })
    .await
    .expect("setup CLI timed out")
}

#[tokio::test]
async fn setup_discovers_a_local_model_and_preserves_existing_configuration() {
    let service = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"id": "  fixture-catalog-model  "},
                {"id": "FIXTURE-CATALOG-MODEL"},
                {"id": ""},
                {"unknown": true}
            ]
        })))
        // The quick rerun reuses the persisted catalogue from full setup.
        .expect(1)
        .mount(&service)
        .await;
    let catalogue_url = format!("{}/v1", service.uri());
    let sandbox = tempfile::tempdir().expect("temporary CLI home");
    let home = sandbox.path().join("hermes");

    let full = run_setup(
        sandbox.path(),
        &catalogue_url,
        "Full setup",
        "Fixture persona\nn\n",
        None,
    )
    .await;
    assert!(full.contains("Wrote config.yaml"), "{full}");
    assert!(full.contains("Created default profile"), "{full}");
    assert!(full.contains("Created SOUL.md"), "{full}");
    let config_path = home.join("config.yaml");
    let config =
        hermes_config::load_user_config_file(&config_path).expect("generated configuration");
    assert_eq!(
        config.model.as_deref(),
        Some("ollama-local:fixture-catalog-model")
    );
    assert_eq!(config.personality.as_deref(), Some("Fixture persona"));
    let profile_path = home.join("profiles/default.yaml");
    let profile = std::fs::read_to_string(&profile_path).expect("generated profile");
    assert!(profile.contains("model: ollama-local:fixture-catalog-model"));
    assert!(profile.contains("personality: Fixture persona"));

    let original = std::fs::read(&config_path).expect("original configuration");
    let soul_path = home.join("SOUL.md");
    std::fs::write(&soul_path, "# Fixture prompt\n").expect("personalised prompt");
    let quick = run_setup(sandbox.path(), &catalogue_url, "Quick setup", "n\n", None).await;
    assert!(quick.contains("Keeping existing config.yaml"), "{quick}");
    assert_eq!(
        std::fs::read(&config_path).expect("preserved configuration"),
        original
    );
    assert_eq!(
        std::fs::read_to_string(profile_path).expect("preserved profile"),
        profile
    );
    assert_eq!(
        std::fs::read_to_string(soul_path).expect("preserved prompt"),
        "# Fixture prompt\n"
    );
}

#[tokio::test]
async fn full_setup_changes_known_tools_and_preserves_custom_keys() {
    let catalogue = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "fixture-catalog-model"}]
        })))
        .expect(1)
        .mount(&catalogue)
        .await;
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("hermes");
    std::fs::create_dir(&home).unwrap();
    let config_path = home.join("config.yaml");
    std::fs::write(
        &config_path,
        "tools_config:\n  enabled: [fixture_custom_enabled]\n  disabled: [fixture_custom_disabled, todo]\n",
    )
    .unwrap();

    let result = run_setup(
        sandbox.path(),
        &format!("{}/v1", catalogue.uri()),
        "Full setup",
        "\nn\n\n\n",
        Some("todo disabled "),
    )
    .await;
    assert!(
        result.contains("Updated tools setup: 1 enabled,"),
        "{result}"
    );
    let config = hermes_config::load_user_config_file(&config_path).unwrap();
    assert_eq!(
        config.tools_config.enabled,
        ["fixture_custom_enabled", "todo"]
    );
    assert!(config
        .tools_config
        .disabled
        .iter()
        .any(|tool| tool == "fixture_custom_disabled"));
    assert!(config
        .tools_config
        .disabled
        .iter()
        .any(|tool| tool == "terminal"));
    assert!(config
        .tools_config
        .disabled
        .iter()
        .all(|tool| tool != "todo"));
}
