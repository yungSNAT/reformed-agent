#[test]
fn default_auth_path_and_project_environment_precedence() {
    const TEST: &str = "default_auth_path_and_project_environment_precedence";
    const EXPECTED_PROJECT: &str = "HERMES_GEMINI_COVERAGE_EXPECTED_PROJECT";
    if in_child(TEST) {
        assert_eq!(
            gemini_cli_auth_path(),
            hermes_config::paths::hermes_home()
                .join("auth")
                .join("google_oauth.json")
        );
        assert_eq!(
            resolve_gemini_project_id_from_env(),
            std::env::var(EXPECTED_PROJECT).ok()
        );
        return;
    }

    let home = tempfile::tempdir().expect("temporary home");
    for (hermes, cloud, cloud_id, expected) in [
        (" hermes ", " cloud ", " cloud-id ", Some("hermes")),
        (" \t ", " cloud ", " cloud-id ", Some("cloud")),
        ("", "", " cloud-id ", Some("cloud-id")),
        ("", "", "", None),
    ] {
        let mut command = child_command(TEST);
        command
            .env("HERMES_HOME", home.path())
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("HERMES_GEMINI_OAUTH_FILE", " \t ")
            .env("HERMES_GEMINI_PROJECT_ID", hermes)
            .env("GOOGLE_CLOUD_PROJECT", cloud)
            .env("GOOGLE_CLOUD_PROJECT_ID", cloud_id)
            .env_remove(EXPECTED_PROJECT);
        if let Some(expected) = expected {
            command.env(EXPECTED_PROJECT, expected);
        }
        assert_child_success(&command.output().expect("run environment test"));
    }
}

#[tokio::test]
async fn credential_file_errors_status_and_cached_runtime_credentials() {
    const TEST: &str = "credential_file_errors_status_and_cached_runtime_credentials";
    if !in_child(TEST) {
        let temp = tempfile::tempdir().expect("temporary credentials directory");
        let path = temp.path().join("oauth.json");
        let output = child_command(TEST)
            .env(
                "HERMES_GEMINI_OAUTH_FILE",
                format!("  {}  ", path.display()),
            )
            .env("HERMES_GEMINI_OAUTH_TOKEN_URL", "not-a-url")
            .output()
            .expect("run credential file test");
        assert_child_success(&output);
        return;
    }

    let path = gemini_cli_auth_path();
    assert_eq!(
        path.file_name().and_then(std::ffi::OsStr::to_str),
        Some("oauth.json")
    );
    let missing = assert_auth_failure(
        read_gemini_cli_state(),
        "Google OAuth credentials not found.",
    );
    assert!(missing.contains("hermes auth google-gemini-cli"));
    let status = get_gemini_oauth_auth_status().await;
    assert!(!status.logged_in);
    assert_eq!(status.auth_file, path);
    assert!(status.source.is_none());
    assert!(status.api_key.is_none());
    assert!(status.expires_at_ms.is_none());
    assert!(status.email.is_none());
    assert!(status.project_id.is_none());
    assert!(status.error.is_some_and(|error| error.contains(&missing)));

    std::fs::create_dir(&path).expect("put a directory at the credential path");
    assert!(
        matches!(read_gemini_cli_state(), Err(AgentError::Io(message))
        if message.contains(&path.display().to_string()))
    );
    std::fs::remove_dir(&path).expect("remove credential directory");

    std::fs::write(&path, "[").expect("write malformed JSON");
    assert!(
        matches!(read_gemini_cli_state(), Err(AgentError::Config(message))
        if message.starts_with("parse "))
    );
    std::fs::write(&path, "[]").expect("write non-object JSON");
    assert!(
        matches!(read_gemini_cli_state(), Err(AgentError::Config(message))
        if message == format!("invalid Google OAuth credentials in {}", path.display()))
    );

    let expiry = jiff::Timestamp::now().as_millisecond() + 86_400_000;
    let payload = serde_json::json!({
        "refresh": " \t ",
        "refresh_token": " original | packed-project | packed-managed ",
        "project_id": " explicit-project ",
        "access_token": " cached-access ",
        "expires_at_ms": expiry,
        "email": " cache@example.test "
    });
    std::fs::write(&path, payload.to_string()).expect("write cached credentials");
    let state = read_gemini_cli_state().expect("read legacy credential fields");
    assert_eq!(
        state.refresh.as_deref(),
        Some("original|explicit-project|packed-managed")
    );
    assert_eq!(state.managed_project_id.as_deref(), Some("packed-managed"));

    let status = get_gemini_oauth_auth_status().await;
    assert!(status.logged_in);
    assert_eq!(status.auth_file, path);
    assert_eq!(status.source.as_deref(), Some("google-oauth"));
    assert_eq!(status.api_key.as_deref(), Some("cached-access"));
    assert_eq!(status.expires_at_ms, Some(expiry));
    assert_eq!(status.email.as_deref(), Some("cache@example.test"));
    assert_eq!(status.project_id.as_deref(), Some("explicit-project"));
    assert!(status.error.is_none());

    let credentials = resolve_gemini_oauth_runtime_credentials(false)
        .await
        .expect("use the cached access token");
    assert_eq!(credentials.provider, "google-gemini-cli");
    assert_eq!(credentials.base_url, DEFAULT_GEMINI_CLOUDCODE_BASE_URL);
    assert_eq!(credentials.source, "google-oauth");
    assert_eq!(credentials.api_key, "cached-access");
    assert_eq!(credentials.auth_file, path);
    assert_eq!(credentials.expires_at_ms, Some(expiry));
    assert_eq!(credentials.email.as_deref(), Some("cache@example.test"));
    assert_eq!(credentials.project_id.as_deref(), Some("explicit-project"));
    assert_eq!(credentials.refresh_token.as_deref(), Some("original"));

    assert_auth_failure(
        resolve_gemini_oauth_runtime_credentials(true).await,
        "Google OAuth refresh failed:",
    );

    let payload = serde_json::json!({"access": " \t ", "expires": expiry});
    std::fs::write(&path, payload.to_string()).expect("write credentials without tokens");
    let status = get_gemini_oauth_auth_status().await;
    assert!(!status.logged_in);
    assert_eq!(status.source.as_deref(), Some("google-oauth"));
    assert!(status.error.is_none());
    assert_auth_failure(
        resolve_gemini_oauth_runtime_credentials(false).await,
        "Google OAuth access token missing.",
    );
    assert_auth_failure(
        resolve_gemini_oauth_runtime_credentials(true).await,
        "Google OAuth refresh token missing.",
    );
}

const REFRESH_ERRORS: [(&str, u16, &str, &str); 4] = [
    (
        "denied",
        400,
        "fixture refresh rejected",
        "Google OAuth refresh failed (400",
    ),
    (
        "broken-json",
        200,
        "{",
        "Google OAuth refresh JSON parse failed:",
    ),
    (
        "not-object",
        200,
        "[]",
        "Google OAuth refresh response is not a JSON object",
    ),
    (
        "blank-access",
        200,
        r#"{"access_token":"   "}"#,
        "Google OAuth refresh response missing access_token",
    ),
];

fn refresh_fixture(token: &str) -> GeminiOAuthFileState {
    GeminiOAuthFileState {
        refresh: Some(format!("{token}|fixture-project|fixture-managed")),
        access: Some("old-access".into()),
        expires: Some(jiff::Timestamp::now().as_millisecond() + 86_400_000),
        email: Some("refresh@example.test".into()),
        project_id: Some("fixture-project".into()),
        managed_project_id: Some("fixture-managed".into()),
    }
}

#[tokio::test]
async fn refresh_uses_mock_endpoint_and_persists_updated_credentials() {
    const TEST: &str = "refresh_uses_mock_endpoint_and_persists_updated_credentials";
    if !in_child(TEST) {
        use wiremock::matchers::{body_string_contains, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let temp = tempfile::tempdir().expect("temporary refresh directory");
        let server = MockServer::start().await;
        let successes = [
            (
                "rotate",
                200,
                r#"{"access_token":" new-access ","refresh_token":" new-refresh ","expires_in":5}"#,
            ),
            ("retain", 200, r#"{"access_token":" retained-access "}"#),
        ];
        for (token, status, body) in successes.into_iter().chain(
            REFRESH_ERRORS
                .into_iter()
                .map(|(token, status, body, _)| (token, status, body)),
        ) {
            Mock::given(method("POST"))
                .and(path("/token"))
                .and(header("accept", "application/json"))
                .and(body_string_contains("grant_type=refresh_token"))
                .and(body_string_contains("client_id=fixture-client"))
                .and(body_string_contains("client_secret=fixture-secret"))
                .and(body_string_contains(format!("refresh_token={token}")))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .expect(1)
                .mount(&server)
                .await;
        }

        let mut command = child_command(TEST);
        command
            .env("HERMES_GEMINI_OAUTH_FILE", temp.path().join("oauth.json"))
            .env(
                "HERMES_GEMINI_OAUTH_TOKEN_URL",
                format!(" {}/token ", server.uri()),
            )
            .env("HERMES_GEMINI_CLIENT_ID", " fixture-client ")
            .env("HERMES_GEMINI_CLIENT_SECRET", " fixture-secret ")
            .env("NO_PROXY", "*")
            .env("no_proxy", "*");
        let output = tokio::task::spawn_blocking(move || command.output())
            .await
            .expect("join child launcher")
            .expect("run refresh test");
        assert_child_success(&output);
        server.verify().await;
        return;
    }

    let path = save_gemini_cli_state(&refresh_fixture("rotate")).expect("write refresh fixture");
    let before = jiff::Timestamp::now().as_millisecond();
    let credentials = resolve_gemini_oauth_runtime_credentials(true)
        .await
        .expect("refresh cached credentials");
    let after = jiff::Timestamp::now().as_millisecond();
    let expiry = credentials.expires_at_ms.expect("refreshed expiration");
    assert!((before + 60_000..=after + 60_000).contains(&expiry));
    assert_eq!(credentials.api_key, "new-access");
    assert_eq!(credentials.refresh_token.as_deref(), Some("new-refresh"));
    assert_eq!(credentials.project_id.as_deref(), Some("fixture-project"));
    assert_eq!(credentials.email.as_deref(), Some("refresh@example.test"));
    assert_eq!(credentials.auth_file, path);

    let saved: Value = serde_json::from_slice(&std::fs::read(&path).expect("read refreshed file"))
        .expect("parse refreshed file");
    assert_eq!(
        saved["refresh"],
        "new-refresh|fixture-project|fixture-managed"
    );
    assert_eq!(saved["access"], "new-access");
    assert_eq!(saved["expires"].as_i64(), Some(expiry));
    assert_eq!(saved["email"], "refresh@example.test");
    assert_eq!(saved["project_id"], "fixture-project");
    assert_eq!(saved["managed_project_id"], "fixture-managed");

    let before = jiff::Timestamp::now().as_millisecond();
    let retained = refresh_gemini_cli_state(&refresh_fixture("retain"), f64::NAN)
        .await
        .expect("refresh without a replacement refresh token");
    let after = jiff::Timestamp::now().as_millisecond();
    assert_eq!(retained.access.as_deref(), Some("retained-access"));
    assert_eq!(
        retained.refresh.as_deref(),
        Some("retain|fixture-project|fixture-managed")
    );
    assert!((before + 3_600_000..=after + 3_600_000)
        .contains(&retained.expires.expect("default expiration")));

    for (token, status, body, expected) in REFRESH_ERRORS {
        let message = assert_auth_failure(
            refresh_gemini_cli_state(&refresh_fixture(token), 5.0).await,
            expected,
        );
        if status == 400 {
            assert!(message.contains(body));
        } else if token != "broken-json" {
            assert_eq!(message, expected);
        }
    }
}
