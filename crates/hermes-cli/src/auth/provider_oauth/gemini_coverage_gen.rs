use super::*;
use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};

const CHILD_TEST: &str = "HERMES_GEMINI_COVERAGE_CHILD";

fn in_child(test: &str) -> bool {
    std::env::var(CHILD_TEST).is_ok_and(|value| value == test)
}

// Set environment overrides on the child, not on the parallel test runner.
fn child_command(test: &str) -> Command {
    let (_, module) = module_path!().split_once("::").expect("test module path");
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", &format!("{module}::{test}"), "--nocapture"])
        .env(CHILD_TEST, test);
    command
}

fn assert_child_success(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("running 1 test"),
        "child status: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
}

fn assert_auth_failure<T>(result: Result<T, AgentError>, expected_prefix: &str) -> String {
    let Err(AgentError::AuthFailed(message)) = result else {
        panic!("expected AuthFailed: {expected_prefix}");
    };
    assert!(
        message.starts_with(expected_prefix),
        "expected {expected_prefix:?}, got {message:?}"
    );
    message
}

#[test]
fn empty_packed_refresh_and_plain_token() {
    assert_eq!(parse_packed_gemini_refresh(None), (None, None, None));
    assert_eq!(
        parse_packed_gemini_refresh(Some(" \t\n ")),
        (None, None, None)
    );
    assert_eq!(
        pack_gemini_refresh(Some(" token "), Some(" "), None).as_deref(),
        Some("token")
    );
}

#[test]
fn default_client_configuration_and_timeout() {
    assert_eq!(
        default_gemini_client_id(),
        format!(
            "{DEFAULT_GEMINI_CLIENT_ID_PROJECT_NUM}-{DEFAULT_GEMINI_CLIENT_ID_HASH}.apps.googleusercontent.com"
        )
    );
    assert_eq!(
        default_gemini_client_secret(),
        format!("GOCSPX-{DEFAULT_GEMINI_CLIENT_SECRET_SUFFIX}")
    );
    assert_eq!(default_http_timeout_seconds(f64::NAN, 20.0), 20.0);
    assert_eq!(default_http_timeout_seconds(f64::INFINITY, 15.0), 15.0);
    assert_eq!(default_http_timeout_seconds(1.0, 20.0), 5.0);
    assert_eq!(default_http_timeout_seconds(150.0, 20.0), 120.0);
    assert_eq!(default_http_timeout_seconds(12.0, 20.0), 12.0);
}

#[test]
fn pkce_and_state_tokens_use_unpadded_base64url() {
    let (verifier, challenge) = build_oauth_pkce_pair();
    assert_eq!(verifier.len(), 43);
    assert_eq!(challenge.len(), 43);
    let verifier_bytes =
        base64::Engine::decode(&BASE64_URL_SAFE_NO_PAD, &verifier).expect("base64url verifier");
    let challenge_bytes =
        base64::Engine::decode(&BASE64_URL_SAFE_NO_PAD, &challenge).expect("base64url challenge");
    let digest = <Sha256 as sha2::Digest>::digest(verifier.as_bytes());
    assert_eq!(challenge_bytes.as_slice(), &digest[..]);
    assert_eq!(verifier_bytes.len(), 32);
    assert_ne!(verifier, build_oauth_pkce_pair().0);

    let state = build_oauth_state_token();
    assert_eq!(state.len(), 22);
    assert_eq!(
        base64::Engine::decode(&BASE64_URL_SAFE_NO_PAD, &state)
            .expect("base64url state")
            .len(),
        16
    );
    assert_ne!(state, build_oauth_state_token());
}

#[test]
fn manual_callback_input_accepts_codes_and_ignores_empty_parameters() {
    for (input, expected) in [
        ("  raw-code  ", Some("raw-code")),
        (" \t\n", None),
        (
            " https://localhost/callback?state=x&code=+++&code=%20a%2Bb%20 ",
            Some("a+b"),
        ),
        ("http://localhost/callback?state=x&code=++", None),
        ("http://[", None),
        ("?error=denied&code=+&code=%20next%20", Some("next")),
        ("?code=++&state=x", None),
    ] {
        assert_eq!(
            parse_code_from_manual_input(input).as_deref(),
            expected,
            "input: {input:?}"
        );
    }
}

#[test]
fn manual_prompt_uses_piped_input_without_opening_browser() {
    const TEST: &str = "manual_prompt_uses_piped_input_without_opening_browser";
    const PROMPT: &str = "Coverage callback prompt: ";
    if in_child(TEST) {
        maybe_open_browser("about:blank", false);
        let line = prompt_line_blocking(PROMPT).expect("read piped input");
        assert_eq!(line, "?state=expected&code=example");
        assert_eq!(
            parse_code_from_manual_input(&line).as_deref(),
            Some("example")
        );
        return;
    }

    let mut child = child_command(TEST)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn prompt test");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"  ?state=expected&code=example \r\n")
        .expect("write callback input");
    let output = child.wait_with_output().expect("wait for prompt test");
    assert_child_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(PROMPT));
    assert!(!stdout.contains("Opened browser"));
    assert!(!stdout.contains("Could not open browser"));
}

fn queue_callback(listener: &TcpListener, target: &str) -> std::net::TcpStream {
    let mut client = std::net::TcpStream::connect(listener.local_addr().expect("callback address"))
        .expect("connect to loopback callback listener");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("bound response read");
    let request = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n");
    client.write_all(request.as_bytes()).expect("send callback");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("finish callback request");
    client
}

fn assert_callback_response(mut client: std::net::TcpStream, status: &str, body: &str) {
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .expect("read callback response");
    let (headers, actual_body) = response.split_once("\r\n\r\n").expect("HTTP response");
    let mut lines = headers.lines();
    assert_eq!(lines.next(), Some(format!("HTTP/1.1 {status}").as_str()));
    assert!(headers.contains("\r\nContent-Type: text/html; charset=utf-8\r\n"));
    assert!(headers.contains(&format!("\r\nContent-Length: {}\r\n", body.len())));
    assert!(headers.ends_with("\r\nConnection: close"));
    assert_eq!(actual_body, body);
}

#[test]
fn callback_ignores_unusable_requests_until_a_valid_code_arrives() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback callback");
    let invalid = queue_callback(&listener, ":not-a-port");
    let wrong_path = queue_callback(&listener, "/gemini-coverage-not-the-callback");
    let no_code = queue_callback(
        &listener,
        &format!("{GEMINI_CALLBACK_PATH}?state=expected&code=+++"),
    );
    let valid = queue_callback(
        &listener,
        &format!(
            "{GEMINI_CALLBACK_PATH}?ignored=x&code=+&code=%20test%2Bcode%20&state=%20expected%20&error=+"
        ),
    );
    assert_eq!(
        wait_for_gemini_oauth_callback(&listener, "expected", 1)
            .expect("valid callback")
            .as_deref(),
        Some("test+code")
    );
    assert_callback_response(
        invalid,
        "400 Bad Request",
        "<html><body><h1>Invalid callback</h1></body></html>",
    );
    assert_callback_response(
        wrong_path,
        "404 Not Found",
        "<html><body><h1>Not found</h1></body></html>",
    );
    assert_callback_response(
        no_code,
        "400 Bad Request",
        "<html><body><h1>Missing authorization code</h1></body></html>",
    );
    assert_callback_response(
        valid,
        "200 OK",
        "<html><body><h1>Signed in to Google</h1><p>You can close this tab and return to Hermes.</p></body></html>",
    );
}

#[test]
fn callback_rejects_provider_errors_and_incorrect_state() {
    for (query, error, body) in [
        (
            "code=unused&state=wrong&error=%20consent%20d%C3%A9clined%20",
            "Google OAuth authorization failed: consent déclined",
            "<html><body><h1>Google sign-in failed</h1><p>consent déclined</p></body></html>",
        ),
        (
            "code=unused&state=wrong",
            "Google OAuth callback state mismatch",
            "<html><body><h1>State mismatch</h1></body></html>",
        ),
    ] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback callback");
        let client = queue_callback(&listener, &format!("{GEMINI_CALLBACK_PATH}?{query}"));
        assert_eq!(
            assert_auth_failure(
                wait_for_gemini_oauth_callback(&listener, "expected", 1),
                error
            ),
            error
        );
        assert_callback_response(client, "400 Bad Request", body);
    }
}

#[test]
fn callback_timeout_returns_no_code() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback callback");
    assert_eq!(
        wait_for_gemini_oauth_callback(&listener, "expected", 0).expect("callback timeout"),
        None
    );
}

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
