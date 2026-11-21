use std::time::Duration;

use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
        Command::new(env!("CARGO_BIN_EXE_hermes-agent-ultra"))
            .env_clear()
            .env("HOME", sandbox.path())
            .env("USERPROFILE", sandbox.path())
            .env("HERMES_HOME", sandbox.path().join("hermes"))
            .env("HERMES_QWEN_CLI_AUTH_FILE", &fixture)
            .env(
                "HERMES_QWEN_OAUTH_TOKEN_URL",
                format!("{}/token", endpoint.uri()),
            )
            .env("HERMES_QWEN_OAUTH_CLIENT_ID", "loopback-fixture")
            .current_dir(sandbox.path())
            .kill_on_drop(true)
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
