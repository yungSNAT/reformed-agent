use super::*;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CHILD_CASE: &str = "HERMES_LUMIO_COVERAGE_CHILD_CASE";
const DONE: &str = "LUMIO_COVERAGE_DONE";
const API_OVERRIDE: &str = "https://api.coverage.invalid/v1";
const CLIENT_ID: &str = "lumio-coverage-client";
const MODEL: &str = "fixture/coverage-model";
const WAIT: Duration = Duration::from_secs(15);

fn isolated_command(test: &str, case: &str, root: &Path) -> Command {
    let (_, module) = module_path!().split_once("::").unwrap();
    let test_name = format!("{module}::{test}");
    let mut command = Command::new(env::current_exe().unwrap()); // nosemgrep: rust.lang.security.current-exe.current-exe
                                                                 // Set overrides before the child starts, so parallel tests never see them.
    command
        .args(["--exact", test_name.as_str(), "--nocapture"])
        .env_clear()
        .env(CHILD_CASE, case)
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("HERMES_HOME", root.join("hermes"))
        .env("LUMIO_API_URL", API_OVERRIDE)
        .env("LUMIO_CLIENT_ID", CLIENT_ID)
        .env("NO_PROXY", "*");
    for key in [
        "LLVM_PROFILE_FILE",
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "SystemRoot",
        "WINDIR",
        "PATH",
    ] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

struct TestChild {
    process: Child,
    lines: mpsc::Receiver<std::io::Result<String>>,
    transcript: String,
}

impl TestChild {
    fn start(command: &mut Command) -> Self {
        let mut process = command.stdout(Stdio::piped()).spawn().unwrap();
        let stdout = process.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        drop(std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        }));
        Self {
            process,
            lines,
            transcript: String::new(),
        }
    }

    fn read_prefixed(&mut self, prefix: &str) -> String {
        let deadline = Instant::now() + WAIT;
        while let Ok(line) = self
            .lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            let line = line.unwrap();
            self.transcript.push_str(&line);
            self.transcript.push('\n');
            if let Some(suffix) = line.trim_start().strip_prefix(prefix) {
                return suffix.to_owned();
            }
        }
        panic!("Child did not print {prefix:?}:\n{}", self.transcript);
    }

    fn finish(mut self) {
        assert_eq!(self.read_prefixed(DONE), "");
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.process.try_wait().unwrap() {
                assert!(status.success(), "{status}:\n{}", self.transcript);
                return;
            }
            assert!(Instant::now() < deadline, "Child did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

fn assert_io_error(result: Result<(), AgentError>, prefix: &str) {
    match result.unwrap_err() {
        AgentError::Io(message) => assert!(message.starts_with(prefix), "{message}"),
        other => panic!("Expected an I/O error, got {other:?}"),
    }
}

fn isolated_paths() -> (PathBuf, PathBuf) {
    let home = PathBuf::from(env::var_os("HERMES_HOME").unwrap());
    assert_eq!(token_path(), home.join("lumio.json"));
    let config = hermes_config::paths::config_path();
    assert!(config.starts_with(&home), "{config:?} is outside {home:?}");
    (home, config)
}

fn model_yaml(api_url: &str, model: &str) -> String {
    format!("model:\n  provider: lumio\n  base_url: {api_url}\n  default: {model}\n")
}

fn check_token_store(home: &Path, api_url: &str) {
    let file = home.join("lumio.json");
    assert!(load_token().is_none());

    let seconds_since_epoch = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    };
    let before = seconds_since_epoch();
    save_token("fixture-token", "Coverage User").unwrap();
    let after = seconds_since_epoch();
    let saved = load_token().unwrap();
    assert_eq!(saved.token, "fixture-token");
    assert_eq!(saved.username, "Coverage User");
    assert_eq!(saved.base_url, api_url);
    assert!((before - 1.0..=after + 1.0).contains(&saved.created_at));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    fs::write(&file, "{malformed").unwrap();
    assert!(load_token().is_none());
    fs::write(&file, r#"{"token":"","username":"ignored"}"#).unwrap();
    assert!(load_token().is_none());
    fs::write(&file, r#"{"token":"legacy-token"}"#).unwrap();
    let legacy = load_token().unwrap();
    assert_eq!(legacy.token, "legacy-token");
    assert!(legacy.username.is_empty());
    assert!(legacy.base_url.is_empty());

    clear_token();
    assert!(!file.exists());
    assert!(load_token().is_none());

    fs::create_dir(&file).unwrap();
    assert_io_error(save_token("unused", "unused"), "Failed to write ");
    assert!(file.is_dir());
    fs::remove_dir(&file).unwrap();
}

fn check_provider_files(home: &Path, config: &Path, api_url: &str) {
    let dotenv = home.join(".env");
    configure_provider("first", MODEL).unwrap();
    assert_eq!(
        fs::read_to_string(&dotenv).unwrap(),
        "LUMIO_API_KEY=first\n"
    );
    assert_eq!(
        fs::read_to_string(config).unwrap(),
        model_yaml(api_url, MODEL)
    );

    fs::write(&dotenv, "KEEP=yes").unwrap();
    fs::write(config, "keep: true\n").unwrap();
    configure_provider("second", "fixture/second").unwrap();
    assert_eq!(
        fs::read_to_string(&dotenv).unwrap(),
        "KEEP=yes\nLUMIO_API_KEY=second\n"
    );
    let appended = format!("keep: true\n\n{}", model_yaml(api_url, "fixture/second"));
    assert_eq!(fs::read_to_string(config).unwrap(), appended);

    configure_provider("rotated", "fixture/unused").unwrap();
    assert_eq!(
        fs::read_to_string(&dotenv).unwrap(),
        "KEEP=yes\nLUMIO_API_KEY=rotated"
    );
    assert_eq!(fs::read_to_string(config).unwrap(), appended);

    fs::remove_file(config).unwrap();
    fs::create_dir(config).unwrap();
    assert_io_error(
        configure_provider("third", MODEL),
        "Failed to write config:",
    );
    assert!(config.is_dir());
    fs::remove_file(&dotenv).unwrap();
    fs::create_dir(&dotenv).unwrap();
    assert_io_error(configure_provider("fourth", MODEL), "Failed to write .env:");
}

#[test]
fn token_and_provider_files_are_isolated() {
    if let Ok(case) = env::var(CHILD_CASE) {
        let expected_url = match case.as_str() {
            "default" => "https://api.lumio.run",
            "override" => API_OVERRIDE,
            _ => panic!("Unexpected filesystem case: {case}"),
        };
        assert_eq!(lumio_api_url(), expected_url);
        let (home, config) = isolated_paths();
        check_token_store(&home, expected_url);
        check_provider_files(&home, &config, expected_url);

        fs::remove_dir_all(&home).unwrap();
        fs::write(&home, "not a directory").unwrap();
        assert_io_error(save_token("unused", "unused"), "Failed to create dir:");
        assert_eq!(fs::read_to_string(&home).unwrap(), "not a directory");
        println!("\n{DONE}");
        return;
    }

    for case in ["default", "override"] {
        let root = tempfile::tempdir().unwrap();
        let mut command =
            isolated_command("token_and_provider_files_are_isolated", case, root.path());
        if case == "default" {
            command.env_remove("LUMIO_API_URL");
        }
        TestChild::start(&mut command).finish();
    }
}

#[test]
fn http_response_formats_status_and_byte_length() {
    for (status, reason) in [
        (200, "OK"),
        (400, "Bad Request"),
        (404, "Not Found"),
        (503, "Error"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut reader = TcpStream::connect_timeout(&listener.local_addr().unwrap(), WAIT).unwrap();
        reader.set_read_timeout(Some(WAIT)).unwrap();
        let (mut writer, _) = listener.accept().unwrap();
        writer.set_write_timeout(Some(WAIT)).unwrap();

        let html = "<p>✓</p>";
        write_http_response(&mut writer, status, html).unwrap();
        writer.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        reader.read_to_string(&mut response).unwrap();
        assert_eq!(
            response,
            format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{html}"
            )
        );
    }
}

#[test]
fn invalid_endpoint_urls_return_errors_without_connecting() {
    let invalid_url = "not a URL";
    assert_eq!(
        exchange_code_blocking(invalid_url, "fixture-code", "http://localhost/callback")
            .unwrap_err(),
        "Could not reach the token endpoint"
    );
    assert!(!fetch_userinfo_blocking(invalid_url, "fixture-token")
        .unwrap_err()
        .is_empty());
}

fn begin_local_login(
    test: &str,
    case: &str,
    root: &Path,
    service: &wiremock::MockServer,
) -> (TestChild, reqwest::Url) {
    let base_url = service.uri();
    let mut command = isolated_command(test, case, root);
    command.env("LUMIO_BASE_URL", &base_url);
    let mut child = TestChild::start(&mut command);
    let query = child.read_prefixed(&format!("{base_url}/api/oauth2/authorize?"));
    let params = parse_query(&query);
    assert_eq!(params["client_id"], CLIENT_ID);
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["scope"], "read");
    let mut callback = reqwest::Url::parse(&params["redirect_uri"]).unwrap();
    assert_eq!(callback.scheme(), "http");
    assert_eq!(callback.host_str(), Some("localhost"));
    assert_eq!(callback.path(), "/callback");
    assert!(callback.port().is_some());
    callback.set_host(Some("127.0.0.1")).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("state", &params["state"]);
    (child, callback)
}

async fn local_login() -> LoginResult {
    let endpoint = reqwest::Url::parse(&lumio_base_url()).unwrap();
    assert_eq!(endpoint.scheme(), "http");
    assert_eq!(endpoint.host_str(), Some("127.0.0.1"));
    assert!(endpoint.port().is_some());
    login(false).await
}

fn callback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(WAIT)
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_login_uses_the_loopback_callback_and_loads_the_account_name() {
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    if env::var_os(CHILD_CASE).is_some() {
        let result = local_login().await;
        assert!(result.success, "{:?}", result.error);
        assert_eq!(result.token.as_deref(), Some("fixture-account-value"));
        assert_eq!(result.username.as_deref(), Some("Fixture & Colleague"));
        assert!(result.error.is_none());
        assert!(!token_path().exists());
        println!("\n{DONE}");
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let service = MockServer::start().await;
    let (child, mut callback) = begin_local_login(
        "interactive_login_uses_the_loopback_callback_and_loads_the_account_name",
        "authorised",
        home.path(),
        &service,
    );
    let redirect_uri = format!("http://localhost:{}/callback", callback.port().unwrap());
    Mock::given(method("POST"))
        .and(path("/api/oauth2/token"))
        .and(body_json(serde_json::json!({
            "client_id": CLIENT_ID,
            "grant_type": "authorization_code",
            "code": "fixture-code",
            "redirect_uri": redirect_uri,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fixture-account-value",
        })))
        .expect(1)
        .mount(&service)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/oauth2/userinfo"))
        .and(header("authorization", "Bearer fixture-account-value"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "username": "Fixture & Colleague",
        })))
        .expect(1)
        .mount(&service)
        .await;

    callback
        .query_pairs_mut()
        .append_pair("code", "fixture-code");
    let response = callback_client().get(callback).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("Fixture &amp; Colleague"));
    child.finish();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_login_reports_denial_unavailability_and_missing_codes() {
    use wiremock::MockServer;

    if let Ok(case) = env::var(CHILD_CASE) {
        let result = local_login().await;
        assert!(!result.success);
        assert!(result.token.is_none());
        assert!(result.username.is_none());
        let expected = match case.as_str() {
            "access_denied" => "Authorization was denied",
            "temporarily_unavailable" => "The authorization service is unavailable, please retry",
            "missing-code" => "No authorization code",
            _ => unreachable!("only declared fixture cases are passed to the child"),
        };
        assert_eq!(result.error.as_deref(), Some(expected));
        println!("\n{DONE}");
        return;
    }

    let client = callback_client();
    let service = MockServer::start().await;
    for (case, status) in [
        ("access_denied", reqwest::StatusCode::OK),
        ("temporarily_unavailable", reqwest::StatusCode::OK),
        ("missing-code", reqwest::StatusCode::BAD_REQUEST),
    ] {
        let home = tempfile::tempdir().unwrap();
        let (child, mut callback) = begin_local_login(
            "interactive_login_reports_denial_unavailability_and_missing_codes",
            case,
            home.path(),
            &service,
        );
        if case != "missing-code" {
            callback.query_pairs_mut().append_pair("error", case);
        }
        let response = client.get(callback).send().await.unwrap();
        assert_eq!(response.status(), status);
        child.finish();
    }
    assert!(service.received_requests().await.unwrap().is_empty());
}
