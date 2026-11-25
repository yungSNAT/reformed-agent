use super::*;

const CHILD_HOME: &str = "HERMES_IMAGE_CONFIG_FIXTURE_HOME";

#[tokio::test]
async fn image_provider_selection_preserves_scoped_settings() {
    if let Some(home) = std::env::var_os(CHILD_HOME) {
        let home = PathBuf::from(home);
        assert_openai_image_config(&home);
        assert_xai_image_config(&home);
        assert_nous_image_config(&home);
        return;
    }

    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().canonicalize().unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            &format!("{module}::image_provider_selection_preserves_scoped_settings"),
        ])
        .kill_on_drop(true)
        .env_clear()
        .current_dir(&home)
        .env(CHILD_HOME, &home)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("HERMES_HOME", &home)
        .env("HERMES_AUTH_FILE", home.join("empty-fixture-auth.json"))
        .env("HERMES_IMAGE_GEN_PROVIDER", "unsupported-provider")
        .env("IMAGE_FIXTURE_VALUE", "fixture-image-value")
        .env("OPENAI_API_KEY", "fixture-openai-fallback")
        .env("OPENAI_IMAGE_BASE_URL", "https://env.fixture.invalid/");
    if let Some(root) = std::env::var_os("SystemRoot") {
        child.env("SystemRoot", root);
    }
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        child.env(
            "LLVM_PROFILE_FILE",
            std::env::current_dir().unwrap().join(profile),
        );
    }
    let output = tokio::time::timeout(Duration::from_secs(15), child.output())
        .await
        .expect("image configuration fixture exits")
        .expect("launch image configuration fixture");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed;"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_openai_image_config(home: &Path) {
    std::fs::write(
        home.join("config.yaml"),
        r#"image_gen:
  backend: " OpenAI-Images "
  model: gpt-image-2-high
  openai:
    model: gpt-image-2-low
    api_base_url: https://images.fixture.invalid/v1/
    api_key: "${IMAGE_FIXTURE_VALUE}"
"#,
    )
    .unwrap();
    let runtime = ImageGenRuntimeBackend::from_env_or_managed();
    assert_eq!(runtime.provider_label(), "openai");
    assert_eq!(runtime.required_env_vars(), ["OPENAI_API_KEY"]);
    let ImageGenRuntimeBackend::OpenAi(backend) = &runtime else {
        panic!("expected the configured OpenAI image provider");
    };
    let config = backend.config();
    assert_eq!(config.model(), "gpt-image-2-low");
    assert_eq!(config.quality(), "low");
    assert_eq!(config.base_url, "https://images.fixture.invalid/v1");
    assert_eq!(config.api_key.as_deref(), Some("fixture-image-value"));
    assert_eq!(config.output_dir, home.join("cache/images"));
}

fn assert_xai_image_config(home: &Path) {
    let yaml = r#"image_gen:
  provider: grok
  xai:
    model: grok-imagine-image-quality
    resolution: 2K
    base_url: https://edit.fixture.invalid/v1/
"#;
    let path = home.join("config.yaml");
    std::fs::write(
        &path,
        format!("{yaml}    api_key_env: IMAGE_FIXTURE_VALUE\n"),
    )
    .unwrap();
    let runtime = ImageGenRuntimeBackend::from_env_or_managed();
    assert_eq!(runtime.provider_label(), "xai");
    assert_eq!(runtime.required_env_vars(), ["XAI_API_KEY"]);
    let ImageGenRuntimeBackend::Xai(backend) = &runtime else {
        panic!("expected the configured xAI image provider");
    };
    assert_eq!(backend.config.model(), DEFAULT_XAI_IMAGE_EDIT_MODEL);
    assert_eq!(backend.config.resolution(), "2k");
    assert_eq!(backend.config.base_url, "https://edit.fixture.invalid/v1");
    assert_eq!(backend.config.source, "config");
    assert_eq!(
        backend.config.api_key.as_deref(),
        Some("fixture-image-value")
    );

    std::fs::write(path, yaml).unwrap();
    let ImageGenRuntimeBackend::Xai(unconfigured) = ImageGenRuntimeBackend::from_env_or_managed()
    else {
        panic!("the selected provider remains xAI when credentials are absent");
    };
    assert_eq!(unconfigured.config.source, "unconfigured");
    assert!(unconfigured.config.api_key.is_none());
}

fn assert_nous_image_config(home: &Path) {
    std::fs::write(
        home.join("config.yaml"),
        r#"image_gen:
  provider: nous-api
  model: fixture-image-model
llm_providers:
  nous-api:
    inference_base_url: https://nous.fixture.invalid/v1/
    api_key_env: IMAGE_FIXTURE_VALUE
"#,
    )
    .unwrap();
    let runtime = ImageGenRuntimeBackend::from_env_or_managed();
    assert_eq!(runtime.provider_label(), "nous");
    assert_eq!(runtime.required_env_vars(), ["NOUS_API_KEY"]);
    let ImageGenRuntimeBackend::OpenRouterCompat(backend) = &runtime else {
        panic!("expected a compatible image provider");
    };
    assert_eq!(
        backend.config.provider(),
        OpenRouterCompatImageProviderKind::Nous
    );
    assert_eq!(backend.config.model(), "fixture-image-model");
    assert_eq!(backend.config.base_url(), "https://nous.fixture.invalid/v1");
    assert_eq!(
        backend.config.api_key.as_deref(),
        Some("fixture-image-value")
    );
}
