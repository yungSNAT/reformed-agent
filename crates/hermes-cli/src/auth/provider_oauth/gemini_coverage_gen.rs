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
    let mut command = Command::new(std::env::current_exe().expect("test executable")); // nosemgrep: rust.lang.security.current-exe.current-exe
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

include!("gemini_part2_coverage_gen.rs");
