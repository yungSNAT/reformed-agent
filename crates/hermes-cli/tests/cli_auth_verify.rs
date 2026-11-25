use std::time::Duration;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[path = "support/isolated_hermes.rs"]
mod isolated;
use isolated::isolated_hermes;

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
