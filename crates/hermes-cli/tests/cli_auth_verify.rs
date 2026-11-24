use std::time::Duration;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[path = "support/isolated_hermes.rs"]
mod isolated;
use isolated::isolated_hermes;

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
