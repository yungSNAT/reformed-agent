use super::*;

#[test]
fn local_provider_aliases_are_recognized_but_unknown_providers_are_not() {
    for alias in ["cdp", " Chrome ", "CHROMIUM", "local"] {
        assert_eq!(normalize_browser_provider(alias), Some("cdp"), "{alias}");
    }
    assert_eq!(normalize_browser_provider("unknown-provider"), None);
}

#[test]
fn cdp_error_formatting_uses_a_generic_label_for_opaque_endpoints() {
    let endpoint = "data:text/plain,private-endpoint-detail";
    let error = format_cdp_command_error(
        "Page.navigate",
        endpoint,
        ToolError::InvalidParams("No usable sandbox".into()),
    );
    let ToolError::ExecutionFailed(message) = error else {
        panic!("expected a CDP execution error");
    };
    assert_eq!(
        message.lines().next(),
        Some("Browser CDP command 'Page.navigate' failed while contacting the configured CDP endpoint: No usable sandbox")
    );
    assert!(message.contains("cold-starting"));
    assert!(message.contains("--no-sandbox"));
    assert!(!message.contains("private-endpoint-detail"));

    assert_eq!(
        tool_error_message(ToolError::NotFound("tab not found".into())),
        "tab not found"
    );
    assert_eq!(
        tool_error_message(ToolError::Timeout("tab did not respond".into())),
        "tab did not respond"
    );
}

#[test]
fn camofox_leaves_unrewritable_inputs_unchanged() {
    for (url, alias) in [
        ("not an absolute URL", "host.docker.internal"),
        ("ftp://localhost/file", "host.docker.internal"),
        ("http://localhost:8080/page", "[not-an-ipv6-address]"),
    ] {
        assert_eq!(
            rewrite_loopback_url_for_camofox(url, true, alias),
            (url.to_owned(), None),
            "{url} with alias {alias}"
        );
    }
}

#[test]
fn secret_query_validation_ignores_unparseable_inputs() {
    assert!(validate_url_does_not_exfiltrate_secret("relative/path?token=fixture").is_ok());
    assert_blocked_secret(validate_url_does_not_exfiltrate_secret(SECRET_QUERY_URL));
}

#[test]
fn fallback_wraps_non_object_results_without_losing_their_type() {
    for (input, expected) in [
        ("[1,true]", json!([1, true])),
        ("null", Value::Null),
        ("plain browser output", json!("plain browser output")),
    ] {
        let output = browser_fallback_response(input.into(), "test cloud", "session unavailable");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({
                "fallback_from_cloud": true,
                "fallback_provider": "test cloud",
                "fallback_reason": "session unavailable",
                "local_result": expected,
            })
        );
    }
}

#[test]
fn slash_only_base_url_uses_the_supplied_default() {
    assert_eq!(
        normalize_base_url_with_default(" \t/// \n", "https://default.invalid/api"),
        "https://default.invalid/api"
    );
}

#[test]
fn yaml_scalar_and_gateway_values_preserve_their_distinct_meanings() {
    for (raw, scalar, truthy, route) in [
        ("true", Some("true"), true, true),
        ("-2", Some("-2"), true, true),
        ("0", Some("0"), false, false),
        ("1.5", Some("1.5"), false, false),
        ("' YeS '", Some(" YeS "), true, true),
        ("' NoUs '", Some(" NoUs "), false, true),
        ("'direct'", Some("direct"), false, false),
        ("[true]", None, false, false),
    ] {
        let value: serde_yaml::Value = serde_yaml::from_str(raw).unwrap();
        assert_eq!(yaml_scalar_as_string(&value).as_deref(), scalar, "{raw}");
        assert_eq!(yaml_truthy(&value), truthy, "{raw}");
        assert_eq!(yaml_gateway_route(&value), route, "{raw}");
    }

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.yaml");
    std::fs::write(&path, "tool_gateway:\n  browser: ' NoUs '\n").unwrap();
    assert_eq!(
        browser_use_gateway_preference_from_yaml_path(&path),
        Some(true)
    );

    std::fs::write(
        &path,
        "browser:\n  use_gateway: 0\ntool_gateway:\n  browser: nous\n",
    )
    .unwrap();
    assert_eq!(
        browser_use_gateway_preference_from_yaml_path(&path),
        Some(false)
    );
}

#[tokio::test]
async fn pruning_keeps_fresh_files_and_survives_filesystem_errors() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let stale = dir.join("browser_screenshot_stale.png");
    let fresh = dir.join("browser_screenshot_fresh.png");
    let unrelated = dir.join("unrelated.png");
    let old_time = std::time::SystemTime::now() - SCREENSHOT_MAX_AGE - Duration::from_secs(3600);

    std::fs::File::create(&stale)
        .unwrap()
        .set_modified(old_time)
        .unwrap();
    std::fs::File::create(&unrelated)
        .unwrap()
        .set_modified(old_time)
        .unwrap();
    std::fs::write(&fresh, b"fresh screenshot").unwrap();

    prune_old_screenshots(&fresh).await;
    assert_eq!(std::fs::read(&fresh).unwrap(), b"fresh screenshot");

    #[cfg(unix)]
    let stale_directory = {
        let path = dir.join("browser_screenshot_directory.png");
        std::fs::create_dir(&path).unwrap();
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(old_time)
            .unwrap();
        path
    };

    prune_old_screenshots(dir).await;
    assert!(!stale.exists());
    assert_eq!(std::fs::read(&fresh).unwrap(), b"fresh screenshot");
    assert!(unrelated.is_file());
    #[cfg(unix)]
    assert!(stale_directory.is_dir());
}

#[tokio::test]
async fn unavailable_backend_reports_its_reason_for_every_action() {
    let reason = "browser credentials are missing";
    let backend = unavailable_browser_backend(reason);
    for result in [
        backend.navigate("about:blank").await,
        backend.blank_page().await,
        backend.snapshot().await,
        backend.click("#button").await,
        backend.r#type("#input", "hello").await,
        backend.scroll("down", Some(80)).await,
        backend.go_back().await,
        backend.press("Enter").await,
        backend.get_images(Some("img")).await,
        backend.screenshot().await,
        backend.console("get").await,
    ] {
        assert_eq!(tool_error_message(result.unwrap_err()), reason);
    }
}

const ENV_CASE: &str = "HERMES_TOOLS_BROWSER_COVERAGE_CASE";
const SECRET_QUERY_URL: &str = "https://example.invalid/?api_key=fixture-only-value";
const FALLBACK_PROVIDER: &str = "test cloud";
const FALLBACK_REASON: &str = "cloud session is unavailable";

fn assert_blocked_secret<T: std::fmt::Debug>(result: Result<T, ToolError>) {
    let error = result.unwrap_err();
    let ToolError::InvalidParams(message) = error else {
        panic!("expected secret query validation to reject the URL: {error:?}");
    };
    assert!(message.contains("'api_key'"), "{message}");
    assert!(!message.contains("fixture-only-value"), "{message}");
}

fn run_env_case(case: &str, home: &Path, overrides: &[(&str, &str)]) {
    let (_, module) = module_path!().split_once("::").unwrap();
    let test_name = format!("{module}::browser_environment_cases");
    // Give the test process its own environment rather than changing the
    // environment shared with other tests in this process.
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .current_dir(home)
        .env(ENV_CASE, case)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home)
        .env("LOCALAPPDATA", home)
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_DATA_HOME", home)
        .env("HERMES_HOME", home)
        .env("CHROME_CDP_URL", "http://[")
        .envs(overrides.iter().copied())
        .args(["--exact", test_name.as_str(), "--nocapture"]);
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }

    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{case} ({}):\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn browser_environment_cases() {
    if let Ok(case) = std::env::var(ENV_CASE) {
        let home = std::env::var_os("HERMES_HOME").unwrap();
        let home = Path::new(&home);
        match case.as_str() {
            "defaults" => check_provider_config(home),
            "env-values" => {
                assert!(!camofox_loopback_rewrite_enabled_from_env());
                assert_eq!(camofox_loopback_alias_from_env(), "test-host.invalid");
                assert_eq!(
                    env_duration_seconds("HERMES_BROWSER_COMMAND_TIMEOUT_SECONDS"),
                    None
                );
                assert_eq!(cdp_command_timeout(), Duration::from_millis(1250));
            }
            "browser-use-missing" => check_missing_provider("browser-use").await,
            "browserbase-missing" => check_missing_provider("browserbase").await,
            "firecrawl-missing" => check_missing_provider("firecrawl").await,
            "browser-use-configured" => {
                assert_eq!(browser_backend_choice_from_env(), "browser-use");
                assert!(camofox_loopback_rewrite_enabled_from_env());
                assert_blocked_secret(browser_backend_from_env().navigate(SECRET_QUERY_URL).await);
                check_cloud_fallback_actions().await;
            }
            "firecrawl-configured" => {
                assert_eq!(browser_backend_choice_from_env(), "firecrawl");
                assert!(FirecrawlBrowserBackend::from_env().is_ok());
                assert_blocked_secret(
                    isolated_browser_backend_from_env()
                        .navigate(SECRET_QUERY_URL)
                        .await,
                );
            }
            "screenshot-create-error" => {
                let cache = home.join("cache");
                std::fs::write(&cache, b"not a directory").unwrap();
                let dir = cache.join("screenshots");
                let error = save_browser_screenshot(b"fixture").await.unwrap_err();
                assert!(matches!(
                    error,
                    ToolError::ExecutionFailed(message)
                        if message.starts_with(&format!("Failed to create {}:", dir.display()))
                ));
                assert_eq!(std::fs::read(cache).unwrap(), b"not a directory");
            }
            #[cfg(target_os = "linux")]
            "screenshot-write-error" => {
                let dir = home.join("cache/screenshots");
                let error = save_browser_screenshot(b"fixture").await.unwrap_err();
                assert!(matches!(
                    error,
                    ToolError::ExecutionFailed(message)
                        if message.starts_with(&format!(
                            "Failed to save {}/browser_screenshot_", dir.display()
                        ))
                ));
                assert!(dir.is_dir());
                assert!(std::fs::read_dir(dir).unwrap().next().is_none());
            }
            _ => panic!("unknown browser test case: {case}"),
        }
        return;
    }

    let cases: &[(&str, &[(&str, &str)])] = &[
        ("defaults", &[]),
        (
            "env-values",
            &[
                ("CAMOFOX_REWRITE_LOOPBACK_URLS", " oFf "),
                ("CAMOFOX_LOOPBACK_HOST_ALIAS", " test-host.invalid "),
                ("HERMES_BROWSER_COMMAND_TIMEOUT_SECONDS", "NaN"),
                ("BROWSER_COMMAND_TIMEOUT", "1.25"),
            ],
        ),
        ("browser-use-missing", &[("BROWSER_PROVIDER", "browseruse")]),
        (
            "browserbase-missing",
            &[("BROWSER_PROVIDER", "browser-base")],
        ),
        ("firecrawl-missing", &[("BROWSER_PROVIDER", "fire-crawl")]),
        (
            "browser-use-configured",
            &[
                ("HERMES_BROWSER_BACKEND", "unknown-provider"),
                ("BROWSER_CLOUD_PROVIDER", " MANAGED_BROWSER "),
                ("BROWSER_USE_API_KEY", "test-browser-use-key"),
                ("BROWSER_USE_BASE_URL", "file:///browser-coverage"),
                ("CAMOFOX_REWRITE_LOOPBACK_URLS", "yes"),
            ],
        ),
        (
            "firecrawl-configured",
            &[
                ("BROWSER_CLOUD_PROVIDER", "firecrawl-browser"),
                ("FIRECRAWL_API_KEY", "test-firecrawl-key"),
                ("FIRECRAWL_BROWSER_BASE_URL", "file:///browser-coverage"),
                ("FIRECRAWL_BASE_URL", "file:///browser-coverage"),
            ],
        ),
        ("screenshot-create-error", &[]),
    ];
    for &(case, overrides) in cases {
        let home = tempfile::tempdir().unwrap();
        run_env_case(case, home.path(), overrides);
    }

    #[cfg(target_os = "linux")]
    {
        // Keep the directory addressable, but exceed Linux's 4096-byte
        // pathname limit once the generated screenshot filename is appended.
        let root = tempfile::tempdir().unwrap();
        let mut home = root.path().to_path_buf();
        while home.as_os_str().len() + 65 <= 4040 {
            home.push("h".repeat(64));
        }
        let remaining = 4040 - home.as_os_str().len();
        if remaining > 1 {
            home.push("h".repeat(remaining - 1));
        }
        std::fs::create_dir_all(&home).unwrap();
        run_env_case("screenshot-write-error", &home, &[]);
    }
}

fn check_provider_config(home: &Path) {
    let path = cli_config_path();
    assert!(path.starts_with(home));
    assert!(config_path().starts_with(home));
    assert!(!camofox_loopback_rewrite_enabled_from_env());
    assert_eq!(camofox_loopback_alias_from_env(), "host.docker.internal");
    assert!(!explicit_browser_use_requested_from_env_or_config());
    assert!(!explicit_firecrawl_requested_from_env_or_config());

    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "browser:\n  cloud_provider: managed-browser\n").unwrap();
    assert!(explicit_browser_use_requested_from_env_or_config());
    assert!(!explicit_firecrawl_requested_from_env_or_config());

    std::fs::write(path, "browser:\n  cloud_provider: firecrawl-browser\n").unwrap();
    assert!(!explicit_browser_use_requested_from_env_or_config());
    assert!(explicit_firecrawl_requested_from_env_or_config());
}

async fn check_missing_provider(provider: &str) {
    assert_eq!(browser_backend_choice_from_env(), provider);
    let reason = match provider {
        "browser-use" => {
            assert!(explicit_browser_use_requested_from_env_or_config());
            BrowserUseBrowserBackend::from_env()
                .err()
                .map(|e| e.to_string())
        }
        "browserbase" => {
            assert!(explicit_browserbase_requested_from_env_or_config());
            BrowserbaseBrowserBackend::from_env()
                .err()
                .map(|e| e.to_string())
        }
        "firecrawl" => {
            assert!(explicit_firecrawl_requested_from_env_or_config());
            FirecrawlBrowserBackend::from_env()
                .err()
                .map(|e| e.to_string())
        }
        _ => panic!("unexpected provider: {provider}"),
    }
    .expect("the isolated environment has no provider credentials");

    let error = browser_backend_from_env().snapshot().await.unwrap_err();
    assert_eq!(tool_error_message(error), reason);
}

async fn check_cloud_fallback_actions() {
    let local = CdpBrowserBackend::from_env();
    local.retired.store(true, Ordering::Release);
    let reference = CdpBrowserBackend::from_env();
    reference.retired.store(true, Ordering::Release);
    let backend = CloudFallbackBrowserBackend::new(
        FALLBACK_PROVIDER,
        BrowserUseBrowserBackend::from_env().unwrap(),
        local,
    );
    *backend.fallback_reason.lock().await = Some(FALLBACK_REASON.into());

    assert_blocked_secret(backend.navigate(SECRET_QUERY_URL).await);
    let url = "https://example.invalid/example";
    for (actual, direct) in [
        (backend.navigate(url).await, reference.navigate(url).await),
        (
            backend.r#type("#input", "hello").await,
            reference.r#type("#input", "hello").await,
        ),
        (
            backend.scroll("down", Some(80)).await,
            reference.scroll("down", Some(80)).await,
        ),
        (backend.go_back().await, reference.go_back().await),
        (backend.press("Enter").await, reference.press("Enter").await),
        (
            backend.get_images(Some("img")).await,
            reference.get_images(Some("img")).await,
        ),
        (backend.screenshot().await, reference.screenshot().await),
        (backend.console("get").await, reference.console("get").await),
        (backend.blank_page().await, reference.blank_page().await),
    ] {
        match direct {
            Ok(result) => assert_eq!(
                actual.unwrap(),
                browser_fallback_response(result, FALLBACK_PROVIDER, FALLBACK_REASON)
            ),
            Err(error) => {
                let expected = tool_error_message(error);
                let actual = tool_error_message(actual.unwrap_err());
                assert!(actual.contains(&expected), "{actual:?} lacks {expected:?}");
            }
        }
    }
    assert_eq!(
        backend.fallback_reason.lock().await.as_deref(),
        Some(FALLBACK_REASON)
    );
}
