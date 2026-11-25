use std::time::Duration;

use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[path = "support/isolated_hermes.rs"]
mod isolated;
use isolated::isolated_hermes;

// OAuth discovery uses the Unix home directory, which the fixture sets via HOME.
#[cfg(unix)]
#[tokio::test]
async fn oneshot_verifies_and_retries_oauth_rejection_once_without_opening_login() {
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{body_partial_json, header};

    let prompt = "Read the fixture";
    let reply = "Reply after the verified retry.";
    let fixture_key = "fixture-oneshot-key";
    for retry_succeeds in [true, false] {
        let provider = MockServer::start().await;
        let attempts = AtomicUsize::new(0);
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", format!("Bearer {fixture_key}")))
            .and(body_partial_json(json!({"model": "gpt-4o-mini"})))
            .respond_with(move |_: &wiremock::Request| {
                let attempt = attempts.fetch_add(1, Ordering::Relaxed);
                if attempt == 0 || !retry_succeeds {
                    return ResponseTemplate::new(401).set_body_json(json!({
                        "error": {"message": "fixture OAuth token rejected", "type": "authentication_error"}
                    }));
                }
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "fixture-retry",
                    "object": "chat.completion",
                    "created": 0,
                    "model": "gpt-4o-mini",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": reply},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            })
            .expect(2)
            .mount(&provider)
            .await;

        let sandbox = tempfile::tempdir().expect("temporary CLI home");
        let home = sandbox.path().join("hermes");
        std::fs::create_dir(&home).expect("create fixture home");
        let endpoint = format!("{}/v1", provider.uri());
        std::fs::write(
            home.join("config.yaml"),
            format!(
                "model: openai:gpt-4o-mini\nllm_providers:\n  openai:\n    base_url: {endpoint}\n    api_mode: chat_completions\n    discover_models: false\n    models:\n      gpt-4o-mini: {{}}\n"
            ),
        )
        .expect("write fixture provider");
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            isolated_hermes(sandbox.path())
                .env("OPENAI_BASE_URL", &endpoint)
                .env("OPENAI_API_KEY", fixture_key)
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("HTTPS_PROXY", "http://127.0.0.1:1")
                .env("NO_PROXY", "127.0.0.1")
                .env("HERMES_QUERY_DISABLE_TOOLS", "1")
                .args(["--provider", "openai", "--oneshot", prompt])
                .output(),
        )
        .await
        .expect("one-shot retry timed out")
        .expect("run one-shot CLI");
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
        let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
        assert_eq!(
            output.status.success(),
            retry_succeeds,
            "{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("Auth verify: provider='openai', status=unverified"),
            "{stdout}\n{stderr}"
        );
        assert_eq!(stderr.matches("and retrying once").count(), 1, "{stderr}");
        assert!(!stdout.contains(fixture_key) && !stderr.contains(fixture_key));
        assert!(!stderr.contains("launching `hermes-ultra auth login"));
        let attempts: Vec<_> = provider
            .received_requests()
            .await
            .expect("recorded fixture requests")
            .into_iter()
            .filter(|request| request.url.path() == "/v1/chat/completions")
            .collect();
        assert_eq!(attempts.len(), 2, "{stdout}\n{stderr}");
        for request in attempts {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("chat body");
            assert!(body["messages"]
                .as_array()
                .expect("chat messages")
                .iter()
                .any(|message| message["role"] == "user" && message["content"] == prompt));
        }
        if retry_succeeds {
            assert!(stdout.lines().any(|line| line == reply), "{stdout}");
        } else {
            assert!(
                stderr.contains("not attached to an interactive terminal"),
                "{stderr}"
            );
            assert!(stderr.contains("fixture OAuth token rejected"), "{stderr}");
        }
    }
}

#[tokio::test]
async fn oauth_status_reports_offline_provider_metadata_and_masks_fixture_tokens() {
    use serde_json::json;

    let sandbox = tempfile::tempdir().unwrap();
    let state = sandbox.path().join("hermes-oauth-fixture.json");
    let google = sandbox.path().join("google-oauth-fixture.json");
    let qwen = sandbox.path().join("qwen-oauth-fixture.json");
    let access = "fixture-access-tail";
    let expiry = 4_102_444_800_000_i64;
    for present in [false, true] {
        if present {
            std::fs::write(
                &state,
                json!({"version": 1, "providers": {
                    "anthropic": {"access_token": access, "source": "fixture-state", "expires_at_ms": expiry},
                    "google-gemini-cli": {"access_token": access},
                    "qwen-oauth": {"access_token": access}
                }})
                .to_string(),
            )
            .unwrap();
            std::fs::write(
                &google,
                json!({
                    "access": access,
                    "expires": expiry,
                    "email": "fixture@example.invalid",
                    "project_id": "fixture-project"
                })
                .to_string(),
            )
            .unwrap();
            std::fs::write(
                &qwen,
                json!({"access_token": access, "expiry_date": expiry}).to_string(),
            )
            .unwrap();
        }

        for (provider, label, fixture_source) in [
            ("anthropic", "Anthropic OAuth", "fixture-state"),
            ("google-gemini-cli", "Google Gemini OAuth", "google-oauth"),
            ("qwen-oauth", "Qwen OAuth", "qwen-cli"),
        ] {
            let output = tokio::time::timeout(
                Duration::from_secs(10),
                isolated_hermes(sandbox.path())
                    .env("HERMES_AUTH_FILE", &state)
                    .env("HERMES_GEMINI_OAUTH_FILE", &google)
                    .env("HERMES_QWEN_CLI_AUTH_FILE", &qwen)
                    .env("HTTP_PROXY", "http://127.0.0.1:1")
                    .env("HTTPS_PROXY", "http://127.0.0.1:1")
                    .args(["auth", "status", provider])
                    .output(),
            )
            .await
            .expect("offline auth status timed out")
            .unwrap();
            assert!(output.status.success(), "{provider}: {output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(!stdout.contains(access) && !stderr.contains(access));
            let status = stdout
                .lines()
                .find(|line| line.starts_with(&format!("{label}: ")))
                .unwrap_or_else(|| panic!("{provider}: {stdout}"));
            let oauth_source = if present { fixture_source } else { "none" };
            let expires = if present {
                expiry.to_string()
            } else {
                "unknown".to_owned()
            };
            assert!(
                status.starts_with(&format!("{label}: logged_in={present} "))
                    && status.ends_with(&format!(" source={oauth_source} expires_at_ms={expires}")),
                "{provider}: {stdout}"
            );
            let source = if present { "auth_json" } else { "none" };
            assert!(
                stdout.contains(&format!(
                    "provider='{provider}', credential_present={present}, source={source}, oauth_state_present={present}"
                )),
                "{provider}: {stdout}"
            );
            if present {
                assert!(
                    stdout.contains("OAuth token: ********"),
                    "{provider}: {stdout}"
                );
                if provider == "google-gemini-cli" {
                    assert!(stdout.contains("Google account: fixture@example.invalid"));
                    assert!(stdout.contains("Google project_id: fixture-project"));
                }
            } else {
                assert!(stdout.contains("OAuth detail:"), "{provider}: {stdout}");
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn auth_pool_imports_local_oauth_state_and_clears_it_when_removed() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("import-fixture.json");
    let state_path = sandbox.path().join("fixture-state.json");
    let access = "fixture-local-oauth-access";
    let refresh = "fixture-local-oauth-refresh";
    let source_state = serde_json::json!({
        "claudeAiOauth": {
            "accessToken": access,
            "refreshToken": refresh,
            "expiresAt": jiff::Timestamp::now().as_millisecond() + 3_600_000
        }
    })
    .to_string();
    std::fs::write(&source, &source_state).unwrap();

    for (args, expected) in [
        (
            &[
                "auth",
                "add",
                "anthropic",
                "--type",
                "oauth",
                "--label",
                "fixture-entry",
            ][..],
            "Added Anthropic OAuth credential (label='fixture-entry'",
        ),
        (&["auth", "list", "anthropic"], "anthropic (1 credentials):"),
        (
            &["auth", "remove", "anthropic", "fixture-entry"],
            "Removed pooled credential for provider 'anthropic' (label='fixture-entry'",
        ),
        (
            &["auth", "list", "anthropic"],
            "No pooled credentials configured.",
        ),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            isolated_hermes(sandbox.path())
                .env("HERMES_ANTHROPIC_OAUTH_FILE", &source)
                .env("HERMES_AUTH_FILE", &state_path)
                // Reject browser setup before it can launch if discovery regresses.
                .env(
                    "HERMES_ANTHROPIC_OAUTH_AUTHORIZE_URL",
                    "invalid-fixture-url",
                )
                .env("HERMES_ANTHROPIC_OAUTH_TOKEN_URL", "http://127.0.0.1:1")
                .args(args)
                .output(),
        )
        .await
        .expect("offline OAuth command timed out")
        .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stdout.contains(expected), "{args:?}: {stdout}");
        assert!(!stdout.contains(access) && !stdout.contains(refresh));
        assert!(!stderr.contains(access) && !stderr.contains(refresh));
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        if args[1] == "add" {
            assert_eq!(saved["providers"]["anthropic"]["access_token"], access);
            assert_eq!(saved["providers"]["anthropic"]["refresh_token"], refresh);
        } else if args[1] == "remove" {
            assert!(saved["providers"].get("anthropic").is_none());
            assert!(saved.get("active_provider").is_none());
        }
    }
    assert_eq!(std::fs::read_to_string(source).unwrap(), source_state);
}

#[tokio::test]
async fn weixin_login_restores_a_saved_account_after_local_logout() {
    let sandbox = tempfile::tempdir().unwrap();
    let account = "weixin-fixture";
    let fixture_token = "hermes-weixin-cli-fixture";
    let account_path = sandbox
        .path()
        .join("hermes/weixin/accounts/weixin-fixture.json");
    let login = |supply_account| {
        let mut command = isolated_hermes(sandbox.path());
        command
            .args(["auth", "login", "weixin"])
            .stdin(std::process::Stdio::null());
        if supply_account {
            command
                .env("WEIXIN_ACCOUNT_ID", format!("  {account}  "))
                .env("WEIXIN_TOKEN", format!("  {fixture_token}  "));
        }
        command
    };
    let first = tokio::time::timeout(Duration::from_secs(15), login(true).output())
        .await
        .expect("Weixin login timed out")
        .unwrap();
    assert!(first.status.success(), "{first:?}");
    assert!(!String::from_utf8_lossy(&first.stdout).contains(fixture_token));
    let saved = std::fs::read(&account_path).unwrap();
    let account_state: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(account_state["token"], fixture_token);

    for (action, expected) in [
        (
            "status",
            "cfg_token_present=true saved_token_present=true enabled=true",
        ),
        ("logout", "token cleared and platform disabled"),
        (
            "status",
            "cfg_token_present=false saved_token_present=true enabled=false",
        ),
    ] {
        let stdout = platform_auth(sandbox.path(), action, "weixin").await;
        assert!(stdout.contains(expected), "{stdout}");
        assert!(!stdout.contains(fixture_token));
    }
    assert_eq!(std::fs::read(account_path).unwrap(), saved);

    let restored = tokio::time::timeout(Duration::from_secs(15), login(false).output())
        .await
        .expect("Weixin saved-account login timed out")
        .unwrap();
    assert!(restored.status.success(), "{restored:?}");
    let config =
        hermes_config::load_user_config_file(&sandbox.path().join("hermes/config.yaml")).unwrap();
    assert!(config.platforms["weixin"].enabled);
    assert_eq!(
        config.platforms["weixin"].token.as_deref(),
        Some(fixture_token)
    );
    assert_eq!(config.platforms["weixin"].extra["account_id"], account);
}

#[tokio::test]
async fn weixin_qr_login_refreshes_an_expired_code_and_saves_the_confirmed_account() {
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let sandbox = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let base_url = server.uri();
    let config_path = sandbox.path().join("hermes/config.yaml");
    std::fs::create_dir(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        format!(
            "platforms:\n  weixin:\n    enabled: false\n    base_url: {base_url}\n    qr_start_endpoint: /start\n    qr_poll_endpoint: /poll\n"
        ),
    )
    .unwrap();

    let starts = AtomicUsize::new(0);
    let display_url = format!("{base_url}/fixture-display");
    Mock::given(method("GET"))
        .and(path("/start"))
        .and(query_param("bot_type", "3"))
        .respond_with(move |_: &wiremock::Request| {
            let reply = match starts.fetch_add(1, Ordering::Relaxed) {
                0 => json!({"qrcode": "expired-fixture"}),
                1 => json!({"qrcode": "renewed-fixture", "qrcode_img_content": display_url}),
                _ => return ResponseTemplate::new(500),
            };
            ResponseTemplate::new(200).set_body_json(reply)
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/poll"))
        .and(query_param("qrcode", "expired-fixture"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "expired"})))
        .expect(1)
        .mount(&server)
        .await;
    let fixture_token = "hermes-weixin-qr-fixture";
    let polls = AtomicUsize::new(0);
    Mock::given(method("GET"))
        .and(path("/poll"))
        .and(query_param("qrcode", "renewed-fixture"))
        .respond_with(move |_: &wiremock::Request| {
            let reply = match polls.fetch_add(1, Ordering::Relaxed) {
                0 => json!({"status": "scaned"}),
                1 => json!({
                    "status": "confirmed",
                    "ilink_bot_id": "qr-account-fixture",
                    "bot_token": fixture_token,
                    "ilink_user_id": "qr-user-fixture"
                }),
                _ => return ResponseTemplate::new(500),
            };
            ResponseTemplate::new(200).set_body_json(reply)
        })
        .expect(2)
        .mount(&server)
        .await;

    let output = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(sandbox.path())
            .env("WEIXIN_BASE_URL", &base_url)
            .args(["auth", "login", "weixin", "--qr"])
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .expect("fixture QR login timed out")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("account_id/token saved and platform enabled"));
    assert!(!stdout.contains(fixture_token));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(fixture_token));
    let config = hermes_config::load_user_config_file(&config_path).unwrap();
    let platform = &config.platforms["weixin"];
    assert!(platform.enabled);
    assert_eq!(platform.extra["account_id"], "qr-account-fixture");
    assert_eq!(platform.token.as_deref(), Some(fixture_token));
    let saved = sandbox
        .path()
        .join("hermes/weixin/accounts/qr-account-fixture.json");
    let saved: Value = serde_json::from_slice(&std::fs::read(saved).unwrap()).unwrap();
    assert_eq!(saved["token"], fixture_token);
    assert_eq!(saved["user_id"], "qr-user-fixture");
    assert_eq!(saved["base_url"], base_url);
    server.verify().await;
    let requests = server.received_requests().await.unwrap();
    let path_and_query: Vec<_> = requests
        .iter()
        .map(|request| (request.url.path(), request.url.query().unwrap_or_default()))
        .collect();
    assert_eq!(
        path_and_query,
        [
            ("/start", "bot_type=3"),
            ("/poll", "qrcode=expired-fixture"),
            ("/start", "bot_type=3"),
            ("/poll", "qrcode=renewed-fixture"),
            ("/poll", "qrcode=renewed-fixture"),
        ]
    );
}

#[tokio::test]
async fn qqbot_manual_login_uses_local_credentials_and_preserves_markdown_settings() {
    let sandbox = tempfile::tempdir().unwrap();
    let config_path = sandbox.path().join("hermes/config.yaml");
    std::fs::create_dir(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        "platforms:\n  qqbot:\n    enabled: false\n    markdown_support: false\n",
    )
    .unwrap();
    let app_id = "qqbot-app-fixture";
    let fixture_secret = "hermes-qqbot-cli-fixture";
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(sandbox.path())
            .args(["auth", "login", "qqbot"])
            .env("QQ_APP_ID", format!("  {app_id}  "))
            .env("QQ_CLIENT_SECRET", format!("  {fixture_secret}  "))
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .expect("QQBot login timed out")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("QQBot: app_id/client_secret saved and platform enabled"));
    assert!(!stdout.contains(fixture_secret));
    let config = hermes_config::load_user_config_file(&config_path).unwrap();
    let platform = &config.platforms["qqbot"];
    assert!(platform.enabled);
    assert_eq!(platform.extra["app_id"], app_id);
    assert_eq!(platform.extra["client_secret"], fixture_secret);
    assert_eq!(platform.extra["markdown_support"], false);
}

#[tokio::test]
async fn auth_matrix_reports_presence_and_source_without_exposing_values() {
    let sandbox = tempfile::tempdir().expect("temporary CLI home");
    let home = sandbox.path().join("hermes");
    std::fs::create_dir(&home).expect("fixture CLI home");
    let fixture = "plain-local-matrix-fixture";
    std::fs::write(
        home.join("config.yaml"),
        format!(
            "platforms:\n  slack:\n    enabled: true\n    token: {fixture}\n  telegram:\n    enabled: false\n"
        ),
    )
    .expect("write fixture configuration");
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(sandbox.path())
            .env("OPENAI_API_KEY", fixture)
            .env("TELEGRAM_BOT_TOKEN", fixture)
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .args(["auth", "status", "all"])
            .output(),
    )
    .await
    .expect("auth matrix timeout")
    .expect("run auth matrix");
    assert!(
        output.status.success(),
        "auth matrix exited with {}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).expect("auth matrix output");
    assert!(stdout.contains("Auth status matrix:"));
    assert!(
        !stdout.contains(fixture),
        "matrix displayed the fixture value"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains(fixture),
        "matrix diagnostics displayed the fixture value"
    );
    for (provider, fields) in [
        ("openai", "present=true source=env"),
        ("telegram", "present=true source=env enabled=false"),
        ("slack", "present=true source=config enabled=true"),
        ("discord", "present=false source=none enabled=false"),
    ] {
        let row = stdout
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("- "))
            .find(|line| line.split_whitespace().next() == Some(provider))
            .unwrap_or_else(|| panic!("matrix is missing {provider}"));
        assert!(row.contains(fields), "unexpected {provider} matrix row");
    }
}

async fn platform_auth(sandbox: &std::path::Path, action: &str, platform: &str) -> String {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        isolated_hermes(sandbox)
            .args(["auth", action, platform])
            .output(),
    )
    .await
    .expect("platform auth timeout")
    .expect("run platform auth");
    assert!(
        output.status.success(),
        "auth {action} {platform}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("platform auth stdout")
}

#[tokio::test]
async fn platform_auth_status_and_logout_update_the_isolated_profile() {
    use std::fmt::Write as _;

    let sandbox = tempfile::tempdir().expect("temporary CLI home");
    let home = sandbox.path().join("hermes");
    std::fs::create_dir(&home).expect("fixture CLI home");
    let config = home.join("config.yaml");
    let platforms = [
        ("telegram", "token_present"),
        ("weixin", "cfg_token_present"),
        ("discord", "credential_present"),
    ];

    for (platform, presence) in platforms {
        let stdout = platform_auth(sandbox.path(), "status", platform).await;
        assert!(stdout.contains(&format!("{presence}=false")), "{stdout}");
        assert!(stdout.contains("enabled=false"), "{stdout}");
    }

    let fixture = "plain-local-test-value";
    let mut yaml = String::from("platforms:\n");
    for (platform, _) in platforms {
        writeln!(
            yaml,
            "  {platform}:\n    enabled: true\n    token: {fixture}"
        )
        .expect("fixture configuration");
    }
    std::fs::write(&config, yaml).expect("write fixture configuration");
    for (platform, presence) in platforms {
        let stdout = platform_auth(sandbox.path(), "status", platform).await;
        assert!(stdout.contains(&format!("{presence}=true")), "{stdout}");
        assert!(stdout.contains("enabled=true"), "{stdout}");
        assert!(
            !stdout.contains(fixture),
            "status displayed the fixture value"
        );

        let stdout = platform_auth(sandbox.path(), "logout", platform).await;
        assert!(stdout.contains("cleared"), "{stdout}");
        assert!(
            !stdout.contains(fixture),
            "logout displayed the fixture value"
        );

        let stdout = platform_auth(sandbox.path(), "status", platform).await;
        assert!(stdout.contains(&format!("{presence}=false")), "{stdout}");
        assert!(stdout.contains("enabled=false"), "{stdout}");
    }
    let config = hermes_config::load_user_config_file(&config).expect("saved configuration");
    for (platform, _) in platforms {
        let row = &config.platforms[platform];
        assert!(!row.enabled, "{platform}");
        assert!(row.token.is_none(), "{platform}");
    }
}

#[tokio::test]
async fn gemini_verify_persists_the_cached_credential_without_contacting_the_provider() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("gemini-oauth.json");
    let state = sandbox.path().join("hermes-oauth.json");
    let endpoint = MockServer::start().await;
    let endpoint_url = format!("{}/token", endpoint.uri());
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(503))
        .expect(0)
        .mount(&endpoint)
        .await;

    let access = "synthetic-gemini-access";
    let cached = serde_json::json!({
        "access": access,
        "expires": 4_102_444_800_000_i64,
        "project_id": "fixture-project"
    })
    .to_string();

    for (fixture, success, expected) in [
        (
            None,
            false,
            "status=missing, source=none, credential_present=false",
        ),
        (
            Some(cached.as_str()),
            true,
            "status=valid, source=google-oauth, credential_present=true",
        ),
        (
            Some("[]"),
            false,
            "status=refresh_failed, source=env, credential_present=true",
        ),
    ] {
        if let Some(fixture) = fixture {
            std::fs::write(&source, fixture).unwrap();
        }
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            isolated_hermes(sandbox.path())
                .env("HERMES_GEMINI_OAUTH_FILE", &source)
                .env("HERMES_GEMINI_OAUTH_TOKEN_URL", &endpoint_url)
                .env("HERMES_AUTH_FILE", &state)
                .args(["auth", "verify", "google-gemini-cli"])
                .output(),
        )
        .await
        .expect("offline Gemini verification timed out")
        .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.success(), success, "{stdout}\n{stderr}");
        assert!(stdout.contains(expected), "{stdout}");
        assert!(!stdout.contains(access) && !stderr.contains(access));
        if success {
            assert!(stdout.contains("expires_at=2100-01-01T00:00:00+00:00"));
            assert_eq!(std::fs::read_to_string(&source).unwrap(), cached);
        }
    }
    assert!(endpoint.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn qwen_verification_omits_untrusted_refresh_error_details() {
    const FIXTURE_MARKER: &str = "loopback-provider-diagnostic-marker";
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant",
            "error_description": FIXTURE_MARKER,
        })))
        .expect(1)
        .mount(&endpoint)
        .await;

    let sandbox = tempfile::tempdir().expect("temporary CLI home");
    let fixture = sandbox.path().join("fixture.json");
    std::fs::write(
        &fixture,
        r#"{"access_token":"expired-fixture","refresh_token":"refresh-fixture","expiry_date":1}"#,
    )
    .expect("write synthetic Qwen state");

    let output = tokio::time::timeout(
        Duration::from_secs(25),
        isolated_hermes(sandbox.path())
            .env("HERMES_QWEN_CLI_AUTH_FILE", &fixture)
            .env(
                "HERMES_QWEN_OAUTH_TOKEN_URL",
                format!("{}/token", endpoint.uri()),
            )
            .env("HERMES_QWEN_OAUTH_CLIENT_ID", "loopback-fixture")
            .args(["auth", "verify", "qwen-oauth"])
            .output(),
    )
    .await
    .expect("CLI timeout")
    .expect("run CLI");
    assert!(
        !output.status.success(),
        "the CLI accepted a rejected refresh"
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(stdout.contains("Auth verify: provider='qwen-oauth', status=missing"));
    assert!(
        !stdout.contains(FIXTURE_MARKER) && !stderr.contains(FIXTURE_MARKER),
        "the CLI displayed the response's free-form description"
    );
    assert!(stdout.contains("Check the provider's OAuth sign-in and try again."));
}

#[tokio::test]
async fn qwen_login_and_pool_import_use_the_isolated_cli_state() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("qwen-fixture.json");
    let state_path = sandbox.path().join("hermes-auth-fixture.json");
    let access = "synthetic-qwen-access";
    let refresh = "synthetic-qwen-refresh";
    let expires = jiff::Timestamp::now().as_millisecond() + 3_600_000;
    let source_contents = serde_json::json!({
        "access_token": access,
        "refresh_token": refresh,
        "expiry_date": expires,
    })
    .to_string();
    std::fs::write(&source, &source_contents).unwrap();

    let endpoint = MockServer::start().await;
    assert!(endpoint.address().ip().is_loopback());
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&endpoint)
        .await;

    for (args, expected) in [
        (
            &["auth", "login", "qwen-oauth"][..],
            "stored as provider 'qwen-oauth'",
        ),
        (
            &[
                "auth",
                "add",
                "qwen-oauth",
                "--type",
                "oauth",
                "--label",
                "field-notes",
            ],
            "Added Qwen OAuth credential (label='field-notes'",
        ),
        (
            &["auth", "list", "qwen-oauth"],
            "qwen-oauth (1 credentials):",
        ),
        (
            &["auth", "verify", "qwen-oauth"],
            "source=qwen-cli, credential_present=true, oauth_state_present=true",
        ),
        (&["secrets", "list"], "qwen-oauth"),
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            isolated_hermes(sandbox.path())
                .env("HERMES_QWEN_CLI_AUTH_FILE", &source)
                .env("HERMES_AUTH_FILE", &state_path)
                .env(
                    "HERMES_QWEN_OAUTH_TOKEN_URL",
                    format!("{}/token", endpoint.uri()),
                )
                .env("HERMES_QWEN_BASE_URL", endpoint.uri())
                .stdin(std::process::Stdio::null())
                .args(args)
                .output(),
        )
        .await
        .expect("offline Qwen CLI timeout")
        .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{args:?}: {stdout}\n{stderr}");
        assert!(stdout.contains(expected), "{args:?}: {stdout}");
        assert!(!stdout.contains(access) && !stdout.contains(refresh));
        assert!(!stderr.contains(access) && !stderr.contains(refresh));

        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        let state = &saved["providers"]["qwen-oauth"];
        assert_eq!(state["access_token"], access);
        assert_eq!(state["refresh_token"], refresh);
        assert_eq!(state["expiry_date"], expires);
    }
    assert_eq!(std::fs::read_to_string(&source).unwrap(), source_contents);

    let vault = sandbox
        .path()
        .join("hermes")
        .join("auth")
        .join("tokens.json");
    let stored = std::fs::read_to_string(vault).unwrap();
    assert!(!stored.is_empty());
    assert!(!stored.contains(access) && !stored.contains(refresh));
}
