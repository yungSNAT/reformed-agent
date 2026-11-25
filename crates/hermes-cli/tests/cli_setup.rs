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
) -> String {
    let mut observed = String::new();
    while let Some(line) = menu.next_line().await.expect("read setup menu") {
        observed.push_str(&line);
        observed.push('\n');
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
        return observed;
    }
    panic!("setup exited before offering {label_prefix}");
}

async fn run_setup(
    home: &Path,
    catalogue_url: &str,
    mode: &str,
    model_choice: &str,
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
        select(&mut menu, &mut input, model_choice).await;
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
async fn setup_can_decline_oauth_and_keep_a_manual_key_in_the_vault() {
    let sandbox = tempfile::tempdir().unwrap();
    let proxy = MockServer::start().await;
    assert!(proxy.address().ip().is_loopback());
    let key = "fixture-setup-key";
    let answers = format!("n\n{key}\ny\n");
    let stdout = run_authenticated_setup(
        isolated_hermes::isolated_hermes(sandbox.path()),
        &proxy.uri(),
        "OpenAI (OAuth/API key)",
        &answers,
        Some("fixture-manual-model"),
    )
    .await;
    assert!(
        stdout.contains("Stored OpenAI key in encrypted vault"),
        "{stdout}"
    );
    assert!(!stdout.contains(key));

    let home = sandbox.path().join("hermes");
    let config_path = home.join("config.yaml");
    let config = hermes_config::load_user_config_file(&config_path).unwrap();
    assert_eq!(config.model.as_deref(), Some("openai:fixture-manual-model"));
    assert!(home.join("auth/tokens.json").is_file());
    assert!(!std::fs::read_to_string(config_path).unwrap().contains(key));

    let result = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes::isolated_hermes(sandbox.path())
            .env("HTTP_PROXY", proxy.uri())
            .env("HTTPS_PROXY", proxy.uri())
            .args(["secrets", "list"])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.status.success(), "{result:?}");
    let listed = String::from_utf8_lossy(&result.stdout);
    assert!(
        listed.contains("openai (env: HERMES_OPENAI_API_KEY)"),
        "{listed}"
    );
    assert!(!listed.contains(key));
}

#[tokio::test]
async fn quick_setup_imports_the_selected_providers_local_oauth_session() {
    use hermes_cli::auth::{CODEX_OAUTH_CLIENT_ID, CODEX_OAUTH_TOKEN_URL, DEFAULT_CODEX_BASE_URL};

    let proxy = MockServer::start().await;
    assert!(proxy.address().ip().is_loopback());
    let token = "local-oauth-setup-fixture";
    for (provider, menu_label, display, answers, model_after_picker) in [
        (
            "openai",
            "OpenAI (OAuth/API key)",
            "OpenAI",
            "y\n",
            Some("fixture-import-model"),
        ),
        (
            "openai-codex",
            "OpenAI Codex",
            "OpenAI Codex",
            "y\nfixture-import-model\n",
            None,
        ),
    ] {
        let sandbox = tempfile::tempdir().unwrap();
        let fixture = sandbox.path().join("fixture-import.json");
        std::fs::write(
            &fixture,
            serde_json::json!({"tokens": {"access_token": token}}).to_string(),
        )
        .unwrap();
        let mut command = isolated_hermes::isolated_hermes(sandbox.path());
        command.env("HERMES_CODEX_AUTH_FILE", &fixture);
        let stdout = run_authenticated_setup(
            command,
            &proxy.uri(),
            menu_label,
            answers,
            model_after_picker,
        )
        .await;
        assert!(
            stdout.contains(&format!("Detected existing {display} OAuth session")),
            "{stdout}"
        );
        assert!(
            stdout.contains(&format!("Stored {display} key in encrypted vault")),
            "{stdout}"
        );
        assert!(!stdout.contains(token));

        let home = sandbox.path().join("hermes");
        let config_path = home.join("config.yaml");
        let config = hermes_config::load_user_config_file(&config_path).unwrap();
        assert_eq!(
            config.model,
            Some(format!("{provider}:fixture-import-model"))
        );
        let settings = &config.llm_providers[provider];
        assert_eq!(settings.base_url.as_deref(), Some(DEFAULT_CODEX_BASE_URL));
        assert_eq!(
            settings.oauth_token_url.as_deref(),
            Some(CODEX_OAUTH_TOKEN_URL)
        );
        assert_eq!(
            settings.oauth_client_id.as_deref(),
            Some(CODEX_OAUTH_CLIENT_ID)
        );
        assert!(settings.api_key.is_none());
        assert!(home.join("auth/tokens.json").is_file());
        assert!(!std::fs::read_to_string(config_path)
            .unwrap()
            .contains(token));
    }
}

async fn run_authenticated_setup(
    mut command: tokio::process::Command,
    proxy: &str,
    provider: &str,
    answers: &str,
    model_after_picker: Option<&str>,
) -> String {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut child = command
            .env("HTTP_PROXY", proxy)
            .env("HTTPS_PROXY", proxy)
            .env("ALL_PROXY", proxy)
            .arg("setup")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut menu = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut input = child.stdin.take().unwrap();
        select(&mut menu, &mut input, "Quick setup").await;
        select(&mut menu, &mut input, provider).await;
        input.write_all(answers.as_bytes()).await.unwrap();
        let mut stdout = String::new();
        if let Some(model) = model_after_picker {
            stdout = select(&mut menu, &mut input, "Custom model ID").await;
            input
                .write_all(format!("{model}\n").as_bytes())
                .await
                .unwrap();
        }
        drop(input);
        menu.into_inner().read_to_string(&mut stdout).await.unwrap();
        assert!(child.wait().await.unwrap().success(), "{stdout}");
        assert!(stdout.contains("Setup complete!"), "{stdout}");
        stdout
    })
    .await
    .expect("authenticated setup timed out")
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
        "ollama-local:fixture-catalog-model",
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
    let quick = run_setup(
        sandbox.path(),
        &catalogue_url,
        "Quick setup",
        "ollama-local:fixture-catalog-model",
        "n\n",
        None,
    )
    .await;
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
async fn setup_saves_a_custom_model_when_the_local_catalogue_is_unavailable() {
    let catalogue = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&catalogue)
        .await;
    let sandbox = tempfile::tempdir().expect("temporary CLI home");

    let result = run_setup(
        sandbox.path(),
        &format!("{}/v1", catalogue.uri()),
        "Full setup",
        "Custom model ID",
        "  fixture-model  \nfixture-personality\nn\n",
        None,
    )
    .await;
    assert!(
        result.contains("personalities/fixture-personality.md"),
        "{result}"
    );
    let home = sandbox.path().join("hermes");
    let config = hermes_config::load_user_config_file(&home.join("config.yaml"))
        .expect("generated configuration");
    assert_eq!(config.model.as_deref(), Some("ollama-local:fixture-model"));
    assert_eq!(config.personality.as_deref(), Some("fixture-personality"));
    let profile =
        std::fs::read_to_string(home.join("profiles/default.yaml")).expect("generated profile");
    let profile: serde_yaml::Value = serde_yaml::from_str(&profile).expect("parse profile");
    assert_eq!(
        profile["model"].as_str(),
        Some("ollama-local:fixture-model")
    );
    assert_eq!(profile["personality"].as_str(), Some("fixture-personality"));
}

#[tokio::test]
async fn vertex_setup_saves_custom_models_and_uses_or_replaces_project_defaults() {
    let sandbox = tempfile::tempdir().expect("temporary CLI home");
    let home = sandbox.path().join("hermes");
    std::fs::create_dir(&home).unwrap();
    let config_path = home.join("config.yaml");
    std::fs::write(
        &config_path,
        "vertex:\n  project_id: fixture-existing-project\n  region: us-central1\nmax_turns: 12\n",
    )
    .unwrap();
    for (project, entered_region, region, custom_model, credential_path) in [
        ("", "", "us-central1", "fixture-gemini-standard", None),
        (
            "fixture-new-project",
            "europe-west4",
            "europe-west4",
            "vertex:fixture-gemini-advanced",
            Some(sandbox.path().join("nonexistent-fixture-credentials.json")),
        ),
    ] {
        let expected_project = if project.is_empty() {
            "fixture-existing-project"
        } else {
            project
        };
        let expected_model = if custom_model.contains(':') {
            custom_model.to_string()
        } else {
            format!("vertex:{custom_model}")
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut command = isolated_hermes::isolated_hermes(sandbox.path());
            command
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("HTTPS_PROXY", "http://127.0.0.1:1")
                .arg("setup")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped());
            if let Some(path) = credential_path {
                command.env("VERTEX_CREDENTIALS_PATH", path);
            }
            let mut child = command.spawn().expect("start Vertex setup");
            let mut menu = BufReader::new(child.stdout.take().unwrap()).lines();
            let mut input = child.stdin.take().unwrap();
            select(&mut menu, &mut input, "Quick setup").await;
            select(&mut menu, &mut input, "Google Vertex AI").await;
            input
                .write_all(format!("{project}\n{entered_region}\n").as_bytes())
                .await
                .unwrap();
            select(&mut menu, &mut input, "Custom model ID").await;
            input
                .write_all(format!("{custom_model}\ny\n").as_bytes())
                .await
                .unwrap();
            drop(input);
            let mut stdout = String::new();
            menu.into_inner().read_to_string(&mut stdout).await.unwrap();
            assert!(child.wait().await.unwrap().success(), "{stdout}");
            assert!(stdout.contains("Wrote config.yaml"), "{stdout}");
            assert!(stdout.contains("Setup complete!"), "{stdout}");
        })
        .await
        .expect("Vertex setup timed out");
        let config = hermes_config::load_user_config_file(&config_path).unwrap();
        assert_eq!(config.vertex.project_id.as_deref(), Some(expected_project));
        assert_eq!(config.vertex.region.as_deref(), Some(region));
        assert_eq!(config.model.as_deref(), Some(expected_model.as_str()));
        assert_eq!(config.personality.as_deref(), Some("default"));
        assert_eq!(config.max_turns, 250);
    }
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
        "ollama-local:fixture-catalog-model",
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
